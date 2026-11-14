//! P1 project operations on records the project graph holds: impacts of
//! deleting project paths, deletion / restore / purge of curated entities,
//! relationships, saved views and saved workflows (with their revisions),
//! duplication of workflows, views and manually curated entities, copied-from
//! provenance for file copies, and cleanup of unreferenced tombstones.
//!
//! Rules (spec "Pre-UI project operations", coordinator decisions 3–6):
//! - Only concrete consequences are impacts: a manuscript that uses a file, a
//!   saved workflow whose input it is, retained evidence that records it and
//!   is still used by a surviving object, a revision pinned by a surviving
//!   composite. Graph membership, containment, historical mentions and
//!   provenance alone are never impacts.
//! - Files remain authoritative: a file-backed resource is deleted by deleting
//!   its source file (`ops.useSourceFile`), a declaration extracted from a
//!   source by editing the source (`ops.sourceDerived`); this module never
//!   makes a metadata-only delete that would reappear on reindex.
//! - Ordinary deletion is app-managed and recoverable (a snapshot in table
//!   `recoverable`, restored with the same identity); permanent deletion and
//!   purge really remove the records.
//! - Copies get fresh identities; they never carry evidence, verification,
//!   source keys, or attachments to existing consumers.

use std::collections::{BTreeMap, BTreeSet};

use exposition_core::ops::{ImpactKind, OpConsumer, OpError, OpImpact, OpTarget};
use exposition_core::path::WorkspacePath;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::ids::{EntityId, EvidenceId, RelationId, ViewId, WorkflowId};
use crate::model::{AssertionStatus, Entity, GraphDelta, Lifecycle, Origin, Relation, SourceBinding, SourceSelector};
use crate::ops::{ChangeRequest, EntityRef, GraphOperation};
use crate::query::GraphView;
use crate::service::{GraphError, GraphService};
use crate::store::{GraphStore, StoreError, Writer, now_ms};
use crate::validate::{EntityUpsert, ServiceBatch};
use crate::vocabulary::{EntityKind, EvidenceKind, RelationKind, ResourceRole};
use crate::workflow::persist::{is_retired, revisions, stored_revision};
use crate::workflow::{SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowDefinition};

/// Recoverable kinds this module keeps (the prefix of a recoverable id).
pub const RECOVERABLE_ENTITY: &str = "entity";
pub const RECOVERABLE_RELATION: &str = "relation";
pub const RECOVERABLE_VIEW: &str = "view";
pub const RECOVERABLE_WORKFLOW: &str = "workflow";

/// Attribute naming the path a copied file was copied from.
pub const COPY_FROM: &str = "copy.from";
/// Attribute naming the entity a copy was made from (provenance only).
pub const COPY_FROM_ENTITY: &str = "copy.fromEntity";

/// The service origin of project-operation writes.
pub fn service_origin() -> Origin {
    Origin::Service { service: "projectOps".into() }
}

/// `<kind>:<id>`: the id of an app-managed recoverable deletion.
pub fn recoverable_id(kind: &str, id: &str) -> String {
    format!("{kind}:{id}")
}

/// Split a recoverable id into its kind and record id.
pub fn split_recoverable(id: &str) -> Option<(&str, &str)> {
    id.split_once(':').filter(|(k, v)| !k.is_empty() && !v.is_empty())
}

fn same_text(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// `path` is `prefix` or lies beneath it (Windows compares names without case).
pub fn path_under(path: &WorkspacePath, prefix: &WorkspacePath) -> bool {
    let (p, c) = (path.as_str(), prefix.as_str());
    if same_text(p, c) {
        return true;
    }
    p.len() > c.len() + 1 && p.is_char_boundary(c.len()) && p.as_bytes()[c.len()] == b'/' && same_text(&p[..c.len()], c)
}

fn under_any(path: &WorkspacePath, prefixes: &[WorkspacePath]) -> Option<usize> {
    prefixes.iter().position(|t| path_under(path, t))
}

fn refuse(code: &str, message: impl Into<String>) -> OpError {
    OpError::new(code, message)
}

fn from_graph(e: GraphError) -> OpError {
    OpError::new(&e.code, e.message)
}

fn quoted(label: &str) -> String {
    format!("“{label}”")
}

/// A label with " (copy)" appended, within `max` bytes and on one line.
pub fn copy_label(label: &str, max: usize) -> String {
    const SUFFIX: &str = " (copy)";
    let base: String = label.lines().next().unwrap_or("").trim().to_owned();
    let base = if base.is_empty() { "Untitled".to_owned() } else { base };
    let room = max.saturating_sub(SUFFIX.len());
    let mut end = base.len().min(room);
    while !base.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{SUFFIX}", base[..end].trim_end())
}

// ------------------------------------------------------------ snapshots ----

/// What a recoverable deletion keeps to restore the records exactly.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    #[serde(default)]
    entities: Vec<Entity>,
    #[serde(default)]
    bindings: Vec<SourceBinding>,
    #[serde(default)]
    relations: Vec<Relation>,
    #[serde(default)]
    view: Option<GraphView>,
    #[serde(default)]
    workflow: Option<WorkflowId>,
}

/// One app-managed recoverable deletion of a graph-held record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverableRecord {
    /// `<kind>:<id>` (see [`recoverable_id`]).
    pub id: String,
    pub kind: String,
    pub label: String,
    /// Size of the kept snapshot (the record payload a purge removes).
    pub bytes: u64,
    pub deleted_at_ms: u64,
}

/// What a restore did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredRecord {
    /// The restored item, as an operation target.
    pub target: OpTarget,
    /// Relationships that could not be restored (an endpoint is gone).
    pub skipped_relations: usize,
}

fn put_recoverable(w: &Writer<'_>, id: &str, kind: &str, label: &str, snapshot: &Snapshot) -> Result<(), StoreError> {
    let body = serde_json::to_string(snapshot)?;
    if body.len() > 4 * 1024 * 1024 {
        return Err(StoreError::Limit("the deleted record is too large to keep for recovery".into()));
    }
    w.tx.execute(
        "INSERT INTO recoverable (id, kind, label, body, deleted_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, label = excluded.label, body = excluded.body,
         deleted_at = excluded.deleted_at",
        params![id, kind, crate::store::truncate(label, 200), body, now_ms() as i64],
    )?;
    Ok(())
}

fn load_snapshot(conn: &rusqlite::Connection, id: &str) -> Result<Option<(String, String, Snapshot)>, StoreError> {
    let row: Option<(String, String, String)> = conn
        .query_row("SELECT kind, label, body FROM recoverable WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    match row {
        None => Ok(None),
        Some((kind, label, body)) => {
            let snapshot: Snapshot = serde_json::from_str(&body)
                .map_err(|e| StoreError::Untrusted(format!("unreadable recoverable record: {e}")))?;
            Ok(Some((kind, label, snapshot)))
        }
    }
}

/// Remove an entity with its bindings and every relation touching it
/// (inside the caller's transaction). Returns the removed relation ids.
fn remove_entity_rows(w: &Writer<'_>, id: &EntityId) -> Result<Vec<RelationId>, StoreError> {
    let mut removed = Vec::new();
    {
        let mut stmt = w.tx.prepare("SELECT id FROM relations WHERE from_id = ?1 OR to_id = ?1 LIMIT 1000000")?;
        let rows = stmt.query_map([id.as_str()], |r| r.get::<_, String>(0))?;
        for row in rows {
            if let Ok(rid) = RelationId::parse(&row?) {
                removed.push(rid);
            }
        }
    }
    w.tx.execute("DELETE FROM relations WHERE from_id = ?1 OR to_id = ?1", [id.as_str()])?;
    w.tx.execute("DELETE FROM bindings WHERE entity_id = ?1", [id.as_str()])?;
    w.tx.execute("DELETE FROM entities WHERE id = ?1", [id.as_str()])?;
    Ok(removed)
}

fn mark(list: &mut Vec<RelationId>, ids: Vec<RelationId>) {
    for id in ids {
        if list.len() < 500 && !list.contains(&id) {
            list.push(id);
        }
    }
}

