//! Saved workflow definitions: immutable revisions in the project graph's
//! `workflows` table, admitted as `Workflow` entities.
//!
//! Saving is a deliberate admission: one transaction inserts the new revision
//! (never updating an old one) and creates or updates the workflow's graph
//! entity, which references the definition id, revision, and hash. Editing
//! creates a new revision; identical content creates none. Drafts are saved
//! with their diagnostics; a stored runnability claim is never trusted (the
//! workflow runner service decides it for the open project). Stored bodies
//! are read back as untrusted data (bounded, strictly decoded).

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    DraftState, ResolveWorkflow, ValidationContext, WorkflowDefinition, WorkflowValidation, definition_hash,
    parse_definition, validate_workflow,
};
use crate::ids::{EntityId, WorkflowId};
use crate::model::{Entity, GraphDelta, Lifecycle, Origin};
use crate::service::{GraphError, GraphResult, GraphService};
use crate::store::{StoreError, Writer, now_ms};
use crate::vocabulary::EntityKind;

/// Bodies larger than this are treated as hostile.
const MAX_BODY: usize = 4 * 1024 * 1024;
const MAX_LIST: usize = 500;

/// Save a draft: a new workflow (`id` none) or a new revision of one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveWorkflowRequest {
    pub id: Option<WorkflowId>,
    /// The revision the editor started from; a newer saved revision is a conflict.
    pub expected_revision: Option<u64>,
    pub definition: WorkflowDefinition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRevisionInfo {
    pub revision: u64,
    pub hash: String,
    pub state: DraftState,
    pub saved_at_ms: u64,
}

/// One saved revision with its diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SavedWorkflow {
    pub id: WorkflowId,
    pub revision: u64,
    /// SHA-256 of the definition content.
    pub hash: String,
    /// The `Workflow` graph entity referencing it.
    pub entity: Option<EntityId>,
    pub definition: WorkflowDefinition,
    /// Diagnostics as validated when saved; `runnable` is false here (never
    /// read from storage) until the runner service applies availability.
    pub validation: WorkflowValidation,
    pub origin: Origin,
    /// Imported from outside (never trusted, never auto-run).
    pub imported: bool,
    pub saved_at_ms: u64,
    /// The content equalled the latest revision; nothing new was saved.
    pub unchanged: bool,
    /// Every revision, newest first (bounded).
    pub revisions: Vec<WorkflowRevisionInfo>,
    pub graph_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSummary {
    pub id: WorkflowId,
    pub revision: u64,
    pub title: String,
    pub preset: Option<String>,
    pub state: DraftState,
    pub runnable: bool,
    pub errors: u32,
    pub warnings: u32,
    pub entity: Option<EntityId>,
    pub origin: Origin,
    pub imported: bool,
    pub saved_at_ms: u64,
}

/// What a row's `body` holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredBody {
    definition: WorkflowDefinition,
    validation: WorkflowValidation,
    origin: Origin,
    imported: bool,
}

struct Row {
    revision: u64,
    hash: String,
    state: DraftState,
    body: StoredBody,
    saved_at_ms: u64,
}

fn untrusted(what: &str) -> StoreError {
    StoreError::Untrusted(format!("invalid workflow {what}"))
}

fn decode_row(revision: i64, hash: String, state: String, body: String, saved_at: i64) -> Result<Row, StoreError> {
    if body.len() > MAX_BODY || hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(untrusted("record"));
    }
    let body: StoredBody =
        serde_json::from_str(&body).map_err(|e| StoreError::Untrusted(format!("unreadable workflow record: {e}")))?;
    Ok(Row {
        revision: u64::try_from(revision).map_err(|_| untrusted("revision"))?,
        hash,
        state: DraftState::parse(&state).ok_or_else(|| untrusted("state"))?,
        body,
        saved_at_ms: saved_at.max(0) as u64,
    })
}

fn latest_revision(conn: &Connection, id: &WorkflowId) -> Result<Option<(u64, String)>, StoreError> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT revision, hash FROM workflows WHERE id = ?1 ORDER BY revision DESC LIMIT 1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(r, h)| (r.max(0) as u64, h)))
}

