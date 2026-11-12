//! One reconciliation pass: scan → read/extract changed files (no lock held)
//! → plan against the current graph and write in one transaction.
//!
//! What a pass maintains (all derived from files, all rebuildable):
//! - resources (one identity per file; confirmed renames keep it: the same
//!   physical file, or a unique same-content disappearance/appearance in one
//!   pass), folders, and their `contains` structure;
//! - extracted declarations (identity by [`super::matching`]) anchored in
//!   their resource;
//! - observed links (`imports`, `usesAsset`, `references`, `proofOf`) of each
//!   re-extracted resource, replaced as a set (diffed, ids kept);
//! - coverage per file (the incremental checkpoint) and the source index.
//!
//! What a pass never does: delete an entity (gone referents are tombstoned),
//! delete or alter a curated record (user/agent/preset/service entities and
//! relations), or reassign an identity on an uncertain match. Curated
//! entities bound to a file that changed are re-anchored or marked
//! missing/ambiguous.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::extract::{self, Extraction};
use super::matching::{self, KnownAnchor};
use super::scan::{self, FileContent, FileMeta, ScanOutcome};
use crate::ids::{EntityId, RelationId};
use crate::model::{
    AssertionStatus, BindingState, CoverageState, Entity, GraphDelta, GraphEvent, Lifecycle, Origin, Relation,
    ResourceCoverage, SourceBinding, SourceSelector, SourceVersion,
};
use crate::service::{GraphError, GraphResult, GraphService};
use crate::store::{Checkpoint, GraphStore, IndexRecord, StoreResult, Writer, now_ms};
use crate::vocabulary::{EntityKind, RelationKind, ResourceRole};

/// Origin name of resources and folders admitted by scanning.
pub const SCAN_EXTRACTOR: &str = "scan/1";
/// Origin name of generated artifacts admitted because a manuscript attaches them.
pub const ATTACHMENT_EXTRACTOR: &str = "attachment/1";
/// Origin of the pass itself (change journal).
pub const PASS_EXTRACTOR: &str = "reconcile/1";
/// Tombstoned declarations of one file considered for revival.
const MAX_TOMBSTONE_CANDIDATES: usize = 200;
/// Ids listed per delta field before `truncated` is set.
const DELTA_LIMIT: usize = 500;

/// Which part of the project a pass looks at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The whole project folder.
    Full,
    /// These files or folders (a folder covers everything beneath it).
    Paths(Vec<WorkspacePath>),
}

#[derive(Clone, Debug)]
pub struct ReconcileOptions {
    pub scope: Scope,
    /// Re-read and re-extract every file in scope even when its checkpoint
    /// (size, modification time, content hash) is unchanged ("Reindex").
    pub force: bool,
}

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileReport {
    /// Graph revision after the pass.
    pub revision: u64,
    /// Project files in scope.
    pub files: u32,
    /// Files read because they were new or changed (or forced).
    pub read: u32,
    /// Files whose text was extracted.
    pub extracted: u32,
    pub resources_added: u32,
    /// Confirmed renames/moves (identity kept).
    pub resources_renamed: u32,
    pub resources_removed: u32,
    pub declarations_added: u32,
    pub declarations_removed: u32,
    pub relations_added: u32,
    pub relations_removed: u32,
    /// References and bindings left unresolved in the files reconciled.
    pub unresolved: u32,
    /// A scan bound was reached (files beyond it are unknown, not removed).
    pub truncated: Option<String>,
    pub duration_ms: u64,
}

/// One file's stored source index.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct IndexBody {
    pub extraction: Extraction,
    /// Entity of each extracted declaration (`None`: not admitted, e.g. an
    /// ambiguous duplicate name).
    pub ids: Vec<Option<EntityId>>,
    /// A LaTeX main file (`\documentclass`).
    pub latex_main: bool,
}

/// Session cache rebuilt from the stored source index (labels by name).
#[derive(Default)]
pub(crate) struct ProjectIndex {
    pub loaded: bool,
    pub labels: HashMap<String, BTreeSet<WorkspacePath>>,
    pub by_path: HashMap<WorkspacePath, Vec<String>>,
    pub latex_mains: BTreeSet<WorkspacePath>,
}

impl ProjectIndex {
    pub fn set(&mut self, path: &WorkspacePath, labels: &[String], latex_main: bool) {
        self.remove(path);
        for l in labels {
            self.labels.entry(l.clone()).or_default().insert(path.clone());
        }
        self.by_path.insert(path.clone(), labels.to_vec());
        if latex_main {
            self.latex_mains.insert(path.clone());
        }
    }

    pub fn remove(&mut self, path: &WorkspacePath) {
        if let Some(old) = self.by_path.remove(path) {
            for l in old {
                if let Some(set) = self.labels.get_mut(&l) {
                    set.remove(path);
                    if set.is_empty() {
                        self.labels.remove(&l);
                    }
                }
            }
        }
        self.latex_mains.remove(path);
    }

    fn load(&mut self, store: &GraphStore) -> StoreResult<()> {
        #[derive(Deserialize)]
        struct Labels {
            extraction: LabelsOnly,
            #[serde(default)]
            latex_main: bool,
        }
        #[derive(Deserialize)]
        struct LabelsOnly {
            #[serde(default)]
            labels: Vec<String>,
        }
        *self = ProjectIndex { loaded: true, ..Default::default() };
        for record in store.all_source_index()? {
            if let Ok(body) = serde_json::from_str::<Labels>(&record.body) {
                self.set(&record.path, &body.extraction.labels, body.latex_main);
            }
        }
        Ok(())
    }
}

/// Per-session reconciliation state (held for the whole pass: passes are
/// serialized).
#[derive(Default)]
pub(crate) struct ReconcileState {
    /// Attach generation this state belongs to.
    pub generation: u64,
    pub root: Option<PathBuf>,
    pub index: ProjectIndex,
}

/// A file read in this pass.
pub(crate) struct Fresh {
    pub meta: FileMeta,
    pub content: FileContent,
    pub extraction: Option<Extraction>,
}

/// Run one pass. Serialized with other passes; file I/O happens without the
/// graph lock; the graph is written in one transaction.
pub fn run(graph: &GraphService, options: &ReconcileOptions) -> GraphResult<ReconcileReport> {
    let started = Instant::now();
    let mut state = graph.reconcile_state();
    let root =
        graph.attached_root().ok_or_else(|| GraphError::new("graph.noProject", "No project graph is attached."))?;
    if state.root.as_deref() != Some(root.as_path()) {
        state.root = Some(root.clone());
        state.index = ProjectIndex::default();
    }
    // 1. Checkpoints and known resources (short lock).
    let (checkpoints, known) = graph.with_store_for(&root, |s| {
        if !state.index.loaded {
            state.index.load(s)?;
        }
        // A few paths are looked up directly; many (a burst of changes) are
        // cheaper to filter from one read.
        let prefixes = match &options.scope {
            Scope::Paths(p) if p.len() <= 16 => Some(p.as_slice()),
            _ => None,
        };
        let checkpoints = match prefixes {
            None => s.checkpoints()?,
            Some(p) => s.checkpoints_under(p)?,
        };
        let known: BTreeMap<WorkspacePath, Vec<ResourceRole>> =
            s.resource_keys(prefixes)?.into_iter().filter(|k| !k.tombstoned).map(|k| (k.path, k.roles)).collect();
        Ok((checkpoints, known))
    })?;

    // Pending marks made before this point are reconciled by this pass.
    let token = graph.next_change_token();
    let t_loaded = started.elapsed();
    // 2. Scan (no lock).
    let mut scan_out = ScanOutcome::default();
    let covered = match &options.scope {
        Scope::Full => {
            scan::scan_tree(&root, None, &mut scan_out);
            Covered::All
        }
        Scope::Paths(paths) => {
            let mut prefixes = Vec::new();
            for p in paths {
                if scan::is_real_dir(&root, p) {
                    scan::scan_tree(&root, Some(p), &mut scan_out);
                } else if let Some(meta) = scan::stat_file(&root, p)
                    && matches!(crate::admission::classify(p), crate::admission::Admission::Admit(_))
                {
                    scan_out.files.insert(p.clone(), meta);
                }
                prefixes.push(p.clone());
            }
            Covered::Prefixes(prefixes)
        }
    };
    // Known resources in scope that the walk does not visit (retained
    // artifacts, files admitted explicitly) are checked individually.
    for (path, roles) in &known {
        if covered.contains(path)
            && !scan_out.files.contains_key(path)
            && let Some(mut meta) = scan::stat_file(&root, path)
        {
            if meta.roles.is_empty() {
                meta.roles = roles.clone();
            }
            scan_out.files.insert(path.clone(), meta);
        }
    }

    let t_scanned = started.elapsed();
    // 3. Read and extract new or changed files (no lock).
    let mut fresh: BTreeMap<WorkspacePath, Fresh> = BTreeMap::new();
    for (path, meta) in &scan_out.files {
        let extractor = extract::extractor_for(path);
        let unchanged = !options.force
            && known.contains_key(path)
            && checkpoints.get(path).is_some_and(|c| {
                c.size == Some(meta.size)
                    && c.modified_ms.is_some()
                    && c.modified_ms == meta.modified_ms
                    && c.coverage.extractor.as_deref() == extractor
            });
        if unchanged {
            continue;
        }
        let content = scan::read_file(&root, path, meta.size, extractor.is_some());
        let same_hash = !options.force
            && known.contains_key(path)
            && content.hash.is_some()
            && checkpoints.get(path).and_then(|c| c.coverage.content_hash.as_ref()) == content.hash.as_ref()
            && checkpoints.get(path).and_then(|c| c.coverage.extractor.as_deref()) == extractor;
        let extraction =
            if same_hash { None } else { content.text.as_deref().and_then(|text| extract::extract(path, text)) };
        fresh.insert(path.clone(), Fresh { meta: meta.clone(), content, extraction });
    }

    let t_read = started.elapsed();
    // 4. Plan and write (one lock, one transaction).
    let now = now_ms();
    let force = options.force;
    let index = &state.index;
    let outcome = graph.with_store_for(&root, |store| {
        let planning = Instant::now();
        let mut planner = Planner::new(store, &root, index, now, force);
        planner.plan(&scan_out, &covered, &checkpoints, &fresh)?;
        let mut plan = planner.finish();
        plan.clear_pending = Some((
            match &options.scope {
                Scope::Full => None,
                Scope::Paths(p) => Some(p.clone()),
            },
            token,
        ));
        let planned = planning.elapsed();
        let revision = plan.write(store)?;
        Ok((plan, revision, planned))
    })?;
    let (plan, revision, planned) = outcome;
    if std::env::var_os("EXPOSITION_GRAPH_TRACE").is_some() {
        eprintln!(
            "reconcile: load {:?}, scan {:?}, read+extract {:?} ({} files), plan {:?}, write {:?}",
            t_loaded,
            t_scanned - t_loaded,
            t_read - t_scanned,
            fresh.len(),
            planned,
            started.elapsed() - t_read - planned
        );
    }
    for (path, update) in &plan.index_updates {
        match update {
            Some((labels, main)) => state.index.set(path, labels, *main),
            None => state.index.remove(path),
        }
    }
    let mut report = plan.report.clone();
    report.revision = revision;
    report.files = scan_out.files.len() as u32;
    report.read = fresh.len() as u32;
    report.extracted = fresh.values().filter(|f| f.extraction.is_some()).count() as u32;
    report.truncated = scan_out.truncated.clone();
    report.duration_ms = started.elapsed().as_millis() as u64;
    if !plan.delta.is_empty() {
        graph.emit_event(GraphEvent::Changed {
            delta: plan.delta.clone(),
            origin: Origin::Extracted { extractor: PASS_EXTRACTOR.into() },
        });
    }
    Ok(report)
}

