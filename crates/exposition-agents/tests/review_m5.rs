//! Independent review R-m5: reproductions of defects in agent permission,
//! scope, review, and provider handling.
//!
//! Every product-failing test is `#[ignore = "R-m5 Fn: ..."]` so the suite
//! stays green; run them with
//! `cargo test -p exposition-agents --test review_m5 -- --ignored --nocapture`.
//! Each one asserts the EXPECTED (secure/honest) behavior and fails on the
//! current code with a `DEFECT:` message. The scripted provider (development
//! only) stands in for the model; every service the tests touch is real
//! (document, workspace, typesetting, TeX, execution) except where a test
//! says it records calls. No test talks to the real model provider.
#![cfg(debug_assertions)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use exposition_agents::credentials::{MemoryStore, Secret};
use exposition_agents::provider::anthropic::{AnthropicProvider, RetryPolicy};
use exposition_agents::provider::{Provider, ProviderMessage, ProviderRequest, ToolDefinition};
use exposition_agents::tools::execution::AppToolServices;
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
    UnavailableServices,
};
use exposition_agents::*;
use exposition_core::path::WorkspacePath;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings, Located};
use exposition_tex::{TexService, TexServiceConfig};
use exposition_typesetting::{Fonts, TypesettingService};
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const MAIN: &str = "= Notes\n\nThe squares grow quickly.\n";
const PAPER: &str = "\\documentclass{article}\n\\begin{document}\nHello.\n\\end{document}\n";
const IN_PROJECT_SECRET: &str = "PRIVATESECRETqx41";
const OUTSIDE_SECRET: &str = "OUTSIDESECRETzq77";

fn p(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

/// TeX-friendly absolute path (forward slashes, no verbatim prefix).
fn tex_path(path: &std::path::Path) -> String {
    let raw = std::fs::canonicalize(path).unwrap().display().to_string();
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).replace('\\', "/")
}

struct App {
    service: AgentService,
    execution: ExecutionService,
    workspace: Arc<WorkspaceService>,
    documents: Arc<DocumentService>,
    project: TempDir,
    outside: TempDir,
    events: broadcast::Receiver<AgentEvent>,
    _data: TempDir,
}

/// The application's services as the desktop app wires them (workspace,
/// typesetting, TeX, execution), with the scripted provider.
fn app_with(settings: ExecutionSettings) -> App {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    std::fs::write(project.path().join("paper.tex"), PAPER).unwrap();
    std::fs::create_dir(project.path().join("private")).unwrap();
    std::fs::write(project.path().join("private/secret.txt"), format!("{IN_PROJECT_SECRET}\n")).unwrap();
    std::fs::create_dir(project.path().join("drafts")).unwrap();
    std::fs::write(project.path().join("drafts/readme.txt"), "drafts\n").unwrap();
    std::fs::create_dir(project.path().join("public")).unwrap();
    std::fs::write(project.path().join("public/notes.txt"), "public notes\n").unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), format!("{OUTSIDE_SECRET}\n")).unwrap();

    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    workspace.open_workspace(project.path()).unwrap();
    let mut config = ExecutionConfig::new(data.path());
    config.settings = Some(settings);
    config.too_broad_to_trust = Some(Vec::new());
    let execution = ExecutionService::with_config(config).unwrap();
    execution.register_workspace_root(project.path()).unwrap();
    let runtime = tokio::runtime::Handle::current();
    let tex =
        TexService::new(execution.clone(), Arc::clone(&workspace), runtime.clone(), TexServiceConfig::new(data.path()))
            .unwrap();
    let typesetting = Arc::new(TypesettingService::new(Arc::clone(&workspace), Fonts::with_system));
    let services = AppToolServices::new(execution.clone())
        .with_workspace(Arc::clone(&workspace))
        .with_typesetting(typesetting)
        .with_tex(tex);
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::clone(&documents),
        workspace: Some(Arc::clone(&workspace)),
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(services),
        runtime,
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let events = service.subscribe();
    App { service, execution, workspace, documents, project, outside, events, _data: data }
}

fn app() -> App {
    app_with(ExecutionSettings::default())
}

fn request(
    root: &std::path::Path,
    scope: TaskScope,
    permission: PermissionProfile,
    execution: ExecutionMode,
) -> CreateTaskRequest {
    CreateTaskRequest {
        prompt: "Work on the project".into(),
        workspace_root: root.display().to_string(),
        scope,
        permission,
        execution,
        consent: Some(ContextConsent::ScopeContent),
        budgets: None,
    }
}