fn live(store: &GraphStore, id: &EntityId) -> Result<Option<Entity>, StoreError> {
    Ok(store.entity(id)?.filter(|e| e.lifecycle != Lifecycle::Tombstoned))
}

// -------------------------------------------------------------- checks ----

/// What deleting a graph entity would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityDeleteCheck {
    pub entity: Entity,
    /// Relationships removed with it.
    pub relations: usize,
    /// Evidence still linked to it (it stays as history; the link goes).
    pub impacts: Vec<OpImpact>,
}

/// What deleting a relationship would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationDeleteCheck {
    pub relation: Relation,
    /// "Theorem 2 depends on Lemma 1".
    pub description: String,
    pub impacts: Vec<OpImpact>,
}

/// A revision of a workflow that a surviving composite runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowPin {
    pub revision: u64,
    pub by: WorkflowId,
    pub by_title: String,
    pub by_revision: u64,
}

/// What deleting a saved workflow would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowDeleteCheck {
    pub id: WorkflowId,
    pub title: String,
    pub latest: u64,
    /// Every stored revision, newest first.
    pub revisions: Vec<u64>,
    /// Revisions other workflows' composite steps pin.
    pub pins: Vec<WorkflowPin>,
    /// Already removed from active use (only a purge remains).
    pub retired: bool,
    /// A recoverable snapshot exists (restorable).
    pub recoverable: bool,
}

/// One stored workflow revision (Storage & History).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionSize {
    pub workflow: WorkflowId,
    pub title: String,
    pub revision: u64,
    pub bytes: u64,
    pub saved_at_ms: u64,
    /// The current revision of a workflow in active use (not history).
    pub current: bool,
    /// The workflow was deleted; this revision is kept for recovery or pins.
    pub retired: bool,
    /// Titles of the workflows whose composite steps pin it.
    pub pinned_by: Vec<String>,
}

/// What a workflow deletion did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowDeletion {
    /// Recoverable id when the deletion can be restored.
    pub recoverable: Option<String>,
    /// Revisions removed for good.
    pub purged: Vec<u64>,
    /// Revisions kept because surviving composites pin them.
    pub kept: Vec<u64>,
}

impl GraphService {
    // ------------------------------------------------------------ entities

    /// Whether `id` can be deleted through the graph (curated records only)
    /// and what that affects. File-backed resources, source-derived
    /// declarations, workflows, and the project's structure are refused with
    /// a code that says where the deletion belongs.
    pub fn check_entity_delete(&self, id: &EntityId) -> Result<EntityDeleteCheck, OpError> {
        self.with_store(|store| {
            let Some(entity) = store.entity(id)? else {
                return Ok(Err(refuse("ops.notFound", "That item no longer exists in the project graph.")));
            };
            if let Err(e) = deletable(store, &entity)? {
                return Ok(Err(e));
            }
            let outgoing = store.relations_from(id)?;
            let incoming = store.relations_to(id)?;
            // Like a path: only a surviving item that relies on evidence this
            // item provides is a consequence (an item it supports loses that
            // support). Being the subject of evidence, or being supported,
            // affects nothing that remains.
            let mut consumers = Vec::new();
            for r in &outgoing {
                if r.kind == RelationKind::EvidenceFor
                    && !r.evidence.is_empty()
                    && let Some(o) = live(store, &r.to)?
                {
                    consumers.push(consumer_entity(&o));
                }
            }
            let mut impacts = Vec::new();
            if !consumers.is_empty() {
                consumers.sort_by(|a, b| a.id.cmp(&b.id));
                consumers.dedup_by(|a, b| a.id == b.id);
                let message = format!(
                    "{} provides recorded evidence for {}; deleting it removes that support. The evidence records and their verdicts stay as history.",
                    quoted(&entity.label),
                    names(&consumers)
                );
                impacts.push(OpImpact {
                    kind: ImpactKind::RetainedEvidence,
                    target: OpTarget::GraphEntity { entity: id.to_string() },
                    consumers,
                    message,
                });
            }
            Ok(Ok(EntityDeleteCheck { relations: outgoing.len() + incoming.len(), entity, impacts }))
        })
        .map_err(from_graph)?
    }

    /// Delete a curated entity with its relationships. Ordinary deletion
    /// keeps a snapshot (restorable with the same identity); `permanent`
    /// removes it for good. Returns the recoverable id, if any.
    pub fn delete_entity(&self, origin: &Origin, id: &EntityId, permanent: bool) -> Result<Option<String>, OpError> {
        let (delta, recoverable) = self
            .with_store(|store| {
                let Some(entity) = store.entity(id)? else {
                    return Ok(Err(refuse("ops.notFound", "That item no longer exists in the project graph.")));
                };
                if let Err(e) = deletable(store, &entity)? {
                    return Ok(Err(e));
                }
                let bindings = store.bindings(id)?;
                let mut relations = store.relations_from(id)?;
                relations.extend(store.relations_to(id)?);
                let w = Writer::new(store.transaction()?);
                let recoverable = if permanent {
                    None
                } else {
                    let rid = recoverable_id(RECOVERABLE_ENTITY, id.as_str());
                    let snapshot =
                        Snapshot { entities: vec![entity.clone()], bindings, relations, ..Default::default() };
                    put_recoverable(&w, &rid, RECOVERABLE_ENTITY, &entity.label, &snapshot)?;
                    Some(rid)
                };
                let removed = remove_entity_rows(&w, id)?;
                let mut delta = GraphDelta { entities_removed: vec![id.clone()], ..Default::default() };
                mark(&mut delta.relations_removed, removed);
                let verb = if permanent { "Delete permanently" } else { "Delete" };
                w.record_change(
                    origin,
                    &format!("{verb} {} {}", entity.kind.title().to_lowercase(), quoted(&entity.label)),
                    &mut delta,
                )?;
                w.commit()?;
                Ok(Ok((delta, recoverable)))
            })
            .map_err(from_graph)??;
        self.emit_changed(delta, origin.clone());
        Ok(recoverable)
    }

    // ----------------------------------------------------------- relations

    /// Whether a relationship can be deleted (curated relations only:
    /// relationships observed in the sources follow the sources).
    pub fn check_relation_delete(&self, id: &RelationId) -> Result<RelationDeleteCheck, OpError> {
        self.with_store(|store| {
            let Some(relation) = store.relation(id)? else {
                return Ok(Err(refuse("ops.notFound", "That relationship no longer exists.")));
            };
            if relation.status == AssertionStatus::Observed || !relation.kind.is_manual() {
                return Ok(Err(refuse(
                    "ops.sourceDerived",
                    "This relationship was observed in the sources (or follows the files); to change it, change the source.",
                )));
            }
            let from = store.entity(&relation.from)?;
            let to = store.entity(&relation.to)?;
            let label = |e: &Option<Entity>| e.as_ref().map(|e| e.label.clone()).unwrap_or_else(|| "(missing)".into());
            let description = format!("{} {} {}", label(&from), relation.kind.reading(), label(&to));
            // Removing the link is the action itself: the evidence records stay
            // (and stay available), so nothing surviving loses anything else.
            Ok(Ok(RelationDeleteCheck { relation, description, impacts: Vec::new() }))
        })
        .map_err(from_graph)?
    }

    /// Delete a curated relationship (recoverable unless `permanent`). The
    /// removal persists across reindexing: indexing re-creates only what the
    /// sources state, never curated relationships.
    pub fn delete_relation(
        &self,
        origin: &Origin,
        id: &RelationId,
        permanent: bool,
    ) -> Result<Option<String>, OpError> {
        let check = self.check_relation_delete(id)?;
        let (delta, recoverable) = self
            .with_store(|store| {
                let w = Writer::new(store.transaction()?);
                let Some(relation) = w.relation(id)? else {
                    return Ok(Err(refuse("ops.notFound", "That relationship no longer exists.")));
                };
                let recoverable = if permanent {
                    None
                } else {
                    let rid = recoverable_id(RECOVERABLE_RELATION, id.as_str());
                    let snapshot = Snapshot { relations: vec![relation], ..Default::default() };
                    put_recoverable(&w, &rid, RECOVERABLE_RELATION, &check.description, &snapshot)?;
                    Some(rid)
                };
                w.delete_relation(id)?;
                let mut delta = GraphDelta { relations_removed: vec![id.clone()], ..Default::default() };
                w.record_change(origin, &format!("Delete relationship: {}", check.description), &mut delta)?;
                w.commit()?;
                Ok(Ok((delta, recoverable)))
            })
            .map_err(from_graph)??;
        self.emit_changed(delta, origin.clone());
        Ok(recoverable)
    }

