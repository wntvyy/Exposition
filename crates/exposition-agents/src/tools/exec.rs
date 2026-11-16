//! Tool execution: the central gate and the tool implementations.
//!
//! Every call passes [`gate`], which applies one set of rules (project
//! binding, scope on the requested and the resolved path, content consent,
//! process launch per execution mode, approvals) to the effects the call
//! declares ([`crate::tools::Effects`]). Only the gate creates the
//! [`Authorization`] that [`run`] requires; after an approval, [`confirm`]
//! checks again that the call is still exactly what the user approved, in
//! the task's own project.
//!
//! Agent document changes go through the document service as transactions
//! with `EditOrigin::Agent` and `ConflictPolicy::RebaseDisjoint`; reads prefer
//! open buffers over disk. Nothing here bypasses an open buffer. An agent
//! save (`save_document`) is the workspace service's own save of one open
//! document, in its strict form ([`WorkspaceService::save_document_exact`]):
//! never a file write of its own, never an overwrite of a file that changed
//! on disk, and gated as a change to that path.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use exposition_core::artifact::ArtifactRecord;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionError, TransactionId,
    TransactionRequest, UndoBehavior,
};
use exposition_core::job::ExecutionPolicy;
use exposition_core::path::WorkspacePath;
use exposition_core::text::byte_range_to_utf16;
use exposition_workspace::{DocumentService, WorkspaceService};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::autoexec::{self, AutoExec};
use super::services::{
    BuildRequest, CommandRequest, LatexEngine, LeanRequest, PythonRequest, ServiceError, ToolServices, WatchingTrigger,
};
use super::{
    CreateFileInput, EditDocumentInput, Exposure, InsertArtifactInput, Launch, ListFilesInput, PathUse, ReadFileInput,
    RunCommandInput, SaveDocumentInput, ToolCall, scope,
};
use crate::graph::propose::Prepared;
use crate::graph::{GraphEnv, TargetUse};
use crate::model::{
    AllowanceKind, ApprovalMode, AuthorizedBy, ContextConsent, ExecutionMode, PermissionProfile, TaskId, TaskScope,
    TaskSpec,
};
use crate::permissions::{CodeAccess, FilesAccess, ProjectPermissions};
use crate::review::{self, EditRecord};
use crate::store::Store;
use crate::tools;
use crate::workspace::{self, FileError, ProjectFiles};

/// Largest file text sent to the model in one read.
pub const MAX_READ_BYTES: usize = 512 * 1024;
const LIST_LIMIT: usize = 500;

/// Document texts the agent has seen, by (document, revision). `edit_document`
/// resolves `old_text` against the exact revision the agent read.
#[derive(Debug, Default)]
pub struct SeenTexts {
    texts: HashMap<(String, u64), Arc<str>>,
    order: VecDeque<(String, u64)>,
}

impl SeenTexts {
    const CAPACITY: usize = 48;

    pub fn insert(&mut self, document: &DocumentId, revision: u64, text: Arc<str>) {
        let key = (document.0.clone(), revision);
        if self.texts.insert(key.clone(), text).is_none() {
            self.order.push_back(key);
            while self.order.len() > Self::CAPACITY {
                if let Some(old) = self.order.pop_front() {
                    self.texts.remove(&old);
                }
            }
        }
    }

    pub fn get(&self, document: &DocumentId, revision: u64) -> Option<Arc<str>> {
        self.texts.get(&(document.0.clone(), revision)).cloned()
    }
}

pub struct ToolEnv<'a> {
    pub task_id: &'a TaskId,
    pub spec: &'a TaskSpec,
    pub files: &'a dyn ProjectFiles,
    pub documents: &'a DocumentService,
    /// The application's workspace service, when present.
    pub workspace: Option<&'a WorkspaceService>,
    pub services: &'a dyn ToolServices,
    pub store: &'a Store,
    /// The project graph, bound to the task's project. Present in the
    /// application (the agent service requires it whenever a workspace
    /// service is present); every call then needs a usable graph and its
    /// targets resolve through it.
    pub graph: Option<GraphEnv<'a>>,
    /// The live authority beyond the task's own settings: the project's
    /// permissions (Auto) and the task's allowances, read when the call is
    /// decided and again immediately before it runs.
    pub authority: Authority,
}

/// What authorizes calls beyond the task's fixed settings, as read now.
/// Only the user changes it (composer, approval card, Settings); nothing a
/// model sends reaches it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Authority {
    /// The project's permissions (used by Auto tasks); `None`: no record.
    pub project: Option<ProjectPermissions>,
    /// What the user approved for the rest of this task.
    pub allowances: Allowances,
}

/// A task's allowances ("Approve for this task") and their version, which
/// changes with every grant, revocation, or clearing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Allowances {
    pub version: u64,
    pub kinds: Vec<AllowanceKind>,
}

impl Allowances {
    pub fn has(&self, kind: AllowanceKind) -> bool {
        self.kinds.contains(&kind)
    }
}

/// The authority an authorization was issued under. A call runs only while
/// it is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Basis {
    /// Auto: the project's permission version (0 without a record); Ask: not used.
    project_version: Option<u64>,
    allowance_version: u64,
}

impl Authority {
    fn basis(&self, mode: ApprovalMode) -> Basis {
        Basis {
            project_version: (mode == ApprovalMode::Auto).then(|| self.project.map_or(0, |p| p.version)),
            allowance_version: self.allowances.version,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: Value,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: Value) -> Self {
        Self { content, is_error: false }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self { content: json!({"error": message.into()}), is_error: true }
    }

    pub fn error_with(message: impl Into<String>, extra: Value) -> Self {
        let mut content = json!({"error": message.into()});
        if let (Some(obj), Value::Object(more)) = (content.as_object_mut(), extra) {
            obj.extend(more);
        }
        Self { content, is_error: true }
    }

    /// Text for the `tool_result` block.
    pub fn text(&self) -> String {
        serde_json::to_string(&self.content).unwrap_or_else(|_| "{\"error\":\"unserializable result\"}".into())
    }
}

/// Stable codes of the gate's refusals, in the `code` field of the tool result
/// (next to the human-readable `error`).
pub mod codes {
    /// A path (as requested or as resolved) is outside the task's scope.
    pub const OUT_OF_SCOPE: &str = "agents.outOfScope";
    /// A path resolves outside the project folder.
    pub const OUTSIDE_PROJECT: &str = "agents.outsideProject";
    pub const INVALID_PATH: &str = "agents.invalidPath";
    /// The tool can read beyond any scope; the task is not scoped to the whole project.
    pub const WHOLE_PROJECT_ONLY: &str = "agents.wholeProjectOnly";
    /// The task's consent allows only file names and metadata.
    pub const METADATA_ONLY: &str = "agents.metadataOnly";
    pub const READ_ONLY: &str = "agents.readOnly";
    pub const EXECUTION_DISABLED: &str = "agents.executionDisabled";
    /// A native-only tool in a restricted-only task.
    pub const NATIVE_NOT_ALLOWED: &str = "agents.nativeNotAllowed";
    /// Restricted execution required, no restricted backend.
    pub const RESTRICTED_UNAVAILABLE: &str = "agents.restrictedUnavailable";
    /// Native execution is not possible here (e.g. the folder is not trusted).
    pub const NATIVE_REFUSED: &str = "agents.nativeRefused";
    pub const SERVICE_UNAVAILABLE: &str = "agents.serviceUnavailable";
    /// The project switched (before running, or while waiting for approval).
    pub const PROJECT_CHANGED: &str = "agents.projectChanged";
    /// What the call would do changed while it waited for approval.
    pub const CHANGED_SINCE_APPROVAL: &str = "agents.changedSinceApproval";
    /// The project graph cannot be used now (damaged, detached, not
    /// synchronized): the project-agent action is paused.
    pub const GRAPH_UNAVAILABLE: &str = "agents.graphUnavailable";
    /// A target changed and the graph has not reconciled it: paused.
    pub const GRAPH_PENDING: &str = "agents.graphPending";
    /// The graph cannot tell which resource a target is: paused.
    pub const GRAPH_AMBIGUOUS: &str = "agents.graphAmbiguous";
    /// The graph's own storage is never a file tool target.
    pub const GRAPH_STORAGE: &str = "agents.graphStorage";
    /// Service-owned records (run records, generated artifacts, application
    /// metadata) are never written by agent file tools.
    pub const SERVICE_OWNED: &str = "agents.serviceOwned";
    /// A proposed graph change is invalid (validator diagnostics attached).
    pub const GRAPH_INVALID: &str = "agents.graphInvalid";
    /// A proposed graph change conflicts with changes made since.
    pub const GRAPH_CONFLICT: &str = "agents.graphConflict";
    /// Investigations and workflows are admitted deliberately, not by graph_propose.
    pub const GRAPH_RESERVED: &str = "agents.graphReservedKind";
    /// A workflow definition that is not well formed, or a save that failed.
    pub const WORKFLOW_INVALID: &str = "agents.workflowInvalid";
    /// A workflow's authority ceiling wider than the task's own.
    pub const WORKFLOW_CEILING: &str = "agents.workflowCeiling";
    /// A workflow that forges a service-owned result (formal evidence, retained artifacts).
    pub const WORKFLOW_SERVICE_OWNED: &str = "agents.workflowServiceOwned";
    /// A workflow whose current revision uses schema /2 features (parallel
    /// steps, composites, conditions, repeats): agents edit only /1 workflows.
    pub const WORKFLOW_UNSUPPORTED: &str = "agents.workflowUnsupported";
    /// A deletion or copy with concrete consequences (a manuscript, workflow
    /// or evidence uses it, unsaved changes, running work) in a task that
    /// applies changes without review: only the user decides those.
    pub const NEEDS_DECISION: &str = "agents.needsDecision";
    /// Every item of a deletion or copy was refused by the project operations.
    pub const OPERATION_REFUSED: &str = "agents.operationRefused";
    /// The project's permissions or the task's allowances changed between
    /// the decision (or the approval) and the moment the call would run.
    pub const PERMISSIONS_CHANGED: &str = "agents.permissionsChanged";
    /// An Auto task needs the project's permissions, and the project has none.
    pub const PERMISSIONS_REQUIRED: &str = "agents.permissionsRequired";
    /// "Approve for this task" on an approval that can only be given once.
    pub const NOT_REPEATABLE: &str = "agents.notRepeatable";
    /// Project permissions that cannot be granted as asked.
    pub const INVALID_PERMISSIONS: &str = "agents.invalidPermissions";
    /// A folder agents never change (`.git`, `.lake`, `.venv`, `venv`): the
    /// application or the user's tools run what is in it.
    pub const PROTECTED_TARGET: &str = "agents.protectedTarget";
    /// A save that only the user can decide: the file changed, was deleted
    /// or moved on disk since the document was opened or last saved, the
    /// document is in a disk conflict, or it holds recovered text.
    pub const SAVE_CONFLICT: &str = "agents.saveConflict";
    /// A save of a document that has no unsaved changes.
    pub const NOTHING_TO_SAVE: &str = "agents.nothingToSave";
}

/// A refusal with its stable code.
fn refusal(code: &str, message: impl Into<String>) -> ToolOutput {
    ToolOutput::error_with(message, json!({"code": code}))
}

/// The application's permission to run one call, bound to the task's project
/// folder. Only [`gate`] (and [`confirm`], after an approval) creates one, and
/// [`run`] requires it: no tool runs without passing the gate.
#[derive(Debug, Clone, PartialEq)]
pub struct Authorization {
    policy: Option<ExecutionPolicy>,
    /// The task's canonical project folder when the call was gated.
    root: PathBuf,
    /// Exactly what the user was shown, when the call waits for approval.
    approved: Option<String>,
    /// An artifact insertion planned (and shown) at the gate.
    insertion: Option<PlannedInsertion>,
    /// A graph change validated (and shown) at the gate.
    proposal: Option<Box<Prepared>>,
    /// A workflow draft validated (and shown) at the gate.
    workflow: Option<Box<crate::graph::workflow::PreparedWorkflow>>,
    /// A project operation (delete/copy) planned (and shown) at the gate.
    project_op: Option<Box<PreparedOp>>,
    /// A package setup planned (and shown) before the gate.
    setup: Option<Box<exposition_research::setup::SetupPlan>>,
    /// A save prepared (and shown) at the gate: exactly this revision.
    save: Option<Box<PreparedSave>>,
    /// The project permission and allowance versions it was issued under.
    basis: Basis,
    /// How it is authorized (recorded with the call).
    authorized_by: AuthorizedBy,
}

/// A deletion or copy planned by the application's project operations at
/// the gate: the plan, and its complete description for approval.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedOp {
    plan: exposition_core::ops::OpPlan,
    title: String,
    detail: String,
}

impl Authorization {
    /// Execution policy for a process launch (`None` for in-process tools).
    pub fn policy(&self) -> Option<&ExecutionPolicy> {
        self.policy.as_ref()
    }

    /// The graph change this authorization covers, if it is one.
    pub fn proposal(&self) -> Option<&Prepared> {
        self.proposal.as_deref()
    }

    /// How the call is authorized.
    pub fn authorized_by(&self) -> &AuthorizedBy {
        &self.authorized_by
    }

    /// The user approved the call (after [`confirm`]); with `for_task`, the
    /// allowances the approval granted, now at that allowance version.
    pub(crate) fn approve(&mut self, for_task: Option<u64>) {
        self.authorized_by = AuthorizedBy::Approved { for_task: for_task.is_some() };
        if let Some(version) = for_task {
            self.basis.allowance_version = version;
        }
    }
}

/// Whether the text a save writes holds changes this task did not make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OtherChanges {
    /// Every change since the last save is this task's own.
    None,
    /// The text was also changed outside this task since the last save.
    Included,
    /// It cannot be told (no saved revision, or the history does not reach it).
    Unknown,
}

