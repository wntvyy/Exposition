//! `save_document`: an agent saves one open document through the workspace
//! service's own save, gated as a change to that file.
//!
//! - `gate_*` tests ask the gate for its exact decision over the kinds of
//!   file a save can target (an ordinary script, Lean source, Lake
//!   configuration, an R startup file) × {Ask, Auto} × task settings or
//!   project profile × folder trust × task allowances, against an
//!   independent statement of the rules, and for every state in which only
//!   the user may decide what the file holds.
//! - `save_*` tests run a gated save and check what reached the disk.
//! - `loop_*` tests run the scripted provider — development only — through
//!   the real task loop on the real workspace service.
#![cfg(debug_assertions)]

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use exposition_agents::credentials::MemoryStore;
use exposition_agents::permissions::{CodeAccess, FilesAccess, PermissionChoice, ProjectPermissions};
use exposition_agents::store::Store;
use exposition_agents::tools::exec::{self, Allowances, Authority, Gate, SeenTexts, ToolEnv, ToolOutput, codes};
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
};
use exposition_agents::tools::{self, ToolCall};
use exposition_agents::workspace::{LocalProjectFiles, ProjectFiles};
use exposition_agents::*;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const SCRIPT: &str = "print(1)\n";
const VERSION: u64 = 7;

fn p(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("analysis.py"), SCRIPT).unwrap();
    std::fs::write(root.join("main.typ"), "= Notes\n").unwrap();
    std::fs::write(root.join("crlf.txt"), "one\r\ntwo\r\n").unwrap();
    std::fs::write(root.join("Main.lean"), "theorem t : 1 = 1 := rfl\n").unwrap();
    std::fs::write(root.join("lakefile.toml"), "name = \"n\"\n").unwrap();
    std::fs::write(root.join(".Rprofile"), "options(digits = 4)\n").unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/.Renviron"), "R_LIBS_USER=lib\n").unwrap();
    std::fs::write(root.join("sub/lean-toolchain"), "leanprover/lean4:v4.35.0\n").unwrap();
    for (folder, file) in
        [(".git", "config"), (".lake/packages/x", "X.lean"), (".venv/Lib", "site.py"), ("tools/venv", "x.py")]
    {
        std::fs::create_dir_all(root.join(folder)).unwrap();
        std::fs::write(root.join(folder).join(file), "x\n").unwrap();
    }
    dir
}

/// Trust is switchable; a command reports the script as it is on disk when
/// the command starts (what a process would read).
struct Services {
    trusted: AtomicBool,
    commands: Mutex<Vec<String>>,
}

impl Services {
    fn new() -> Arc<Self> {
        Arc::new(Self { trusted: AtomicBool::new(true), commands: Mutex::new(Vec::new()) })
    }
}

#[async_trait]
impl ToolServices for Services {
    fn available(&self, _kind: ServiceKind) -> bool {
        true
    }
    fn restricted_backend(&self) -> Option<String> {
        Some("docker".to_owned())
    }
    fn native_refusal(&self, _root: &Path) -> Option<String> {
        (!self.trusted.load(Ordering::SeqCst))
            .then(|| "This project folder is not trusted for native execution.".into())
    }
    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({"status": "succeeded"}))
    }
    async fn run_python(&self, _: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({"status": "succeeded"}))
    }
    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Ok(json!({"checked": true}))
    }
    async fn run_command(&self, request: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        let seen = std::fs::read_to_string(request.root.join("analysis.py")).unwrap_or_default();
        self.commands.lock().unwrap().push(seen.clone());
        Ok(json!({"status": "exited", "exitCode": 0, "scriptOnDisk": seen}))
    }
}

struct Fixture {
    dir: TempDir,
    #[allow(dead_code)]
    data: TempDir,
    files: LocalProjectFiles,
    documents: Arc<DocumentService>,
    workspace: Arc<WorkspaceService>,
    store: Store,
    services: Arc<Services>,
    task: TaskId,
    counter: std::sync::atomic::AtomicU64,
}

fn fixture() -> Fixture {
    fixture_in(project(), tempfile::tempdir().unwrap())
}

/// A fresh set of services over a project and data folder (a new session).
fn fixture_in(dir: TempDir, data: TempDir) -> Fixture {
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().to_path_buf());
    workspace.open_workspace(dir.path()).unwrap();
    let files = LocalProjectFiles::open(dir.path()).unwrap();
    Fixture {
        dir,
        data,
        files,
        documents,
        workspace,
        store: Store::open_in_memory().unwrap(),
        services: Services::new(),
        task: TaskId::new(),
        counter: std::sync::atomic::AtomicU64::new(0),
    }
}

impl Fixture {
    fn env<'a>(&'a self, spec: &'a TaskSpec, authority: Authority) -> ToolEnv<'a> {
        ToolEnv {
            task_id: &self.task,
            spec,
            files: &self.files,
            documents: &self.documents,
            workspace: Some(&self.workspace),
            services: self.services.as_ref(),
            store: &self.store,
            graph: None,
            authority,
        }
    }

    fn open(&self, path: &str) -> DocumentId {
        self.workspace.open_document(&p(path)).unwrap().state.info.id
    }

