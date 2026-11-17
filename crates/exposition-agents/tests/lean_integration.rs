//! `check_lean` on the real Lean service (`lake serve` of the project's
//! toolchain), driven by the scripted provider — development only. Skips
//! (printing SKIPPED) when the fixture's toolchain is not installed via elan.
#![cfg(debug_assertions)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::permissions::{CodeAccess, FilesAccess, PermissionChoice};
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
    workspace: Arc<WorkspaceService>,
    documents: Arc<DocumentService>,
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
    // The project graph, attached to the open project (agents require it with a workspace).
    let graph =
        exposition_graph::GraphService::new(exposition_graph::GraphConfig { data_dir: data.path().join("graph") });
    graph.attach(&std::fs::canonicalize(project.path()).unwrap());
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
        documents: Arc::clone(&documents),
        workspace: Some(Arc::clone(&workspace)),
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(services),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
        graph: Some(graph),
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let events = service.subscribe();
    Harness { service, execution, workspace, documents, project, events, _data: data }
}

fn check(id: &str, path: &str) -> Value {
    json!({"toolUse": {"id": id, "name": "check_lean", "input": {"path": path}}})
}

async fn run(
    h: &mut Harness,
    script: Value,
    execution: ExecutionMode,
) -> (TaskSummary, Vec<(bool, Value)>, Vec<ApprovalRequest>) {
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
            approval_mode: None,
            provider: None,
            attachments: None,
        })
        .unwrap();
    let mut approvals = Vec::new();
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == task.id => {
                    h.service.resolve_approval(&task.id, &approval.id, true).unwrap();
                    approvals.push(approval);
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
    assert!(approvals.is_empty());
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
    assert_eq!(approvals.len(), 2, "each Lean check is approved");
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

/// Wait (bounded) until `path` exists.
async fn appears(path: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    path.exists()
}

/// A Lean line that writes `marker` when Lean elaborates it.
fn eval_writes(marker: &Path) -> String {
    let target = marker.display().to_string().replace('\\', "/");
    format!("\n#eval IO.FS.writeFile \"{target}\" \"ran\"\n")
}

/// D7 (R-m9): the live Lean server elaborates the unsaved text of every open
/// Lean document in a trusted folder (the Lean service tracks each document
/// service document whose path ends in `.lean` and sends its buffer, so an
/// agent's edit is elaborated as soon as it is applied). A change that puts
/// text into a Lean file is therefore native execution.
///
/// Fail-before: an Ask task that applies changes directly with execution
/// disabled could make that edit before A1, and the code in it ran. Now the
/// edit is refused (execution disabled), the buffer is unchanged, and nothing
/// runs; with per-command approval it asks first and says that Lean
/// elaborates the text natively.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lean_file_edits_are_native_execution() {
    if !toolchain_installed() {
        eprintln!("SKIPPED: Lean toolchain {TOOLCHAIN_DIR} is not installed through elan");
        return;
    }
    let mut h = harness();
    h.execution.grant_trust(&h.project.path().display().to_string()).unwrap();
    let markers = tempfile::tempdir().unwrap();
    let path = exposition_core::path::WorkspacePath::parse("LeanBasic/Defs.lean").unwrap();

    // 1. The elaboration path itself: an agent-origin transaction on the open
    // buffer (never saved) makes Lean run the code.
    let direct = markers.path().join("direct.txt");
    let opened = h.workspace.open_document(&path).unwrap();
    let id = opened.state.info.id.clone();
    let text = h.documents.snapshot(&id).unwrap().text;
    let end = exposition_core::text::byte_to_utf16(&text, text.len()).unwrap() as u32;
    h.documents
        .apply(exposition_core::document::TransactionRequest {
            id: exposition_core::document::TransactionId("agent-probe".into()),
            origin: exposition_core::document::EditOrigin::Agent { task_id: "task-probe".into() },
            edits: vec![exposition_core::document::DocumentEdit {
                document: id.clone(),
                base_revision: opened.state.info.revision,
                patches: vec![exposition_core::document::TextPatch::insert(end, eval_writes(&direct))],
            }],
            undo: exposition_core::document::UndoBehavior::NewGroup,
            conflict: exposition_core::document::ConflictPolicy::RebaseDisjoint,
            label: None,
            selection: None,
        })
        .unwrap();
    assert!(appears(&direct, Duration::from_secs(300)).await, "Lean elaborated the unsaved agent edit natively");
    assert!(
        !std::fs::read_to_string(h.project.path().join("LeanBasic/Defs.lean")).unwrap().contains("#eval"),
        "never saved"
    );
    // Undo the probe so the next steps start from the saved text.
    h.documents.undo(&id, exposition_core::document::EditOrigin::Agent { task_id: "task-probe".into() }).unwrap();

    // 2. Fail-before: execution disabled refuses the edit; nothing reaches the buffer.
    let refused = markers.path().join("refused.txt");
    let edit = |id: &str, marker: &Path| {
        json!({"toolUse": {"id": id, "name": "edit_document", "input": {
            "path": "LeanBasic/Defs.lean",
            "base_revision": format!("$ref:read-{id}/revision"),
            "edits": [{"old_text": "end LeanBasic", "new_text": format!("end LeanBasic{}", eval_writes(marker))}]
        }}})
    };
    let read = |id: &str| json!({"toolUse": {"id": format!("read-{id}"), "name": "read_file", "input": {"path": "LeanBasic/Defs.lean"}}});
    let script =
        json!({"turns": [{"steps": [read("e1")]}, {"steps": [edit("e1", &refused)]}, {"steps": [{"text": "done"}]}]});
    let (_, out, approvals) = run(&mut h, script, ExecutionMode::Disabled).await;
    assert!(approvals.is_empty(), "refused, never asked");
    let (is_error, result) = &out[1];
    assert!(is_error, "{result}");
    assert_eq!(result["code"], "agents.executionDisabled", "{result}");
    assert!(result["error"].as_str().unwrap().contains("Lean elaborates"), "{result}");
    assert!(!h.documents.snapshot(&id).unwrap().text.contains("refused.txt"), "the buffer is unchanged");
    assert!(!appears(&refused, Duration::from_secs(20)).await, "nothing ran");

    // 3. With per-command approval it asks first, saying what happens; approved, it runs.
    let approved = markers.path().join("approved.txt");
    let script =
        json!({"turns": [{"steps": [read("e2")]}, {"steps": [edit("e2", &approved)]}, {"steps": [{"text": "done"}]}]});
    let (_, out, approvals) = run(&mut h, script, ExecutionMode::ApproveEach).await;
    assert_eq!(approvals.len(), 1, "one card for the Lean edit");
    assert_eq!(approvals[0].title, "Apply edits to LeanBasic/Defs.lean");
    assert!(
        approvals[0].detail.contains("Lean elaborates the text of an open Lean file natively"),
        "{}",
        approvals[0].detail
    );
    assert_eq!(approvals[0].for_task, vec![AllowanceKind::NativeCode]);
    assert!(!out[1].0, "{}", out[1].1);
    assert!(appears(&approved, Duration::from_secs(300)).await, "approved: Lean elaborated it");
}