/// Which known paths a pass is responsible for.
pub(crate) enum Covered {
    All,
    Prefixes(Vec<WorkspacePath>),
}

impl Covered {
    pub fn contains(&self, path: &WorkspacePath) -> bool {
        match self {
            Covered::All => true,
            Covered::Prefixes(ps) => ps
                .iter()
                .any(|p| path == p || path.as_str().strip_prefix(p.as_str()).is_some_and(|rest| rest.starts_with('/'))),
        }
    }
}

/// File state recorded with coverage.
#[derive(Clone, Debug)]
pub(crate) struct Stamp {
    pub size: Option<u64>,
    pub modified_ms: Option<u64>,
    pub identity: Option<String>,
}

/// Labels of a file and whether it is a LaTeX main file (session index update).
pub(crate) type IndexLabels = (Vec<String>, bool);

/// Everything a pass writes.
pub(crate) struct Plan {
    pub entities: Vec<Entity>,
    pub bindings: Vec<(EntityId, Vec<SourceBinding>)>,
    pub add: Vec<Relation>,
    pub remove: Vec<RelationId>,
    pub coverage: Vec<(ResourceCoverage, Option<Stamp>)>,
    pub stamps: Vec<(WorkspacePath, Stamp)>,
    pub remove_coverage: Vec<WorkspacePath>,
    pub index: Vec<IndexRecord>,
    pub remove_index: Vec<WorkspacePath>,
    pub wanted: Vec<(WorkspacePath, Vec<String>)>,
    pub index_updates: Vec<(WorkspacePath, Option<IndexLabels>)>,
    pub delta: GraphDelta,
    pub report: ReconcileReport,
    /// Coverage contents changed (bumps the graph revision even without
    /// entity changes: source versions are graph state).
    pub semantic: bool,
    /// Clear pending marks (scope, up to token) when written.
    pub clear_pending: Option<(Option<Vec<WorkspacePath>>, u64)>,
}

impl Plan {
    /// Nothing to write (e.g. a pass over paths that hold no project files).
    fn is_empty(&self) -> bool {
        self.entities.is_empty()
            && self.bindings.is_empty()
            && self.add.is_empty()
            && self.remove.is_empty()
            && self.coverage.is_empty()
            && self.stamps.is_empty()
            && self.remove_coverage.is_empty()
            && self.index.is_empty()
            && self.remove_index.is_empty()
            && self.wanted.is_empty()
            && self.delta.is_empty()
    }

    /// Write the plan in one transaction. Returns the graph revision after it.
    fn write(&self, store: &mut GraphStore) -> StoreResult<u64> {
        let w = Writer::new(store.transaction()?);
        if let Some((scope, token)) = &self.clear_pending {
            w.clear_pending(scope.as_deref(), *token)?;
        }
        if self.is_empty() {
            let revision = w.revision()?;
            w.commit()?;
            return Ok(revision);
        }
        for e in &self.entities {
            w.put_entity(e)?;
        }
        for (id, b) in &self.bindings {
            w.set_bindings(id, b)?;
        }
        for id in &self.remove {
            w.delete_relation(id)?;
        }
        for r in &self.add {
            w.put_relation(r)?;
        }
        for p in &self.remove_coverage {
            w.remove_coverage(p)?;
        }
        for (c, stamp) in &self.coverage {
            w.set_coverage(c)?;
            if let Some(stamp) = stamp {
                w.set_file_state(&c.path, stamp.size, stamp.modified_ms, stamp.identity.as_deref())?;
            }
        }
        for (p, stamp) in &self.stamps {
            w.set_file_state(p, stamp.size, stamp.modified_ms, stamp.identity.as_deref())?;
        }
        for p in &self.remove_index {
            w.remove_source_index(p)?;
        }
        for r in &self.index {
            w.put_source_index(r)?;
        }
        for (p, wanted) in &self.wanted {
            w.set_index_wanted(p, wanted)?;
        }
        let now = now_ms().to_string();
        w.set_meta("reconciled_at", &now)?;
        let revision = if self.semantic || !self.delta.is_empty() {
            let mut delta = self.delta.clone();
            let summary = format!(
                "Reconcile: {} added, {} updated, {} removed",
                delta.entities_added.len(),
                delta.entities_updated.len(),
                delta.entities_removed.len()
            );
            w.record_change(&Origin::Extracted { extractor: PASS_EXTRACTOR.into() }, &summary, &mut delta)?
        } else {
            w.revision()?
        };
        w.commit()?;
        Ok(revision)
    }
}

/// A declaration known for a file (from its bindings).
#[derive(Clone)]
struct Known {
    entity: Entity,
    anchor: KnownAnchor,
}

/// The outcome of matching one changed file.
struct FileMatch {
    path: WorkspacePath,
    resource: EntityId,
    extraction: Extraction,
    hash: Option<String>,
    known: Vec<Known>,
    matching: matching::Matching,
    /// Final entity id per new declaration (filled while applying).
    ids: Vec<Option<EntityId>>,
    latex_main: bool,
}