    /// Insert `text` at the start of the document, as `origin`.
    fn edit(&self, id: &DocumentId, text: &str, origin: EditOrigin) -> u64 {
        let revision = self.documents.info(id).unwrap().revision;
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let outcome = self
            .documents
            .apply(TransactionRequest {
                id: TransactionId(format!("test-{n}")),
                origin,
                edits: vec![DocumentEdit {
                    document: id.clone(),
                    base_revision: revision,
                    patches: vec![TextPatch::insert(0, text)],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        outcome.applied[0].revision
    }

    fn agent_edit(&self, id: &DocumentId, text: &str) -> u64 {
        self.edit(id, text, EditOrigin::Agent { task_id: self.task.0.clone() })
    }

    fn user_edit(&self, id: &DocumentId, text: &str) -> u64 {
        self.edit(id, text, EditOrigin::Source)
    }

    fn disk(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join(path)).unwrap()
    }
}

fn spec(root: &Path, mode: ApprovalMode, permission: PermissionProfile, execution: ExecutionMode) -> TaskSpec {
    TaskSpec {
        prompt: "save".into(),
        workspace_root: root.display().to_string(),
        scope: TaskScope::Workspace,
        permission,
        execution,
        consent: ContextConsent::ScopeContent,
        budgets: Budgets::default(),
        provider: ProviderChoice { provider: ProviderKind::Scripted, model: "scripted".into(), effort: Effort::Low },
        approval_mode: mode,
    }
}

/// An Ask task that applies changes directly and may run native code after approval.
fn direct(root: &Path) -> TaskSpec {
    spec(root, ApprovalMode::Ask, PermissionProfile::AutoApply, ExecutionMode::ApproveEach)
}

fn grant(choice: PermissionChoice) -> ProjectPermissions {
    ProjectPermissions {
        version: VERSION,
        granted_at_ms: 1,
        updated_at_ms: 1,
        files: choice.files,
        code: choice.code,
        setup: choice.setup,
    }
}

fn authority(project: Option<PermissionChoice>, allowed: &[AllowanceKind]) -> Authority {
    Authority { project: project.map(grant), allowances: Allowances { version: 3, kinds: allowed.to_vec() } }
}

fn save_call(path: &str, revision: u64) -> ToolCall {
    ToolCall::parse(tools::SAVE_DOCUMENT, &json!({"path": path, "revision": revision})).unwrap()
}

fn refused(gate: Gate) -> ToolOutput {
    match gate {
        Gate::Refused(output) => output,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

async fn run(f: &Fixture, spec: &TaskSpec, authority: Authority, call: ToolCall) -> ToolOutput {
    let env = f.env(spec, authority);
    let authorization = match exec::gate(&call, &env) {
        Gate::Allowed(authorization) => authorization,
        other => panic!("expected the save to be allowed, got {other:?}"),
    };
    exec::run(call, &env, &mut SeenTexts::default(), "save-1", authorization, CancellationToken::new()).await
}

// ---------------------------------------------------------------------------
// The gate: authority.
// ---------------------------------------------------------------------------

/// The files a save can target: (path, whether the application or a tool it
/// starts executes the file's text on its own).
const TARGETS: [(&str, bool); 6] = [
    ("analysis.py", false),
    ("main.typ", false),
    ("Main.lean", true),
    ("lakefile.toml", true),
    (".Rprofile", true),
    ("sub/.Renviron", true),
];

const ALLOWANCE_SETS: [&[AllowanceKind]; 5] = [
    &[],
    &[AllowanceKind::PackageSetup],
    &[AllowanceKind::NativeCode],
    &[AllowanceKind::DocumentChanges],
    &[AllowanceKind::NativeCode, AllowanceKind::DocumentChanges],
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Expect {
    Refused(&'static str),
    Approval(Vec<AllowanceKind>),
    Allowed(AuthorizedBy),
}

fn decision(mut needs: Vec<AllowanceKind>, mut used: Vec<AllowanceKind>, otherwise: AuthorizedBy) -> Expect {
    if !needs.is_empty() {
        needs.sort();
        return Expect::Approval(needs);
    }
    used.sort();
    Expect::Allowed(if used.is_empty() { otherwise } else { AuthorizedBy::TaskAllowance { kinds: used } })
}

/// Ask: a save is a file change (the task's change setting, or the user's
/// approval); text that runs on its own needs native-code authority as well,
/// which the task's execution setting can prohibit outright.
fn expect_ask(executed: bool, mode: ExecutionMode, permission: PermissionProfile, allowed: &[AllowanceKind]) -> Expect {
    if permission == PermissionProfile::ReadOnly {
        return Expect::Refused(codes::READ_ONLY);
    }
    if executed {
        match mode {
            ExecutionMode::Disabled => return Expect::Refused(codes::EXECUTION_DISABLED),
            ExecutionMode::Restricted => return Expect::Refused(codes::NATIVE_NOT_ALLOWED),
            ExecutionMode::ApproveEach => {}
        }
    }
    let (mut needs, mut used) = (Vec::new(), Vec::new());
    let mut need = |kind: AllowanceKind| if allowed.contains(&kind) { used.push(kind) } else { needs.push(kind) };
    if permission == PermissionProfile::ReviewEdits {
        need(AllowanceKind::DocumentChanges);
    }
    if executed {
        need(AllowanceKind::NativeCode);
    }
    decision(needs, used, AuthorizedBy::TaskSettings)
}

/// Auto: within the project's Files permission (and, for text that runs on
/// its own, its native Code permission); anything beyond asks.
fn expect_auto(executed: bool, profile: Option<PermissionChoice>, allowed: &[AllowanceKind]) -> Expect {
    let (mut needs, mut used) = (Vec::new(), Vec::new());
    let mut need = |kind: AllowanceKind| if allowed.contains(&kind) { used.push(kind) } else { needs.push(kind) };
    if profile.is_none_or(|p| p.files != FilesAccess::Edit) {
        need(AllowanceKind::DocumentChanges);
    }
    if executed && profile.is_none_or(|p| p.code != CodeAccess::Native) {
        need(AllowanceKind::NativeCode);
    }
    decision(needs, used, AuthorizedBy::ProjectPermissions { version: VERSION })
}

fn agrees(expected: &Expect, gate: &Gate) -> bool {
    match (expected, gate) {
        (Expect::Refused(code), Gate::Refused(out)) => out.is_error && out.content["code"] == json!(code),
        (Expect::Approval(for_task), Gate::NeedsApproval { for_task: got, authorization, .. }) => {
            got == for_task && authorization.policy().is_none()
        }
        (Expect::Allowed(by), Gate::Allowed(authorization)) => {
            authorization.authorized_by() == by && authorization.policy().is_none()
        }
        _ => false,
    }
}

/// Every target open with unsaved text; returns each one's revision.
fn dirty_targets(f: &Fixture) -> Vec<(&'static str, bool, u64)> {
    TARGETS
        .iter()
        .map(|(path, executed)| {
            let id = f.open(path);
            (*path, *executed, f.agent_edit(&id, "# agent\n"))
        })
        .collect()
}

#[test]
fn gate_ask_matrix_every_target_setting_trust_and_allowance() {
    let f = fixture();
    let targets = dirty_targets(&f);
    let before: Vec<Vec<u8>> = TARGETS.iter().map(|(path, _)| f.disk(path)).collect();
    let (mut failures, mut n) = (Vec::new(), 0);
    for (path, executed, revision) in &targets {
        let call = save_call(path, *revision);
        for mode in [ExecutionMode::Disabled, ExecutionMode::ApproveEach, ExecutionMode::Restricted] {
            for permission in
                [PermissionProfile::ReadOnly, PermissionProfile::ReviewEdits, PermissionProfile::AutoApply]
            {
                for trusted in [true, false] {
                    f.services.trusted.store(trusted, Ordering::SeqCst);
                    for allowed in ALLOWANCE_SETS {
                        n += 1;
                        let spec = spec(f.files.root(), ApprovalMode::Ask, permission, mode);
                        // A project record never applies to an Ask task.
                        let full = PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Native, setup: true };
                        let gate = exec::gate(&call, &f.env(&spec, authority(Some(full), allowed)));
                        let expected = expect_ask(*executed, mode, permission, allowed);
                        if !agrees(&expected, &gate) {
                            failures.push(format!(
                                "{path} {mode:?} {permission:?} trusted={trusted} {allowed:?}: expected {expected:?}, gate {gate:?}"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} of {n} combinations differ:\n{}", failures.len(), failures.join("\n"));
    let after: Vec<Vec<u8>> = TARGETS.iter().map(|(path, _)| f.disk(path)).collect();
    assert_eq!(before, after, "the gate itself writes nothing");
}

#[test]
fn gate_auto_matrix_every_target_profile_trust_and_allowance() {
    let f = fixture();
    let targets = dirty_targets(&f);
    let mut profiles = vec![None];
    for files in [FilesAccess::Off, FilesAccess::Edit] {
        for code in [CodeAccess::Off, CodeAccess::Restricted, CodeAccess::Native] {
            profiles.push(Some(PermissionChoice { files, code, setup: false }));
        }
    }
    let (mut failures, mut n) = (Vec::new(), 0);
    for (path, executed, revision) in &targets {
        let call = save_call(path, *revision);
        for profile in &profiles {
            for trusted in [true, false] {
                f.services.trusted.store(trusted, Ordering::SeqCst);
                for allowed in ALLOWANCE_SETS {
                    n += 1;
                    let spec = spec(
                        f.files.root(),
                        ApprovalMode::Auto,
                        PermissionProfile::AutoApply,
                        ExecutionMode::ApproveEach,
                    );
                    let gate = exec::gate(&call, &f.env(&spec, authority(*profile, allowed)));
                    let expected = expect_auto(*executed, *profile, allowed);
                    if !agrees(&expected, &gate) {
                        failures.push(format!(
                            "{path} {profile:?} trusted={trusted} {allowed:?}: expected {expected:?}, gate {gate:?}"
                        ));
                    }
                    // Whatever asks in Auto says why.
                    if let Gate::NeedsApproval { reason, .. } = &gate
                        && reason.as_deref().is_none_or(str::is_empty)
                    {
                        failures.push(format!("{path} {profile:?}: an Auto approval without a reason"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} of {n} combinations differ:\n{}", failures.len(), failures.join("\n"));
}

/// An Auto task's own ceiling prohibits (never asks): a read-only task
/// saves nothing, and a task without native execution never saves text that
/// runs on its own.
#[test]
fn gate_auto_task_ceiling_refuses_and_never_asks() {
    let f = fixture();
    let targets = dirty_targets(&f);
    let full = PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Native, setup: true };
    let all = [AllowanceKind::NativeCode, AllowanceKind::DocumentChanges];
    for (path, executed, revision) in &targets {
        let call = save_call(path, *revision);
        let read_only =
            spec(f.files.root(), ApprovalMode::Auto, PermissionProfile::ReadOnly, ExecutionMode::ApproveEach);
        let out = refused(exec::gate(&call, &f.env(&read_only, authority(Some(full), &all))));
        assert_eq!(out.content["code"], codes::READ_ONLY, "{path}");
        for (mode, code) in [
            (ExecutionMode::Disabled, codes::EXECUTION_DISABLED),
            (ExecutionMode::Restricted, codes::NATIVE_NOT_ALLOWED),
        ] {
            let spec = spec(f.files.root(), ApprovalMode::Auto, PermissionProfile::AutoApply, mode);
            let gate = exec::gate(&call, &f.env(&spec, authority(Some(full), &all)));
            if *executed {
                assert_eq!(refused(gate).content["code"], code, "{path} {mode:?}");
            } else {
                assert!(matches!(gate, Gate::Allowed(_)), "{path} {mode:?}: {gate:?}");
            }
        }
    }
}

/// The approval states what the file is: saving text that runs on its own
/// says so, in a trusted folder and in one that is not trusted yet.
#[test]
fn gate_approval_for_executed_text_states_the_consequence() {
    let f = fixture();
    let targets = dirty_targets(&f);
    let spec = direct(f.files.root());
    for (path, executed, revision) in targets.iter().filter(|t| t.1) {
        assert!(executed);
        for trusted in [true, false] {
            f.services.trusted.store(trusted, Ordering::SeqCst);
            let gate = exec::gate(&save_call(path, *revision), &f.env(&spec, authority(None, &[])));
            let Gate::NeedsApproval { title, detail, reason, for_task, .. } = gate else {
                panic!("{path}: expected an approval, got {gate:?}")
            };
            assert_eq!(title, format!("Save {path}"));
            assert_eq!(for_task, vec![AllowanceKind::NativeCode]);
            let reason = reason.unwrap();
            assert!(reason.contains("natively"), "{path}: {reason}");
            assert!(detail.contains("with your account's privileges"), "{path}: {detail}");
            assert_eq!(!trusted, detail.contains("not trusted yet"), "{path} trusted={trusted}: {detail}");
        }
    }
}

/// Protected folders, the graph's storage, and service-owned records are
/// never saved by an agent, whatever authority the task has.
#[test]
fn gate_protected_targets_are_refused_under_every_authority() {
    let f = fixture();
    let full = PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Native, setup: true };
    let all = [AllowanceKind::NativeCode, AllowanceKind::DocumentChanges, AllowanceKind::PackageSetup];
    let specs = [
        direct(f.files.root()),
        spec(f.files.root(), ApprovalMode::Auto, PermissionProfile::AutoApply, ExecutionMode::ApproveEach),
    ];
    for (path, code) in [
        (".git/config", codes::PROTECTED_TARGET),
        (".GIT/config", codes::PROTECTED_TARGET),
        (".lake/packages/x/X.lean", codes::PROTECTED_TARGET),
        (".venv/Lib/site.py", codes::PROTECTED_TARGET),
        ("tools/venv/x.py", codes::PROTECTED_TARGET),
        (".exposition/graph.sqlite3", codes::GRAPH_STORAGE),
        (".exposition/runs/run-1/record.json", codes::SERVICE_OWNED),
        ("../outside.py", codes::INVALID_PATH),
    ] {
        for spec in &specs {
            let out = refused(exec::gate(&save_call(path, 1), &f.env(spec, authority(Some(full), &all))));
            assert_eq!(out.content["code"], code, "{path}: {out:?}");
        }
    }
    assert_eq!(f.disk(".git/config"), b"x\n");
}

/// A save is confined to the task's scope like any other change.
#[test]
fn gate_a_save_outside_the_scope_is_refused() {
    let f = fixture();
    let id = f.open("analysis.py");
    let revision = f.agent_edit(&id, "# agent\n");
    let mut spec = direct(f.files.root());
    spec.scope = TaskScope::Document { path: p("main.typ") };
    let out = refused(exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))));
    assert_eq!(out.content["code"], codes::OUT_OF_SCOPE);
    assert_eq!(f.disk("analysis.py"), SCRIPT.as_bytes());
}

// ---------------------------------------------------------------------------
// The gate and the save: document states only the user decides.
// ---------------------------------------------------------------------------

#[test]
fn gate_refuses_documents_that_are_not_open_clean_or_at_another_revision() {
    let f = fixture();
    let spec = direct(f.files.root());
    let gate = |path: &str, revision: u64| exec::gate(&save_call(path, revision), &f.env(&spec, authority(None, &[])));

    let out = refused(gate("analysis.py", 1));
    assert!(out.content["error"].as_str().unwrap().contains("is not open in the editor"), "{out:?}");

    let id = f.open("analysis.py");
    let clean = f.documents.info(&id).unwrap().revision;
    let out = refused(gate("analysis.py", clean));
    assert_eq!(out.content["code"], codes::NOTHING_TO_SAVE, "{out:?}");

    let revision = f.agent_edit(&id, "# agent\n");
    let out = refused(gate("analysis.py", revision - 1));
    assert_eq!(out.content["currentRevision"], revision, "{out:?}");
    assert!(out.content["error"].as_str().unwrap().contains("Nothing was saved"), "{out:?}");
    let out = refused(gate("analysis.py", revision + 1));
    assert_eq!(out.content["currentRevision"], revision, "{out:?}");

    assert!(matches!(gate("analysis.py", revision), Gate::Allowed(_)));
    assert_eq!(f.disk("analysis.py"), SCRIPT.as_bytes(), "nothing was written by any of these");
}

/// Without the workspace's document service there is no save: the tool never
/// writes the file itself.
#[test]
fn gate_without_the_workspace_service_nothing_is_saved() {
    let f = fixture();
    let id = f.open("analysis.py");
    let revision = f.agent_edit(&id, "# agent\n");
    let spec = direct(f.files.root());
    let mut env = f.env(&spec, authority(None, &[]));
    env.workspace = None;
    let out = refused(exec::gate(&save_call("analysis.py", revision), &env));
    assert_eq!(out.content["code"], codes::SERVICE_UNAVAILABLE, "{out:?}");
    assert_eq!(f.disk("analysis.py"), SCRIPT.as_bytes());
}

/// The file changed on disk since the buffer's base: refused before and
/// after the workspace noticed (a recorded conflict), never overwritten.
#[test]
fn gate_refuses_a_file_that_changed_on_disk() {
    let f = fixture();
    let spec = direct(f.files.root());
    let id = f.open("analysis.py");
    let revision = f.agent_edit(&id, "# agent\n");
    let external = "print('edited in another program')\n";
    std::fs::write(f.dir.path().join("analysis.py"), external).unwrap();

    let out = refused(exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))));
    assert_eq!(out.content["code"], codes::SAVE_CONFLICT, "{out:?}");
    assert_eq!(out.content["reason"], "file.modifiedOnDisk");
    assert_eq!(out.content["saved"], false);
    let message = out.content["error"].as_str().unwrap();
    assert!(message.contains("changed on disk") && message.contains("Only the user resolves this"), "{message}");

    // The watcher's check records the conflict; it stays refused.
    f.workspace.check_document(&id);
    let out = refused(exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))));
    assert_eq!(out.content["code"], codes::SAVE_CONFLICT, "{out:?}");
    assert_eq!(out.content["reason"], "document.conflict");
    assert_eq!(f.disk("analysis.py"), external.as_bytes(), "the other program's version is untouched");
    assert!(f.documents.state(&id).unwrap().text.starts_with("# agent\n"), "the buffer keeps its text");

    // The user decides (keeps the buffer): from then on it is an ordinary unsaved document.
    f.workspace.keep_buffer(&id).unwrap();
    assert!(matches!(
        exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))),
        Gate::Allowed(_)
    ));
}

/// A file deleted (or moved) on disk is not brought back by an agent's save.
#[test]
fn gate_refuses_a_file_that_was_deleted_on_disk() {
    let f = fixture();
    let spec = direct(f.files.root());
    let id = f.open("analysis.py");
    let revision = f.agent_edit(&id, "# agent\n");
    std::fs::remove_file(f.dir.path().join("analysis.py")).unwrap();
    let out = refused(exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))));
    assert_eq!(out.content["code"], codes::SAVE_CONFLICT, "{out:?}");
    assert!(out.content["error"].as_str().unwrap().contains("deleted or moved on disk"), "{out:?}");
    f.workspace.check_document(&id);
    let revision = f.documents.info(&id).unwrap().revision;
    let out = refused(exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))));
    assert_eq!(out.content["reason"], "document.conflict", "{out:?}");
    assert!(!f.dir.path().join("analysis.py").exists(), "the file was not recreated");
}

