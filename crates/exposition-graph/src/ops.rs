//! Typed graph changes: proposals are validated and reviewable; application
//! is one SQLite transaction or an explicit conflict.
//!
//! People (IPC), presets, and agents all use these operations through the
//! same validator. Service-owned records (formal/computational evidence,
//! observed relations from indexing) are not expressible here: they enter
//! only through the owning service's internal API.

use std::collections::BTreeMap;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{EntityId, EvidenceId, ProposalId, RelationId};
use crate::model::{AssertionStatus, Entity, GraphDelta, Lifecycle, Origin, Relation, SourceSelector};
use crate::vocabulary::{EntityKind, EvidenceKind, RelationKind, ResourceRole};

/// An entity named by id, or by a temporary id created earlier in the same request.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum EntityRef {
    Id { id: EntityId },
    Temp { temp_id: String },
}

/// A binding requested by a person or agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewBinding {
    pub path: WorkspacePath,
    pub selector: SourceSelector,
    /// UTF-16 range over the current text of `path` (`Region` selectors).
    pub range: Option<TextRange>,
}

/// One typed operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum GraphOperation {
    /// Admit a meaningful entity (a claim, a collection, an experiment, ...).
    CreateEntity {
        /// Request-local id so later operations can refer to it (`[A-Za-z0-9_-]{1,32}`).
        temp_id: String,
        kind: EntityKind,
        roles: Vec<ResourceRole>,
        label: String,
        key: Option<String>,
        attributes: BTreeMap<String, String>,
        binding: Option<NewBinding>,
    },
    UpdateEntity {
        entity: EntityId,
        expected_revision: u64,
        label: Option<String>,
        attributes: Option<BTreeMap<String, String>>,
    },
    /// Archive, discard, or reactivate. Tombstoning is reserved for reconciliation.
    SetLifecycle { entity: EntityId, expected_revision: u64, lifecycle: Lifecycle },
    AddRelation {
        kind: RelationKind,
        from: EntityRef,
        to: EntityRef,
        /// Required for `evidenceFor`; service-owned kinds are refused here.
        evidence_kind: Option<EvidenceKind>,
        /// Existing evidence records to cite.
        evidence: Vec<EvidenceId>,
        note: Option<String>,
    },
    /// Accept or reject a suggested relation (people only).
    SetRelationStatus { relation: RelationId, status: AssertionStatus },
    /// Remove a relationship (the entities and files stay).
    RemoveRelation { relation: RelationId },
    /// Bind an entity to a source region (or correct its binding).
    BindSource { entity: EntityRef, binding: NewBinding },
}

/// A change as requested. The caller's origin is set by the backend from the
/// calling surface (IPC = user, agent tool = that task), never by the payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeRequest {
    /// Graph revision the requester saw.
    pub base_revision: u64,
    pub operations: Vec<GraphOperation>,
    /// Optional human summary (≤ 500 chars).
    pub summary: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

/// Why an operation is invalid (or worth noting).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphDiagnostic {
    /// Index of the operation in the request, when it concerns one.
    pub operation: Option<u32>,
    /// Stable code, e.g. `graph.endpointRule`, `graph.unknownEntity`.
    pub code: String,
    pub message: String,
    pub severity: DiagnosticSeverity,
}

impl GraphDiagnostic {
    pub fn error(operation: Option<usize>, code: &str, message: impl Into<String>) -> Self {
        Self {
            operation: operation.map(|i| i as u32),
            code: code.to_owned(),
            message: message.into(),
            severity: DiagnosticSeverity::Error,
        }
    }

    pub fn warning(operation: Option<usize>, code: &str, message: impl Into<String>) -> Self {
        Self {
            operation: operation.map(|i| i as u32),
            code: code.to_owned(),
            message: message.into(),
            severity: DiagnosticSeverity::Warning,
        }
    }
}

/// A validated, reviewable change. Valid proposals can be applied by id; the
/// backend re-validates against the then-current graph when applying.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ChangeProposal {
    pub id: ProposalId,
    pub base_revision: u64,
    pub origin: Origin,
    pub operations: Vec<GraphOperation>,
    pub diagnostics: Vec<GraphDiagnostic>,
    /// No error diagnostics.
    pub valid: bool,
    /// One line per effect, in plain words ("Add: Theorem 2 depends on Lemma 1").
    pub effects: Vec<String>,
    pub created_at_ms: u64,
}

/// Apply a stored proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyChangeRequest {
    pub proposal: ProposalId,
    /// Graph revision the reviewer saw when approving (recorded; per-entity
    /// revisions inside the operations decide conflicts).
    pub expected_revision: u64,
}

/// Temporary id → minted id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreatedEntity {
    pub temp_id: String,
    pub id: EntityId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ApplyOutcome {
    Applied {
        revision: u64,
        /// The graph changed since the proposal; it was re-validated and still applied.
        rebased: bool,
        created: Vec<CreatedEntity>,
        delta: GraphDelta,
    },
    /// Entities the operations depend on changed or vanished; nothing applied.
    Conflict { current_revision: u64, diagnostics: Vec<GraphDiagnostic> },
    /// The operations are invalid against the current graph; nothing applied.
    Invalid { current_revision: u64, diagnostics: Vec<GraphDiagnostic> },
}

/// A result of a validated change for consumers that need the records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AppliedRecords {
    pub entities: Vec<Entity>,
    pub relations: Vec<Relation>,
}