fn git_available() -> bool {
    std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn slash(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// A git repository (standing in for a remote one an agent names) holding a
/// Lake package whose `lakefile.lean` writes `marker` when Lake elaborates it.
/// (Adapted from the independent review's R-a1 F1 repro.)
fn hostile_lake_package(dir: &Path, marker: &Path) -> PathBuf {
    let repo = dir.join("hostile-lake-package");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("lean-toolchain"), "leanprover/lean4:v4.35.0\n").unwrap();
    std::fs::write(
        repo.join("lakefile.lean"),
        format!("import Lake\nopen Lake DSL\n{}\npackage hostile\n", eval_writes(marker)),
    )
    .unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.email=r@example.invalid", "-c", "user.name=review", "-c", "core.autocrlf=false"])
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "master"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "hostile"]);
    repo
}

/// A nested Lake project's `lakefile.toml` that requires the hostile package.
fn nested_lakefile(repo: &Path) -> String {
    format!("name = \"nested\"\n\n[[require]]\nname = \"hostile\"\ngit = \"{}\"\nrev = \"master\"\n", slash(repo))
}

/// Run one task in `mode`, answering every approval with `approve`; the
/// tool results and the approval cards.
async fn run_mode(
    h: &mut Harness,
    script: Value,
    mode: ApprovalMode,
    execution: ExecutionMode,
    approve: bool,
) -> (Vec<(bool, Value)>, Vec<ApprovalRequest>) {
    h.service.load_script(script).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "Work on the Lean project".into(),
            workspace_root: h.project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution,
            consent: Some(ContextConsent::ScopeContent),
            budgets: None,
            approval_mode: Some(mode),
            provider: None,
            attachments: None,
        })
        .unwrap();
    let mut approvals = Vec::new();
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == task.id => {
                    h.service.resolve_approval(&task.id, &approval.id, approve).unwrap();
                    approvals.push(approval);
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
    (results, approvals)
}