impl OtherChanges {
    fn key(self) -> &'static str {
        match self {
            OtherChanges::None => "none",
            OtherChanges::Included => "included",
            OtherChanges::Unknown => "unknown",
        }
    }

    /// The disclosure, as the approval card and the result state it.
    fn note(self) -> &'static str {
        match self {
            OtherChanges::None => "Every unsaved change in this file was made by this task.",
            OtherChanges::Included => {
                "Since the last save, this file's editor text was also changed outside this task (your own unsaved \
                 edits, or another task's). Those changes are saved with it: a save writes the whole current text."
            }
            OtherChanges::Unknown => {
                "Exposition cannot tell which of the unsaved changes this task made. The whole current editor text \
                 is saved, whoever wrote it."
            }
        }
    }

    /// The same disclosure addressed to the agent (in the tool result).
    fn result_note(self) -> &'static str {
        match self {
            OtherChanges::None => "Every unsaved change that was saved was made by this task.",
            OtherChanges::Included => {
                "Since the last save, the editor text had also been changed outside this task (the user's own \
                 unsaved edits, or another task's). Those changes were saved with yours: a save writes the whole \
                 current text. Tell the user."
            }
            OtherChanges::Unknown => {
                "Exposition cannot tell which of the saved changes this task made: the whole current editor text \
                 was saved, whoever wrote it. Tell the user."
            }
        }
    }
}

/// The enabled workflow triggers that watch the file a save writes. Saving
/// makes them react as they do to the user's save; the save states it (the
/// approval card, and the result when no card was shown). Disclosure only:
/// it changes neither who may save nor what a trigger does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveTriggers {
    /// Read from the trigger service (sorted, without duplicates).
    Known(Vec<WatchingTrigger>),
    /// They could not be read: never stated as "none".
    Unknown,
}

impl SaveTriggers {
    /// One line per trigger; `saved`: after the save (the result) rather
    /// than before it (the approval card).
    fn lines(triggers: &[WatchingTrigger], saved: bool) -> String {
        let mut out = String::new();
        for trigger in triggers {
            let title = shown(&trigger.workflow);
            let what = match (trigger.starts_on_save, saved) {
                (true, false) => "saving this file can start a run of this workflow, within the trigger's own limits.",
                (true, true) => "this save can start a run of this workflow, within the trigger's own limits.",
                (false, false) => {
                    "it runs on a schedule; saving does not start it, and its next run reads the file as saved."
                }
                (false, true) => {
                    "it runs on a schedule; this save did not start it, and its next run reads the file as saved."
                }
            };
            out.push_str(&format!("\n- “{title}”: {what}"));
        }
        out
    }

    fn heading(triggers: &[WatchingTrigger], file: &str) -> String {
        if triggers.len() == 1 {
            format!("An enabled workflow trigger watches {file}:")
        } else {
            format!("Enabled workflow triggers watch {file}:")
        }
    }

    /// The disclosure on the approval card (`None`: no enabled trigger
    /// watches the file).
    fn note(&self) -> Option<String> {
        match self {
            SaveTriggers::Known(triggers) if triggers.is_empty() => None,
            SaveTriggers::Known(triggers) => {
                Some(format!("{}{}", Self::heading(triggers, "this file"), Self::lines(triggers, false)))
            }
            SaveTriggers::Unknown => Some(
                "Exposition could not determine whether an enabled workflow trigger watches this file. If one \
                 does, saving can start a run of its workflow, within the trigger's own limits."
                    .to_owned(),
            ),
        }
    }

    /// The same disclosure addressed to the agent (in the tool result).
    fn result_note(&self) -> Option<String> {
        match self {
            SaveTriggers::Known(triggers) if triggers.is_empty() => None,
            SaveTriggers::Known(triggers) => Some(format!(
                "{}{}\nTell the user.",
                Self::heading(triggers, "the file you saved"),
                Self::lines(triggers, true)
            )),
            SaveTriggers::Unknown => Some(
                "Exposition could not determine whether an enabled workflow trigger watches the file you saved. \
                 If one does, this save can start a run of its workflow, within the trigger's own limits. Tell \
                 the user."
                    .to_owned(),
            ),
        }
    }

    /// In the result: `triggersKnown` and the list (empty when unknown).
    fn list(&self) -> Value {
        match self {
            SaveTriggers::Known(triggers) => triggers
                .iter()
                .map(|t| json!({"workflow": t.workflow, "startsOnSave": t.starts_on_save}))
                .collect::<Vec<_>>()
                .into(),
            SaveTriggers::Unknown => json!([]),
        }
    }
}

/// A save prepared at the gate: the document and the exact revision that
/// may be written, what it holds, and its complete description for approval.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedSave {
    path: WorkspacePath,
    document: DocumentId,
    revision: u64,
    content_hash: String,
    others: OtherChanges,
    title: String,
    detail: String,
}

/// An artifact insertion as planned when it was shown for approval.
#[derive(Debug, Clone, PartialEq)]
struct PlannedInsertion {
    key: String,
    /// Text of each patch, in order.
    texts: Vec<String>,
}

/// Decision before running a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Allowed(Authorization),
    /// The user must approve first; `detail` is the complete payload (nothing
    /// truncated) of what will run or change.
    NeedsApproval {
        title: String,
        detail: String,
        /// Why it asks when the task's settings do not make that obvious.
        reason: Option<String>,
        /// What "Approve for this task" would allow (empty: once only).
        for_task: Vec<AllowanceKind>,
        authorization: Authorization,
    },
    Refused(ToolOutput),
}

fn parse_path(raw: &str) -> Result<WorkspacePath, ToolOutput> {
    WorkspacePath::parse(raw).map_err(|e| refusal(codes::INVALID_PATH, format!("invalid path {raw:?}: {e}")))
}

fn parse_dir(raw: &str) -> Result<Option<WorkspacePath>, ToolOutput> {
    if tools::is_root(raw) { Ok(None) } else { parse_path(raw.trim()).map(Some) }
}

fn scoped_paths(scope: &TaskScope) -> Vec<WorkspacePath> {
    match scope {
        TaskScope::Workspace => Vec::new(),
        TaskScope::Files { paths } => paths.clone(),
        TaskScope::Document { path } | TaskScope::Selection { path, .. } => vec![path.clone()],
    }
}

fn entry_visible(path: &str, scope: &TaskScope) -> bool {
    let Ok(parsed) = WorkspacePath::parse(path) else { return false };
    scope.allows(&parsed) || scoped_paths(scope).iter().any(|p| p.as_str().starts_with(&format!("{path}/")))
}

fn read_only_refusal() -> ToolOutput {
    refusal(
        codes::READ_ONLY,
        "This task is read-only: the user did not allow document changes. Describe the change instead.",
    )
}

fn execution_disabled() -> ToolOutput {
    refusal(codes::EXECUTION_DISABLED, "Process execution is disabled for this task by the user.")
}

fn project_changed(reason: &str) -> ToolOutput {
    refusal(
        codes::PROJECT_CHANGED,
        format!("The project changed: {reason} This task acts only on its own project folder, so nothing was done."),
    )
}

/// Check one path a call names: valid, and in the task's scope both as
/// requested and as resolved (links and aliases followed).
fn check_path(env: &ToolEnv<'_>, raw: &str, path_use: PathUse) -> Result<(), ToolOutput> {
    let scope = &env.spec.scope;
    let root = env.files.root();
    let checked = match path_use {
        PathUse::Folder | PathUse::WorkingFolder => scope::check_dir(root, scope, parse_dir(raw)?.as_ref()),
        PathUse::Read | PathUse::Write | PathUse::Create => scope::check(root, scope, &parse_path(raw)?).map(|_| ()),
    };
    checked.map_err(|e| refusal(e.code(), e.message(scope)))
}

/// Decide whether `call` may run, and whether it needs the user's approval:
/// Allowed, NeedsApproval, or Refused (with a stable code).
///
/// The rules apply to the call's declared [`Effects`], in this order:
/// 1. the task's folder is the open project (calls act only on their own project);
/// 2. a change is refused for a read-only task;
/// 3. every named path is valid and in scope, as requested and as resolved;
/// 4. what the result can carry to the model: content needs the user's content
///    consent; content beyond the scope (processes, TeX, Lean) needs a task
///    scoped to the whole project as well;
/// 5. processes never start with execution disabled or read-only, and
///    native-only tools never start in a restricted-only task (in Auto these
///    task settings are the ceiling: they prohibit, never ask);
/// 6. the service exists;
/// 7. files the application executes automatically ([`super::autoexec`]):
///    a change or copy that puts Lean source, Lean/Lake configuration
///    (`lakefile.lean`, `lakefile.toml`, `lean-toolchain`,
///    `lake-manifest.json`), or an R startup file (`.Rprofile`, `.Renviron`)
///    into the project is native execution too (the Lean server elaborates
///    Lean text; Lake may download and run packages, elan may download
///    toolchains; R runs its startup files at every session start in the
///    folder, Rscript included): it needs native authority as well as
///    change authority (D7), whether or not the folder is trusted yet — trust
///    decides only whether it runs now or once the folder is trusted, and
///    this authority does not itself require trust. Protected folders (`.git`, `.lake`,
///    `.venv`, `venv`, at any depth) are never written, created, copied
///    into, or deleted (rule 3, `agents.protectedTarget`);
/// 8. authority: Ask — the task's settings and its allowances; Auto — the
///    project's permissions, the task's allowances, and one approval for a
///    legitimate expansion. Restricted launches need the restricted backend
///    (never downgraded, never asked around); native launches need a trusted
///    folder; consequential project operations always ask (once, never
///    repeatable), or are refused in Ask tasks that apply changes directly;
/// 9. tool-specific preparation (an inserted artifact's own scope check and
///    the exact text shown for approval, graph and workflow validation, the
///    project-operation plan).
pub fn gate(call: &ToolCall, env: &ToolEnv<'_>) -> Gate {
    match decide(call, env) {
        Ok(gate) => gate,
        Err(refusal) => Gate::Refused(refusal),
    }
}

/// Why a call needs the user now, and what approving it for the task allows.
#[derive(Default)]
struct Asks {
    reasons: Vec<String>,
    /// Kinds "Approve for this task" would allow.
    kinds: Vec<AllowanceKind>,
    /// Something only a one-off approval can cover.
    once_only: bool,
    asked: bool,
}

impl Asks {
    fn ask(&mut self, reason: Option<String>, kind: Option<AllowanceKind>) {
        self.asked = true;
        if let Some(reason) = reason
            && !self.reasons.contains(&reason)
        {
            self.reasons.push(reason);
        }
        match kind {
            Some(kind) if !self.kinds.contains(&kind) => self.kinds.push(kind),
            Some(_) => {}
            None => self.once_only = true,
        }
    }

    fn for_task(&self) -> Vec<AllowanceKind> {
        if self.once_only {
            return Vec::new();
        }
        let mut kinds = self.kinds.clone();
        kinds.sort();
        kinds
    }

    fn reason(&self) -> Option<String> {
        (!self.reasons.is_empty()).then(|| self.reasons.join(" "))
    }
}

/// What covered the parts of a call that needed authority.
#[derive(Default)]
struct Covered {
    settings: bool,
    project: bool,
    allowances: Vec<AllowanceKind>,
}

impl Covered {
    fn authorized_by(&self, authority: &Authority) -> AuthorizedBy {
        if !self.allowances.is_empty() {
            let mut kinds = self.allowances.clone();
            kinds.sort();
            kinds.dedup();
            AuthorizedBy::TaskAllowance { kinds }
        } else if self.project {
            AuthorizedBy::ProjectPermissions { version: authority.project.map_or(0, |p| p.version) }
        } else if self.settings {
            AuthorizedBy::TaskSettings
        } else {
            AuthorizedBy::NotRequired
        }
    }
}

/// Covered by an allowance, or asked for (repeatable as `kind`).
fn covered_or_ask(
    env: &ToolEnv<'_>,
    kind: AllowanceKind,
    reason: Option<String>,
    asks: &mut Asks,
    covered: &mut Covered,
) {
    if env.authority.allowances.has(kind) {
        covered.allowances.push(kind);
    } else {
        asks.ask(reason, Some(kind));
    }
}

const NO_PROJECT_PERMISSIONS: &str = "This project has no Auto permissions.";

/// The authority a document or file change needs.
fn change_authority(env: &ToolEnv<'_>, asks: &mut Asks, covered: &mut Covered) {
    let spec = env.spec;
    match spec.approval_mode {
        ApprovalMode::Ask => match spec.permission {
            PermissionProfile::ReviewEdits => covered_or_ask(env, AllowanceKind::DocumentChanges, None, asks, covered),
            // Read-only tasks were refused before; changes apply directly.
            PermissionProfile::AutoApply | PermissionProfile::ReadOnly => covered.settings = true,
        },
        ApprovalMode::Auto => match env.authority.project {
            Some(project) if project.files == FilesAccess::Edit => covered.project = true,
            project => {
                let reason = match project {
                    Some(_) => "Changing files is beyond this project's permissions (Files: off).",
                    None => NO_PROJECT_PERMISSIONS,
                };
                covered_or_ask(env, AllowanceKind::DocumentChanges, Some(reason.to_owned()), asks, covered);
            }
        },
    }
}

fn code_reason(project: Option<&ProjectPermissions>, launch: Launch, what: &str) -> String {
    match project.map(|p| p.code) {
        None => NO_PROJECT_PERMISSIONS.to_owned(),
        Some(CodeAccess::Restricted) if launch == Launch::NativeOnly => format!(
            "{what} runs only natively on this computer, which is beyond this project's permissions (Code: \
             restricted)."
        ),
        Some(CodeAccess::Restricted) => {
            "Running code natively is beyond this project's permissions (Code: restricted).".to_owned()
        }
        Some(CodeAccess::Off) | Some(CodeAccess::Native) => {
            "Running code is beyond this project's permissions (Code: off).".to_owned()
        }
    }
}

fn restricted_unavailable() -> ToolOutput {
    refusal(
        codes::RESTRICTED_UNAVAILABLE,
        "This task only allows unattended execution inside the restricted (container) backend, which is not \
         available. Exposition never falls back to running it natively; the user can choose per-command approval \
         instead.",
    )
}

