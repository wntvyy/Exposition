//! Exposition's project graph: the persistent structural model of a project.
//!
//! Files remain authoritative for their contents. The graph owns stable
//! identities of meaningful project resources and research entities, their
//! typed relations (a closed vocabulary with endpoint rules), source bindings,
//! immutable evidence records, saved canvas views (presentation only), and
//! the change journal. It is consumed by the Editor, Research, Agents, Lean
//! verification, and workflows whether or not any graph interface is open.
//!
//! - [`vocabulary`]: entity kinds, resource roles, relation kinds + rules.
//! - [`model`]: records, status, events.
//! - [`ops`]: typed change operations, proposals, outcomes.
//! - [`query`]: snapshots, source inspection, impact, Mathematics, views.
//! - [`store`]: hardened SQLite persistence (`.exposition/graph.sqlite3`).
//! - [`validate`]: the one validator for people, presets, agents, services.
//! - [`service`]: [`GraphService`], one graph context per open project.
//! - [`journal`]: app-owned journal of cross-service operations.
//! - [`reconcile`]: incremental reconciliation with the project's files
//!   (scanning, extractors, identity matching, live-buffer overlay).
//! - [`backup`]: consistent `VACUUM INTO` backups and restore.
//!
//! Contract for services that use the graph (research, Lean, agents,
//! project creation):
//! - A resource entity's `key` is its workspace path; the reconciler adopts
//!   resources other services created with that key (one identity per file).
//! - After writing files, call [`GraphService::reconcile`] with
//!   `Scope::Paths` to index them synchronously, then look identities up
//!   ([`GraphService::resource`], [`GraphService::formal_declaration`]).
//! - Freshness ([`GraphService::freshness`]) says whether changes are
//!   pending and which unsaved buffers are overlaid; act on stale or pending
//!   state instead of assuming the graph is current.
//! - Observed relations (origin `extracted`) belong to the reconciler and
//!   are replaced when their file is re-extracted; attachment `usesAsset`
//!   relations are observed from the marker blocks (removing the block
//!   removes the relation). Services record their own assertions with a
//!   `service` origin, which reconciliation never deletes.

pub mod admission;
pub mod backup;
pub mod blueprint;
pub mod ids;
pub mod journal;
pub mod mac;
pub mod model;
pub mod ops;
pub mod query;
pub mod reconcile;
pub mod service;
pub mod store;
pub mod validate;
pub mod vocabulary;
pub mod workflow;

pub use admission::{Admission, classify};
pub use backup::BackupInfo;
pub use ids::{EntityId, EvidenceId, OperationId, ProjectId, ProposalId, RelationId, ViewId, WorkflowId};
pub use journal::{OperationJournal, OperationKind, OperationRecord, OperationState};
pub use model::*;
pub use ops::*;
pub use query::*;
pub use service::{GraphConfig, GraphError, GraphResult, GraphService, is_graph_storage};
pub use validate::{BatchOutcome, BatchRef, EntityUpsert, RelationUpsert, ServiceBatch};
pub use vocabulary::{
    EntityFamily, EntityKind, EvidenceKind, RelationFamily, RelationKind, ResourceRole, Vocabulary, vocabulary,
};
pub use workflow::{
    DraftState, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowDefinition, WorkflowSummary,
    WorkflowValidation, validate_workflow,
};