pub(crate) struct Planner<'a> {
    pub(crate) store: &'a GraphStore,
    pub(crate) root: &'a Path,
    pub(crate) index: &'a ProjectIndex,
    pub(crate) now: u64,
    force: bool,
    pub(crate) entities: HashMap<EntityId, Entity>,
    write_entities: BTreeSet<EntityId>,
    pub(crate) bindings: HashMap<EntityId, Vec<SourceBinding>>,
    /// Entities with a planned binding in each path.
    bound_at: HashMap<WorkspacePath, BTreeSet<EntityId>>,
    /// Relations to add (`None` once withdrawn again in this pass).
    add: Vec<Option<Relation>>,
    add_keys: HashMap<(RelationKind, EntityId, EntityId), usize>,
    adds_from: HashMap<EntityId, Vec<usize>>,
    remove: BTreeSet<RelationId>,
    pub(crate) coverage: BTreeMap<WorkspacePath, (ResourceCoverage, Option<Stamp>)>,
    stamps: BTreeMap<WorkspacePath, Stamp>,
    remove_coverage: BTreeSet<WorkspacePath>,
    pub(crate) index_records: BTreeMap<WorkspacePath, std::sync::Arc<IndexBody>>,
    /// Labels and LaTeX main files of the index records planned in this pass.
    pub(crate) planned_labels: HashMap<String, BTreeSet<WorkspacePath>>,
    pub(crate) planned_mains: BTreeSet<WorkspacePath>,
    index_hashes: BTreeMap<WorkspacePath, (String, String)>,
    pub(crate) remove_index: BTreeSet<WorkspacePath>,
    /// Link targets each resolved file still waits for.
    pub(crate) wanted: BTreeMap<WorkspacePath, Vec<String>>,
    /// Targets that appeared or changed in this pass (`name:…`, `path:…`).
    pub(crate) new_targets: BTreeSet<String>,
    delta: GraphDelta,
    pub(crate) report: ReconcileReport,
    semantic: bool,
    started: Instant,
    pub(crate) lake_roots_cache: Option<Vec<Option<WorkspacePath>>>,
    /// All bindings by path, read once when many files are indexed.
    preloaded_bindings: Option<HashMap<WorkspacePath, Vec<SourceBinding>>>,
    /// Entities created in this pass (no stored relations to look up).
    pub(crate) created_now: std::collections::HashSet<EntityId>,
    /// Stored live entities by natural key (store state is fixed during planning).
    pub(crate) key_cache: std::cell::RefCell<HashMap<String, Vec<EntityId>>>,
    /// Active resource per path after this pass's changes.
    pub(crate) resources: BTreeMap<WorkspacePath, EntityId>,
    /// Entities whose natural key was set in this pass (for resolution).
    pub(crate) keyed: HashMap<String, BTreeSet<EntityId>>,
    pub(crate) tombstoned_now: BTreeSet<EntityId>,
    /// Resources moved/renamed in this pass.
    moved_now: BTreeSet<EntityId>,
    /// Resources whose observed links must be resolved again.
    pub(crate) resolve: BTreeSet<WorkspacePath>,
    /// New link targets appeared (names, files): retry unresolved files.
    pub(crate) targets_added: bool,
    pub(crate) structure_dirty: bool,
    /// Old → new path of resources renamed in this pass.
    pub(crate) renames: Vec<(WorkspacePath, WorkspacePath)>,
    /// Unresolved bindings per path (missing/ambiguous), with reasons.
    pub(crate) binding_problems: BTreeMap<WorkspacePath, Vec<String>>,
    /// Base coverage of files indexed this pass (before link resolution).
    pub(crate) base_coverage: BTreeMap<WorkspacePath, (ResourceCoverage, Stamp)>,
}

impl<'a> Planner<'a> {
    pub(crate) fn new(store: &'a GraphStore, root: &'a Path, index: &'a ProjectIndex, now: u64, force: bool) -> Self {
        Self {
            store,
            root,
            index,
            now,
            force,
            entities: HashMap::new(),
            write_entities: BTreeSet::new(),
            bindings: HashMap::new(),
            bound_at: HashMap::new(),
            add: Vec::new(),
            add_keys: HashMap::new(),
            adds_from: HashMap::new(),
            remove: BTreeSet::new(),
            coverage: BTreeMap::new(),
            stamps: BTreeMap::new(),
            remove_coverage: BTreeSet::new(),
            index_records: BTreeMap::new(),
            planned_labels: HashMap::new(),
            planned_mains: BTreeSet::new(),
            index_hashes: BTreeMap::new(),
            remove_index: BTreeSet::new(),
            wanted: BTreeMap::new(),
            new_targets: BTreeSet::new(),
            delta: GraphDelta::default(),
            report: ReconcileReport::default(),
            semantic: false,
            started: Instant::now(),
            lake_roots_cache: None,
            preloaded_bindings: None,
            created_now: std::collections::HashSet::new(),
            key_cache: std::cell::RefCell::new(HashMap::new()),
            resources: BTreeMap::new(),
            keyed: HashMap::new(),
            tombstoned_now: BTreeSet::new(),
            moved_now: BTreeSet::new(),
            resolve: BTreeSet::new(),
            targets_added: false,
            structure_dirty: false,
            renames: Vec::new(),
            binding_problems: BTreeMap::new(),
            base_coverage: BTreeMap::new(),
        }
    }

    // ----- record helpers -----

    pub(crate) fn entity(&self, id: &EntityId) -> StoreResult<Option<Entity>> {
        match self.entities.get(id) {
            Some(e) => Ok(Some(e.clone())),
            None => self.store.entity(id),
        }
    }

    /// An entity that must exist (a resource id read under the same lock).
    fn must(&self, id: &EntityId) -> StoreResult<Entity> {
        self.entity(id)?.ok_or_else(|| crate::store::StoreError::Corrupt(format!("entity {id} vanished")))
    }

    /// Stored bindings in `path` (before this pass's changes).
    pub(crate) fn bindings_at(&self, path: &WorkspacePath) -> StoreResult<Vec<SourceBinding>> {
        match &self.preloaded_bindings {
            Some(map) => Ok(map.get(path).cloned().unwrap_or_default()),
            None => self.store.bindings_at_path(path),
        }
    }

    pub(crate) fn bindings_of(&self, id: &EntityId) -> StoreResult<Vec<SourceBinding>> {
        match self.bindings.get(id) {
            Some(b) => Ok(b.clone()),
            None => self.store.bindings(id),
        }
    }

    fn mark(list: &mut Vec<EntityId>, truncated: &mut bool, id: &EntityId) {
        if list.contains(id) {
            return;
        }
        if list.len() < DELTA_LIMIT {
            list.push(id.clone());
        } else {
            *truncated = true;
        }
    }

    /// Store a new entity.
    pub(crate) fn create(&mut self, e: Entity) {
        Self::mark(&mut self.delta.entities_added, &mut self.delta.truncated, &e.id);
        if let Some(k) = &e.key {
            self.keyed.entry(k.clone()).or_default().insert(e.id.clone());
            self.targets_added = true;
            self.new_targets.insert(target_key(e.kind, k));
        }
        self.write_entities.insert(e.id.clone());
        self.created_now.insert(e.id.clone());
        self.entities.insert(e.id.clone(), e);
    }

    /// Store an updated entity if its record changed (bumps its revision).
    pub(crate) fn update(&mut self, before: &Entity, mut after: Entity) {
        let changed = before.label != after.label
            || before.roles != after.roles
            || before.key != after.key
            || before.attributes != after.attributes
            || before.lifecycle != after.lifecycle
            || before.tombstone_reason != after.tombstone_reason;
        if !changed {
            return;
        }
        // A name or file that appears, moves, or disappears can change how
        // other files' references resolve (missing → found, ambiguous → unique).
        if before.key != after.key || before.lifecycle != after.lifecycle {
            for k in [&before.key, &after.key].into_iter().flatten() {
                self.new_targets.insert(target_key(after.kind, k));
            }
        }
        if before.lifecycle == Lifecycle::Tombstoned && after.lifecycle != Lifecycle::Tombstoned {
            self.tombstoned_now.remove(&after.id);
            self.targets_added = true;
        }
        if before.key != after.key
            && let Some(k) = &after.key
        {
            self.keyed.entry(k.clone()).or_default().insert(after.id.clone());
            self.targets_added = true;
        }
        if after.lifecycle == Lifecycle::Tombstoned && before.lifecycle != Lifecycle::Tombstoned {
            self.tombstoned_now.insert(after.id.clone());
            Self::mark(&mut self.delta.entities_removed, &mut self.delta.truncated, &after.id);
        } else {
            Self::mark(&mut self.delta.entities_updated, &mut self.delta.truncated, &after.id);
        }
        after.revision = before.revision + 1;
        after.updated_at_ms = self.now;
        self.write_entities.insert(after.id.clone());
        self.entities.insert(after.id.clone(), after);
    }

    fn tombstone(&mut self, e: &Entity, reason: String) {
        if e.lifecycle == Lifecycle::Tombstoned {
            return;
        }
        let mut after = e.clone();
        after.lifecycle = Lifecycle::Tombstoned;
        after.tombstone_reason = Some(crate::store::truncate(&reason, 300));
        self.update(e, after);
    }

    pub(crate) fn set_bindings(&mut self, id: &EntityId, bindings: Vec<SourceBinding>) {
        for b in &bindings {
            self.bound_at.entry(b.path.clone()).or_default().insert(id.clone());
        }
        self.bindings.insert(id.clone(), bindings);
    }

    /// Entities with a planned binding in `path` (possibly moved away since).
    pub(crate) fn planned_bound_at(&self, path: &WorkspacePath) -> Vec<EntityId> {
        self.bound_at.get(path).map(|s| s.iter().cloned().collect()).unwrap_or_default()
    }