/// The policy a process launch runs with, and the authority it needs. The
/// task's contract (read-only, execution disabled, native-only tools in a
/// restricted task) was checked before.
fn launch_authority(
    env: &ToolEnv<'_>,
    launch: Launch,
    what: &str,
    asks: &mut Asks,
    covered: &mut Covered,
) -> Result<ExecutionPolicy, ToolOutput> {
    let spec = env.spec;
    let restricted = || match env.services.restricted_backend() {
        Some(backend) => Ok(ExecutionPolicy::Restricted { backend, image_id: None }),
        None => Err(restricted_unavailable()),
    };
    let native = || match env.services.native_refusal(env.files.root()) {
        None => Ok(ExecutionPolicy::TrustedNative),
        Some(reason) => Err(ToolOutput::error_with(
            reason.clone(),
            json!({"code": codes::NATIVE_REFUSED, "status": "refused", "reason": reason}),
        )),
    };
    match (spec.approval_mode, spec.execution) {
        (_, ExecutionMode::Disabled) => Err(execution_disabled()),
        (ApprovalMode::Ask, ExecutionMode::Restricted) => {
            let policy = restricted()?;
            covered.settings = true;
            Ok(policy)
        }
        (ApprovalMode::Ask, ExecutionMode::ApproveEach) => {
            let policy = native()?;
            covered_or_ask(env, AllowanceKind::NativeCode, None, asks, covered);
            Ok(policy)
        }
        // Auto within a restricted ceiling: restricted only (native-only
        // tools were refused by the contract).
        (ApprovalMode::Auto, ExecutionMode::Restricted) => {
            let policy = restricted()?;
            match env.authority.project {
                Some(p) if p.code >= CodeAccess::Restricted => covered.project = true,
                project => asks.ask(Some(code_reason(project.as_ref(), launch, what)), None),
            }
            Ok(policy)
        }
        (ApprovalMode::Auto, ExecutionMode::ApproveEach) => {
            let project = env.authority.project;
            match project.map(|p| p.code) {
                Some(CodeAccess::Native) => {
                    let policy = native()?;
                    covered.project = true;
                    Ok(policy)
                }
                Some(CodeAccess::Restricted) if launch == Launch::NativeOrRestricted => {
                    let policy = restricted()?;
                    covered.project = true;
                    Ok(policy)
                }
                // A legitimate expansion: native, asked once (repeatable).
                _ => {
                    let policy = native()?;
                    let reason = code_reason(project.as_ref(), launch, what);
                    covered_or_ask(env, AllowanceKind::NativeCode, Some(reason), asks, covered);
                    Ok(policy)
                }
            }
        }
    }
}

/// The authority a package setup needs: setup authority (Ask: the user's
/// approval of the complete plan, or "for this task"; Auto: the project's
/// setup permission), natively in a trusted folder. The task's contract
/// (read only, execution disabled, restricted only) was checked before.
fn setup_authority(env: &ToolEnv<'_>, asks: &mut Asks, covered: &mut Covered) -> Result<ExecutionPolicy, ToolOutput> {
    if let Some(reason) = env.services.native_refusal(env.files.root()) {
        return Err(ToolOutput::error_with(
            reason.clone(),
            json!({"code": codes::NATIVE_REFUSED, "status": "refused", "reason": reason}),
        ));
    }
    match (env.spec.approval_mode, env.authority.project) {
        (ApprovalMode::Auto, Some(p)) if p.setup && p.code == CodeAccess::Native => covered.project = true,
        (ApprovalMode::Auto, project) => {
            let reason = match project {
                Some(_) => "Installing packages is beyond this project's permissions (Setup: off).",
                None => NO_PROJECT_PERMISSIONS,
            };
            covered_or_ask(env, AllowanceKind::PackageSetup, Some(reason.to_owned()), asks, covered);
        }
        (ApprovalMode::Ask, _) => covered_or_ask(env, AllowanceKind::PackageSetup, None, asks, covered),
    }
    Ok(ExecutionPolicy::TrustedNative)
}

/// Title and complete detail of a package setup: the plan's summary, target,
/// index, packages, and every command exactly as it runs.
fn describe_setup(plan: &exposition_research::setup::SetupPlan) -> (String, String) {
    let title = if plan.creates_environment {
        format!("Create {} and install packages", plan.target_name)
    } else {
        format!("Install packages into {}", plan.target_name)
    };
    let mut detail = format!(
        "{}\n\nTarget environment: {}\nPackage index: {}\nPackages: {}\n",
        shown(&plan.summary),
        shown(&plan.target),
        shown(&plan.index_url),
        shown(&plan.packages.join(" "))
    );
    if let Some(base) = &plan.base_interpreter {
        detail.push_str(&format!("Created from: {}\n", shown(base)));
    }
    detail.push_str(
        "Needs network; installs into the project environment. Packages run code when they are imported later.\n",
    );
    if plan.removes_unfinished {
        detail.push_str("An unfinished environment an earlier setup by Exposition left there is removed first.\n");
    }
    detail.push_str("\nCommands, exactly as they run (no shell):\n");
    for (i, step) in plan.steps.iter().enumerate() {
        let args: Vec<String> = step.args.iter().map(|a| format!("{a:?}")).collect();
        detail.push_str(&format!(
            "{}. {}{}\n   {:?} [{}]\n",
            i + 1,
            shown(&step.label),
            if step.network { " (network)" } else { "" },
            shown(&step.program),
            shown(&args.join(", "))
        ));
    }
    (title, detail.trim_end().to_owned())
}

/// The auto-executed content ([`AutoExec`]) a change or copy would put into
/// the project: an edited, created, or inserted file named as a Lean source
/// or Lean/Lake configuration (requested or resolved name), or such a file
/// anywhere in a copied tree (bounded; a tree too large to inspect counts as
/// configuration). In any folder, trusted or not: the Lean service runs the
/// toolchain (and Lake) on such text as soon as the folder is trusted. A
/// copy that would create a protected folder is refused here.
fn auto_exec_target(call: &ToolCall, env: &ToolEnv<'_>) -> Result<Option<AutoExec>, ToolOutput> {
    let root = env.files.root();
    let found = match call {
        ToolCall::EditDocument(input) => path_kind(env, &input.path),
        ToolCall::CreateFile(input) => path_kind(env, &input.path),
        // A save puts the buffer's text on disk under this name: the same
        // rule as the edits that wrote it, whoever wrote it.
        ToolCall::SaveDocument(input) => path_kind(env, &input.path),
        ToolCall::InsertArtifact(input) => path_kind(env, &input.path),
        ToolCall::CopyPaths(input) => {
            let mut budget = autoexec::MAX_SCAN_ENTRIES;
            let mut found = None;
            for raw in &input.paths {
                let requested = parse_path(raw.trim())?;
                // What the copy creates is named after the source entry and
                // what it holds (a copy out of a protected folder is fine; a
                // copy that creates one is not).
                let source = scope::resolve(root, &requested).ok().flatten().unwrap_or_else(|| requested.clone());
                let scan = autoexec::scan_tree(root, &source, &mut budget);
                if let Some((at, p)) = scan.protected {
                    return Err(refusal(
                        codes::PROTECTED_TARGET,
                        format!(
                            "Copying {requested} would create {at}, which is {}. Agents never create or change it; \
                             nothing was copied. {}",
                            p.what, p.instead
                        ),
                    ));
                }
                let kind = if scan.truncated { Some(AutoExec::LakeConfig) } else { scan.auto };
                found = found.max(kind);
            }
            found
        }
        _ => None,
    };
    Ok(found)
}

/// The edited, created, or inserted path's auto-executed kind (requested or resolved name).
fn path_kind(env: &ToolEnv<'_>, raw: &str) -> Option<AutoExec> {
    let requested = WorkspacePath::parse(raw.trim()).ok()?;
    let resolved = scope::resolve(env.files.root(), &requested).ok().flatten();
    autoexec::auto_executed(&requested).max(resolved.as_ref().and_then(autoexec::auto_executed))
}

/// D7: the task's contract for a change that puts auto-executed content in
/// place (`trusted`: whether the folder is trusted now, for the wording).
fn lean_contract(spec: &TaskSpec, kind: AutoExec, trusted: bool) -> Result<(), ToolOutput> {
    let consequence = kind.consequence(trusted);
    match spec.execution {
        ExecutionMode::Disabled => Err(refusal(
            codes::EXECUTION_DISABLED,
            format!(
                "{consequence} Process execution is disabled for this task by the user, so nothing was changed. \
                 Describe the change instead."
            ),
        )),
        ExecutionMode::Restricted => Err(refusal(
            codes::NATIVE_NOT_ALLOWED,
            format!(
                "{consequence} That runs natively, which this task does not allow (restricted sandbox only), so \
                 nothing was changed. Describe the change instead."
            ),
        )),
        ExecutionMode::ApproveEach => Ok(()),
    }
}

/// D7: the native authority such a change needs besides change authority.
/// It does not require trust (nothing runs now in an untrusted folder; a
/// native launch itself still needs trust).
fn lean_authority(env: &ToolEnv<'_>, kind: AutoExec, trusted: bool, asks: &mut Asks, covered: &mut Covered) {
    let reason = kind.reason(trusted);
    match (env.spec.approval_mode, env.authority.project) {
        (ApprovalMode::Auto, Some(p)) if p.code == CodeAccess::Native => covered.project = true,
        (ApprovalMode::Auto, project) => {
            let beyond = match project {
                Some(p) => format!("{reason} Native code is beyond this project's permissions (Code: {}).", {
                    crate::permissions::code_label(p.code)
                }),
                None => format!("{reason} {NO_PROJECT_PERMISSIONS}"),
            };
            covered_or_ask(env, AllowanceKind::NativeCode, Some(beyond), asks, covered);
        }
        (ApprovalMode::Ask, _) => {
            covered_or_ask(env, AllowanceKind::NativeCode, Some(reason.to_owned()), asks, covered)
        }
    }
}

/// The rules that need nothing but the call and the task (rules 1–6): the
/// project, read only, paths, consent and scope, the execution contract, the
/// service, the project graph. The service checks them before it plans a
/// package setup, so nothing is planned for a call that is refused anyway.
pub fn precheck(call: &ToolCall, env: &ToolEnv<'_>) -> Result<(), ToolOutput> {
    contract(call, env)
}

fn setup_refused_in_restricted() -> ToolOutput {
    refusal(
        codes::NATIVE_NOT_ALLOWED,
        "Package setup installs into the project's own environment with installers that run natively; the restricted \
         container does not use that environment, and this task allows only the restricted sandbox. Nothing was \
         installed.",
    )
}

fn contract(call: &ToolCall, env: &ToolEnv<'_>) -> Result<(), ToolOutput> {
    let spec = env.spec;
    let root = env.files.root();
    workspace::check_root(env.workspace, root).map_err(ToolOutput::error)?;
    let effects = call.effects();
    if effects.changes && spec.permission == PermissionProfile::ReadOnly {
        return Err(read_only_refusal());
    }
    for (raw, path_use) in &effects.paths {
        check_path(env, raw, *path_use)?;
        let folder = matches!(path_use, PathUse::Folder | PathUse::WorkingFolder);
        if !(folder && tools::is_root(raw)) {
            // Protected paths, as requested and as resolved (links, junctions,
            // case and 8.3 aliases): the rule is about the folder the path names.
            let requested = parse_path(raw.trim())?;
            crate::graph::targets::check_path_policy(&requested, *path_use)?;
            if let Ok(Some(resolved)) = scope::resolve(root, &requested)
                && resolved != requested
            {
                crate::graph::targets::check_path_policy(&resolved, *path_use)?;
            }
        }
    }
    effects_contract(env, &effects)?;
    if let Some(kind) = effects.service
        && !env.services.available(kind)
    {
        return Err(refusal(codes::SERVICE_UNAVAILABLE, ServiceError::NotAvailable(kind).to_string()));
    }
    // The project graph: usable, bound to the task's own project, and every
    // target in a usable graph state. Never skipped for "legacy" file tools.
    match env.graph {
        Some(graph) => {
            crate::graph::bound_status(graph, root)?;
            crate::graph::targets::check_targets(graph, &effects)?;
        }
        None if matches!(call, ToolCall::GraphQuery(_) | ToolCall::GraphPropose(_) | ToolCall::WorkflowPropose(_)) => {
            return Err(crate::graph::unavailable("no project graph is attached to this task"));
        }
        None => {}
    }
    Ok(())
}

/// Rules 4 and 5 for a call's effects (what its result can carry to the model
/// and the task's execution contract), apart from the paths it names. The
/// gate applies them to every call; [`outlook`] applies the same rules to
/// describe what a kind of action would get.
fn effects_contract(env: &ToolEnv<'_>, effects: &tools::Effects<'_>) -> Result<(), ToolOutput> {
    let spec = env.spec;
    let root = env.files.root();
    match effects.exposes {
        Exposure::Metadata | Exposure::ConsentFiltered => {}
        Exposure::ScopedContent => require_content_consent(spec, effects.what)?,
        Exposure::Unconfined => {
            if spec.scope != TaskScope::Workspace {
                return Err(refusal(
                    codes::WHOLE_PROJECT_ONLY,
                    format!(
                        "{} can read any file of the project (and natively any file your account can read), so it is \
                     only available for tasks scoped to the whole project. Nothing was run.",
                        effects.what
                    ),
                ));
            }
            require_content_consent(spec, effects.what)?;
        }
    }
    if effects.launch != Launch::None {
        if spec.permission == PermissionProfile::ReadOnly || spec.execution == ExecutionMode::Disabled {
            return Err(execution_disabled());
        }
        if effects.setup && spec.execution == ExecutionMode::Restricted {
            return Err(setup_refused_in_restricted());
        }
        // Setup always runs natively: an untrusted folder refuses it before
        // anything is planned (never asked: approving cannot grant trust).
        if effects.setup
            && let Some(reason) = env.services.native_refusal(root)
        {
            return Err(ToolOutput::error_with(
                reason.clone(),
                json!({"code": codes::NATIVE_REFUSED, "status": "refused", "reason": reason}),
            ));
        }
        if effects.launch == Launch::NativeOnly && spec.execution == ExecutionMode::Restricted {
            return Err(refusal(
                codes::NATIVE_NOT_ALLOWED,
                format!(
                    "{} runs natively with your account's privileges, which this task does not allow (restricted \
                 sandbox only; there is no restricted backend for it). Nothing was run.",
                    effects.what
                ),
            ));
        }
    }
    Ok(())
}

/// What the gate would decide now for one kind of action. A description for
/// the context the agent is given, never an authorization: every call is
/// still decided by [`gate`] when it is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Proceeds without asking the user (`policy`: how a process would run).
    Proceeds { policy: Option<ExecutionPolicy>, by: AuthorizedBy },
    /// The user is asked first; `for_task`: the approval can cover the rest
    /// of the task.
    Asks { for_task: bool },
    /// Refused, with the gate's stable code.
    Refused { code: String, reason: String },
}