/// Recovery: neither a buffer restored from a recovery journal nor a file
/// whose recovery data still waits for the user's decision is saved.
#[test]
fn gate_refuses_documents_in_a_recovery_state() {
    let f = fixture();
    // An earlier session left unsaved text for the script.
    let id = f.open("analysis.py");
    f.user_edit(&id, "# unsaved in an earlier session\n");
    f.workspace.journal_documents(&HashSet::from([id]));
    // The application ends without a save; a new session opens the folder.
    let Fixture { dir, data, .. } = f;
    let f = fixture_in(dir, data);
    let spec = direct(f.files.root());
    let entries = f.workspace.recovery_entries().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");

    // The recovery data waits for a decision: an agent edit of the file is not saved over it.
    let id = f.open("analysis.py");
    let revision = f.agent_edit(&id, "# agent\n");
    let out = refused(exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))));
    assert_eq!(out.content["code"], codes::SAVE_CONFLICT, "{out:?}");
    assert_eq!(out.content["reason"], "document.recoveryPending");
    assert_eq!(f.disk("analysis.py"), SCRIPT.as_bytes());

    // The user reloads the file and restores the recovered text: only they save it.
    f.workspace.reload_document(&id).unwrap();
    f.workspace.restore_recovery(&entries[0].id).unwrap();
    let revision = f.agent_edit(&id, "# agent again\n");
    let out = refused(exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))));
    assert_eq!(out.content["code"], codes::SAVE_CONFLICT, "{out:?}");
    assert_eq!(out.content["reason"], "document.recovered");
    assert_eq!(f.disk("analysis.py"), SCRIPT.as_bytes());

    // After the user's own save the document is ordinary again.
    f.workspace.save_document(&id, false).unwrap();
    let revision = f.agent_edit(&id, "# later\n");
    assert!(matches!(
        exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))),
        Gate::Allowed(_)
    ));
}