    /// Record a file's index for this pass (keeps the label/main caches).
    pub(crate) fn put_index_record(&mut self, path: &WorkspacePath, body: IndexBody) {
        if let Some(old) = self.index_records.get(path) {
            for l in &old.extraction.labels {
                if let Some(set) = self.planned_labels.get_mut(l) {
                    set.remove(path);
                }
            }
        }
        for l in &body.extraction.labels {
            self.planned_labels.entry(l.clone()).or_default().insert(path.clone());
            self.new_targets.insert(format!("name:{l}"));
        }
        if body.latex_main {
            self.planned_mains.insert(path.clone());
        } else {
            self.planned_mains.remove(path);
        }
        self.index_records.insert(path.clone(), std::sync::Arc::new(body));
    }

    /// Add an observed relation unless one of this kind between these
    /// endpoints exists (any origin/status) or the vocabulary refuses it.
    /// Returns whether the relation exists afterwards.
    pub(crate) fn relate(
        &mut self,
        kind: RelationKind,
        from: &Entity,
        to: &Entity,
        extractor: &str,
    ) -> StoreResult<bool> {
        if from.id == to.id || !kind.allows(from.kind, to.kind) {
            return Ok(false);
        }
        let key = (kind, from.id.clone(), to.id.clone());
        if self.add_keys.contains_key(&key) {
            return Ok(true);
        }
        let fresh_endpoint = self.created_now.contains(&from.id) || self.created_now.contains(&to.id);
        if !fresh_endpoint && let Some(existing) = self.store.find_relation(kind, &from.id, &to.id)? {
            if self.remove.remove(&existing.id) {
                // Kept after all.
                self.unmark_removed(&existing.id);
            }
            return Ok(true);
        }
        let relation = Relation {
            id: RelationId::mint(),
            kind,
            from: from.id.clone(),
            to: to.id.clone(),
            origin: Origin::Extracted { extractor: extractor.to_owned() },
            status: AssertionStatus::Observed,
            evidence_kind: None,
            evidence: Vec::new(),
            from_version: None,
            to_version: None,
            note: None,
            revision: 1,
            created_at_ms: self.now,
            updated_at_ms: self.now,
        };
        if self.delta.relations_added.len() < DELTA_LIMIT {
            self.delta.relations_added.push(relation.id.clone());
        } else {
            self.delta.truncated = true;
        }
        self.report.relations_added += 1;
        self.add_keys.insert(key, self.add.len());
        self.adds_from.entry(relation.from.clone()).or_default().push(self.add.len());
        self.add.push(Some(relation));
        Ok(true)
    }

    fn unmark_removed(&mut self, id: &RelationId) {
        self.delta.relations_removed.retain(|r| r != id);
        self.report.relations_removed = self.report.relations_removed.saturating_sub(1);
    }

    /// Remove a relation (observed relations only; callers check origin).
    pub(crate) fn unrelate(&mut self, r: &Relation) {
        let key = (r.kind, r.from.clone(), r.to.clone());
        if let Some(&pos) = self.add_keys.get(&key)
            && self.add[pos].as_ref().is_some_and(|a| a.id == r.id)
        {
            self.add[pos] = None;
            self.add_keys.remove(&key);
            self.report.relations_added = self.report.relations_added.saturating_sub(1);
            self.delta.relations_added.retain(|x| x != &r.id);
            return;
        }
        if self.remove.insert(r.id.clone()) {
            if self.delta.relations_removed.len() < DELTA_LIMIT {
                self.delta.relations_removed.push(r.id.clone());
            } else {
                self.delta.truncated = true;
            }
            self.report.relations_removed += 1;
        }
    }

    pub(crate) fn is_removed(&self, id: &RelationId) -> bool {
        self.remove.contains(id)
    }

    /// Planned relations added in this pass from `from`.
    pub(crate) fn added_from(&self, from: &EntityId) -> Vec<Relation> {
        self.adds_from.get(from).map(|ix| ix.iter().filter_map(|&i| self.add[i].clone()).collect()).unwrap_or_default()
    }

    pub(crate) fn set_coverage(&mut self, c: ResourceCoverage, stamp: Option<Stamp>) {
        self.remove_coverage.remove(&c.path);
        self.coverage.insert(c.path.clone(), (c, stamp));
    }

    // ----- the pass -----

    fn plan(
        &mut self,
        scan_out: &ScanOutcome,
        covered: &Covered,
        checkpoints: &BTreeMap<WorkspacePath, Checkpoint>,
        fresh: &BTreeMap<WorkspacePath, Fresh>,
    ) -> StoreResult<()> {
        // Current resources (fresh state under the lock).
        let mut active: BTreeMap<WorkspacePath, EntityId> = BTreeMap::new();
        let mut created: HashMap<EntityId, u64> = HashMap::new();
        let mut tombs: HashMap<WorkspacePath, Vec<EntityId>> = HashMap::new();
        let mut duplicates: Vec<(WorkspacePath, EntityId)> = Vec::new();
        for k in self.store.resource_keys(None)? {
            if k.tombstoned {
                tombs.entry(k.path).or_default().push(k.id);
                continue;
            }
            created.insert(k.id.clone(), k.created_at_ms);
            match active.get(&k.path) {
                // Two live identities for one path (written outside the
                // service's checks): the oldest stays the file's resource.
                Some(existing) if created.get(existing).is_some_and(|c| (*c, existing) <= (k.created_at_ms, &k.id)) => {
                    duplicates.push((k.path, k.id));
                }
                Some(existing) => {
                    duplicates.push((k.path.clone(), existing.clone()));
                    active.insert(k.path, k.id);
                }
                None => {
                    active.insert(k.path, k.id);
                }
            }
        }
        for (path, id) in &active {
            self.resources.insert(path.clone(), id.clone());
        }
        // Never leave two live identities claiming one file: the newer ones
        // are tombstoned with the reason (their curated relations stay, and
        // show the identity as gone); nothing is merged into the kept one.
        for (path, id) in duplicates {
            let entity = self.must(&id)?;
            let kept = active.get(&path).map(|k| k.to_string()).unwrap_or_default();
            self.tombstone(&entity, format!("a second identity for {path}; {kept} is this file's resource"));
            let mut bindings = self.bindings_of(&id)?;
            for b in bindings.iter_mut() {
                b.state = BindingState::Missing;
                b.detail = Some(format!("Another identity ({kept}) is the resource of {path}."));
            }
            self.set_bindings(&id, bindings);
            for r in self.store.relations_from(&id)? {
                if matches!(r.origin, Origin::Extracted { .. }) {
                    self.unrelate(&r);
                }
            }
            for r in self.store.relations_to(&id)? {
                if r.kind == RelationKind::Contains && matches!(r.origin, Origin::Extracted { .. }) {
                    self.unrelate(&r);
                }
            }
            self.semantic = true;
        }
        if fresh.len() > 64 {
            // Many files to index: read all bindings once instead of per file.
            let mut by_path: HashMap<WorkspacePath, Vec<SourceBinding>> = HashMap::new();
            for b in self.store.all_bindings()? {
                by_path.entry(b.path.clone()).or_default().push(b);
            }
            self.preloaded_bindings = Some(by_path);
        }

        let observed = &scan_out.files;
        let gone: Vec<WorkspacePath> =
            active.keys().filter(|p| covered.contains(p) && !observed.contains_key(*p)).cloned().collect();
        let new_paths: Vec<WorkspacePath> = observed.keys().filter(|p| !active.contains_key(*p)).cloned().collect();

        // Confirmed renames: same physical file, else unique same content.
        let renames = confirm_renames(&gone, &new_paths, checkpoints, fresh);
        let renamed_old: BTreeSet<&WorkspacePath> = renames.iter().map(|(o, _)| o).collect();
        let renamed_new: BTreeSet<&WorkspacePath> = renames.iter().map(|(_, n)| n).collect();

        let mut matches: Vec<FileMatch> = Vec::new();

        for (old, new) in &renames {
            let entity = self.must(&active[old])?;
            let meta = &observed[new];
            let mut after = entity.clone();
            after.key = Some(new.as_str().to_owned());
            after.label = crate::store::truncate(new.file_name(), 200);
            if !meta.roles.is_empty() {
                after.roles = sorted_roles(&meta.roles);
            }
            self.update(&entity, after);
            self.resources.remove(old);
            self.resources.insert(new.clone(), entity.id.clone());
            self.moved_now.insert(entity.id.clone());
            self.renames.push((old.clone(), new.clone()));
            self.report.resources_renamed += 1;
            self.structure_dirty = true;
            self.targets_added = true;
            self.remove_coverage.insert(old.clone());
            self.remove_index.insert(old.clone());
            self.semantic = true;
            // Every binding at the old path follows the file.
            self.rebind_path(old, new)?;
            self.trigger_referrers(&entity.id)?;
            if let Some(f) = fresh.get(new) {
                self.index_file(new, &entity, f, Some(old), &mut matches)?;
            }
        }

        for path in &gone {
            if renamed_old.contains(path) {
                continue;
            }
            let entity = self.must(&active[path])?;
            self.remove_resource(path, &entity)?;
        }

        for path in &new_paths {
            if renamed_new.contains(path) {
                continue;
            }
            let meta = &observed[path];
            let Some(f) = fresh.get(path) else { continue };
            // A file restored at the same path with the same content is the
            // same resource (e.g. switching back to a branch).
            let revived = match tombs.get(path) {
                Some(candidates) if f.content.hash.is_some() => {
                    let mut same: Vec<Entity> = Vec::new();
                    for id in candidates {
                        let version = self.store.bindings(id)?.first().and_then(|b| b.version.clone());
                        if version.map(|v| v.content_hash) == f.content.hash {
                            same.push(self.must(id)?);
                        }
                    }
                    (same.len() == 1).then(|| same.remove(0))
                }
                _ => None,
            };
            let entity = match revived {
                Some(t) => {
                    let mut after = t.clone();
                    after.lifecycle = Lifecycle::Active;
                    after.tombstone_reason = None;
                    if !meta.roles.is_empty() {
                        after.roles = sorted_roles(&meta.roles);
                    }
                    self.update(&t, after.clone());
                    after
                }
                None => {
                    let e = Entity {
                        id: EntityId::mint(),
                        kind: EntityKind::Resource,
                        roles: sorted_roles(&meta.roles),
                        label: crate::store::truncate(path.file_name(), 200),
                        key: Some(path.as_str().to_owned()),
                        lifecycle: Lifecycle::Active,
                        tombstone_reason: None,
                        revision: 1,
                        origin: Origin::Extracted { extractor: SCAN_EXTRACTOR.into() },
                        attributes: BTreeMap::new(),
                        created_at_ms: self.now,
                        updated_at_ms: self.now,
                    };
                    self.create(e.clone());
                    self.report.resources_added += 1;
                    e
                }
            };
            self.resources.insert(path.clone(), entity.id.clone());
            self.structure_dirty = true;
            self.targets_added = true;
            self.index_file(path, &entity, f, None, &mut matches)?;
        }

        // Changed (or forced) files at the same path.
        for (path, f) in fresh {
            if !active.contains_key(path) || renamed_old.contains(path) {
                continue;
            }
            let entity = self.must(&active[path])?;
            let checkpoint = checkpoints.get(path);
            let same = !self.force
                && f.extraction.is_none()
                && f.content.hash.is_some()
                && checkpoint.and_then(|c| c.coverage.content_hash.as_ref()) == f.content.hash.as_ref();
            if same {
                // Only the file's timestamp changed.
                self.stamps.insert(
                    path.clone(),
                    Stamp {
                        size: Some(f.meta.size),
                        modified_ms: f.meta.modified_ms,
                        identity: f.content.identity.clone(),
                    },
                );
                continue;
            }
            // Roles may change with the policy (never emptied).
            if !f.meta.roles.is_empty() && sorted_roles(&f.meta.roles) != entity.roles {
                let mut after = entity.clone();
                after.roles = sorted_roles(&f.meta.roles);
                self.update(&entity, after);
            }
            self.index_file(path, &entity, f, None, &mut matches)?;
        }

        let t_files = Instant::now();
        self.apply_matches(matches)?;
        let t_matches = Instant::now();
        if !self.new_targets.is_empty() {
            for (p, wanted) in self.store.index_wanted()? {
                if self.resources.contains_key(&p) && wanted.iter().any(|w| self.new_targets.contains(w)) {
                    self.resolve.insert(p);
                }
            }
        }
        if self.structure_dirty {
            self.plan_structure()?;
        }
        let t_structure = Instant::now();
        let paths: Vec<WorkspacePath> = self.resolve.iter().cloned().collect();
        let resolved = paths.len();
        for path in paths {
            self.resolve_links(&path)?;
        }
        if std::env::var_os("EXPOSITION_GRAPH_TRACE").is_some() {
            eprintln!(
                "plan: files {:?}, matches {:?}, structure {:?}, resolve {:?} ({resolved} files)",
                t_files - self.started,
                t_matches - t_files,
                t_structure - t_matches,
                t_structure.elapsed()
            );
        }
        Ok(())
    }

