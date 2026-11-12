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

pub mod admission;
pub mod blueprint;
pub mod ids;
pub mod journal;
pub mod model;
pub mod ops;
pub mod query;
pub mod service;
pub mod store;
pub mod validate;
pub mod vocabulary;
pub mod workflow;

pub use admission::{Admission, classify};
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