#[tokio::test]
async fn save_writes_exactly_the_buffer_through_the_workspace_save() {
    let f = fixture();
    let spec = direct(f.files.root());
    let id = f.open("analysis.py");
    f.agent_edit(&id, "print(2)\n");
    let revision = f.agent_edit(&id, "# agent\n");
    let out = run(&f, &spec, authority(None, &[]), save_call("analysis.py", revision)).await;
    assert!(!out.is_error, "{out:?}");
    let disk = f.disk("analysis.py");
    assert_eq!(disk, b"# agent\nprint(2)\nprint(1)\n");
    assert_eq!(out.content["saved"], true);
    assert_eq!(out.content["path"], "analysis.py");
    assert_eq!(out.content["revision"], revision);
    assert_eq!(out.content["bytes"], disk.len());
    assert_eq!(out.content["contentHash"], sha(&disk));
    assert_eq!(out.content["unsavedChanges"], false);
    assert_eq!(out.content["otherChanges"], "none");
    assert!(out.content["note"].as_str().unwrap().contains("Saved to disk"), "{out:?}");
    // The document service knows the revision is saved (no dot on the tab, no conflict later).
    let info = f.documents.info(&id).unwrap();
    assert_eq!(info.saved_revision, Some(revision));
    f.workspace.check_document(&id);
    assert_eq!(f.documents.info(&id).unwrap().revision, revision, "the save is not seen as an external change");

    // Saving again has nothing to write.
    let again = refused(exec::gate(&save_call("analysis.py", revision), &f.env(&spec, authority(None, &[]))));
    assert_eq!(again.content["code"], codes::NOTHING_TO_SAVE);
}

