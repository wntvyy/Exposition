//! Resolving a file's observed links against the project: labels to the
//! declarations that carry them, citation keys to bibliography entries,
//! written paths to resources (admitting attached artifacts explicitly), Lean
//! modules to files. A link that cannot be resolved is counted and explained
//! in the file's coverage; nothing is guessed.

use std::collections::{BTreeMap, BTreeSet};

use exposition_core::path::WorkspacePath;

use super::engine::{ATTACHMENT_EXTRACTOR, IndexBody, Planner, Stamp};
use super::extract::{self, LinkFrom, LinkTarget, PathBase};
use super::scan;
use crate::admission::{Admission, classify};
use crate::ids::EntityId;
use crate::model::{
    BindingState, CoverageState, Entity, Lifecycle, Origin, Relation, ResourceCoverage, SourceBinding, SourceSelector,
    SourceVersion,
};
use crate::store::StoreResult;
use crate::vocabulary::{EntityKind, RelationKind};

/// Link kinds a resource's extraction owns (replaced on re-extraction).
pub(crate) fn is_link_kind(kind: RelationKind) -> bool {
    matches!(kind, RelationKind::Imports | RelationKind::UsesAsset | RelationKind::References | RelationKind::ProofOf)
}

const IMAGE_EXTENSIONS: &[&str] = &["pdf", "png", "jpg", "jpeg", "eps", "svg", "gif", "webp"];

enum Target {
    Entity(EntityId),
    /// The label exists but names nothing admitted (a section, an equation).
    NotAnEntity,
    Ambiguous(usize),
    Missing,
}

/// The live entities of one name, as a pass leaves them.
enum Named {
    None,
    One(EntityId),
    Many(usize),
}

