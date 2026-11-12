//! Graph records: entities, source bindings, relations, evidence, project
//! status, change deltas, and events (exported to TypeScript).
//!
//! Four version concepts stay separate: the project graph revision (every
//! applied change), an entity's own revision (its record changed), a source
//! content version (hash/document revision of the bound text), and an
//! immutable run/check snapshot (evidence).

use std::collections::BTreeMap;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{EntityId, EvidenceId, ProjectId, RelationId};
use crate::vocabulary::{EntityKind, EvidenceKind, RelationKind, ResourceRole};

/// Who asserted or admitted a record.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Origin {
    /// Indexing of project sources (`extractor` names the extractor and its
    /// version, e.g. `latex/1`).
    Extracted { extractor: String },
    /// A person, through the application.
    User,
    /// An agent task (always reviewable; its assertions start as suggestions).
    Agent { task_id: String },
    /// A project or workflow preset / blueprint.
    Preset { blueprint: String },
    /// An application service (research, Lean, attachment, workspace).
    Service { service: String },
}

/// Assertion status of a relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum AssertionStatus {
    /// Observed in sources or by a service (a parser saw a citation, an
    /// attachment was inserted). Says nothing beyond what was observed.
    Observed,
    /// Proposed (e.g. by an agent) and not accepted; distinguishable from
    /// accepted assertions everywhere.
    Suggested,
    /// Asserted by a person or a chosen preset, or a suggestion a person accepted.
    Accepted,
    /// A suggestion a person rejected (kept so it is not proposed again).
    Rejected,
}

impl AssertionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AssertionStatus::Observed => "observed",
            AssertionStatus::Suggested => "suggested",
            AssertionStatus::Accepted => "accepted",
            AssertionStatus::Rejected => "rejected",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [Self::Observed, Self::Suggested, Self::Accepted, Self::Rejected].into_iter().find(|s| s.as_str() == raw)
    }
}

/// Lifecycle of an entity. Hiding from a view is presentation state, not a
/// lifecycle; nothing here deletes an underlying file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Lifecycle {
    Active,
    /// Kept for reference, hidden from default views.
    Archived,
    /// A research candidate the user discarded. Its record stays inspectable;
    /// attachments, source data, and evidence elsewhere are untouched.
    Discarded,
    /// The referent no longer exists (file deleted, declaration removed). The
    /// identity is kept for history and evidence; never silently reassigned.
    Tombstoned,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Active => "active",
            Lifecycle::Archived => "archived",
            Lifecycle::Discarded => "discarded",
            Lifecycle::Tombstoned => "tombstoned",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [Self::Active, Self::Archived, Self::Discarded, Self::Tombstoned].into_iter().find(|s| s.as_str() == raw)
    }
}

/// A graph entity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    pub id: EntityId,
    pub kind: EntityKind,
    /// Roles of a resource (empty for other kinds).
    pub roles: Vec<ResourceRole>,
    /// Concise title (e.g. a file name, `Theorem 2.1`, a declaration name).
    pub label: String,
    /// Natural key within its source when one exists (workspace path of a
    /// resource, LaTeX label, BibTeX key, Lean declaration name). Lookup aid
    /// only: identity is `id`.
    pub key: Option<String>,
    pub lifecycle: Lifecycle,
    /// Why the entity was tombstoned (file deleted, declaration removed).
    pub tombstone_reason: Option<String>,
    /// Entity revision: increases whenever this record changes.
    pub revision: u64,
    /// Who admitted it.
    pub origin: Origin,
    /// Validated metadata (keys `[a-zA-Z0-9._-]{1,64}`, values ≤ 4 KiB, ≤ 32 entries).
    pub attributes: BTreeMap<String, String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// How a binding selects its region of a source.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SourceSelector {
    /// The whole file (resources).
    WholeFile,
    /// A declaration recognised by an extractor: `syntax` names the construct
    /// (e.g. `latex.env:theorem`, `typst.figure`, `lean.theorem`,
    /// `bibtex.entry`), `name` its declared name/label when it has one, and
    /// `ordinal` disambiguates unnamed constructs within the file.
    Declaration { syntax: String, name: Option<String>, ordinal: Option<u32> },
    /// An explicit region chosen by a person or agent; re-anchored by its
    /// contextual fingerprint, never by range alone.
    Region,
}