/// The save keeps the file's line endings and encoding (it is the editor's save).
#[tokio::test]
async fn save_keeps_line_endings() {
    let f = fixture();
    let spec = direct(f.files.root());
    let id = f.open("crlf.txt");
    let revision = f.agent_edit(&id, "zero\r\n");
    let out = run(&f, &spec, authority(None, &[]), save_call("crlf.txt", revision)).await;
    assert!(!out.is_error, "{out:?}");
    assert_eq!(f.disk("crlf.txt"), b"zero\r\none\r\ntwo\r\n");
}

/// A document that changes between the gate's decision and the save is not
/// saved: the save itself checks the revision and the file again.
#[tokio::test]
async fn save_is_refused_when_the_buffer_or_the_file_changed_after_the_gate() {
    let f = fixture();
    let spec = direct(f.files.root());
    let id = f.open("analysis.py");
    let revision = f.agent_edit(&id, "# agent\n");
    let call = save_call("analysis.py", revision);

    // The user types after the gate allowed the save.
    let env = f.env(&spec, authority(None, &[]));
    let Gate::Allowed(authorization) = exec::gate(&call, &env) else { panic!("allowed") };
    f.user_edit(&id, "# typed meanwhile\n");
    let out =
        exec::run(call.clone(), &env, &mut SeenTexts::default(), "s1", authorization, CancellationToken::new()).await;
    assert!(out.is_error, "{out:?}");
    assert_eq!(out.content["code"], codes::CHANGED_SINCE_APPROVAL, "{out:?}");
    assert_eq!(f.disk("analysis.py"), SCRIPT.as_bytes());

    // Another program writes the file after the gate allowed the save.
    let revision = f.documents.info(&id).unwrap().revision;
    let call = save_call("analysis.py", revision);
    let Gate::Allowed(authorization) = exec::gate(&call, &env) else { panic!("allowed") };
    std::fs::write(f.dir.path().join("analysis.py"), "external\n").unwrap();
    let out = exec::run(call, &env, &mut SeenTexts::default(), "s2", authorization, CancellationToken::new()).await;
    assert_eq!(out.content["code"], codes::SAVE_CONFLICT, "{out:?}");
    assert_eq!(f.disk("analysis.py"), b"external\n");
}

/// What the buffer holds besides this task's edits is disclosed: in the
/// approval and in the result.
#[tokio::test]
async fn save_discloses_changes_the_task_did_not_make() {
    let f = fixture();
    let review = spec(f.files.root(), ApprovalMode::Ask, PermissionProfile::ReviewEdits, ExecutionMode::ApproveEach);
    let detail =
        |path: &str, revision: u64| match exec::gate(&save_call(path, revision), &f.env(&review, authority(None, &[])))
        {
            Gate::NeedsApproval { detail, .. } => detail,
            other => panic!("expected an approval, got {other:?}"),
        };
    let own = "Every unsaved change in this file was made by this task.";
    let others = "was also changed outside this task (your own unsaved edits, or another task's)";

    // Only this task's edits.
    let id = f.open("analysis.py");
    let revision = f.agent_edit(&id, "# agent\n");
    let shown = detail("analysis.py", revision);
    assert!(shown.contains(own) && !shown.contains(others), "{shown}");
    assert!(shown.contains(&format!("revision {revision} of the editor text")), "{shown}");
    assert!(shown.contains("+ # agent\n") && shown.contains("  print(1)"), "{shown}");
    let buffer = f.documents.state(&id).unwrap().text;
    assert!(shown.contains(&sha(buffer.as_bytes())), "the approval names the exact bytes: {shown}");

    // The user's own unsaved edit is in the buffer too.
    let revision = f.user_edit(&id, "# the user's unsaved line\n");
    let shown = detail("analysis.py", revision);
    assert!(shown.contains(others) && !shown.contains(own), "{shown}");
    assert!(shown.contains("+ # the user's unsaved line\n"), "what is saved is shown in full: {shown}");
    assert!(shown.contains("a save writes the whole current text"), "{shown}");
    let direct = direct(f.files.root());
    let out = run(&f, &direct, authority(None, &[]), save_call("analysis.py", revision)).await;
    assert_eq!(out.content["otherChanges"], "included", "{out:?}");
    assert!(out.content["otherChangesNote"].as_str().unwrap().contains("Tell the user"), "{out:?}");
    assert_eq!(f.disk("analysis.py"), b"# the user's unsaved line\n# agent\nprint(1)\n");

    // After that save, a further edit by this task alone is again only its own.
    let revision = f.agent_edit(&id, "# more\n");
    assert!(detail("analysis.py", revision).contains(own));
    // Another task's edit is not this task's.
    let revision = f.edit(&id, "# other task\n", EditOrigin::Agent { task_id: "another-task".into() });
    assert!(detail("analysis.py", revision).contains(others));
    // An undo by the user is a change this task did not make either.
    f.documents.undo(&id, EditOrigin::Source).unwrap();
    let revision = f.documents.info(&id).unwrap().revision;
    assert!(detail("analysis.py", revision).contains(others));

    // When the history no longer reaches the saved revision, it says it cannot tell.
    let id = f.open("main.typ");
    let mut revision = 0;
    for _ in 0..2100 {
        revision = f.agent_edit(&id, "x");
    }
    let shown = detail("main.typ", revision);
    assert!(shown.contains("cannot tell which of the unsaved changes this task made"), "{shown}");
    assert!(shown.contains("The whole current editor text is saved"), "{shown}");
    let out = run(&f, &direct, authority(None, &[]), save_call("main.typ", revision)).await;
    assert_eq!(out.content["otherChanges"], "unknown", "{out:?}");
}

