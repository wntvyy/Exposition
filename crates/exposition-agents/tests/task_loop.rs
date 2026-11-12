//! Task loop behavior with the development-only scripted provider (same
//! provider trait as the Anthropic adapter) and the real document service.
//!
//! The scripted provider cannot establish live model behavior; these tests
//! verify the application side: streaming events, tool execution, parallel
//! results, budgets, cancellation, scope/permission/consent enforcement,
//! transactions with concurrent user edits, review/revert, and persistence.
#![cfg(debug_assertions)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
    UnavailableServices,
};
use exposition_agents::*;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentEvent, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::job::ExecutionPolicy;
use exposition_core::path::WorkspacePath;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const MAIN: &str = "= Notes\nThe cat sat on the mat.\n";

struct Harness {
    service: AgentService,
    documents: Arc<DocumentService>,
    project: TempDir,
    events: broadcast::Receiver<AgentEvent>,
    _data: TempDir,
}

fn harness_with(services: Arc<dyn ToolServices>) -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    std::fs::write(project.path().join("refs.bib"), "@book{knuth, title={TAOCP}}\n").unwrap();
    std::fs::create_dir(project.path().join("chapters")).unwrap();
    std::fs::write(project.path().join("chapters/intro.typ"), "Intro\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::clone(&documents),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services,
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
    })
    .unwrap();
    let settings = ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() };
    service.update_settings(settings).unwrap();
    let events = service.subscribe();
    Harness { service, documents, project, events, _data: data }
}

fn harness() -> Harness {
    harness_with(Arc::new(UnavailableServices))
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
    }
}

async fn run(h: &Harness, script: Value, req: CreateTaskRequest) -> TaskSummary {
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(req).unwrap();
    tokio::time::timeout(Duration::from_secs(20), h.service.join(&task.id)).await.expect("task finished");
    h.service.task(&task.id).unwrap()
}

fn text_of(h: &Harness, path: &str) -> String {
    let id = h.documents.find_by_path(&p(path)).expect("document open");
    h.documents.state(&id).unwrap().text
}

/// Wait for an event matching `pred`.
async fn wait_for(events: &mut broadcast::Receiver<AgentEvent>, pred: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Ok(event) if pred(&event) => return event,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("event channel closed: {e}"),
            }
        }
    })
    .await
    .expect("expected event")
}

fn tool_results(h: &Harness, id: &TaskId) -> Vec<Value> {
    h.service
        .task_detail(id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { name, result, is_error, status, .. } => Some(json!({
                "name": name,
                "isError": is_error,
                "status": status,
                "result": result.and_then(|r| serde_json::from_str::<Value>(&r).ok()),
            })),
            _ => None,
        })
        .collect()
}