/// Run a scripted task to its end; approvals are answered with `approve`.
/// Returns the final summary and how many approvals were requested.
async fn run_task(
    service: &AgentService,
    events: &mut broadcast::Receiver<AgentEvent>,
    script: Value,
    req: CreateTaskRequest,
    approve: bool,
) -> (TaskSummary, usize) {
    service.load_script(script).unwrap();
    let task = service.create_task(req).unwrap();
    let id = task.id.clone();
    let mut approvals = 0;
    let done = tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            match events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == id => {
                    approvals += 1;
                    service.resolve_approval(&id, &approval.id, approve).unwrap();
                }
                Ok(AgentEvent::TaskChanged { task }) if task.id == id && !task.status.is_active() => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("event channel closed: {e}"),
            }
        }
    })
    .await;
    assert!(done.is_ok(), "task finished in time");
    service.join(&id).await;
    (service.task(&id).unwrap(), approvals)
}

/// Tool results in transcript order: (tool, is_error, raw result text).
fn results(service: &AgentService, id: &TaskId) -> Vec<(String, bool, String)> {
    service
        .task_detail(id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { name, is_error, result, .. } => {
                Some((name, is_error, result.unwrap_or_default()))
            }
            _ => None,
        })
        .collect()
}

fn tool(id: &str, name: &str, input: Value) -> Value {
    json!({"toolUse": {"id": id, "name": name, "input": input}})
}

// ---------------------------------------------------------------------------
// F1: LaTeX builds bypass the task's execution mode.
// ---------------------------------------------------------------------------

/// "Restricted sandbox only (unattended)" promises no native execution, and
/// "Ask before each command (runs natively)" promises an approval per launch.
/// build_document on a .tex entry runs pdfLaTeX natively (TrustedNative job)
/// with no approval in BOTH modes. Combined with TeX reading any file the user
/// can read, an unattended agent scoped to ONE document puts a file from
/// OUTSIDE the project into its tool result (sent to the model provider).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r_m5_f1_latex_build_runs_natively_unattended_and_leaks_files_outside_the_project() {
    let mut a = app();
    if !matches!(a.execution.locate_tool("pdflatex"), Located::Found { .. }) {
        eprintln!("SKIPPED: pdflatex not found");
        return;
    }
    // The user trusts their project for their own builds.
    a.execution.grant_trust(&a.project.path().display().to_string()).unwrap();
    let outside = tex_path(&a.outside.path().join("secret.txt"));
    let leak = format!(
        "\\makeatletter\n\\edef\\leak{{\\@@input{{{outside}}} }}\n\\errmessage{{LEAK[\\leak]}}\n\\end{{document}}"
    );
    let script = json!({"turns": [
        {"steps": [tool("r", "read_file", json!({"path": "paper.tex"}))]},
        {"steps": [tool("e", "edit_document", json!({"path": "paper.tex", "base_revision": "$ref:r/revision",
            "edits": [{"old_text": "\\end{document}", "new_text": leak}]}))]},
        {"steps": [tool("b", "build_document", json!({"entry": "paper.tex", "engine": "pdflatex"}))]},
        {"steps": [{"text": "done"}]}
    ]});
    // Unattended task: restricted execution only, scoped to paper.tex.
    let req = request(
        a.project.path(),
        TaskScope::Document { path: p("paper.tex") },
        PermissionProfile::AutoApply,
        ExecutionMode::Restricted,
    );
    let (task, approvals) = run_task(&a.service, &mut a.events, script, req, false).await;
    let out = results(&a.service, &task.id);
    let (_, build_error, build) = &out[2];
    println!("restricted-mode build result (is_error={build_error}): {build}");
    let leaked = build.contains(OUTSIDE_SECRET);
    let ran_natively = task.usage.commands > 0 && !build.contains("never falls back") && !build.contains("refused");
    if leaked {
        println!("DEFECT: a file outside the project reached the agent's tool result: {OUTSIDE_SECRET}");
    }
    if ran_natively {
        println!(
            "DEFECT: restricted (unattended) task ran TeX natively (commands={}, approvals={approvals})",
            task.usage.commands
        );
    }

    // Approve-each task: a LaTeX build launches TeX natively but asks nobody.
    let script = json!({"turns": [
        {"steps": [tool("b", "build_document", json!({"entry": "paper.tex", "engine": "pdflatex"}))]},
        {"steps": [{"text": "done"}]}
    ]});
    let req = request(a.project.path(), TaskScope::Workspace, PermissionProfile::AutoApply, ExecutionMode::ApproveEach);
    let (task2, approvals2) = run_task(&a.service, &mut a.events, script, req, false).await;
    let (_, _, build2) = &results(&a.service, &task2.id)[0];
    println!("approve-each build result: {build2}");
    if approvals2 == 0 && task2.usage.commands > 0 {
        println!(
            "DEFECT: approve-each task launched TeX natively without asking (approvals=0, commands={})",
            task2.usage.commands
        );
    }

    assert!(!leaked, "DEFECT: file outside the project leaked through a LaTeX build: {build}");
    assert!(!ran_natively, "DEFECT: restricted task ran TeX natively without approval: {build}");
    assert!(approvals2 > 0 || task2.usage.commands == 0, "DEFECT: approve-each LaTeX build ran without approval");
}