/// Invisible and direction-changing characters in what is saved are shown.
#[test]
fn gate_approval_shows_hidden_characters_in_the_saved_text() {
    let f = fixture();
    let review = spec(f.files.root(), ApprovalMode::Ask, PermissionProfile::ReviewEdits, ExecutionMode::ApproveEach);
    let id = f.open("analysis.py");
    let revision = f.agent_edit(&id, "x = 1 \u{202E}# hidden\n");
    let Gate::NeedsApproval { detail, .. } =
        exec::gate(&save_call("analysis.py", revision), &f.env(&review, authority(None, &[])))
    else {
        panic!("expected an approval")
    };
    assert!(detail.contains("⟨U+202E⟩") && detail.contains("they are part of what is saved"), "{detail}");
}

// ---------------------------------------------------------------------------
// Through the task loop, on the real workspace service.
// ---------------------------------------------------------------------------

struct Harness {
    service: AgentService,
    services: Arc<Services>,
    workspace: Arc<WorkspaceService>,
    documents: Arc<DocumentService>,
    events: broadcast::Receiver<AgentEvent>,
    project: TempDir,
    _data: TempDir,
}

fn harness() -> Harness {
    let project = project();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    workspace.open_workspace(project.path()).unwrap();
    let graph =
        exposition_graph::GraphService::new(exposition_graph::GraphConfig { data_dir: data.path().join("graph") });
    graph.attach(&std::fs::canonicalize(project.path()).unwrap());
    let services = Services::new();
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::clone(&documents),
        workspace: Some(Arc::clone(&workspace)),
        credentials: Arc::new(MemoryStore::default()),
        services: services.clone(),
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
    Harness { service, services, workspace, documents, events, project, _data: data }
}

impl Harness {
    fn disk(&self, path: &str) -> String {
        std::fs::read_to_string(self.project.path().join(path)).unwrap()
    }

    fn start(&self, script: Value, mode: ApprovalMode, permission: PermissionProfile) -> TaskSummary {
        self.service.load_script(script).unwrap();
        self.service
            .create_task(CreateTaskRequest {
                prompt: "Change the script and run it".into(),
                workspace_root: self.project.path().display().to_string(),
                scope: TaskScope::Workspace,
                permission,
                execution: ExecutionMode::ApproveEach,
                consent: Some(ContextConsent::ScopeContent),
                budgets: None,
                approval_mode: Some(mode),
            })
            .unwrap()
    }

    /// Run the task to its end, answering each approval with `answer`.
    async fn drive(
        &mut self,
        task: &TaskId,
        mut answer: impl FnMut(&ApprovalRequest) -> Option<ApprovalScope>,
    ) -> Vec<ApprovalRequest> {
        let mut cards = Vec::new();
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                match self.events.recv().await {
                    Ok(AgentEvent::ApprovalRequested { approval }) if &approval.task_id == task => {
                        match answer(&approval) {
                            Some(scope) => {
                                self.service.resolve_approval_scoped(task, &approval.id, true, scope).unwrap()
                            }
                            None => self.service.resolve_approval(task, &approval.id, false).unwrap(),
                        }
                        cards.push(approval);
                    }
                    Ok(AgentEvent::TaskChanged { task: t }) if &t.id == task && !t.status.is_active() => break,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => panic!("events closed: {e}"),
                }
            }
        })
        .await
        .expect("task finished");
        self.service.join(task).await;
        cards
    }

    /// (status, how it was authorized, result) of a call.
    fn call(&self, task: &TaskId, id: &str) -> (ToolCallStatus, Option<AuthorizedBy>, Value) {
        self.service
            .task_detail(task)
            .unwrap()
            .transcript
            .into_iter()
            .find_map(|item| match item {
                TranscriptItem::ToolCall { tool_use_id, status, authorized_by, result, .. } if tool_use_id == id => {
                    Some((status, authorized_by, serde_json::from_str(&result.unwrap_or_default()).unwrap()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no call {id}"))
    }
}

/// Read the script, change it, save it, run it.
fn edit_save_run(path: &str, old: &str, new: &str) -> Value {
    json!({"turns": [
        {"steps": [{"toolUse": {"id": "r", "name": "read_file", "input": {"path": path}}}]},
        {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {
            "path": path, "base_revision": "$ref:r/revision", "edits": [{"old_text": old, "new_text": new}]}}}]},
        {"steps": [{"toolUse": {"id": "s", "name": "save_document", "input": {"path": path, "revision": "$ref:e/revision"}}}]},
        {"steps": [{"toolUse": {"id": "c", "name": "run_command", "input": {
            "program": "python", "args": ["analysis.py"], "working_directory": ""}}}]},
        {"steps": [{"text": "done"}]}
    ]})
}

/// Ask, every change reviewed: the save asks with its own card (a file
/// change, repeatable as one); approved once, the script on disk is the
/// edited one when the command starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_ask_an_approved_save_reaches_the_disk_before_the_run() {
    let mut h = harness();
    let task = h.start(
        edit_save_run("analysis.py", "print(1)", "print(2)"),
        ApprovalMode::Ask,
        PermissionProfile::ReviewEdits,
    );
    let cards = h.drive(&task.id, |_| Some(ApprovalScope::Once)).await;
    let titles: Vec<&str> = cards.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Apply edits to analysis.py", "Save analysis.py", "Run python"]);
    let save = &cards[1];
    assert_eq!(save.tool, "save_document");
    assert_eq!(save.for_task, vec![AllowanceKind::DocumentChanges]);
    assert!(save.detail.contains("- print(1)\n+ print(2)"), "{}", save.detail);
    assert!(save.detail.contains("Every unsaved change in this file was made by this task."), "{}", save.detail);

    let (status, by, result) = h.call(&task.id, "s");
    assert_eq!(status, ToolCallStatus::Succeeded);
    assert_eq!(by, Some(AuthorizedBy::Approved { for_task: false }));
    assert_eq!(result["saved"], true);
    assert_eq!(result["contentHash"], sha(b"print(2)\n"));
    assert!(result["authorization"].as_str().unwrap().contains("approved this call in a prompt, once"), "{result}");
    assert_eq!(h.disk("analysis.py"), "print(2)\n");
    // The command read the saved script, and its result has no unsaved-files note.
    assert_eq!(*h.services.commands.lock().unwrap(), ["print(2)\n"]);
    let (_, _, run) = h.call(&task.id, "c");
    assert_eq!(run["scriptOnDisk"], "print(2)\n");
    assert!(run.get("unsavedNotSeen").is_none(), "{run}");
    // The edit's own result said where the text was and how it gets saved.
    let (_, _, edit) = h.call(&task.id, "e");
    let note = edit["note"].as_str().unwrap();
    assert!(note.contains("not saved to disk") && note.contains("save_document saves it"), "{note}");
}

