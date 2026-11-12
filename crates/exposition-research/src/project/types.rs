//! IPC types of the research project integration (exported to TypeScript).

use exposition_core::artifact::{ArtifactRecord, AttachmentPlacement};
use exposition_core::document::DocumentId;
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_graph::{EntityId, EvidenceId, Lifecycle, OperationId, OperationRecord, RelationId, ResourceRole};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::types::RunSummary;

/// The project's research space (`.exposition/research/<space-id>/`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ResearchSpaceInfo {
    /// The `ResearchSpace` entity.
    pub entity: EntityId,
    /// Folder name under `.exposition/research/`.
    pub space_id: String,
    pub label: String,
    /// `.exposition/research/<space-id>`.
    pub root: WorkspacePath,
    pub created_at_ms: u64,
}

/// File groups of a research space (folders under its root).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum SpaceGroup {
    /// Authored research code.
    Sources,
    /// Retained input data.
    Data,
    /// Research notes.
    Notes,
}

impl SpaceGroup {
    pub const ALL: [SpaceGroup; 3] = [SpaceGroup::Sources, SpaceGroup::Data, SpaceGroup::Notes];

    pub fn dir(self) -> &'static str {
        match self {
            SpaceGroup::Sources => "sources",
            SpaceGroup::Data => "data",
            SpaceGroup::Notes => "notes",
        }
    }
}

/// How a file came into the research space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ImportMode {
    /// Copied into the space; the copy is a project file.
    Copy,
    /// Referenced where it is (read-only; never crawled or executed).
    Reference,
}

/// Recorded origin of an imported file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportOrigin {
    /// Where it came from (absolute location as chosen).
    pub source: String,
    /// SHA-256 (hex) when imported.
    pub content_hash: String,
    pub size_bytes: u64,
    pub mode: ImportMode,
    pub imported_at_ms: u64,
}

/// A file in a research-space group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SpaceFile {
    pub path: WorkspacePath,
    pub name: String,
    pub group: SpaceGroup,
    pub size_bytes: Option<u64>,
    /// Resource identity (none while the graph is unavailable).
    pub entity: Option<EntityId>,
    pub roles: Vec<ResourceRole>,
    pub origin: Option<ImportOrigin>,
}

/// An explicit read-only reference to a file outside the project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExternalReference {
    pub entity: EntityId,
    pub label: String,
    pub origin: ImportOrigin,
}

/// A manuscript (or other resource) that uses a retained result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ResultUse {
    pub entity: EntityId,
    pub label: String,
    pub path: Option<WorkspacePath>,
    pub relation: RelationId,
}

