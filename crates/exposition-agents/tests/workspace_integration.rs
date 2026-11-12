//! Agent tools on the real workspace service (scripted provider — development
//! only — drives the loop): documents open through the workspace (disk
//! bookkeeping, so saving an agent-edited buffer works), listing follows the
//! workspace's rules, and tasks may only act on the open folder.
#![cfg(debug_assertions)]

use std::sync::Arc;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::*;
use exposition_core::path::WorkspacePath;
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tempfile::TempDir;

const MAIN: &str = "= Notes\nThe cat sat on the mat.\n";

struct Harness {
    service: AgentService,
    workspace: Arc<WorkspaceService>,
    documents: Arc<DocumentService>,
    project: TempDir,
    other: TempDir,
    _data: TempDir,
}

fn harness() -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    std::fs::create_dir_all(project.path().join("chapters")).unwrap();
    std::fs::write(project.path().join("chapters/intro.typ"), "Intro\n").unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    std::fs::write(project.path().join(".git/config"), "x").unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("main.typ"), "other project\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().to_path_buf());
    workspace.open_workspace(project.path()).unwrap();
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::clone(&documents),
        workspace: Some(Arc::clone(&workspace)),
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
    Harness { service, workspace, documents, project, other, _data: data }
}

fn request(root: &std::path::Path) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "Edit".into(),
        workspace_root: root.display().to_string(),
        scope: TaskScope::Workspace,
        permission: PermissionProfile::AutoApply,
        execution: ExecutionMode::Disabled,
        consent: None,
        budgets: None,
    }
}

fn results(h: &Harness, id: &TaskId) -> Vec<Value> {
    h.service
        .task_detail(id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { result, .. } => result.and_then(|r| serde_json::from_str(&r).ok()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_documents_open_through_the_workspace_and_can_be_saved() {
    let h = harness();
    let script = json!({"turns": [
        {"steps": [
            {"toolUse": {"id": "l", "name": "list_files", "input": {"path": "", "recursive": true}}},
            {"toolUse": {"id": "r", "name": "read_file", "input": {"path": "main.typ"}}}
        ]},
        {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {
            "path": "main.typ", "base_revision": "$ref:r/revision", "edits": [{"old_text": "cat", "new_text": "owl"}]}}}]},
        {"steps": [{"text": "done"}]}
    ]});
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(request(h.project.path())).unwrap();
    h.service.join(&task.id).await;
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
    let out = results(&h, &task.id);
    let listed: Vec<&str> = out[0]["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert!(listed.contains(&"chapters/intro.typ") && listed.contains(&"main.typ"), "{listed:?}");
    assert!(!listed.iter().any(|p| p.starts_with(".git")), "workspace hidden names are not listed: {listed:?}");
    assert_eq!(out[1]["contentHash"].as_str().unwrap().len(), 64);

    // The buffer the agent edited is a workspace document: saving it writes the
    // agent's change to disk without a spurious external-change conflict.
    let id = h.documents.find_by_path(&WorkspacePath::parse("main.typ").unwrap()).unwrap();
    assert_eq!(std::fs::read_to_string(h.project.path().join("main.typ")).unwrap(), MAIN);
    h.workspace.save_document(&id, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(h.project.path().join("main.typ")).unwrap(),
        "= Notes\nThe owl sat on the mat.\n"
    );
    // Revert still works after the save (the buffer becomes dirty again).
    assert!(h.service.revert(&task.id, None).unwrap()[0].ok);
    assert_eq!(h.documents.state(&id).unwrap().text, MAIN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tasks_only_act_on_the_open_workspace() {
    let h = harness();
    // Another folder (same relative paths) is refused up front.
    let err = h.service.create_task(request(h.other.path())).unwrap_err();
    assert_eq!(err.code, "agents.workspaceNotOpen");

    // Closing the workspace while a task runs makes later tools refuse.
    let script = json!({"turns": [
        {"steps": [{"delayMs": 300}, {"toolUse": {"id": "r", "name": "read_file", "input": {"path": "main.typ"}}}]},
        {"steps": [{"text": "done"}]}
    ]});
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(request(h.project.path())).unwrap();
    h.workspace.close_workspace();
    h.service.join(&task.id).await;
    let out = results(&h, &task.id);
    assert!(out[0]["error"].as_str().unwrap().contains("No project folder is open"), "{:?}", out[0]);
    assert!(h.documents.find_by_path(&WorkspacePath::parse("main.typ").unwrap()).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn created_files_open_in_the_workspace_and_revert_closes_them() {
    let h = harness();
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "c", "name": "create_file", "input": {"path": "figures/plot.typ", "content": "#figure[]\n"}}}]},
        {"steps": [{"text": "done"}]}
    ]});
    h.service.load_script(script).unwrap();
    let task = h.service.create_task(request(h.project.path())).unwrap();
    h.service.join(&task.id).await;
    let path = WorkspacePath::parse("figures/plot.typ").unwrap();
    assert!(h.documents.find_by_path(&path).is_some(), "the created file is open as a workspace document");
    assert!(h.service.revert(&task.id, None).unwrap()[0].ok);
    assert!(h.documents.find_by_path(&path).is_none());
    assert!(!h.project.path().join("figures/plot.typ").exists());
}