/// Resolution state of a binding against the current source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum BindingState {
    /// Found exactly once in the current source.
    Resolved,
    /// The file or construct is gone.
    Missing,
    /// More than one candidate matches; nothing was guessed.
    Ambiguous,
    /// Not yet checked against the current source (pending reconciliation).
    Unresolved,
}

impl BindingState {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingState::Resolved => "resolved",
            BindingState::Missing => "missing",
            BindingState::Ambiguous => "ambiguous",
            BindingState::Unresolved => "unresolved",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [Self::Resolved, Self::Missing, Self::Ambiguous, Self::Unresolved].into_iter().find(|s| s.as_str() == raw)
    }
}

/// The version of a source a binding or evidence refers to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceVersion {
    /// SHA-256 (hex) of the exact UTF-8 text (or bytes, for binary files).
    pub content_hash: String,
    /// Document revision when the text came from an open buffer.
    pub revision: Option<u64>,
    /// False when the text was an unsaved buffer (never presented as saved state).
    pub saved: bool,
}

/// Where an entity lives in a source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceBinding {
    pub entity: EntityId,
    pub path: WorkspacePath,
    pub selector: SourceSelector,
    /// UTF-16 range over `version`'s text (none for whole files).
    pub range: Option<TextRange>,
    pub version: Option<SourceVersion>,
    /// Contextual fingerprint (hash of normalized surrounding text) used to
    /// re-anchor after edits; ranges alone are insufficient.
    pub fingerprint: Option<String>,
    pub state: BindingState,
    /// Why the binding is missing/ambiguous, in plain words.
    pub detail: Option<String>,
}

/// A typed relation between two entities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub id: RelationId,
    pub kind: RelationKind,
    pub from: EntityId,
    pub to: EntityId,
    pub origin: Origin,
    pub status: AssertionStatus,
    /// Kind of support (`evidenceFor` only).
    pub evidence_kind: Option<EvidenceKind>,
    /// Evidence records backing the relation.
    pub evidence: Vec<EvidenceId>,
    /// Source versions of the endpoints when the relation was asserted.
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    /// Short note (never executable, never semantic).
    pub note: Option<String>,
    pub revision: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// An input a run/check consumed, pinned by version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct VersionedInput {
    pub path: WorkspacePath,
    pub content_hash: Option<String>,
    pub revision: Option<u64>,
    /// The input was an unsaved buffer. When only its digest remains, the
    /// evidence says so (`content_hash` without stored text).
    pub unsaved: bool,
}

/// A retained output of a run/check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceOutput {
    pub path: WorkspacePath,
    pub content_hash: String,
    pub media_type: String,
}

/// Exact Lean audit verdicts (the Lean service's distinctions, preserved).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FormalVerdict {
    Checked,
    CheckedWithAxioms,
    /// Depends on `sorry`.
    Incomplete,
    ReliesOnCompiledCode,
    PartiallyChecked,
    NotVerified,
    Errors,
}

/// Whether the checker exposed formal dependencies between declarations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum FormalDependencies {
    /// The audit does not report declaration dependencies; none are inferred.
    Unavailable { reason: String },
    /// Dependencies reported by the checker itself.
    Reported { names: Vec<String> },
}

