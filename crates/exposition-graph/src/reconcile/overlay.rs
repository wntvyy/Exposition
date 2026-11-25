//! Live-buffer overlay: the unsaved text of an open document, extracted and
//! matched exactly like a saved file but never written.
//!
//! An overlay is labelled with the document revision it was computed from.
//! Queries that name that revision (`inspect_at_source`) see the buffer's
//! declarations, bindings, and observed links; everything else keeps seeing
//! the saved projection. Declarations that exist only in the buffer get
//! provisional identities (`ent-unsaved-…`, attribute `overlay.provisional`)
//! that the validator refuses as targets of changes: they are not graph
//! entities until the file is saved and reconciled. Saving, reverting to the
//! saved text, closing, and discarding drop the overlay (the document
//! service's actual state decides; see the driver).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use exposition_core::path::WorkspacePath;
use sha2::{Digest, Sha256};

use super::engine::{Fresh, Planner, ProjectIndex};
use super::extract;
use super::scan::{FileContent, FileMeta};
use crate::ids::{EntityId, RelationId};
use crate::model::{Entity, Lifecycle, Relation, ResourceCoverage, SourceBinding, SourceSelector, SourceVersion};
use crate::service::{GraphError, GraphResult, GraphService};
use crate::store::now_ms;
use crate::vocabulary::EntityKind;

/// The overlay of one unsaved buffer.
#[derive(Clone, Debug)]
pub struct OverlayEntry {
    pub path: WorkspacePath,
    /// Document revision of the text.
    pub revision: u64,
    /// SHA-256 of the buffer's UTF-8 text.
    pub content_hash: String,
    /// Entities as the buffer states them (updated declarations and
    /// provisional new ones), by id.
    pub entities: HashMap<EntityId, Entity>,
    /// Their bindings (version labelled unsaved).
    pub bindings: HashMap<EntityId, Vec<SourceBinding>>,
    /// Observed relations the buffer adds…
    pub added: Vec<Relation>,
    /// …and saved observed relations it no longer states.
    pub removed: BTreeSet<RelationId>,
    /// Coverage of the buffer's extraction.
    pub coverage: Option<ResourceCoverage>,
    /// Entities that exist only in the buffer.
    pub provisional: BTreeSet<EntityId>,
    pub computed_at_ms: u64,
}

fn hex(bytes: &[u8], n: usize) -> String {
    bytes.iter().take(n).map(|b| format!("{b:02x}")).collect()
}

/// Code of the error [`compute`] returns while a pass holds the session's
/// index of labels and LaTeX main files: nothing was computed or read; ask
/// again once the pass is over (the driver does).
pub const DEFERRED: &str = "graph.overlayDeferred";

/// Whether `error` is [`DEFERRED`].
pub fn is_deferred(error: &GraphError) -> bool {
    error.code == DEFERRED
}

fn deferred() -> GraphError {
    GraphError::new(DEFERRED, "A pass holds the project's index; the overlay is computed when it is over.")
}