fn user_edit(h: &Harness, path: &str, base: u64, patch: TextPatch) -> u64 {
    let id = h.documents.find_by_path(&p(path)).unwrap();
    let outcome = h
        .documents
        .apply(TransactionRequest {
            id: TransactionId(format!("user-{}", uuid_like())),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit { document: id, base_revision: base, patches: vec![patch] }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    outcome.applied[0].revision
}

fn uuid_like() -> u64 {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

fn read_edit_done(old: &str, new: &str) -> Value {
    json!({"turns": [
        {"steps": [{"thinking": "Look at the file first."}, {"text": "Reading main.typ."},
                   {"toolUse": {"id": "toolu_read", "name": "read_file", "input": {"path": "main.typ"}}}]},
        {"steps": [{"toolUse": {"id": "toolu_edit", "name": "edit_document", "input": {
            "path": "main.typ", "base_revision": "$ref:toolu_read/revision",
            "edits": [{"old_text": old, "new_text": new}]}}}]},
        {"steps": [{"text": "Done: changed main.typ."}]}
    ]})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_an_open_document_through_a_transaction_streams_and_reverts() {
    let mut h = harness();
    let mut doc_events = h.documents.subscribe();
    let task = run(&h, read_edit_done("cat sat", "dog sat"), request(&h, PermissionProfile::AutoApply)).await;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(text_of(&h, "main.typ"), "= Notes\nThe dog sat on the mat.\n");
    assert_eq!((task.usage.turns, task.usage.tool_calls), (3, 2));

    // The change went through the document service as an agent transaction.
    let changed = loop {
        match doc_events.try_recv().unwrap() {
            DocumentEvent::Changed(c) => break c,
            _ => continue,
        }
    };
    assert_eq!(changed.origin, EditOrigin::Agent { task_id: task.id.0.clone() });
    assert_eq!(changed.patches, vec![TextPatch::replace(12, 19, "dog sat")]);

    // The transcript streamed.
    let mut streamed = String::new();
    while let Ok(event) = h.events.try_recv() {
        if let AgentEvent::TextDelta { task_id, text, .. } = event
            && task_id == task.id
        {
            streamed.push_str(&text);
        }
    }
    assert!(streamed.contains("Reading main.typ.") && streamed.contains("Done: changed main.typ."), "{streamed}");
    let detail = h.service.task_detail(&task.id).unwrap();
    assert!(
        detail.transcript.iter().any(|i| matches!(i, TranscriptItem::Thinking { text, .. } if text.contains("Look")))
    );
    assert!(
        matches!(detail.transcript.first(), Some(TranscriptItem::UserText { text, .. }) if text == "Improve the notes")
    );

    // Review shows the change; revert restores the text.
    let review = h.service.review(&task.id).unwrap();
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].state, ReviewState::Pending);
    assert!(review.checkpoint.is_some());
    let diff = h.service.file_diff(&task.id, &p("main.typ")).unwrap();
    assert_eq!(diff.original, MAIN);
    assert_eq!(diff.modified, "= Notes\nThe dog sat on the mat.\n");
    let results = h.service.revert(&task.id, None).unwrap();
    assert!(results[0].ok, "{results:?}");
    assert_eq!(text_of(&h, "main.typ"), MAIN);
    assert_eq!(h.service.review(&task.id).unwrap().files[0].state, ReviewState::Reverted);
    // Disk is untouched: agent edits live in the open buffer until the user saves.
    assert_eq!(std::fs::read_to_string(h.project.path().join("main.typ")).unwrap(), MAIN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_tool_calls_return_all_results_in_one_user_message() {
    let h = harness();
    let script = json!({"turns": [
        {"steps": [
            {"toolUse": {"id": "toolu_a", "name": "read_file", "input": {"path": "main.typ"}}},
            {"toolUse": {"id": "toolu_b", "name": "read_file", "input": {"path": "refs.bib"}}},
            {"toolUse": {"id": "toolu_c", "name": "list_files", "input": {"path": "", "recursive": true}}}
        ]},
        {"steps": [{"text": "Read them."}]}
    ]});
    let task = run(&h, script, request(&h, PermissionProfile::ReadOnly)).await;
    assert_eq!(task.status, TaskStatus::Completed);
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    let messages = store.messages(&task.id).unwrap();
    let roles: Vec<&str> = messages.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
    let results = &messages[2].1;
    let ids: Vec<&str> = results.iter().map(|b| b["tool_use_id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["toolu_a", "toolu_b", "toolu_c"]);
    assert!(results.iter().all(|b| b["type"] == "tool_result" && b.get("is_error").is_none()));
    let listed: Value = serde_json::from_str(results[2]["content"].as_str().unwrap()).unwrap();
    let paths: Vec<&str> = listed["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&"chapters/intro.typ") && paths.contains(&"main.typ"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scope_consent_and_permission_are_enforced_by_the_application() {
    let h = harness();
    let script = json!({"turns": [
        {"steps": [
            {"toolUse": {"id": "t1", "name": "read_file", "input": {"path": "refs.bib"}}},
            {"toolUse": {"id": "t2", "name": "list_files", "input": {"path": "", "recursive": true}}},
            {"toolUse": {"id": "t3", "name": "read_file", "input": {"path": "../outside.txt"}}},
            {"toolUse": {"id": "t4", "name": "create_file", "input": {"path": "new.typ", "content": "x"}}},
            {"toolUse": {"id": "t5", "name": "read_file", "input": {"path": "main.typ"}}}
        ]},
        {"steps": [{"text": "ok"}]}
    ]});
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.scope = TaskScope::Document { path: p("main.typ") };
    let task = run(&h, script, req).await;
    let results = tool_results(&h, &task.id);
    assert!(results[0]["result"]["error"].as_str().unwrap().contains("outside this task's scope"));
    let listed: Vec<&str> =
        results[1]["result"]["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert_eq!(listed, vec!["main.typ"]);
    assert!(results[2]["result"]["error"].as_str().unwrap().contains("invalid path"));
    assert!(results[3]["result"]["error"].as_str().unwrap().contains("read-only"));
    assert_eq!(results[4]["isError"], false);
    assert!(!h.project.path().join("new.typ").exists());

    // Metadata-only consent refuses content reads but allows listing.
    let script = json!({"turns": [
        {"steps": [
            {"toolUse": {"id": "m1", "name": "read_file", "input": {"path": "main.typ"}}},
            {"toolUse": {"id": "m2", "name": "list_files", "input": {"path": "chapters", "recursive": false}}}
        ]},
        {"steps": [{"text": "ok"}]}
    ]});
    let mut req = request(&h, PermissionProfile::AutoApply);
    req.consent = Some(ContextConsent::MetadataOnly);
    let task = run(&h, script, req).await;
    let results = tool_results(&h, &task.id);
    assert!(results[0]["result"]["error"].as_str().unwrap().contains("consent"));
    assert_eq!(results[1]["isError"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_user_edit_is_rebased_when_disjoint_and_reported_on_conflict() {
    // Disjoint: the user edits while the agent's edit waits for approval.
    let mut h = harness();
    h.service.load_script(read_edit_done("sat on", "lay on")).unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReviewEdits)).unwrap();
    let approval = match wait_for(&mut h.events, |e| matches!(e, AgentEvent::ApprovalRequested { .. })).await {
        AgentEvent::ApprovalRequested { approval } => approval,
        _ => unreachable!(),
    };
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::AwaitingApproval);
    assert!(approval.detail.contains("- sat on") && approval.detail.contains("+ lay on"), "{}", approval.detail);
    user_edit(&h, "main.typ", 1, TextPatch::insert(12, "big "));
    h.service.resolve_approval(&task.id, &approval.id, true).unwrap();
    h.service.join(&task.id).await;
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
    assert_eq!(text_of(&h, "main.typ"), "= Notes\nThe big cat lay on the mat.\n");
    let results = tool_results(&h, &task.id);
    assert_eq!(results[1]["result"]["rebased"], true);
    // Reverting keeps the user's concurrent edit.
    assert!(h.service.revert(&task.id, None).unwrap()[0].ok);
    assert_eq!(text_of(&h, "main.typ"), "= Notes\nThe big cat sat on the mat.\n");

    // Overlap: the user changes the same words; the agent gets a clear error
    // with the current revision, re-reads, and redoes the edit.
    let mut h = harness();
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "r1", "name": "read_file", "input": {"path": "main.typ"}}}]},
        {"steps": [{"toolUse": {"id": "e1", "name": "edit_document", "input": {
            "path": "main.typ", "base_revision": "$ref:r1/revision",
            "edits": [{"old_text": "sat on", "new_text": "lay on"}]}}}]},
        {"steps": [{"toolUse": {"id": "r2", "name": "read_file", "input": {"path": "main.typ"}}}]},
        {"steps": [{"toolUse": {"id": "e2", "name": "edit_document", "input": {
            "path": "main.typ", "base_revision": "$ref:r2/revision",
            "edits": [{"old_text": "stood on", "new_text": "stood proudly on"}]}}}]},
        {"steps": [{"text": "Done."}]}
    ]});
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReviewEdits)).unwrap();
    let first = match wait_for(&mut h.events, |e| matches!(e, AgentEvent::ApprovalRequested { .. })).await {
        AgentEvent::ApprovalRequested { approval } => approval,
        _ => unreachable!(),
    };
    let now = user_edit(&h, "main.typ", 1, TextPatch::replace(16, 22, "stood on"));
    h.service.resolve_approval(&task.id, &first.id, true).unwrap();
    let second = match wait_for(&mut h.events, |e| matches!(e, AgentEvent::ApprovalRequested { .. })).await {
        AgentEvent::ApprovalRequested { approval } => approval,
        _ => unreachable!(),
    };
    h.service.resolve_approval(&task.id, &second.id, true).unwrap();
    h.service.join(&task.id).await;
    let results = tool_results(&h, &task.id);
    assert_eq!(results[1]["isError"], true);
    let error = results[1]["result"]["error"].as_str().unwrap();
    assert!(error.contains("overlaps changes") && error.contains("do not overwrite"), "{error}");
    assert_eq!(results[1]["result"]["currentRevision"], now);
    assert_eq!(text_of(&h, "main.typ"), "= Notes\nThe cat stood proudly on the mat.\n");
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revert_rebases_over_later_edits_and_refuses_on_overlap() {
    let h = harness();
    let task = run(&h, read_edit_done("cat sat", "dog sat"), request(&h, PermissionProfile::AutoApply)).await;
    // A later disjoint user edit survives the revert.
    let rev = h.documents.info(&h.documents.find_by_path(&p("main.typ")).unwrap()).unwrap().revision;
    user_edit(&h, "main.typ", rev, TextPatch::replace(2, 7, "Diary"));
    let results = h.service.revert(&task.id, Some(&p("main.typ"))).unwrap();
    assert!(results[0].ok && results[0].message.contains("later edits were kept"), "{results:?}");
    assert_eq!(text_of(&h, "main.typ"), "= Diary\nThe cat sat on the mat.\n");

    // A later overlapping edit makes the revert refuse with a clear message.
    let task = run(&h, read_edit_done("mat", "rug"), request(&h, PermissionProfile::AutoApply)).await;
    let rev = h.documents.info(&h.documents.find_by_path(&p("main.typ")).unwrap()).unwrap().revision;
    user_edit(&h, "main.typ", rev, TextPatch::replace(27, 30, "carpet"));
    let before = text_of(&h, "main.typ");
    let results = h.service.revert(&task.id, None).unwrap();
    assert!(!results[0].ok && results[0].message.contains("overlap"), "{results:?}");
    assert_eq!(text_of(&h, "main.typ"), before);
    let review = h.service.review(&task.id).unwrap();
    assert_eq!(review.files[0].state, ReviewState::Pending);
    assert!(review.files[0].message.as_deref().unwrap().contains("overlap"));
    // Accepting keeps the change.
    assert!(h.service.accept(&task.id, None).unwrap()[0].ok);
    assert_eq!(h.service.review(&task.id).unwrap().files[0].state, ReviewState::Accepted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn created_files_and_multi_file_changes_are_reviewable_per_file() {
    let h = harness();
    let script = json!({"turns": [
        {"steps": [
            {"toolUse": {"id": "c1", "name": "create_file", "input": {"path": "figures/plot.typ", "content": "#figure[]\n"}}},
            {"toolUse": {"id": "r1", "name": "read_file", "input": {"path": "chapters/intro.typ"}}}
        ]},
        {"steps": [{"toolUse": {"id": "e1", "name": "edit_document", "input": {
            "path": "chapters/intro.typ", "base_revision": "$ref:r1/revision",
            "edits": [{"old_text": "Intro", "new_text": "Introduction"}]}}}]},
        {"steps": [{"text": "Created a figure and renamed the intro."}]}
    ]});
    let task = run(&h, script, request(&h, PermissionProfile::AutoApply)).await;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!((task.changed_files, task.pending_review), (2, 2));
    assert_eq!(std::fs::read_to_string(h.project.path().join("figures/plot.typ")).unwrap(), "#figure[]\n");
    // Revert only the intro; the created file stays (partial revert keeps evidence).
    assert!(h.service.revert(&task.id, Some(&p("chapters/intro.typ"))).unwrap()[0].ok);
    assert_eq!(text_of(&h, "chapters/intro.typ"), "Intro\n");
    let review = h.service.review(&task.id).unwrap();
    let states: Vec<_> = review.files.iter().map(|f| (f.path.as_str().to_owned(), f.state, f.kind)).collect();
    assert_eq!(
        states,
        vec![
            ("chapters/intro.typ".into(), ReviewState::Reverted, ReviewChangeKind::Edited),
            ("figures/plot.typ".into(), ReviewState::Pending, ReviewChangeKind::Created)
        ]
    );
    // Reverting the creation deletes the untouched file.
    assert!(h.service.revert(&task.id, None).unwrap().iter().all(|r| r.ok));
    assert!(!h.project.path().join("figures/plot.typ").exists());
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    let kinds: Vec<String> = store.changes(&task.id).unwrap().into_iter().map(|c| c.kind).collect();
    assert_eq!(kinds, vec!["create", "edit", "revert", "delete"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budgets_stop_the_loop_outside_model_prose() {
    let h = harness();
    let forever = |n: usize| {
        let turns: Vec<Value> = (0..n)
            .map(|i| json!({"steps": [{"toolUse": {"id": format!("t{i}"), "name": "list_files", "input": {"path": "", "recursive": false}}}]}))
            .collect();
        json!({"turns": turns})
    };
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.budgets = Some(Budgets { max_turns: 2, ..Budgets::default() });
    let task = run(&h, forever(10), req).await;
    assert!(matches!(task.status, TaskStatus::BudgetExceeded { budget: BudgetKind::Turns, .. }), "{:?}", task.status);
    assert_eq!(task.usage.turns, 2);

    // Tool-call budget: the excess parallel call is not executed but answered.
    let script = json!({"turns": [{"steps": [
        {"toolUse": {"id": "a", "name": "list_files", "input": {"path": "", "recursive": false}}},
        {"toolUse": {"id": "b", "name": "list_files", "input": {"path": "", "recursive": false}}}
    ]}]});
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.budgets = Some(Budgets { max_tool_calls: 1, ..Budgets::default() });
    let task = run(&h, script, req).await;
    assert!(matches!(task.status, TaskStatus::BudgetExceeded { budget: BudgetKind::ToolCalls, .. }));
    let results = tool_results(&h, &task.id);
    assert_eq!((results[0]["isError"].clone(), results[1]["isError"].clone()), (json!(false), json!(true)));

    // Output tokens: max_tokens is capped by the remaining budget.
    let long = "word ".repeat(3000);
    let script = json!({"turns": [{"steps": [{"text": long}], "chunkDelayMs": 0}]});
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.budgets = Some(Budgets { max_output_tokens: 1024, ..Budgets::default() });
    let task = run(&h, script, req).await;
    assert!(
        matches!(task.status, TaskStatus::BudgetExceeded { budget: BudgetKind::OutputTokens, .. }),
        "{:?}",
        task.status
    );

    // Wall time: a slow response is aborted at the deadline.
    let script = json!({"turns": [{"steps": [{"text": "thinking..."}, {"delayMs": 20000}]}]});
    let mut req = request(&h, PermissionProfile::ReadOnly);
    req.budgets = Some(Budgets { max_wall_ms: 1000, ..Budgets::default() });
    let started = std::time::Instant::now();
    let task = run(&h, script, req).await;
    assert!(matches!(task.status, TaskStatus::BudgetExceeded { budget: BudgetKind::WallTime, .. }));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_mid_stream_aborts_and_keeps_history_valid() {
    let mut h = harness();
    let script = json!({"turns": [
        {"steps": [{"text": "Starting a long answer"}, {"waitForCancel": true}]},
        {"steps": [{"text": "Resumed."}]}
    ]});
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReadOnly)).unwrap();
    wait_for(&mut h.events, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    let started = std::time::Instant::now();
    h.service.cancel_task(&task.id).unwrap();
    h.service.join(&task.id).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    let task = h.service.task(&task.id).unwrap();
    assert_eq!(task.status, TaskStatus::Canceled);
    let detail = h.service.task_detail(&task.id).unwrap();
    assert!(!detail.transcript.iter().any(|i| matches!(i, TranscriptItem::AssistantText { .. })));
    assert!(
        detail
            .transcript
            .iter()
            .any(|i| matches!(i, TranscriptItem::Notice { message, .. } if message.contains("Canceled")))
    );
    // The task can be continued afterwards.
    h.service.continue_task(&task.id, "Please continue", None).unwrap();
    h.service.join(&task.id).await;
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
}

/// Process services that record calls; used to verify policy, approvals,
/// budgets, and mid-tool cancellation (not to simulate results for users).
struct RecordingServices {
    restricted: Option<String>,
    block: bool,
    calls: Mutex<Vec<CommandRequest>>,
    canceled: AtomicBool,
}

#[async_trait]
impl ToolServices for RecordingServices {
    fn available(&self, _kind: ServiceKind) -> bool {
        true
    }
    fn restricted_backend(&self) -> Option<String> {
        self.restricted.clone()
    }
    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({"status": "succeeded"}))
    }
    async fn run_python(&self, _: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({"stdout": "2"}))
    }
    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({"status": "checked"}))
    }
    async fn run_command(&self, request: CommandRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        self.calls.lock().unwrap().push(request);
        if self.block {
            cancel.cancelled().await;
            self.canceled.store(true, Ordering::SeqCst);
            return Err(ServiceError::Canceled);
        }
        Ok(json!({"exitCode": 0}))
    }
}

