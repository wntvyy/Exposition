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

impl Planner<'_> {
    /// Live entities (other than files/folders) whose natural key is `key`,
    /// as of this pass.
    fn keyed_entities(&self, key: &str, kinds: impl Fn(EntityKind) -> bool) -> StoreResult<Vec<Entity>> {
        let mut ids: BTreeSet<EntityId> = BTreeSet::new();
        for e in self.store.live_entities_with_key(key)? {
            ids.insert(e.id);
        }
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

    fn label_exists(&self, label: &str, body: &IndexBody) -> bool {
        if body.extraction.labels.iter().any(|l| l == label) {
            return true;
        }
        if self.index_records.values().any(|b| b.extraction.labels.iter().any(|l| l == label)) {
            return true;
        }
        self.index.labels.get(label).is_some_and(|paths| paths.iter().any(|p| self.resources.contains_key(p)))
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
        let elsewhere = self.keyed_entities(name, |k| {
            !matches!(k, EntityKind::Resource | EntityKind::Folder | EntityKind::Reference | EntityKind::Project)
        })?;
        match elsewhere.len() {
            1 => Ok(Target::Entity(elsewhere[0].id.clone())),
            0 if self.label_exists(name, body) => Ok(Target::NotAnEntity),
            0 => Ok(Target::Missing),
            n => Ok(Target::Ambiguous(n)),
        }
    }

    fn resolve_citation(&self, key: &str) -> StoreResult<Target> {
        let refs = self.keyed_entities(key, |k| k == EntityKind::Reference)?;
        Ok(match refs.len() {
            1 => Target::Entity(refs[0].id.clone()),
            0 => Target::Missing,
            n => Target::Ambiguous(n),
        })
    }

    /// Candidate project paths for a written path.
    fn path_candidates(&self, from: &WorkspacePath, written: &str, base: PathBase, extensions: &[String]) -> Vec<WorkspacePath> {
        let dir = from.parent();
        let mut bases: Vec<Option<WorkspacePath>> = Vec::new();
        match base {
            PathBase::File => bases.push(dir.clone()),
            PathBase::Root => bases.push(None),
            PathBase::LatexMain => {
                bases.push(dir.clone());
                bases.push(None);
                let mut mains: BTreeSet<Option<WorkspacePath>> = BTreeSet::new();
                for m in self.index.latex_mains.iter().chain(
                    self.index_records.iter().filter(|(_, b)| b.latex_main).map(|(p, _)| p),
                ) {
                    mains.insert(m.parent());
                }
                bases.extend(mains);
            }
        }
        let mut out: Vec<WorkspacePath> = Vec::new();
        let has_ext = written.rsplit('/').next().is_some_and(|n| n.contains('.'));
        for b in bases {
            let Some(p) = extract::join_relative(b.as_ref(), written) else { continue };
            if has_ext || extensions.is_empty() {
                out.push(p.clone());
            }
            for ext in extensions {
                if let Ok(with) = WorkspacePath::parse(&format!("{}.{ext}", p.as_str())) {
                    out.push(with);
                }
            }
            if base == PathBase::Root && extensions.iter().any(|e| e == "py")
                && let Ok(init) = p.join("__init__.py")
            {
                out.push(init);
            }
        }
        let mut seen = BTreeSet::new();
        out.retain(|p| seen.insert(p.clone()));
        out
    }

    /// The resource for a written path; admits a retained/attached generated
    /// artifact explicitly when `admit_generated`.
    fn resolve_path(&mut self, candidates: &[WorkspacePath], admit_generated: bool) -> StoreResult<Option<EntityId>> {
        for c in candidates {
            if let Some(id) = self.resources.get(c) {
                return Ok(Some(id.clone()));
            }
        }
        if !admit_generated {
            return Ok(None);
        }
        for c in candidates {
            let Admission::OnRequest(roles) = classify(c) else { continue };
            let Some(meta) = scan::stat_file(self.root, c) else { continue };
            return Ok(Some(self.admit_generated(c, roles, &meta)?));
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
                version: content.hash.as_ref().map(|h| SourceVersion { content_hash: h.clone(), revision: None, saved: true }),
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

    fn lean_candidates(&self, module: &str) -> StoreResult<Vec<WorkspacePath>> {
        if module.is_empty() || module.contains(['/', '\\']) {
            return Ok(Vec::new());
        }
        let rel = format!("{}.lean", module.replace('.', "/"));
        Ok(self
            .lake_roots()?
            .into_iter()
            .filter_map(|root| extract::join_relative(root.as_ref(), &rel))
            .collect())
    }

    /// Resolve every observed link of `path` and replace its observed
    /// relations by the result (diffed: unchanged relations keep their ids).
    pub(crate) fn resolve_links(&mut self, path: &WorkspacePath) -> StoreResult<()> {
        let Some(resource_id) = self.resources.get(path).cloned() else { return Ok(()) };
        let Some(resource) = self.entity(&resource_id)? else { return Ok(()) };
        let body: IndexBody = match self.index_records.get(path) {
            Some(b) => b.clone(),
            None => match self.store.source_index(path)? {
                Some(r) => match serde_json::from_str(&r.body) {
                    Ok(b) => b,
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
            for r in self.store.relations_from(id)? {
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
        for link in &body.extraction.links {
            let from = match link.from {
                LinkFrom::Resource => resource_id.clone(),
                LinkFrom::Declaration(i) => body.ids.get(i).cloned().flatten().unwrap_or_else(|| resource_id.clone()),
            };
            let target = match &link.target {
                LinkTarget::Label(x) => self.resolve_label(x, &body)?,
                LinkTarget::Citation(k) => self.resolve_citation(k)?,
                LinkTarget::LabelOrCitation(x) => match self.resolve_label(x, &body)? {
                    Target::Missing => self.resolve_citation(x)?,
                    other => other,
                },
                LinkTarget::Declaration(i) => match body.ids.get(*i).cloned().flatten() {
                    Some(id) => Target::Entity(id),
                    None => Target::Missing,
                },
                LinkTarget::Path { written, base, extensions } => {
                    let candidates = self.path_candidates(path, written, *base, extensions);
                    let generated = link.kind == RelationKind::UsesAsset;
                    match self.resolve_path(&candidates, generated)? {
                        Some(id) => Target::Entity(id),
                        None if code => Target::NotAnEntity,
                        None => {
                            // An existing file that is not a project resource type is not unresolved.
                            let exists = candidates.iter().any(|c| scan::stat_file(self.root, c).is_some());
                            if exists { Target::NotAnEntity } else { Target::Missing }
                        }
                    }
                }
                LinkTarget::LeanModule(m) => {
                    let candidates = self.lean_candidates(m)?;
                    match self.resolve_path(&candidates, false)? {
                        Some(id) => Target::Entity(id),
                        None => Target::NotAnEntity,
                    }
                }
            };
            let described = describe(&link.target);
            match target {
                Target::Entity(to) => {
                    if from == to {
                        continue;
                    }
                    let (Some(fe), Some(te)) = (self.entity(&from)?, self.entity(&to)?) else { continue };
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
            let candidates = self.path_candidates(path, image, att.base, &exts);
            match self.resolve_path(&candidates, true)? {
                Some(to) if to != resource_id => {
                    desired.insert((RelationKind::UsesAsset, resource_id.clone(), to));
                }
                Some(_) => {}
                None => unresolved.push(format!("attachment {} uses “{image}”, which was not found", att.key)),
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
            let link_extractor = if kind == RelationKind::UsesAsset && te.origin
                == (Origin::Extracted { extractor: ATTACHMENT_EXTRACTOR.into() })
            {
                ATTACHMENT_EXTRACTOR.to_owned()
            } else {
                extractor.clone()
            };
            self.relate(kind, &fe, &te, &link_extractor)?;
        }
        self.finish_coverage(path, &resource, &body, unresolved)?;
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
            None => match self.store.coverage(path)? {
                Some(c) => (c, None),
                None => return Ok(()),
            },
        };
        let (binding_count, mut binding_reasons) = self.binding_unresolved(path, resource)?;
        if let Some(extra) = self.binding_problems.get(path) {
            binding_reasons.extend(extra.iter().cloned());
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
        for b in self.store.bindings_at_path(path)? {
            ids.insert(b.entity);
        }
        for (id, bs) in &self.bindings {
            if bs.iter().any(|b| &b.path == path) {
                ids.insert(id.clone());
            }
        }
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
