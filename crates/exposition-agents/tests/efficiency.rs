//! Cost regressions of the agent harness, asserted with deterministic
//! counters (never wall-clock time), next to the correctness each saving
//! must keep. The benchmarks that measured these costs are in
//! `perf_agents.rs`.
#![cfg(debug_assertions)]

use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::store::StoreStats;
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::*;
use exposition_agents::prompt;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;

const MAIN: &str = "= Notes\nThe cat sat on the mat.\n";

struct Harness {
    service: AgentService,
    documents: Arc<DocumentService>,
    project: TempDir,
    _data: TempDir,
}

fn harness() -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    std::fs::write(project.path().join("other.typ"), "= Other\nA dog.\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::clone(&documents),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(UnavailableServices),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
        graph: None,
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    Harness { service, documents, project, _data: data }
}

fn p(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn request(h: &Harness, permission: PermissionProfile) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "Improve the notes".into(),
        workspace_root: h.project.path().display().to_string(),
        scope: TaskScope::Workspace,
        permission,
        execution: ExecutionMode::Disabled,
        consent: None,
        budgets: None,
        approval_mode: None,
        provider: None,
        attachments: None,
    }
}

async fn run(h: &Harness, script: Value, req: CreateTaskRequest) -> TaskSummary {
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(req).unwrap();
    tokio::time::timeout(Duration::from_secs(30), h.service.join(&task.id)).await.expect("task finished");
    h.service.task(&task.id).unwrap()
}

fn tool(id: &str, name: &str, input: Value) -> Value {
    json!({"toolUse": {"id": id, "name": name, "input": input}})
}

fn turns(calls: Vec<Value>) -> Value {
    let mut turns: Vec<Value> = calls.into_iter().map(|c| json!({"steps": [c], "chunkDelayMs": 0})).collect();
    turns.push(json!({"steps": [{"text": "Done."}], "chunkDelayMs": 0}));
    json!({"turns": turns})
}

fn read_then_edit(path: &str, old: &str, new: &str) -> Value {
    turns(vec![
        tool("read", "read_file", json!({"path": path})),
        tool(
            "edit",
            "edit_document",
            json!({"path": path, "base_revision": "$ref:read/revision", "edits": [{"old_text": old, "new_text": new}]}),
        ),
    ])
}

fn since(h: &Harness, before: StoreStats) -> StoreStats {
    let after = h.service.store_stats();
    StoreStats {
        ops: after.ops - before.ops,
        changes: after.changes - before.changes,
        review_text_bytes: after.review_text_bytes - before.review_text_bytes,
        transcript_bytes: after.transcript_bytes - before.transcript_bytes,
        tool_result_bytes: after.tool_result_bytes - before.tool_result_bytes,
    }
}