fn recording(restricted: Option<&str>, block: bool) -> Arc<RecordingServices> {
    Arc::new(RecordingServices {
        restricted: restricted.map(str::to_owned),
        block,
        calls: Mutex::new(Vec::new()),
        canceled: AtomicBool::new(false),
    })
}

fn command_script() -> Value {
    json!({"turns": [
        {"steps": [{"toolUse": {"id": "cmd", "name": "run_command", "input": {"program": "latexmk", "args": ["-pdf", "main.tex"], "working_directory": ""}}}]},
        {"steps": [{"text": "done"}]}
    ]})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_requires_approval_or_restricted_backend_and_never_downgrades() {
    // Restricted mode without a restricted backend: refused, never run natively.
    let services = recording(None, false);
    let h = harness_with(services.clone());
    let mut req = request(&h, PermissionProfile::AutoApply);
    req.execution = ExecutionMode::Restricted;
    let task = run(&h, command_script(), req).await;
    let results = tool_results(&h, &task.id);
    assert!(results[0]["result"]["error"].as_str().unwrap().contains("never falls back"));
    assert!(services.calls.lock().unwrap().is_empty());

    // Restricted backend available: runs unattended with the restricted policy.
    let services = recording(Some("docker"), false);
    let h = harness_with(services.clone());
    let mut req = request(&h, PermissionProfile::AutoApply);
    req.execution = ExecutionMode::Restricted;
    run(&h, command_script(), req).await;
    let calls = services.calls.lock().unwrap().clone();
    assert_eq!(calls[0].policy, ExecutionPolicy::Restricted { backend: "docker".into() });
    assert_eq!(calls[0].args, vec!["-pdf", "main.tex"]);

    // Trusted native requires approval per launch; a decline never runs it.
    let services = recording(None, false);
    let mut h = harness_with(services.clone());
    let mut req = request(&h, PermissionProfile::AutoApply);
    req.execution = ExecutionMode::ApproveEach;
    h.service.load_script(command_script()).unwrap();
    let task = h.service.create_task(req.clone()).unwrap();
    let approval = match wait_for(&mut h.events, |e| matches!(e, AgentEvent::ApprovalRequested { .. })).await {
        AgentEvent::ApprovalRequested { approval } => approval,
        _ => unreachable!(),
    };
    assert!(approval.detail.contains("latexmk") && approval.detail.contains("natively"));
    h.service.resolve_approval(&task.id, &approval.id, false).unwrap();
    h.service.join(&task.id).await;
    assert!(services.calls.lock().unwrap().is_empty());
    assert!(tool_results(&h, &task.id)[0]["result"]["error"].as_str().unwrap().contains("declined"));

    h.service.load_script(command_script()).unwrap();
    let task = h.service.create_task(req).unwrap();
    let approval = match wait_for(&mut h.events, |e| matches!(e, AgentEvent::ApprovalRequested { .. })).await {
        AgentEvent::ApprovalRequested { approval } => approval,
        _ => unreachable!(),
    };
    h.service.resolve_approval(&task.id, &approval.id, true).unwrap();
    h.service.join(&task.id).await;
    assert_eq!(services.calls.lock().unwrap()[0].policy, ExecutionPolicy::TrustedNative);
    assert_eq!(h.service.task(&task.id).unwrap().usage.commands, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_services_report_clearly_and_are_never_faked() {
    let h = harness();
    let script = json!({"turns": [
        {"steps": [
            {"toolUse": {"id": "b", "name": "build_document", "input": {"entry": "main.typ", "engine": "auto"}}},
            {"toolUse": {"id": "p", "name": "run_python", "input": {"code": "print(1+1)"}}},
            {"toolUse": {"id": "l", "name": "check_lean", "input": {"path": "main.typ"}}},
            {"toolUse": {"id": "c", "name": "run_command", "input": {"program": "ls", "args": [], "working_directory": ""}}}
        ]},
        {"steps": [{"text": "Could not build."}]}
    ]});
    let mut req = request(&h, PermissionProfile::AutoApply);
    req.execution = ExecutionMode::ApproveEach;
    let task = run(&h, script, req).await;
    for result in tool_results(&h, &task.id) {
        assert_eq!(result["isError"], true);
        assert!(result["result"]["error"].as_str().unwrap().contains("not available in this build"), "{result}");
    }
    assert!(h.service.task_detail(&task.id).unwrap().approvals.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_mid_tool_stops_the_process_and_keeps_changes_revertible() {
    let services = recording(Some("docker"), true);
    let mut h = harness_with(services.clone());
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "r", "name": "read_file", "input": {"path": "main.typ"}}}]},
        {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {
            "path": "main.typ", "base_revision": "$ref:r/revision", "edits": [{"old_text": "cat", "new_text": "cow"}]}}}]},
        {"steps": [{"toolUse": {"id": "cmd", "name": "run_command", "input": {"program": "sleep", "args": ["60"], "working_directory": ""}}}]},
        {"steps": [{"text": "Continuing after the interruption."}]}
    ]});
    h.service.load_script(script).unwrap();
    let mut req = request(&h, PermissionProfile::AutoApply);
    req.execution = ExecutionMode::Restricted;
    let task = h.service.create_task(req).unwrap();
    wait_for(&mut h.events, |e| matches!(e, AgentEvent::ToolCallStarted { name, .. } if name == "run_command")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.service.cancel_task(&task.id).unwrap();
    h.service.join(&task.id).await;
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Canceled);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(services.canceled.load(Ordering::SeqCst), "the running process was told to cancel");
    let results = tool_results(&h, &task.id);
    assert_eq!(results[2]["status"], json!("failed"));
    assert_eq!(results[2]["result"]["error"], "canceled");
    // The edit made before cancellation is reviewable and revertible.
    assert_eq!(h.service.review(&task.id).unwrap().files.len(), 1);
    assert!(h.service.revert(&task.id, None).unwrap()[0].ok);
    assert_eq!(text_of(&h, "main.typ"), MAIN);
    // Continuing answers the dangling tool call before the new request.
    h.service.continue_task(&task.id, "Go on", None).unwrap();
    h.service.join(&task.id).await;
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    let messages = store.messages(&task.id).unwrap();
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
    // The interrupted call is answered (with what it reported) before the new request.
    let answered = messages
        .iter()
        .flat_map(|(_, content)| content.iter())
        .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == "cmd")
        .expect("dangling tool call answered");
    assert_eq!(answered["is_error"], true);
    assert!(answered["content"].as_str().unwrap().contains("canceled"));
    let (role, last_user) = &messages[messages.len() - 2];
    assert_eq!((role.as_str(), last_user[0]["text"].as_str()), ("user", Some("Go on")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_tool_json_refusals_and_fallbacks_are_handled_safely() {
    let h = harness();
    // Malformed streamed input: an error result, nothing runs.
    let script = json!({"turns": [
        {"steps": [{"rawToolUse": {"id": "bad", "name": "create_file", "partialJson": "{\"path\": \"x.typ\", \"content\": \"unterminated"}}]},
        {"steps": [{"text": "Sorry."}]}
    ]});
    let task = run(&h, script, request(&h, PermissionProfile::AutoApply)).await;
    let results = tool_results(&h, &task.id);
    assert!(results[0]["result"]["INVALID_JSON"].as_str().unwrap().contains("unterminated"));
    assert!(!h.project.path().join("x.typ").exists());

    // Refusal: the declined turn's tool calls never run and are not kept.
    let script = json!({"turns": [{"steps": [
        {"toolUse": {"id": "c", "name": "create_file", "input": {"path": "y.typ", "content": "y"}}},
        {"refusal": {"category": "cyber", "explanation": "Declined by policy."}}
    ]}]});
    let task = run(&h, script, request(&h, PermissionProfile::AutoApply)).await;
    assert_eq!(
        task.status,
        TaskStatus::Refused { category: Some("cyber".into()), explanation: Some("Declined by policy.".into()) }
    );
    assert!(!h.project.path().join("y.typ").exists());
    let detail = h.service.task_detail(&task.id).unwrap();
    assert!(
        detail
            .transcript
            .iter()
            .any(|i| matches!(i, TranscriptItem::Notice { message, .. } if message.contains("declined")))
    );

    // Server-side fallback: blocks from the declined attempt are dropped and
    // never executed; the switch is reported.
    let script = json!({"turns": [
        {"steps": [
            {"thinking": "first attempt"},
            {"toolUse": {"id": "pre", "name": "create_file", "input": {"path": "pre.typ", "content": "x"}}},
            {"fallback": {"from": "claude-opus-5-5", "to": "claude-opus-5"}},
            {"toolUse": {"id": "post", "name": "list_files", "input": {"path": "", "recursive": false}}}
        ]},
        {"steps": [{"text": "ok"}]}
    ]});
    let task = run(&h, script, request(&h, PermissionProfile::AutoApply)).await;
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(!h.project.path().join("pre.typ").exists());
    let names: Vec<String> =
        tool_results(&h, &task.id).iter().map(|r| r["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names, vec!["list_files"]);
    let detail = h.service.task_detail(&task.id).unwrap();
    assert!(
        detail
            .transcript
            .iter()
            .any(|i| matches!(i, TranscriptItem::Notice { message, .. } if message.contains("server-side fallback")))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasks_persist_and_interrupted_runs_are_marked_after_restart() {
    let data = tempfile::tempdir().unwrap();
    let db = data.path().join("agents.sqlite3");
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    let make = || {
        AgentService::new(AgentConfig {
            database: Some(db.clone()),
            documents: Arc::new(DocumentService::new()),
            workspace: None,
            credentials: Arc::new(MemoryStore::default()),
            services: Arc::new(UnavailableServices),
            runtime: tokio::runtime::Handle::current(),
            allow_scripted: true,
            anthropic_base_url: None,
            retry: None,
        })
        .unwrap()
    };
    let first = make();
    first
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    first.load_script(read_edit_done("cat", "owl")).unwrap();
    let req = CreateTaskRequest {
        prompt: "Change the animal".into(),
        workspace_root: project.path().display().to_string(),
        scope: TaskScope::Workspace,
        permission: PermissionProfile::AutoApply,
        execution: ExecutionMode::Disabled,
        consent: None,
        budgets: None,
    };
    let done = first.create_task(req.clone()).unwrap();
    first.join(&done.id).await;
    // A second task is left "running" when the process goes away.
    first.load_script(json!({"turns": [{"steps": [{"waitForCancel": true}]}]})).unwrap();
    let stuck = first.create_task(req).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(first.task(&stuck.id).unwrap().status, TaskStatus::Running);

    // "Restart": a new service over the same database.
    let second = make();
    let tasks = second.list_tasks().unwrap();
    assert_eq!(tasks.len(), 2);
    let done = second.task(&done.id).unwrap();
    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(done.changed_files, 1);
    let detail = second.task_detail(&done.id).unwrap();
    assert!(
        detail
            .transcript
            .iter()
            .any(|i| matches!(i, TranscriptItem::AssistantText { text, .. } if text.contains("Done")))
    );
    assert_eq!(second.file_diff(&done.id, &p("main.typ")).unwrap().modified, "= Notes\nThe owl sat on the mat.\n");
    assert_eq!(second.task(&stuck.id).unwrap().status, TaskStatus::Interrupted);
    first.cancel_task(&stuck.id).unwrap();
    first.join(&stuck.id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credentials_are_never_exposed_and_required_for_the_live_provider() {
    let h = harness();
    h.service.update_settings(ProviderSettings::default()).unwrap();
    let status = h.service.provider_status();
    assert!(!status.configured);
    let err = h.service.create_task(request(&h, PermissionProfile::ReadOnly)).unwrap_err();
    assert_eq!(err.code, "agents.notConfigured");
    let key = "sk-ant-api03-test-key-not-real-0123456789";
    let status = h.service.set_api_key(key).unwrap();
    assert!(status.configured);
    let serialized = serde_json::to_string(&status).unwrap();
    assert!(!serialized.contains(key) && !serialized.contains("0123456789"));
    let store = exposition_agents::store::Store::open(&h._data.path().join("agents.sqlite3")).unwrap();
    assert!(!store.setting("provider").unwrap().unwrap().contains("sk-ant"));
    assert!(!h.service.clear_api_key().unwrap().configured);
    assert!(h.service.set_api_key("not a key").is_err());
}

/// Services whose end-of-task cleanup is slow (e.g. stopping an agent's
/// research kernel), to expose the window between "completed" and the worker
/// actually exiting.
struct SlowFinishServices;

#[async_trait]
impl ToolServices for SlowFinishServices {
    fn available(&self, kind: ServiceKind) -> bool {
        UnavailableServices.available(kind)
    }
    fn restricted_backend(&self) -> Option<String> {
        None
    }
    async fn build_document(&self, r: BuildRequest, c: CancellationToken) -> Result<Value, ServiceError> {
        UnavailableServices.build_document(r, c).await
    }
    async fn run_python(&self, r: PythonRequest, c: CancellationToken) -> Result<Value, ServiceError> {
        UnavailableServices.run_python(r, c).await
    }
    async fn check_lean(&self, r: LeanRequest, c: CancellationToken) -> Result<Value, ServiceError> {
        UnavailableServices.check_lean(r, c).await
    }
    async fn run_command(&self, r: CommandRequest, c: CancellationToken) -> Result<Value, ServiceError> {
        UnavailableServices.run_command(r, c).await
    }
    async fn task_finished(&self, _task_id: &str) {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Found by the live provider run: a task reported Completed while its worker
/// was still releasing services, and review actions were refused with
/// "Cancel the task before reverting its changes." Review must work as soon
/// as the task reports a terminal status.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_actions_work_as_soon_as_the_task_reports_finished() {
    let h = harness_with(Arc::new(SlowFinishServices));
    h.service.load_script(read_edit_done("cat sat", "dog sat")).unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::AutoApply)).unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = h.service.task(&task.id).unwrap().status;
            if !status.is_active() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("task reports a terminal status");
    assert_eq!(finished, TaskStatus::Completed);
    // Immediately, while slow cleanup may still be running:
    let results = h.service.revert(&task.id, None).expect("revert is allowed once the task reports finished");
    assert!(results[0].ok, "{results:?}");
    assert_eq!(text_of(&h, "main.typ"), MAIN);
    tokio::time::timeout(Duration::from_secs(20), h.service.join(&task.id)).await.expect("worker exits");
}