impl Planner<'_> {
    /// The live entities of the admitted `kinds` whose natural key is `key`,
    /// as this pass leaves them: the one, or how many.
    ///
    /// One read of a key returns a bounded number of rows. When the stored
    /// entities of the key fill it, the rows read are not the whole answer:
    /// the number is then counted (the stored count, less what this pass
    /// tombstoned or renamed, plus what it added), and a single survivor is
    /// looked up page by page, so an incremental pass and a full one report
    /// the same count and neither takes a part for the whole.
    fn named(&self, key: &str, kinds: impl Fn(EntityKind) -> bool) -> StoreResult<Named> {
        let seen = self.keyed_entities(key, &kinds)?;
        let from = |seen: &[Entity]| match seen {
            [] => Named::None,
            [one] => Named::One(one.id.clone()),
            many => Named::Many(many.len()),
        };
        if self.stored_with_key(key)?.len() < crate::store::KEY_READ_LIMIT {
            return Ok(from(&seen));
        }
        let was_stored = |id: &EntityId| -> StoreResult<bool> {
            Ok(self.store.entity(id)?.is_some_and(|e| {
                e.lifecycle != Lifecycle::Tombstoned && e.key.as_deref() == Some(key) && kinds(e.kind)
            }))
        };
        let stored: usize =
            self.store.count_live_entities_with_key(key)?.into_iter().filter(|(k, _)| kinds(*k)).map(|(_, n)| n).sum();
        // Stored ones this pass tombstoned, or gave another key.
        let mut left = 0usize;
        for id in &self.tombstoned_now {
            if self.entities.get(id).is_some_and(|e| e.key.as_deref() == Some(key)) && was_stored(id)? {
                left += 1;
            }
        }
        for id in self.unkeyed.get(key).into_iter().flatten() {
            let renamed = self.entities.get(id).is_some_and(|e| e.key.as_deref() != Some(key));
            if renamed && !self.tombstoned_now.contains(id) && was_stored(id)? {
                left += 1;
            }
        }
        // Planned ones that were not among the stored.
        let mut arrived: Vec<&Entity> = Vec::new();
        for e in &seen {
            if self.keyed.get(key).is_some_and(|ids| ids.contains(&e.id))
                && (self.created_now.contains(&e.id) || !was_stored(&e.id)?)
            {
                arrived.push(e);
            }
        }
        match (stored.saturating_sub(left), arrived.as_slice()) {
            (0, arrived) => Ok(match arrived {
                [] => Named::None,
                [one] => Named::One(one.id.clone()),
                many => Named::Many(many.len()),
            }),
            (1, []) => {
                // The one stored entity this pass left: not necessarily
                // among the rows read first.
                let mut after: Option<EntityId> = None;
                loop {
                    let page = self.store.live_entities_with_key_after(key, after.as_ref())?;
                    let Some(last) = page.last().map(|e| e.id.clone()) else { return Ok(Named::None) };
                    for stored in &page {
                        if self.tombstoned_now.contains(&stored.id) || !kinds(stored.kind) {
                            continue;
                        }
                        let now = self.entity(&stored.id)?;
                        if now.is_some_and(|e| e.lifecycle != Lifecycle::Tombstoned && e.key.as_deref() == Some(key)) {
                            return Ok(Named::One(stored.id.clone()));
                        }
                    }
                    after = Some(last);
                }
            }
            (kept, arrived) => Ok(Named::Many(kept + arrived.len())),
        }
    }

    /// Stored live entities whose natural key is `key` (before this pass):
    /// the rows of one bounded read.
    pub(crate) fn stored_with_key(&self, key: &str) -> StoreResult<Vec<EntityId>> {
        if let Some(ids) = self.key_cache.borrow().get(key) {
            return Ok(ids.clone());
        }
        let ids: Vec<EntityId> = self.store.live_entities_with_key(key)?.into_iter().map(|e| e.id).collect();
        self.key_cache.borrow_mut().insert(key.to_owned(), ids.clone());
        Ok(ids)
    }

    /// The live entities of `key` among the rows read and those planned
    /// (see [`Self::named`] for the whole answer).
    fn keyed_entities(&self, key: &str, kinds: impl Fn(EntityKind) -> bool) -> StoreResult<Vec<Entity>> {
        let mut ids: BTreeSet<EntityId> = BTreeSet::new();
        ids.extend(self.stored_with_key(key)?);
        if let Some(planned) = self.keyed.get(key) {
            ids.extend(planned.iter().cloned());
        }
        let mut out = Vec::new();
        for id in ids {
            if self.tombstoned_now.contains(&id) {
                continue;
            }
            let Some(e) = self.entity(&id)? else { continue };
            if e.lifecycle == Lifecycle::Tombstoned || e.key.as_deref() != Some(key) || !kinds(e.kind) {
                continue;
            }
            out.push(e);
        }
        Ok(out)
    }

    fn label_exists(&self, label: &str, body: &IndexBody) -> StoreResult<bool> {
        if body.extraction.labels.iter().any(|l| l == label) {
            return Ok(true);
        }
        if self.planned_labels.get(label).is_some_and(|paths| !paths.is_empty()) {
            return Ok(true);
        }
        let Some(paths) = self.index.labels.get(label) else { return Ok(false) };
        for p in paths {
            if !self.index_records.contains_key(p) && !self.remove_index.contains(p) && self.resources.get(p)?.is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn resolve_label(&self, name: &str, body: &IndexBody) -> StoreResult<Target> {
        let local: Vec<&EntityId> = body
            .extraction
            .declarations
            .iter()
            .zip(&body.ids)
            .filter(|(d, _)| d.name.as_deref() == Some(name))
            .filter_map(|(_, id)| id.as_ref())
            .collect();
        match local.as_slice() {
            [one] => return Ok(Target::Entity((*one).clone())),
            [] => {}
            many => return Ok(Target::Ambiguous(many.len())),
        }
        let elsewhere = self.named(name, |k| {
            !matches!(k, EntityKind::Resource | EntityKind::Folder | EntityKind::Reference | EntityKind::Project)
        })?;
        match elsewhere {
            Named::One(id) => Ok(Target::Entity(id)),
            Named::None if self.label_exists(name, body)? => Ok(Target::NotAnEntity),
            Named::None => Ok(Target::Missing),
            Named::Many(n) => Ok(Target::Ambiguous(n)),
        }
    }

    fn resolve_citation(&self, key: &str) -> StoreResult<Target> {
        Ok(match self.named(key, |k| k == EntityKind::Reference)? {
            Named::One(id) => Target::Entity(id),
            Named::None => Target::Missing,
            Named::Many(n) => Target::Ambiguous(n),
        })
    }

    /// Where a written path is looked up, in order.
    fn path_lookups(&self, from: &WorkspacePath, written: &str, base: PathBase, extensions: &[String]) -> Vec<Lookup> {
        let mains = self.latex_mains();
        path_lookups(from, written, base, extensions, mains.iter())
    }

    /// The resource for a written path (the first candidate that is one,
    /// with the candidate); admits a retained/attached generated artifact
    /// explicitly when `admit_generated`.
    fn resolve_path(
        &mut self,
        lookups: &[Lookup],
        admit_generated: bool,
    ) -> StoreResult<Option<(EntityId, WorkspacePath)>> {
        for c in lookups.iter().flat_map(|l| &l.candidates) {
            if let Some(id) = self.resources.get(c)? {
                return Ok(Some((id, c.clone())));
            }
        }
        if !admit_generated {
            return Ok(None);
        }
        for c in lookups.iter().flat_map(|l| &l.candidates) {
            let Admission::OnRequest(roles) = classify(c) else { continue };
            let Some(meta) = scan::stat_file(self.root, c) else { continue };
            return Ok(Some((self.admit_generated(c, roles, &meta)?, c.clone())));
        }
        Ok(None)
    }

    /// Admit a generated artifact a manuscript attaches (or revive its
    /// tombstoned identity at the same path).
    fn admit_generated(
        &mut self,
        path: &WorkspacePath,
        roles: Vec<crate::vocabulary::ResourceRole>,
        meta: &scan::FileMeta,
    ) -> StoreResult<EntityId> {
        let content = scan::read_file(self.root, path, meta.size, false);
        let existing = self
            .store
            .entities_by_key(EntityKind::Resource, path.as_str())?
            .into_iter()
            .find(|e| e.lifecycle == Lifecycle::Tombstoned);
        let entity = match existing {
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
                    kind: EntityKind::Resource,
                    roles,
                    label: crate::store::truncate(path.file_name(), 200),
                    key: Some(path.as_str().to_owned()),
                    lifecycle: Lifecycle::Active,
                    tombstone_reason: None,
                    revision: 1,
                    origin: Origin::Extracted { extractor: ATTACHMENT_EXTRACTOR.into() },
                    attributes: BTreeMap::new(),
                    created_at_ms: self.now,
                    updated_at_ms: self.now,
                };
                self.create(e.clone());
                self.report.resources_added += 1;
                e
            }
        };
        self.set_bindings(
            &entity.id,
            vec![SourceBinding {
                entity: entity.id.clone(),
                path: path.clone(),
                selector: SourceSelector::WholeFile,
                range: None,
                version: content.hash.as_ref().map(|h| SourceVersion {
                    content_hash: h.clone(),
                    revision: None,
                    saved: true,
                }),
                fingerprint: None,
                state: BindingState::Resolved,
                detail: None,
            }],
        );
        self.set_coverage(
            ResourceCoverage {
                path: path.clone(),
                extractor: None,
                state: CoverageState::Unsupported,
                unresolved: 0,
                detail: None,
                content_hash: content.hash.clone(),
            },
            Some(Stamp { size: Some(meta.size), modified_ms: meta.modified_ms, identity: content.identity }),
        );
        self.resources.insert(path.clone(), entity.id.clone());
        self.structure_dirty = true;
        Ok(entity.id)
    }

    /// Where a Lean module is looked up: its file below each Lake package root.
    fn lean_lookups(&self, module: &str) -> StoreResult<Vec<Lookup>> {
        if module.is_empty() || module.contains(['/', '\\']) {
            return Ok(Vec::new());
        }
        let rel = format!("{}.lean", module.replace('.', "/"));
        let roots = match &self.lake_roots_cache {
            Some(r) => r.clone(),
            None => self.lake_roots()?,
        };
        Ok(roots
            .into_iter()
            .filter_map(|root| extract::join_relative(root.as_ref(), &rel))
            .map(|c| Lookup { awaited: c.clone(), candidates: vec![c], latex_main: false })
            .collect())
    }

    /// Resolve every observed link of `path` and replace its observed
    /// relations by the result (diffed: unchanged relations keep their ids).
    pub(crate) fn resolve_links(&mut self, path: &WorkspacePath) -> StoreResult<()> {
        let Some(resource_id) = self.resources.get(path)? else { return Ok(()) };
        let Some(resource) = self.entity(&resource_id)? else { return Ok(()) };
        let body: std::sync::Arc<IndexBody> = match self.index_records.get(path) {
            Some(b) => std::sync::Arc::clone(b),
            None => match self.store.source_index(path)? {
                Some(r) => match serde_json::from_str::<IndexBody>(&r.body) {
                    Ok(b) => std::sync::Arc::new(b),
                    Err(_) => return Ok(()),
                },
                None => return Ok(()),
            },
        };
        let extractor = body.extraction.extractor.clone();
        let code = extractor.starts_with("code/");
        let mut sources: Vec<EntityId> = vec![resource_id.clone()];
        sources.extend(body.ids.iter().flatten().cloned());
        let mut existing: Vec<Relation> = Vec::new();
        for id in &sources {
            let stored = if self.created_now.contains(id) { Vec::new() } else { self.store.relations_from(id)? };
            for r in stored {
                if is_link_kind(r.kind) && matches!(r.origin, Origin::Extracted { .. }) && !self.is_removed(&r.id) {
                    existing.push(r);
                }
            }
            for r in self.added_from(id) {
                if is_link_kind(r.kind) {
                    existing.push(r);
                }
            }
        }
        let mut desired: BTreeSet<(RelationKind, EntityId, EntityId)> = BTreeSet::new();
        let mut unresolved: Vec<String> = Vec::new();
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        for link in &body.extraction.links {
            let from = match link.from {
                LinkFrom::Resource => resource_id.clone(),
                LinkFrom::Declaration(i) => body.ids.get(i).cloned().flatten().unwrap_or_else(|| resource_id.clone()),
            };
            let target = match &link.target {
                LinkTarget::Label(x) => self.resolve_label(x, &body)?,
                LinkTarget::Citation(k) => self.resolve_citation(k)?,
                LinkTarget::LabelOrCitation(x) => match self.resolve_label(x, &body)? {
                    Target::Missing => {
                        // A label of this name, once declared, takes the
                        // place of the bibliography entry.
                        wanted.insert(format!("name:{x}"));
                        self.resolve_citation(x)?
                    }
                    other => other,
                },
                LinkTarget::Declaration(i) => match body.ids.get(*i).cloned().flatten() {
                    Some(id) => Target::Entity(id),
                    None => Target::Missing,
                },
                LinkTarget::Path { written, base, extensions } => {
                    let lookups = self.path_lookups(path, written, *base, extensions);
                    let generated = link.kind == RelationKind::UsesAsset;
                    let found = self.resolve_path(&lookups, generated)?;
                    await_lookups(
                        &mut wanted,
                        &lookups,
                        found.as_ref().map(|(_, at)| at),
                        *base == PathBase::LatexMain,
                    );
                    match found {
                        Some((id, _)) => Target::Entity(id),
                        None if code => Target::NotAnEntity,
                        None => {
                            // An existing file that is not a project resource type is not unresolved.
                            let exists = lookups
                                .iter()
                                .flat_map(|l| &l.candidates)
                                .any(|c| scan::stat_file(self.root, c).is_some());
                            if exists { Target::NotAnEntity } else { Target::Missing }
                        }
                    }
                }
                LinkTarget::LeanModule(m) => {
                    let lookups = self.lean_lookups(m)?;
                    let found = self.resolve_path(&lookups, false)?;
                    await_lookups(&mut wanted, &lookups, found.as_ref().map(|(_, at)| at), false);
                    match found {
                        Some((id, _)) => Target::Entity(id),
                        None => Target::NotAnEntity,
                    }
                }
            };
            let described = describe(&link.target);
            // A reference that did not become a relation to the one
            // declaration of its name waits for that name: a declaration or
            // label of it that appears or disappears anywhere changes the
            // outcome. One that did is found again from the relation.
            if !matches!(target, Target::Entity(_))
                && let LinkTarget::Label(x) | LinkTarget::Citation(x) | LinkTarget::LabelOrCitation(x) = &link.target
            {
                wanted.insert(format!("name:{x}"));
            }
            match target {
                Target::Entity(to) => {
                    let (Some(fe), Some(te)) = (self.entity(&from)?, self.entity(&to)?) else { continue };
                    if from == to || !link.kind.allows(fe.kind, te.kind) {
                        // No relation will lead back from the target.
                        if let Some(key) = &te.key {
                            wanted.insert(super::engine::target_key(te.kind, key));
                        }
                    }
                    if from == to {
                        continue;
                    }
                    if !link.kind.allows(fe.kind, te.kind) {
                        unresolved.push(format!(
                            "{described}: a {} cannot {} a {}",
                            fe.kind.title().to_lowercase(),
                            link.kind.reading(),
                            te.kind.title().to_lowercase()
                        ));
                        continue;
                    }
                    desired.insert((link.kind, from, to));
                }
                Target::NotAnEntity => {}
                Target::Ambiguous(n) => unresolved.push(format!("{described} matches {n} declarations")),
                Target::Missing => unresolved.push(format!("{described} not found")),
            }
        }
        for att in &body.extraction.attachments {
            let Some(image) = &att.image else { continue };
            let exts: Vec<String> = IMAGE_EXTENSIONS.iter().map(|e| (*e).to_owned()).collect();
            let lookups = self.path_lookups(path, image, att.base, &exts);
            let found = self.resolve_path(&lookups, true)?;
            await_lookups(&mut wanted, &lookups, found.as_ref().map(|(_, at)| at), att.base == PathBase::LatexMain);
            match found {
                Some((to, _)) if to != resource_id => {
                    desired.insert((RelationKind::UsesAsset, resource_id.clone(), to));
                }
                Some((_, at)) => {
                    wanted.insert(format!("path:{at}"));
                }
                None => {
                    unresolved.push(format!("attachment {} uses “{image}”, which was not found", att.key));
                }
            }
        }
        // Replace the observed set.
        let mut kept: BTreeSet<(RelationKind, EntityId, EntityId)> = BTreeSet::new();
        for r in &existing {
            let key = (r.kind, r.from.clone(), r.to.clone());
            if desired.contains(&key) && !kept.contains(&key) {
                kept.insert(key);
            } else {
                self.unrelate(r);
            }
        }
        for (kind, from, to) in desired {
            if kept.contains(&(kind, from.clone(), to.clone())) {
                continue;
            }
            let (Some(fe), Some(te)) = (self.entity(&from)?, self.entity(&to)?) else { continue };
            let link_extractor = if kind == RelationKind::UsesAsset
                && te.origin == (Origin::Extracted { extractor: ATTACHMENT_EXTRACTOR.into() })
            {
                ATTACHMENT_EXTRACTOR.to_owned()
            } else {
                extractor.clone()
            };
            self.relate(kind, &fe, &te, &link_extractor)?;
        }
        self.finish_coverage(path, &resource, &body, unresolved)?;
        // More targets than a stored list names: wait for any target, so
        // none of them is dropped (the file is resolved again more often).
        let wanted: Vec<String> = if wanted.len() > super::engine::MAX_WANTED {
            vec![super::engine::ANY_TARGET.to_owned()]
        } else {
            wanted.into_iter().collect()
        };
        self.wanted.insert(path.clone(), wanted);
        Ok(())
    }

    /// Coverage of a file after resolution: unresolved references and
    /// bindings counted and explained.
    fn finish_coverage(
        &mut self,
        path: &WorkspacePath,
        resource: &Entity,
        body: &IndexBody,
        unresolved: Vec<String>,
    ) -> StoreResult<()> {
        let (mut coverage, stamp) = match self.base_coverage.get(path) {
            Some((c, s)) => (c.clone(), Some(s.clone())),
            None => match self.store.coverage_stored(path)? {
                Some(c) => (c, None),
                None => return Ok(()),
            },
        };
        let (binding_count, mut binding_reasons) = self.binding_unresolved(path, resource)?;
        if let Some(extra) = self.binding_problems.get(path) {
            binding_reasons.extend(extra.iter().cloned());
        }
        // Names the file declares more than once (none of them is admitted).
        let mut declared: BTreeMap<&str, usize> = BTreeMap::new();
        for d in &body.extraction.declarations {
            if let Some(name) = &d.name {
                *declared.entry(name.as_str()).or_default() += 1;
            }
        }
        for (name, count) in declared {
            if count > 1 {
                binding_reasons.push(format!("“{name}” is declared {count} times"));
            }
        }
        let duplicates = body.ids.iter().filter(|i| i.is_none()).count() as u32;
        coverage.unresolved = unresolved.len() as u32 + binding_count + duplicates;
        if coverage.state != CoverageState::Failed {
            coverage.state =
                if body.extraction.partial.is_empty() { CoverageState::Complete } else { CoverageState::Partial };
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(problem) = self.base_coverage.get(path).and_then(|(c, _)| c.detail.clone()) {
            parts.push(problem);
        }
        if !body.extraction.partial.is_empty() {
            parts.push(format!("Not interpreted: {}", list(&body.extraction.partial, 5)));
        }
        if !unresolved.is_empty() {
            parts.push(format!("Unresolved: {}", list(&unresolved, 6)));
        }
        if !binding_reasons.is_empty() {
            binding_reasons.sort();
            binding_reasons.dedup();
            parts.push(format!("Bindings: {}", list(&binding_reasons, 4)));
        }
        if !body.extraction.notes.is_empty() {
            parts.push(format!("Conventions: {}", list(&body.extraction.notes, 4)));
        }
        let detail = parts.join(". ");
        coverage.detail = if detail.is_empty() { None } else { Some(crate::store::truncate(&detail, 1500)) };
        self.report.unresolved += coverage.unresolved;
        self.set_coverage(coverage, stamp);
        Ok(())
    }

    /// Bindings in `path` (other than the file's own) that are not resolved,
    /// for live entities.
    fn binding_unresolved(&self, path: &WorkspacePath, resource: &Entity) -> StoreResult<(u32, Vec<String>)> {
        let mut ids: BTreeSet<EntityId> = BTreeSet::new();
        for b in self.bindings_at(path)? {
            ids.insert(b.entity);
        }
        ids.extend(self.planned_bound_at(path));
        let mut count = 0u32;
        let mut reasons = Vec::new();
        for id in ids {
            if id == resource.id || self.tombstoned_now.contains(&id) {
                continue;
            }
            let Some(e) = self.entity(&id)? else { continue };
            if e.lifecycle == Lifecycle::Tombstoned {
                continue;
            }
            for b in self.bindings_of(&id)?.iter().filter(|b| &b.path == path) {
                if b.state != BindingState::Resolved {
                    count += 1;
                    if let Some(d) = &b.detail {
                        reasons.push(format!("{}: {d}", e.label));
                    }
                }
            }
        }
        Ok((count, reasons))
    }
}