fn load_row(conn: &Connection, id: &WorkflowId, revision: Option<u64>) -> Result<Option<Row>, StoreError> {
    let raw: Option<(i64, String, String, String, i64)> = match revision {
        Some(rev) => conn
            .query_row(
                "SELECT revision, hash, state, body, saved_at FROM workflows WHERE id = ?1 AND revision = ?2",
                params![id.as_str(), rev as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?,
        None => conn
            .query_row(
                "SELECT revision, hash, state, body, saved_at FROM workflows WHERE id = ?1 ORDER BY revision DESC LIMIT 1",
                [id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?,
    };
    raw.map(|(rev, hash, state, body, saved)| decode_row(rev, hash, state, body, saved)).transpose()
}

/// One stored revision's definition, hash, and state (strictly decoded).
pub(crate) fn stored_revision(
    conn: &Connection,
    id: &WorkflowId,
    revision: Option<u64>,
) -> Result<Option<(u64, String, DraftState, WorkflowDefinition)>, StoreError> {
    Ok(load_row(conn, id, revision)?.map(|r| (r.revision, r.hash, r.state, r.body.definition)))
}

pub(crate) fn revisions(conn: &Connection, id: &WorkflowId) -> Result<Vec<WorkflowRevisionInfo>, StoreError> {
    let mut stmt = conn.prepare_cached(
        "SELECT revision, hash, state, saved_at FROM workflows WHERE id = ?1 ORDER BY revision DESC LIMIT 1000",
    )?;
    let rows = stmt.query_map([id.as_str()], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (revision, hash, state, saved) = row?;
        out.push(WorkflowRevisionInfo {
            revision: revision.max(0) as u64,
            hash,
            state: DraftState::parse(&state).ok_or_else(|| untrusted("state"))?,
            saved_at_ms: saved.max(0) as u64,
        });
    }
    Ok(out)
}

/// Whether a saved workflow was removed from active use (P1 project
/// operations): it is not listed, planned, triggered, or revised; its pinned
/// revisions stay readable for the composites that run them.
pub(crate) fn is_retired(conn: &Connection, id: &WorkflowId) -> Result<bool, StoreError> {
    Ok(conn.query_row("SELECT 1 FROM retired_workflows WHERE id = ?1", [id.as_str()], |_| Ok(())).optional()?.is_some())
}

fn workflow_entity(store_entities: Vec<Entity>) -> Option<Entity> {
    store_entities.into_iter().find(|e| e.lifecycle != Lifecycle::Tombstoned)
}

fn entity_attributes(
    id: &WorkflowId,
    revision: u64,
    hash: &str,
    definition: &WorkflowDefinition,
    validation: &WorkflowValidation,
    imported: bool,
) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    a.insert("workflow.id".to_owned(), id.to_string());
    a.insert("workflow.revision".to_owned(), revision.to_string());
    a.insert("workflow.hash".to_owned(), hash.to_owned());
    a.insert("workflow.state".to_owned(), validation.state.as_str().to_owned());
    a.insert("workflow.runnable".to_owned(), "false".to_owned());
    if let Some(p) = &definition.preset {
        a.insert("workflow.preset".to_owned(), crate::store::truncate(p, 64));
    }
    if imported {
        a.insert("workflow.imported".to_owned(), "true".to_owned());
    }
    a
}

fn label_for(definition: &WorkflowDefinition) -> String {
    let title = definition.title.trim();
    if crate::validate::check_label(title).is_ok() { title.to_owned() } else { "Untitled workflow".to_owned() }
}

impl GraphService {
    /// Validate and save a workflow draft as a new immutable revision, and
    /// admit (or update) its `Workflow` entity — one graph transaction.
    /// Incomplete drafts are saved with their diagnostics; nothing is marked
    /// runnable here. Identical content to the latest revision saves nothing.
    pub fn save_workflow(
        &self,
        origin: &Origin,
        request: &SaveWorkflowRequest,
        context: &ValidationContext<'_>,
    ) -> GraphResult<SavedWorkflow> {
        self.save_definition(origin, request, context, false)
    }

    /// Import a definition from untrusted JSON text: strict schema and size
    /// limits, a fresh workflow id (never the one in the file), no trust, and
    /// never run.
    pub fn import_workflow(
        &self,
        origin: &Origin,
        text: &str,
        context: &ValidationContext<'_>,
    ) -> GraphResult<SavedWorkflow> {
        let definition = parse_definition(text).map_err(|m| GraphError::new("workflow.invalid", m))?;
        let request = SaveWorkflowRequest { id: None, expected_revision: None, definition };
        self.save_definition(origin, &request, context, true)
    }

    pub(crate) fn save_definition(
        &self,
        origin: &Origin,
        request: &SaveWorkflowRequest,
        context: &ValidationContext<'_>,
        imported: bool,
    ) -> GraphResult<SavedWorkflow> {
        let size = serde_json::to_vec(&request.definition).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > super::MAX_DEFINITION_BYTES {
            return Err(GraphError::new("workflow.tooLarge", "the workflow definition is too large"));
        }
        let definition = &request.definition;
        // Composite steps resolve against this project's saved revisions
        // (unless the caller supplies its own resolver), and a workflow may
        // not include itself. Saving never touches the children.
        let from_store = |id: &WorkflowId, revision: u64| self.workflow_definition(id, revision);
        let resolve: &ResolveWorkflow<'_> = match context.resolve_workflow {
            Some(r) => r,
            None => &from_store,
        };
        let context = ValidationContext {
            ceiling: context.ceiling,
            file_exists: context.file_exists,
            resolve_workflow: Some(resolve),
            workflow_id: context.workflow_id.or(request.id.as_ref()),
        };
        let validation = validate_workflow(definition, &context);
        let hash = definition_hash(definition);
        let (saved, delta) = self.with_store(|store| {
            let (id, revision, previous) = match &request.id {
                Some(id) => {
                    if is_retired(store.connection(), id)? {
                        return Err(GraphError::new("workflow.unknown", "That workflow was deleted from this project."));
                    }
                    let Some((latest, latest_hash)) = latest_revision(store.connection(), id)? else {
                        return Err(GraphError::new("workflow.unknown", "That workflow does not exist in this project."));
                    };
                    if let Some(expected) = request.expected_revision
                        && expected != latest
                    {
                        return Err(GraphError::new(
                            "workflow.stale",
                            format!(
                                "The workflow was saved again since you opened it (revision {expected} → {latest}). Reopen it to continue."
                            ),
                        ));
                    }
                    if latest_hash == hash {
                        return Ok((None, None));
                    }
                    (id.clone(), latest + 1, Some(latest))
                }
                None => (WorkflowId::mint(), 1, None),
            };
            let existing = workflow_entity(store.entities_by_key(EntityKind::Workflow, id.as_str())?);
            let now = now_ms();
            let body = StoredBody {
                definition: definition.clone(),
                validation: validation.clone(),
                origin: origin.clone(),
                imported: imported || (previous.is_some() && self.was_imported(store.connection(), &id)),
            };
            let w = Writer::new(store.transaction()?);
            w.tx.execute(
                "INSERT INTO workflows (id, revision, hash, state, body, saved_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.as_str(),
                    revision as i64,
                    hash,
                    validation.state.as_str(),
                    serde_json::to_string(&body).map_err(StoreError::from)?,
                    now as i64
                ],
            )
            .map_err(StoreError::from)?;
            let attributes = entity_attributes(&id, revision, &hash, definition, &validation, body.imported);
            let mut delta = GraphDelta::default();
            let entity = match existing {
                Some(mut e) => {
                    e.label = label_for(definition);
                    e.attributes = attributes;
                    e.lifecycle = Lifecycle::Active;
                    e.revision += 1;
                    e.updated_at_ms = now;
                    w.put_entity(&e)?;
                    delta.entities_updated.push(e.id.clone());
                    e
                }
                None => {
                    let e = Entity {
                        id: EntityId::mint(),
                        kind: EntityKind::Workflow,
                        roles: Vec::new(),
                        label: label_for(definition),
                        key: Some(id.to_string()),
                        lifecycle: Lifecycle::Active,
                        tombstone_reason: None,
                        revision: 1,
                        origin: origin.clone(),
                        attributes,
                        created_at_ms: now,
                        updated_at_ms: now,
                    };
                    w.put_entity(&e)?;
                    delta.entities_added.push(e.id.clone());
                    e
                }
            };
            let summary = format!(
                "Save workflow draft “{}” (revision {revision}{})",
                entity.label,
                if validation.state == DraftState::Incomplete { ", incomplete" } else { "" }
            );
            w.record_change(origin, &summary, &mut delta)?;
            w.commit()?;
            Ok((Some((id, revision)), Some(delta)))
        })?;
        if let Some(delta) = delta {
            self.emit_changed(delta, origin.clone());
        }
        match saved {
            Some((id, revision)) => {
                let mut out = self.workflow(&id, Some(revision))?;
                out.unchanged = false;
                Ok(out)
            }
            None => {
                let id = request.id.clone().ok_or_else(|| GraphError::new("workflow.unknown", "missing id"))?;
                let mut out = self.workflow(&id, None)?;
                out.unchanged = true;
                Ok(out)
            }
        }
    }

    fn was_imported(&self, conn: &Connection, id: &WorkflowId) -> bool {
        load_row(conn, id, Some(1)).ok().flatten().is_some_and(|r| r.body.imported)
    }

    /// The definition of one saved revision, or `None` when it does not exist
    /// or cannot be read (the resolver of composite steps).
    pub fn workflow_definition(&self, id: &WorkflowId, revision: u64) -> Option<WorkflowDefinition> {
        self.with_store(|store| Ok(load_row(store.connection(), id, Some(revision))?.map(|r| r.body.definition)))
            .ok()
            .flatten()
    }

    /// One saved revision (the latest when `revision` is none).
    pub fn workflow(&self, id: &WorkflowId, revision: Option<u64>) -> GraphResult<SavedWorkflow> {
        self.with_store(|store| {
            if is_retired(store.connection(), id)? {
                return Err(GraphError::new("workflow.unknown", "That workflow was deleted from this project."));
            }
            let row = load_row(store.connection(), id, revision)?
                .ok_or_else(|| GraphError::new("workflow.unknown", "That workflow (revision) does not exist."))?;
            let entity = workflow_entity(store.entities_by_key(EntityKind::Workflow, id.as_str())?).map(|e| e.id);
            let mut validation = row.body.validation;
            // Never trust a stored claim of runnability.
            validation.runnable = false;
            validation.runnable_reason = super::NOT_RUNNABLE_REASON.to_owned();
            Ok(SavedWorkflow {
                id: id.clone(),
                revision: row.revision,
                hash: row.hash,
                entity,
                definition: row.body.definition,
                validation,
                origin: row.body.origin,
                imported: row.body.imported,
                saved_at_ms: row.saved_at_ms,
                unchanged: false,
                revisions: revisions(store.connection(), id)?,
                graph_revision: store.revision()?,
            })
        })
    }

    /// Saved workflows of the open project (latest revision of each).
    pub fn workflows(&self) -> GraphResult<Vec<WorkflowSummary>> {
        self.with_store(|store| {
            let conn = store.connection();
            let mut stmt = conn
                .prepare_cached(
                    "SELECT w.id, w.revision, w.hash, w.state, w.body, w.saved_at FROM workflows w
                     WHERE w.revision = (SELECT MAX(revision) FROM workflows v WHERE v.id = w.id)
                     AND w.id NOT IN (SELECT id FROM retired_workflows)
                     ORDER BY w.saved_at DESC, w.id LIMIT ?1",
                )
                .map_err(StoreError::from)?;
            let rows = stmt
                .query_map([MAX_LIST as i64], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                })
                .map_err(StoreError::from)?;
            let mut out = Vec::new();
            for row in rows {
                let (id, rev, hash, state, body, saved) = row.map_err(StoreError::from)?;
                let id = WorkflowId::parse(&id).map_err(|_| untrusted("id"))?;
                let row = decode_row(rev, hash, state, body, saved)?;
                let entity = workflow_entity(store.entities_by_key(EntityKind::Workflow, id.as_str())?).map(|e| e.id);
                out.push(WorkflowSummary {
                    id,
                    revision: row.revision,
                    title: row.body.definition.title.clone(),
                    preset: row.body.definition.preset.clone(),
                    state: row.state,
                    runnable: false,
                    errors: row.body.validation.errors,
                    warnings: row.body.validation.warnings,
                    entity,
                    origin: row.body.origin,
                    imported: row.body.imported,
                    saved_at_ms: row.saved_at_ms,
                });
            }
            Ok(out)
        })
    }
}
