//! Project operations (P1): delete, duplicate, paste (copy), and restore across
//! files, workflows, runs, agent tasks, research records and the project graph.
//!
//! One contract for every surface (explorers, graph, workflows, Settings →
//! Storage & History, agents):
//! 1. `project_op_plan(OpRequest)` normalizes the targets, refuses invalid ones
//!    (outside the project, protected app infrastructure, read-only, a folder
//!    into itself, …), and lists the CONCRETE consequences ([`OpImpact`]) —
//!    never graph membership alone. `needs_decision` is true only when an
//!    impact requires the user's decision; the UI then shows ONE combined
//!    decision with the impacts, otherwise it applies at once.
//! 2. `project_op_apply(OpApply)` re-validates against the plan hash (a target
//!    or impact that changed materially → `ops.planStale`, nothing applied),
//!    performs each target independently, and returns every outcome (partial
//!    success is normal and reported, never hidden). Graph and history state
//!    follow the actual filesystem outcomes.
//! 3. `project_op_restore(OpRestore)` restores an app-known deletion without
//!    overwriting anything at the original location (files and run outputs:
//!    a collision gets a fresh name unless the user chose `cancel`; data
//!    records whose identity is taken are refused rather than renamed).
//!
//! IDs are strings here (the owning services validate their prefixes: `wf-`,
//! `wrun-`, `ent-`, `rel-`, `view-`, agent task ids, research run ids).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::path::WorkspacePath;

/// What an operation acts on.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum OpTarget {
    /// A file or folder of the open project (workspace-relative).
    Path { path: WorkspacePath },
    /// A file or folder outside the project, only as a paste SOURCE (from the
    /// OS clipboard or a file dialog). Never a delete/duplicate target; it
    /// grants no access beyond reading what the user explicitly pasted.
    External { path: String },
    /// A saved workflow (every revision).
    Workflow { workflow: String },
    /// One revision of a saved workflow.
    WorkflowRevision { workflow: String, revision: u64 },
    /// A workflow run record (and, only when `outputs` is chosen, its outputs).
    WorkflowRun { run: String },
    /// A research run record.
    ResearchRun { run: String },
    /// The generated outputs of a workflow run (`wrun-…`) or research run —
    /// a scope separate from the run record (deleting a record keeps its
    /// outputs unless this target is selected too). Never source inputs or
    /// manuscript consumers; files the project keeps as resources stay.
    RunOutputs { run: String },
    /// An agent task record.
    AgentTask { task: String },
    /// A graph entity. File-backed entities are refused here (`ops.useSourceFile`):
    /// their deletion is the deletion of the source file, a `Path` target.
    GraphEntity { entity: String },
    /// A graph relationship.
    GraphRelation { relation: String },
    /// A saved graph view (presentation only).
    GraphView { view: String },
    /// An app-managed recoverable deletion (restore/purge in Storage & History).
    Recoverable { id: String },
}

/// Where pasted copies go.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum OpDestination {
    /// A project folder (`None` = the project root). For a selected file the
    /// UI passes its parent folder and shows that it did.
    Folder { folder: Option<WorkspacePath> },
    /// Same kind of container as the source (workflow/graph copies stay in the
    /// open project; a pasted workflow-canvas subgraph goes into a draft).
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum OpKind {
    /// Delete. `permanent: false` uses the recoverable path: the OS Trash /
    /// Recycle Bin for files (never silently permanent), app-managed recovery
    /// for app records. `permanent: true` is the explicit "Delete Permanently".
    Delete { permanent: bool },
    /// Copy into the same parent / container with a fresh, collision-free name
    /// and a fresh identity.
    Duplicate,
    /// Copy the targets into `destination` (files: saved bytes only).
    Paste { destination: OpDestination },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpRequest {
    pub kind: OpKind,
    pub targets: Vec<OpTarget>,
    /// Who asks: the user (any UI route) or an agent task (existing grants and
    /// scope apply; nothing here widens them).
    pub actor: OpActor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum OpActor {
    User,
    Agent { task: String },
}

/// A concrete project consequence that requires (or informs) a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ImpactKind {
    /// A manuscript uses it (an attached artifact, an included/input file, a
    /// bibliography, an image) — the manuscript will show it missing.
    ManuscriptDependency,
    /// A saved workflow's required input — the workflow cannot run until rebound.
    WorkflowInput,
    /// Retained evidence (computational or formal) records it — the evidence
    /// becomes unavailable for reproduction; its historical verdict is kept.
    RetainedEvidence,
    /// A revision pinned by a surviving composite workflow.
    PinnedRevision,
    /// An open buffer with unsaved changes would lose its file.
    UnsavedBuffer,
    /// A running workflow run, research run, or agent task of this project
    /// uses it. (Document builds are not detected as active work — a known
    /// limitation: a build of a deleted source fails and reports it.)
    ActiveWork,
    /// An enabled trigger watches it (would fire or suspend).
    EnabledTrigger,
    /// The operation destroys the last recoverable copy (permanent deletion of
    /// any project content other than links and disposable caches, or purging
    /// app-managed recovery).
    RecoveryLoss,
    /// Informational, no decision: a source with unsaved edits was copied as
    /// its SAVED version.
    SavedVersionCopied,
}

impl ImpactKind {
    /// Whether this kind needs the user's decision before applying.
    pub fn needs_decision(self) -> bool {
        !matches!(self, ImpactKind::SavedVersionCopied)
    }
}

/// Something affected by an impact (shown by name in the decision).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpConsumer {
    /// `document`, `workflow`, `workflowRevision`, `evidence`, `run`, `task`,
    /// `trigger`, `buffer`, `entity`.
    pub kind: String,
    pub id: String,
    pub label: String,
    pub path: Option<WorkspacePath>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpImpact {
    pub kind: ImpactKind,
    /// The selected target this impact belongs to.
    pub target: OpTarget,
    pub consumers: Vec<OpConsumer>,
    /// Plain-language consequence naming the affected resources.
    pub message: String,
}

/// A choice the decision offers (only those an impact requires).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpChoice {
    /// Stable key, e.g. `unsavedBuffers` (`preserve` | `discard` | `cancel`),
    /// `activeWork` (`stop`), `triggers` (`disable`).
    pub key: String,
    pub label: String,
    pub options: Vec<OpChoiceOption>,
    /// Option used when the user does not pick: the one that loses nothing
    /// irreversibly (e.g. preserve a buffer, keep pinned revisions, skip active
    /// work); `triggers` defaults to `disable`, which proceeds and turns the
    /// trigger off (restorable by re-enabling).
    pub default: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpChoiceOption {
    pub value: String,
    pub label: String,
}

/// How the deleted content can come back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OpRecovery {
    /// Moved to the OS Trash / Recycle Bin (restorable from there; the app
    /// may offer Undo when it can match the item reliably).
    OsTrash,
    /// App-managed recoverable deletion (Storage & History lists it).
    AppManaged,
    /// Not recoverable (permanent deletion, or nothing to recover).
    None,
}