// ---------------------------------------------------------------------------
// F2: Typst build diagnostics bypass scope and metadata-only consent.
// ---------------------------------------------------------------------------

/// A task scoped to `drafts/` with "File names only" consent and NO process
/// execution: read_file of the private file is refused (scope) and of any file
/// (consent), but the agent creates drafts/leak.typ with
/// `#panic(read("/private/secret.txt"))` and builds it in-process. The panic
/// message — the out-of-scope file's content — comes back in the build
/// diagnostics and goes to the model provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r_m5_f2_typst_build_diagnostics_reveal_out_of_scope_content_despite_metadata_only_consent() {
    let mut a = app();
    let script = json!({"turns": [
        {"steps": [
            tool("r1", "read_file", json!({"path": "private/secret.txt"})),
            tool("r2", "read_file", json!({"path": "drafts/readme.txt"})),
            tool("c", "create_file", json!({"path": "drafts/leak.typ", "content": "#panic(read(\"/private/secret.txt\"))\n"}))
        ]},
        {"steps": [tool("b", "build_document", json!({"entry": "drafts/leak.typ", "engine": "auto"}))]},
        {"steps": [{"text": "done"}]}
    ]});
    let mut req = request(
        a.project.path(),
        TaskScope::Files { paths: vec![p("drafts")] },
        PermissionProfile::AutoApply,
        ExecutionMode::Disabled,
    );
    req.consent = Some(ContextConsent::MetadataOnly);
    let (task, _) = run_task(&a.service, &mut a.events, script, req, false).await;
    let out = results(&a.service, &task.id);
    for (name, is_error, result) in &out {
        println!("{name} is_error={is_error}: {result}");
    }
    assert!(out[0].1 && out[0].2.contains("outside this task's scope"), "control: scope refusal");
    assert!(out[1].1 && out[1].2.contains("consent"), "control: consent refusal");
    let build = &out[3].2;
    assert!(
        !build.contains(IN_PROJECT_SECRET),
        "DEFECT: out-of-scope file content reached the model through Typst build diagnostics \
         (scope drafts/, metadata-only consent, execution disabled): {build}"
    );
}

// ---------------------------------------------------------------------------
// F3: run_python in the restricted sandbox ignores scope and consent.
// ---------------------------------------------------------------------------

/// Records process-tool calls; reports a restricted backend.
struct RecordingServices {
    python: Mutex<Vec<PythonRequest>>,
}

#[async_trait]
impl ToolServices for RecordingServices {
    fn available(&self, _kind: ServiceKind) -> bool {
        true
    }
    fn restricted_backend(&self) -> Option<String> {
        Some("docker".into())
    }
    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Typesetting))
    }
    async fn run_python(&self, request: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.python.lock().unwrap().push(request);
        Ok(json!({"status": "exited", "exitCode": 0}))
    }
    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Lean))
    }
    async fn run_command(&self, _: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Execution))
    }
}