    /// Plan the unsaved text of one file (a live-buffer overlay): the same
    /// matching and resolution as a pass, never written.
    pub(crate) fn plan_overlay(&mut self, path: &WorkspacePath, resource: &Entity, f: &Fresh) -> StoreResult<()> {
        for k in self.store.resource_keys(None)? {
            if !k.tombstoned {
                self.resources.entry(k.path).or_insert(k.id);
            }
        }
        let mut matches = Vec::new();
        self.index_file(path, resource, f, None, &mut matches)?;
        self.apply_matches(matches)?;
        self.resolve_links(path)?;
        Ok(())
    }

    /// Bindings at `old` move to `new` (renamed file).
    fn rebind_path(&mut self, old: &WorkspacePath, new: &WorkspacePath) -> StoreResult<()> {
        let mut ids: BTreeSet<EntityId> = BTreeSet::new();
        for b in self.bindings_at(old)? {
            ids.insert(b.entity);
        }
        for id in ids {
            let mut bindings = self.bindings_of(&id)?;
            for b in bindings.iter_mut() {
                if &b.path == old {
                    b.path = new.clone();
                }
            }
            self.set_bindings(&id, bindings);
        }
        Ok(())
    }

    /// Resources whose observed links point at `id` (or at its declarations)
    /// are resolved again.
    fn trigger_referrers(&mut self, id: &EntityId) -> StoreResult<()> {
        for r in self.store.relations_to(id)? {
            if !matches!(r.origin, Origin::Extracted { .. }) || r.kind == RelationKind::Contains {
                continue;
            }
            if let Some(path) = self.source_path_of(&r.from)? {
                self.resolve.insert(path);
            }
        }
        Ok(())
    }

    /// The file an entity's observed links come from: a resource's own path,
    /// or the path of a declaration's binding.
    pub(crate) fn source_path_of(&self, id: &EntityId) -> StoreResult<Option<WorkspacePath>> {
        let Some(e) = self.entity(id)? else { return Ok(None) };
        if e.kind == EntityKind::Resource {
            return Ok(e.key.as_deref().and_then(|k| WorkspacePath::parse(k).ok()));
        }
        Ok(self.bindings_of(id)?.first().map(|b| b.path.clone()))
    }

    fn remove_resource(&mut self, path: &WorkspacePath, entity: &Entity) -> StoreResult<()> {
        self.tombstone(entity, format!("{path} was deleted or moved outside the project"));
        self.resources.remove(path);
        self.report.resources_removed += 1;
        self.structure_dirty = true;
        self.semantic = true;
        let mut bindings = self.bindings_of(&entity.id)?;
        for b in bindings.iter_mut() {
            b.state = BindingState::Missing;
            b.detail = Some("The file no longer exists.".into());
        }
        self.set_bindings(&entity.id, bindings);
        self.trigger_referrers(&entity.id)?;
        // Observed relations from the file, and its containment, end with it.
        for r in self.store.relations_from(&entity.id)? {
            if matches!(r.origin, Origin::Extracted { .. }) {
                self.unrelate(&r);
            }
        }
        for r in self.store.relations_to(&entity.id)? {
            if r.kind == RelationKind::Contains && matches!(r.origin, Origin::Extracted { .. }) {
                self.unrelate(&r);
            }
        }
        // Declarations and curated bindings in the file.
        let mut ids: BTreeSet<EntityId> = BTreeSet::new();
        for b in self.bindings_at(path)? {
            if b.entity != entity.id {
                ids.insert(b.entity);
            }
        }
        for id in ids {
            let Some(e) = self.entity(&id)? else { continue };
            let mut bindings = self.bindings_of(&id)?;
            for b in bindings.iter_mut().filter(|b| &b.path == path) {
                b.state = BindingState::Missing;
                b.detail = Some(format!("{path} no longer exists."));
            }
            self.set_bindings(&id, bindings);
            if is_extracted_declaration(&e) {
                self.tombstone(&e, format!("its file {path} was deleted"));
                self.report.declarations_removed += 1;
                self.trigger_referrers(&id)?;
                for r in self.store.relations_from(&id)? {
                    if matches!(r.origin, Origin::Extracted { .. }) && r.kind != RelationKind::AnchoredIn {
                        self.unrelate(&r);
                    }
                }
            }
        }
        self.remove_coverage.insert(path.clone());
        self.coverage.remove(path);
        self.remove_index.insert(path.clone());
        self.index_records.remove(path);
        Ok(())
    }