/// Declined: nothing reaches the disk, the buffer keeps the edit, and the
/// command reads the old script and says which file it did not see.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_ask_a_declined_save_leaves_the_file_unchanged() {
    let mut h = harness();
    let task = h.start(
        edit_save_run("analysis.py", "print(1)", "print(2)"),
        ApprovalMode::Ask,
        PermissionProfile::ReviewEdits,
    );
    h.drive(&task.id, |card| (card.tool != "save_document").then_some(ApprovalScope::Once)).await;
    let (status, by, result) = h.call(&task.id, "s");
    assert_eq!(status, ToolCallStatus::Declined);
    assert_eq!(by, None);
    assert!(result["error"].as_str().unwrap().contains("declined"), "{result}");
    assert_eq!(h.disk("analysis.py"), SCRIPT);
    let id = h.documents.find_by_path(&p("analysis.py")).unwrap();
    assert_eq!(h.documents.state(&id).unwrap().text, "print(2)\n");
    assert_ne!(h.documents.info(&id).unwrap().saved_revision, Some(h.documents.info(&id).unwrap().revision));
    let (_, _, run) = h.call(&task.id, "c");
    assert_eq!(run["scriptOnDisk"], SCRIPT);
    assert_eq!(run["unsavedNotSeen"], json!(["analysis.py"]));
    assert!(run["filesNote"].as_str().unwrap().contains("save_document saves one of them"), "{run}");
}

/// "Approve for this task" on the edit covers the save (the same kind of
/// change): no second card, and the result says what covered it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_ask_approve_for_this_task_covers_the_save() {
    let mut h = harness();
    let task = h.start(
        edit_save_run("analysis.py", "print(1)", "print(2)"),
        ApprovalMode::Ask,
        PermissionProfile::ReviewEdits,
    );
    let cards = h
        .drive(&task.id, |card| {
            Some(if card.tool == "edit_document" { ApprovalScope::Task } else { ApprovalScope::Once })
        })
        .await;
    let tools: Vec<&str> = cards.iter().map(|c| c.tool.as_str()).collect();
    assert_eq!(tools, ["edit_document", "run_command"], "the save did not ask");
    let (status, by, result) = h.call(&task.id, "s");
    assert_eq!(status, ToolCallStatus::Succeeded);
    assert_eq!(by, Some(AuthorizedBy::TaskAllowance { kinds: vec![AllowanceKind::DocumentChanges] }));
    assert!(result["authorization"].as_str().unwrap().starts_with("No prompt: covered by what the user approved"));
    assert_eq!(h.disk("analysis.py"), "print(2)\n");
}

/// An Ask task that applies changes directly saves without a card (its own
/// settings), and nothing is saved by the edit alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_ask_direct_changes_save_without_a_card_and_never_on_their_own() {
    let mut h = harness();
    // An edit without a save: nothing saves automatically, whatever the task may do.
    let edit_only = json!({"turns": [
        {"steps": [{"toolUse": {"id": "r", "name": "read_file", "input": {"path": "main.typ"}}}]},
        {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {
            "path": "main.typ", "base_revision": "$ref:r/revision", "edits": [{"old_text": "Notes", "new_text": "Saved?"}]}}}]},
        {"steps": [{"text": "done"}]}
    ]});
    let task = h.start(edit_only, ApprovalMode::Ask, PermissionProfile::AutoApply);
    assert!(h.drive(&task.id, |_| Some(ApprovalScope::Task)).await.is_empty());
    assert_eq!(h.disk("main.typ"), "= Notes\n");

    let task =
        h.start(edit_save_run("analysis.py", "print(1)", "print(2)"), ApprovalMode::Ask, PermissionProfile::AutoApply);
    let cards = h.drive(&task.id, |_| Some(ApprovalScope::Once)).await;
    assert_eq!(cards.iter().map(|c| c.tool.as_str()).collect::<Vec<_>>(), ["run_command"]);
    let (status, by, result) = h.call(&task.id, "s");
    assert_eq!(status, ToolCallStatus::Succeeded);
    assert_eq!(by, Some(AuthorizedBy::TaskSettings));
    assert!(result["authorization"].as_str().unwrap().starts_with("No prompt: this task's settings allow it"));
    assert_eq!(h.disk("analysis.py"), "print(2)\n");
    assert_eq!(h.disk("main.typ"), "= Notes\n", "the other task's edit is still only in the editor");
}

/// Auto: within the project's Files permission a save proceeds; beyond it,
/// it asks and says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_auto_saves_within_the_files_permission_and_asks_beyond_it() {
    let mut h = harness();
    let root = h.project.path().display().to_string();
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Edit, code: CodeAccess::Native, setup: false },
        )
        .unwrap();
    let task =
        h.start(edit_save_run("analysis.py", "print(1)", "print(2)"), ApprovalMode::Auto, PermissionProfile::AutoApply);
    assert!(h.drive(&task.id, |_| None).await.is_empty(), "within the project's permissions: no card");
    let (status, by, result) = h.call(&task.id, "s");
    assert_eq!(status, ToolCallStatus::Succeeded);
    assert!(matches!(by, Some(AuthorizedBy::ProjectPermissions { .. })), "{by:?}");
    assert!(result["authorization"].as_str().unwrap().contains("the project's permissions cover it"), "{result}");
    assert_eq!(h.disk("analysis.py"), "print(2)\n");
    assert_eq!(*h.services.commands.lock().unwrap(), ["print(2)\n"]);

    // Files: off — the edit and the save are beyond the permissions; the save is declined.
    h.service
        .set_project_permissions(
            &root,
            PermissionChoice { files: FilesAccess::Off, code: CodeAccess::Native, setup: false },
        )
        .unwrap();
    let task =
        h.start(edit_save_run("analysis.py", "print(2)", "print(3)"), ApprovalMode::Auto, PermissionProfile::AutoApply);
    let cards = h.drive(&task.id, |card| (card.tool != "save_document").then_some(ApprovalScope::Once)).await;
    let save = cards.iter().find(|c| c.tool == "save_document").expect("the save asked");
    assert_eq!(save.reason.as_deref(), Some("Changing files is beyond this project's permissions (Files: off)."));
    assert_eq!(save.for_task, vec![AllowanceKind::DocumentChanges]);
    assert_eq!(h.call(&task.id, "s").0, ToolCallStatus::Declined);
    assert_eq!(h.disk("analysis.py"), "print(2)\n", "declined: the file keeps the earlier save");
}

