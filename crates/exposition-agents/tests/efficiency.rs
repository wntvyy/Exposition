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

/// The transcript's result preview, as it was computed from a whole result.
fn preview(text: &str) -> String {
    const LIMIT: usize = 4000;
    if text.len() <= LIMIT {
        return text.to_owned();
    }
    let mut end = LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn seq_of(item: &TranscriptItem) -> u64 {
    match item {
        TranscriptItem::UserText { seq, .. }
        | TranscriptItem::AssistantText { seq, .. }
        | TranscriptItem::Thinking { seq, .. }
        | TranscriptItem::ToolCall { seq, .. }
        | TranscriptItem::Notice { seq, .. } => *seq,
    }
}

/// AGENTS-6. Reading a task's detail reads no more of each tool result than
/// its preview shows, and a partial read (what a transcript event makes the
/// webview do) reads and returns only the tail, identical to the tail of a
/// full read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detail_reads_are_bounded_and_a_partial_read_is_the_tail_of_a_full_one() {
    let h = harness();
    // Multi-byte text, so a preview cut can fall inside a character.
    let big = "→é".repeat(12_000);
    std::fs::write(h.project.path().join("big.typ"), format!("= Big\n{big}\n")).unwrap();
    let mut calls = vec![tool("read", "read_file", json!({"path": "big.typ"}))];
    for i in 0..10 {
        calls.push(tool(&format!("l{i}"), "list_files", json!({"path": "", "recursive": false})));
    }
    let task = run(&h, turns(calls), request(&h, PermissionProfile::ReadOnly)).await;
    assert_eq!(task.status, TaskStatus::Completed);

    let before = h.service.store_stats();
    let full = h.service.task_detail(&task.id).unwrap();
    let cost = since(&h, before);
    assert_eq!(full.transcript_from, None);
    // Eleven results, each read up to its preview (4,001 characters of at most four bytes).
    assert!(cost.tool_result_bytes < 11 * 4_001 * 4, "a detail read {} bytes of tool results", cost.tool_result_bytes);
    // The previews are what cutting the whole results gives.
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    let whole = store.tool_calls(&task.id).unwrap();
    assert!(whole[0].result.as_ref().unwrap().len() > 50_000);
    let mut shown = 0;
    for item in &full.transcript {
        if let TranscriptItem::ToolCall { tool_use_id, result, .. } = item {
            let stored = whole.iter().find(|c| &c.tool_use_id == tool_use_id).unwrap();
            assert_eq!(result.as_deref(), stored.result.as_deref().map(preview).as_deref(), "{tool_use_id}");
            shown += 1;
        }
    }
    assert_eq!(shown, 11);

    // A partial read from the latest assistant message.
    let from = full
        .transcript
        .iter()
        .filter(|item| !matches!(item, TranscriptItem::UserText { .. } | TranscriptItem::Notice { .. }))
        .map(seq_of)
        .max()
        .unwrap();
    let before = h.service.store_stats();
    let partial = h.service.task_detail_from(&task.id, Some(from)).unwrap();
    let cost = since(&h, before);
    assert_eq!(partial.transcript_from, Some(from));
    let tail: Vec<TranscriptItem> = full.transcript.iter().filter(|item| seq_of(item) >= from).cloned().collect();
    assert!(!tail.is_empty() && tail.len() < full.transcript.len());
    assert_eq!(partial.transcript, tail);
    assert_eq!((&partial.summary, &partial.approvals, &partial.allowances), (&full.summary, &full.approvals, &full.allowances));
    assert_eq!(partial.questions, full.questions);
    assert_eq!(partial.graph.contexts, full.graph.contexts);
    assert!(cost.transcript_bytes < 1_000, "a partial read parsed {} bytes of the transcript", cost.transcript_bytes);

    // From before everything: the whole transcript again.
    assert_eq!(h.service.task_detail_from(&task.id, Some(0)).unwrap().transcript, full.transcript);
}

/// The graph context blocks of a task's conversation, in order.
fn graph_blocks(h: &Harness, id: &TaskId) -> Vec<String> {
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    store
        .messages(id)
        .unwrap()
        .iter()
        .flat_map(|(_, content)| content.iter())
        .filter_map(|block| block["text"].as_str())
        .filter(|text| text.starts_with("<graph_context>"))
        .map(str::to_owned)
        .collect()
}