    // --------------------------------------------------------------- views

    /// Delete a saved view (presentation only; recoverable unless `permanent`).
    pub fn delete_view_recoverable(&self, id: &ViewId, permanent: bool) -> Result<Option<String>, OpError> {
        self.with_store(|store| {
            let Some(view) = store.view(id)? else {
                return Ok(Err(refuse("ops.notFound", "That saved view no longer exists.")));
            };
            let w = Writer::new(store.transaction()?);
            let recoverable = if permanent {
                None
            } else {
                let rid = recoverable_id(RECOVERABLE_VIEW, id.as_str());
                let name = view.name.clone();
                put_recoverable(
                    &w,
                    &rid,
                    RECOVERABLE_VIEW,
                    &name,
                    &Snapshot { view: Some(view), ..Default::default() },
                )?;
                Some(rid)
            };
            w.tx.execute("DELETE FROM views WHERE id = ?1", [id.as_str()])?;
            w.commit()?;
            Ok(Ok(recoverable))
        })
        .map_err(from_graph)?
    }

    /// Duplicate a saved view under a fresh id ("… (copy)").
    pub fn duplicate_view(&self, id: &ViewId) -> Result<GraphView, OpError> {
        self.with_store(|store| {
            let Some(view) = store.view(id)? else {
                return Ok(Err(refuse("ops.notFound", "That saved view no longer exists.")));
            };
            let copy = GraphView {
                id: ViewId::mint(),
                name: copy_label(&view.name, 200),
                revision: 1,
                updated_at_ms: now_ms(),
                ..view
            };
            store.put_view(&copy)?;
            Ok(Ok(copy))
        })
        .map_err(from_graph)?
    }

    // ----------------------------------------------------------- workflows

    /// Whether a saved workflow was removed from active use.
    pub fn workflow_retired(&self, id: &WorkflowId) -> bool {
        self.with_store(|store| Ok(is_retired(store.connection(), id)?)).unwrap_or(false)
    }

    /// Revisions of `id` that other saved workflows' composite steps pin
    /// (any of their stored revisions: kept by default).
    pub fn workflow_pins(&self, id: &WorkflowId) -> Result<Vec<WorkflowPin>, OpError> {
        self.with_store(|store| Ok(pins_of(store.connection(), id)?)).map_err(from_graph)
    }

    /// What deleting `id` would do.
    pub fn check_workflow_delete(&self, id: &WorkflowId) -> Result<WorkflowDeleteCheck, OpError> {
        self.with_store(|store| {
            let conn = store.connection();
            let Some((latest, _, _, definition)) = stored_revision(conn, id, None)? else {
                return Ok(Err(refuse("ops.notFound", "That workflow no longer exists in this project.")));
            };
            let revisions: Vec<u64> = revisions(conn, id)?.into_iter().map(|r| r.revision).collect();
            let recoverable = conn
                .query_row(
                    "SELECT 1 FROM recoverable WHERE id = ?1",
                    [recoverable_id(RECOVERABLE_WORKFLOW, id.as_str())],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            Ok(Ok(WorkflowDeleteCheck {
                id: id.clone(),
                title: definition.title.clone(),
                latest,
                revisions,
                pins: pins_of(conn, id)?,
                retired: is_retired(conn, id)?,
                recoverable,
            }))
        })
        .map_err(from_graph)?
    }

    /// Remove a saved workflow from active use: it is no longer listed,
    /// planned, triggered or revised, and its graph entity is removed. Its
    /// revisions stay (composites that pin them keep resolving). Ordinary
    /// deletion is recoverable; `permanent` also removes every revision no
    /// surviving composite pins — and the pinned ones too when
    /// `purge_pinned` (after the user's PinnedRevision decision).
    pub fn delete_workflow(
        &self,
        origin: &Origin,
        id: &WorkflowId,
        permanent: bool,
        purge_pinned: bool,
    ) -> Result<WorkflowDeletion, OpError> {
        let (delta, outcome) = self
            .with_store(|store| {
                let conn = store.connection();
                let Some((_, _, _, definition)) = stored_revision(conn, id, None)? else {
                    return Ok(Err(refuse("ops.notFound", "That workflow no longer exists in this project.")));
                };
                let retired = is_retired(conn, id)?;
                if retired && !permanent {
                    return Ok(Err(refuse(
                        "ops.alreadyDeleted",
                        "That workflow is already deleted; restore it or delete it permanently in Storage & History.",
                    )));
                }
                let all: Vec<u64> = revisions(conn, id)?.into_iter().map(|r| r.revision).collect();
                let pinned: BTreeSet<u64> = pins_of(conn, id)?.into_iter().map(|p| p.revision).collect();
                let entity = store
                    .entities_by_key(EntityKind::Workflow, id.as_str())?
                    .into_iter()
                    .find(|e| e.lifecycle != Lifecycle::Tombstoned);
                let relations = match &entity {
                    Some(e) => {
                        let mut r = store.relations_from(&e.id)?;
                        r.extend(store.relations_to(&e.id)?);
                        r
                    }
                    None => Vec::new(),
                };
                let rid = recoverable_id(RECOVERABLE_WORKFLOW, id.as_str());
                let w = Writer::new(store.transaction()?);
                let mut outcome = WorkflowDeletion::default();
                if !retired {
                    w.tx.execute(
                        "INSERT OR IGNORE INTO retired_workflows (id, retired_at) VALUES (?1, ?2)",
                        params![id.as_str(), now_ms() as i64],
                    )?;
                }
                if !permanent {
                    let snapshot = Snapshot {
                        entities: entity.iter().cloned().collect(),
                        relations,
                        workflow: Some(id.clone()),
                        ..Default::default()
                    };
                    put_recoverable(&w, &rid, RECOVERABLE_WORKFLOW, &definition.title, &snapshot)?;
                    outcome.recoverable = Some(rid.clone());
                }
                let mut delta = GraphDelta::default();
                if let Some(e) = &entity {
                    let removed = remove_entity_rows(&w, &e.id)?;
                    delta.entities_removed.push(e.id.clone());
                    mark(&mut delta.relations_removed, removed);
                }
                if permanent {
                    for rev in &all {
                        if pinned.contains(rev) && !purge_pinned {
                            outcome.kept.push(*rev);
                        } else {
                            w.tx.execute(
                                "DELETE FROM workflows WHERE id = ?1 AND revision = ?2",
                                params![id.as_str(), *rev as i64],
                            )?;
                            outcome.purged.push(*rev);
                        }
                    }
                    w.tx.execute("DELETE FROM recoverable WHERE id = ?1", [rid.as_str()])?;
                    if outcome.kept.is_empty() {
                        w.tx.execute("DELETE FROM retired_workflows WHERE id = ?1", [id.as_str()])?;
                    }
                }
                let verb = if permanent { "Delete permanently" } else { "Delete" };
                w.record_change(origin, &format!("{verb} workflow {}", quoted(&definition.title)), &mut delta)?;
                w.commit()?;
                Ok(Ok((delta, outcome)))
            })
            .map_err(from_graph)??;
        self.emit_changed(delta, origin.clone());
        Ok(outcome)
    }

    /// Whether one revision can be purged: never the latest revision of a
    /// workflow in active use (delete the workflow instead).
    pub fn check_revision_purge(&self, id: &WorkflowId, revision: u64) -> Result<Vec<WorkflowPin>, OpError> {
        self.with_store(|store| {
            let conn = store.connection();
            let Some((latest, _, _, _)) = stored_revision(conn, id, None)? else {
                return Ok(Err(refuse("ops.notFound", "That workflow no longer exists in this project.")));
            };
            if stored_revision(conn, id, Some(revision))?.is_none() {
                return Ok(Err(refuse("ops.notFound", format!("Revision {revision} no longer exists."))));
            }
            if revision == latest && !is_retired(conn, id)? {
                return Ok(Err(refuse(
                    "ops.latestRevision",
                    "This is the workflow's current revision; delete the workflow instead.",
                )));
            }
            Ok(Ok(pins_of(conn, id)?.into_iter().filter(|p| p.revision == revision).collect()))
        })
        .map_err(from_graph)?
    }

    /// Purge one stored revision for good (composites that pin it then show
    /// it missing and cannot run). A deleted workflow whose last revision
    /// goes is forgotten entirely (its recoverable snapshot too).
    pub fn purge_workflow_revision(&self, origin: &Origin, id: &WorkflowId, revision: u64) -> Result<(), OpError> {
        self.check_revision_purge(id, revision)?;
        let delta = self
            .with_store(|store| {
                let w = Writer::new(store.transaction()?);
                w.tx.execute(
                    "DELETE FROM workflows WHERE id = ?1 AND revision = ?2",
                    params![id.as_str(), revision as i64],
                )?;
                let left: i64 =
                    w.tx.query_row("SELECT count(*) FROM workflows WHERE id = ?1", [id.as_str()], |r| r.get(0))?;
                if left == 0 {
                    w.tx.execute("DELETE FROM retired_workflows WHERE id = ?1", [id.as_str()])?;
                    w.tx.execute(
                        "DELETE FROM recoverable WHERE id = ?1",
                        [recoverable_id(RECOVERABLE_WORKFLOW, id.as_str())],
                    )?;
                }
                let mut delta = GraphDelta::default();
                w.record_change(origin, &format!("Delete workflow revision {revision} of {id}"), &mut delta)?;
                w.commit()?;
                Ok(delta)
            })
            .map_err(from_graph)?;
        self.emit_changed(delta, origin.clone());
        Ok(())
    }

    /// Duplicate a saved workflow revision (the latest when `revision` is
    /// none) as a NEW workflow: fresh id, revision 1, fresh step ids with
    /// every internal connection, condition and outcome remapped, external
    /// inputs and composite pins kept explicit, title "… (copy)". Nothing
    /// else is carried: no triggers, runs, approvals, verdicts, trust, or
    /// "imported" mark; nothing runs.
    pub fn duplicate_workflow(
        &self,
        origin: &Origin,
        id: &WorkflowId,
        revision: Option<u64>,
        context: &ValidationContext<'_>,
    ) -> Result<SavedWorkflow, OpError> {
        let definition = self
            .with_store(|store| {
                let conn = store.connection();
                if is_retired(conn, id)? {
                    return Ok(None);
                }
                Ok(stored_revision(conn, id, revision)?.map(|(_, _, _, d)| d))
            })
            .map_err(from_graph)?
            .ok_or_else(|| refuse("ops.notFound", "That workflow (revision) no longer exists in this project."))?;
        let copy = remap_workflow(&definition);
        let request = SaveWorkflowRequest { id: None, expected_revision: None, definition: copy };
        let context = ValidationContext {
            ceiling: context.ceiling,
            file_exists: context.file_exists,
            resolve_workflow: context.resolve_workflow,
            workflow_id: None,
        };
        self.save_definition(origin, &request, &context, false).map_err(from_graph)
    }

    // ------------------------------------------------------- recoverables

    /// App-managed recoverable deletions of graph-held records, newest first.
    pub fn recoverables(&self) -> Result<Vec<RecoverableRecord>, OpError> {
        self.with_store(|store| {
            let mut stmt = store.connection().prepare(
                "SELECT id, kind, label, length(body), deleted_at FROM recoverable ORDER BY deleted_at DESC, id LIMIT 20000",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(RecoverableRecord {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    label: r.get(2)?,
                    bytes: r.get::<_, i64>(3)?.max(0) as u64,
                    deleted_at_ms: r.get::<_, i64>(4)?.max(0) as u64,
                })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::from)?)
        })
        .map_err(from_graph)
    }

