//! Every tool-call state the transcript shows matches what happened to the
//! call: pending (queued behind an earlier call), awaiting approval, running
//! (including after approval), succeeded, failed, declined, canceled (stopped
//! while running), and not run. Provider changes are announced to every view.
//! The scripted provider (development only) stands in for the model.
#![cfg(debug_assertions)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
};
use exposition_agents::*;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Native execution; `run_command` blocks until canceled.
struct BlockingServices {
    started: AtomicBool,
}

#[async_trait]
impl ToolServices for BlockingServices {
    fn available(&self, _kind: ServiceKind) -> bool {
        true
    }
    fn restricted_backend(&self) -> Option<String> {
        None
    }
    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Typesetting))
    }
    async fn run_python(&self, _: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Research))
    }
    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Lean))
    }
    async fn run_command(&self, _: CommandRequest, cancel: CancellationToken) -> Result<Value, ServiceError> {
        self.started.store(true, Ordering::SeqCst);
        cancel.cancelled().await;
        Err(ServiceError::Canceled)
    }
}

struct Harness {
    service: AgentService,
    services: Arc<BlockingServices>,
    project: TempDir,
    events: broadcast::Receiver<AgentEvent>,
}

fn harness() -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), "= Notes\n").unwrap();
    let services = Arc::new(BlockingServices { started: AtomicBool::new(false) });
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::new(DocumentService::new()),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: services.clone(),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
    })
    .unwrap();
    let events = service.subscribe();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    Harness { service, services, project, events }
}

fn request(h: &Harness, permission: PermissionProfile, execution: ExecutionMode) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "Do things".into(),
        workspace_root: h.project.path().display().to_string(),
        scope: TaskScope::Workspace,
        permission,
        execution,
        consent: None,
        budgets: None,
    }
}

fn states(h: &Harness, id: &TaskId) -> Vec<(String, ToolCallStatus)> {
    h.service
        .task_detail(id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|i| match i {
            TranscriptItem::ToolCall { tool_use_id, status, .. } => Some((tool_use_id, status)),
            _ => None,
        })
        .collect()
}

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn approval(events: &mut broadcast::Receiver<AgentEvent>) -> ApprovalRequest {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(AgentEvent::ApprovalRequested { approval }) = events.recv().await {
                return approval;
            }
        }
    })
    .await
    .expect("approval requested")
}

fn tool(id: &str, name: &str, input: Value) -> Value {
    json!({"toolUse": {"id": id, "name": name, "input": input}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_awaiting_running_and_canceled_follow_the_call() {
    let mut h = harness();
    h.service
        .load_script(json!({"turns": [
            {"steps": [
                tool("cmd", "run_command", json!({"program": "latexmk", "args": [], "working_directory": ""})),
                tool("ls", "list_files", json!({"path": "", "recursive": false}))
            ]},
            {"steps": [{"text": "done"}]}
        ]}))
        .unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::AutoApply, ExecutionMode::ApproveEach)).unwrap();
    let asked = approval(&mut h.events).await;
    // Waiting for approval; the second call is queued behind it, not "not run".
    assert_eq!(
        states(&h, &task.id),
        vec![("cmd".into(), ToolCallStatus::AwaitingApproval), ("ls".into(), ToolCallStatus::Pending)]
    );
    h.service.resolve_approval(&task.id, &asked.id, true).unwrap();
    // Approved: it is running (not still "waiting for approval").
    until("the command to start", || h.services.started.load(Ordering::SeqCst)).await;
    assert_eq!(states(&h, &task.id)[0], ("cmd".into(), ToolCallStatus::Running));
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Running);
    h.service.cancel_task(&task.id).unwrap();
    h.service.join(&task.id).await;
    // Stopped while running: canceled (with what it reported); the queued call never ran.
    let detail = h.service.task_detail(&task.id).unwrap();
    let calls: Vec<_> = detail
        .transcript
        .iter()
        .filter_map(|i| match i {
            TranscriptItem::ToolCall { tool_use_id, status, result, .. } => {
                Some((tool_use_id.clone(), *status, result.clone().unwrap_or_default()))
            }
            _ => None,
        })
        .collect();
    assert_eq!((calls[0].1, calls[1].1), (ToolCallStatus::Canceled, ToolCallStatus::NotRun), "{calls:?}");
    assert!(calls[0].2.contains("canceled"), "{calls:?}");
    assert!(calls[1].2.contains("Not executed"), "{calls:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declined_and_budget_skipped_calls_are_not_failures() {
    let mut h = harness();
    h.service
        .load_script(json!({"turns": [
            {"steps": [tool("c", "create_file", json!({"path": "a.typ", "content": "x"}))]},
            {"steps": [{"text": "ok"}]}
        ]}))
        .unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReviewEdits, ExecutionMode::Disabled)).unwrap();
    let asked = approval(&mut h.events).await;
    h.service.resolve_approval(&task.id, &asked.id, false).unwrap();
    h.service.join(&task.id).await;
    assert_eq!(states(&h, &task.id), vec![("c".into(), ToolCallStatus::Declined)]);
    assert!(!h.project.path().join("a.typ").exists());

    // Over the tool-call budget: the extra call was not executed (not "failed");
    // a call the application refuses is a failure.
    h.service
        .load_script(json!({"turns": [{"steps": [
            tool("ok", "list_files", json!({"path": "", "recursive": false})),
            tool("over", "list_files", json!({"path": "", "recursive": false}))
        ]}]}))
        .unwrap();
    let mut req = request(&h, PermissionProfile::ReadOnly, ExecutionMode::Disabled);
    req.budgets = Some(Budgets { max_tool_calls: 1, ..Budgets::default() });
    let task = h.service.create_task(req).unwrap();
    h.service.join(&task.id).await;
    assert_eq!(
        states(&h, &task.id),
        vec![("ok".into(), ToolCallStatus::Succeeded), ("over".into(), ToolCallStatus::NotRun)]
    );
    h.service
        .load_script(json!({"turns": [
            {"steps": [tool("w", "create_file", json!({"path": "b.typ", "content": "x"}))]},
            {"steps": [{"text": "ok"}]}
        ]}))
        .unwrap();
    let task = h.service.create_task(request(&h, PermissionProfile::ReadOnly, ExecutionMode::Disabled)).unwrap();
    h.service.join(&task.id).await;
    assert_eq!(states(&h, &task.id), vec![("w".into(), ToolCallStatus::Failed)], "read-only refusal");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_changes_are_announced_without_the_key() {
    let mut h = harness();
    let mut seen = Vec::new();
    while let Ok(event) = h.events.try_recv() {
        if let AgentEvent::ProviderChanged { status } = event {
            seen.push(*status);
        }
    }
    assert_eq!(seen.last().map(|s| s.settings.provider), Some(ProviderKind::Scripted), "update_settings announced");
    let key = "sk-ant-api03-provider-changed-test-key-000000000000";
    h.service.set_api_key(key).unwrap();
    h.service.clear_api_key().unwrap();
    let mut configured = Vec::new();
    while let Ok(event) = h.events.try_recv() {
        if let AgentEvent::ProviderChanged { status } = event {
            assert!(!serde_json::to_string(&status).unwrap().contains(key));
            configured.push(status.configured);
        }
    }
    assert_eq!(configured, vec![true, false]);
}