/// run_command is refused unless the task is scoped to the whole project
/// ("Commands can affect any file"), but run_python is not: a task scoped to
/// one document with metadata-only consent runs Python unattended in the
/// restricted backend, which mounts the WHOLE project at /workspace — so the
/// code can print any project file back to the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r_m5_f3_restricted_run_python_ignores_scope_and_metadata_only_consent() {
    let recording = Arc::new(RecordingServices { python: Mutex::new(Vec::new()) });
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    std::fs::create_dir(project.path().join("private")).unwrap();
    std::fs::write(project.path().join("private/secret.txt"), IN_PROJECT_SECRET).unwrap();
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::new(DocumentService::new()),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: recording.clone(),
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
    let script = json!({"turns": [
        {"steps": [
            tool("c", "run_command", json!({"program": "cat", "args": ["private/secret.txt"], "working_directory": ""})),
            tool("py", "run_python", json!({"code": "print(open('/workspace/private/secret.txt').read())"}))
        ]},
        {"steps": [{"text": "done"}]}
    ]});
    let mut req = request(
        project.path(),
        TaskScope::Document { path: p("main.typ") },
        PermissionProfile::AutoApply,
        ExecutionMode::Restricted,
    );
    req.consent = Some(ContextConsent::MetadataOnly);
    let (task, approvals) = run_task(&service, &mut events, script, req, false).await;
    let out = results(&service, &task.id);
    println!("run_command: {:?}", out[0]);
    println!("run_python:  {:?}", out[1]);
    assert!(out[0].1 && out[0].2.contains("whole project"), "control: run_command is refused for a narrow scope");
    let calls = recording.python.lock().unwrap().clone();
    if let Some(call) = calls.first() {
        println!(
            "DEFECT: run_python ran unattended (approvals={approvals}) with root={} policy={:?} for a task scoped to main.typ \
             with metadata-only consent",
            call.root.display(),
            call.policy
        );
    }
    assert!(calls.is_empty(), "DEFECT: run_python was not refused for a document-scoped, metadata-only task");
}

/// Same as F3 on the real restricted backend (Docker, local image only; the
/// test never pulls). Shows the file content actually reaching the result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r_m5_f3_real_docker_run_python_reads_out_of_scope_file() {
    const IMAGE: &str = "python:3.13-slim";
    let present = std::process::Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", IMAGE])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !present {
        eprintln!("SKIPPED: docker or the local image {IMAGE} is not available (never pulled)");
        return;
    }
    let mut settings = ExecutionSettings::default();
    settings.docker.image = Some(IMAGE.into());
    let mut a = app_with(settings);
    let script = json!({"turns": [
        {"steps": [tool("py", "run_python", json!({"code": "print(open('/workspace/private/secret.txt').read())"}))]},
        {"steps": [{"text": "done"}]}
    ]});
    let mut req = request(
        a.project.path(),
        TaskScope::Document { path: p("main.typ") },
        PermissionProfile::AutoApply,
        ExecutionMode::Restricted,
    );
    req.consent = Some(ContextConsent::MetadataOnly);
    let (task, approvals) = run_task(&a.service, &mut a.events, script, req, false).await;
    let (_, is_error, result) = &results(&a.service, &task.id)[0];
    println!("run_python (approvals={approvals}, is_error={is_error}): {result}");
    assert!(
        !result.contains(IN_PROJECT_SECRET),
        "DEFECT: out-of-scope project file reached the model through restricted run_python: {result}"
    );
}

// ---------------------------------------------------------------------------
// F4: scope is checked on the requested spelling, not the resolved file.
// ---------------------------------------------------------------------------