/// A retained research output (one resource identity; dataset + artifact
/// roles for tables).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RetainedResult {
    pub entity: EntityId,
    pub label: String,
    pub path: WorkspacePath,
    pub lifecycle: Lifecycle,
    pub roles: Vec<ResourceRole>,
    /// The run and artifact it came from (from the run manifest).
    pub run_id: Option<String>,
    pub artifact_id: Option<String>,
    /// `figure`, `table`, or `data`.
    pub kind: Option<String>,
    pub media_type: Option<String>,
    pub content_hash: Option<String>,
    pub used_by: Vec<ResultUse>,
    /// Experiments it was derived from.
    pub experiments: Vec<EntityId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentSummary {
    pub entity: EntityId,
    pub label: String,
    pub lifecycle: Lifecycle,
    pub revision: u64,
    pub objective: String,
    pub procedure: Option<WorkspacePath>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSummary {
    pub entity: EntityId,
    pub label: String,
    pub lifecycle: Lifecycle,
}

/// Everything the Research explorer shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ResearchListing {
    pub space: Option<ResearchSpaceInfo>,
    pub sources: Vec<SpaceFile>,
    pub data: Vec<SpaceFile>,
    pub notes: Vec<SpaceFile>,
    pub references: Vec<ExternalReference>,
    pub experiments: Vec<ExperimentSummary>,
    /// Retained results (active first, then archived, then discarded).
    pub results: Vec<RetainedResult>,
    /// Saved workflow definitions (read-only here).
    pub workflows: Vec<WorkflowSummary>,
    /// More files exist than were listed.
    pub truncated: bool,
    pub graph_revision: Option<u64>,
    /// Why graph identities are missing (the graph is unavailable).
    pub graph_unavailable: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSpaceFileRequest {
    pub group: SpaceGroup,
    /// File name (a single path component, e.g. `fit.py`).
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportRequest {
    pub group: SpaceGroup,
    /// Absolute path of the file chosen by the person.
    pub source: String,
    pub mode: ImportMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ImportOutcome {
    Copied { file: SpaceFile },
    Referenced { reference: ExternalReference },
}

/// Keep, archive, or discard a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum RetainState {
    /// Keep (active).
    Active,
    /// Keep for reference, hidden from default views.
    Archived,
    /// Discard a retained candidate (its record stays inspectable; files,
    /// attachments, and evidence elsewhere are untouched).
    Discarded,
}

impl RetainState {
    pub fn lifecycle(self) -> Lifecycle {
        match self {
            RetainState::Active => Lifecycle::Active,
            RetainState::Archived => Lifecycle::Archived,
            RetainState::Discarded => Lifecycle::Discarded,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainRequest {
    pub run_id: String,
    pub artifact_id: String,
    pub state: RetainState,
    /// Record the result as an outcome of this experiment.
    pub experiment: Option<EntityId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RetainOutcome {
    pub result: RetainedResult,
    /// Service-owned computational evidence of the run that produced it.
    pub evidence: Option<EvidenceId>,
    /// The evidence already existed.
    pub evidence_existing: bool,
    pub graph_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetResultStateRequest {
    pub entity: EntityId,
    pub state: RetainState,
}

/// Save (create or update) an experiment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExperimentInput {
    /// The experiment to update (none creates one).
    pub entity: Option<EntityId>,
    /// Entity revision the editor saw (required for updates).
    pub expected_revision: Option<u64>,
    pub title: String,
    pub objective: String,
    /// The procedure: a research script.
    pub procedure: Option<WorkspacePath>,
    /// Declared inputs (datasets, other files).
    pub inputs: Vec<WorkspacePath>,
    /// Retained results that are outcomes of this experiment.
    pub outcomes: Vec<EntityId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentFile {
    pub path: WorkspacePath,
    pub entity: EntityId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentDetail {
    pub summary: ExperimentSummary,
    pub procedure: Option<ExperimentFile>,
    pub inputs: Vec<ExperimentFile>,
    pub outcomes: Vec<RetainedResult>,
    /// Runs of the procedure and runs that produced its outcomes (operational
    /// history, newest first; not research entities).
    pub attempts: Vec<RunSummary>,
    pub graph_revision: u64,
}

/// Prepare a reviewed promotion of a run's figure/table into a manuscript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromotionRequest {
    pub run_id: String,
    pub artifact_id: String,
    pub document: DocumentId,
    /// Revision an insert offset was chosen in (refused if the document moved on).
    pub base_revision: Option<u64>,
    pub placement: AttachmentPlacement,
    pub caption: Option<String>,
    pub attachment_key: Option<String>,
}

/// One change the format adapter will make.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PreviewEdit {
    /// UTF-16 range replaced in the reviewed revision (empty for inserts).
    pub range: TextRange,
    /// One-based line of the range start.
    pub line: u32,
    /// Text being replaced (bounded).
    pub replaced: String,
    /// Exact text inserted.
    pub text: String,
}

/// What a person reviews before approving a promotion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PromotionPreview {
    pub promotion: String,
    pub document: DocumentId,
    pub path: WorkspacePath,
    /// `typst`, `latex`, or `markdown`.
    pub format: String,
    /// Document revision the markup is computed against; approval is refused
    /// when the document has changed since.
    pub revision: u64,
    pub attachment_key: String,
    pub update: bool,
    pub edits: Vec<PreviewEdit>,
    pub run_id: String,
    /// The artifact record from the run manifest (digest verified now).
    pub artifact: ArtifactRecord,
    /// The artifact's resource identity if it is already retained.
    pub retained: Option<EntityId>,
    pub warnings: Vec<String>,
    pub expires_at_ms: u64,
}

/// What became of the graph step of a promotion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PromotionGraphState {
    Recorded {
        graph_revision: u64,
        manuscript: EntityId,
        artifact: EntityId,
        relation: Option<RelationId>,
    },
    /// The document changed but the graph write failed: the operation journal
    /// keeps a record that needs reconciliation (nothing is claimed complete).
    PendingReconciliation {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PromotionOutcome {
    pub operation: OperationId,
    pub document: DocumentId,
    pub path: WorkspacePath,
    /// Document revision after the insertion (one undoable transaction).
    pub revision: u64,
    pub transaction: String,
    pub attachment_key: String,
    pub range: TextRange,
    pub warnings: Vec<String>,
    pub graph: PromotionGraphState,
}

/// Result of reconciling an interrupted promotion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileOutcome {
    pub record: OperationRecord,
    /// The attachment markers were found in the document.
    pub attachment_present: bool,
}

/// A promotion the graph follows (insertion present or undone).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TrackedAttachment {
    pub operation: OperationId,
    pub path: WorkspacePath,
    pub attachment_key: String,
    pub manuscript: EntityId,
    pub artifact: EntityId,
    /// The attachment markers are in the current text (open buffer or file).
    pub present: bool,
}

/// Promotions of the open project: those needing reconciliation and those
/// followed for undo/redo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PromotionsState {
    pub unresolved: Vec<OperationRecord>,
    pub tracked: Vec<TrackedAttachment>,
}