/// AGENTS-5. A follow-up on an unchanged graph says the earlier context
/// still holds instead of sending it again; any change to what the context
/// says sends the whole context; the task-context update is sent every time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follow_up_does_not_repeat_an_unchanged_graph_context() {
    use exposition_graph::{ChangeRequest, EntityKind, GraphConfig, GraphOperation, GraphService, GraphState, Origin};

    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::write(root.join("main.typ"), MAIN).unwrap();
    let data = tempfile::tempdir().unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().join("graph") });
    assert!(matches!(graph.attach(&root).state, GraphState::Attached));
    graph.ensure_resource(&p("main.typ"), None, false, "test").unwrap();
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
        graph: Some(Arc::clone(&graph)),
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let h = Harness { service, documents, project, _data: data };

    let answers: Vec<Value> =
        (0..5).map(|i| json!({"steps": [{"text": format!("Answer {i}.")}], "chunkDelayMs": 0})).collect();
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.workspace_root = root.display().to_string();
    let task = run(&h, json!({"turns": answers}), req).await;
    assert_eq!(task.status, TaskStatus::Completed);
    let follow_up = |prompt: &'static str| {
        let h = &h;
        let id = task.id.clone();
        async move {
            h.service.continue_task(&id, prompt, None).unwrap();
            tokio::time::timeout(Duration::from_secs(30), h.service.join(&id)).await.expect("task finished");
            assert_eq!(h.service.task(&id).unwrap().status, TaskStatus::Completed);
        }
    };
    let whole = |block: &str| block.contains("\"entities\"");
    let revision = graph.status().revision;

    // Two follow-ups on the unchanged graph: one whole context, then short statements.
    follow_up("Second question").await;
    follow_up("Third question").await;
    let blocks = graph_blocks(&h, &task.id);
    assert_eq!(blocks.len(), 3, "every run still gets a graph context block");
    assert!(whole(&blocks[0]));
    for short in &blocks[1..] {
        assert!(!whole(short) && short.len() < 400, "{short}");
        assert!(short.contains("unchanged") && short.contains(&format!("revision {revision}")), "{short}");
        assert!(short.ends_with("</graph_context>"));
    }

    // The graph changes: the next follow-up carries the whole new context.
    let request = ChangeRequest {
        base_revision: revision,
        operations: vec![GraphOperation::CreateEntity {
            temp_id: "q".into(),
            kind: EntityKind::Question,
            roles: vec![],
            label: "A NEW-QUESTION-MARK".into(),
            key: None,
            attributes: std::collections::BTreeMap::new(),
            binding: None,
        }],
        summary: None,
    };
    graph.propose_and_apply(Origin::User, &request).unwrap();
    follow_up("Fourth question").await;
    follow_up("Fifth question").await;
    let blocks = graph_blocks(&h, &task.id);
    assert_eq!(blocks.len(), 5);
    assert!(whole(&blocks[3]), "a changed context is sent whole: {}", blocks[3]);
    assert_ne!(blocks[3], blocks[0]);
    assert!(!whole(&blocks[4]) && blocks[4].contains("unchanged"), "{}", blocks[4]);

    // The task context (approvals, files' saved and review state) is sent with every follow-up.
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    let updates = store
        .messages(&task.id)
        .unwrap()
        .iter()
        .flat_map(|(_, content)| content.iter())
        .filter(|block| block["text"].as_str().is_some_and(|text| text.starts_with("<task_context")))
        .count();
    assert_eq!(updates, 5);
    // Every run's context is recorded with the task, and the detail shows the latest.
    let detail = h.service.task_detail(&task.id).unwrap();
    assert_eq!(detail.graph.contexts, 5);
    assert_eq!(detail.graph.context.unwrap().revision, graph.status().revision);
}

/// The result of a finished tool call, parsed.
fn result(h: &Harness, id: &TaskId, tool_use_id: &str) -> (bool, Value) {
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    let call = store.tool_calls(id).unwrap().into_iter().find(|c| c.tool_use_id == tool_use_id).unwrap();
    (call.is_error, serde_json::from_str(&call.result.unwrap()).unwrap())
}