/// Scope `public/` only. A junction public/link -> private (inside the project)
/// lets read_file("public/link/secret.txt") return private/secret.txt: the
/// lexical path passes the scope check and the workspace resolves the link.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r_m5_f4_link_inside_scope_reads_out_of_scope_file() {
    let mut a = app();
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(a.project.path().join("public").join("link"))
        .arg(a.project.path().join("private"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !made {
        eprintln!("SKIPPED: could not create a junction");
        return;
    }
    let script = json!({"turns": [
        {"steps": [
            tool("direct", "read_file", json!({"path": "private/secret.txt"})),
            tool("alias", "read_file", json!({"path": "public/link/secret.txt"}))
        ]},
        {"steps": [{"text": "done"}]}
    ]});
    let req = request(
        a.project.path(),
        TaskScope::Files { paths: vec![p("public")] },
        PermissionProfile::ReadOnly,
        ExecutionMode::Disabled,
    );
    let (task, _) = run_task(&a.service, &mut a.events, script, req, false).await;
    let out = results(&a.service, &task.id);
    println!("direct: {:?}\nalias:  {:?}", out[0], out[1]);
    assert!(out[0].1, "control: the direct out-of-scope read is refused");
    assert!(
        !out[1].2.contains(IN_PROJECT_SECRET),
        "DEFECT: out-of-scope file read through a link inside the scope: {}",
        out[1].2
    );
}

// ---------------------------------------------------------------------------
// F5: revert of a file the agent created and then edited.
// ---------------------------------------------------------------------------

/// The agent creates notes/new.typ and then edits it (both are this task's
/// changes; nobody else touched the file). Revert refuses to delete it,
/// claiming "The file changed after the agent created it", because agent
/// edits live in the unsaved buffer while the created file on disk still has
/// the original content.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "R-m5 F5: revert refuses to delete a file the agent created and then edited (false 'changed after the agent created it')"]
async fn r_m5_f5_revert_of_created_then_edited_file_is_refused() {
    let mut a = app();
    let script = json!({"turns": [
        {"steps": [tool("c", "create_file", json!({"path": "notes/new.typ", "content": "= Draft\nAlpha\n"}))]},
        {"steps": [tool("r", "read_file", json!({"path": "notes/new.typ"}))]},
        {"steps": [tool("e", "edit_document", json!({"path": "notes/new.typ", "base_revision": "$ref:r/revision",
            "edits": [{"old_text": "Alpha", "new_text": "Beta"}]}))]},
        {"steps": [{"text": "done"}]}
    ]});
    let req = request(a.project.path(), TaskScope::Workspace, PermissionProfile::AutoApply, ExecutionMode::Disabled);
    let (task, _) = run_task(&a.service, &mut a.events, script, req, false).await;
    assert!(results(&a.service, &task.id).iter().all(|(_, err, _)| !err), "setup: create, read, edit succeed");
    let reverted = a.service.revert(&task.id, None).unwrap();
    println!("revert: {reverted:?}");
    let still_there = a.project.path().join("notes/new.typ").exists();
    assert!(
        reverted.iter().all(|r| r.ok) && !still_there,
        "DEFECT: reverting the task did not remove the file it created (exists={still_there}): {reverted:?}"
    );
}

// ---------------------------------------------------------------------------
// F6: approval outlives a workspace switch.
// ---------------------------------------------------------------------------

/// The task's folder is checked when a call is gated, not when it runs after
/// approval. A create_file approval requested in project A, approved after
/// the user opened project B, creates the file in project B.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r_m5_f6_create_approved_after_workspace_switch_writes_into_other_project() {
    let mut a = app();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("main.typ"), "= Other project\n").unwrap();
    let script = json!({"turns": [
        {"steps": [tool("c", "create_file", json!({"path": "agent-note.typ", "content": "= From task A\n"}))]},
        {"steps": [{"text": "done"}]}
    ]});
    a.service.load_script(script).unwrap();
    let req = request(a.project.path(), TaskScope::Workspace, PermissionProfile::ReviewEdits, ExecutionMode::Disabled);
    let task = a.service.create_task(req).unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(AgentEvent::ApprovalRequested { approval }) = a.events.recv().await {
                break approval;
            }
        }
    })
    .await
    .expect("approval requested");
    // The user opens another project, then answers the pending approval.
    a.workspace.open_workspace(other.path()).unwrap();
    a.service.resolve_approval(&task.id, &approval.id, true).unwrap();
    tokio::time::timeout(Duration::from_secs(20), a.service.join(&task.id)).await.unwrap();
    let out = results(&a.service, &task.id);
    println!("create_file after switch: {:?}", out[0]);
    let in_other = other.path().join("agent-note.typ").exists();
    let in_own = a.project.path().join("agent-note.typ").exists();
    println!("created in task's project: {in_own}; created in the OTHER project: {in_other}");
    assert!(!in_other, "DEFECT: task A's approved create_file wrote into project B");
}

// ---------------------------------------------------------------------------
// F7: stop reasons the loop does not know are reported as success.
// ---------------------------------------------------------------------------

/// `model_context_window_exceeded` (the response was cut off because the
/// context window is full) ends the task as "Task completed". Its tool calls
/// are silently not run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "R-m5 F7: an unknown/cut-off stop reason (model_context_window_exceeded) is reported as Completed"]
async fn r_m5_f7_context_window_stop_is_reported_as_completed() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
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
    let script = json!({"turns": [{
        "steps": [{"text": "I will now edit the"}, tool("e", "list_files", json!({"path": "", "recursive": false}))],
        "stopReason": "model_context_window_exceeded"
    }]});
    let req = request(project.path(), TaskScope::Workspace, PermissionProfile::ReadOnly, ExecutionMode::Disabled);
    let (task, _) = run_task(&service, &mut events, script, req, false).await;
    println!("status: {:?}", task.status);
    assert_ne!(
        task.status,
        TaskStatus::Completed,
        "DEFECT: a response cut off by the context window (tool call not run) is shown as 'Task completed'"
    );
}