    /// Index one new/changed/renamed file: resource binding, base coverage,
    /// and (for extracted files) the declaration matching to apply.
    fn index_file(
        &mut self,
        path: &WorkspacePath,
        resource: &Entity,
        f: &Fresh,
        old_path: Option<&WorkspacePath>,
        matches: &mut Vec<FileMatch>,
    ) -> StoreResult<()> {
        self.semantic = true;
        let stamp =
            Stamp { size: Some(f.meta.size), modified_ms: f.meta.modified_ms, identity: f.content.identity.clone() };
        let version =
            f.content.hash.as_ref().map(|h| SourceVersion { content_hash: h.clone(), revision: None, saved: true });
        self.set_bindings(
            &resource.id,
            vec![SourceBinding {
                entity: resource.id.clone(),
                path: path.clone(),
                selector: SourceSelector::WholeFile,
                range: None,
                version,
                fingerprint: None,
                state: BindingState::Resolved,
                detail: f.content.problem.clone(),
            }],
        );
        let extractor = extract::extractor_for(path);
        let state = match (&f.extraction, extractor, &f.content.problem) {
            (_, None, _) => CoverageState::Unsupported,
            (Some(x), Some(_), _) if !x.partial.is_empty() => CoverageState::Partial,
            (Some(_), Some(_), _) => CoverageState::Complete,
            (None, Some(_), Some(_)) => CoverageState::Failed,
            (None, Some(_), None) => CoverageState::Complete,
        };
        let coverage = ResourceCoverage {
            path: path.clone(),
            extractor: extractor.map(str::to_owned),
            state,
            unresolved: 0,
            detail: match (extractor, &f.content.problem) {
                (Some(_), Some(p)) => Some(p.clone()),
                _ => None,
            },
            content_hash: f.content.hash.clone(),
        };
        self.base_coverage.insert(path.clone(), (coverage.clone(), stamp.clone()));
        self.set_coverage(coverage, Some(stamp));
        // Curated bindings in this file are re-anchored against the new text.
        self.reanchor_curated(path, old_path, f)?;
        match &f.extraction {
            Some(x) => {
                let known_path = old_path.unwrap_or(path);
                let known = self.known_declarations(known_path)?;
                let anchors: Vec<KnownAnchor> = known.iter().map(|k| k.anchor.clone()).collect();
                let m = matching::match_declarations(&anchors, &x.declarations);
                matches.push(FileMatch {
                    path: path.clone(),
                    resource: resource.id.clone(),
                    extraction: x.clone(),
                    hash: f.content.hash.clone(),
                    known,
                    matching: m,
                    ids: vec![None; x.declarations.len()],
                    latex_main: x.extractor.starts_with("latex/")
                        && f.content.text.as_deref().is_some_and(has_documentclass),
                });
                self.resolve.insert(path.clone());
            }
            None if extractor.is_some() && f.content.problem.is_some() => {
                // Unreadable now: its previous index no longer describes it.
                self.remove_index.insert(path.clone());
            }
            None if extractor.is_some() && old_path.is_some() => {
                // Renamed with identical content and no fresh extraction:
                // keep the stored index under the new path.
                if let Some(old) = old_path
                    && let Some(record) = self.store.source_index(old)?
                    && let Ok(body) = serde_json::from_str::<IndexBody>(&record.body)
                {
                    self.index_hashes.insert(path.clone(), (record.content_hash.clone(), record.extractor.clone()));
                    self.put_index_record(path, body);
                    self.resolve.insert(path.clone());
                }
            }
            None => {}
        }
        Ok(())
    }

    /// Extracted declarations bound in `path` (live ones, then recent tombstones).
    fn known_declarations(&self, path: &WorkspacePath) -> StoreResult<Vec<Known>> {
        let mut live = Vec::new();
        let mut tombs = Vec::new();
        let mut seen = BTreeSet::new();
        for b in self.bindings_at(path)? {
            if !seen.insert(b.entity.clone()) {
                continue;
            }
            let Some(e) = self.entity(&b.entity)? else { continue };
            if !is_extracted_declaration(&e) {
                continue;
            }
            let SourceSelector::Declaration { syntax, name, .. } = &b.selector else { continue };
            let (context, body, scope) =
                b.fingerprint.as_deref().and_then(matching::decode_fingerprint).unwrap_or_default();
            let anchor = KnownAnchor {
                kind: e.kind,
                syntax: syntax.clone(),
                name: name.clone(),
                body,
                context,
                scope,
                tombstoned: e.lifecycle == Lifecycle::Tombstoned,
            };
            let k = Known { entity: e, anchor };
            if k.anchor.tombstoned { tombs.push(k) } else { live.push(k) }
        }
        tombs.sort_by_key(|k| std::cmp::Reverse(k.entity.updated_at_ms));
        tombs.truncate(MAX_TOMBSTONE_CANDIDATES);
        live.extend(tombs);
        Ok(live)
    }

    /// Apply the matchings of all files indexed in this pass. Leftovers of
    /// different files are paired when a declared name moved between files
    /// (unique on both sides); everything else is created or tombstoned.
    fn apply_matches(&mut self, mut matches: Vec<FileMatch>) -> StoreResult<()> {
        // Cross-file moves of named declarations.
        let mut gone_by_name: HashMap<(String, EntityKind), Vec<(usize, usize)>> = HashMap::new();
        let mut new_by_name: HashMap<(String, EntityKind), Vec<(usize, usize)>> = HashMap::new();
        for (fi, fm) in matches.iter().enumerate() {
            for &k in &fm.matching.unmatched_known {
                if let Some(n) = &fm.known[k].anchor.name {
                    gone_by_name.entry((n.clone(), fm.known[k].anchor.kind)).or_default().push((fi, k));
                }
            }
            for &n in &fm.matching.unmatched_new {
                if let Some(name) = &fm.extraction.declarations[n].name {
                    new_by_name.entry((name.clone(), fm.extraction.declarations[n].kind)).or_default().push((fi, n));
                }
            }
        }
        let mut moved: Vec<((usize, usize), (usize, usize))> = Vec::new();
        for (key, olds) in &gone_by_name {
            if let Some(news) = new_by_name.get(key)
                && olds.len() == 1
                && news.len() == 1
                && olds[0].0 != news[0].0
            {
                moved.push((olds[0], news[0]));
            }
        }
        let mut moved_known: HashMap<(usize, usize), Entity> = HashMap::new();
        for ((ofi, ok), (nfi, nn)) in &moved {
            let e = matches[*ofi].known[*ok].entity.clone();
            matches[*ofi].matching.unmatched_known.retain(|k| k != ok);
            matches[*nfi].matching.unmatched_new.retain(|n| n != nn);
            moved_known.insert((*nfi, *nn), e);
        }

        #[allow(clippy::needless_range_loop)] // `matches[fi]` is mutated while `self` is borrowed.
        for fi in 0..matches.len() {
            let path = matches[fi].path.clone();
            let resource = self.must(&matches[fi].resource)?;
            let extractor = matches[fi].extraction.extractor.clone();
            if self.lake_roots_cache.is_none() {
                self.lake_roots_cache = Some(self.lake_roots()?);
            }
            let lean_module = lean_module(&path, self.lake_roots_cache.as_deref().unwrap_or(&[]));
            let hash = matches[fi].hash.clone();
            let mut problems: Vec<String> = Vec::new();
            let decls = matches[fi].extraction.declarations.clone();
            let unnamed_ordinals = ordinals(&decls);
            // Pairs: same entity, new anchor.
            let pairs = matches[fi].matching.pairs.clone();
            for (k, n) in pairs {
                let before = matches[fi].known[k].entity.clone();
                let before = self.entity(&before.id)?.unwrap_or(before);
                let after = declaration_entity(&before, &decls[n], &path, lean_module.as_deref());
                if before.lifecycle == Lifecycle::Tombstoned {
                    self.report.declarations_added += 1;
                }
                self.update(&before, after);
                self.bind_declaration(&before.id, &decls[n], unnamed_ordinals[n], &path, hash.as_deref());
                self.anchor(&before.id, &resource)?;
                matches[fi].ids[n] = Some(before.id.clone());
            }
            // Moved in from another file.
            let created = matches[fi].matching.unmatched_new.clone();
            for n in 0..decls.len() {
                if let Some(before) = moved_known.get(&(fi, n)).cloned() {
                    let before = self.entity(&before.id)?.unwrap_or(before);
                    let after = declaration_entity(&before, &decls[n], &path, lean_module.as_deref());
                    self.update(&before, after);
                    self.bind_declaration(&before.id, &decls[n], unnamed_ordinals[n], &path, hash.as_deref());
                    self.anchor(&before.id, &resource)?;
                    self.trigger_referrers(&before.id)?;
                    matches[fi].ids[n] = Some(before.id.clone());
                }
            }
            // New declarations.
            for n in created {
                let template = Entity {
                    id: EntityId::mint(),
                    kind: decls[n].kind,
                    roles: Vec::new(),
                    label: String::new(),
                    key: None,
                    lifecycle: Lifecycle::Active,
                    tombstone_reason: None,
                    revision: 1,
                    origin: Origin::Extracted { extractor: extractor.clone() },
                    attributes: BTreeMap::new(),
                    created_at_ms: self.now,
                    updated_at_ms: self.now,
                };
                let e = declaration_entity(&template, &decls[n], &path, lean_module.as_deref());
                self.create(e.clone());
                self.report.declarations_added += 1;
                self.bind_declaration(&e.id, &decls[n], unnamed_ordinals[n], &path, hash.as_deref());
                self.anchor(&e.id, &resource)?;
                matches[fi].ids[n] = Some(e.id.clone());
            }
            // Ambiguous names: the existing entity is kept, nothing is chosen.
            let ambiguous = matches[fi].matching.ambiguous_known.clone();
            for (k, count) in ambiguous {
                let id = matches[fi].known[k].entity.id.clone();
                let name = matches[fi].known[k].anchor.name.clone().unwrap_or_default();
                let mut bindings = self.bindings_of(&id)?;
                for b in bindings.iter_mut().filter(|b| b.path == path) {
                    b.state = BindingState::Ambiguous;
                    b.range = None;
                    b.version =
                        hash.as_ref().map(|h| SourceVersion { content_hash: h.clone(), revision: None, saved: true });
                    b.detail = Some(format!("“{name}” is declared {count} times in {path}; none was chosen."));
                }
                self.set_bindings(&id, bindings);
            }
            for (name, count) in &matches[fi].matching.ambiguous_names {
                problems.push(format!("“{name}” is declared {count} times"));
            }
            // Removed declarations.
            let removed = matches[fi].matching.unmatched_known.clone();
            for k in removed {
                let e = matches[fi].known[k].entity.clone();
                let e = self.entity(&e.id)?.unwrap_or(e);
                let renamed_kind = decls.iter().any(|d| d.name.is_some() && d.name == matches[fi].known[k].anchor.name);
                let reason = if renamed_kind {
                    format!("its declaration in {path} changed kind")
                } else {
                    format!("removed from {path}")
                };
                self.tombstone(&e, reason);
                self.report.declarations_removed += 1;
                let mut bindings = self.bindings_of(&e.id)?;
                for b in bindings.iter_mut().filter(|b| b.path == path) {
                    b.state = BindingState::Missing;
                    b.range = None;
                    b.detail = Some(format!("No longer declared in {path}."));
                }
                self.set_bindings(&e.id, bindings);
                self.trigger_referrers(&e.id)?;
                for r in self.store.relations_from(&e.id)? {
                    if matches!(r.origin, Origin::Extracted { .. }) && r.kind != RelationKind::AnchoredIn {
                        self.unrelate(&r);
                    }
                }
            }
            if !problems.is_empty() {
                self.binding_problems.entry(path.clone()).or_default().extend(problems);
            }
            let fm = &matches[fi];
            let body = IndexBody { extraction: fm.extraction.clone(), ids: fm.ids.clone(), latex_main: fm.latex_main };
            self.index_hashes
                .insert(path.clone(), (fm.hash.clone().unwrap_or_default(), fm.extraction.extractor.clone()));
            self.put_index_record(&path, body);
            self.remove_index.remove(&path);
        }
        Ok(())
    }

