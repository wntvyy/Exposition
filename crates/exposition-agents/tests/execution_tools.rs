//! Agent process tools on the real execution service (scripted provider —
//! development only — drives the loop; the jobs are real).
//!
//! Docker-backed tests run only when `EXPOSITION_TEST_DOCKER_PYTHON_IMAGE`
//! names a locally present image with `python` (e.g. `python:3.13-slim`);
//! otherwise they print SKIPPED. Nothing is pulled.
#![cfg(debug_assertions)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::execution::ExecutionToolServices;
use exposition_agents::*;
use exposition_execution::{DockerSettings, ExecutionConfig, ExecutionService, ExecutionSettings, Located};
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;

struct Harness {
    service: AgentService,
    execution: ExecutionService,
    project: TempDir,
    events: broadcast::Receiver<AgentEvent>,
    _data: TempDir,
}

fn harness(docker_image: Option<String>) -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), "= Notes\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut config = ExecutionConfig::new(data.path());
    config.settings = Some(ExecutionSettings {
        docker: DockerSettings { image: docker_image, ..Default::default() },
        ..Default::default()
    });
    config.too_broad_to_trust = Some(Vec::new());
    let execution = ExecutionService::with_config(config).unwrap();
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::new(DocumentService::new()),
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(ExecutionToolServices::new(execution.clone())),
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

fn docker_image() -> Option<String> {
    match std::env::var("EXPOSITION_TEST_DOCKER_PYTHON_IMAGE") {
        Ok(image) if !image.is_empty() => Some(image),
        _ => {
            eprintln!(
                "SKIPPED: set EXPOSITION_TEST_DOCKER_PYTHON_IMAGE to a local image with python (e.g. python:3.13-slim)"
            );
            None
        }
    }
}

fn request(h: &Harness, execution: ExecutionMode) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "Compute".into(),
        workspace_root: h.project.path().display().to_string(),
        scope: TaskScope::Workspace,
        permission: PermissionProfile::AutoApply,
        execution,
        consent: None,
        budgets: None,
    }
}

fn script(tool: Value) -> Value {
    json!({"turns": [{"steps": [{"toolUse": tool}]}, {"steps": [{"text": "done"}]}]})
}

fn results(h: &Harness, id: &TaskId) -> Vec<(ToolCallStatus, Value)> {
    h.service
        .task_detail(id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { status, result, .. } => {
                Some((status, result.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null)))
            }
            _ => None,
        })
        .collect()
}