/// Service-owned formal evidence for one declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FormalEvidence {
    /// Fully qualified declaration name as audited.
    pub declaration: String,
    /// `theorem`, `def`, ... as the audit reported it.
    pub declaration_kind: String,
    /// The statement the audit printed (toolchain notation only).
    pub statement: String,
    pub axioms: Vec<String>,
    /// Verdict of the whole checked file (the audit's granularity).
    pub file_verdict: FormalVerdict,
    /// Verdict the audit reported for this declaration individually, when it
    /// did (otherwise only the file verdict applies).
    pub declaration_verdict: Option<FormalVerdict>,
    pub toolchain: String,
    pub source_path: WorkspacePath,
    /// SHA-256 of the exact checked text.
    pub source_digest: String,
    pub revision: Option<u64>,
    /// Fingerprints of the declaration's formal dependencies when the checker
    /// supplied them; `None` means freshness against dependencies is unknown.
    pub dependency_fingerprints: Option<BTreeMap<String, String>>,
    pub dependencies: FormalDependencies,
}

/// Outcome recorded by an evidence record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EvidenceOutcome {
    Succeeded,
    Failed {
        message: String,
    },
    Canceled,
    Interrupted,
    Verdict {
        verdict: FormalVerdict,
    },
    /// A review/report outcome in words.
    Reported {
        summary: String,
    },
}

/// The service operation an evidence record refers to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OperationRef {
    /// `researchRun`, `leanAudit`, `build`, `agentTask`, `attachment`, `review`.
    pub kind: String,
    pub id: String,
}

/// Immutable evidence: a reference to a run/audit with its exact inputs.
/// Old evidence stays valid history for its own inputs; current applicability
/// is computed (`Freshness`), never rewritten into the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub id: EvidenceId,
    pub kind: EvidenceKind,
    /// The entity it is about (e.g. the formal declaration, the artifact).
    pub subject: Option<EntityId>,
    pub operation: OperationRef,
    pub inputs: Vec<VersionedInput>,
    pub outcome: EvidenceOutcome,
    /// Tool/runtime versions (name, version).
    pub environment: Vec<(String, String)>,
    pub outputs: Vec<EvidenceOutput>,
    pub formal: Option<FormalEvidence>,
    pub recorded_by: Origin,
    pub recorded_at_ms: u64,
}

/// Whether a record still applies to the current sources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Freshness {
    /// Recorded against exactly the current versions.
    Current,
    /// Something it depends on changed since; it is history for old inputs.
    Stale { reasons: Vec<String> },
    /// Cannot be determined (missing fingerprints, unindexed sources). Unknown is not current.
    Unknown { reason: String },
}

/// Coverage of a resource's extraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CoverageState {
    /// Every supported construct was extracted.
    Complete,
    /// Some constructs were not understood (macros, unsupported syntax).
    Partial,
    /// No extractor for this kind of file: only the resource itself is known.
    Unsupported,
    /// Not indexed yet (or changed since).
    Pending,
    /// The extractor failed (unreadable, too large).
    Failed,
}