fn describe(target: &LinkTarget) -> String {
    match target {
        LinkTarget::Label(x) => format!("reference to “{x}”"),
        LinkTarget::Citation(k) => format!("citation “{k}”"),
        LinkTarget::LabelOrCitation(x) => format!("reference “@{x}”"),
        LinkTarget::Path { written, .. } => format!("file “{written}”"),
        LinkTarget::LeanModule(m) => format!("module {m}"),
        LinkTarget::Declaration(_) => "the preceding statement".into(),
    }
}

fn list(items: &[String], max: usize) -> String {
    let mut shown: Vec<String> = items.iter().take(max).map(|s| crate::store::truncate(s, 160)).collect();
    if items.len() > max {
        shown.push(format!("{} more", items.len() - max));
    }
    shown.join("; ")
}

/// One folder a written path is looked up in.
pub(crate) struct Lookup {
    /// The written path below that folder, without the extensions tried: a
    /// file waiting for it is resolved again when a resource appears at
    /// this path, at it with any extension, or as its package file (see
    /// `target_keys`).
    awaited: WorkspacePath,
    /// The paths tried there, in order (none already tried in an earlier folder).
    candidates: Vec<WorkspacePath>,
    /// The folder of a LaTeX main file (tried after the file's own folder
    /// and the project root).
    latex_main: bool,
}