/// A target the plan will not touch, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpRefusal {
    pub target: OpTarget,
    /// Stable code, e.g. `ops.outsideProject`, `ops.protected`, `ops.readOnly`,
    /// `ops.intoItself`, `ops.linkNotCopied`,
    /// `ops.notFound`, `ops.notSupported`, `ops.agentScope`.
    pub code: String,
    pub message: String,
}

/// What will happen to one target if applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpEffect {
    pub target: OpTarget,
    /// Plain description, e.g. "Move to the Recycle Bin", "Copy to
    /// `figures/plot (2).png`", "Delete the record; outputs are kept".
    pub description: String,
    /// For copies: the planned destination (final name may still change if
    /// the name was taken meanwhile; the outcome reports the actual one).
    pub destination: Option<OpTarget>,
    pub recovery: OpRecovery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpPlan {
    pub plan: String,
    pub request: OpRequest,
    pub effects: Vec<OpEffect>,
    pub refusals: Vec<OpRefusal>,
    pub impacts: Vec<OpImpact>,
    /// True when any impact needs a decision (one combined decision).
    pub needs_decision: bool,
    pub choices: Vec<OpChoice>,
    /// Hash of everything the decision rests on; apply re-checks it.
    pub plan_hash: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpApply {
    pub plan: String,
    pub plan_hash: String,
    /// Values for the plan's choices (missing → each choice's default).
    pub choices: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OpStatus {
    Succeeded,
    Failed,
    /// Not attempted (refused at plan time, or the user cancelled a part).
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpOutcome {
    pub target: OpTarget,
    pub status: OpStatus,
    pub code: Option<String>,
    pub message: Option<String>,
    /// For copies: what was created.
    pub created: Option<OpTarget>,
    pub recovery: OpRecovery,
    /// Token for `project_op_restore` when the app can restore this outcome.
    pub restore: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OpResult {
    pub plan: String,
    pub outcomes: Vec<OpOutcome>,
    /// Notes for the user (e.g. "the saved version of X was copied", graph
    /// reconciliation pending after an interrupted update).
    pub notices: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpRestore {
    pub restore: String,
    /// When the original location is occupied: `rename` (default: a fresh
    /// collision-free name, both versions kept) or `cancel`. Never overwrite.
    pub on_collision: Option<String>,
}

/// Stable error of the operation commands (whole-request failures; per-target
/// failures are outcomes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct OpError {
    /// e.g. `ops.noProject`, `ops.planStale`, `ops.planUnknown`, `ops.invalidRequest`.
    pub code: String,
    pub message: String,
}

impl OpError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into() }
    }
}

// ------------------------------------------------- Storage & History -------

/// One category in Settings → Storage & History.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct StorageCategory {
    /// `workflowRuns`, `researchRuns`, `agentTasks`, `generatedOutputs`,
    /// `workflowRevisions`, `recoverableDeletions`, `caches`.
    pub id: String,
    pub label: String,
    pub items: Vec<StorageItem>,
    /// Bytes on disk when measurable (`None`: not measurable).
    pub bytes: Option<u64>,
    /// What deleting removes (e.g. "the record only; outputs stay").
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct StorageItem {
    /// The operation target that deletes this item.
    pub target: OpTarget,
    pub label: String,
    pub bytes: Option<u64>,
    pub created_at_ms: Option<u64>,
    /// Referenced or shared (kept by default; including it needs a decision).
    pub referenced: bool,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct StorageOverview {
    pub categories: Vec<StorageCategory>,
    /// Retained copies/backups outside any category (disclosed, not purged).
    pub excluded: Vec<String>,
}
