//! `check_lean` on the real Lean service (`lake serve` of the project's
//! toolchain), driven by the scripted provider — development only. Skips
//! (printing SKIPPED) when the fixture's toolchain is not installed via elan.
#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::execution::AppToolServices;
use exposition_agents::*;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_lean::{LeanConfig, LeanService};
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;

const TOOLCHAIN_DIR: &str = "leanprover--lean4---v4.35.0";

fn toolchain_installed() -> bool {
    let home = std::env::var_os("ELAN_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".elan")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".elan")));
    home.is_some_and(|h| h.join("toolchains").join(TOOLCHAIN_DIR).join("bin").exists())
}

fn copy_fixture(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == ".lake" || name == "lake-manifest.json" {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

struct Harness {
    service: AgentService,
    execution: ExecutionService,
    project: TempDir,
    events: broadcast::Receiver<AgentEvent>,
    _data: TempDir,
}

fn harness() -> Harness {
    let project = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lean-basic");
    copy_fixture(&fixture, project.path());
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    workspace.open_workspace(project.path()).unwrap();
    let mut config = ExecutionConfig::new(data.path());
    config.settings = Some(ExecutionSettings::default());
    config.too_broad_to_trust = Some(Vec::new());
    let execution = ExecutionService::with_config(config).unwrap();
    execution.register_workspace_root(project.path()).unwrap();
    let (lean, actor) = LeanService::new(execution.clone(), Arc::clone(&workspace), LeanConfig::default());
    tokio::spawn(actor);
    let services = AppToolServices::new(execution.clone()).with_workspace(Arc::clone(&workspace)).with_lean(lean);
    let service = AgentService::new(AgentConfig {
        database: None,
        documents,
        workspace: Some(workspace),
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(services),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let events = service.subscribe();
    Harness { service, execution, project, events, _data: data }
}

fn check(id: &str, path: &str) -> Value {
    json!({"toolUse": {"id": id, "name": "check_lean", "input": {"path": path}}})
}

async fn run(h: &mut Harness, script: Value, execution: ExecutionMode) -> (TaskSummary, Vec<(bool, Value)>, u32) {
    h.service.load_script(script).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "Check the proofs".into(),
            workspace_root: h.project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution,
            consent: None,
            budgets: None,
        })
        .unwrap();
    let mut approvals = 0;
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == task.id => {
                    approvals += 1;
                    h.service.resolve_approval(&task.id, &approval.id, true).unwrap();
                }
                Ok(AgentEvent::TaskChanged { task: t }) if t.id == task.id && !t.status.is_active() => break,
                _ => {}
            }
        }
    })
    .await
    .expect("task finished");
    h.service.join(&task.id).await;
    let results = h
        .service
        .task_detail(&task.id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { is_error, result, .. } => {
                Some((is_error, result.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null)))
            }
            _ => None,
        })
        .collect();
    (h.service.task(&task.id).unwrap(), results, approvals)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn check_lean_reports_checked_only_for_complete_error_free_text() {
    if !toolchain_installed() {
        eprintln!("SKIPPED: Lean toolchain {TOOLCHAIN_DIR} is not installed through elan");
        return;
    }
    let mut h = harness();
    let script = json!({"turns": [{"steps": [check("c", "LeanBasic/Defs.lean")]}, {"steps": [{"text": "done"}]}]});

    // Untrusted folder: refused before any approval; nothing runs.
    let (_, out, approvals) = run(&mut h, script.clone(), ExecutionMode::ApproveEach).await;
    assert_eq!(approvals, 0);
    assert!(out[0].0 && out[0].1["error"].as_str().unwrap().contains("not trusted"), "{}", out[0].1);
    // Restricted-only tasks never run Lean (it only runs natively).
    let (_, out, _) = run(&mut h, script, ExecutionMode::Restricted).await;
    assert!(out[0].1["error"].as_str().unwrap().contains("natively"), "{}", out[0].1);

    h.execution.grant_trust(&h.project.path().display().to_string()).unwrap();
    let script = json!({"turns": [
        {"steps": [check("defs", "LeanBasic/Defs.lean"), check("proofs", "LeanBasic/Proofs.lean")]},
        {"steps": [{"text": "Defs.lean is checked; Proofs.lean has an error and a sorry."}]}
    ]});
    let (task, out, approvals) = run(&mut h, script, ExecutionMode::ApproveEach).await;
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(approvals, 2, "each Lean check is approved");
    let (defs_error, defs) = &out[0];
    assert!(!defs_error, "{defs}");
    assert_eq!(defs["checked"], true);
    assert_eq!(defs["path"], "LeanBasic/Defs.lean");
    assert_eq!(defs["contentHash"].as_str().unwrap().len(), 64, "{defs}");
    let (proofs_error, proofs) = &out[1];
    assert!(proofs_error, "an unchecked file is never reported as success: {proofs}");
    assert_eq!(proofs["checked"], false);
    assert!(!proofs["sorries"].as_array().unwrap().is_empty(), "{proofs}");
    assert!(proofs["diagnostics"].as_array().unwrap().iter().any(|d| d["severity"] == "error"), "{proofs}");
    assert!(proofs["note"].as_str().unwrap().starts_with("Not checked"), "{proofs}");
    assert_eq!(task.usage.commands, 2);
}
