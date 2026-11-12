//! Research in the project graph: the research space, retained outcomes,
//! experiments, and reviewed promotion into manuscripts.
//!
//! Files stay authoritative for their contents; the graph holds identities
//! and relations. Every graph write here goes through the graph service's
//! validated service path (origin `research`) or, for what a person asserts
//! (experiments), the same validated change path as manual graph edits.
//! Nothing here launches code: enabling Research creates a typed container
//! and folders, and execution stays with the existing (trusted-native)
//! research sessions.

mod experiment;
mod promotion;
mod retain;
mod space;
pub mod types;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use exposition_graph::{
    AssertionStatus, BatchRef, Entity, EntityId, EntityKind, EntityUpsert, GraphService, Origin, RelationKind,
    RelationUpsert, ServiceBatch,
};
use exposition_workspace::DocumentService;

pub use promotion::{attachment_present, marker_lines};

use crate::ResearchService;
use crate::error::ResearchError;

/// The service name research graph writes are attributed to.
pub const SERVICE: &str = "research";

/// Research's integration with the project graph and manuscripts.
pub struct ResearchProject {
    graph: Arc<GraphService>,
    research: Arc<ResearchService>,
    documents: Arc<DocumentService>,
    /// Serializes enabling the research space (exactly one per project).
    enable: Mutex<()>,
    promotions: promotion::Promotions,
}

pub(crate) fn origin() -> Origin {
    Origin::Service { service: SERVICE.into() }
}

/// `sha256:<hex>` → `<hex>` (graph records use bare hex digests).
pub(crate) fn bare_hash(hash: &str) -> String {
    hash.strip_prefix("sha256:").unwrap_or(hash).to_owned()
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Truncate to a graph label (one line, ≤ 200 bytes).
pub(crate) fn label(text: &str) -> String {
    let one: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let one = one.trim();
    if one.len() <= 200 {
        return if one.is_empty() { "Untitled".into() } else { one.to_owned() };
    }
    let mut end = 197;
    while !one.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &one[..end])
}

impl ResearchProject {
    pub fn new(graph: Arc<GraphService>, research: Arc<ResearchService>, documents: Arc<DocumentService>) -> Self {
        Self { graph, research, documents, enable: Mutex::new(()), promotions: promotion::Promotions::default() }
    }

    pub fn graph(&self) -> &Arc<GraphService> {
        &self.graph
    }

    /// The open project folder, when research and the graph agree on it.
    fn root(&self) -> Result<PathBuf, ResearchError> {
        let root = self.research.workspace_root().ok_or(ResearchError::NoWorkspace)?;
        Ok(root)
    }

    /// The open project folder, refusing graph writes while the graph is not
    /// attached to exactly this folder.
    fn graph_root(&self) -> Result<PathBuf, ResearchError> {
        let root = self.root()?;
        let status = self.graph.status();
        if !status.is_usable() {
            return Err(match status.state {
                exposition_graph::GraphState::Unavailable { reason, .. } => ResearchError::coded(
                    "graph.unavailable",
                    format!("The project graph is unavailable ({reason}); nothing was recorded."),
                ),
                _ => ResearchError::coded("graph.noProject", "No project graph is attached."),
            });
        }
        match self.graph.root() {
            Some(g) if g == root => Ok(root),
            _ => Err(ResearchError::coded(
                "graph.projectChanged",
                "The project graph belongs to another folder; reopen the project and try again.",
            )),
        }
    }

    fn entity(&self, id: &EntityId) -> Result<Entity, ResearchError> {
        self.graph
            .with_store(|s| Ok(s.entity(id)?))?
            .ok_or_else(|| ResearchError::coded("graph.unknownEntity", format!("entity {id} does not exist")))
    }

    /// Replace an entity's attributes and/or lifecycle (service upsert; the
    /// rest of the record is kept).
    fn update_entity(
        &self,
        entity: &Entity,
        attributes: Option<BTreeMap<String, String>>,
        lifecycle: Option<exposition_graph::Lifecycle>,
        summary: &str,
    ) -> Result<(), ResearchError> {
        let attributes = attributes.unwrap_or_else(|| entity.attributes.clone());
        if attributes == entity.attributes && lifecycle.is_none_or(|l| l == entity.lifecycle) {
            return Ok(());
        }
        let batch = ServiceBatch {
            entities: vec![EntityUpsert {
                id: Some(entity.id.clone()),
                kind: entity.kind,
                roles: entity.roles.clone(),
                label: entity.label.clone(),
                key: entity.key.clone(),
                attributes,
                lifecycle,
                tombstone_reason: entity.tombstone_reason.clone(),
                bindings: None,
            }],
            summary: summary.to_owned(),
            ..Default::default()
        };
        self.graph.apply_service_batch(&origin(), &batch)?;
        Ok(())
    }

    /// Add relations observed/asserted by research (duplicates are skipped).
    fn add_relations(
        &self,
        relations: Vec<(RelationKind, EntityId, EntityId, Option<AssertionStatus>, Option<String>)>,
        summary: &str,
    ) -> Result<u64, ResearchError> {
        if relations.is_empty() {
            return Ok(self.graph.with_store(|s| Ok(s.revision()?))?);
        }
        let batch = ServiceBatch {
            relations: relations
                .into_iter()
                .map(|(kind, from, to, status, note)| RelationUpsert {
                    kind,
                    from: BatchRef::Existing(from),
                    to: BatchRef::Existing(to),
                    status,
                    evidence_kind: None,
                    evidence: Vec::new(),
                    note,
                })
                .collect(),
            summary: summary.to_owned(),
            ..Default::default()
        };
        Ok(self.graph.apply_service_batch(&origin(), &batch)?.revision)
    }

    /// Entities of `kinds` (active and inactive), bounded.
    fn entities_of(&self, kinds: &[EntityKind], limit: usize) -> Result<Vec<Entity>, ResearchError> {
        Ok(self.graph.with_store(|s| Ok(s.find_entities(Some(kinds), None, true, limit)?.0))?)
    }
}

#[cfg(test)]
mod tests;
