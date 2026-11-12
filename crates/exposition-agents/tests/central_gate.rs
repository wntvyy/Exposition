//! The central gate: every tool kind × execution mode × permission × scope ×
//! consent, against an independent statement of the rules.
//!
//! - `gate_*` tests ask the gate itself (`exec::gate`) for its exact decision.
//! - `loop_*` tests run each combination through the real task loop (scripted
//!   provider — development only — with every approval granted) and record
//!   what actually reached the services, the disk, and the documents. They
//!   fail if any tool acts outside the decision, whatever its own code does:
//!   a process started with execution disabled or read-only, natively without
//!   an approval or in a restricted-only task, or with a narrow scope or
//!   metadata-only consent; a change in a read-only task; a result carrying a
//!   file the task may not read.
#![cfg(debug_assertions)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use exposition_agents::credentials::MemoryStore;
use exposition_agents::store::Store;
use exposition_agents::tools::exec::{self, Gate, ToolEnv};
use exposition_agents::tools::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
};
use exposition_agents::tools::{self, ToolCall};
use exposition_agents::workspace::{LocalProjectFiles, ProjectFiles};
use exposition_agents::*;
use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord};
use exposition_core::job::ExecutionPolicy;
use exposition_core::path::WorkspacePath;
use exposition_workspace::{DocumentService, OpenDocument};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const MAIN: &str = "= Notes\n\nThe squares grow quickly.\n";
const SECRET: &str = "OUTOFSCOPESECRETk2v9";
const FIGURE: &[u8] = b"\x89PNG\r\n\x1a\nnot-really-a-png";

fn p(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("main.typ"), MAIN).unwrap();
    std::fs::write(root.join("paper.tex"), "\\documentclass{article}\n\\begin{document}\nHi\n\\end{document}\n")
        .unwrap();
    std::fs::write(root.join("Main.lean"), "theorem t : 1 = 1 := rfl\n").unwrap();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    std::fs::write(root.join("notes/a.typ"), "= A\n").unwrap();
    std::fs::create_dir_all(root.join("drafts")).unwrap();
    std::fs::write(root.join("drafts/readme.txt"), "drafts\n").unwrap();
    std::fs::create_dir_all(root.join("private")).unwrap();
    std::fs::write(root.join("private/secret.txt"), format!("{SECRET}\n")).unwrap();
    std::fs::create_dir_all(root.join(".exposition/artifacts/run-1")).unwrap();
    std::fs::write(root.join(".exposition/artifacts/run-1/fig.png"), FIGURE).unwrap();
    dir
}

fn artifact() -> ArtifactRecord {
    let hash: String = Sha256::digest(FIGURE).iter().map(|b| format!("{b:02x}")).collect();
    ArtifactRecord {
        id: ArtifactId("fig".into()),
        kind: ArtifactKind::Figure,
        path: Some(p(".exposition/artifacts/run-1/fig.png")),
        scratch_path: None,
        media_type: "image/png".into(),
        size_bytes: FIGURE.len() as u64,
        content_hash: format!("sha256:{hash}"),
    }
}

/// A service call that reached the (recording) application services.
#[derive(Debug, Clone)]
struct Call {
    tool: &'static str,
    policy: Option<ExecutionPolicy>,
    /// Debug form of the request (scope and root for builds).
    request: String,
}

/// Records every call; a restricted backend is available and the folder is
/// trusted, so only the gate stands between a call and the service.
struct Recorder {
    calls: Mutex<Vec<Call>>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self { calls: Mutex::new(Vec::new()) })
    }
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
    fn record(&self, tool: &'static str, policy: Option<ExecutionPolicy>, request: String) {
        self.calls.lock().unwrap().push(Call { tool, policy, request });
    }
}