fn user_edit(h: &Harness, path: &str, patch: TextPatch) {
    let id = h.documents.find_by_path(&p(path)).unwrap();
    let base = h.documents.info(&id).unwrap().revision;
    h.documents
        .apply(TransactionRequest {
            id: TransactionId(format!("user-{base}")),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit { document: id, base_revision: base, patches: vec![patch] }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
}

fn undo(h: &Harness, path: &str) {
    let id = h.documents.find_by_path(&p(path)).unwrap();
    h.documents.undo(&id, EditOrigin::Source).unwrap();
}

fn text_of(h: &Harness, path: &str) -> String {
    let id = h.documents.find_by_path(&p(path)).unwrap();
    h.documents.state(&id).unwrap().text
}

/// AGENTS-1. A task summary (computed on every status update, and for every
/// task in the list) reads no review text out of the database while the file
/// is at the task's tip or back at its baseline, and the pending count still
/// follows the file's content through undo, later edits, and an undone revert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summaries_read_no_review_texts_and_still_follow_content() {
    let h = harness();
    let task = run(&h, read_then_edit("main.typ", "cat sat", "dog sat"), request(&h, PermissionProfile::AutoApply)).await;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!((task.changed_files, task.pending_review), (1, 1));

    // At the tip: decided without reading a text.
    let before = h.service.store_stats();
    assert_eq!(h.service.task(&task.id).unwrap().pending_review, 1);
    let listed = h.service.list_tasks().unwrap();
    assert_eq!(listed.iter().find(|t| t.id == task.id).unwrap().pending_review, 1);
    assert_eq!(since(&h, before).review_text_bytes, 0, "a summary read review texts");

    // The user edits elsewhere: the change is still there (decided by diffing).
    user_edit(&h, "main.typ", TextPatch::insert(0, "// note\n"));
    assert_eq!(h.service.task(&task.id).unwrap().pending_review, 1);

    // The user undoes their edit, then the agent's: back at the baseline, not pending.
    undo(&h, "main.typ");
    undo(&h, "main.typ");
    assert_eq!(text_of(&h, "main.typ"), MAIN);
    let before = h.service.store_stats();
    assert_eq!(h.service.task(&task.id).unwrap().pending_review, 0, "an undone change is not pending");
    assert_eq!(since(&h, before).review_text_bytes, 0);
    // The review view agrees (it reads the texts: it shows them).
    assert_eq!(h.service.review(&task.id).unwrap().files[0].state, ReviewState::Undone);

    // A second task whose change the user reverts, then brings back with undo.
    let second =
        run(&h, read_then_edit("other.typ", "A dog.", "A cat."), request(&h, PermissionProfile::AutoApply)).await;
    assert_eq!(second.pending_review, 1);
    assert!(h.service.revert(&second.id, None).unwrap()[0].ok);
    assert_eq!(h.service.task(&second.id).unwrap().pending_review, 0);
    undo(&h, "other.typ");
    assert_eq!(text_of(&h, "other.typ"), "= Other\nA cat.\n");
    let before = h.service.store_stats();
    assert_eq!(h.service.task(&second.id).unwrap().pending_review, 1, "an undone revert is pending again");
    assert_eq!(since(&h, before).review_text_bytes, 0);
    assert_eq!(h.service.review(&second.id).unwrap().files[0].state, ReviewState::Pending);
}

/// AGENTS-1. Recording an edit reads the record of the edited file only, not
/// the texts of every file the task changed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recording_an_edit_reads_one_file_record() {
    let h = harness();
    let big = "x".repeat(40_000);
    std::fs::write(h.project.path().join("big.typ"), format!("= Big\nMARK\n{big}\n")).unwrap();
    let script = turns(vec![
        tool("r1", "read_file", json!({"path": "big.typ"})),
        tool("e1", "edit_document", json!({"path": "big.typ", "base_revision": "$ref:r1/revision",
            "edits": [{"old_text": "MARK", "new_text": "DONE"}]})),
        tool("r2", "read_file", json!({"path": "main.typ"})),
        tool("e2", "edit_document", json!({"path": "main.typ", "base_revision": "$ref:r2/revision",
            "edits": [{"old_text": "cat sat", "new_text": "dog sat"}]})),
        tool("e3", "edit_document", json!({"path": "main.typ", "base_revision": "$ref:e2/revision",
            "edits": [{"old_text": "the mat", "new_text": "a mat"}]})),
    ]);
    let before = h.service.store_stats();
    let task = run(&h, script, request(&h, PermissionProfile::AutoApply)).await;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!((task.changed_files, task.pending_review), (2, 2));
    assert_eq!(text_of(&h, "main.typ"), "= Notes\nThe dog sat on a mat.\n");
    // Only the third edit finds an existing record: main.typ's three small texts.
    let read = since(&h, before).review_text_bytes;
    assert!(read < 1_000, "edits to main.typ read {read} bytes of review texts (big.typ's were read too)");
}

/// Every event of a task's run, in order.
async fn events_of_run(h: &Harness, script: Value) -> (TaskSummary, Vec<AgentEvent>) {
    let mut events = h.service.subscribe();
    let task = run(h, script, request(h, PermissionProfile::ReadOnly)).await;
    let mut seen = Vec::new();
    loop {
        match events.try_recv() {
            Ok(event) => seen.push(event),
            Err(broadcast::error::TryRecvError::Lagged(n)) => panic!("{n} events were dropped"),
            Err(_) => break,
        }
    }
    (task, seen)
}

/// AGENTS-7. Streamed deltas reach the webview joined into few events, with
/// every character, in order, and before the stored message is announced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_deltas_are_joined_complete_and_in_order() {
    let h = harness();
    let words = 600;
    let body: String = (0..words).map(|i| format!("word{i} ")).collect();
    let script = json!({"turns": [{"chunkDelayMs": 0, "steps": [
        {"thinking": "First thoughts. "}, {"text": body}, {"thinking": "Second thoughts."}, {"text": "The end."}
    ]}]});
    let (task, events) = events_of_run(&h, script).await;
    assert_eq!(task.status, TaskStatus::Completed);
    // Consecutive deltas of one kind, joined: exactly what the model streamed.
    let mut runs: Vec<(bool, String)> = Vec::new();
    let mut deltas = 0;
    let mut last_delta = 0;
    for (index, event) in events.iter().enumerate() {
        let (thinking, text) = match event {
            AgentEvent::TextDelta { text, turn, .. } => (false, (text, turn)),
            AgentEvent::ThinkingDelta { text, turn, .. } => (true, (text, turn)),
            _ => continue,
        };
        assert_eq!(*text.1, 1);
        assert!(!text.0.is_empty(), "an empty delta was sent");
        deltas += 1;
        last_delta = index;
        match runs.last_mut() {
            Some((kind, joined)) if *kind == thinking => joined.push_str(text.0),
            _ => runs.push((thinking, text.0.clone())),
        }
    }
    let expected = vec![
        (true, "First thoughts. ".to_owned()),
        (false, body.clone()),
        (true, "Second thoughts.".to_owned()),
        (false, "The end.".to_owned()),
    ];
    assert_eq!(runs, expected);
    assert!(deltas <= words / 10, "{deltas} delta events for {words} streamed words");
    let stored = events.iter().position(|e| matches!(e, AgentEvent::TranscriptChanged { .. })).unwrap();
    assert!(last_delta < stored, "a delta arrived after the stored message was announced");
}

