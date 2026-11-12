//! Live smoke test against the Anthropic Messages API.
//!
//! Runs only when a key is explicitly provided for the test run through
//! `EXPOSITION_LIVE_ANTHROPIC_KEY` (optionally `EXPOSITION_LIVE_MODEL`); it
//! reads no other environment variables, credential stores, or config files.
//! Without the variable it reports "skipped: no credentials".

use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::{MemoryStore, SecretStore};
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::*;
use exposition_core::path::WorkspacePath;
use exposition_workspace::DocumentService;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_anthropic_task_fixes_a_typo_through_a_transaction() {
    let key = match std::env::var("EXPOSITION_LIVE_ANTHROPIC_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            println!("skipped: no credentials (set EXPOSITION_LIVE_ANTHROPIC_KEY for this test run to enable)");
            return;
        }
    };
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("notes.md"), "# Notes\n\nHello wrold, this is a test document.\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let credentials = Arc::new(MemoryStore::default());
    credentials.set(key.trim()).unwrap();
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::clone(&documents),
        workspace: None,
        credentials,
        services: Arc::new(UnavailableServices),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: false,
        anthropic_base_url: None,
        retry: None,
        graph: None,
    })
    .unwrap();
    let model = std::env::var("EXPOSITION_LIVE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    service.update_settings(ProviderSettings { model, effort: Effort::Low, ..ProviderSettings::default() }).unwrap();
    let task = service
        .create_task(CreateTaskRequest {
            prompt: "Fix the spelling mistake in notes.md. Change nothing else.".into(),
            workspace_root: project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::Disabled,
            consent: Some(ContextConsent::ScopeContent),
            budgets: Some(Budgets {
                max_turns: 8,
                max_output_tokens: 60_000,
                max_input_tokens: 400_000,
                max_wall_ms: 5 * 60 * 1000,
                max_tool_calls: 12,
                max_commands: 0,
            }),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(330), service.join(&task.id)).await.expect("task finished");
    let summary = service.task(&task.id).unwrap();
    println!("live status: {:?}", summary.status);
    println!("live usage: {:?}", summary.usage);
    for item in service.task_detail(&task.id).unwrap().transcript {
        println!("{item:?}");
    }
    assert_eq!(summary.status, TaskStatus::Completed);
    let id = documents.find_by_path(&WorkspacePath::parse("notes.md").unwrap()).expect("document opened");
    let text = documents.state(&id).unwrap().text;
    assert!(text.contains("Hello world"), "{text}");
    assert_eq!(service.review(&task.id).unwrap().files.len(), 1);
}