/// What the gate would decide now, per kind of action, from the same rules
/// and the same live authority [`gate`] uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outlook {
    /// Document, file, graph, and workflow-draft changes.
    pub changes: Expect,
    /// Changes to files the application executes (Lean sources, Lean/Lake
    /// configuration, R startup files).
    pub executed_changes: Expect,
    /// `run_python`, `run_command`.
    pub code: Expect,
    /// Native-only launches: `check_lean`, a LaTeX build.
    pub native_only: Expect,
    /// `install_packages`.
    pub setup: Expect,
}

fn expect(
    result: Result<Option<ExecutionPolicy>, ToolOutput>,
    asks: &Asks,
    covered: &Covered,
    authority: &Authority,
) -> Expect {
    match result {
        Err(refused) => Expect::Refused {
            code: refused.content.get("code").and_then(Value::as_str).unwrap_or_default().to_owned(),
            reason: refused.content.get("error").and_then(Value::as_str).unwrap_or_default().to_owned(),
        },
        Ok(_) if asks.asked => Expect::Asks { for_task: !asks.for_task().is_empty() },
        Ok(policy) => Expect::Proceeds { policy, by: covered.authorized_by(authority) },
    }
}

/// See [`Outlook`]. Paths, services, and the graph are per call and not
/// considered here.
pub fn outlook(env: &ToolEnv<'_>) -> Outlook {
    let spec = env.spec;
    let effects = |launch, what, setup| tools::Effects {
        paths: Vec::new(),
        exposes: Exposure::Unconfined,
        launch,
        changes: false,
        service: None,
        what,
        targets: false,
        setup,
    };
    let read_only = spec.permission == PermissionProfile::ReadOnly;
    let changes = {
        let (mut asks, mut covered) = (Asks::default(), Covered::default());
        let result = if read_only { Err(read_only_refusal()) } else { Ok(None) };
        if result.is_ok() {
            change_authority(env, &mut asks, &mut covered);
        }
        expect(result, &asks, &covered, &env.authority)
    };
    let executed_changes = {
        let (mut asks, mut covered) = (Asks::default(), Covered::default());
        let trusted = env.services.native_refusal(env.files.root()).is_none();
        let kind = AutoExec::LeanSource;
        let result =
            if read_only { Err(read_only_refusal()) } else { lean_contract(spec, kind, trusted).map(|()| None) };
        if result.is_ok() {
            change_authority(env, &mut asks, &mut covered);
            lean_authority(env, kind, trusted, &mut asks, &mut covered);
        }
        expect(result, &asks, &covered, &env.authority)
    };
    let launch = |launch: Launch, what: &'static str| {
        let (mut asks, mut covered) = (Asks::default(), Covered::default());
        let result = effects_contract(env, &effects(launch, what, false))
            .and_then(|()| launch_authority(env, launch, what, &mut asks, &mut covered))
            .map(Some);
        expect(result, &asks, &covered, &env.authority)
    };
    let setup = {
        let (mut asks, mut covered) = (Asks::default(), Covered::default());
        let result = effects_contract(env, &effects(Launch::NativeOnly, "Package setup", true))
            .and_then(|()| setup_authority(env, &mut asks, &mut covered))
            .map(Some);
        expect(result, &asks, &covered, &env.authority)
    };
    Outlook {
        changes,
        executed_changes,
        code: launch(Launch::NativeOrRestricted, "Code"),
        native_only: launch(Launch::NativeOnly, "Lean checking"),
        setup,
    }
}

fn decide(call: &ToolCall, env: &ToolEnv<'_>) -> Result<Gate, ToolOutput> {
    contract(call, env)?;
    let spec = env.spec;
    let root = env.files.root();
    let effects = call.effects();
    // D7: content the application executes automatically (Lean text, Lake
    // configuration) needs native authority too.
    let auto = if effects.changes { auto_exec_target(call, env)? } else { None };
    let trusted = auto.is_some() && env.services.native_refusal(root).is_none();
    if let Some(kind) = auto {
        lean_contract(spec, kind, trusted)?;
    }
    // Authority: who covers each part, and what asks the user.
    let mut asks = Asks::default();
    let mut covered = Covered::default();
    if effects.changes {
        change_authority(env, &mut asks, &mut covered);
    }
    if let Some(kind) = auto {
        lean_authority(env, kind, trusted, &mut asks, &mut covered);
    }
    let policy = match effects.launch {
        Launch::None => None,
        _ if effects.setup => Some(setup_authority(env, &mut asks, &mut covered)?),
        launch => Some(launch_authority(env, launch, effects.what, &mut asks, &mut covered)?),
    };
    // Package setup: the plan the application made (exactly what will run).
    let setup = match call {
        ToolCall::InstallPackages(input) => match &input.plan {
            Some(plan) => Some(plan.clone()),
            None => {
                return Err(refusal(
                    codes::SERVICE_UNAVAILABLE,
                    "The package setup could not be planned, so nothing was installed.",
                ));
            }
        },
        _ => None,
    };
    let project_op = match call {
        ToolCall::DeletePaths(_) | ToolCall::CopyPaths(_) => {
            let prepared = prepare_project_op(call, env)?;
            if prepared.plan.needs_decision {
                if spec.approval_mode == ApprovalMode::Ask && spec.permission != PermissionProfile::ReviewEdits {
                    return Err(needs_decision(&prepared.plan));
                }
                // Consequences only the user decides: one approval, never repeatable.
                asks.ask(
                    Some("It has consequences beyond the files themselves; only you decide those.".to_owned()),
                    None,
                );
            }
            Some(prepared)
        }
        _ => None,
    };
    let insertion = match call {
        ToolCall::InsertArtifact(input) => prepare_insertion(env, input, asks.asked)?,
        _ => None,
    };
    // A save: the document's state decides before anyone is asked (a
    // conflict, recovered text, or a stale revision is refused, not shown).
    let save = match call {
        ToolCall::SaveDocument(input) => Some(prepare_save(env, input)?),
        _ => None,
    };
    let proposal = match (call, env.graph) {
        (ToolCall::GraphPropose(input), Some(graph)) => {
            Some(crate::graph::propose::prepare(graph, root, spec, env.task_id, input)?)
        }
        _ => None,
    };
    let workflow = match (call, env.graph) {
        (ToolCall::WorkflowPropose(input), Some(graph)) => {
            Some(crate::graph::workflow::prepare(graph, root, spec, input)?)
        }
        _ => None,
    };
    let native = policy == Some(ExecutionPolicy::TrustedNative);
    let mut authorization = Authorization {
        policy,
        root: root.to_path_buf(),
        approved: None,
        insertion: None,
        proposal: None,
        workflow: None,
        project_op: None,
        setup,
        save: None,
        basis: env.authority.basis(spec.approval_mode),
        authorized_by: covered.authorized_by(&env.authority),
    };
    if !asks.asked {
        authorization.insertion = insertion.map(|(planned, _)| planned);
        authorization.proposal = proposal.map(Box::new);
        authorization.workflow = workflow.map(Box::new);
        authorization.project_op = project_op.map(Box::new);
        authorization.save = save.map(Box::new);
        return Ok(Gate::Allowed(authorization));
    }
    let (title, mut detail) = match (&proposal, &workflow, &project_op, &save) {
        (Some(prepared), _, _, _) => (prepared.title.clone(), prepared.detail.clone()),
        (_, Some(prepared), _, _) => (prepared.title.clone(), prepared.detail.clone()),
        (_, _, Some(prepared), _) => (prepared.title.clone(), prepared.detail.clone()),
        (_, _, _, Some(prepared)) => (prepared.title.clone(), prepared.detail.clone()),
        _ => describe(call, insertion.as_ref().map(|(_, shown)| shown.as_str())),
    };
    authorization.save = save.map(Box::new);
    authorization.project_op = project_op.map(Box::new);
    authorization.proposal = proposal.map(Box::new);
    authorization.workflow = workflow.map(Box::new);
    if native {
        detail.push_str("\n\nRuns natively with your account's privileges.");
    }
    if let Some(kind) = auto {
        detail.push_str(&format!("\n\n{}", kind.consequence(trusted)));
    }
    authorization.insertion = insertion.map(|(planned, _)| planned);
    authorization.approved = Some(detail.clone());
    Ok(Gate::NeedsApproval { title, detail, reason: asks.reason(), for_task: asks.for_task(), authorization })
}

/// A consequential deletion or copy in an Ask task that applies changes
/// directly: refused with the consequences (only the user decides those).
fn needs_decision(plan: &exposition_core::ops::OpPlan) -> ToolOutput {
    let impacts: Vec<Value> = plan
        .impacts
        .iter()
        .filter(|i| i.kind.needs_decision())
        .map(|i| json!({"path": target_label(&i.target), "kind": i.kind, "consequence": i.message}))
        .collect();
    ToolOutput::error_with(
        "This needs the user's decision because it has consequences beyond the files themselves (listed in \
         `impacts`). Nothing was changed; ask the user to do it, or to run the task with review of each change.",
        json!({"code": codes::NEEDS_DECISION, "impacts": impacts}),
    )
}

fn require_content_consent(spec: &TaskSpec, what: &str) -> Result<(), ToolOutput> {
    if spec.consent == ContextConsent::MetadataOnly {
        return Err(refusal(
            codes::METADATA_ONLY,
            format!(
                "The user's context consent for this task allows only file names and metadata to be sent to the model \
             provider. {what} returns file content or results derived from it, so it was refused."
            ),
        ));
    }
    Ok(())
}

fn permissions_changed() -> ToolOutput {
    refusal(
        codes::PERMISSIONS_CHANGED,
        "The project's permissions or this task's allowances changed after this action was authorized, so it was \
         not done. Call the tool again; it is decided under the current permissions.",
    )
}

/// After the user approved: the call runs only as approved, in the task's own
/// project, under the same authority. The gate is evaluated again; it must
/// still ask for exactly the approval the user gave (an insertion re-planned
/// on a changed document, a revoked trust, a switched project, or changed
/// project permissions or allowances refuse the call).
pub fn confirm(call: &ToolCall, env: &ToolEnv<'_>, approved: Authorization) -> Result<Authorization, ToolOutput> {
    if let Err(reason) = workspace::check_root(env.workspace, &approved.root) {
        return Err(project_changed(&reason));
    }
    if env.files.root() != approved.root {
        return Err(project_changed("the task's folder is not the folder the action was approved for."));
    }
    match gate(call, env) {
        Gate::Refused(output) => Err(output),
        Gate::Allowed(authorization) | Gate::NeedsApproval { authorization, .. }
            if authorization.basis != approved.basis =>
        {
            Err(permissions_changed())
        }
        Gate::NeedsApproval { authorization, .. } if authorization.approved == approved.approved => Ok(authorization),
        Gate::Allowed(_) | Gate::NeedsApproval { .. } => Err(refusal(
            codes::CHANGED_SINCE_APPROVAL,
            "What this action would do changed while it waited for approval (for example, the document or the \
             artifact changed). Nothing was done; call the tool again so the user can approve the current version.",
        )),
    }
}

/// Immediately before a call runs: the project's permissions and the task's
/// allowances are still the ones it was authorized under.
pub fn check_basis(env: &ToolEnv<'_>, authorization: &Authorization) -> Result<(), ToolOutput> {
    if env.authority.basis(env.spec.approval_mode) == authorization.basis { Ok(()) } else { Err(permissions_changed()) }
}

/// Characters that are invisible, or reorder the text around them, in the
/// approval view.
fn hidden(c: char, next: Option<char>) -> bool {
    match c {
        '\n' | '\t' => false,
        '\r' => next != Some('\n'),
        c if c.is_control() => true,
        // The escape's own opening bracket, so a literal "⟨U+…⟩" is never mistaken for an escape.
        '\u{27E8}' => true,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}' | '\u{3164}'
        | '\u{FEFF}' | '\u{FFA0}' => true,
        '\u{180B}'..='\u{180F}'
        | '\u{200B}'..='\u{200F}'
        | '\u{2028}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0000}'..='\u{E0FFF}' => true,
        _ => false,
    }
}