#[async_trait]
impl ToolServices for Recorder {
    fn available(&self, _kind: ServiceKind) -> bool {
        true
    }
    fn restricted_backend(&self) -> Option<String> {
        Some("docker".into())
    }
    async fn build_document(&self, request: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.record("build_document", request.policy.clone(), format!("{request:?}"));
        Ok(json!({"status": "succeeded", "pages": 1}))
    }
    async fn run_python(&self, request: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.record("run_python", Some(request.policy.clone()), format!("{request:?}"));
        Ok(json!({"status": "succeeded"}))
    }
    async fn check_lean(&self, request: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.record("check_lean", Some(request.policy.clone()), format!("{request:?}"));
        Ok(json!({"checked": true}))
    }
    async fn run_command(&self, request: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        self.record("run_command", Some(request.policy.clone()), format!("{request:?}"));
        Ok(json!({"status": "exited", "exitCode": 0}))
    }
    fn research_artifact(&self, _run: &str, _artifact: &str) -> Result<ArtifactRecord, ServiceError> {
        Ok(artifact())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    List,
    Read,
    Edit,
    Create,
    BuildTypst,
    BuildLatex,
    Insert,
    Python,
    Lean,
    Command,
}

impl Kind {
    fn call(self) -> (&'static str, Value) {
        match self {
            Kind::List => (tools::LIST_FILES, json!({"path": "notes", "recursive": false})),
            Kind::Read => (tools::READ_FILE, json!({"path": "main.typ"})),
            Kind::Edit => (
                tools::EDIT_DOCUMENT,
                json!({"path": "main.typ", "base_revision": 1, "edits": [{"old_text": "quickly", "new_text": "QUICKLY-EDITED"}]}),
            ),
            Kind::Create => (tools::CREATE_FILE, json!({"path": "notes/new.typ", "content": "= New CREATED-MARK\n"})),
            Kind::BuildTypst => (tools::BUILD_DOCUMENT, json!({"entry": "main.typ", "engine": "auto"})),
            Kind::BuildLatex => (tools::BUILD_DOCUMENT, json!({"entry": "paper.tex", "engine": "pdflatex"})),
            Kind::Insert => (
                tools::INSERT_ARTIFACT,
                json!({"path": "main.typ", "run_id": "run-1", "artifact_id": "fig", "after_text": "", "caption": "Fig", "update_key": ""}),
            ),
            Kind::Python => (tools::RUN_PYTHON, json!({"code": "print(6 * 7)  # PAYLOAD-MARK"})),
            Kind::Lean => (tools::CHECK_LEAN, json!({"path": "Main.lean"})),
            Kind::Command => (
                tools::RUN_COMMAND,
                json!({"program": "python", "args": ["-c", "print('PAYLOAD-MARK')"], "working_directory": "notes"}),
            ),
        }
    }

    /// Text the approval request must show in full.
    fn payload(self) -> &'static str {
        match self {
            Kind::Edit => "QUICKLY-EDITED",
            Kind::Create => "CREATED-MARK",
            Kind::Insert => "fig.png",
            Kind::Python | Kind::Command => "PAYLOAD-MARK",
            Kind::BuildLatex => "paper.tex",
            Kind::Lean => "Main.lean",
            Kind::List | Kind::Read | Kind::BuildTypst => "",
        }
    }

