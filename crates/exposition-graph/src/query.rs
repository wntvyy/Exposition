//! Bounded read requests: snapshots, source inspection, change impact, the
//! Mathematics projection, and saved canvas views (presentation only).

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{EntityId, EvidenceId, ProjectId, RelationId, ViewId, WorkflowId};
use crate::model::{
    CoverageSummary, Entity, Evidence, FormalDependencies, FormalVerdict, Freshness, GraphFreshness, Relation,
    ResourceCoverage, SourceBinding,
};
use crate::vocabulary::{EntityKind, EvidenceKind, RelationKind};

/// Default and maximum numbers of entities a snapshot returns.
pub const DEFAULT_LIMIT: u32 = 300;
pub const MAX_LIMIT: u32 = 2000;
/// Maximum traversal depth for neighborhoods and impact.
pub const MAX_DEPTH: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Outgoing,
    Incoming,
    Both,
}

/// Entities within `depth` relations of `entity`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Neighborhood {
    pub entity: EntityId,
    pub depth: u32,
    /// Only follow these kinds (all when absent).
    pub relation_kinds: Option<Vec<RelationKind>>,
    pub direction: Direction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphQuery {
    /// Only these entity kinds (all when absent).
    pub kinds: Option<Vec<EntityKind>>,
    /// Case-insensitive match on label or key (≤ 200 chars).
    pub text: Option<String>,
    pub around: Option<Neighborhood>,
    /// Include archived, discarded, and tombstoned entities.
    pub include_inactive: bool,
    /// Maximum entities (default 300, at most 2000).
    pub limit: Option<u32>,
}

/// A bounded slice of the graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphSnapshot {
    pub project: Option<ProjectId>,
    pub revision: u64,
    pub entities: Vec<Entity>,
    /// Relations among the returned entities.
    pub relations: Vec<Relation>,
    /// More entities matched than were returned.
    pub truncated: bool,
    /// Matching entities in total.
    pub total: u32,
    pub coverage: CoverageSummary,
    pub freshness: GraphFreshness,
}