/// AGENTS-7. Text held back to be joined is sent while the stream is quiet:
/// the tail of a burst does not wait for the model's next delta.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_text_is_sent_while_the_stream_is_quiet() {
    let h = harness();
    let mut events = h.service.subscribe();
    let script = json!({"turns": [{"chunkDelayMs": 0, "steps": [
        {"text": "alpha beta gamma"}, {"delayMs": 3000}, {"text": " omega"}
    ]}]});
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReadOnly)).unwrap();
    let mut text = String::new();
    tokio::time::timeout(Duration::from_millis(1500), async {
        while text != "alpha beta gamma" {
            if let Ok(AgentEvent::TextDelta { text: delta, .. }) = events.recv().await {
                text.push_str(&delta);
            }
        }
    })
    .await
    .expect("the burst's tail arrived while the stream was quiet");
    tokio::time::timeout(Duration::from_secs(30), h.service.join(&task.id)).await.expect("task finished");
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
}

/// What the scripted provider reports as input tokens for a request that
/// carries exactly `messages` (it counts what it is sent).
fn scripted_input_tokens(messages: &[(String, Vec<Value>)]) -> u64 {
    let chars: usize = messages.iter().map(|(_, content)| serde_json::to_string(content).unwrap().len()).sum();
    (chars / 4 + prompt::SYSTEM_PROMPT.len() / 4) as u64
}

/// AGENTS-4. A run reads and parses the stored conversation once, not once
/// per model request, and every request still carries exactly the stored
/// conversation up to that point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_reads_the_conversation_once_and_sends_all_of_it_every_turn() {
    let h = harness();
    let big = "y".repeat(40_000);
    std::fs::write(h.project.path().join("big.typ"), format!("= Big\n{big}\n")).unwrap();
    let mut calls = vec![tool("read", "read_file", json!({"path": "big.typ"}))];
    for i in 0..10 {
        calls.push(tool(&format!("l{i}"), "list_files", json!({"path": "", "recursive": false})));
    }
    // One more turn in the script, for the follow-up below.
    let mut script = turns(calls);
    script["turns"].as_array_mut().unwrap().push(json!({"steps": [{"text": "More."}], "chunkDelayMs": 0}));
    let before = h.service.store_stats();
    let task = run(&h, script, request(&h, PermissionProfile::ReadOnly)).await;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(task.usage.turns, 12);
    let parsed = since(&h, before).transcript_bytes;

    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    let messages = store.messages(&task.id).unwrap();
    let total: usize = messages.iter().map(|(_, content)| serde_json::to_string(content).unwrap().len()).sum();
    assert!(total > 40_000);
    // Twelve requests re-reading a 40 KB conversation would parse about 450 KB.
    assert!((parsed as usize) < 2 * total, "the run parsed {parsed} bytes of a {total}-byte conversation");

    // Each request carried the whole conversation stored before it.
    let sent = |messages: &[(String, Vec<Value>)]| -> u64 {
        let requests = messages.iter().enumerate().filter(|(_, (role, _))| role == "assistant");
        requests.map(|(index, _)| scripted_input_tokens(&messages[..index])).sum()
    };
    assert_eq!(task.usage.input_tokens, sent(&messages));

    // A follow-up is a new run: it reads the conversation again, the follow-up included.
    h.service.continue_task(&task.id, "And then?", None).unwrap();
    tokio::time::timeout(Duration::from_secs(30), h.service.join(&task.id)).await.expect("task finished");
    let task = h.service.task(&task.id).unwrap();
    assert_eq!((task.status.clone(), task.usage.turns), (TaskStatus::Completed, 13));
    let messages = store.messages(&task.id).unwrap();
    assert_eq!(messages.len(), 26);
    assert_eq!(task.usage.input_tokens, sent(&messages));
}

/// AGENTS-10. Attachments are converted for the provider once per run (with
/// the conversation, which is read once), and every request of the run still
/// carries them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_request_of_a_run_carries_the_attachments() {
    let h = harness();
    let text = "attached line\n".repeat(2_000);
    let encoded = exposition_agents::attachments::base64_encode(text.as_bytes());
    let id = match h.service.add_attachment_bytes(Some("notes.txt"), Some("text/plain"), &encoded) {
        exposition_agents::attachments::IngestOutcome::Added { attachment } => attachment.id,
        exposition_agents::attachments::IngestOutcome::Rejected { message, .. } => panic!("{message}"),
    };
    let calls = (0..5).map(|i| tool(&format!("l{i}"), "list_files", json!({"path": "", "recursive": false}))).collect();
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.attachments = Some(vec![id]);
    let before = h.service.store_stats();
    let task = run(&h, turns(calls), req).await;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(task.usage.turns, 6);
    // The provider was sent the attachment text with each of the six requests.
    assert!(task.usage.input_tokens >= 6 * (text.len() as u64 / 4), "{:?}", task.usage);
    // The stored conversation (which refers to the attachment) was read once.
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    let stored: usize =
        store.messages(&task.id).unwrap().iter().map(|(_, c)| serde_json::to_string(c).unwrap().len()).sum();
    assert!((since(&h, before).transcript_bytes as usize) < 2 * stored);
}