// ---------------------------------------------------------------------------
// F8: the API key follows a cross-origin redirect.
// ---------------------------------------------------------------------------

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(head_end) = text.find("\r\n\r\n") {
            let len = text[..head_end]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if buf.len() >= head_end + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// reqwest follows redirects by default and strips only standard auth headers
/// (Authorization, Cookie) on a host change; the custom `x-api-key` header and
/// the request body (document context) are replayed to the new host.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r_m5_f8_api_key_is_sent_to_a_redirect_target_on_another_host() {
    const KEY: &str = "sk-ant-review-m5-fake-key-0123456789";
    let other = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other_port = other.local_addr().unwrap().port();
    let seen_by_other = Arc::new(Mutex::new(String::new()));
    let seen = Arc::clone(&seen_by_other);
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = other.accept().await {
            let request = read_request(&mut socket).await;
            *seen.lock().unwrap() = request;
            let body = "{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"stop\"}}";
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_url = format!("http://{}", api.local_addr().unwrap());
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = api.accept().await {
            let _ = read_request(&mut socket).await;
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://localhost:{other_port}/v1/messages\r\nconnection: close\r\ncontent-length: 0\r\n\r\n"
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let provider = AnthropicProvider::with_base_url(
        &api_url,
        Secret::new(KEY.to_owned()),
        "claude-opus-5-5".into(),
        Effort::Low,
        RetryPolicy { max_retries: 0, base_delay: Duration::from_millis(10), max_delay: Duration::from_millis(10) },
    )
    .unwrap();
    let request = ProviderRequest {
        system: "system".into(),
        tools: vec![ToolDefinition {
            name: "read_file",
            description: "Read",
            input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        }],
        messages: vec![ProviderMessage {
            role: "user".into(),
            content: vec![json!({"type": "text", "text": "PRIVATE DOCUMENT CONTEXT"})],
        }],
        max_tokens: 1024,
    };
    let outcome =
        tokio::time::timeout(Duration::from_secs(20), provider.stream(&request, &|_| {}, &CancellationToken::new()))
            .await;
    println!("provider outcome: {outcome:?}");
    let other_saw = seen_by_other.lock().unwrap().clone();
    let leaked_key = other_saw.to_ascii_lowercase().contains(&format!("x-api-key: {}", KEY.to_ascii_lowercase()));
    let leaked_body = other_saw.contains("PRIVATE DOCUMENT CONTEXT");
    println!("redirect target received x-api-key={leaked_key} body={leaked_body}");
    assert!(!leaked_key, "DEFECT: the API key was sent to the redirect target on another host");
}

// ---------------------------------------------------------------------------
// F9: the user's undo of an agent edit leaves review offering a failing revert.
// ---------------------------------------------------------------------------

/// Undo history is shared by every origin, so the user's Ctrl+Z in the editor
/// undoes the agent's edit. Review still lists the file as "Pending review"
/// with an enabled Revert, and Revert is then refused as "Later edits overlap
/// the agent's changes" although the text is exactly the pre-task text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "R-m5 F9: after the user undoes an agent edit, review stays pending and Revert reports a false overlap"]
async fn r_m5_f9_user_undo_of_agent_edit_leaves_a_false_overlap_refusal() {
    use exposition_core::document::EditOrigin;
    let mut a = app();
    let script = json!({"turns": [
        {"steps": [tool("r", "read_file", json!({"path": "main.typ"}))]},
        {"steps": [tool("e", "edit_document", json!({"path": "main.typ", "base_revision": "$ref:r/revision",
            "edits": [{"old_text": "quickly", "new_text": "very quickly"}]}))]},
        {"steps": [{"text": "done"}]}
    ]});
    let req = request(a.project.path(), TaskScope::Workspace, PermissionProfile::AutoApply, ExecutionMode::Disabled);
    let (task, _) = run_task(&a.service, &mut a.events, script, req, false).await;
    let id = a.documents.find_by_path(&p("main.typ")).unwrap();
    assert!(a.documents.state(&id).unwrap().text.contains("very quickly"));
    // The user presses Ctrl+Z in the source editor.
    a.documents.undo(&id, EditOrigin::Source).unwrap();
    assert_eq!(a.documents.state(&id).unwrap().text, MAIN, "the user's undo removed the agent's edit");
    let review = a.service.review(&task.id).unwrap();
    println!("review after undo: {:?}", review.files[0]);
    let reverted = a.service.revert(&task.id, None).unwrap();
    println!("revert after undo: {reverted:?}");
    assert_eq!(a.documents.state(&id).unwrap().text, MAIN, "text unchanged by the refused revert");
    assert!(
        reverted[0].ok || !reverted[0].message.contains("overlap"),
        "DEFECT: the agent's change is already undone, but Revert reports overlapping edits: {}",
        reverted[0].message
    );
}

// ---------------------------------------------------------------------------
// F10: the approval for native code shows only its first 4000 characters.
// ---------------------------------------------------------------------------

/// Native services that accept everything (trusted folder), recording runs.
struct TrustedNativeServices {
    python: Mutex<Vec<PythonRequest>>,
}

#[async_trait]
impl ToolServices for TrustedNativeServices {
    fn available(&self, _kind: ServiceKind) -> bool {
        true
    }
    fn restricted_backend(&self) -> Option<String> {
        None
    }
    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Typesetting))
    }
    async fn run_python(&self, request: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.python.lock().unwrap().push(request);
        Ok(json!({"status": "succeeded"}))
    }
    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Lean))
    }
    async fn run_command(&self, _: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Execution))
    }
}