/// What is at a source position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InspectAtSourceRequest {
    pub path: WorkspacePath,
    /// UTF-16 offset in the current text (the whole file when absent).
    pub offset: Option<u32>,
    /// Document revision the offset refers to (open buffers).
    pub revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct InspectedEntity {
    pub entity: Entity,
    pub binding: SourceBinding,
    pub relations: Vec<Relation>,
    /// Other endpoints of `relations`.
    pub related: Vec<Entity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceInspection {
    pub path: WorkspacePath,
    /// Revision of the text inspected (open buffer), if any.
    pub revision: Option<u64>,
    /// The inspected text is an unsaved buffer (never presented as saved state).
    pub unsaved: bool,
    /// The resource entity of the file.
    pub resource: Option<Entity>,
    /// Entities bound at the offset, innermost first.
    pub entities: Vec<InspectedEntity>,
    pub coverage: Option<ResourceCoverage>,
    pub freshness: GraphFreshness,
    pub graph_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImpactRequest {
    pub entities: Vec<EntityId>,
    /// Default 4, at most 8.
    pub max_depth: Option<u32>,
    /// Default 200, at most 1000.
    pub max_items: Option<u32>,
}

/// What a dependent should do after a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ImpactAction {
    /// Mathematical dependents: review (a changed dependency never makes a theorem false).
    NeedsReview,
    /// Derived outputs/evidence no longer match their inputs.
    Stale,
    /// Citations/mentions may need a look.
    CitationReview,
    /// Related, no action implied.
    Informational,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImpactStep {
    pub relation: RelationId,
    pub kind: RelationKind,
    pub from: EntityId,
    pub to: EntityId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImpactItem {
    pub entity: Entity,
    pub depth: u32,
    /// Relations from a changed entity to this one.
    pub path: Vec<ImpactStep>,
    pub action: ImpactAction,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImpactReport {
    pub roots: Vec<EntityId>,
    pub items: Vec<ImpactItem>,
    /// Cycles met during traversal (each listed once, as entity ids).
    pub cycles: Vec<Vec<EntityId>>,
    pub truncated: bool,
    pub work_limit_reached: bool,
    /// Files through which impact is unknown (extraction partial, failed,
    /// pending, or unsupported for a text source; unresolved bindings).
    pub unknown_coverage: Vec<WorkspacePath>,
    /// Evidence recorded for the changed entities and for dependents that
    /// are stale or need review, with its current applicability (old
    /// evidence stays valid history for the inputs it recorded).
    pub evidence: Vec<EvidenceImpact>,
    pub graph_revision: u64,
}

/// Evidence affected by a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceImpact {
    pub evidence: EvidenceId,
    pub kind: EvidenceKind,
    /// The entity it was found through.
    pub subject: EntityId,
    /// Applicability to the current sources (unknown is never current).
    pub freshness: Freshness,
    pub reason: String,
}

/// Informal proof/review state of a mathematical entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum InformalState {
    /// Not a statement (definitions, assumptions, objects).
    NotApplicable,
    /// A statement with no linked proof.
    NoProof,
    /// A proof targets it (`proofOf`, accepted or observed).
    ProofLinked,
    /// Only suggested proof links exist.
    ProofSuggested,
}

/// Formal association state, separate from the verifier's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FormalAssociationState {
    None,
    /// A `formalizes` relation was suggested, not reviewed.
    Suggested,
    /// A person reviewed and accepted the correspondence (an assertion, not a
    /// machine proof of equivalence).
    Reviewed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MathRow {
    pub entity: Entity,
    pub binding: Option<SourceBinding>,
    pub informal: InformalState,
    pub formal_association: FormalAssociationState,
    /// Associated formal declarations.
    pub formal_declarations: Vec<EntityId>,
    /// Exact verdict of the latest formal evidence of the associated
    /// declaration (or of this declaration). `None` = no evidence.
    pub verdict: Option<FormalVerdict>,
    /// Whether that evidence still matches the current sources.
    pub freshness: Option<Freshness>,
    pub formal_dependencies: Option<FormalDependencies>,
    /// The latest formal evidence, when any.
    pub evidence: Option<Evidence>,
    /// That evidence was not recorded by this installation (imported with the
    /// project's graph): a historical claim, never current verification.
    pub imported: bool,
}

/// The Mathematics projection of the canonical graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MathOverview {
    pub rows: Vec<MathRow>,
    /// Mathematical relations among the rows (dependsOn, assumes, proofOf,
    /// formalizes, references, evidenceFor).
    pub relations: Vec<Relation>,
    pub truncated: bool,
    pub coverage: CoverageSummary,
    pub graph_revision: u64,
}

/// Which projection a saved view shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum ViewScope {
    Project,
    Mathematics,
    Workflow { workflow: WorkflowId },
    Local { entity: EntityId, depth: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewNode {
    pub entity: EntityId,
    pub x: f64,
    pub y: f64,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    /// 0.05–8.
    pub zoom: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewFilters {
    pub kinds: Vec<EntityKind>,
    pub relation_kinds: Vec<RelationKind>,
    pub text: Option<String>,
    pub show_inactive: bool,
    pub depth: Option<u32>,
}

/// A saved canvas view: positions, viewport, filters, hidden and collapsed
/// entities. Presentation only — never changes dependency semantics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphView {
    pub id: ViewId,
    pub name: String,
    pub scope: ViewScope,
    pub nodes: Vec<ViewNode>,
    pub viewport: Viewport,
    pub filters: ViewFilters,
    /// Hidden from this view (not deleted, not removed from the graph).
    pub hidden: Vec<EntityId>,
    pub collapsed: Vec<EntityId>,
    pub revision: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphViewSummary {
    pub id: ViewId,
    pub name: String,
    pub scope: ViewScope,
    pub revision: u64,
    pub updated_at_ms: u64,
}

/// Save (create or replace) a view. `expected_revision` must match the stored
/// view's revision when replacing (absent = create).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphViewInput {
    pub id: Option<ViewId>,
    pub name: String,
    pub scope: ViewScope,
    pub nodes: Vec<ViewNode>,
    pub viewport: Viewport,
    pub filters: ViewFilters,
    pub hidden: Vec<EntityId>,
    pub collapsed: Vec<EntityId>,
    pub expected_revision: Option<u64>,
}