    /// Restore an app-managed deletion with its original identity. Nothing
    /// is overwritten: an id or relationship that exists again is refused or
    /// skipped (reported). A restored workflow's triggers stay disabled.
    pub fn restore_recoverable(&self, origin: &Origin, id: &str) -> Result<RestoredRecord, OpError> {
        let (delta, restored, rebind) = self
            .with_store(|store| {
                let Some((kind, _label, snapshot)) = load_snapshot(store.connection(), id)? else {
                    return Ok(Err(refuse("ops.notFound", "That deleted item is no longer recoverable.")));
                };
                let w = Writer::new(store.transaction()?);
                let mut delta = GraphDelta::default();
                let mut skipped = 0usize;
                let mut rebind: Vec<WorkspacePath> = Vec::new();
                let target = match kind.as_str() {
                    RECOVERABLE_ENTITY => {
                        let Some(entity) = snapshot.entities.first().cloned() else {
                            return Ok(Err(refuse("ops.notFound", "The deleted item's record is empty.")));
                        };
                        if w.entity(&entity.id)?.is_some() {
                            return Ok(Err(refuse("ops.restoreConflict", "An item with this identity exists again.")));
                        }
                        w.put_entity(&Entity { updated_at_ms: now_ms(), ..entity.clone() })?;
                        let bindings: Vec<SourceBinding> =
                            snapshot.bindings.iter().filter(|b| b.entity == entity.id).cloned().collect();
                        if !bindings.is_empty() {
                            w.set_bindings(&entity.id, &bindings)?;
                            rebind.extend(bindings.iter().map(|b| b.path.clone()));
                        }
                        delta.entities_added.push(entity.id.clone());
                        skipped = restore_relations(&w, &snapshot.relations, &mut delta)?;
                        OpTarget::GraphEntity { entity: entity.id.to_string() }
                    }
                    RECOVERABLE_RELATION => {
                        let Some(relation) = snapshot.relations.first() else {
                            return Ok(Err(refuse("ops.notFound", "The deleted relationship's record is empty.")));
                        };
                        if restore_relations(&w, std::slice::from_ref(relation), &mut delta)? > 0 {
                            return Ok(Err(refuse(
                                "ops.restoreConflict",
                                "The relationship cannot be restored: an item it connects no longer exists, or the same relationship exists again.",
                            )));
                        }
                        OpTarget::GraphRelation { relation: relation.id.to_string() }
                    }
                    RECOVERABLE_VIEW => {
                        let Some(mut view) = snapshot.view.clone() else {
                            return Ok(Err(refuse("ops.notFound", "The deleted view's record is empty.")));
                        };
                        let taken: bool = w
                            .tx
                            .query_row("SELECT 1 FROM views WHERE id = ?1", [view.id.as_str()], |_| Ok(()))
                            .optional()?
                            .is_some();
                        if taken {
                            view.id = ViewId::mint();
                        }
                        view.updated_at_ms = now_ms();
                        let body = serde_json::to_string(&view)?;
                        w.tx.execute(
                            "INSERT INTO views (id, name, scope, body, revision, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![
                                view.id.as_str(),
                                view.name,
                                serde_json::to_string(&view.scope)?,
                                body,
                                view.revision as i64,
                                view.updated_at_ms as i64
                            ],
                        )?;
                        OpTarget::GraphView { view: view.id.to_string() }
                    }
                    RECOVERABLE_WORKFLOW => {
                        let Some(wf) = snapshot.workflow.clone() else {
                            return Ok(Err(refuse("ops.notFound", "The deleted workflow's record is empty.")));
                        };
                        let Some((latest, hash, state, definition)) = stored_revision(&w.tx, &wf, None)? else {
                            return Ok(Err(refuse("ops.notFound", "The workflow's revisions were purged; it cannot be restored.")));
                        };
                        w.tx.execute("DELETE FROM retired_workflows WHERE id = ?1", [wf.as_str()])?;
                        let existing: Option<String> = w
                            .tx
                            .query_row(
                                "SELECT id FROM entities WHERE kind = 'workflow' AND key = ?1 AND lifecycle != 'tombstoned' LIMIT 1",
                                [wf.as_str()],
                                |r| r.get(0),
                            )
                            .optional()?;
                        if existing.is_none()
                            && let Some(mut entity) = snapshot.entities.first().cloned()
                            && w.entity(&entity.id)?.is_none()
                        {
                            entity.attributes.insert("workflow.revision".into(), latest.to_string());
                            entity.attributes.insert("workflow.hash".into(), hash);
                            entity.attributes.insert("workflow.state".into(), state.as_str().to_owned());
                            entity.attributes.insert("workflow.runnable".into(), "false".into());
                            entity.label = crate::store::truncate(definition.title.trim(), 200);
                            if crate::validate::check_label(&entity.label).is_err() {
                                entity.label = "Untitled workflow".into();
                            }
                            entity.lifecycle = Lifecycle::Active;
                            entity.updated_at_ms = now_ms();
                            w.put_entity(&entity)?;
                            delta.entities_added.push(entity.id.clone());
                            skipped = restore_relations(&w, &snapshot.relations, &mut delta)?;
                        }
                        OpTarget::Workflow { workflow: wf.to_string() }
                    }
                    other => {
                        return Ok(Err(refuse("ops.notSupported", format!("Unknown recoverable kind {other}."))));
                    }
                };
                w.tx.execute("DELETE FROM recoverable WHERE id = ?1", [id])?;
                if kind != RECOVERABLE_VIEW {
                    w.record_change(origin, &format!("Restore {kind}"), &mut delta)?;
                }
                w.commit()?;
                Ok(Ok((delta, RestoredRecord { target, skipped_relations: skipped }, rebind)))
            })
            .map_err(from_graph)??;
        self.emit_changed(delta, origin.clone());
        if !rebind.is_empty() {
            // Region bindings are resolved against the current text again.
            let _ = self.reconcile(&crate::reconcile::ReconcileOptions {
                scope: crate::reconcile::Scope::Paths(rebind),
                force: true,
            });
        }
        Ok(restored)
    }

    /// Purge an app-managed deletion for good. A deleted workflow loses its
    /// revisions except those surviving composites pin (unless
    /// `purge_pinned`). Returns the bytes of the removed snapshot.
    pub fn purge_recoverable(
        &self,
        origin: &Origin,
        id: &str,
        purge_pinned: bool,
    ) -> Result<WorkflowDeletion, OpError> {
        let Some((kind, record)) = split_recoverable(id) else {
            return Err(refuse("ops.notFound", "That deleted item is no longer recoverable."));
        };
        if kind == RECOVERABLE_WORKFLOW {
            let wf = WorkflowId::parse(record).map_err(|_| refuse("ops.notFound", "Unknown workflow."))?;
            return self.delete_workflow(origin, &wf, true, purge_pinned);
        }
        let removed = self
            .with_store(|store| Ok(store.connection().execute("DELETE FROM recoverable WHERE id = ?1", [id])?))
            .map_err(from_graph)?;
        if removed == 0 {
            return Err(refuse("ops.notFound", "That deleted item is no longer recoverable."));
        }
        Ok(WorkflowDeletion::default())
    }

    // ----------------------------------------------------- duplicate items

    /// Whether `id` has an independent-copy contract: a manually curated
    /// item not anchored in a source. Source-anchored statements, proofs and
    /// sections are refused (`ops.sourceAnchored`: copy their reference or
    /// text instead); files duplicate as files (`ops.useSourceFile`).
    pub fn check_entity_duplicate(&self, id: &EntityId) -> Result<Entity, OpError> {
        self.with_store(|store| {
            let Some(entity) = live(store, id)? else {
                return Ok(Err(refuse("ops.notFound", "That item no longer exists in the project graph.")));
            };
            Ok(duplicable(store, &entity)?.map(|()| entity))
        })
        .map_err(from_graph)?
    }

    /// Duplicate a selection of curated items in one validated change:
    /// relationships entirely within the selection are remapped onto the
    /// copies; of the relationships leaving the selection only accepted
    /// inputs/references (`dependsOn`, `assumes`, `references`,
    /// `derivedFrom`) are kept; nothing is attached to existing consumers;
    /// evidence and verification are never copied. Returns (original, copy).
    pub fn duplicate_entities(&self, origin: &Origin, ids: &[EntityId]) -> Result<Vec<(EntityId, EntityId)>, OpError> {
        const KEEP_OUTGOING: [RelationKind; 4] =
            [RelationKind::DependsOn, RelationKind::Assumes, RelationKind::References, RelationKind::DerivedFrom];
        let (base, operations, temps) = self
            .with_store(|store| {
                let mut entities = Vec::new();
                for id in ids {
                    let Some(e) = live(store, id)? else {
                        return Ok(Err(refuse("ops.notFound", "That item no longer exists in the project graph.")));
                    };
                    if let Err(err) = duplicable(store, &e)? {
                        return Ok(Err(err));
                    }
                    entities.push(e);
                }
                let selected: BTreeSet<EntityId> = entities.iter().map(|e| e.id.clone()).collect();
                let mut temps: BTreeMap<EntityId, String> = BTreeMap::new();
                let mut operations = Vec::new();
                for (i, e) in entities.iter().enumerate() {
                    let temp = format!("c{i}");
                    temps.insert(e.id.clone(), temp.clone());
                    let mut attributes = e.attributes.clone();
                    if attributes.len() < 32 || attributes.contains_key(COPY_FROM_ENTITY) {
                        attributes.insert(COPY_FROM_ENTITY.into(), e.id.to_string());
                    }
                    operations.push(GraphOperation::CreateEntity {
                        temp_id: temp,
                        kind: e.kind,
                        roles: Vec::new(),
                        label: copy_label(&e.label, 200),
                        key: None,
                        attributes,
                        binding: None,
                    });
                }
                let mut seen = BTreeSet::new();
                for e in &entities {
                    for r in store.relations_from(&e.id)? {
                        if r.status != AssertionStatus::Accepted
                            || !r.kind.is_manual()
                            || r.kind == RelationKind::EvidenceFor
                            || !seen.insert(r.id.clone())
                        {
                            continue;
                        }
                        let to = if selected.contains(&r.to) {
                            EntityRef::Temp { temp_id: temps[&r.to].clone() }
                        } else if KEEP_OUTGOING.contains(&r.kind) && live(store, &r.to)?.is_some() {
                            EntityRef::Id { id: r.to.clone() }
                        } else {
                            continue;
                        };
                        operations.push(GraphOperation::AddRelation {
                            kind: r.kind,
                            from: EntityRef::Temp { temp_id: temps[&r.from].clone() },
                            to,
                            evidence_kind: None,
                            evidence: Vec::new(),
                            note: r.note.clone(),
                        });
                    }
                }
                Ok(Ok((store.revision()?, operations, temps)))
            })
            .map_err(from_graph)??;
        let request = ChangeRequest {
            base_revision: base,
            operations,
            summary: Some(format!("Duplicate {} item(s)", ids.len())),
        };
        let (proposal, outcome) = self.propose_and_apply(origin.clone(), &request).map_err(from_graph)?;
        match outcome {
            crate::ops::ApplyOutcome::Applied { created, .. } => {
                let by_temp: BTreeMap<String, EntityId> = created.into_iter().map(|c| (c.temp_id, c.id)).collect();
                Ok(temps
                    .into_iter()
                    .filter_map(|(old, temp)| by_temp.get(&temp).map(|new| (old, new.clone())))
                    .collect())
            }
            crate::ops::ApplyOutcome::Conflict { diagnostics, .. }
            | crate::ops::ApplyOutcome::Invalid { diagnostics, .. } => {
                let reasons: Vec<String> =
                    diagnostics.iter().chain(proposal.diagnostics.iter()).map(|d| d.message.clone()).take(3).collect();
                Err(refuse("ops.invalid", format!("The copy could not be made: {}", reasons.join("; "))))
            }
        }
    }

    // ---------------------------------------------------------- path impacts

    /// Concrete consequences of deleting these project files or folders for
    /// what survives them: manuscripts that use them, saved workflows whose
    /// inputs they are, and retained evidence that records them while a
    /// surviving item still relies on it. Never graph membership,
    /// containment, citations of their contents, or provenance alone.
    pub fn path_impacts(&self, targets: &[WorkspacePath]) -> Result<Vec<OpImpact>, OpError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        self.with_store(|store| Ok(path_impacts(store, targets)?)).map_err(from_graph)
    }

    // ---------------------------------------------------- copy provenance

    /// After files were copied (`(source, copy)` pairs; folders cover
    /// everything beneath): index the copies now, then record copied-from
    /// provenance on each copy's own fresh identity (attributes
    /// [`COPY_FROM`] / [`COPY_FROM_ENTITY`]). Nothing else is carried over:
    /// evidence, verification and relationships belong to the original.
    pub fn record_copies(&self, pairs: &[(WorkspacePath, WorkspacePath)]) -> Result<usize, OpError> {
        if pairs.is_empty() {
            return Ok(0);
        }
        let copies: Vec<WorkspacePath> = pairs.iter().map(|(_, c)| c.clone()).collect();
        self.reconcile(&crate::reconcile::ReconcileOptions {
            scope: crate::reconcile::Scope::Paths(copies.clone()),
            force: false,
        })
        .map_err(from_graph)?;
        let entities = self
            .with_store(|store| {
                let mut out = Vec::new();
                for (source, copy) in pairs {
                    for key in store.resource_keys(Some(std::slice::from_ref(copy)))? {
                        if key.tombstoned || out.len() >= 2000 {
                            continue;
                        }
                        let Some(suffix) = key.path.as_str().get(copy.as_str().len()..) else { continue };
                        let Ok(from) = WorkspacePath::parse(&format!("{}{suffix}", source.as_str())) else { continue };
                        let Some(mut e) = store.entity(&key.id)? else { continue };
                        let original = store
                            .entities_by_key(EntityKind::Resource, from.as_str())?
                            .into_iter()
                            .find(|o| o.lifecycle != Lifecycle::Tombstoned && o.id != e.id);
                        if e.attributes.len() >= 31 && !e.attributes.contains_key(COPY_FROM) {
                            continue;
                        }
                        e.attributes.insert(COPY_FROM.into(), from.as_str().to_owned());
                        if let Some(o) = original {
                            e.attributes.insert(COPY_FROM_ENTITY.into(), o.id.to_string());
                        }
                        out.push(e);
                    }
                }
                Ok(out)
            })
            .map_err(from_graph)?;
        let n = entities.len();
        for chunk in entities.chunks(400) {
            let batch = ServiceBatch {
                entities: chunk
                    .iter()
                    .map(|e| EntityUpsert {
                        id: Some(e.id.clone()),
                        kind: e.kind,
                        roles: e.roles.clone(),
                        label: e.label.clone(),
                        key: e.key.clone(),
                        attributes: e.attributes.clone(),
                        lifecycle: None,
                        tombstone_reason: None,
                        bindings: None,
                    })
                    .collect(),
                summary: "Record copied-from provenance".into(),
                ..Default::default()
            };
            self.apply_service_batch(&service_origin(), &batch).map_err(from_graph)?;
        }
        Ok(n)
    }

    // ------------------------------------------------- storage & history

    /// Every stored workflow revision with its size, whether it is the
    /// current revision of a workflow in active use, and who pins it.
    pub fn workflow_revision_sizes(&self) -> Result<Vec<RevisionSize>, OpError> {
        self.with_store(|store| {
            let conn = store.connection();
            let rows: Vec<(String, i64, i64, i64)> = {
                let mut stmt = conn.prepare(
                    "SELECT id, revision, length(body), saved_at FROM workflows ORDER BY id, revision DESC LIMIT 20000",
                )?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?
            };
            let mut out = Vec::new();
            let mut latest: BTreeMap<String, i64> = BTreeMap::new();
            for (id, rev, _, _) in &rows {
                let e = latest.entry(id.clone()).or_insert(*rev);
                *e = (*e).max(*rev);
            }
            let mut pins: BTreeMap<String, Vec<WorkflowPin>> = BTreeMap::new();
            for (id, rev, bytes, saved) in rows {
                let Ok(wf) = WorkflowId::parse(&id) else { continue };
                let retired = is_retired(conn, &wf)?;
                let title = stored_revision(conn, &wf, Some(rev.max(0) as u64))?
                    .map(|(_, _, _, d)| d.title)
                    .unwrap_or_default();
                if !pins.contains_key(&id) {
                    pins.insert(id.clone(), pins_of(conn, &wf)?);
                }
                let pinned_by: Vec<String> =
                    pins[&id].iter().filter(|p| p.revision == rev.max(0) as u64).map(|p| p.by_title.clone()).collect();
                out.push(RevisionSize {
                    workflow: wf,
                    title,
                    revision: rev.max(0) as u64,
                    bytes: bytes.max(0) as u64,
                    saved_at_ms: saved.max(0) as u64,
                    current: !retired && latest.get(&id) == Some(&rev),
                    retired,
                    pinned_by,
                });
            }
            Ok(out)
        })
        .map_err(from_graph)
    }

    /// Retained evidence recorded from one service operation (e.g. a
    /// research run), with its live subject when it has one.
    pub fn evidence_for_operation(&self, kind: &str, id: &str) -> Result<Vec<(EvidenceId, Option<Entity>)>, OpError> {
        self.with_store(|store| {
            let mut stmt =
                store.connection().prepare("SELECT body FROM evidence WHERE instr(body, ?1) > 0 LIMIT 2000")?;
            let bodies: Vec<String> = stmt.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?;
            let mut out = Vec::new();
            for body in bodies {
                let e: crate::model::Evidence = serde_json::from_str(&body)?;
                if e.operation.kind == kind && e.operation.id == id {
                    let subject = match &e.subject {
                        Some(s) => live(store, s)?,
                        None => None,
                    };
                    out.push((e.id, subject));
                }
            }
            Ok(out)
        })
        .map_err(from_graph)
    }

    // ------------------------------------------------------------ tombstones

    /// Tombstoned identities nothing refers to any more (no relationship, no
    /// evidence about them): only metadata a cleanup may remove.
    pub fn unreferenced_tombstones(&self, under: Option<&[WorkspacePath]>) -> Result<Vec<EntityId>, OpError> {
        self.with_store(|store| Ok(unreferenced_tombstones(store, under)?)).map_err(from_graph)
    }

    /// Remove unreferenced tombstones (see [`Self::unreferenced_tombstones`]);
    /// serialized with reconciliation passes. Returns how many were removed.
    pub fn cleanup_tombstones(&self, under: Option<&[WorkspacePath]>) -> Result<usize, OpError> {
        let _pass = self.reconcile_state();
        let delta = self
            .with_store(|store| {
                let ids = unreferenced_tombstones(store, under)?;
                if ids.is_empty() {
                    return Ok(None);
                }
                let w = Writer::new(store.transaction()?);
                let mut delta = GraphDelta::default();
                for id in &ids {
                    remove_entity_rows(&w, id)?;
                    if delta.entities_removed.len() < 500 {
                        delta.entities_removed.push(id.clone());
                    } else {
                        delta.truncated = true;
                    }
                }
                w.record_change(&service_origin(), &format!("Clean up {} removed item(s)", ids.len()), &mut delta)?;
                w.commit()?;
                Ok(Some((ids.len(), delta)))
            })
            .map_err(from_graph)?;
        Ok(match delta {
            Some((n, delta)) => {
                self.emit_changed(delta, service_origin());
                n
            }
            None => 0,
        })
    }
}