/// "Ask before each command (runs natively)": the approval request — the
/// card the user approves from — truncates the code at 4000 bytes, so code
/// after a long harmless prefix runs natively without ever being shown there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r_m5_f10_native_python_approval_hides_the_tail_of_the_code() {
    let services = Arc::new(TrustedNativeServices { python: Mutex::new(Vec::new()) });
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
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
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let mut events = service.subscribe();
    let tail = "import shutil; shutil.rmtree('C:/Users/someone/Documents')  # HIDDEN-TAIL";
    let code = format!("{}\n{tail}\n", "x = 1  # a long, harmless-looking prelude\n".repeat(110));
    let script = json!({"turns": [
        {"steps": [tool("py", "run_python", json!({"code": code}))]},
        {"steps": [{"text": "done"}]}
    ]});
    service.load_script(script).unwrap();
    let req = request(project.path(), TaskScope::Workspace, PermissionProfile::AutoApply, ExecutionMode::ApproveEach);
    let task = service.create_task(req).unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(AgentEvent::ApprovalRequested { approval }) = events.recv().await {
                break approval;
            }
        }
    })
    .await
    .expect("approval requested");
    println!(
        "approval detail ({} bytes) ends with: {:?}",
        approval.detail.len(),
        &approval.detail[approval.detail.len().saturating_sub(160)..]
    );
    service.resolve_approval(&task.id, &approval.id, true).unwrap();
    service.join(&task.id).await;
    let ran = services.python.lock().unwrap().first().map(|r| r.code.contains("HIDDEN-TAIL")).unwrap_or(false);
    println!("code that ran contains the hidden tail: {ran}");
    assert!(
        approval.detail.contains("HIDDEN-TAIL") || !ran,
        "DEFECT: the user approved native code whose tail was not in the approval request, and the tail ran"
    );
}

// ---------------------------------------------------------------------------
// HELD checks (pass on the current code; kept as regression evidence).
// ---------------------------------------------------------------------------

/// Restricted mode with a configured image that is not present locally: the
/// run is refused by the execution service and never runs natively.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r_m5_held_restricted_python_with_missing_image_is_refused_not_native() {
    let mut settings = ExecutionSettings::default();
    settings.docker.image = Some("exposition-review-m5-absent-image:0".into());
    let mut a = app_with(settings);
    let marker = a.project.path().join("ran-natively.txt");
    let code = format!("open(r'{}', 'w').write('x')\nprint('ran')", marker.display());
    let script = json!({"turns": [
        {"steps": [tool("py", "run_python", json!({"code": code}))]},
        {"steps": [{"text": "done"}]}
    ]});
    let req = request(a.project.path(), TaskScope::Workspace, PermissionProfile::AutoApply, ExecutionMode::Restricted);
    let (task, approvals) = run_task(&a.service, &mut a.events, script, req, false).await;
    let (_, is_error, result) = &results(&a.service, &task.id)[0];
    println!("restricted run with a missing image (approvals={approvals}): {result}");
    assert!(*is_error, "{result}");
    assert!(!result.contains("\"stdout\":\"ran"), "{result}");
    assert!(!marker.exists(), "nothing ran on the host");
    assert_eq!(approvals, 0);
}