/// AGENTS-2. A partial read returns exactly the lines asked for with the
/// whole file's revision and hash, says what it holds and how to continue,
/// and leaves edits resolving against the whole revision. A whole read is
/// what it was. Scope and consent decide a partial read like any read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_partial_read_is_a_revisioned_excerpt_with_a_continuation() {
    let h = harness();
    let lines: Vec<String> = (1..=100).map(|n| format!("line {n} of the notes\n")).collect();
    let text = lines.concat();
    std::fs::write(h.project.path().join("long.typ"), &text).unwrap();
    let read = |id: &str, start: u64, max: u64| {
        tool(id, "read_file", json!({"path": "long.typ", "start_line": start, "max_lines": max}))
    };
    let script = turns(vec![
        tool("whole", "read_file", json!({"path": "long.typ"})),
        read("part", 10, 5),
        read("first", 0, 40),
        read("rest", 41, 0),
        read("past", 1000, 5),
        // An edit against the revision of a partial read, to text outside the excerpt.
        tool("edit", "edit_document", json!({"path": "long.typ", "base_revision": "$ref:part/revision",
            "edits": [{"old_text": "line 77 of the notes", "new_text": "line seventy-seven"}]})),
        // Unique in an excerpt is not enough: old_text is looked for in the whole file.
        tool("vague", "edit_document", json!({"path": "long.typ", "base_revision": "$ref:edit/revision",
            "edits": [{"old_text": "line 1", "new_text": "x"}]})),
    ]);
    let task = run(&h, script, request(&h, PermissionProfile::AutoApply)).await;
    assert_eq!(task.status, TaskStatus::Completed);

    let (err, whole) = result(&h, &task.id, "whole");
    assert!(!err);
    let mut keys: Vec<&str> = whole.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["contentHash", "path", "revision", "text", "unsavedChanges"], "a whole read is unchanged");
    assert_eq!(whole["text"], json!(text));

    let (err, part) = result(&h, &task.id, "part");
    assert!(!err, "{part}");
    assert_eq!(part["text"], json!(lines[9..14].concat()));
    assert_eq!((&part["startLine"], &part["endLine"], &part["totalLines"]), (&json!(10), &json!(14), &json!(100)));
    assert_eq!((&part["complete"], &part["nextStartLine"]), (&json!(false), &json!(15)));
    assert_eq!((&part["revision"], &part["contentHash"]), (&whole["revision"], &whole["contentHash"]));
    assert_eq!(part["unsavedChanges"], json!(false));

    // Excerpts joined in order are the file; the last one says it reached the end.
    let (_, first) = result(&h, &task.id, "first");
    let (_, rest) = result(&h, &task.id, "rest");
    assert_eq!(format!("{}{}", first["text"].as_str().unwrap(), rest["text"].as_str().unwrap()), text);
    assert_eq!((&first["startLine"], &first["nextStartLine"]), (&json!(1), &json!(41)));
    assert_eq!((&rest["endLine"], &rest["complete"]), (&json!(100), &json!(false)));
    assert!(rest.get("nextStartLine").is_none(), "nothing follows the last line");

    let (err, past) = result(&h, &task.id, "past");
    assert!(err && past["totalLines"] == json!(100), "{past}");

    let (err, edit) = result(&h, &task.id, "edit");
    assert!(!err, "{edit}");
    assert!(text_of(&h, "long.typ").contains("line seventy-seven\n"));
    let (err, vague) = result(&h, &task.id, "vague");
    assert!(err && vague["error"].as_str().unwrap().contains("occurs"), "{vague}");

    // The gate decides a partial read exactly like a whole one.
    let partial = |id: &str, path: &str| tool(id, "read_file", json!({"path": path, "start_line": 1, "max_lines": 1}));
    let mut scoped = request(&h, PermissionProfile::ReadOnly);
    scoped.scope = TaskScope::Document { path: p("main.typ") };
    let task = run(&h, turns(vec![partial("out", "long.typ"), partial("up", "../long.typ")]), scoped).await;
    let (err, out) = result(&h, &task.id, "out");
    assert!(err && out["error"].as_str().unwrap().contains("outside this task's scope"), "{out}");
    assert!(out.get("text").is_none());
    assert!(result(&h, &task.id, "up").0);
    let mut metadata = request(&h, PermissionProfile::ReadOnly);
    metadata.consent = Some(ContextConsent::MetadataOnly);
    let task = run(&h, turns(vec![partial("meta", "long.typ")]), metadata).await;
    let (err, meta) = result(&h, &task.id, "meta");
    assert!(err && meta["error"].as_str().unwrap().contains("consent") && meta.get("text").is_none(), "{meta}");
}

/// AGENTS-2. A listing that is not complete says so and how to continue; the
/// pages together are the whole listing, once each, and only what the task's
/// scope shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_truncated_listing_continues_from_its_next_offset() {
    let h = harness();
    std::fs::create_dir(h.project.path().join("many")).unwrap();
    for n in 0..620 {
        std::fs::write(h.project.path().join(format!("many/f{n:04}.txt")), "x").unwrap();
    }
    let list = |id: &str, offset: Value| {
        tool(id, "list_files", json!({"path": "many", "recursive": false, "offset": offset}))
    };
    let script = turns(vec![
        list("a", json!(0)),
        list("b", json!("$ref:a/nextOffset")),
        tool("small", "list_files", json!({"path": "", "recursive": false})),
    ]);
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.scope = TaskScope::Files { paths: vec![p("many"), p("main.typ")] };
    let task = run(&h, script, req).await;
    assert_eq!(task.status, TaskStatus::Completed);
    let (err, a) = result(&h, &task.id, "a");
    assert!(!err, "{a}");
    assert_eq!((a["entries"].as_array().unwrap().len(), &a["truncated"], &a["nextOffset"]), (500, &json!(true), &json!(500)));
    assert!(a["note"].as_str().unwrap().contains("offset 500"));
    let (err, b) = result(&h, &task.id, "b");
    assert!(!err, "{b}");
    assert_eq!((b["entries"].as_array().unwrap().len(), &b["truncated"]), (120, &json!(false)));
    assert!(b.get("nextOffset").is_none() && b.get("note").is_none());
    let mut paths: Vec<&str> =
        a["entries"].as_array().unwrap().iter().chain(b["entries"].as_array().unwrap()).map(|e| e["path"].as_str().unwrap()).collect();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths.len(), 620);
    assert_eq!((paths[0], paths[619]), ("many/f0000.txt", "many/f0619.txt"));
    // A complete listing is what it was, and shows only what the scope allows.
    let (_, small) = result(&h, &task.id, "small");
    let mut keys: Vec<&str> = small.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["entries", "truncated"]);
    let shown: Vec<&str> = small["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert_eq!(shown, ["main.typ", "many"]);
}