/// Deletion rules for an entity (`Err` = refused, with where it belongs).
fn deletable(store: &GraphStore, entity: &Entity) -> Result<Result<(), OpError>, StoreError> {
    let tombstoned = entity.lifecycle == Lifecycle::Tombstoned;
    Ok(match entity.kind {
        EntityKind::Project | EntityKind::ResearchSpace => Err(refuse(
            "ops.protected",
            format!("{} is part of the project's structure and cannot be deleted here.", quoted(&entity.label)),
        )),
        EntityKind::Workflow => Err(refuse(
            "ops.useWorkflow",
            format!(
                "{} is a saved workflow: delete it from the workflow list ({}).",
                quoted(&entity.label),
                entity.attributes.get("workflow.id").cloned().unwrap_or_default()
            ),
        )),
        kind if kind.is_file_backed() && !tombstoned => Err(refuse(
            "ops.useSourceFile",
            format!(
                "{} is the file {}: delete the source file itself (it would reappear when the project is indexed again).",
                quoted(&entity.label),
                entity.key.clone().unwrap_or_default()
            ),
        )),
        _ if !tombstoned && source_derived(store, entity)? => {
            let path = store.bindings(&entity.id)?.first().map(|b| b.path.to_string()).unwrap_or_default();
            Err(refuse(
                "ops.sourceDerived",
                format!("{} is declared in {path}; edit the source to remove it.", quoted(&entity.label)),
            ))
        }
        _ => Ok(()),
    })
}