/// What a file waits for after its written path was looked up: every folder
/// tried before the one it was found in, that folder too unless its first
/// candidate was the one found (an earlier candidate that appears takes its
/// place), and every folder when it was not found. A path that was not
/// settled before the folders of the LaTeX main files also waits for that
/// set of folders (`latex`: the path is looked up as LaTeX does).
fn await_lookups(wanted: &mut BTreeSet<String>, lookups: &[Lookup], found: Option<&WorkspacePath>, latex: bool) {
    for lookup in lookups {
        let here = found.is_some_and(|at| lookup.candidates.contains(at));
        if here {
            if lookup.candidates.first() != found {
                wanted.insert(format!("path:{}", lookup.awaited));
            }
            if lookup.latex_main {
                wanted.insert(super::engine::LATEX_MAINS_TARGET.to_owned());
            }
            return;
        }
        wanted.insert(format!("path:{}", lookup.awaited));
    }
    // Not found: the folder of another main file may come to hold it.
    if latex {
        wanted.insert(super::engine::LATEX_MAINS_TARGET.to_owned());
    }
}

/// Where a path written in `from` is looked up, in order: the base folders
/// of `base` (for LaTeX also the folders of the project's main files
/// `latex_mains`), each with the written name and then with each of
/// `extensions` appended. Paths that leave the project are not candidates.
pub(crate) fn path_lookups<'a>(
    from: &WorkspacePath,
    written: &str,
    base: PathBase,
    extensions: &[String],
    latex_mains: impl IntoIterator<Item = &'a WorkspacePath>,
) -> Vec<Lookup> {
    let dir = from.parent();
    // (folder, the folder of a LaTeX main file)
    let mut bases: Vec<(Option<WorkspacePath>, bool)> = Vec::new();
    match base {
        PathBase::File => bases.push((dir.clone(), false)),
        PathBase::Root => {
            // A Python script's own folder comes first on its import path.
            if extensions.iter().any(|e| e == "py") {
                bases.push((dir.clone(), false));
            }
            bases.push((None, false));
        }
        PathBase::LatexMain => {
            bases.push((dir.clone(), false));
            bases.push((None, false));
            let mut mains: BTreeSet<Option<WorkspacePath>> = BTreeSet::new();
            for m in latex_mains {
                mains.insert(m.parent());
            }
            bases.extend(mains.into_iter().map(|m| (m, true)));
        }
    }
    let mut out: Vec<Lookup> = Vec::new();
    let mut seen = BTreeSet::new();
    let has_ext = written.rsplit('/').next().is_some_and(|n| n.contains('.'));
    for (b, latex_main) in bases {
        let Some(p) = extract::join_relative(b.as_ref(), written) else { continue };
        let mut candidates: Vec<WorkspacePath> = Vec::new();
        if has_ext || extensions.is_empty() {
            candidates.push(p.clone());
        }
        for ext in extensions {
            if let Ok(with) = WorkspacePath::parse(&format!("{}.{ext}", p.as_str())) {
                candidates.push(with);
            }
        }
        if base == PathBase::Root
            && extensions.iter().any(|e| e == "py")
            && let Ok(init) = p.join("__init__.py")
        {
            candidates.push(init);
        }
        candidates.retain(|c| seen.insert(c.clone()));
        if !candidates.is_empty() {
            out.push(Lookup { awaited: p, candidates, latex_main });
        }
    }
    out
}

/// Candidate project paths for a path written in `from`, in the order they
/// are tried (see [`path_lookups`]). Shared by the reconciler and transient
/// readers (image previews).
pub(crate) fn path_candidates<'a>(
    from: &WorkspacePath,
    written: &str,
    base: PathBase,
    extensions: &[String],
    latex_mains: impl IntoIterator<Item = &'a WorkspacePath>,
) -> Vec<WorkspacePath> {
    path_lookups(from, written, base, extensions, latex_mains).into_iter().flat_map(|l| l.candidates).collect()
}
