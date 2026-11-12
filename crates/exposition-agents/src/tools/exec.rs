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
//! open buffers over disk. Nothing here bypasses an open buffer.

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

use super::services::{
    BuildRequest, CommandRequest, LatexEngine, LeanRequest, PythonRequest, ServiceError, ToolServices,
};
use super::{
    CreateFileInput, EditDocumentInput, Exposure, InsertArtifactInput, Launch, ListFilesInput, PathUse, ReadFileInput,
    RunCommandInput, ToolCall, scope,
};
use crate::graph::propose::Prepared;
use crate::graph::{GraphEnv, TargetUse};
use crate::model::{ContextConsent, ExecutionMode, PermissionProfile, TaskId, TaskScope, TaskSpec};
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

fn read_only() -> ToolOutput {
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
        PathUse::Folder => scope::check_dir(root, scope, parse_dir(raw)?.as_ref()),
        PathUse::Read | PathUse::Write | PathUse::Create => scope::check(root, scope, &parse_path(raw)?).map(|_| ()),
    };
    checked.map_err(|e| refusal(e.code(), e.message(scope)))
}

/// Decide whether `call` may run, and whether it needs the user's approval.
///
/// The rules apply to the call's declared [`Effects`], in this order:
/// 1. the task's folder is the open project (calls act only on their own project);
/// 2. a change is refused for a read-only task;
/// 3. every named path is valid and in scope, as requested and as resolved;
/// 4. what the result can carry to the model: content needs the user's content
///    consent; content beyond the scope (processes, TeX, Lean) needs a task
///    scoped to the whole project as well;
/// 5. processes never start with execution disabled or read-only, and
///    native-only tools never start in a restricted-only task;
/// 6. the service exists;
/// 7. tool-specific preparation (an inserted artifact's own scope check and
///    the exact text shown for approval);
/// 8. restricted launches need the restricted backend (never downgraded);
///    native launches need a trusted folder and the user's approval of the
///    complete payload; changes in a review-edits task need approval too.
pub fn gate(call: &ToolCall, env: &ToolEnv<'_>) -> Gate {
    match decide(call, env) {
        Ok(gate) => gate,
        Err(refusal) => Gate::Refused(refusal),
    }
}

fn decide(call: &ToolCall, env: &ToolEnv<'_>) -> Result<Gate, ToolOutput> {
    let spec = env.spec;
    let root = env.files.root();
    workspace::check_root(env.workspace, root).map_err(ToolOutput::error)?;
    let effects = call.effects();
    if effects.changes && spec.permission == PermissionProfile::ReadOnly {
        return Err(read_only());
    }
    for (raw, path_use) in &effects.paths {
        check_path(env, raw, *path_use)?;
        if *path_use != PathUse::Folder || !tools::is_root(raw) {
            crate::graph::targets::check_path_policy(&parse_path(raw.trim())?, *path_use)?;
        }
    }
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
        None if matches!(call, ToolCall::GraphQuery(_) | ToolCall::GraphPropose(_)) => {
            return Err(crate::graph::unavailable("no project graph is attached to this task"));
        }
        None => {}
    }
    let insertion = match call {
        ToolCall::InsertArtifact(input) => prepare_insertion(env, input)?,
        _ => None,
    };
    let proposal = match (call, env.graph) {
        (ToolCall::GraphPropose(input), Some(graph)) => {
            Some(crate::graph::propose::prepare(graph, root, spec, env.task_id, input)?)
        }
        _ => None,
    };
    let policy = match (effects.launch, spec.execution) {
        (Launch::None, _) => None,
        (_, ExecutionMode::Disabled) => return Err(execution_disabled()),
        (_, ExecutionMode::Restricted) => match env.services.restricted_backend() {
            Some(backend) => Some(ExecutionPolicy::Restricted { backend }),
            None => {
                return Err(refusal(
                    codes::RESTRICTED_UNAVAILABLE,
                    "This task only allows unattended execution inside the restricted (container) backend, which is \
                     not available. Exposition never falls back to running it natively; the user can choose \
                     per-command approval instead.",
                ));
            }
        },
        (_, ExecutionMode::ApproveEach) => {
            if let Some(reason) = env.services.native_refusal(root) {
                return Err(ToolOutput::error_with(
                    reason.clone(),
                    json!({"code": codes::NATIVE_REFUSED, "status": "refused", "reason": reason}),
                ));
            }
            Some(ExecutionPolicy::TrustedNative)
        }
    };
    let native = policy == Some(ExecutionPolicy::TrustedNative);
    let review = effects.changes && spec.permission == PermissionProfile::ReviewEdits;
    let mut authorization =
        Authorization { policy, root: root.to_path_buf(), approved: None, insertion: None, proposal: None };
    if !(native || review) {
        authorization.insertion = insertion.map(|(planned, _)| planned);
        authorization.proposal = proposal.map(Box::new);
        return Ok(Gate::Allowed(authorization));
    }
    let (title, mut detail) = match &proposal {
        Some(prepared) => (prepared.title.clone(), prepared.detail.clone()),
        None => describe(call, insertion.as_ref().map(|(_, shown)| shown.as_str())),
    };
    authorization.proposal = proposal.map(Box::new);
    if native {
        detail.push_str("\n\nRuns natively with your account's privileges.");
    }
    authorization.insertion = insertion.map(|(planned, _)| planned);
    authorization.approved = Some(detail.clone());
    Ok(Gate::NeedsApproval { title, detail, authorization })
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