    fn service_tool(self) -> Option<&'static str> {
        match self {
            Kind::BuildTypst | Kind::BuildLatex => Some("build_document"),
            Kind::Python => Some("run_python"),
            Kind::Lean => Some("check_lean"),
            Kind::Command => Some("run_command"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Whole,
    /// Every path the calls name (and the artifact), but not the whole project.
    Covering,
    /// None of the paths the calls name.
    Elsewhere,
}

impl Scope {
    fn task_scope(self) -> TaskScope {
        match self {
            Scope::Whole => TaskScope::Workspace,
            Scope::Covering => TaskScope::Files {
                paths: ["main.typ", "paper.tex", "Main.lean", "notes", ".exposition/artifacts"].map(p).to_vec(),
            },
            Scope::Elsewhere => TaskScope::Files { paths: vec![p("drafts")] },
        }
    }
}

const MODES: [ExecutionMode; 3] = [ExecutionMode::Disabled, ExecutionMode::ApproveEach, ExecutionMode::Restricted];
const PERMISSIONS: [PermissionProfile; 3] =
    [PermissionProfile::ReadOnly, PermissionProfile::ReviewEdits, PermissionProfile::AutoApply];
const SCOPES: [Scope; 3] = [Scope::Whole, Scope::Covering, Scope::Elsewhere];
const CONSENTS: [ContextConsent; 2] = [ContextConsent::ScopeContent, ContextConsent::MetadataOnly];

#[derive(Clone, Debug, PartialEq)]
enum Expect {
    /// Refused, with this text in the message; nothing runs or changes.
    Refused(&'static str),
    /// Runs only after the user approves the complete payload.
    Approval(Option<ExecutionPolicy>),
    Allowed(Option<ExecutionPolicy>),
}

/// The rules, stated independently of the implementation.
fn expect(
    kind: Kind,
    mode: ExecutionMode,
    permission: PermissionProfile,
    scope: Scope,
    consent: ContextConsent,
) -> Expect {
    use Kind::*;
    let changes = matches!(kind, Edit | Create | Insert);
    let names_a_path = kind != Python;
    let scoped_content = matches!(kind, Read | BuildTypst);
    // Processes, TeX, and Lean can read beyond any scope.
    let process = matches!(kind, BuildLatex | Python | Lean | Command);
    let native_only = matches!(kind, BuildLatex | Lean);
    let metadata_only = consent == ContextConsent::MetadataOnly;
    if changes && permission == PermissionProfile::ReadOnly {
        return Expect::Refused("read-only");
    }
    if names_a_path && scope == Scope::Elsewhere {
        return Expect::Refused("outside this task's scope");
    }
    if scoped_content && metadata_only {
        return Expect::Refused("consent");
    }
    if process {
        if scope != Scope::Whole {
            return Expect::Refused("whole project");
        }
        if metadata_only {
            return Expect::Refused("consent");
        }
        if permission == PermissionProfile::ReadOnly || mode == ExecutionMode::Disabled {
            return Expect::Refused("disabled");
        }
        if native_only && mode == ExecutionMode::Restricted {
            return Expect::Refused("natively");
        }
        return match mode {
            ExecutionMode::Restricted => {
                Expect::Allowed(Some(ExecutionPolicy::Restricted { backend: "docker".into() }))
            }
            _ => Expect::Approval(Some(ExecutionPolicy::TrustedNative)),
        };
    }
    if changes && permission == PermissionProfile::ReviewEdits {
        return Expect::Approval(None);
    }
    Expect::Allowed(None)
}

/// The stable refusal code that goes with each refusal of the oracle.
fn code_for(text: &str) -> &'static str {
    use exposition_agents::tools::exec::codes;
    match text {
        "read-only" => codes::READ_ONLY,
        "outside this task's scope" => codes::OUT_OF_SCOPE,
        "consent" => codes::METADATA_ONLY,
        "whole project" => codes::WHOLE_PROJECT_ONLY,
        "disabled" => codes::EXECUTION_DISABLED,
        "natively" => codes::NATIVE_NOT_ALLOWED,
        other => panic!("no code for {other:?}"),
    }
}

fn spec(
    mode: ExecutionMode,
    permission: PermissionProfile,
    scope: Scope,
    consent: ContextConsent,
    root: &Path,
) -> TaskSpec {
    TaskSpec {
        prompt: "gate matrix".into(),
        workspace_root: root.display().to_string(),
        scope: scope.task_scope(),
        permission,
        execution: mode,
        consent,
        budgets: Budgets::default(),
        provider: ProviderChoice { provider: ProviderKind::Scripted, model: "scripted".into(), effort: Effort::Low },
    }
}

/// Every combination for `kind`, asked of the gate directly.
fn gate_matrix(kind: Kind) {
    let dir = project();
    let files = LocalProjectFiles::open(dir.path()).unwrap();
    let documents = DocumentService::new();
    let store = Store::open_in_memory().unwrap();
    let services = Recorder::new();
    let task_id = TaskId::new();
    let (name, input) = kind.call();
    let call = ToolCall::parse(name, &input).unwrap();
    let mut failures = Vec::new();
    for mode in MODES {
        for permission in PERMISSIONS {
            for scope in SCOPES {
                for consent in CONSENTS {
                    let spec = spec(mode, permission, scope, consent, files.root());
                    let env = ToolEnv {
                        task_id: &task_id,
                        spec: &spec,
                        files: &files,
                        documents: &documents,
                        workspace: None,
                        services: services.as_ref(),
                        store: &store,
                    };
                    let expected = expect(kind, mode, permission, scope, consent);
                    let gate = exec::gate(&call, &env);
                    let ok = match (&expected, &gate) {
                        (Expect::Refused(text), Gate::Refused(out)) => {
                            out.is_error && out.text().contains(text) && out.content["code"] == json!(code_for(text))
                        }
                        (Expect::Approval(policy), Gate::NeedsApproval { detail, authorization, .. }) => {
                            authorization.policy() == policy.as_ref() && detail.contains(kind.payload())
                        }
                        (Expect::Allowed(policy), Gate::Allowed(authorization)) => {
                            authorization.policy() == policy.as_ref()
                        }
                        _ => false,
                    };
                    if !ok {
                        failures.push(format!(
                            "{kind:?} {mode:?} {permission:?} {scope:?} {consent:?}: expected {expected:?}, gate {gate:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(services.take().is_empty(), "the gate itself never calls a service that runs anything");
    assert!(failures.is_empty(), "{} of 54 combinations differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn gate_list_files() {
    gate_matrix(Kind::List);
}
#[test]
fn gate_read_file() {
    gate_matrix(Kind::Read);
}
#[test]
fn gate_edit_document() {
    gate_matrix(Kind::Edit);
}
#[test]
fn gate_create_file() {
    gate_matrix(Kind::Create);
}
#[test]
fn gate_build_typst() {
    gate_matrix(Kind::BuildTypst);
}
#[test]
fn gate_build_latex() {
    gate_matrix(Kind::BuildLatex);
}
#[test]
fn gate_insert_artifact() {
    gate_matrix(Kind::Insert);
}
#[test]
fn gate_run_python() {
    gate_matrix(Kind::Python);
}
#[test]
fn gate_check_lean() {
    gate_matrix(Kind::Lean);
}
#[test]
fn gate_run_command() {
    gate_matrix(Kind::Command);
}

// ---------------------------------------------------------------------------
// The same matrix through the task loop: what actually happened.
// ---------------------------------------------------------------------------

struct Harness {
    service: AgentService,
    documents: Arc<DocumentService>,
    recorder: Arc<Recorder>,
    events: broadcast::Receiver<AgentEvent>,
}

fn harness() -> Harness {
    let recorder = Recorder::new();
    let documents = Arc::new(DocumentService::new());
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::clone(&documents),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: recorder.clone(),
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
    Harness { service, documents, recorder, events }
}

/// Outcome of one call through the loop, with every approval granted.
struct Observed {
    approvals: Vec<ApprovalRequest>,
    calls: Vec<Call>,
    is_error: bool,
    result: String,
    /// main.typ's open-buffer text (if open) and whether notes/new.typ exists.
    main_text: Option<String>,
    created: bool,
}

async fn observe(
    h: &mut Harness,
    root: &Path,
    kind: Kind,
    mode: ExecutionMode,
    permission: PermissionProfile,
    scope: Scope,
    consent: ContextConsent,
) -> Observed {
    let (name, input) = kind.call();
    // A real edit needs a read of the revision first, when the task may read.
    let read_first =
        kind == Kind::Edit && matches!(expect(Kind::Read, mode, permission, scope, consent), Expect::Allowed(_));
    let mut steps = Vec::new();
    if read_first {
        steps.push(json!({"steps": [{"toolUse": {"id": "r", "name": "read_file", "input": {"path": "main.typ"}}}]}));
    }
    let mut input = input;
    if read_first {
        input["base_revision"] = json!("$ref:r/revision");
    }
    steps.push(json!({"steps": [{"toolUse": {"id": "t", "name": name, "input": input}}]}));
    steps.push(json!({"steps": [{"text": "done"}]}));
    h.service.load_script(json!({"turns": steps})).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "matrix".into(),
            workspace_root: root.display().to_string(),
            scope: scope.task_scope(),
            permission,
            execution: mode,
            consent: Some(consent),
            budgets: None,
        })
        .unwrap();
    let id = task.id.clone();
    let mut approvals = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match h.events.recv().await {
                Ok(AgentEvent::ApprovalRequested { approval }) if approval.task_id == id => {
                    h.service.resolve_approval(&id, &approval.id, true).unwrap();
                    approvals.push(approval);
                }
                Ok(AgentEvent::TaskChanged { task }) if task.id == id && !task.status.is_active() => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("events closed: {e}"),
            }
        }
    })
    .await
    .expect("task finished");
    h.service.join(&id).await;
    let (is_error, result) = h
        .service
        .task_detail(&id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { tool_use_id, is_error, result, .. } if tool_use_id == "t" => {
                Some((is_error, result.unwrap_or_default()))
            }
            _ => None,
        })
        .next()
        .expect("the call is in the transcript");
    let main_text = h.documents.find_by_path(&p("main.typ")).map(|d| h.documents.state(&d).unwrap().text);
    let created = root.join("notes/new.typ").exists();
    let calls = h.recorder.take();
    // Reset for the next combination: close documents, remove created files.
    for info in h.documents.list() {
        let _ = h.documents.close(&info.id);
    }
    let _ = std::fs::remove_file(root.join("notes/new.typ"));
    std::fs::write(root.join("main.typ"), MAIN).unwrap();
    Observed { approvals, calls, is_error, result, main_text, created }
}

async fn loop_matrix(kind: Kind) {
    let dir = project();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mut h = harness();
    let mut failures = Vec::new();
    for mode in MODES {
        for permission in PERMISSIONS {
            for scope in SCOPES {
                for consent in CONSENTS {
                    // An edit is based on revision 1 of the open document.
                    if kind == Kind::Edit {
                        h.documents.open(OpenDocument {
                            path: Some(p("main.typ")),
                            text: MAIN.into(),
                            language: "typst".into(),
                            encoding: exposition_core::document::TextEncoding::Utf8,
                            read_only: None,
                        });
                    }
                    let expected = expect(kind, mode, permission, scope, consent);
                    let o = observe(&mut h, &root, kind, mode, permission, scope, consent).await;
                    let label =
                        format!("{kind:?} {mode:?} {permission:?} {scope:?} {consent:?} (expected {expected:?})");
                    let mut problems = Vec::new();
                    // Invariants that hold whatever the decision.
                    if o.result.contains(SECRET) {
                        problems.push("a file the task may not read reached the result".to_owned());
                    }
                    for call in &o.calls {
                        let process = call.tool != "build_document" || call.policy.is_some();
                        if process && (mode == ExecutionMode::Disabled || permission == PermissionProfile::ReadOnly) {
                            problems.push(format!("process started with execution disabled/read-only: {call:?}"));
                        }
                        if process && (scope != Scope::Whole || consent == ContextConsent::MetadataOnly) {
                            problems.push(format!(
                                "process started without whole-project scope and content consent: {call:?}"
                            ));
                        }
                        if call.policy == Some(ExecutionPolicy::TrustedNative) {
                            if mode != ExecutionMode::ApproveEach {
                                problems.push(format!("native launch outside ask-each mode: {call:?}"));
                            }
                            if !o.approvals.iter().any(|a| a.tool_use_id == "t" && a.detail.contains(kind.payload())) {
                                problems
                                    .push(format!("native launch without an approval showing its payload: {call:?}"));
                            }
                        }
                        if call.tool == "build_document" && call.policy.is_none() {
                            let scope_text = format!("scope: {:?}", scope.task_scope());
                            if !call.request.contains(&scope_text) {
                                problems.push(format!("in-process build without the task's scope: {}", call.request));
                            }
                        }
                    }
                    let changed_main = o.main_text.as_deref().is_some_and(|t| t != MAIN);
                    if permission == PermissionProfile::ReadOnly && (changed_main || o.created) {
                        problems.push("a read-only task changed a document or created a file".to_owned());
                    }
                    // The decision.
                    let ran_service = kind.service_tool().is_some_and(|t| o.calls.iter().any(|c| c.tool == t));
                    let asked = o.approvals.iter().filter(|a| a.tool_use_id == "t").count();
                    match &expected {
                        Expect::Refused(text) => {
                            if !(o.is_error && o.result.contains(text)) {
                                problems.push(format!("not refused with {text:?}: {}", o.result));
                            }
                            let code = format!("\"code\":\"{}\"", code_for(text));
                            if !o.result.contains(&code) {
                                problems.push(format!("refusal without the stable code {code}: {}", o.result));
                            }
                            if asked > 0 || ran_service || changed_main || o.created {
                                problems.push(format!(
                                    "a refused call acted (approvals {asked}, service {ran_service}, edit {changed_main}, created {})",
                                    o.created
                                ));
                            }
                        }
                        Expect::Approval(policy) | Expect::Allowed(policy) => {
                            let want = usize::from(matches!(expected, Expect::Approval(_)));
                            if asked != want {
                                problems.push(format!("{asked} approval request(s), expected {want}"));
                            }
                            if let Some(tool) = kind.service_tool() {
                                let calls: Vec<&Call> = o.calls.iter().filter(|c| c.tool == tool).collect();
                                if calls.len() != 1 || calls[0].policy != *policy {
                                    problems.push(format!("service calls {calls:?}, expected one with {policy:?}"));
                                }
                            }
                            match kind {
                                Kind::Create if !o.created => problems.push("the file was not created".into()),
                                Kind::Insert if !o.main_text.as_deref().is_some_and(|t| t.contains("fig.png")) => {
                                    problems.push(format!("the figure was not inserted: {}", o.result))
                                }
                                Kind::Edit => {
                                    let edited = o.main_text.as_deref().is_some_and(|t| t.contains("QUICKLY-EDITED"));
                                    let read_allowed = matches!(
                                        expect(Kind::Read, mode, permission, scope, consent),
                                        Expect::Allowed(_)
                                    );
                                    // Without a read, the allowed edit fails on its own terms.
                                    if read_allowed != edited {
                                        problems.push(format!(
                                            "edit applied {edited}, read allowed {read_allowed}: {}",
                                            o.result
                                        ));
                                    }
                                }
                                Kind::List | Kind::Read if o.is_error => {
                                    problems.push(format!("allowed call failed: {}", o.result))
                                }
                                _ => {}
                            }
                        }
                    }
                    if !problems.is_empty() {
                        failures.push(format!("{label}:\n  {}", problems.join("\n  ")));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} of 54 combinations misbehaved:\n{}", failures.len(), failures.join("\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_list_files() {
    loop_matrix(Kind::List).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_read_file() {
    loop_matrix(Kind::Read).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_edit_document() {
    loop_matrix(Kind::Edit).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_create_file() {
    loop_matrix(Kind::Create).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_build_typst() {
    loop_matrix(Kind::BuildTypst).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_build_latex() {
    loop_matrix(Kind::BuildLatex).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_insert_artifact() {
    loop_matrix(Kind::Insert).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_run_python() {
    loop_matrix(Kind::Python).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_check_lean() {
    loop_matrix(Kind::Lean).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loop_run_command() {
    loop_matrix(Kind::Command).await;
}

// ---------------------------------------------------------------------------
// Links inside the scope (F4), for every tool that names a path.
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn every_path_tool_refuses_a_link_inside_the_scope_that_leads_outside_it() {
    let dir = project();
    let root = dir.path();
    std::fs::create_dir_all(root.join("public")).unwrap();
    std::fs::write(root.join("public/notes.typ"), "= Public\n").unwrap();
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.join("public").join("link"))
        .arg(root.join("private"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !made {
        eprintln!("SKIPPED: could not create a junction");
        return;
    }
    std::fs::write(root.join("private/doc.typ"), "= Private\n").unwrap();
    let files = LocalProjectFiles::open(root).unwrap();
    let documents = DocumentService::new();
    let store = Store::open_in_memory().unwrap();
    let services = Recorder::new();
    let task_id = TaskId::new();
    let mut spec = spec(
        ExecutionMode::Disabled,
        PermissionProfile::AutoApply,
        Scope::Whole,
        ContextConsent::ScopeContent,
        files.root(),
    );
    spec.scope = TaskScope::Files { paths: vec![p("public"), p(".exposition/artifacts")] };
    let env = ToolEnv {
        task_id: &task_id,
        spec: &spec,
        files: &files,
        documents: &documents,
        workspace: None,
        services: services.as_ref(),
        store: &store,
    };
    let through_link = [
        (tools::READ_FILE, json!({"path": "public/link/secret.txt"})),
        (tools::LIST_FILES, json!({"path": "public/link", "recursive": true})),
        (
            tools::EDIT_DOCUMENT,
            json!({"path": "public/link/doc.typ", "base_revision": 1, "edits": [{"old_text": "Private", "new_text": "x"}]}),
        ),
        (tools::CREATE_FILE, json!({"path": "public/link/new.typ", "content": "x"})),
        (
            tools::INSERT_ARTIFACT,
            json!({"path": "public/link/doc.typ", "run_id": "run-1", "artifact_id": "fig", "after_text": "", "caption": "", "update_key": ""}),
        ),
        (tools::BUILD_DOCUMENT, json!({"entry": "public/link/doc.typ", "engine": "auto"})),
    ];
    for (name, input) in through_link {
        let call = ToolCall::parse(name, &input).unwrap();
        match exec::gate(&call, &env) {
            Gate::Refused(out) => {
                assert!(out.text().contains("is a link to private"), "{name}: {}", out.text());
                assert!(out.text().contains("outside this task's scope"), "{name}: {}", out.text());
                assert_eq!(out.content["code"], json!("agents.outOfScope"), "{name}");
            }
            other => panic!("{name} through a link to an out-of-scope folder was not refused: {other:?}"),
        }
    }
    // The same tools on the real in-scope spelling are allowed.
    let call = ToolCall::parse(tools::READ_FILE, &json!({"path": "public/notes.typ"})).unwrap();
    assert!(matches!(exec::gate(&call, &env), Gate::Allowed(_)));
    assert!(!root.join("private/new.typ").exists());
}

// ---------------------------------------------------------------------------
// In-process builds read only the task's scope (F2 with content consent).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_typst_build_cannot_read_files_outside_the_scope() {
    use exposition_agents::tools::execution::AppToolServices;
    use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings};
    use exposition_typesetting::{Fonts, TypesettingService};
    use exposition_workspace::WorkspaceService;

    let dir = project();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    workspace.open_workspace(dir.path()).unwrap();
    let mut config = ExecutionConfig::new(data.path());
    config.settings = Some(ExecutionSettings::default());
    let execution = ExecutionService::with_config(config).unwrap();
    let typesetting = Arc::new(TypesettingService::new(Arc::clone(&workspace), Fonts::embedded_only));
    let services = AppToolServices::new(execution).with_workspace(Arc::clone(&workspace)).with_typesetting(typesetting);
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
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let mut events = service.subscribe();
    let leak = "#panic(read(\"/private/secret.txt\"))\n";
    let include = "#include \"/private/secret.txt\"\n";
    let run = |scope: TaskScope| {
        let script = json!({"turns": [
            {"steps": [
                {"toolUse": {"id": "c1", "name": "create_file", "input": {"path": "drafts/leak.typ", "content": leak}}},
                {"toolUse": {"id": "c2", "name": "create_file", "input": {"path": "drafts/include.typ", "content": include}}}
            ]},
            {"steps": [
                {"toolUse": {"id": "b1", "name": "build_document", "input": {"entry": "drafts/leak.typ", "engine": "auto"}}},
                {"toolUse": {"id": "b2", "name": "build_document", "input": {"entry": "drafts/include.typ", "engine": "auto"}}}
            ]},
            {"steps": [{"text": "done"}]}
        ]});
        service.load_script(script).unwrap();
        service
            .create_task(CreateTaskRequest {
                prompt: "build".into(),
                workspace_root: dir.path().display().to_string(),
                scope,
                permission: PermissionProfile::AutoApply,
                execution: ExecutionMode::Disabled,
                consent: Some(ContextConsent::ScopeContent),
                budgets: None,
            })
            .unwrap()
    };
    let builds = |id: &TaskId| -> Vec<String> {
        service
            .task_detail(id)
            .unwrap()
            .transcript
            .into_iter()
            .filter_map(|item| match item {
                TranscriptItem::ToolCall { name, result, .. } if name == "build_document" => result,
                _ => None,
            })
            .collect()
    };
    let scoped = run(TaskScope::Files { paths: vec![p("drafts")] });
    wait_done(&service, &mut events, &scoped.id).await;
    let results = builds(&scoped.id);
    assert_eq!(results.len(), 2);
    for result in &results {
        assert!(!result.contains(SECRET), "out-of-scope content reached the result: {result}");
        assert!(result.contains("access denied"), "the read was refused: {result}");
    }
    // Control: the same builds in a whole-project task do read the file, so
    // the check above would see a leak.
    for path in ["drafts/leak.typ", "drafts/include.typ"] {
        let id = documents.find_by_path(&p(path)).unwrap();
        workspace.close_document(&id).unwrap();
        std::fs::remove_file(dir.path().join(path)).unwrap();
    }
    let whole = run(TaskScope::Workspace);
    wait_done(&service, &mut events, &whole.id).await;
    let results = builds(&whole.id);
    assert!(results[0].contains(SECRET), "control: the whole-project build reads the file: {results:?}");
}

async fn wait_done(service: &AgentService, events: &mut broadcast::Receiver<AgentEvent>, id: &TaskId) {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match events.recv().await {
                Ok(AgentEvent::TaskChanged { task }) if &task.id == id && !task.status.is_active() => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("events closed: {e}"),
            }
        }
    })
    .await
    .expect("task finished");
    service.join(id).await;
}

// ---------------------------------------------------------------------------
// An approval binds the call to the task's own project (F6), for processes
// and edits as well as file creation.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approvals_answered_after_a_project_switch_do_nothing() {
    use exposition_workspace::WorkspaceService;
    let dir = project();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("main.typ"), MAIN).unwrap();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(Arc::clone(&documents), data.path().join("workspace"));
    let recorder = Recorder::new();
    let service = AgentService::new(AgentConfig {
        database: None,
        documents: Arc::clone(&documents),
        workspace: Some(Arc::clone(&workspace)),
        credentials: Arc::new(MemoryStore::default()),
        services: recorder.clone(),
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
    let cases = [
        (
            PermissionProfile::AutoApply,
            json!({"toolUse": {"id": "t", "name": "run_python", "input": {"code": "print(1)"}}}),
        ),
        (
            PermissionProfile::AutoApply,
            json!({"toolUse": {"id": "t", "name": "run_command", "input": {"program": "python", "args": [], "working_directory": ""}}}),
        ),
        (
            PermissionProfile::ReviewEdits,
            json!({"toolUse": {"id": "t", "name": "insert_artifact", "input": {"path": "main.typ", "run_id": "run-1", "artifact_id": "fig", "after_text": "", "caption": "", "update_key": ""}}}),
        ),
    ];
    for (permission, step) in cases {
        workspace.open_workspace(dir.path()).unwrap();
        service.load_script(json!({"turns": [{"steps": [step]}, {"steps": [{"text": "done"}]}]})).unwrap();
        let task = service
            .create_task(CreateTaskRequest {
                prompt: "switch".into(),
                workspace_root: dir.path().display().to_string(),
                scope: TaskScope::Workspace,
                permission,
                execution: ExecutionMode::ApproveEach,
                consent: Some(ContextConsent::ScopeContent),
                budgets: None,
            })
            .unwrap();
        let approval = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(AgentEvent::ApprovalRequested { approval }) = events.recv().await {
                    break approval;
                }
            }
        })
        .await
        .expect("approval requested");
        workspace.open_workspace(other.path()).unwrap();
        service.resolve_approval(&task.id, &approval.id, true).unwrap();
        service.join(&task.id).await;
        let result = service
            .task_detail(&task.id)
            .unwrap()
            .transcript
            .into_iter()
            .find_map(|item| match item {
                TranscriptItem::ToolCall { result, .. } => result,
                _ => None,
            })
            .unwrap_or_default();
        assert!(result.contains("The project changed"), "{}: {result}", approval.tool);
        assert!(result.contains("\"code\":\"agents.projectChanged\""), "{}: {result}", approval.tool);
        assert!(recorder.take().is_empty(), "{}: nothing ran", approval.tool);
        assert_eq!(std::fs::read_to_string(other.path().join("main.typ")).unwrap(), MAIN);
        assert_eq!(std::fs::read_to_string(dir.path().join("main.typ")).unwrap(), MAIN);
        for info in documents.list() {
            let _ = workspace.close_document(&info.id);
        }
    }
}

// ---------------------------------------------------------------------------
// What is approved is exactly what runs (F10).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_shows_hidden_characters_and_long_payloads_completely() {
    let dir = project();
    let mut h = harness();
    let code = format!("{}print('ok')\u{202E}# reversed\u{200B}tail-END\n", "x = 1\n".repeat(5000));
    let script = json!({"turns": [
        {"steps": [{"toolUse": {"id": "t", "name": "run_python", "input": {"code": code}}}]},
        {"steps": [{"text": "done"}]}
    ]});
    h.service.load_script(script).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "run".into(),
            workspace_root: dir.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::ApproveEach,
            consent: Some(ContextConsent::ScopeContent),
            budgets: None,
        })
        .unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(AgentEvent::ApprovalRequested { approval }) = h.events.recv().await {
                break approval;
            }
        }
    })
    .await
    .unwrap();
    assert!(approval.detail.len() > code.len(), "nothing omitted ({} bytes shown)", approval.detail.len());
    assert!(approval.detail.contains("x = 1\n".repeat(5000).as_str()));
    assert!(
        approval.detail.contains("print('ok')⟨U+202E⟩# reversed⟨U+200B⟩tail-END"),
        "{}",
        &approval.detail[approval.detail.len() - 400..]
    );
    assert!(approval.detail.contains("shown as ⟨U+XXXX⟩"));
    h.service.resolve_approval(&task.id, &approval.id, true).unwrap();
    h.service.join(&task.id).await;
    let ran = h.recorder.take();
    assert_eq!(ran.len(), 1);
    assert!(ran[0].request.contains("tail-END"), "the code that ran is the code that was shown");
}