/// Declared in a source (extracted, or bound to a declaration): it follows
/// its source.
fn source_derived(store: &GraphStore, entity: &Entity) -> Result<bool, StoreError> {
    if matches!(entity.origin, Origin::Extracted { .. }) || entity.kind == EntityKind::FormalDeclaration {
        return Ok(true);
    }
    Ok(store.bindings(&entity.id)?.iter().any(|b| matches!(b.selector, SourceSelector::Declaration { .. })))
}

/// The independent-copy contract (`Err` = refused, with what to do instead).
fn duplicable(store: &GraphStore, entity: &Entity) -> Result<Result<(), OpError>, StoreError> {
    use EntityKind::*;
    Ok(match entity.kind {
        Resource | Folder => {
            Err(refuse("ops.useSourceFile", format!("{} is a file: duplicate the file itself.", quoted(&entity.label))))
        }
        Workflow => Err(refuse(
            "ops.useWorkflow",
            format!("{} is a saved workflow: duplicate it from the workflow list.", quoted(&entity.label)),
        )),
        Project | ResearchSpace => Err(refuse("ops.protected", "The project's structure cannot be duplicated.")),
        ExternalResource | Reference => Err(refuse(
            "ops.sourceAnchored",
            format!(
                "{} identifies one source; a second card for it would be a duplicate identity. Copy its reference instead.",
                quoted(&entity.label)
            ),
        )),
        _ if source_derived(store, entity)? || !store.bindings(&entity.id)?.is_empty() => Err(refuse(
            "ops.sourceAnchored",
            format!(
                "{} is anchored in its source; duplicating it would need a source edit. Use Copy Reference or Copy Text instead.",
                quoted(&entity.label)
            ),
        )),
        _ => Ok(()),
    })
}