// ---------------------------------------------------------------------------
// HELD: canceling a running native agent Python run.
// ---------------------------------------------------------------------------

/// Cancel a native agent Python run whose code ignores KeyboardInterrupt
/// (the research service must stop the kernel). Probed whether a cancel that
/// outlives the loop's 5 s grace mislabels a call that ran as "Not run": here
/// the cancel completes in about 3 s, the call is recorded as failed
/// ("canceled"), and the agent's research session is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r_m5_held_canceled_native_python_is_recorded_and_its_session_released() {
    use exposition_research::launcher::ExecutionLauncher;
    use exposition_research::types::{Availability, RuntimeLanguage};
    use exposition_research::{ResearchConfig, ResearchService};

    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    workspace.open_workspace(project.path()).unwrap();
    let mut config = ExecutionConfig::new(data.path());
    config.settings = Some(ExecutionSettings::default());
    config.too_broad_to_trust = Some(Vec::new());
    let execution = ExecutionService::with_config(config).unwrap();
    execution.register_workspace_root(project.path()).unwrap();
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        data.path().join("research"),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::clone(&documents),
    )));
    let report = research.environments(false).await.unwrap();
    if !report
        .runtimes
        .iter()
        .any(|r| r.language == RuntimeLanguage::Python && matches!(r.availability, Availability::Available))
    {
        eprintln!("SKIPPED: no Python research environment");
        return;
    }
    execution.grant_trust(&project.path().display().to_string()).unwrap();
    let services = AppToolServices::new(execution.clone())
        .with_workspace(Arc::clone(&workspace))
        .with_research(Arc::clone(&research));
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::clone(&documents),
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
    let mut events = service.subscribe();
    let marker = project.path().join("python-ran.txt");
    let code = format!(
        "open(r'{}', 'w').write('ran')\nimport time\nwhile True:\n    try:\n        time.sleep(0.5)\n    except KeyboardInterrupt:\n        pass\n",
        marker.display()
    );
    service
        .load_script(json!({"turns": [
            {"steps": [tool("py", "run_python", json!({"code": code}))]},
            {"steps": [{"text": "done"}]}
        ]}))
        .unwrap();
    let req = request(project.path(), TaskScope::Workspace, PermissionProfile::AutoApply, ExecutionMode::ApproveEach);
    let task = service.create_task(req).unwrap();
    let id = task.id.clone();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(AgentEvent::ApprovalRequested { approval }) = events.recv().await {
                service.resolve_approval(&id, &approval.id, true).unwrap();
                break;
            }
        }
    })
    .await
    .expect("approval requested");
    // Wait until the code has demonstrably run.
    tokio::time::timeout(Duration::from_secs(60), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the python code ran");
    let canceled_at = std::time::Instant::now();
    service.cancel_task(&id).unwrap();
    tokio::time::timeout(Duration::from_secs(120), service.join(&id)).await.expect("task stopped");
    println!("cancel took {:?}; status {:?}", canceled_at.elapsed(), service.task(&id).unwrap().status);
    println!(
        "agent research sessions left: {:?}",
        research.sessions().iter().map(|s| (&s.owner, &s.state)).collect::<Vec<_>>()
    );
    let detail = service.task_detail(&id).unwrap();
    let call = detail
        .transcript
        .iter()
        .find_map(|i| match i {
            TranscriptItem::ToolCall { name, status, result, .. } if name == "run_python" => {
                Some((*status, result.clone()))
            }
            _ => None,
        })
        .unwrap();
    println!("run_python recorded as {:?}: {:?}", call.0, call.1);
    assert!(marker.exists());
    assert_ne!(call.0, ToolCallStatus::NotRun, "a call that ran is not recorded as not run: {:?}", call.1);
    assert!(
        research.sessions().iter().all(|s| !matches!(s.owner, exposition_research::types::SessionOwner::Agent { .. })),
        "the agent's research session was released"
    );
}
