//! The project graph as a mandatory part of every project agent task.
//!
//! The graph is not an optional tool the agent may ignore. The shared agent
//! service enforces it, not prompt text:
//!
//! 1. Before a task run starts (create and continue), a scoped graph context
//!    is attached ([`context::build`]): project identity and graph revision,
//!    the entities in the task's scope with their source versions, coverage,
//!    evidence freshness, the typed relations among them, and the operations
//!    the task is authorized to perform. It is filtered by scope and content
//!    consent exactly like file content, bounded in size, and recorded with
//!    the task (its project identity binds the task to that graph).
//! 2. Every tool call needs a usable graph bound to the task's own project
//!    ([`bound_status`]); paths a call acts on resolve to graph resource
//!    identities, and a target whose graph state is pending reconciliation or
//!    ambiguous pauses the call with a stable code ([`targets`]). Nothing falls
//!    back to filesystem-only operation.
//! 3. `graph_query` ([`query`]) reads bounded, consent-filtered slices and
//!    `graph_propose` ([`propose`]) turns typed operations into a validated
//!    proposal that is applied through the central gate (approval in
//!    review-edits tasks; agent relations are always suggestions).
//!
//! Graph content (labels, notes, attributes) is data from the project: it is
//! never read as instructions or as a permission grant. Only the task's
//! fixed settings decide what a call may do.

pub mod context;
pub mod propose;
pub mod query;
pub mod targets;

use std::path::Path;

use exposition_core::path::WorkspacePath;
use exposition_graph::{
    AssertionStatus, BindingState, CoverageState, CoverageSummary, CreatedEntity, EntityId, EntityKind, EvidenceKind,
    GraphDiagnostic, GraphService, GraphState, GraphStatus, Lifecycle, Origin, ProjectId, ProposalId, RelationId,
    RelationKind, ResourceRole, SourceVersion, SyncState,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::model::ContextConsent;
use crate::tools::exec::{ToolOutput, codes};

/// The project graph as one task run sees it.
#[derive(Clone, Copy)]
pub struct GraphEnv<'a> {
    pub service: &'a GraphService,
    /// Identity of the project graph the task is bound to (recorded with its
    /// first graph context).
    pub project: Option<&'a ProjectId>,
}

/// Who asserted or admitted a record, without identifiers of other tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OriginKind {
    Extracted,
    User,
    Agent,
    Preset,
    Service,
}

impl OriginKind {
    pub fn of(origin: &Origin) -> Self {
        match origin {
            Origin::Extracted { .. } => OriginKind::Extracted,
            Origin::User => OriginKind::User,
            Origin::Agent { .. } => OriginKind::Agent,
            Origin::Preset { .. } => OriginKind::Preset,
            Origin::Service { .. } => OriginKind::Service,
        }
    }
}

/// Whether evidence still applies to the current sources (reasons omitted:
/// they can name files outside the task's scope).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceState {
    Current,
    Stale,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ContextEvidence {
    pub kind: EvidenceKind,
    pub freshness: EvidenceState,
}

/// An entity as a task may see it. Under metadata-only consent the label of
/// anything but a file, folder, or the project is withheld (it is derived from
/// file content or written by people), as are notes and attributes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntity {
    pub id: EntityId,
    pub kind: EntityKind,
    pub roles: Vec<ResourceRole>,
    pub label: Option<String>,
    /// The file it is bound to (resources: the file itself).
    pub path: Option<WorkspacePath>,
    pub lifecycle: Lifecycle,
    /// Entity revision (needed to update it).
    pub revision: u64,
    pub origin: OriginKind,
    /// Source version the graph last recorded for its binding.
    pub version: Option<SourceVersion>,
    pub binding: Option<BindingState>,
    /// Extraction coverage of the file (resources).
    pub coverage: Option<CoverageState>,
    pub evidence: Vec<ContextEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ContextRelation {
    pub id: RelationId,
    pub kind: RelationKind,
    pub from: EntityId,
    pub to: EntityId,
    pub status: AssertionStatus,
    pub origin: OriginKind,
    /// Withheld under metadata-only consent.
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct KindCount {
    pub kind: EntityKind,
    pub count: u32,
}

/// One operation the task may (or may not) perform, as its settings decide.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ContextOperation {
    pub tool: String,
    pub allowed: bool,
    pub detail: String,
}

/// The scoped graph context attached to a task run (exactly what the agent
/// was given, also shown to the user on the task).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphContext {
    pub project: ProjectId,
    /// Project graph revision when the context was taken.
    pub revision: u64,
    /// A temporary graph for a folder that cannot hold one (nothing persists).
    pub temporary: bool,
    pub freshness: SyncState,
    /// Files in scope waiting for reconciliation.
    pub pending: u32,
    /// The task's scope in words.
    pub scope: String,
    pub consent: ContextConsent,
    /// Extraction coverage of the files in scope.
    pub coverage: CoverageSummary,
    /// Active entities in scope by kind.
    pub counts: Vec<KindCount>,
    pub entities: Vec<ContextEntity>,
    /// Typed relations whose both endpoints are listed above.
    pub relations: Vec<ContextRelation>,
    /// More entities or relations are in scope than were listed (use graph_query).
    pub truncated: bool,
    pub operations: Vec<ContextOperation>,
    pub created_at_ms: u64,
}