fn consumer_entity(e: &Entity) -> OpConsumer {
    OpConsumer {
        kind: if e.kind == EntityKind::Resource { "document".into() } else { "entity".into() },
        id: e.id.to_string(),
        label: e.label.clone(),
        path: e.key.as_deref().filter(|_| e.kind == EntityKind::Resource).and_then(|k| WorkspacePath::parse(k).ok()),
    }
}

fn names(consumers: &[OpConsumer]) -> String {
    let mut shown: Vec<String> = consumers.iter().take(3).map(|c| quoted(&c.label)).collect();
    if consumers.len() > 3 {
        shown.push(format!("{} more", consumers.len() - 3));
    }
    match shown.len() {
        0 => String::new(),
        1 => shown.remove(0),
        _ => {
            let last = shown.pop().unwrap_or_default();
            format!("{} and {last}", shown.join(", "))
        }
    }
}

/// Re-insert relationships whose endpoints both exist (live) and that do not
/// exist again; returns how many were skipped.
fn restore_relations(w: &Writer<'_>, relations: &[Relation], delta: &mut GraphDelta) -> Result<usize, StoreError> {
    let mut skipped = 0;
    for r in relations {
        let ok_end = |id: &EntityId| -> Result<bool, StoreError> {
            Ok(w.entity(id)?.is_some_and(|e| e.lifecycle != Lifecycle::Tombstoned))
        };
        let taken = w.relation(&r.id)?.is_some() || w.find_relation(r.kind, &r.from, &r.to)?.is_some();
        let contained = r.kind == RelationKind::Contains && w.container_of(&r.to)?.is_some();
        if taken || contained || !ok_end(&r.from)? || !ok_end(&r.to)? {
            skipped += 1;
            continue;
        }
        w.put_relation(r)?;
        if delta.relations_added.len() < 500 {
            delta.relations_added.push(r.id.clone());
        }
    }
    Ok(skipped)
}

/// Pins of `id`'s revisions by any stored revision of another workflow.
fn pins_of(conn: &rusqlite::Connection, id: &WorkflowId) -> Result<Vec<WorkflowPin>, StoreError> {
    let candidates: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT id, revision FROM workflows WHERE id != ?1 AND instr(body, ?1) > 0 ORDER BY id, revision LIMIT 20000",
        )?;
        stmt.query_map([id.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
    };
    let mut pins = Vec::new();
    for (other, revision) in candidates {
        let Ok(other) = WorkflowId::parse(&other) else { continue };
        let Some((rev, _, _, definition)) = stored_revision(conn, &other, Some(revision.max(0) as u64))? else {
            continue;
        };
        for step in &definition.steps {
            if let Some(c) = &step.composite
                && &c.workflow == id
                && !pins.iter().any(|p: &WorkflowPin| p.revision == c.revision && p.by == other && p.by_revision == rev)
            {
                pins.push(WorkflowPin {
                    revision: c.revision,
                    by: other.clone(),
                    by_title: definition.title.clone(),
                    by_revision: rev,
                });
            }
        }
    }
    Ok(pins)
}

/// The copy of a definition: fresh step ids, every reference remapped.
fn remap_workflow(definition: &WorkflowDefinition) -> WorkflowDefinition {
    use crate::workflow::{PortSource, WorkflowConnection};
    let mut copy = definition.clone();
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    let mut used: BTreeSet<String> = definition.steps.iter().map(|s| s.id.clone()).collect();
    for step in &definition.steps {
        let prefix: String =
            step.id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(24).collect();
        let prefix = if prefix.is_empty() { "step".to_owned() } else { prefix };
        let fresh = loop {
            let tag = uuid::Uuid::new_v4().simple().to_string();
            let candidate = format!("{prefix}_{}", &tag[..6]);
            if used.insert(candidate.clone()) {
                break candidate;
            }
        };
        map.insert(step.id.clone(), fresh);
    }
    let re = |id: &mut String| {
        if let Some(n) = map.get(id.as_str()) {
            *id = n.clone();
        }
    };
    for step in &mut copy.steps {
        re(&mut step.id);
        if let Some(c) = &mut step.condition {
            re(&mut c.source.step);
        }
    }
    for c in &mut copy.connections {
        match c {
            WorkflowConnection::Data { from, to } => {
                if let PortSource::Step { step, .. } = from {
                    re(step);
                }
                re(&mut to.step);
            }
            WorkflowConnection::Prerequisite { before, after } => {
                re(before);
                re(after);
            }
            WorkflowConnection::Annotation { from, to, .. } => {
                re(from);
                re(to);
            }
        }
    }
    for o in &mut copy.outcomes {
        re(&mut o.from.step);
    }
    copy.title = copy_label(&definition.title, crate::workflow::MAX_TITLE);
    copy
}