/// A payload as shown for approval: complete, with invisible and
/// direction-changing characters written as ⟨U+XXXX⟩, so what the user reads
/// is exactly what runs or changes.
pub fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if hidden(c, chars.peek().copied()) {
            out.push_str(&format!("⟨U+{:04X}⟩", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// Title and complete detail of an approval request.
fn describe(call: &ToolCall, insertion: Option<&str>) -> (String, String) {
    let (title, detail) = match call {
        ToolCall::EditDocument(input) => {
            let mut detail = format!(
                "Edit {} (based on revision {}), {} replacement(s):\n",
                input.path,
                input.base_revision,
                input.edits.len()
            );
            for (i, edit) in input.edits.iter().enumerate() {
                detail.push_str(&format!(
                    "\n#{}\n- {}\n+ {}\n",
                    i + 1,
                    shown(&edit.old_text).replace('\n', "\n  "),
                    shown(&edit.new_text).replace('\n', "\n  ")
                ));
            }
            (format!("Apply edits to {}", input.path), detail)
        }
        ToolCall::CreateFile(input) => (
            format!("Create {}", input.path),
            format!("Create {} with content ({} bytes):\n\n{}", input.path, input.content.len(), shown(&input.content)),
        ),
        ToolCall::InsertArtifact(input) => {
            let action = if input.update_key.is_empty() {
                format!("Insert artifact {} of run {} into {}", input.artifact_id, input.run_id, input.path)
            } else {
                format!(
                    "Update attachment {} in {} with artifact {} of run {}",
                    input.update_key, input.path, input.artifact_id, input.run_id
                )
            };
            let mut detail = action.clone();
            if !input.caption.is_empty() {
                detail.push_str(&format!("\nCaption: {}", shown(&input.caption)));
            }
            if let Some(text) = insertion {
                detail.push_str(&format!("\n\n{text}"));
            }
            (action, detail)
        }
        ToolCall::BuildDocument(input) => (
            format!("Build {} with LaTeX", input.entry),
            format!(
                "Build {} with {} (TeX tools as the project needs them, e.g. latexmk and biber). TeX can read any \
                 file your account can read, and the build log and diagnostics are returned to the agent.",
                input.entry,
                match input.engine.as_str() {
                    "lualatex" => "LuaLaTeX",
                    "pdflatex" => "pdfLaTeX",
                    _ => "the project's LaTeX engine",
                }
            ),
        ),
        ToolCall::RunPython(input) => (
            "Run Python code".to_owned(),
            format!(
                "Run in this task's separate research session ({} bytes):\n\n{}",
                input.code.len(),
                shown(&input.code)
            ),
        ),
        ToolCall::CheckLean(input) => (
            format!("Check {} with Lean", input.path),
            format!("Check {} with the project's Lean toolchain (lake serve)", input.path),
        ),
        ToolCall::RunCommand(input) => {
            let wd = if tools::is_root(&input.working_directory) { "." } else { input.working_directory.as_str() };
            let args: Vec<String> = input.args.iter().map(|a| format!("{a:?}")).collect();
            (
                format!("Run {}", input.program),
                format!(
                    "Program: {}\nArguments: [{}]\nWorking folder: {}",
                    shown(&format!("{:?}", input.program)),
                    shown(&args.join(", ")),
                    shown(wd)
                ),
            )
        }
        ToolCall::InstallPackages(input) => match &input.plan {
            Some(plan) => describe_setup(plan),
            None => (String::new(), String::new()),
        },
        ToolCall::ListFiles(_)
        | ToolCall::ReadFile(_)
        | ToolCall::GraphQuery(_)
        | ToolCall::GraphPropose(_)
        | ToolCall::WorkflowPropose(_)
        | ToolCall::SaveDocument(_)
        | ToolCall::DeletePaths(_)
        | ToolCall::CopyPaths(_) => (String::new(), String::new()),
    };
    let detail = if detail.contains("⟨U+") {
        format!(
            "{detail}\n\nNote: invisible or direction-changing characters are shown as ⟨U+XXXX⟩; they are part of \
             what runs or changes."
        )
    } else {
        detail
    };
    (title, detail)
}

/// A workspace refusal of a save, as the tool result states it: conflicts
/// and recovered text are the user's to resolve.
fn save_refusal(e: exposition_workspace::WorkspaceError) -> ToolOutput {
    match e.code {
        "file.modifiedOnDisk"
        | "document.conflict"
        | "document.recovered"
        | "document.recoveryPending"
        | "document.fileDeleted" => ToolOutput::error_with(
            format!(
                "{} Nothing was saved and the file on disk was not changed. Only the user resolves this, in the \
                 editor; tell them, and do not work around it.",
                e.message
            ),
            json!({"code": codes::SAVE_CONFLICT, "saved": false, "reason": e.code}),
        ),
        code => {
            ToolOutput::error_with(format!("{} Nothing was saved.", e.message), json!({"saved": false, "reason": code}))
        }
    }
}

/// Whether the text a save of `document` writes holds changes this task did
/// not make: every change since the saved revision is read from the document
/// service's history.
fn other_changes(env: &ToolEnv<'_>, document: &DocumentId) -> OtherChanges {
    let Some(saved) = env.documents.info(document).ok().and_then(|info| info.saved_revision) else {
        return OtherChanges::Unknown;
    };
    match env.documents.origins_since(document, saved) {
        Ok(Some(origins)) => {
            let own =
                |origin: &EditOrigin| matches!(origin, EditOrigin::Agent { task_id } if *task_id == env.task_id.0);
            if origins.iter().all(own) { OtherChanges::None } else { OtherChanges::Included }
        }
        _ => OtherChanges::Unknown,
    }
}

/// The enabled workflow triggers watching `path`, through the application's
/// project operations (the lookup a deletion's consequences use). A missing
/// service or a failed read is unknown, not none.
fn save_triggers(env: &ToolEnv<'_>, path: &WorkspacePath) -> SaveTriggers {
    match env.services.project_operations().map(|ops| ops.triggers_watching(path)) {
        Some(Ok(mut triggers)) => {
            triggers.sort();
            triggers.dedup();
            SaveTriggers::Known(triggers)
        }
        Some(Err(_)) | None => SaveTriggers::Unknown,
    }
}

/// What a save changes in the file, complete: every line it removes from the
/// version on disk and every line it adds, with two lines of context.
fn save_changes(disk: &str, text: &str) -> String {
    let diff = similar::TextDiff::configure().timeout(std::time::Duration::from_millis(500)).diff_lines(disk, text);
    let mut out = String::new();
    for group in diff.grouped_ops(2) {
        let Some(first) = group.first() else { continue };
        out.push_str(&format!("\n@@ from line {} @@\n", first.old_range().start + 1));
        for op in &group {
            for change in diff.iter_changes(op) {
                let sign = match change.tag() {
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                    similar::ChangeTag::Equal => ' ',
                };
                let line = change.value();
                out.push_str(&format!("{sign} {}", shown(line.strip_suffix('\n').unwrap_or(line))));
                out.push('\n');
                if !line.ends_with('\n') {
                    out.push_str("  (no line break at the end of the file)\n");
                }
            }
        }
    }
    out
}

/// Prepare a save at the gate: the document must be open, at the revision
/// the agent named, with unsaved changes, and in a state the workspace
/// service's strict save accepts (no disk conflict, no recovered text, the
/// file unchanged on disk since it was opened or last saved). The
/// description is complete: what is written, whose changes it holds, and
/// every line that changes on disk.
fn prepare_save(env: &ToolEnv<'_>, input: &SaveDocumentInput) -> Result<PreparedSave, ToolOutput> {
    let path = parse_path(&input.path)?;
    let Some(workspace) = env.workspace else {
        return Err(refusal(
            codes::SERVICE_UNAVAILABLE,
            "Saving goes through the editor's document service, which is not available here. Nothing was saved.",
        ));
    };
    let Some(id) = env.documents.find_by_path(&path) else {
        return Err(ToolOutput::error_with(
            format!(
                "{path} is not open in the editor, so it has no unsaved text: the file on disk is its saved version. \
                 Nothing was saved."
            ),
            json!({"saved": false}),
        ));
    };
    let preview = workspace.exact_save_preview(&id).map_err(save_refusal)?;
    if preview.revision != input.revision {
        return Err(ToolOutput::error_with(
            format!(
                "{path} is at revision {}, not revision {}: it changed since you read or edited it. Nothing was \
                 saved. Read the file again and save the revision you mean.",
                preview.revision, input.revision
            ),
            json!({"saved": false, "currentRevision": preview.revision}),
        ));
    }
    if !preview.unsaved {
        return Err(ToolOutput::error_with(
            format!(
                "{path} has no unsaved changes: the file on disk already holds revision {} of the editor text. \
                 Nothing was written.",
                preview.revision
            ),
            json!({"code": codes::NOTHING_TO_SAVE, "saved": false, "unsavedChanges": false}),
        ));
    }
    let others = other_changes(env, &id);
    let mut detail = format!(
        "Save {path} to disk: revision {} of the editor text ({} bytes, sha256 {}) replaces the file's saved \
         version.\n{}\n",
        preview.revision,
        preview.bytes,
        preview.content_hash,
        others.note()
    );
    if let Some(note) = save_triggers(env, &path).note() {
        detail.push_str(&format!("\n{note}\n"));
    }
    match &preview.disk_text {
        Some(disk) => {
            detail.push_str("\nWhat changes in the file (- on disk now, + after the save):\n");
            detail.push_str(&save_changes(disk, &preview.text));
        }
        None => {
            detail.push_str("\nThere is no file on disk to compare with. The text that is written:\n\n");
            detail.push_str(&shown(&preview.text));
        }
    }
    let mut detail = detail.trim_end().to_owned();
    if detail.contains("⟨U+") {
        detail.push_str(
            "\n\nNote: invisible or direction-changing characters are shown as ⟨U+XXXX⟩; they are part of what is \
             saved.",
        );
    }
    Ok(PreparedSave {
        title: format!("Save {path}"),
        detail,
        path,
        document: id,
        revision: preview.revision,
        content_hash: preview.content_hash,
        others,
    })
}

/// Where a saved document is (in every such result).
const SAVED_NOTE: &str = "Saved to disk with the editor's own save: the file now holds this revision of the editor \
    text, so programs you run and copy_paths see it. The user still reviews your changes (Keep or Revert); a revert \
    changes the editor text, and the file keeps what was saved until it is saved again.";

/// Save the prepared revision with the workspace service's strict save (the
/// user's save, which also refuses whatever only the user can decide). The
/// checks of [`prepare_save`] are made again by the save itself, under its
/// lock: a document that changed since the gate is not saved.
fn save_document(env: &ToolEnv<'_>, prepared: &PreparedSave) -> Result<ToolOutput, ToolOutput> {
    let Some(workspace) = env.workspace else {
        return Err(refusal(
            codes::SERVICE_UNAVAILABLE,
            "Saving goes through the editor's document service, which is not available here. Nothing was saved.",
        ));
    };
    workspace::check_root(Some(workspace), env.files.root()).map_err(|reason| project_changed(&reason))?;
    let path = &prepared.path;
    // Whose changes the text holds is read again: it is reported as saved.
    let others = other_changes(env, &prepared.document);
    // So are the triggers watching the file: a save without a card states them here.
    let triggers = save_triggers(env, path);
    let saved = workspace.save_document_exact(&prepared.document, prepared.revision).map_err(|e| match e.code {
        "document.changed" => ToolOutput::error_with(
            format!(
                "{path} changed after this save was authorized (it is no longer at revision {}). Nothing was saved. \
                 Read the file again and save the revision you mean.",
                prepared.revision
            ),
            json!({"code": codes::CHANGED_SINCE_APPROVAL, "saved": false}),
        ),
        _ => save_refusal(e),
    })?;
    let graph_info = resolve_target(env, path, TargetUse::Edit);
    let mut result = json!({
        "path": path.as_str(),
        "saved": true,
        "revision": saved.revision,
        "bytes": saved.bytes,
        "contentHash": saved.content_hash,
        "unsavedChanges": saved.info.saved_revision != Some(saved.info.revision),
        "otherChanges": others.key(),
        "otherChangesNote": others.result_note(),
        "note": SAVED_NOTE,
        "triggersKnown": triggers != SaveTriggers::Unknown,
        "triggers": triggers.list(),
    });
    if let Some(note) = triggers.result_note() {
        result["triggersNote"] = note.into();
    }
    if let Some(graph) = graph_info {
        result["graph"] = graph;
    }
    Ok(ToolOutput::ok(result))
}

/// The text of `path` now: its open buffer, or the file on disk (without
/// opening it), with the document and revision when it is open.
/// A document's text, with its id and revision when it is open.
type CurrentText = (Arc<str>, Option<(DocumentId, u64)>);

fn current_text(env: &ToolEnv<'_>, path: &WorkspacePath) -> Result<CurrentText, ToolOutput> {
    if let Some(id) = env.documents.find_by_path(path)
        && let Ok(snapshot) = env.documents.snapshot(&id)
    {
        return Ok((snapshot.text, Some((id, snapshot.revision))));
    }
    if let Some(workspace) = env.workspace
        && let Ok(snapshot) = workspace.snapshot()
        && let Ok(input) = snapshot.read(path)
        && let Some(text) = input.text
    {
        let open = input.document.zip(input.revision);
        return Ok((text, open));
    }
    let loaded = env.files.read_text(path).map_err(file_error)?;
    Ok((Arc::from(loaded.text), None))
}

/// The artifact an insertion uses must itself be in the task's scope (it
/// becomes readable in the document); for approval (`shown`), plan the exact
/// text now.
fn prepare_insertion(
    env: &ToolEnv<'_>,
    input: &InsertArtifactInput,
    shown_for_approval: bool,
) -> Result<Option<(PlannedInsertion, String)>, ToolOutput> {
    let artifact = env
        .services
        .research_artifact(&input.run_id, &input.artifact_id)
        .map_err(|e| ToolOutput::error(e.to_string()))?;
    let source = artifact
        .path
        .as_ref()
        .ok_or_else(|| ToolOutput::error("The artifact is not stored in the project, so it cannot be inserted."))?;
    scope::check(env.files.root(), &env.spec.scope, source).map_err(|e| {
        refusal(
            e.code(),
            format!(
                "The artifact file {source} is not in this task's scope ({}), so its content cannot be inserted. {}",
                env.spec.scope.describe(),
                e.message(&env.spec.scope)
            ),
        )
    })?;
    if !shown_for_approval {
        return Ok(None);
    }
    let path = parse_path(&input.path)?;
    let (text, open) = current_text(env, &path)?;
    let (document, revision) = open.unwrap_or_else(|| (DocumentId("preview".into()), 0));
    let plan = plan_insertion(env, input, &path, artifact.clone(), &document, revision, &text, None)?;
    let texts: Vec<String> =
        plan.transaction.edits.iter().flat_map(|e| e.patches.iter().map(|p| p.text.clone())).collect();
    let placement = if !input.update_key.is_empty() {
        format!("Replaces the block of attachment {}.", input.update_key)
    } else if input.after_text.is_empty() {
        "Inserted at the end of the document.".to_owned()
    } else {
        format!("Inserted after:\n  {}", shown(&input.after_text).replace('\n', "\n  "))
    };
    let shown_text = format!(
        "Artifact: {:?} {source} ({}, {} bytes)\n{placement}\n\nText that will be inserted (attachment key {}):\n\n{}",
        artifact.kind,
        artifact.media_type,
        artifact.size_bytes,
        plan.attachment_key,
        texts.iter().map(|t| shown(t)).collect::<Vec<_>>().join("\n…\n")
    );
    Ok(Some((PlannedInsertion { key: plan.attachment_key, texts }, shown_text)))
}

#[allow(clippy::too_many_arguments)]
fn plan_insertion(
    env: &ToolEnv<'_>,
    input: &InsertArtifactInput,
    path: &WorkspacePath,
    artifact: ArtifactRecord,
    document: &DocumentId,
    revision: u64,
    text: &str,
    key: Option<String>,
) -> Result<exposition_attach::AttachPlan, ToolOutput> {
    use exposition_core::artifact::{AttachArtifactRequest, AttachmentPlacement, RunId};
    let format = exposition_attach::Format::from_path(path)
        .ok_or_else(|| ToolOutput::error(format!("{path} is not a Typst, LaTeX, or Markdown document.")))?;
    let placement = if input.update_key.is_empty() {
        let offset = if input.after_text.is_empty() {
            text.len()
        } else {
            let mut matches = text.match_indices(input.after_text.as_str());
            let Some((start, _)) = matches.next() else {
                return Err(ToolOutput::error(format!("after_text was not found in {path}; read the file again.")));
            };
            if matches.next().is_some() {
                return Err(ToolOutput::error("after_text occurs more than once; include more surrounding text."));
            }
            start + input.after_text.len()
        };
        let offset =
            exposition_core::text::byte_to_utf16(text, offset).map_err(|e| ToolOutput::error(e.to_string()))?;
        AttachmentPlacement::Insert { offset: offset as u32 }
    } else {
        AttachmentPlacement::Update { attachment_key: input.update_key.clone() }
    };
    let request = AttachArtifactRequest {
        document: document.clone(),
        base_revision: revision,
        artifact,
        run: Some(RunId(input.run_id.clone())),
        placement,
        caption: (!input.caption.is_empty()).then(|| input.caption.clone()),
        attachment_key: key,
        origin: EditOrigin::Agent { task_id: env.task_id.0.clone() },
    };
    let content = exposition_attach::load_content(env.files.root(), format, path, &request.artifact)
        .map_err(|e| ToolOutput::error(e.to_string()))?;
    exposition_attach::plan(
        &exposition_attach::PlanInput { format, document, document_path: path, revision, text },
        &request,
        &content,
    )
    .map_err(|e| ToolOutput::error(e.to_string()))
}

/// Run a call the gate allowed (and, if needed, the user approved), bound to
/// the project folder it was authorized for.
pub async fn run(
    call: ToolCall,
    env: &ToolEnv<'_>,
    seen: &mut SeenTexts,
    tool_use_id: &str,
    authorization: Authorization,
    cancel: CancellationToken,
) -> ToolOutput {
    if env.files.root() != authorization.root {
        return project_changed("the task's folder is not the folder the action was authorized for.");
    }
    if let Err(reason) = workspace::check_root(env.workspace, &authorization.root) {
        return project_changed(&reason);
    }
    if let Err(refused) = check_basis(env, &authorization) {
        return refused;
    }
    let Authorization { policy, insertion, proposal, workflow, project_op, setup, save, authorized_by, .. } =
        authorization;
    let result = match call {
        ToolCall::GraphQuery(input) => match env.graph {
            Some(graph) => crate::graph::query::run(graph, env.files.root(), env.spec, &input),
            None => Err(crate::graph::unavailable("no project graph is attached to this task")),
        },
        ToolCall::GraphPropose(_) => match (env.graph, proposal) {
            (Some(graph), Some(prepared)) => crate::graph::propose::apply(
                graph,
                env.files.root(),
                env.spec,
                env.task_id,
                env.store,
                tool_use_id,
                &prepared,
            ),
            _ => Err(crate::graph::unavailable("the graph change was not validated by the gate")),
        },
        ToolCall::WorkflowPropose(_) => match (env.graph, workflow) {
            (Some(graph), Some(prepared)) => {
                crate::graph::workflow::save(graph, env.files.root(), env.task_id, &prepared)
            }
            _ => Err(crate::graph::unavailable("the workflow draft was not validated by the gate")),
        },
        ToolCall::DeletePaths(_) | ToolCall::CopyPaths(_) => match project_op {
            Some(prepared) => apply_project_op(env, tool_use_id, &prepared),
            None => Err(ToolOutput::error("the operation was not planned by the gate")),
        },
        ToolCall::ListFiles(input) => list_files(env, input),
        ToolCall::ReadFile(input) => read_file(env, seen, input),
        ToolCall::EditDocument(input) => edit_document(env, seen, tool_use_id, input),
        ToolCall::CreateFile(input) => create_file(env, seen, tool_use_id, input),
        ToolCall::SaveDocument(_) => match save {
            Some(prepared) => save_document(env, &prepared),
            None => Err(ToolOutput::error("the save was not prepared by the gate")),
        },
        ToolCall::BuildDocument(input) => match parse_path(&input.entry) {
            Ok(entry) => {
                let engine = match input.engine.as_str() {
                    "pdflatex" => LatexEngine::PdfLatex,
                    "lualatex" => LatexEngine::LuaLatex,
                    _ => LatexEngine::Auto,
                };
                let graph_info = resolve_target(env, &entry, TargetUse::Build);
                let request = BuildRequest {
                    entry,
                    engine,
                    task_id: env.task_id.0.clone(),
                    root: env.files.root().to_path_buf(),
                    scope: env.spec.scope.clone(),
                    policy,
                };
                with_graph(service_result(env.services.build_document(request, cancel).await), graph_info)
            }
            Err(e) => Err(e),
        },
        ToolCall::InsertArtifact(input) => insert_artifact(env, seen, tool_use_id, input, insertion),
        ToolCall::RunPython(input) => {
            let Some(policy) = policy else { return ToolOutput::error("missing execution policy") };
            let unsaved = unsaved_documents(env);
            let result = service_result(
                env.services
                    .run_python(
                        PythonRequest {
                            code: input.code,
                            task_id: env.task_id.0.clone(),
                            root: env.files.root().to_path_buf(),
                            working_directory: env.files.root().to_path_buf(),
                            policy,
                            session: format!("agent-{}", env.task_id.0),
                            authorization: authorized_by.run_authorization(env.task_id, env.spec.approval_mode),
                        },
                        cancel,
                    )
                    .await,
            );
            with_disk_note(result, unsaved)
        }
        ToolCall::CheckLean(input) => {
            let Some(policy) = policy else { return ToolOutput::error("missing execution policy") };
            match parse_path(&input.path) {
                Ok(path) => {
                    let graph_info = resolve_target(env, &path, TargetUse::Check);
                    let result = service_result(
                        env.services.check_lean(LeanRequest { path: path.clone(), policy }, cancel).await,
                    );
                    with_graph(with_checked_text(result, env, &path), graph_info)
                }
                Err(e) => Err(e),
            }
        }
        ToolCall::RunCommand(input) => {
            let unsaved = unsaved_documents(env);
            with_disk_note(run_command(env, input, policy, cancel).await, unsaved)
        }
        ToolCall::InstallPackages(_) => match (setup, policy) {
            (Some(plan), Some(ExecutionPolicy::TrustedNative)) => service_result(
                env.services
                    .install_packages(
                        super::services::SetupInstallRequest {
                            root: env.files.root().to_path_buf(),
                            task_id: env.task_id.0.clone(),
                            plan: *plan,
                        },
                        cancel,
                    )
                    .await,
            ),
            _ => Err(ToolOutput::error("the package setup was not authorized by the gate")),
        },
    };
    result.unwrap_or_else(|e| e)
}

/// Open documents in the task's scope whose editor text is not saved, now
/// (bounded). A process reads files from disk, so it does not see that text.
fn unsaved_documents(env: &ToolEnv<'_>) -> Vec<String> {
    const LIMIT: usize = 20;
    let mut paths: Vec<String> = env
        .documents
        .list()
        .into_iter()
        .filter(|info| info.saved_revision != Some(info.revision))
        .filter_map(|info| info.path)
        .filter(|path| env.spec.scope.allows(path) && !crate::graph::targets::graph_storage(path))
        .map(|path| path.as_str().to_owned())
        .collect();
    paths.sort();
    paths.truncate(LIMIT);
    paths
}

/// Say which snapshot a process saw: the files on disk, not the unsaved
/// editor text of the documents listed (as they were when it started).
fn with_disk_note(result: Result<ToolOutput, ToolOutput>, unsaved: Vec<String>) -> Result<ToolOutput, ToolOutput> {
    if unsaved.is_empty() {
        return result;
    }
    let add = |mut output: ToolOutput| {
        if let Some(obj) = output.content.as_object_mut() {
            obj.insert("unsavedNotSeen".into(), json!(unsaved));
            obj.insert(
                "filesNote".into(),
                json!(
                    "The process read files as saved on disk. The files in unsavedNotSeen had unsaved editor changes \
                     when it started, which it did not see. save_document saves one of them when this task needs it \
                     on disk (the whole editor text, the user's unsaved changes included); otherwise the user saves."
                ),
            );
        }
        output
    };
    match result {
        Ok(output) => Ok(add(output)),
        Err(output) => Err(add(output)),
    }
}

/// Say which text a Lean check used: the report's `revision` is the editor
/// revision checked (absent when the file was read from disk).
fn with_checked_text(
    result: Result<ToolOutput, ToolOutput>,
    env: &ToolEnv<'_>,
    path: &WorkspacePath,
) -> Result<ToolOutput, ToolOutput> {
    let saved_revision =
        env.documents.find_by_path(path).and_then(|id| env.documents.info(&id).ok()).map(|info| info.saved_revision);
    let add = |mut output: ToolOutput| {
        let Some(obj) = output.content.as_object_mut() else { return output };
        // Only a result that reports a checked text (it carries its hash).
        if !obj.contains_key("contentHash") {
            return output;
        }
        let text = match (obj.get("revision").and_then(Value::as_u64), saved_revision) {
            (Some(revision), Some(saved)) if saved == Some(revision) => {
                format!("the editor text at revision {revision}, which is the saved file (no unsaved changes)")
            }
            (Some(revision), Some(_)) => format!(
                "the editor text at revision {revision}, which has unsaved changes: the file on disk differs until it \
                 is saved"
            ),
            _ => "the saved file on disk".to_owned(),
        };
        obj.insert("checkedText".into(), json!(text));
        output
    };
    match result {
        Ok(output) => Ok(add(output)),
        Err(output) => Err(add(output)),
    }
}

/// The saved version of a file (its hash on disk), never an unsaved buffer.
fn saved_hash(env: &ToolEnv<'_>, path: &WorkspacePath) -> Option<String> {
    if let Some(id) = env.documents.find_by_path(path)
        && let Ok(info) = env.documents.info(&id)
        && info.saved_revision != Some(info.revision)
    {
        return None;
    }
    let bytes = std::fs::read(scope::resolve(env.files.root(), path).ok().flatten().map(|p| {
        let mut abs = env.files.root().to_path_buf();
        for part in p.components() {
            abs.push(part);
        }
        abs
    })?)
    .ok()?;
    Some(sha256_hex(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The spelling of `path` as it exists on disk (links and case/8.3 aliases
/// resolved; components that do not exist yet kept as given). Graph
/// identities use it, so an alias never becomes a second identity.
fn on_disk(env: &ToolEnv<'_>, path: &WorkspacePath) -> WorkspacePath {
    scope::resolve(env.files.root(), path).ok().flatten().unwrap_or_else(|| path.clone())
}

/// Resolve a target to its graph identity (when the task has a graph) and
/// record it with the task; the description goes into the tool result.
fn resolve_target(env: &ToolEnv<'_>, path: &WorkspacePath, target_use: TargetUse) -> Option<Value> {
    let graph = env.graph?;
    let hash = saved_hash(env, path);
    let path = on_disk(env, path);
    Some(crate::graph::targets::resolve(graph, env.store, env.task_id, &path, target_use, hash.as_deref()))
}

/// Add the graph description to a result (success or error object).
fn with_graph(result: Result<ToolOutput, ToolOutput>, graph: Option<Value>) -> Result<ToolOutput, ToolOutput> {
    let Some(graph) = graph else { return result };
    let add = |mut output: ToolOutput| {
        if let Some(obj) = output.content.as_object_mut() {
            obj.insert("graph".into(), graph.clone());
        }
        output
    };
    match result {
        Ok(output) => Ok(add(output)),
        Err(output) => Err(add(output)),
    }
}

fn service_result(result: Result<Value, ServiceError>) -> Result<ToolOutput, ToolOutput> {
    match result {
        Ok(value) => Ok(ToolOutput::ok(value)),
        // A genuine failure reported by the service, with its details.
        Err(ServiceError::Outcome(value)) => Err(ToolOutput { content: value, is_error: true }),
        Err(e) => Err(ToolOutput::error(e.to_string())),
    }
}

async fn run_command(
    env: &ToolEnv<'_>,
    input: RunCommandInput,
    policy: Option<ExecutionPolicy>,
    cancel: CancellationToken,
) -> Result<ToolOutput, ToolOutput> {
    let Some(policy) = policy else { return Err(ToolOutput::error("missing execution policy")) };
    let mut working_directory = env.files.root().to_path_buf();
    if let Some(dir) = parse_dir(&input.working_directory)? {
        for part in dir.components() {
            working_directory.push(part);
        }
    }
    service_result(
        env.services
            .run_command(
                CommandRequest {
                    program: input.program,
                    args: input.args,
                    working_directory,
                    root: env.files.root().to_path_buf(),
                    policy,
                    task_id: env.task_id.0.clone(),
                },
                cancel,
            )
            .await,
    )
}

fn file_error(e: FileError) -> ToolOutput {
    ToolOutput::error(e.to_string())
}

/// List through the workspace service (its link and hidden-file rules), breadth first.
fn list_with_workspace(
    workspace: &WorkspaceService,
    dir: Option<&WorkspacePath>,
    recursive: bool,
) -> Result<workspace::Listing, ToolOutput> {
    use exposition_workspace::types::EntryKind;
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut queue: VecDeque<Option<WorkspacePath>> = VecDeque::from([dir.cloned()]);
    'walk: while let Some(current) = queue.pop_front() {
        let mut items = workspace.list_dir(current.as_ref()).map_err(|e| ToolOutput::error(e.message))?;
        items.sort_by(|a, b| a.path.cmp(&b.path));
        for item in items {
            if item.unavailable.is_some() || item.kind == EntryKind::Other {
                continue;
            }
            if entries.len() >= LIST_LIMIT {
                truncated = true;
                break 'walk;
            }
            let is_dir = item.kind == EntryKind::Directory;
            if is_dir && workspace::SKIPPED_DIRS.contains(&item.name.as_str()) {
                continue;
            }
            if recursive && is_dir && !item.link {
                queue.push_back(Some(item.path.clone()));
            }
            entries.push(workspace::FileEntry {
                path: item.path.to_string(),
                kind: if is_dir { "directory" } else { "file" },
                size: item.size,
            });
        }
    }
    Ok(workspace::Listing { entries, truncated })
}

fn list_files(env: &ToolEnv<'_>, input: ListFilesInput) -> Result<ToolOutput, ToolOutput> {
    let dir = parse_dir(&input.path)?;
    let listing = match env.workspace {
        Some(workspace) => list_with_workspace(workspace, dir.as_ref(), input.recursive)?,
        None => env.files.list(dir.as_ref(), input.recursive, LIST_LIMIT).map_err(file_error)?,
    };
    let open: HashMap<String, (u64, bool)> = env
        .documents
        .list()
        .into_iter()
        .filter_map(|info| {
            let unsaved = info.saved_revision != Some(info.revision);
            info.path.map(|p| (p.as_str().to_owned(), (info.revision, unsaved)))
        })
        .collect();
    let resources = env.graph.map(graph_resources).unwrap_or_default();
    let entries: Vec<Value> = listing
        .entries
        .into_iter()
        .filter(|e| entry_visible(&e.path, &env.spec.scope))
        .filter(|e| WorkspacePath::parse(&e.path).map(|p| !crate::graph::targets::graph_storage(&p)).unwrap_or(true))
        .map(|e| {
            let mut value = serde_json::to_value(&e).unwrap_or(Value::Null);
            if let Some((revision, unsaved)) = open.get(&e.path) {
                value["openRevision"] = json!(revision);
                value["unsavedChanges"] = json!(unsaved);
            }
            if let Some(id) = resources.get(&e.path) {
                value["resource"] = json!(id);
            }
            value
        })
        .collect();
    let mut result = json!({"entries": entries, "truncated": listing.truncated});
    if let Some(graph) = env.graph {
        let status = graph.service.status();
        result["graph"] = crate::graph::header(&status);
    }
    Ok(ToolOutput::ok(result))
}

/// Resource identities of the project's files, by path (bounded).
fn graph_resources(graph: GraphEnv<'_>) -> HashMap<String, exposition_graph::EntityId> {
    graph
        .service
        .with_store(|store| {
            let (found, _) = store.find_entities(Some(&[exposition_graph::EntityKind::Resource]), None, true, 5000)?;
            Ok(found
                .into_iter()
                .filter(|e| e.lifecycle != exposition_graph::Lifecycle::Tombstoned)
                .filter_map(|e| e.key.clone().map(|k| (k, e.id)))
                .collect())
        })
        .unwrap_or_default()
}

fn read_file(env: &ToolEnv<'_>, seen: &mut SeenTexts, input: ReadFileInput) -> Result<ToolOutput, ToolOutput> {
    let path = parse_path(&input.path)?;
    let id = workspace::open_document_in(env.workspace, env.documents, env.files, &path).map_err(file_error)?;
    let snapshot = env.documents.snapshot(&id).map_err(|e| ToolOutput::error(e.to_string()))?;
    let info = env.documents.info(&id).map_err(|e| ToolOutput::error(e.to_string()))?;
    if snapshot.text.len() > MAX_READ_BYTES {
        return Err(ToolOutput::error(format!(
            "{path} is {} bytes, more than the {MAX_READ_BYTES}-byte limit for sending a file to the model.",
            snapshot.text.len()
        )));
    }
    seen.insert(&id, snapshot.revision, Arc::clone(&snapshot.text));
    // The workspace snapshot reports the hash of the exact bytes a save or a
    // build would use (the buffer encoded in its encoding).
    let content_hash = env
        .workspace
        .and_then(|w| w.snapshot().ok())
        .and_then(|s| s.read(&path).ok())
        .filter(|input| input.revision == Some(snapshot.revision))
        .map(|input| input.content_hash)
        .unwrap_or_else(|| workspace::content_hash(&snapshot.text));
    let unsaved = info.saved_revision != Some(info.revision);
    let graph_info = resolve_target(env, &path, TargetUse::Read);
    let mut result = json!({
        "path": path.as_str(),
        "revision": snapshot.revision,
        "unsavedChanges": unsaved,
        "contentHash": content_hash,
        "text": &*snapshot.text,
    });
    if let Some(graph) = graph_info {
        result["graph"] = graph;
    }
    Ok(ToolOutput::ok(result))
}

/// Where an applied edit or insertion is (in every such result).
const EDITED_NOTE: &str = "Applied to the editor text, not saved to disk: the file on disk keeps its last saved \
    version until it is saved (save_document saves it, with the authority a file change needs; so can the user). \
    read_file, build_document, and check_lean use the editor text; programs you run and copy_paths read the saved \
    file. The user reviews the change (Keep or Revert).";
/// Where a created file is (in every such result).
const CREATED_NOTE: &str =
    "Written to disk (saved) and opened in the editor. The user reviews the creation (Keep or Revert).";
const CREATED_CHANGED_NOTE: &str = "Written to disk and opened in the editor; the editor text has changed since and \
    has unsaved changes. The user reviews the creation (Keep or Revert).";

/// Locate each `old_text` exactly once in `base` and build sorted patches.
fn resolve_edits(base: &str, input: &EditDocumentInput) -> Result<Vec<TextPatch>, ToolOutput> {
    let mut ranges: Vec<(usize, usize, &str, usize)> = Vec::new();
    for (i, edit) in input.edits.iter().enumerate() {
        let n = i + 1;
        if edit.old_text.is_empty() {
            if base.is_empty() && input.edits.len() == 1 {
                ranges.push((0, 0, &edit.new_text, n));
                continue;
            }
            return Err(ToolOutput::error(format!(
                "edit #{n}: old_text is empty; it may only be empty to fill an empty file. Include the neighbouring \
                 text you want to keep in both old_text and new_text."
            )));
        }
        let mut matches = base.match_indices(edit.old_text.as_str());
        let Some((start, _)) = matches.next() else {
            return Err(ToolOutput::error(format!(
                "edit #{n}: old_text was not found in revision {} of {}. Copy it exactly from read_file output \
                 (including whitespace and line breaks).",
                input.base_revision, input.path
            )));
        };
        let extra = matches.count();
        if extra > 0 {
            return Err(ToolOutput::error(format!(
                "edit #{n}: old_text occurs {} times; include more surrounding text so it is unique.",
                extra + 1
            )));
        }
        ranges.push((start, start + edit.old_text.len(), &edit.new_text, n));
    }
    ranges.sort_by_key(|r| (r.0, r.1));
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(ToolOutput::error(format!("edits #{} and #{} overlap; combine them.", pair[0].3, pair[1].3)));
        }
    }
    ranges
        .into_iter()
        .map(|(start, end, text, _)| {
            let range = byte_range_to_utf16(base, start..end).map_err(|e| ToolOutput::error(e.to_string()))?;
            Ok(TextPatch { range, text: text.to_owned() })
        })
        .collect()
}

fn edit_document(
    env: &ToolEnv<'_>,
    seen: &mut SeenTexts,
    tool_use_id: &str,
    input: EditDocumentInput,
) -> Result<ToolOutput, ToolOutput> {
    let path = parse_path(&input.path)?;
    let Some(id) = env.documents.find_by_path(&path) else {
        return Err(ToolOutput::error(format!(
            "{path} is not open. Call read_file first and edit the revision it returns."
        )));
    };
    let Some(base) = seen.get(&id, input.base_revision) else {
        let current = env.documents.info(&id).map(|i| i.revision).unwrap_or_default();
        return Err(ToolOutput::error_with(
            format!(
                "Revision {} of {path} is not a revision you have read. Call read_file and edit the revision it \
                 returns.",
                input.base_revision
            ),
            json!({"currentRevision": current}),
        ));
    };
    let patches = resolve_edits(&base, &input)?;
    if patches.iter().all(|p| {
        exposition_core::text::utf16_range_to_bytes(&base, p.range).map(|b| base[b] == p.text).unwrap_or(false)
    }) {
        return Err(ToolOutput::error("The edits do not change anything."));
    }
    let before_apply = env.documents.snapshot(&id).map_err(|e| ToolOutput::error(e.to_string()))?;
    let request = TransactionRequest {
        id: TransactionId(format!("agent-{}-{tool_use_id}", env.task_id.0)),
        origin: EditOrigin::Agent { task_id: env.task_id.0.clone() },
        edits: vec![DocumentEdit { document: id.clone(), base_revision: input.base_revision, patches }],
        undo: UndoBehavior::NewGroup,
        conflict: ConflictPolicy::RebaseDisjoint,
        label: Some(format!("Agent edit to {path}")),
        selection: None,
    };
    let outcome = match env.documents.apply(request) {
        Ok(outcome) => outcome,
        Err(e) => {
            let current = env.documents.info(&id).map(|i| i.revision).unwrap_or_default();
            let message = match e {
                TransactionError::Conflict { .. } | TransactionError::HistoryUnavailable { .. } => format!(
                    "Your edit overlaps changes made to {path} after revision {} (it is now at revision {current}), \
                     most likely by the user. Nothing was changed. Read the file again and redo the edit on the new \
                     text; do not overwrite the user's work.",
                    input.base_revision
                ),
                TransactionError::ReadOnly { reason, .. } => format!("{path} is read-only: {reason}"),
                other => format!("The edit was rejected: {other}"),
            };
            return Err(ToolOutput::error_with(message, json!({"currentRevision": current})));
        }
    };
    let applied = &outcome.applied[0];
    record_applied(env, seen, tool_use_id, &path, &id, &before_apply, applied)?;
    let graph_info = resolve_target(env, &path, TargetUse::Edit);
    let mut result = json!({
        "path": path.as_str(),
        "revision": applied.revision,
        "previousRevision": applied.previous_revision,
        "unsavedChanges": true,
        "note": EDITED_NOTE,
    });
    if let Some(graph) = graph_info {
        result["graph"] = graph;
    }
    if applied.rebased {
        result["rebased"] = json!(true);
        result["note"] = json!(format!(
            "Applied over concurrent edits made since your base revision (they did not overlap). Read the file \
             again before further edits. {EDITED_NOTE}"
        ));
    }
    Ok(ToolOutput::ok(result))
}

/// Record an applied agent change for review and remember the new text.
fn record_applied(
    env: &ToolEnv<'_>,
    seen: &mut SeenTexts,
    tool_use_id: &str,
    path: &WorkspacePath,
    id: &DocumentId,
    before_apply: &exposition_workspace::DocumentSnapshot,
    applied: &exposition_core::document::AppliedEdit,
) -> Result<(), ToolOutput> {
    let before = (before_apply.revision == applied.previous_revision).then(|| Arc::clone(&before_apply.text));
    let after: Option<Arc<str>> = match &before {
        Some(text) => crate::patch::apply(text, &applied.patches).ok().map(Arc::from),
        None => env.documents.snapshot(id).ok().filter(|s| s.revision == applied.revision).map(|s| s.text),
    };
    if let Some(after) = &after {
        seen.insert(id, applied.revision, Arc::clone(after));
    }
    review::record_edit(
        env.store,
        env.task_id,
        &EditRecord {
            tool_use_id,
            path,
            document: id,
            previous_revision: applied.previous_revision,
            revision: applied.revision,
            patches: &applied.patches,
            before: before.as_deref(),
            after: after.as_deref(),
        },
    )
    .map_err(|e| ToolOutput::error(format!("The change was applied but could not be recorded for review: {e}")))
}

/// Insert or update a research artifact with the format-owned attachment
/// transform (the same one the research panel uses), as an agent edit that is
/// recorded for review. An insertion the user approved applies exactly the
/// text that was shown, or nothing.
fn insert_artifact(
    env: &ToolEnv<'_>,
    seen: &mut SeenTexts,
    tool_use_id: &str,
    input: InsertArtifactInput,
    approved: Option<PlannedInsertion>,
) -> Result<ToolOutput, ToolOutput> {
    let path = parse_path(&input.path)?;
    let artifact = env
        .services
        .research_artifact(&input.run_id, &input.artifact_id)
        .map_err(|e| ToolOutput::error(e.to_string()))?;
    let artifact_path = artifact.path.clone();
    let id = workspace::open_document_in(env.workspace, env.documents, env.files, &path).map_err(file_error)?;
    let snapshot = env.documents.snapshot(&id).map_err(|e| ToolOutput::error(e.to_string()))?;
    let key = approved.as_ref().map(|a| a.key.clone()).filter(|_| input.update_key.is_empty());
    let plan = plan_insertion(env, &input, &path, artifact, &id, snapshot.revision, &snapshot.text, key)?;
    if let Some(approved) = &approved {
        let texts: Vec<&str> =
            plan.transaction.edits.iter().flat_map(|e| e.patches.iter().map(|p| p.text.as_str())).collect();
        if texts != approved.texts.iter().map(String::as_str).collect::<Vec<_>>() {
            return Err(refusal(
                codes::CHANGED_SINCE_APPROVAL,
                "The text to insert is no longer the text the user approved (the document or the artifact changed \
                 meanwhile). Nothing was inserted; call the tool again so the user can approve the current version.",
            ));
        }
    }
    let outcome = env.documents.apply(plan.transaction.clone()).map_err(|e| {
        ToolOutput::error(format!("The attachment was not applied: {e}. Read the file again and retry."))
    })?;
    let applied = outcome
        .applied
        .iter()
        .find(|edit| edit.document == id)
        .ok_or_else(|| ToolOutput::error("the document service applied no edit"))?;
    record_applied(env, seen, tool_use_id, &path, &id, &snapshot, applied)?;
    // The attachment in the project graph (artifact retained, usesAsset,
    // evidence, journal), recorded like a reviewed promotion.
    let attachment = record_attachment(
        env,
        &input,
        &id,
        &plan.transaction.id.0,
        &plan.attachment_key,
        applied.revision,
        artifact_path.as_ref(),
    );
    let mut graph_info = resolve_target(env, &path, TargetUse::Insert);
    if let (Some(graph), Some(attachment)) = (graph_info.as_mut(), attachment.as_ref())
        && let Some(obj) = graph.as_object_mut()
    {
        obj.insert("attachment".into(), attachment.clone());
    }
    let pending = attachment.as_ref().is_some_and(|a| a["state"] != json!("recorded"));
    let mut result = json!({
        "path": path.as_str(),
        "revision": applied.revision,
        "attachmentKey": plan.attachment_key,
        "warnings": plan.warnings,
        "unsavedChanges": true,
        "note": format!("Inserted with the format's figure/table markup. {EDITED_NOTE}"),
    });
    if let Some(graph) = graph_info {
        result["graph"] = graph;
    }
    if pending {
        result["graphNote"] = json!(
            "The figure is in the document, but the project graph has not recorded the attachment; it is not \
             complete. Tell the user it needs reconciliation."
        );
    }
    Ok(ToolOutput::ok(result))
}

/// Record an inserted attachment in the project graph through the research
/// project (when the task has a graph). The outcome is described as it is:
/// `recorded`, `pendingReconciliation` (the journal keeps a record that needs
/// reconciliation), or `notRecorded` (the service refused or is missing).
#[allow(clippy::too_many_arguments)]
fn record_attachment(
    env: &ToolEnv<'_>,
    input: &InsertArtifactInput,
    document: &DocumentId,
    transaction: &str,
    attachment_key: &str,
    revision: u64,
    artifact_path: Option<&WorkspacePath>,
) -> Option<Value> {
    use exposition_research::project::types::{AttachmentNotice, PromotionGraphState};
    // Only tasks with a project graph record attachments in it.
    env.graph?;
    let notice = AttachmentNotice {
        run_id: input.run_id.clone(),
        artifact_id: input.artifact_id.clone(),
        document: document.clone(),
        attachment_key: attachment_key.to_owned(),
        transaction: Some(transaction.to_owned()),
        revision,
        actor: format!("agent task {}", env.task_id),
    };
    Some(match env.services.record_attachment(&notice) {
        Ok(recorded) => match recorded.graph {
            PromotionGraphState::Recorded { graph_revision, manuscript, artifact, relation } => {
                if let Some(path) = artifact_path {
                    crate::graph::targets::record_target(
                        env.store,
                        env.task_id,
                        path,
                        Some(artifact.clone()),
                        TargetUse::Insert,
                        Some(format!("retained by attachment {attachment_key}")),
                        Some(recorded.operation.to_string()),
                    );
                }
                json!({
                    "state": "recorded",
                    "operation": recorded.operation,
                    "manuscript": manuscript,
                    "artifact": artifact,
                    "relation": relation,
                    "graphRevision": graph_revision,
                })
            }
            PromotionGraphState::PendingReconciliation { reason } => {
                json!({
                    "state": "pendingReconciliation",
                    "code": codes::GRAPH_PENDING,
                    "operation": recorded.operation,
                    "reason": reason,
                })
            }
        },
        Err(e) => {
            let (code, reason) = match &e {
                ServiceError::Outcome(value) => (
                    value.get("code").and_then(Value::as_str).unwrap_or("research.failed").to_owned(),
                    value.get("error").and_then(Value::as_str).unwrap_or_default().to_owned(),
                ),
                other => ("agents.serviceUnavailable".to_owned(), other.to_string()),
            };
            json!({"state": "notRecorded", "code": code, "reason": reason})
        }
    })
}

fn create_file(
    env: &ToolEnv<'_>,
    seen: &mut SeenTexts,
    tool_use_id: &str,
    input: CreateFileInput,
) -> Result<ToolOutput, ToolOutput> {
    let path = parse_path(&input.path)?;
    let exists = || ToolOutput::error(format!("{path} already exists; use read_file and edit_document to change it."));
    // A provisional identity (operation journal) before the file exists.
    let provisional =
        env.graph.map(|graph| (graph, crate::graph::targets::reserve(graph, env.task_id, &on_disk(env, &path))));
    let created = create_file_inner(env, &path, &input, exists);
    let opened = match created {
        Ok(opened) => opened,
        Err(e) => {
            if let Some((graph, provisional)) = provisional {
                crate::graph::targets::abandon(graph, provisional, "the file was not created");
            }
            return Err(e);
        }
    };
    let graph_info = provisional.map(|(graph, provisional)| {
        let hash = sha256_hex(input.content.as_bytes());
        let path = on_disk(env, &path);
        crate::graph::targets::finalize(graph, env.store, env.task_id, &path, provisional, &hash)
    });
    let document = opened.as_ref().and_then(|id| env.documents.snapshot(id).ok().map(|s| (id.clone(), s)));
    if let Some((id, snapshot)) = &document {
        seen.insert(id, snapshot.revision, Arc::clone(&snapshot.text));
    }
    review::record_create(
        env.store,
        env.task_id,
        tool_use_id,
        &path,
        &input.content,
        document.as_ref().map(|(id, s)| (id, s.revision)),
    )
    .map_err(|e| ToolOutput::error(format!("The file was created but could not be recorded for review: {e}")))?;
    let mut result = json!({"path": path.as_str(), "created": true, "bytes": input.content.len()});
    if let Some((_, snapshot)) = &document {
        result["revision"] = json!(snapshot.revision);
    }
    // The file is written to disk before it is opened: say so (an edit, by
    // contrast, changes only the editor text).
    let unsaved = document
        .as_ref()
        .and_then(|(id, _)| env.documents.info(id).ok())
        .is_some_and(|info| info.saved_revision != Some(info.revision));
    result["unsavedChanges"] = json!(unsaved);
    result["note"] = json!(if unsaved { CREATED_CHANGED_NOTE } else { CREATED_NOTE });
    if let Some(graph) = graph_info {
        result["graph"] = graph;
    }
    Ok(ToolOutput::ok(result))
}

/// Create the file (never overwriting) and open it as a document.
fn create_file_inner(
    env: &ToolEnv<'_>,
    path: &WorkspacePath,
    input: &CreateFileInput,
    exists: impl Fn() -> ToolOutput,
) -> Result<Option<DocumentId>, ToolOutput> {
    let path = path.clone();
    let opened = match env.workspace {
        // The workspace service creates the file (never overwriting) and opens it.
        Some(workspace) => {
            // In the open project, which must still be the task's.
            workspace::check_root(Some(workspace), env.files.root()).map_err(|reason| project_changed(&reason))?;
            match workspace.create_document(&path, &input.content) {
                Ok(opened) => Some(opened.state.info.id),
                Err(e) if e.code == "file.exists" => return Err(exists()),
                Err(e) => return Err(ToolOutput::error(e.message)),
            }
        }
        None => {
            if env.documents.find_by_path(&path).is_some() || env.files.exists(&path).map_err(file_error)? {
                return Err(exists());
            }
            env.files.create_text(&path, &input.content).map_err(file_error)?;
            workspace::open_document_in(None, env.documents, env.files, &path).ok()
        }
    };
    Ok(opened)
}

// ----- project operations: delete_paths, copy_paths (P1) ---------------------

/// Plan a deletion or copy with the application's project operations (the
/// orchestrator the interface uses, with this task as the actor). The gate
/// has already checked every named path against the task's scope and the
/// path policy. A plan with concrete consequences runs only after the user
/// reviewed them (one approval, never repeatable); Ask tasks that apply
/// changes directly refuse it with them (the gate decides).
fn prepare_project_op(call: &ToolCall, env: &ToolEnv<'_>) -> Result<PreparedOp, ToolOutput> {
    use exposition_core::ops::{OpActor, OpDestination, OpKind, OpRequest, OpTarget};
    let ops = env.services.project_operations().ok_or_else(|| {
        refusal(
            codes::SERVICE_UNAVAILABLE,
            ServiceError::NotAvailable(super::services::ServiceKind::ProjectOps).to_string(),
        )
    })?;
    let (kind, raws, destination) = match call {
        ToolCall::DeletePaths(input) => (OpKind::Delete { permanent: false }, &input.paths, None),
        ToolCall::CopyPaths(input) => {
            // Copies into the project root create entries outside any narrower scope.
            let folder = if tools::is_root(&input.destination) {
                if env.spec.scope != TaskScope::Workspace {
                    return Err(refusal(
                        codes::OUT_OF_SCOPE,
                        "The project root is outside this task's scope; copy into a folder in scope.",
                    ));
                }
                None
            } else {
                Some(parse_path(input.destination.trim())?)
            };
            (
                OpKind::Paste { destination: OpDestination::Folder { folder: folder.clone() } },
                &input.paths,
                Some(folder),
            )
        }
        _ => return Err(ToolOutput::error("not a project operation")),
    };
    let mut targets = Vec::new();
    for raw in raws {
        targets.push(OpTarget::Path { path: parse_path(raw.trim())? });
    }
    let request = OpRequest { kind, targets, actor: OpActor::Agent { task: env.task_id.0.clone() } };
    let plan = ops
        .plan(request)
        .map_err(|e| ToolOutput::error_with(e.message.clone(), json!({"code": e.code, "status": "refused"})))?;
    if plan.effects.is_empty() {
        let reasons: Vec<Value> = plan
            .refusals
            .iter()
            .map(|r| json!({"path": target_label(&r.target), "code": r.code, "reason": r.message}))
            .collect();
        return Err(ToolOutput::error_with(
            "Nothing was changed: every item was refused.",
            json!({"code": codes::OPERATION_REFUSED, "refusals": reasons}),
        ));
    }
    let n = plan.effects.len();
    let items = if n == 1 { "1 item".to_owned() } else { format!("{n} items") };
    let title = match &destination {
        None => format!("Move {items} to the Recycle Bin"),
        Some(None) => format!("Copy {items} into the project folder"),
        Some(Some(folder)) => format!("Copy {items} into {folder}"),
    };
    let mut detail = String::new();
    for effect in &plan.effects {
        detail.push_str(&format!("- {}\n", shown(&effect.description)));
    }
    if !plan.refusals.is_empty() {
        detail.push_str("\nNot changed:\n");
        for r in &plan.refusals {
            detail.push_str(&format!("- {}: {}\n", shown(&target_label(&r.target)), shown(&r.message)));
        }
    }
    let decisions: Vec<&exposition_core::ops::OpImpact> =
        plan.impacts.iter().filter(|i| i.kind.needs_decision()).collect();
    if !decisions.is_empty() {
        detail.push_str("\nConsequences:\n");
        for impact in decisions {
            detail.push_str(&format!("- {}\n", shown(&impact.message)));
            for consumer in &impact.consumers {
                detail.push_str(&format!("    {} {}\n", consumer.kind, shown(&consumer.label)));
            }
        }
    }
    for notice in plan.impacts.iter().filter(|i| !i.kind.needs_decision()) {
        detail.push_str(&format!("\nNote: {}\n", shown(&notice.message)));
    }
    for choice in &plan.choices {
        let chosen = choice.options.iter().find(|o| o.value == choice.default).map(|o| o.label.as_str()).unwrap_or("");
        detail.push_str(&format!("\n{}: {}\n", choice.label, chosen));
    }
    if destination.is_none() {
        detail.push_str("\nItems go to the Recycle Bin, never deleted permanently.");
    }
    Ok(PreparedOp { plan, title, detail: detail.trim_end().to_owned() })
}

fn target_label(target: &exposition_core::ops::OpTarget) -> String {
    match target {
        exposition_core::ops::OpTarget::Path { path } => path.to_string(),
        exposition_core::ops::OpTarget::External { path } => path.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Apply a planned deletion or copy (the orchestrator re-validates the plan)
/// and record what actually happened for review: deleted entries with their
/// restore token, copies with their content digest.
fn apply_project_op(env: &ToolEnv<'_>, tool_use_id: &str, prepared: &PreparedOp) -> Result<ToolOutput, ToolOutput> {
    use exposition_core::ops::{OpKind, OpStatus, OpTarget};
    let ops = env.services.project_operations().ok_or_else(|| {
        refusal(
            codes::SERVICE_UNAVAILABLE,
            ServiceError::NotAvailable(super::services::ServiceKind::ProjectOps).to_string(),
        )
    })?;
    let result = ops
        .apply(&prepared.plan)
        .map_err(|e| ToolOutput::error_with(e.message.clone(), json!({"code": e.code, "status": "notChanged"})))?;
    let deleting = matches!(prepared.plan.request.kind, OpKind::Delete { .. });
    let mut outcomes = Vec::new();
    let mut any = false;
    for o in &result.outcomes {
        let ok = o.status == OpStatus::Succeeded;
        any |= ok;
        if ok {
            let recorded = match (&o.target, &o.created, deleting) {
                (OpTarget::Path { path }, None, true) => {
                    review::record_delete(env.store, env.task_id, tool_use_id, path, o.restore.clone())
                }
                (_, Some(OpTarget::Path { path }), false) => {
                    let digest = review::entry_digest(env.files, path);
                    review::record_copy(env.store, env.task_id, tool_use_id, path, digest)
                }
                _ => Ok(()),
            };
            if let Err(e) = recorded {
                return Err(ToolOutput::error(format!(
                    "The operation was applied but could not be recorded for review: {e}"
                )));
            }
        }
        let mut item = json!({
            "path": target_label(&o.target),
            "status": o.status,
            "code": o.code,
            "message": o.message,
        });
        if let Some(created) = &o.created {
            item["created"] = json!(target_label(created));
        }
        outcomes.push(item);
    }
    let content = json!({"outcomes": outcomes, "notices": result.notices});
    if any { Ok(ToolOutput::ok(content)) } else { Err(ToolOutput::error_with("Nothing was changed.", content)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::EditInput;

    fn input(edits: &[(&str, &str)]) -> EditDocumentInput {
        EditDocumentInput {
            path: "main.typ".into(),
            base_revision: 1,
            edits: edits.iter().map(|(o, n)| EditInput { old_text: (*o).into(), new_text: (*n).into() }).collect(),
        }
    }

    #[test]
    fn resolves_unique_snippets_in_utf16() {
        let base = "α = 1\nβ = 2\n";
        let patches = resolve_edits(base, &input(&[("β = 2", "β = 3"), ("α", "a")])).unwrap();
        assert_eq!(patches, vec![TextPatch::replace(0, 1, "a"), TextPatch::replace(6, 11, "β = 3")]);
        assert!(resolve_edits("x x", &input(&[("x", "y")])).unwrap_err().text().contains("occurs 2 times"));
        assert!(resolve_edits("abc", &input(&[("zzz", "y")])).unwrap_err().text().contains("not found"));
        assert!(
            resolve_edits("abcdef", &input(&[("abcd", "1"), ("cdef", "2")])).unwrap_err().text().contains("overlap")
        );
        assert_eq!(resolve_edits("", &input(&[("", "hello")])).unwrap(), vec![TextPatch::insert(0, "hello")]);
        assert!(resolve_edits("abc", &input(&[("", "hello")])).is_err());
    }

    #[test]
    fn approval_payloads_show_every_character_unambiguously() {
        assert_eq!(shown("a\tb\r\nc\n"), "a\tb\r\nc\n", "tabs, CRLF, and newlines stay as they are");
        assert_eq!(shown("x\u{202E}y\u{200B}z\u{0007}"), "x⟨U+202E⟩y⟨U+200B⟩z⟨U+0007⟩");
        assert_eq!(shown("lone\rreturn"), "lone⟨U+000D⟩return");
        // A literal escape-looking text cannot pass for an escape.
        assert_eq!(shown("⟨U+0041⟩"), "⟨U+27E8⟩U+0041⟩");
        let long = "y = 2\n".repeat(10_000);
        assert_eq!(shown(&long), long, "nothing is truncated");
    }
}