/// Compute the overlay of `text` (revision `revision`) for the file `path`.
/// `None` when the file is not a known resource (nothing to overlay).
///
/// The buffer's links resolve against the session's index of labels and
/// LaTeX main files, as a pass resolves them (read from the store when
/// nothing in this session has read it yet). While a pass holds that index
/// the overlay is not computed (error [`DEFERRED`]): resolved against less,
/// it would report links as unresolved that are not; resolved against a
/// copy of the stored index, it would cost every index record of the project
/// instead of the document.
pub fn compute(
    graph: &GraphService,
    path: &WorkspacePath,
    revision: u64,
    text: &str,
) -> GraphResult<Option<OverlayEntry>> {
    // Nothing is read or extracted for an overlay that would be deferred
    // anyway (not even the attached project, under the lock a pass writes in).
    if graph.reconcile_busy() {
        return Err(deferred());
    }
    let root: PathBuf =
        graph.attached_root().ok_or_else(|| GraphError::new("graph.noProject", "No project graph is attached."))?;
    if text.len() > extract::MAX_EXTRACT_BYTES {
        return Ok(None);
    }
    let content_hash = super::scan::sha256_hex(text.as_bytes());
    let extraction = extract::extract(path, text);
    let fresh = Fresh {
        meta: FileMeta { size: text.len() as u64, modified_ms: None, roles: Vec::new() },
        content: FileContent {
            hash: Some(content_hash.clone()),
            identity: None,
            text: Some(text.to_owned()),
            problem: None,
        },
        extraction,
    };
    // Labels and LaTeX main files are looked up in the session's index, as a
    // pass looks them up; a session that has not read it yet (no pass since
    // the project was attached) reads it now.
    let Some(mut state) = graph.try_reconcile_state() else { return Err(deferred()) };
    let now = now_ms();
    graph.with_store_for(&root, |store| {
        let index: &ProjectIndex = state.index_of(&root, store)?;
        let Some(resource) = store
            .entities_by_key(EntityKind::Resource, path.as_str())?
            .into_iter()
            .find(|e| e.lifecycle != Lifecycle::Tombstoned)
        else {
            return Ok(None);
        };
        let mut planner = Planner::new(store, &root, index, now, true);
        planner.plan_overlay(path, &resource, &fresh)?;
        let plan = planner.finish();
        // Entities that do not exist in the saved graph get provisional ids.
        let mut remap: BTreeMap<EntityId, EntityId> = BTreeMap::new();
        for e in &plan.entities {
            if store.entity(&e.id)?.is_none() {
                let selector = plan
                    .bindings
                    .iter()
                    .find(|(id, _)| id == &e.id)
                    .and_then(|(_, b)| b.first().map(|b| b.selector.clone()));
                let seed = format!("{path}\u{1}{:?}\u{1}{:?}\u{1}{selector:?}", e.kind, e.key);
                let digest = Sha256::digest(seed.as_bytes());
                if let Ok(id) = EntityId::parse(&format!("ent-unsaved-{}", hex(&digest, 12))) {
                    remap.insert(e.id.clone(), id);
                }
            }
        }
        let map_id = |id: &EntityId| remap.get(id).cloned().unwrap_or_else(|| id.clone());
        let mut entities = HashMap::new();
        let mut provisional = BTreeSet::new();
        for e in &plan.entities {
            let mut e = e.clone();
            if let Some(new) = remap.get(&e.id) {
                e.id = new.clone();
                e.attributes.insert("overlay.provisional".into(), "true".into());
                provisional.insert(new.clone());
            }
            entities.insert(e.id.clone(), e);
        }
        let mut bindings = HashMap::new();
        for (id, bs) in &plan.bindings {
            let id = map_id(id);
            let bs: Vec<SourceBinding> = bs
                .iter()
                .map(|b| {
                    let mut b = b.clone();
                    b.entity = id.clone();
                    if &b.path == path {
                        b.version = Some(SourceVersion {
                            content_hash: content_hash.clone(),
                            revision: Some(revision),
                            saved: false,
                        });
                        if provisional.contains(&id) && matches!(b.selector, SourceSelector::Declaration { .. }) {
                            b.detail = Some("Unsaved: this declaration is not in the saved file yet.".into());
                        }
                    }
                    b
                })
                .collect();
            bindings.insert(id, bs);
        }
        let added = plan
            .add
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.from = map_id(&r.from);
                r.to = map_id(&r.to);
                let digest = Sha256::digest(format!("{}\u{1}{}\u{1}{}", r.kind.as_str(), r.from, r.to).as_bytes());
                if let Ok(id) = RelationId::parse(&format!("rel-unsaved-{}", hex(&digest, 12))) {
                    r.id = id;
                }
                r
            })
            .collect();
        let coverage = plan.coverage.iter().find(|(c, _)| &c.path == path).map(|(c, _)| c.clone());
        Ok(Some(OverlayEntry {
            path: path.clone(),
            revision,
            content_hash: content_hash.clone(),
            entities,
            bindings,
            added,
            removed: plan.remove.into_iter().collect(),
            coverage,
            provisional,
            computed_at_ms: now,
        }))
    })
}