fn unreferenced_tombstones(store: &GraphStore, under: Option<&[WorkspacePath]>) -> Result<Vec<EntityId>, StoreError> {
    let mut stmt = store.connection().prepare(
        "SELECT e.id, e.key FROM entities e WHERE e.lifecycle = 'tombstoned'
         AND NOT EXISTS (SELECT 1 FROM relations r WHERE r.from_id = e.id OR r.to_id = e.id)
         AND NOT EXISTS (SELECT 1 FROM evidence v WHERE v.subject = e.id)
         LIMIT 20000",
    )?;
    let rows: Vec<(String, Option<String>)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for (id, key) in rows {
        let Ok(id) = EntityId::parse(&id) else { continue };
        if let Some(prefixes) = under {
            let by_key = key
                .as_deref()
                .and_then(|k| WorkspacePath::parse(k).ok())
                .is_some_and(|p| under_any(&p, prefixes).is_some());
            let by_binding = store.bindings(&id)?.iter().any(|b| under_any(&b.path, prefixes).is_some());
            if !by_key && !by_binding {
                continue;
            }
        }
        out.push(id);
    }
    Ok(out)
}

// --------------------------------------------------------- path impacts ----

fn path_impacts(store: &GraphStore, targets: &[WorkspacePath]) -> Result<Vec<OpImpact>, StoreError> {
    // Everything that goes with the targets: their live resources and the
    // items declared in them.
    let mut gone: BTreeSet<EntityId> = BTreeSet::new();
    let mut resources: Vec<(usize, EntityId, WorkspacePath)> = Vec::new();
    for key in store.resource_keys(Some(targets))? {
        let Some(t) = under_any(&key.path, targets) else { continue };
        if key.tombstoned {
            continue;
        }
        gone.insert(key.id.clone());
        for b in store.bindings_at_path(&key.path)? {
            gone.insert(b.entity);
        }
        resources.push((t, key.id, key.path));
    }
    let mut per_target: BTreeMap<(usize, ImpactKind), Vec<OpConsumer>> = BTreeMap::new();
    let mut what: BTreeMap<(usize, ImpactKind), BTreeSet<String>> = BTreeMap::new();

    // Manuscripts that use them (includes, inputs, bibliographies, images,
    // attached artifacts).
    for (t, id, path) in &resources {
        for r in store.relations_to(id)? {
            if !matches!(r.kind, RelationKind::Imports | RelationKind::UsesAsset)
                || r.status == AssertionStatus::Rejected
                || gone.contains(&r.from)
            {
                continue;
            }
            let Some(user) = live(store, &r.from)? else { continue };
            if user.kind != EntityKind::Resource || !user.roles.contains(&ResourceRole::Manuscript) {
                continue;
            }
            per_target.entry((*t, ImpactKind::ManuscriptDependency)).or_default().push(consumer_entity(&user));
            what.entry((*t, ImpactKind::ManuscriptDependency)).or_default().insert(path.file_name().to_owned());
        }
    }

    // Saved workflows whose inputs they are (latest revision of every
    // workflow in active use).
    {
        let mut stmt = store.connection().prepare(
            "SELECT DISTINCT id FROM workflows WHERE id NOT IN (SELECT id FROM retired_workflows) LIMIT 20000",
        )?;
        let ids: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for id in ids {
            let Ok(wf) = WorkflowId::parse(&id) else { continue };
            let Some((_, _, _, definition)) = stored_revision(store.connection(), &wf, None)? else { continue };
            for input in &definition.inputs {
                let crate::workflow::InputSource::Resource { path, .. } = &input.source else { continue };
                let Some(t) = under_any(path, targets) else { continue };
                per_target.entry((t, ImpactKind::WorkflowInput)).or_default().push(OpConsumer {
                    kind: "workflow".into(),
                    id: wf.to_string(),
                    label: definition.title.clone(),
                    path: Some(path.clone()),
                });
                what.entry((t, ImpactKind::WorkflowInput)).or_default().insert(format!(
                    "{} ({})",
                    input.label,
                    path.file_name()
                ));
            }
        }
    }

    // Retained evidence that records them and that a surviving item still
    // relies on (its subject, an item it supports, or a reviewed formal
    // association of its subject).
    let mut evidence: BTreeMap<EvidenceId, crate::model::Evidence> = BTreeMap::new();
    {
        let mut stmt = store.connection().prepare(
            "SELECT body FROM evidence WHERE kind IN ('computational', 'formalCheck') AND instr(lower(body), lower(?1)) > 0 LIMIT 20000",
        )?;
        for t in targets {
            for body in stmt.query_map([t.as_str()], |r| r.get::<_, String>(0))? {
                let e: crate::model::Evidence = serde_json::from_str(&body?)
                    .map_err(|e| StoreError::Untrusted(format!("unreadable evidence record: {e}")))?;
                evidence.insert(e.id.clone(), e);
            }
        }
    }
    if !evidence.is_empty() {
        let supports = store.relations_of_kind(RelationKind::EvidenceFor)?;
        let formalizes = store.relations_of_kind(RelationKind::Formalizes)?;
        for e in evidence.values() {
            if !matches!(e.kind, EvidenceKind::Computational | EvidenceKind::FormalCheck) {
                continue;
            }
            let recorded = e
                .inputs
                .iter()
                .map(|i| &i.path)
                .chain(e.outputs.iter().map(|o| &o.path))
                .chain(e.formal.iter().map(|f| &f.source_path))
                .find_map(|p| under_any(p, targets).map(|t| (t, p.clone())));
            let Some((t, path)) = recorded else { continue };
            let mut users: Vec<Entity> = Vec::new();
            if let Some(subject) = &e.subject
                && !gone.contains(subject)
                && let Some(s) = live(store, subject)?
            {
                users.push(s);
            }
            for r in &supports {
                if r.evidence.contains(&e.id) {
                    for end in [&r.from, &r.to] {
                        if !gone.contains(end)
                            && let Some(s) = live(store, end)?
                        {
                            users.push(s);
                        }
                    }
                }
            }
            if let Some(subject) = &e.subject {
                for r in formalizes.iter().filter(|r| &r.from == subject && r.status == AssertionStatus::Accepted) {
                    if !gone.contains(&r.to)
                        && let Some(s) = live(store, &r.to)?
                    {
                        users.push(s);
                    }
                }
            }
            if users.is_empty() {
                continue;
            }
            for u in users {
                per_target.entry((t, ImpactKind::RetainedEvidence)).or_default().push(OpConsumer {
                    kind: "evidence".into(),
                    id: e.id.to_string(),
                    label: format!("{} evidence for {}", evidence_title(e.kind), quoted(&u.label)),
                    path: u
                        .key
                        .as_deref()
                        .filter(|_| u.kind == EntityKind::Resource)
                        .and_then(|k| WorkspacePath::parse(k).ok()),
                });
            }
            what.entry((t, ImpactKind::RetainedEvidence)).or_default().insert(path.file_name().to_owned());
        }
    }

    let mut out = Vec::new();
    for ((t, kind), mut consumers) in per_target {
        consumers.sort_by(|a, b| {
            (a.kind.as_str(), a.id.as_str(), a.label.as_str()).cmp(&(b.kind.as_str(), b.id.as_str(), b.label.as_str()))
        });
        consumers.dedup();
        let files: Vec<String> = what.remove(&(t, kind)).unwrap_or_default().into_iter().collect();
        let files_text = files.iter().take(3).map(|f| quoted(f)).collect::<Vec<_>>().join(", ");
        let message = match kind {
            ImpactKind::ManuscriptDependency => format!(
                "{} uses {files_text}. After deletion the manuscript shows it missing until you restore or replace it.",
                names(&consumers)
            ),
            ImpactKind::WorkflowInput => format!(
                "{} takes {files_text} as an input; it cannot run until you choose another file.",
                names(&consumers)
            ),
            _ => format!(
                "Retained evidence records {files_text} ({}). The evidence becomes unavailable for reproduction; its recorded verdicts stay as history.",
                names(&consumers)
            ),
        };
        out.push(OpImpact { kind, target: OpTarget::Path { path: targets[t].clone() }, consumers, message });
    }
    Ok(out)
}

fn evidence_title(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Computational => "Computational",
        EvidenceKind::FormalCheck => "Formal",
        EvidenceKind::Build => "Build",
        EvidenceKind::Review => "Review",
        EvidenceKind::Literature => "Literature",
    }
}