/// R-a1 F1 (generalized D7): in a trusted Lean folder an agent with only
/// change authority creates `LeanBasic/lean-toolchain` and
/// `LeanBasic/lakefile.toml` requiring a Lake package from a git URL of its
/// choice, then reads `LeanBasic/Defs.lean`. Before the fix the Lean service
/// started `lake serve` for the nested project, Lake cloned the package and
/// elaborated its `lakefile.lean` natively (the marker appeared). Now Lake
/// configuration is execution: with execution disabled the creations are
/// refused (naming Lake), under Auto without native code they ask (declined
/// here), so nothing is created and nothing runs; reading the Lean file is
/// still allowed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lake_configuration_from_an_agent_cannot_run_code_without_native_authority() {
    if !toolchain_installed() {
        eprintln!("SKIPPED: Lean toolchain {TOOLCHAIN_DIR} is not installed through elan");
        return;
    }
    if !git_available() {
        eprintln!("SKIPPED: git is not available");
        return;
    }
    let mut h = harness();
    let root = h.project.path().display().to_string();
    h.execution.grant_trust(&root).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("lake-package-ran.txt");
    let repo = hostile_lake_package(outside.path(), &marker);
    let script = || {
        json!({"turns": [
            {"steps": [{"toolUse": {"id": "c0", "name": "create_file", "input": {"path": "LeanBasic/lean-toolchain", "content": "leanprover/lean4:v4.35.0\n"}}}]},
            {"steps": [{"toolUse": {"id": "c1", "name": "create_file", "input": {"path": "LeanBasic/lakefile.toml", "content": nested_lakefile(&repo)}}}]},
            {"steps": [{"toolUse": {"id": "r1", "name": "read_file", "input": {"path": "LeanBasic/Defs.lean"}}}]},
            {"steps": [{"text": "done"}]}
        ]})
    };
    // Ask, changes applied directly, execution disabled: refused, never asked.
    let (out, approvals) = run_mode(&mut h, script(), ApprovalMode::Ask, ExecutionMode::Disabled, true).await;
    assert!(approvals.is_empty(), "refused, never asked");
    for (is_error, result) in &out[..2] {
        assert!(is_error, "{result}");
        assert_eq!(result["code"], "agents.executionDisabled", "{result}");
        assert!(result["error"].as_str().unwrap().contains("Lake"), "{result}");
    }
    assert!(!out[2].0, "reading the Lean file is allowed: {}", out[2].1);
    assert!(!h.project.path().join("LeanBasic/lakefile.toml").exists());
    assert!(!h.project.path().join("LeanBasic/lean-toolchain").exists());

    // Auto with file changes but no native code: each asks once; declined.
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Off, setup: false },
        )
        .unwrap();
    let (out, approvals) = run_mode(&mut h, script(), ApprovalMode::Auto, ExecutionMode::ApproveEach, false).await;
    assert_eq!(approvals.len(), 2, "one card per Lake configuration file");
    for approval in &approvals {
        assert!(approval.detail.contains("Lake reads Lean and Lake configuration natively"), "{}", approval.detail);
    }
    assert!(out[0].0 && out[1].0, "declined");
    assert!(!h.project.path().join("LeanBasic/lakefile.toml").exists());
    // The Lean file was opened twice; the hostile package never ran.
    assert!(!appears(&marker, Duration::from_secs(90)).await, "Lake ran a package an agent named");
}

/// R-a1 F3: D7 does not depend on trust at decision time. In a folder that
/// is NOT trusted, an Auto task with file changes but no native code
/// (Files: edit, Code: off) edits an open Lean file, creates a Lean file, and
/// creates Lake configuration. Before the fix the edit applied with no card
/// (no Lean server runs while untrusted), and when the user later trusted the
/// folder Lean elaborated the agent's unsaved text natively (the marker
/// appeared). Now each change asks first, saying Lean/Lake run it natively
/// once the folder is trusted; declined, nothing changes, and trusting the
/// folder afterwards runs nothing of the agent's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_untrusted_lean_change_needs_native_authority_before_trust_is_granted() {
    if !toolchain_installed() {
        eprintln!("SKIPPED: Lean toolchain {TOOLCHAIN_DIR} is not installed through elan");
        return;
    }
    let mut h = harness();
    let root = h.project.path().display().to_string();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Off, setup: false },
        )
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("untrusted-edit-ran.txt");
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "read-e1", "name": "read_file", "input": {"path": "LeanBasic/Defs.lean"}}}]},
        {"steps": [{"toolUse": {"id": "e1", "name": "edit_document", "input": {
            "path": "LeanBasic/Defs.lean",
            "base_revision": "$ref:read-e1/revision",
            "edits": [{"old_text": "end LeanBasic", "new_text": format!("end LeanBasic{}", eval_writes(&marker))}]
        }}}]},
        {"steps": [{"toolUse": {"id": "c1", "name": "create_file", "input": {"path": "LeanBasic/Extra.lean", "content": eval_writes(&marker)}}}]},
        {"steps": [{"toolUse": {"id": "c2", "name": "create_file", "input": {"path": "LeanBasic/lakefile.toml", "content": "name = \"nested\"\n"}}}]},
        {"steps": [{"text": "done"}]}
    ]});
    let (out, approvals) = run_mode(&mut h, script, ApprovalMode::Auto, ExecutionMode::ApproveEach, false).await;
    assert_eq!(approvals.len(), 3, "each Lean change asks, though the folder is not trusted yet");
    assert!(approvals[0].detail.contains("once it is trusted, code in this text"), "{}", approvals[0].detail);
    assert!(approvals[2].detail.contains("once it is trusted, Lake may download"), "{}", approvals[2].detail);
    for (is_error, result) in &out[1..] {
        assert!(is_error, "declined: {result}");
    }
    assert!(!h.project.path().join("LeanBasic/Extra.lean").exists());
    assert!(!h.project.path().join("LeanBasic/lakefile.toml").exists());
    // The user trusts the folder (for anything): nothing of the agent's runs.
    h.execution.grant_trust(&root).unwrap();
    assert!(!appears(&marker, Duration::from_secs(90)).await, "the agent's Lean text ran once the folder was trusted");
}