async fn approval(events: &mut broadcast::Receiver<AgentEvent>) -> Option<ApprovalRequest> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) => return Some(approval),
                Ok(AgentEvent::TaskChanged { task }) if !task.status.is_active() => return None,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_execution_without_a_backend_is_refused_and_never_native() {
    let h = harness(None);
    h.service
        .load_script(script(json!({"id": "c", "name": "run_command", "input": {"program": "python", "args": ["-c", "print(1)"], "working_directory": ""}})))
        .unwrap();
    let task = h.service.create_task(request(&h, ExecutionMode::Restricted)).unwrap();
    h.service.join(&task.id).await;
    let (status, result) = &results(&h, &task.id)[0];
    assert_eq!(*status, ToolCallStatus::Failed);
    assert!(result["error"].as_str().unwrap().contains("never falls back"), "{result}");
    assert!(h.execution.list().is_empty(), "no job was launched");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_execution_needs_a_trusted_workspace_and_per_launch_approval() {
    let mut h = harness(None);
    let command = json!({"id": "c", "name": "run_command", "input": {"program": "python", "args": ["-c", "print(6*7)"], "working_directory": ""}});

    // Untrusted folder: refused before any approval prompt; nothing runs.
    h.service.load_script(script(command.clone())).unwrap();
    let task = h.service.create_task(request(&h, ExecutionMode::ApproveEach)).unwrap();
    assert!(approval(&mut h.events).await.is_none(), "no approval is requested for an untrusted folder");
    h.service.join(&task.id).await;
    assert!(results(&h, &task.id)[0].1["error"].as_str().unwrap().contains("not trusted"));
    assert!(h.execution.list().is_empty());

    if !matches!(h.execution.locate_tool("python"), Located::Found { .. }) {
        eprintln!("SKIPPED: python was not found for the native run");
        return;
    }
    // Trusted folder (an explicit user action) + approval: runs natively.
    h.execution.grant_trust(&h.project.path().display().to_string()).unwrap();
    h.service.load_script(script(command)).unwrap();
    let task = h.service.create_task(request(&h, ExecutionMode::ApproveEach)).unwrap();
    let ask = approval(&mut h.events).await.expect("approval requested");
    assert!(ask.detail.contains("python") && ask.detail.contains("natively"), "{}", ask.detail);
    h.service.resolve_approval(&task.id, &ask.id, true).unwrap();
    h.service.join(&task.id).await;
    let (status, result) = &results(&h, &task.id)[0];
    assert_eq!(*status, ToolCallStatus::Succeeded, "{result}");
    assert_eq!(result["policy"], "trustedNative");
    assert_eq!(result["exitCode"], 0);
    assert_eq!(result["stdout"].as_str().unwrap().trim(), "42");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unattended_python_runs_in_the_restricted_container() {
    let Some(image) = docker_image() else { return };
    let h = harness(Some(image));
    let code = "import os, sys\nprint(6*7)\nprint(sorted(k for k in os.environ if 'ANTHROPIC' in k.upper() or 'API_KEY' in k.upper() or 'TOKEN' in k.upper()))\nopen('/workspace/x','w')";
    h.service.load_script(script(json!({"id": "p", "name": "run_python", "input": {"code": code}}))).unwrap();
    let task = h.service.create_task(request(&h, ExecutionMode::Restricted)).unwrap();
    h.service.join(&task.id).await;
    assert!(h.service.task_detail(&task.id).unwrap().approvals.is_empty());
    let (status, result) = &results(&h, &task.id)[0];
    assert_eq!(*status, ToolCallStatus::Succeeded, "{result}");
    assert_eq!(result["policy"], "restricted:docker");
    let stdout = result["stdout"].as_str().unwrap();
    assert!(stdout.starts_with("42"), "{stdout}");
    assert!(stdout.contains("[]"), "no provider-credential variables reach the container: {stdout}");
    // The project is mounted read-only: the write fails and nothing appears.
    assert_ne!(result["exitCode"], 0);
    assert!(result["stderr"].as_str().unwrap().contains("Read-only file system"), "{result}");
    assert!(!h.project.path().join("x").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceling_a_task_kills_its_running_container_job() {
    let Some(image) = docker_image() else { return };
    let mut h = harness(Some(image));
    h.service
        .load_script(script(json!({"id": "p", "name": "run_python", "input": {"code": "import time\nprint('started', flush=True)\ntime.sleep(120)"}})))
        .unwrap();
    let task = h.service.create_task(request(&h, ExecutionMode::Restricted)).unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(AgentEvent::ToolCallStarted { .. }) = h.events.recv().await {
                break;
            }
        }
    })
    .await
    .unwrap();
    // Let the container start, then cancel the task.
    let deadline = Instant::now() + Duration::from_secs(60);
    while h.execution.list().iter().all(|j| j.info.started_at_ms.is_none()) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let canceled_at = Instant::now();
    h.service.cancel_task(&task.id).unwrap();
    h.service.join(&task.id).await;
    let elapsed = canceled_at.elapsed();
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Canceled);
    assert!(elapsed < Duration::from_secs(20), "cancel took {elapsed:?}");
    let job = h.execution.list().pop().expect("job");
    assert_eq!(job.info.result.map(|r| r.outcome), Some(exposition_core::job::JobOutcome::Canceled));
}
