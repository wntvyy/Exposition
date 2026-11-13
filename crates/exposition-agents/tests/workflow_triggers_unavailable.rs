//! Workflow triggers are user-only (M9): no agent tool reviews, enables, or
//! lists them, a call naming any trigger command is refused before anything
//! runs, and a workflow an agent proposes cannot carry one.
//!
//! The trigger service lives in `exposition-workflow` (which depends on this
//! crate); its own test `agents_cannot_create_or_enable_triggers` drives the
//! same refusal against a real trigger store.
#![cfg(debug_assertions)]

use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::tools::{self, ToolCall};
use exposition_agents::*;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};

const TRIGGER_COMMANDS: [&str; 4] =
    ["workflow_trigger_plan", "workflow_trigger_enable", "workflow_trigger_disable", "workflow_trigger_list"];

#[test]
fn no_agent_tool_reviews_or_enables_a_trigger() {
    for tool in tools::definitions() {
        let text = format!("{} {} {}", tool.name, tool.description, tool.input_schema).to_lowercase();
        assert!(!text.contains("trigger"), "agent tool {} mentions triggers", tool.name);
    }
    for name in TRIGGER_COMMANDS {
        let refused = ToolCall::parse(name, &json!({})).unwrap_err();
        assert!(refused.contains("unknown tool"), "{name}: {refused}");
    }
}

fn propose_input() -> Value {
    json!({
        "workflow_id": "",
        "expected_revision": -1,
        "title": "Proposed",
        "description": "",
        "preset": "",
        "inputs": [],
        "steps": [],
        "connections": [],
        "outcomes": [],
        "ceiling": {"execution": "restricted", "model_requests": false, "retain_results": false, "propose_changes": false}
    })
}

#[test]
fn a_proposed_workflow_cannot_carry_a_trigger() {
    assert!(ToolCall::parse(tools::WORKFLOW_PROPOSE, &propose_input()).is_ok(), "the base proposal parses");
    for (field, value) in [
        ("trigger", json!({"kind": "inputsChanged", "inputs": ["data"]})),
        ("triggers", json!([{"kind": "interval", "minutes": 15}])),
        ("standing_hash", json!("0".repeat(64))),
    ] {
        let mut input = propose_input();
        input[field] = value;
        let refused = ToolCall::parse(tools::WORKFLOW_PROPOSE, &input).unwrap_err();
        assert!(refused.contains("unknown field"), "{field}: {refused}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_calling_trigger_commands_is_refused() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), "= Notes\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::new(DocumentService::new()),
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
    let calls: Vec<Value> = TRIGGER_COMMANDS
        .iter()
        .enumerate()
        .map(|(i, name)| json!({"toolUse": {"id": format!("t{i}"), "name": name, "input": {"workflow": "wf-1"}}}))
        .collect();
    service
        .load_script(json!({"turns": [{"steps": calls}, {"steps": [{"text": "No trigger tool exists."}]}]}))
        .unwrap();
    let task = service
        .create_task(CreateTaskRequest {
            prompt: "Enable a trigger".into(),
            workspace_root: project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::Disabled,
            consent: None,
            budgets: None,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), service.join(&task.id)).await.expect("task finished");
    let results: Vec<(String, bool, String)> = service
        .task_detail(&task.id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { name, is_error, result, .. } => {
                Some((name, is_error, result.unwrap_or_default()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), TRIGGER_COMMANDS.len(), "{results:?}");
    for (name, is_error, result) in &results {
        assert!(*is_error && result.contains("unknown tool"), "{name} was not refused: {result}");
    }
    assert_eq!(service.task(&task.id).unwrap().status, TaskStatus::Completed);
    let files: Vec<_> = std::fs::read_dir(project.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(files, vec![std::ffi::OsString::from("main.typ")], "the refused calls changed the project");
}