/// The file changes on disk while the save waits for approval: approving
/// does not overwrite it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_a_conflict_that_appears_before_the_approval_refuses_the_save() {
    let mut h = harness();
    let target = h.project.path().join("analysis.py");
    let task = h.start(
        edit_save_run("analysis.py", "print(1)", "print(2)"),
        ApprovalMode::Ask,
        PermissionProfile::ReviewEdits,
    );
    h.drive(&task.id, |card| {
        if card.tool == "save_document" {
            std::fs::write(&target, "print('another program')\n").unwrap();
        }
        Some(ApprovalScope::Once)
    })
    .await;
    let (status, by, result) = h.call(&task.id, "s");
    assert_eq!(status, ToolCallStatus::Failed, "{result}");
    assert_eq!(by, None, "it did not run");
    assert_eq!(result["code"], codes::SAVE_CONFLICT, "{result}");
    assert_eq!(h.disk("analysis.py"), "print('another program')\n");
    assert_eq!(*h.services.commands.lock().unwrap(), ["print('another program')\n"]);
}

/// The user types in the file while the save waits for approval: what was
/// approved is no longer what would be saved, so nothing is saved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_a_buffer_changed_before_the_approval_is_not_saved() {
    let mut h = harness();
    let documents = Arc::clone(&h.documents);
    let task = h.start(
        edit_save_run("analysis.py", "print(1)", "print(2)"),
        ApprovalMode::Ask,
        PermissionProfile::ReviewEdits,
    );
    h.drive(&task.id, |card| {
        if card.tool == "save_document" {
            let id = documents.find_by_path(&p("analysis.py")).unwrap();
            let revision = documents.info(&id).unwrap().revision;
            documents
                .apply(TransactionRequest {
                    id: TransactionId("typed".into()),
                    origin: EditOrigin::Source,
                    edits: vec![DocumentEdit {
                        document: id,
                        base_revision: revision,
                        patches: vec![TextPatch::insert(0, "# typed while the card was open\n")],
                    }],
                    undo: UndoBehavior::NewGroup,
                    conflict: ConflictPolicy::Reject,
                    label: None,
                    selection: None,
                })
                .unwrap();
        }
        Some(ApprovalScope::Once)
    })
    .await;
    let (status, _, result) = h.call(&task.id, "s");
    assert_eq!(status, ToolCallStatus::Failed, "{result}");
    assert!(result["error"].as_str().unwrap().contains("Nothing was saved"), "{result}");
    assert_eq!(h.disk("analysis.py"), SCRIPT);
}

/// The user's own unsaved edit in the file is disclosed on the card and in the result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_the_users_unsaved_changes_are_disclosed() {
    let mut h = harness();
    let id = h.workspace.open_document(&p("analysis.py")).unwrap().state.info.id;
    h.documents
        .apply(TransactionRequest {
            id: TransactionId("user".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: id,
                base_revision: 1,
                patches: vec![TextPatch::insert(0, "# my unsaved note\n")],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    let task = h.start(
        edit_save_run("analysis.py", "print(1)", "print(2)"),
        ApprovalMode::Ask,
        PermissionProfile::ReviewEdits,
    );
    let cards = h.drive(&task.id, |_| Some(ApprovalScope::Once)).await;
    let save = cards.iter().find(|c| c.tool == "save_document").unwrap();
    assert!(save.detail.contains("was also changed outside this task (your own unsaved edits"), "{}", save.detail);
    assert!(save.detail.contains("+ # my unsaved note\n"), "{}", save.detail);
    let (_, _, result) = h.call(&task.id, "s");
    assert_eq!(result["otherChanges"], "included", "{result}");
    assert_eq!(h.disk("analysis.py"), "# my unsaved note\nprint(2)\n");
}

/// Agent text reaches an R startup file on disk only with native-code
/// authority: the edit needs it, and so does the save, each on its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_agent_text_reaches_an_r_startup_file_only_with_native_authority() {
    let script = || {
        json!({"turns": [
            {"steps": [{"toolUse": {"id": "r", "name": "read_file", "input": {"path": ".Rprofile"}}}]},
            {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {
                "path": ".Rprofile", "base_revision": "$ref:r/revision",
                "edits": [{"old_text": "options(digits = 4)", "new_text": "system(\"calc\")"}]}}}]},
            {"steps": [{"toolUse": {"id": "s", "name": "save_document", "input": {"path": ".Rprofile", "revision": "$ref:e/revision"}}}]},
            {"steps": [{"text": "done"}]}
        ]})
    };
    let before = "options(digits = 4)\n";
    for trusted in [true, false] {
        let mut h = harness();
        h.services.trusted.store(trusted, Ordering::SeqCst);
        // The edit is approved once; the save asks again for native code, and is declined.
        let task = h.start(script(), ApprovalMode::Ask, PermissionProfile::AutoApply);
        let cards = h.drive(&task.id, |card| (card.tool != "save_document").then_some(ApprovalScope::Once)).await;
        assert_eq!(cards.iter().map(|c| c.tool.as_str()).collect::<Vec<_>>(), ["edit_document", "save_document"]);
        assert_eq!(cards[1].for_task, vec![AllowanceKind::NativeCode]);
        assert!(cards[1].reason.as_deref().unwrap().contains("R runs this file natively"), "{:?}", cards[1].reason);
        assert_eq!(h.call(&task.id, "s").0, ToolCallStatus::Declined);
        assert_eq!(h.disk(".Rprofile"), before, "trusted={trusted}");
    }
    // A task without native execution cannot put the text there at all.
    let mut h = harness();
    h.service.load_script(script()).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "profile".into(),
            workspace_root: h.project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::Disabled,
            consent: Some(ContextConsent::ScopeContent),
            budgets: None,
            approval_mode: Some(ApprovalMode::Ask),
        })
        .unwrap();
    assert!(h.drive(&task.id, |_| Some(ApprovalScope::Task)).await.is_empty());
    assert_eq!(h.call(&task.id, "e").2["code"], codes::EXECUTION_DISABLED);
    assert_eq!(h.disk(".Rprofile"), before);
}
