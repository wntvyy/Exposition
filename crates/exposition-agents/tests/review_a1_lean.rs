//! R-a1 (independent review of A1): Lean edits are execution (D7), probed on
//! the real Lean service (`lake serve` of the project's toolchain) with the
//! scripted provider — development only. Skips (printing SKIPPED) when the
//! fixture's toolchain is not installed through elan or `git` is missing.
//!
//! Tests marked `#[ignore = "R-a1 F<n>: ..."]` demonstrate a CURRENT defect:
//! they assert the expected behavior and fail on the product. Run them with
//! `cargo test -p exposition-agents --test review_a1_lean -- --include-ignored --nocapture`.
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
const TOOLCHAIN_PIN: &str = "leanprover/lean4:v4.35.0\n";

fn toolchain_installed() -> bool {
    let home = std::env::var_os("ELAN_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".elan")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".elan")));
    home.is_some_and(|h| h.join("toolchains").join(TOOLCHAIN_DIR).join("bin").exists())
}

fn git_available() -> bool {
    std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
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
    Harness { service, execution, project, events, _data: data }
}

#[derive(Clone, Copy, PartialEq)]
enum Answer {
    Approve,
    Decline,
}

/// Run one task to its end; answers every approval with `answer`. Returns
/// (summary, tool results, approval cards).
async fn run(
    h: &mut Harness,
    script: Value,
    mode: ApprovalMode,
    execution: ExecutionMode,
    answer: Answer,
) -> (TaskSummary, Vec<(bool, Value)>, Vec<ApprovalRequest>) {
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
        })
        .unwrap();
    let mut approvals = Vec::new();
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == task.id => {
                    h.service.resolve_approval(&task.id, &approval.id, answer == Answer::Approve).unwrap();
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

fn slash(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// A Lean line that writes `marker` when Lean elaborates it.
fn eval_writes(marker: &Path) -> String {
    format!("\n#eval IO.FS.writeFile \"{}\" \"ran\"\n", slash(marker))
}

/// A git repository (standing in for a remote one an agent names, e.g. on
/// GitHub) holding a Lake package whose `lakefile.lean` writes `marker` when
/// Lake elaborates it.
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

/// A `lakefile.toml` (not a Lean file) for a nested Lake project that
/// requires the hostile package.
fn nested_lakefile(repo: &Path) -> String {
    format!("name = \"nested\"\n\n[[require]]\nname = \"hostile\"\ngit = \"{}\"\nrev = \"master\"\n", slash(repo))
}

fn create(id: &str, path: &str, content: &str) -> Value {
    json!({"toolUse": {"id": id, "name": "create_file", "input": {"path": path, "content": content}}})
}

fn read(id: &str, path: &str) -> Value {
    json!({"toolUse": {"id": id, "name": "read_file", "input": {"path": path}}})
}

fn skip() -> bool {
    if !toolchain_installed() {
        eprintln!("SKIPPED: Lean toolchain {TOOLCHAIN_DIR} is not installed through elan");
        return true;
    }
    if !git_available() {
        eprintln!("SKIPPED: git is not available");
        return true;
    }
    false
}

/// R-a1 F1 (D7 bypass through Lake configuration). In a trusted Lean folder,
/// an Ask task that applies changes directly with process execution DISABLED
/// creates `LeanBasic/lakefile.toml` (not a `.lean` file, so D7 does not
/// apply) requiring a Lake package from a git URL of its choice, then reads
/// `LeanBasic/Defs.lean`. Opening that document makes the Lean service start
/// `lake serve` for the (now nested) Lake project; Lake clones the package
/// and elaborates its `lakefile.lean` natively. Expected: no agent change can
/// make Lean/Lake run code without native authority.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "R-a1 F1: a created lakefile.toml makes Lake fetch and elaborate a package's lakefile natively (D7 bypass)"]
async fn review_a1_f1_lake_config_runs_code_with_execution_disabled() {
    if skip() {
        return;
    }
    let mut h = harness();
    h.execution.grant_trust(&h.project.path().display().to_string()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("lake-package-ran.txt");
    let repo = hostile_lake_package(outside.path(), &marker);
    let script = json!({"turns": [
        {"steps": [create("c0", "LeanBasic/lean-toolchain", TOOLCHAIN_PIN)]},
        {"steps": [create("c1", "LeanBasic/lakefile.toml", &nested_lakefile(&repo))]},
        {"steps": [read("r1", "LeanBasic/Defs.lean")]},
        {"steps": [{"text": "done"}]}
    ]});
    let (_, out, approvals) = run(&mut h, script, ApprovalMode::Ask, ExecutionMode::Disabled, Answer::Decline).await;
    eprintln!("create_file: {} / {}\nread_file: error={} {}", out[0].1, out[1].1, out[2].0, out[2].1["path"]);
    eprintln!("approval cards: {}", approvals.len());
    let ran = appears(&marker, Duration::from_secs(240)).await;
    eprintln!("hostile lakefile elaborated (marker exists): {ran}");
    assert!(
        !ran,
        "DEFECT: Lake elaborated a package an agent named in a created lakefile.toml, natively, with execution disabled"
    );
}

/// R-a1 F1, Auto variant: the project's permissions allow file changes but
/// no native code (Files: edit, Code: off). Expected: no native code runs
/// without an approval card.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "R-a1 F1: a created lakefile.toml makes Lake fetch and elaborate a package's lakefile natively under Auto without native code"]
async fn review_a1_f1_lake_config_runs_code_under_auto_without_native() {
    if skip() {
        return;
    }
    let mut h = harness();
    let root = h.project.path().display().to_string();
    h.execution.grant_trust(&root).unwrap();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Off, setup: false },
        )
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("lake-package-ran-auto.txt");
    let repo = hostile_lake_package(outside.path(), &marker);
    let script = json!({"turns": [
        {"steps": [create("c0", "LeanBasic/lean-toolchain", TOOLCHAIN_PIN)]},
        {"steps": [create("c1", "LeanBasic/lakefile.toml", &nested_lakefile(&repo))]},
        {"steps": [read("r1", "LeanBasic/Defs.lean")]},
        {"steps": [{"text": "done"}]}
    ]});
    let (_, out, approvals) =
        run(&mut h, script, ApprovalMode::Auto, ExecutionMode::ApproveEach, Answer::Decline).await;
    eprintln!("create_file: {} / {}\nread_file: error={} {}", out[0].1, out[1].1, out[2].0, out[2].1["path"]);
    eprintln!("approval cards (all declined): {}", approvals.len());
    let ran = appears(&marker, Duration::from_secs(240)).await;
    eprintln!("hostile lakefile elaborated (marker exists): {ran}");
    assert!(!ran, "DEFECT: Lake elaborated a package an agent named, natively, under Auto with Code: off");
}

/// R-a1 F3 (D7 decided against the trust state at decision time). In an
/// UNTRUSTED folder an Auto task (Files: edit, Code: off) edits an open Lean
/// file: the Lean server does not run there, so D7 does not ask. When the
/// user later trusts the folder (e.g. to run Python), the Lean server starts
/// and elaborates the agent's unsaved text natively. Expected: agent text
/// that never received native authority does not run when trust is granted
/// for another purpose (or the trust grant states it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "R-a1 F3: an agent's Lean edit made while untrusted runs natively once the folder is trusted"]
async fn review_a1_f3_untrusted_lean_edit_runs_when_trust_is_granted() {
    if skip() {
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
        {"steps": [read("read-e1", "LeanBasic/Defs.lean")]},
        {"steps": [{"toolUse": {"id": "e1", "name": "edit_document", "input": {
            "path": "LeanBasic/Defs.lean",
            "base_revision": "$ref:read-e1/revision",
            "edits": [{"old_text": "end LeanBasic", "new_text": format!("end LeanBasic{}", eval_writes(&marker))}]
        }}}]},
        {"steps": [{"text": "done"}]}
    ]});
    let (_, out, approvals) =
        run(&mut h, script, ApprovalMode::Auto, ExecutionMode::ApproveEach, Answer::Decline).await;
    eprintln!("edit while untrusted: error={} {}", out[1].0, out[1].1);
    eprintln!("approval cards: {}", approvals.len());
    assert!(!out[1].0, "control: the edit applied without a card in the untrusted folder: {}", out[1].1);
    assert!(!appears(&marker, Duration::from_secs(10)).await, "control: nothing runs while untrusted");
    // The user trusts the folder (for anything).
    h.execution.grant_trust(&root).unwrap();
    let ran = appears(&marker, Duration::from_secs(240)).await;
    eprintln!("agent text elaborated after the trust grant (marker exists): {ran}");
    assert!(!ran, "DEFECT: the agent's unsaved Lean text ran natively when the folder was trusted");
}