    /// The declaration's binding (one, in `path`).
    fn bind_declaration(
        &mut self,
        id: &EntityId,
        d: &extract::Declaration,
        ordinal: Option<u32>,
        path: &WorkspacePath,
        hash: Option<&str>,
    ) {
        self.set_bindings(
            id,
            vec![SourceBinding {
                entity: id.clone(),
                path: path.clone(),
                selector: SourceSelector::Declaration { syntax: d.syntax.clone(), name: d.name.clone(), ordinal },
                range: Some(d.range),
                version: hash.map(|h| SourceVersion { content_hash: h.to_owned(), revision: None, saved: true }),
                fingerprint: Some(matching::encode_fingerprint(d)),
                state: BindingState::Resolved,
                detail: None,
            }],
        );
    }

    /// `anchoredIn` of a declaration: exactly the resource it is bound in.
    fn anchor(&mut self, id: &EntityId, resource: &Entity) -> StoreResult<()> {
        let Some(e) = self.entity(id)? else { return Ok(()) };
        let stored = if self.created_now.contains(id) { Vec::new() } else { self.store.relations_from(id)? };
        for r in stored {
            if r.kind == RelationKind::AnchoredIn && r.to != resource.id && matches!(r.origin, Origin::Extracted { .. })
            {
                self.unrelate(&r);
            }
        }
        self.relate(RelationKind::AnchoredIn, &e, resource, SCAN_EXTRACTOR)?;
        Ok(())
    }

    /// Re-anchor curated bindings (people, agents, presets, services) in a
    /// changed file: whole-file bindings follow the file; declaration
    /// bindings resolve against the new declarations; region bindings are
    /// found again by their text. Unresolvable ones become missing/ambiguous.
    fn reanchor_curated(
        &mut self,
        path: &WorkspacePath,
        old_path: Option<&WorkspacePath>,
        f: &Fresh,
    ) -> StoreResult<()> {
        let mut ids: BTreeSet<EntityId> = BTreeSet::new();
        for p in [Some(path), old_path].into_iter().flatten() {
            for b in self.bindings_at(p)? {
                ids.insert(b.entity);
            }
        }
        let text = f.content.text.as_deref();
        let decls = f.extraction.as_ref().map(|x| x.declarations.as_slice()).unwrap_or(&[]);
        let ordinals = ordinals(decls);
        let version =
            f.content.hash.as_ref().map(|h| SourceVersion { content_hash: h.clone(), revision: None, saved: true });
        let mut problems = Vec::new();
        for id in ids {
            let Some(e) = self.entity(&id)? else { continue };
            if e.kind == EntityKind::Resource || is_extracted_declaration(&e) || e.kind == EntityKind::Folder {
                continue;
            }
            let mut bindings = self.bindings_of(&id)?;
            let mut changed = false;
            for b in bindings.iter_mut().filter(|b| &b.path == path || Some(&b.path) == old_path) {
                changed = true;
                b.path = path.clone();
                match b.selector.clone() {
                    SourceSelector::WholeFile => {
                        b.state = BindingState::Resolved;
                        b.version = version.clone();
                        b.detail = None;
                    }
                    SourceSelector::Declaration { syntax, name, ordinal } => {
                        let found: Vec<usize> = (0..decls.len())
                            .filter(|&i| {
                                decls[i].syntax == syntax
                                    && match (&name, ordinal) {
                                        (Some(n), _) => decls[i].name.as_deref() == Some(n.as_str()),
                                        (None, Some(o)) => ordinals[i] == Some(o),
                                        (None, None) => true,
                                    }
                            })
                            .collect();
                        match found.as_slice() {
                            [i] => {
                                b.state = BindingState::Resolved;
                                b.range = Some(decls[*i].range);
                                b.fingerprint = Some(matching::encode_fingerprint(&decls[*i]));
                                b.detail = None;
                            }
                            [] if f.extraction.is_none() => {
                                b.state = BindingState::Unresolved;
                                b.detail = Some("The file could not be indexed.".into());
                            }
                            [] => {
                                b.state = BindingState::Missing;
                                b.range = None;
                                b.detail = Some(format!("No matching {syntax} declaration in {path}."));
                            }
                            many => {
                                b.state = BindingState::Ambiguous;
                                b.range = None;
                                b.detail =
                                    Some(format!("{} matching declarations in {path}; none was chosen.", many.len()));
                            }
                        }
                        b.version = version.clone();
                    }
                    SourceSelector::Region => {
                        let Some(text) = text else {
                            b.state = BindingState::Unresolved;
                            b.detail = Some("The file could not be read as text.".into());
                            continue;
                        };
                        match (&b.fingerprint, b.range) {
                            (None, Some(range)) => match super::anchor::region_fingerprint(text, range) {
                                // First sight: the range refers to this text.
                                Some(fp) => {
                                    b.fingerprint = Some(fp);
                                    b.state = BindingState::Resolved;
                                    b.detail = None;
                                }
                                None => {
                                    b.state = BindingState::Missing;
                                    b.detail = Some("The selected range is not in the file.".into());
                                }
                            },
                            (Some(fp), _) => match super::anchor::locate(text, fp) {
                                super::anchor::Located::Found(r) => {
                                    b.range = Some(r);
                                    b.state = BindingState::Resolved;
                                    b.detail = None;
                                }
                                super::anchor::Located::Missing => {
                                    b.state = BindingState::Missing;
                                    b.detail = Some("The bound text was changed or removed.".into());
                                }
                                super::anchor::Located::Ambiguous(n) => {
                                    b.state = BindingState::Ambiguous;
                                    b.detail = Some(format!("The bound text now occurs {n} times; none was chosen."));
                                }
                            },
                            (None, None) => {
                                b.state = BindingState::Missing;
                                b.detail = Some("The region has no range.".into());
                            }
                        }
                        b.version = version.clone();
                    }
                }
                if b.state != BindingState::Resolved {
                    problems.push(format!("{}: {}", e.label, b.detail.clone().unwrap_or_default()));
                }
            }
            if changed {
                self.set_bindings(&id, bindings);
            }
        }
        if !problems.is_empty() {
            self.binding_problems.entry(path.clone()).or_default().extend(problems);
        }
        Ok(())
    }