/// After the user approved: the call runs only as approved, in the task's own
/// project. The gate is evaluated again; it must still ask for exactly the
/// approval the user gave (an insertion re-planned on a changed document, a
/// revoked trust, or a switched project refuses the call).
pub fn confirm(call: &ToolCall, env: &ToolEnv<'_>, approved: Authorization) -> Result<Authorization, ToolOutput> {
    if let Err(reason) = workspace::check_root(env.workspace, &approved.root) {
        return Err(project_changed(&reason));
    }
    if env.files.root() != approved.root {
        return Err(project_changed("the task's folder is not the folder the action was approved for."));
    }
    match gate(call, env) {
        Gate::Refused(output) => Err(output),
        Gate::NeedsApproval { authorization, .. } if authorization.approved == approved.approved => Ok(authorization),
        Gate::Allowed(_) | Gate::NeedsApproval { .. } => Err(refusal(
            codes::CHANGED_SINCE_APPROVAL,
            "What this action would do changed while it waited for approval (for example, the document or the \
             artifact changed). Nothing was done; call the tool again so the user can approve the current version.",
        )),
    }
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
        ToolCall::ListFiles(_) | ToolCall::ReadFile(_) | ToolCall::GraphQuery(_) | ToolCall::GraphPropose(_) => {
            (String::new(), String::new())
        }
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
/// becomes readable in the document); for approval, plan the exact text now.
fn prepare_insertion(
    env: &ToolEnv<'_>,
    input: &InsertArtifactInput,
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
    if env.spec.permission != PermissionProfile::ReviewEdits {
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
    let Authorization { policy, insertion, proposal, .. } = authorization;
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
        ToolCall::ListFiles(input) => list_files(env, input),
        ToolCall::ReadFile(input) => read_file(env, seen, input),
        ToolCall::EditDocument(input) => edit_document(env, seen, tool_use_id, input),
        ToolCall::CreateFile(input) => create_file(env, seen, tool_use_id, input),
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
            service_result(
                env.services
                    .run_python(
                        PythonRequest {
                            code: input.code,
                            task_id: env.task_id.0.clone(),
                            root: env.files.root().to_path_buf(),
                            working_directory: env.files.root().to_path_buf(),
                            policy,
                            session: format!("agent-{}", env.task_id.0),
                        },
                        cancel,
                    )
                    .await,
            )
        }
        ToolCall::CheckLean(input) => {
            let Some(policy) = policy else { return ToolOutput::error("missing execution policy") };
            match parse_path(&input.path) {
                Ok(path) => {
                    let graph_info = resolve_target(env, &path, TargetUse::Check);
                    with_graph(
                        service_result(env.services.check_lean(LeanRequest { path, policy }, cancel).await),
                        graph_info,
                    )
                }
                Err(e) => Err(e),
            }
        }
        ToolCall::RunCommand(input) => run_command(env, input, policy, cancel).await,
    };
    result.unwrap_or_else(|e| e)
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

/// Resolve a target to its graph identity (when the task has a graph) and
/// record it with the task; the description goes into the tool result.
fn resolve_target(env: &ToolEnv<'_>, path: &WorkspacePath, target_use: TargetUse) -> Option<Value> {
    let graph = env.graph?;
    let hash = saved_hash(env, path);
    Some(crate::graph::targets::resolve(graph, env.store, env.task_id, path, target_use, hash.as_deref()))
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
        .filter(|e| WorkspacePath::parse(&e.path).map(|p| !exposition_graph::is_graph_storage(&p)).unwrap_or(true))
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
        "note": "Applied to the open document (not saved). The user reviews agent changes.",
    });
    if let Some(graph) = graph_info {
        result["graph"] = graph;
    }
    if applied.rebased {
        result["rebased"] = json!(true);
        result["note"] = json!(
            "Applied over concurrent edits made since your base revision (they did not overlap). Read the file \
             again before further edits."
        );
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
    let graph_info = resolve_target(env, &path, TargetUse::Insert);
    let mut result = json!({
        "path": path.as_str(),
        "revision": applied.revision,
        "attachmentKey": plan.attachment_key,
        "warnings": plan.warnings,
        "note": "Inserted with the format's figure/table markup into the open document (not saved); reviewable like any agent edit.",
    });
    if let Some(graph) = graph_info {
        result["graph"] = graph;
    }
    Ok(ToolOutput::ok(result))
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
    let provisional = env.graph.map(|graph| (graph, crate::graph::targets::reserve(graph, env.task_id, &path)));
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