impl CoverageState {
    pub fn as_str(self) -> &'static str {
        match self {
            CoverageState::Complete => "complete",
            CoverageState::Partial => "partial",
            CoverageState::Unsupported => "unsupported",
            CoverageState::Pending => "pending",
            CoverageState::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [Self::Complete, Self::Partial, Self::Unsupported, Self::Pending, Self::Failed]
            .into_iter()
            .find(|s| s.as_str() == raw)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCoverage {
    pub path: WorkspacePath,
    /// Extractor name and version (`latex/1`), or none.
    pub extractor: Option<String>,
    pub state: CoverageState,
    /// Bindings in this resource that are missing, ambiguous, or unresolved.
    pub unresolved: u32,
    /// What was not understood, in plain words.
    pub detail: Option<String>,
    pub content_hash: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSummary {
    pub resources: u32,
    pub complete: u32,
    pub partial: u32,
    pub unsupported: u32,
    pub pending: u32,
    pub failed: u32,
    pub unresolved_bindings: u32,
}

/// Synchronization of the graph with the project's sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Every known source version is reconciled.
    Synchronized,
    /// Reconciliation is running.
    Reconciling,
    /// Changes are waiting to be reconciled.
    Pending,
    /// The graph cannot be used now (see `GraphStatus`).
    Unavailable,
}

/// An open buffer whose unsaved text is overlaid on the saved projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OverlayDocument {
    pub path: WorkspacePath,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphFreshness {
    pub state: SyncState,
    /// Paths waiting for reconciliation.
    pub pending: u32,
    /// Last completed reconciliation (Unix ms).
    pub checkpoint_ms: Option<u64>,
    /// Unsaved buffers included in queries (labelled; not saved file state).
    pub overlay: Vec<OverlayDocument>,
}

impl GraphFreshness {
    pub fn unavailable() -> Self {
        Self { state: SyncState::Unavailable, pending: 0, checkpoint_ms: None, overlay: Vec::new() }
    }
}

/// How a damaged or unavailable graph can be recovered (never automatic
/// replacement with an empty graph).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RecoveryOption {
    /// Restore a consistent backup taken at `taken_at_ms`.
    RestoreBackup { backup: String, taken_at_ms: u64, revision: Option<u64> },
    /// Start a new graph for this project; the damaged database is kept aside.
    StartNew,
    /// Retry opening (e.g. the file was locked).
    Retry,
}

/// Where the project graph stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GraphState {
    /// No project open.
    Detached,
    /// The project graph is open for reading and writing.
    Attached,
    /// The folder cannot hold the graph (read-only): a temporary graph in
    /// application data is used and nothing curated there persists with the project.
    TemporaryReadOnly { reason: String },
    /// The graph cannot be used. Source editing keeps working; graph-dependent
    /// actions report unavailable instead of claiming success.
    Unavailable { reason: String, recovery: Vec<RecoveryOption> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphStatus {
    pub state: GraphState,
    pub project: Option<ProjectId>,
    /// Absolute project folder.
    pub root: Option<String>,
    pub schema_version: Option<u32>,
    /// Project graph revision (every applied change increments it).
    pub revision: u64,
    pub entities: u32,
    pub relations: u32,
    pub freshness: GraphFreshness,
    pub coverage: CoverageSummary,
}

impl GraphStatus {
    pub fn detached() -> Self {
        Self {
            state: GraphState::Detached,
            project: None,
            root: None,
            schema_version: None,
            revision: 0,
            entities: 0,
            relations: 0,
            freshness: GraphFreshness::unavailable(),
            coverage: CoverageSummary::default(),
        }
    }

    pub fn is_usable(&self) -> bool {
        matches!(self.state, GraphState::Attached | GraphState::TemporaryReadOnly { .. })
    }
}

/// What one applied change did (ids only; bounded).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphDelta {
    pub revision: u64,
    pub entities_added: Vec<EntityId>,
    pub entities_updated: Vec<EntityId>,
    /// Tombstoned (never hard-deleted).
    pub entities_removed: Vec<EntityId>,
    pub relations_added: Vec<RelationId>,
    pub relations_updated: Vec<RelationId>,
    pub relations_removed: Vec<RelationId>,
    /// More ids changed than listed; re-query.
    pub truncated: bool,
}

impl GraphDelta {
    pub fn is_empty(&self) -> bool {
        self.entities_added.is_empty()
            && self.entities_updated.is_empty()
            && self.entities_removed.is_empty()
            && self.relations_added.is_empty()
            && self.relations_updated.is_empty()
            && self.relations_removed.is_empty()
    }
}

/// Graph events, forwarded to the webview as `graph:event` (`graph:resync`
/// when a listener fell behind).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum GraphEvent {
    Status { status: GraphStatus },
    Changed { delta: GraphDelta, origin: Origin },
    Reconciled { revision: u64, freshness: GraphFreshness },
}

/// An entity with everything needed to inspect it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EntityDetail {
    pub entity: Entity,
    pub bindings: Vec<SourceBinding>,
    pub outgoing: Vec<Relation>,
    pub incoming: Vec<Relation>,
    /// The other endpoints of those relations (bounded).
    pub neighbors: Vec<Entity>,
    pub evidence: Vec<Evidence>,
    /// Applicability of each evidence record to current sources.
    pub evidence_freshness: Vec<(EvidenceId, Freshness)>,
    pub coverage: Option<ResourceCoverage>,
    pub graph_revision: u64,
}