    /// Folders that hold a Lake package (`lakefile.lean`/`lakefile.toml`).
    pub(crate) fn lake_roots(&self) -> StoreResult<Vec<Option<WorkspacePath>>> {
        let mut roots: Vec<Option<WorkspacePath>> = self
            .resources
            .keys()
            .filter(|p| matches!(p.file_name(), "lakefile.lean" | "lakefile.toml"))
            .map(|p| p.parent())
            .collect();
        roots.sort();
        roots.dedup();
        if roots.is_empty() {
            roots.push(None);
        }
        Ok(roots)
    }

    pub(crate) fn finish(self) -> Plan {
        let mut entities: Vec<Entity> =
            self.write_entities.iter().filter_map(|id| self.entities.get(id).cloned()).collect();
        entities.sort_by(|a, b| a.id.cmp(&b.id));
        let mut bindings: Vec<(EntityId, Vec<SourceBinding>)> = self.bindings.into_iter().collect();
        bindings.sort_by(|a, b| a.0.cmp(&b.0));
        let mut index = Vec::new();
        let mut index_updates = Vec::new();
        for (path, body) in &self.index_records {
            let (hash, extractor) = self.index_hashes.get(path).cloned().unwrap_or_default();
            if let Ok(json) = serde_json::to_string(&**body) {
                index.push(IndexRecord { path: path.clone(), content_hash: hash, extractor, body: json });
                index_updates.push((path.clone(), Some((body.extraction.labels.clone(), body.latex_main))));
            }
        }
        for p in &self.remove_index {
            if !self.index_records.contains_key(p) {
                index_updates.push((p.clone(), None));
            }
        }
        let semantic = self.semantic
            || self.coverage.values().any(|(c, _)| c.content_hash.is_some())
            || !self.remove_coverage.is_empty();
        Plan {
            entities,
            bindings,
            add: self.add.into_iter().flatten().collect(),
            remove: self.remove.into_iter().collect(),
            coverage: self.coverage.into_values().collect(),
            stamps: self.stamps.into_iter().collect(),
            remove_coverage: self.remove_coverage.into_iter().collect(),
            index,
            remove_index: self.remove_index.iter().filter(|p| !self.index_records.contains_key(*p)).cloned().collect(),
            wanted: self.wanted.into_iter().collect(),
            index_updates,
            delta: self.delta,
            report: self.report,
            semantic,
            clear_pending: None,
        }
    }
}

/// The key under which a file waits for a link target (see `wanted`).
pub(crate) fn target_key(kind: EntityKind, key: &str) -> String {
    match kind {
        EntityKind::Resource | EntityKind::Folder => format!("path:{key}"),
        _ => format!("name:{key}"),
    }
}

fn sorted_roles(roles: &[ResourceRole]) -> Vec<ResourceRole> {
    let mut r = roles.to_vec();
    r.sort();
    r.dedup();
    r
}

/// An entity created by an extractor for a declaration (not a resource or folder).
pub(crate) fn is_extracted_declaration(e: &Entity) -> bool {
    matches!(e.origin, Origin::Extracted { .. }) && !matches!(e.kind, EntityKind::Resource | EntityKind::Folder)
}

/// A LaTeX main file: a line starting with `\documentclass` (not commented out).
fn has_documentclass(text: &str) -> bool {
    text.lines().any(|l| l.trim_start().starts_with(r"\documentclass"))
}

/// Ordinal of each unnamed declaration among the unnamed ones of its syntax.
pub(crate) fn ordinals(decls: &[extract::Declaration]) -> Vec<Option<u32>> {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    decls
        .iter()
        .map(|d| {
            if d.name.is_some() {
                return None;
            }
            let c = counts.entry(d.syntax.as_str()).or_insert(0);
            let o = *c;
            *c += 1;
            Some(o)
        })
        .collect()
}

/// The Lean module of a file: its path below the Lake package root, without
/// `.lean`, separated by dots.
pub(crate) fn lean_module(path: &WorkspacePath, lake_roots: &[Option<WorkspacePath>]) -> Option<String> {
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("lean")) {
        return None;
    }
    let mut best: Option<&str> = None;
    for root in lake_roots {
        let rel = match root {
            None => Some(path.as_str()),
            Some(r) => path.as_str().strip_prefix(r.as_str()).and_then(|rest| rest.strip_prefix('/')),
        };
        if let Some(rel) = rel
            && best.is_none_or(|b| rel.len() < b.len())
        {
            best = Some(rel);
        }
    }
    let rel = best?;
    Some(rel.strip_suffix(".lean").unwrap_or(rel).replace('/', "."))
}

/// The record of a declaration entity after (re)extraction.
fn declaration_entity(
    before: &Entity,
    d: &extract::Declaration,
    path: &WorkspacePath,
    lean_module: Option<&str>,
) -> Entity {
    let mut e = before.clone();
    e.kind = d.kind;
    e.lifecycle = Lifecycle::Active;
    e.tombstone_reason = None;
    e.key = d.name.clone();
    e.label = declaration_label(d, path);
    let mut attributes = BTreeMap::new();
    for (k, v) in d.attributes.iter().take(24) {
        let key_ok = !k.is_empty()
            && k.len() <= 64
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
        if key_ok {
            let v: String = v.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect();
            attributes.insert(k.clone(), crate::store::truncate(&v, 1000));
        }
    }
    if let Some(m) = lean_module
        && d.kind == EntityKind::FormalDeclaration
    {
        attributes.insert("lean.module".into(), crate::store::truncate(m, 500));
    }
    if let Some(t) = &d.title {
        attributes.insert("source.title".into(), crate::store::truncate(t, 500));
    }
    e.attributes = attributes;
    e
}

/// A concise label: the kind and the title or name the source gives it.
pub(crate) fn declaration_label(d: &extract::Declaration, path: &WorkspacePath) -> String {
    let clean = |s: &str| -> String {
        let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        one.chars().filter(|c| !c.is_control()).collect()
    };
    let label = match (d.kind, &d.title, &d.name) {
        (EntityKind::FormalDeclaration, _, Some(n)) => clean(n),
        (EntityKind::Reference, Some(t), _) if !t.trim().is_empty() => clean(t),
        (EntityKind::Reference, _, Some(n)) => clean(n),
        (k, Some(t), _) if !t.trim().is_empty() => format!("{}: {}", k.title(), clean(t)),
        (k, None, Some(n)) | (k, Some(_), Some(n)) => format!("{} {}", k.title(), clean(n)),
        (k, _, None) => match &d.context {
            Some(c) if !c.trim().is_empty() => format!("{} ({}, {})", k.title(), path.file_name(), clean(c)),
            _ => format!("{} ({})", k.title(), path.file_name()),
        },
    };
    let label = crate::store::truncate(&label, 190);
    if label.trim().is_empty() { d.kind.title().to_owned() } else { label }
}

/// Confirmed renames among files that disappeared and appeared in one pass.
fn confirm_renames(
    gone: &[WorkspacePath],
    new: &[WorkspacePath],
    checkpoints: &BTreeMap<WorkspacePath, Checkpoint>,
    fresh: &BTreeMap<WorkspacePath, Fresh>,
) -> Vec<(WorkspacePath, WorkspacePath)> {
    let mut out = Vec::new();
    let mut taken_old: BTreeSet<WorkspacePath> = BTreeSet::new();
    let mut taken_new: BTreeSet<WorkspacePath> = BTreeSet::new();
    type Key = fn(Option<&Checkpoint>, Option<&Fresh>) -> (Option<String>, Option<String>);
    let keys: [Key; 2] = [
        |c, f| (c.and_then(|c| c.identity.clone()), f.and_then(|f| f.content.identity.clone())),
        |c, f| (c.and_then(|c| c.coverage.content_hash.clone()), f.and_then(|f| f.content.hash.clone())),
    ];
    for key in keys {
        let mut olds: HashMap<String, Vec<&WorkspacePath>> = HashMap::new();
        for g in gone.iter().filter(|g| !taken_old.contains(*g)) {
            if let (Some(k), _) = key(checkpoints.get(g), None) {
                olds.entry(k).or_default().push(g);
            }
        }
        let mut news: HashMap<String, Vec<&WorkspacePath>> = HashMap::new();
        for n in new.iter().filter(|n| !taken_new.contains(*n)) {
            if let (_, Some(k)) = key(None, fresh.get(n)) {
                news.entry(k).or_default().push(n);
            }
        }
        let mut pairs: Vec<(WorkspacePath, WorkspacePath)> = Vec::new();
        for (k, o) in &olds {
            if let Some(n) = news.get(k)
                && o.len() == 1
                && n.len() == 1
            {
                pairs.push((o[0].clone(), n[0].clone()));
            }
        }
        pairs.sort();
        for (o, n) in pairs {
            taken_old.insert(o.clone());
            taken_new.insert(n.clone());
            out.push((o, n));
        }
    }
    out
}