/// How a task used a project file it resolved through the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum TargetUse {
    Read,
    Edit,
    Create,
    Build,
    Check,
    Insert,
}

/// A project file a task acted on, with the graph identity it resolved to.
/// An ordinary task is an operational record linked to its targets; it is not
/// itself a research entity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TaskTarget {
    pub path: WorkspacePath,
    /// Resource identity (none for files the inclusion policy keeps out of
    /// the graph: logs, run records, generated outputs).
    pub entity: Option<EntityId>,
    pub uses: Vec<TargetUse>,
    /// Why the file has no graph identity, or what is pending.
    pub note: Option<String>,
    /// Operation journal record of a file creation (provisional identity).
    pub operation: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ProposalState {
    /// Shown to the user; nothing applied yet.
    AwaitingApproval,
    Applied,
    /// The user declined it.
    Declined,
    /// The graph changed in a way that conflicts; nothing applied.
    Conflict,
    /// Invalid against the current graph when applied; nothing applied.
    Invalid,
    /// Not applied for another reason (project changed, graph unavailable).
    Failed,
}

/// A graph change an agent proposed in a task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TaskProposal {
    pub tool_use_id: String,
    pub proposal: ProposalId,
    pub state: ProposalState,
    pub summary: Option<String>,
    pub base_revision: u64,
    /// One line per effect, as the user was shown.
    pub effects: Vec<String>,
    /// The complete typed operations.
    #[ts(type = "unknown")]
    pub operations: serde_json::Value,
    pub diagnostics: Vec<GraphDiagnostic>,
    /// Graph revision after it was applied.
    pub revision: Option<u64>,
    pub created: Vec<CreatedEntity>,
    pub message: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// A task's graph record, for the task detail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraphInfo {
    /// Project graph the task is bound to.
    pub project: Option<ProjectId>,
    /// The latest graph context attached to a run.
    pub context: Option<GraphContext>,
    /// Number of contexts attached (one per run).
    pub contexts: u32,
    pub targets: Vec<TaskTarget>,
    pub proposals: Vec<TaskProposal>,
    /// The investigation entity the user kept for this task, if any.
    pub investigation: Option<EntityId>,
}

/// A refusal with its stable code.
pub(crate) fn refusal(code: &str, message: impl Into<String>) -> ToolOutput {
    ToolOutput::error_with(message, json!({"code": code}))
}

/// The task's folder and the graph's folder are the same folder.
fn same_folder(a: &Path, b: &Path) -> bool {
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

pub(crate) fn unavailable(reason: &str) -> ToolOutput {
    refusal(
        codes::GRAPH_UNAVAILABLE,
        format!(
            "Not executed: the project graph is unavailable ({reason}). Project-agent actions are paused until it is \
             available again; nothing was read or changed. Tell the user; do not work around it."
        ),
    )
}

/// The graph status, when it can be used by this task now: a usable graph
/// (attached, or a temporary graph for a read-only folder) of the task's own
/// folder, with the identity recorded with the task.
pub fn bound_status(graph: GraphEnv<'_>, root: &Path) -> Result<GraphStatus, ToolOutput> {
    let status = graph.service.status();
    match &status.state {
        GraphState::Attached | GraphState::TemporaryReadOnly { .. } => {}
        GraphState::Unavailable { reason, .. } => return Err(unavailable(reason)),
        GraphState::Detached => return Err(unavailable("no project graph is attached")),
    }
    if status.freshness.state == SyncState::Unavailable {
        return Err(unavailable("its synchronization with the project's files is unavailable"));
    }
    match graph.service.root() {
        Some(graph_root) if same_folder(&graph_root, root) => {}
        _ => {
            return Err(refusal(
                codes::PROJECT_CHANGED,
                "The project changed: the open project graph belongs to another folder. This task acts only on its \
                 own project, so nothing was done.",
            ));
        }
    }
    if let (Some(recorded), Some(current)) = (graph.project, status.project.as_ref())
        && recorded != current
    {
        return Err(refusal(
            codes::PROJECT_CHANGED,
            "The project changed: this folder's project graph is not the one this task was started with (it was \
             recovered or replaced). This task acts only on its own project graph, so nothing was done; start a new \
             task.",
        ));
    }
    Ok(status)
}

/// The JSON header of every graph result: identity, revision, freshness.
pub(crate) fn header(status: &GraphStatus) -> serde_json::Value {
    json!({
        "project": status.project,
        "revision": status.revision,
        "freshness": status.freshness.state,
    })
}

/// Escape characters that could close or open a tagged block in the prompt,
/// keeping valid JSON (`<` and `>` only occur inside JSON strings).
pub(crate) fn prompt_safe_json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".into()).replace('<', "\\u003c").replace('>', "\\u003e")
}

pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}
