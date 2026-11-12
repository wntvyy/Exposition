//! Review actions are refused while the task runs (it may still change the
//! files under review), in the service, not only in the UI.
#![cfg(debug_assertions)]

use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::*;
use exposition_workspace::DocumentService;
use serde_json::json;

async fn next_approval(events: &mut tokio::sync::broadcast::Receiver<AgentEvent>) -> ApprovalRequest {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(AgentEvent::ApprovalRequested { approval }) = events.recv().await {
                break approval;
            }
        }
    })
    .await
    .expect("approval requested")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accept_and_revert_are_refused_while_the_task_runs() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), "= Notes\n").unwrap();
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::new(DocumentService::new()),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(UnavailableServices),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let mut events = service.subscribe();
    service
        .load_script(json!({"turns": [
            {"steps": [{"toolUse": {"id": "c", "name": "create_file", "input": {"path": "a.typ", "content": "= A\n"}}}]},
            {"steps": [{"toolUse": {"id": "d", "name": "create_file", "input": {"path": "b.typ", "content": "= B\n"}}}]},
            {"steps": [{"text": "done"}]}
        ]}))
        .unwrap();
    let task = service
        .create_task(CreateTaskRequest {
            prompt: "Create two files".into(),
            workspace_root: project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::ReviewEdits,
            execution: ExecutionMode::Disabled,
            consent: Some(ContextConsent::ScopeContent),
            budgets: None,
        })
        .unwrap();
    // The first file is created; the task then waits for the second approval.
    let first = next_approval(&mut events).await;
    service.resolve_approval(&task.id, &first.id, true).unwrap();
    let second = next_approval(&mut events).await;
    assert_eq!(service.review(&task.id).unwrap().files.len(), 1, "one change to review");
    let accept = service.accept(&task.id, None).unwrap_err();
    assert_eq!(accept.code, "agents.taskRunning", "{accept:?}");
    let revert = service.revert(&task.id, None).unwrap_err();
    assert_eq!(revert.code, accept.code, "the same refusal as revert");
    assert!(service.review(&task.id).unwrap().files.iter().all(|f| f.state == ReviewState::Pending));
    // Once the task has finished, accepting works.
    service.resolve_approval(&task.id, &second.id, true).unwrap();
    service.join(&task.id).await;
    let accepted = service.accept(&task.id, None).unwrap();
    assert_eq!(accepted.len(), 2);
    assert!(accepted.iter().all(|r| r.ok), "{accepted:?}");
}
