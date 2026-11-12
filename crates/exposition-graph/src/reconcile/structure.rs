//! Folder entities and `contains` structure.
//!
//! A folder is admitted while it holds admitted resources (application
//! metadata folders — `.exposition/`, `.exposition/research/`, generated
//! artifact folders — are not). Every resource and folder is contained by its
//! nearest admitted folder, the research space whose folder it is in, or the
//! project. Only observed (extracted) containment is managed here; a
//! containment asserted by a preset or service is left as it is. A folder
//! whose resources all moved together under a new name keeps its identity.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use exposition_core::path::WorkspacePath;

use super::engine::{Planner, SCAN_EXTRACTOR};
use crate::ids::EntityId;
use crate::model::{BindingState, Entity, Lifecycle, Origin, SourceBinding, SourceSelector};
use crate::store::StoreResult;
use crate::vocabulary::{EntityKind, RelationKind};

/// Folders that are application metadata, never project folders.
fn structural(dir: &WorkspacePath) -> bool {
    let p = dir.as_str();
    !(p == ".exposition"
        || p == ".exposition/research"
        || p == ".exposition/artifacts"
        || p.starts_with(".exposition/artifacts/"))
}

impl Planner<'_> {
    pub(crate) fn plan_structure(&mut self) -> StoreResult<()> {
        let Some(project) = self.store.find_entities(Some(&[EntityKind::Project]), None, true, 1)?.0.into_iter().next()
        else {
            return Ok(());
        };
        let mut spaces: BTreeMap<WorkspacePath, EntityId> = BTreeMap::new();
        for e in self.store.all_of_kind(EntityKind::ResearchSpace)? {
            if e.lifecycle == Lifecycle::Tombstoned {
                continue;
            }
            if let Some(p) = e.key.as_deref().and_then(|k| WorkspacePath::parse(k).ok()) {
                spaces.insert(p, e.id);
            }
        }
        let mut folders: BTreeMap<WorkspacePath, Entity> = BTreeMap::new();
        let mut folder_tombs: HashMap<WorkspacePath, Vec<Entity>> = HashMap::new();
        for e in self.store.all_of_kind(EntityKind::Folder)? {
            let e = self.entity(&e.id)?.unwrap_or(e);
            let Some(p) = e.key.as_deref().and_then(|k| WorkspacePath::parse(k).ok()) else { continue };
            if e.lifecycle == Lifecycle::Tombstoned {
                folder_tombs.entry(p).or_default().push(e);
            } else {
                folders.entry(p).or_insert(e);
            }
        }
        // Folders that must exist: ancestors of live resources.
        let mut required: BTreeSet<WorkspacePath> = BTreeSet::new();
        for path in self.resources.keys() {
            let mut dir = path.parent();
            while let Some(d) = dir {
                if structural(&d) && !spaces.contains_key(&d) {
                    required.insert(d.clone());
                }
                dir = d.parent();
            }
        }
        // Folder renames implied by resource renames (all agree on one target).
        let mut mapping: BTreeMap<WorkspacePath, BTreeSet<WorkspacePath>> = BTreeMap::new();
        for (old, new) in &self.renames {
            let (mut o, mut n) = (old.clone(), new.clone());
            while o.file_name() == n.file_name() {
                match (o.parent(), n.parent()) {
                    (Some(op), Some(np)) => {
                        mapping.entry(op.clone()).or_default().insert(np.clone());
                        o = op;
                        n = np;
                    }
                    _ => break,
                }
            }
        }
        for (old, targets) in mapping {
            if targets.len() != 1 || required.contains(&old) {
                continue;
            }
            let Some(new) = targets.into_iter().next() else { continue };
            if !required.contains(&new) || folders.contains_key(&new) {
                continue;
            }
            let Some(entity) = folders.remove(&old) else { continue };
            let mut after = entity.clone();
            after.key = Some(new.as_str().to_owned());
            after.label = crate::store::truncate(new.file_name(), 200);
            self.update(&entity, after.clone());
            self.set_bindings(&entity.id, vec![folder_binding(&entity.id, &new)]);
            folders.insert(new, after);
        }
        // Create missing folders (reviving a tombstone at the same path), retire empty ones.
        for dir in &required {
            if folders.contains_key(dir) {
                continue;
            }
            let revived = folder_tombs.get(dir).and_then(|t| (t.len() == 1).then(|| t[0].clone()));
            let entity = match revived {
                Some(t) => {
                    let mut after = t.clone();
                    after.lifecycle = Lifecycle::Active;
                    after.tombstone_reason = None;
                    self.update(&t, after.clone());
                    after
                }
                None => {
                    let e = Entity {
                        id: EntityId::mint(),
                        kind: EntityKind::Folder,
                        roles: Vec::new(),
                        label: crate::store::truncate(dir.file_name(), 200),
                        key: Some(dir.as_str().to_owned()),
                        lifecycle: Lifecycle::Active,
                        tombstone_reason: None,
                        revision: 1,
                        origin: Origin::Extracted { extractor: SCAN_EXTRACTOR.into() },
                        attributes: BTreeMap::new(),
                        created_at_ms: self.now,
                        updated_at_ms: self.now,
                    };
                    self.create(e.clone());
                    e
                }
            };
            self.set_bindings(&entity.id, vec![folder_binding(&entity.id, dir)]);
            folders.insert(dir.clone(), entity);
        }
        let retired: Vec<(WorkspacePath, Entity)> =
            folders.iter().filter(|(p, _)| !required.contains(*p)).map(|(p, e)| (p.clone(), e.clone())).collect();
        for (path, e) in retired {
            let mut after = e.clone();
            after.lifecycle = Lifecycle::Tombstoned;
            after.tombstone_reason = Some(format!("{path} was removed or no longer holds project resources"));
            self.update(&e, after);
            let mut b = folder_binding(&e.id, &path);
            b.state = BindingState::Missing;
            self.set_bindings(&e.id, vec![b]);
            for r in self.store.relations_to(&e.id)? {
                if r.kind == RelationKind::Contains && matches!(r.origin, Origin::Extracted { .. }) {
                    self.unrelate(&r);
                }
            }
            folders.remove(&path);
        }

        // Containment of every live resource and folder.
        let container_of = |path: &WorkspacePath| -> EntityId {
            let mut dir = path.parent();
            while let Some(d) = dir {
                if let Some(space) = spaces.get(&d) {
                    return space.clone();
                }
                if let Some(f) = folders.get(&d) {
                    return f.id.clone();
                }
                dir = d.parent();
            }
            project.id.clone()
        };
        let mut children: Vec<(EntityId, EntityId)> = Vec::new();
        for (path, id) in &self.resources {
            children.push((id.clone(), container_of(path)));
        }
        for (path, e) in &folders {
            children.push((e.id.clone(), container_of(path)));
        }
        let mut contains_to: HashMap<EntityId, Vec<crate::model::Relation>> = HashMap::new();
        for r in self.store.relations_of_kind(RelationKind::Contains)? {
            contains_to.entry(r.to.clone()).or_default().push(r);
        }
        for (child, parent) in children {
            let existing = contains_to.get(&child).cloned().unwrap_or_default();
            let mut satisfied = false;
            let mut curated = false;
            for r in &existing {
                if self.is_removed(&r.id) {
                    continue;
                }
                if !matches!(r.origin, Origin::Extracted { .. }) {
                    curated = true;
                } else if r.from == parent {
                    satisfied = true;
                } else {
                    self.unrelate(r);
                }
            }
            if satisfied || curated {
                continue;
            }
            let (Some(p), Some(c)) = (self.entity(&parent)?, self.entity(&child)?) else { continue };
            self.relate(RelationKind::Contains, &p, &c, SCAN_EXTRACTOR)?;
        }
        Ok(())
    }
}

fn folder_binding(id: &EntityId, path: &WorkspacePath) -> SourceBinding {
    SourceBinding {
        entity: id.clone(),
        path: path.clone(),
        selector: SourceSelector::WholeFile,
        range: None,
        version: None,
        fingerprint: None,
        state: BindingState::Resolved,
        detail: None,
    }
}