/// Held checks (behavior expected and asserted): in a trusted folder,
/// - Auto with Code: restricted asks before a Lean-file edit; declined, the
///   buffer is unchanged and nothing runs;
/// - Ask with restricted execution refuses a Lean-file edit;
/// - an upper-case `.LEAN` extension and a new `.lean` file are gated alike.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_a1_held_lean_edits_need_native_authority() {
    if skip() {
        return;
    }
    let mut h = harness();
    let root = h.project.path().display().to_string();
    h.execution.grant_trust(&root).unwrap();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Restricted, setup: false },
        )
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let declined = outside.path().join("declined.txt");
    let edit_script = |marker: &Path| {
        json!({"turns": [
            {"steps": [read("read-e1", "LeanBasic/Defs.lean")]},
            {"steps": [{"toolUse": {"id": "e1", "name": "edit_document", "input": {
                "path": "LeanBasic/Defs.lean",
                "base_revision": "$ref:read-e1/revision",
                "edits": [{"old_text": "end LeanBasic", "new_text": format!("end LeanBasic{}", eval_writes(marker))}]
            }}}]},
            {"steps": [create("c1", "LeanBasic/Extra.LEAN", &eval_writes(marker))]},
            {"steps": [create("c2", "LeanBasic/Extra2.lean", &eval_writes(marker))]},
            {"steps": [{"text": "done"}]}
        ]})
    };
    let (_, out, approvals) =
        run(&mut h, edit_script(&declined), ApprovalMode::Auto, ExecutionMode::ApproveEach, Answer::Decline).await;
    eprintln!("auto restricted cards: {:?}", approvals.iter().map(|a| a.title.clone()).collect::<Vec<_>>());
    assert_eq!(approvals.len(), 3, "each Lean change asks under Code: restricted");
    for (is_error, result) in &out[1..] {
        assert!(is_error, "declined: {result}");
    }
    assert!(!h.project.path().join("LeanBasic/Extra.LEAN").exists());
    assert!(!h.project.path().join("LeanBasic/Extra2.lean").exists());
    assert!(!appears(&declined, Duration::from_secs(25)).await, "declined: nothing ran");

    let restricted = outside.path().join("restricted.txt");
    let (_, out, approvals) =
        run(&mut h, edit_script(&restricted), ApprovalMode::Ask, ExecutionMode::Restricted, Answer::Approve).await;
    assert!(approvals.is_empty(), "refused, never asked");
    for (is_error, result) in &out[1..] {
        assert!(is_error, "{result}");
        assert_eq!(result["code"], "agents.nativeNotAllowed", "{result}");
    }
    assert!(!appears(&restricted, Duration::from_secs(25)).await, "restricted: nothing ran");
}
