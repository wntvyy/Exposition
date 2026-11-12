//! The project graph service: one graph context per open project, consumed
//! by Editor, Research, Agents, verification, and workflows.
//!
//! Opening a project attaches its graph without running any project code. A
//! damaged or unavailable graph never prevents source editing: graph
//! operations then fail with `graph.unavailable` instead of claiming success.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::ids::{EntityId, EvidenceId, ProposalId, ViewId};
use crate::journal::OperationJournal;
use crate::model::{
    AssertionStatus, BindingState, CoverageState, Entity, EntityDetail, Evidence, Freshness, GraphEvent,
    GraphFreshness, GraphState, GraphStatus, Lifecycle, Origin, RecoveryOption, Relation, SyncState,
};
use crate::ops::{ApplyChangeRequest, ApplyOutcome, ChangeProposal, ChangeRequest};
use crate::query::{
    DEFAULT_LIMIT, Direction, FormalAssociationState, GraphQuery, GraphSnapshot, GraphView, GraphViewInput,
    GraphViewSummary, ImpactAction, ImpactItem, ImpactReport, ImpactRequest, ImpactStep, InformalState,
    InspectAtSourceRequest, InspectedEntity, MAX_DEPTH, MAX_LIMIT, MathOverview, MathRow, SourceInspection, ViewScope,
};
use crate::store::{DATABASE_FILE, GraphStore, StoreError, Writer, now_ms};
use crate::validate::{self, BatchOutcome, ServiceBatch};
use crate::vocabulary::{EntityFamily, EntityKind, EvidenceKind, RelationKind};

/// Error for graph requests: a stable code and a message safe to show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
#[error("{code}: {message}")]
pub struct GraphError {
    pub code: String,
    pub message: String,
}

impl GraphError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into() }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::new("graph.unavailable", reason)
    }
}

impl From<StoreError> for GraphError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Untrusted(m) | StoreError::Corrupt(m) => {
                GraphError::new("graph.damaged", format!("The project graph is damaged: {m}"))
            }
            other => GraphError::new("graph.storage", other.to_string()),
        }
    }
}

pub type GraphResult<T> = Result<T, GraphError>;

/// Folder inside a project that holds the graph.
pub const PROJECT_DIR: &str = ".exposition";

/// Paths that belong to the graph's own storage (never indexed as project
/// resources, never reported by the file watcher as source changes).
pub fn is_graph_storage(path: &WorkspacePath) -> bool {
    let p = path.as_str();
    let Some(rest) = p.strip_prefix(".exposition/") else { return false };
    rest.starts_with("graph.sqlite3") || rest.starts_with("graph-backups/") || rest.starts_with("graph.damaged")
}

#[derive(Clone, Debug)]
pub struct GraphConfig {
    /// Application data directory (operation journal, temporary graphs).
    pub data_dir: PathBuf,
}

struct OpenGraph {
    root: PathBuf,
    store: GraphStore,
    temporary: Option<String>,
}

struct Inner {
    open: Option<OpenGraph>,
    status: GraphStatus,
    /// Folder whose graph is unavailable (kept for recovery actions).
    failed_root: Option<PathBuf>,
}

struct ProposalCache {
    order: VecDeque<ProposalId>,
    map: HashMap<ProposalId, (ChangeProposal, Option<crate::ids::ProjectId>)>,
}

const MAX_PROPOSALS: usize = 256;

/// Shared project graph service.
pub struct GraphService {
    config: GraphConfig,
    inner: Mutex<Inner>,
    proposals: Mutex<ProposalCache>,
    events: broadcast::Sender<GraphEvent>,
    journal: OperationJournal,
    /// Freshness reported by the reconciler (it owns this state).
    sync: Mutex<GraphFreshness>,
}

impl GraphService {
    pub fn new(config: GraphConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        Arc::new(Self {
            journal: OperationJournal::new(&config.data_dir),
            config,
            inner: Mutex::new(Inner { open: None, status: GraphStatus::detached(), failed_root: None }),
            proposals: Mutex::new(ProposalCache { order: VecDeque::new(), map: HashMap::new() }),
            events,
            sync: Mutex::new(GraphFreshness {
                state: SyncState::Pending,
                pending: 0,
                checkpoint_ms: None,
                overlay: Vec::new(),
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<GraphEvent> {
        self.events.subscribe()
    }

    pub fn journal(&self) -> &OperationJournal {
        &self.journal
    }

    pub fn data_dir(&self) -> &Path {
        &self.config.data_dir
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, event: GraphEvent) {
        let _ = self.events.send(event);
    }

    // ----- lifecycle -----

    /// Attach the graph of the project at `root` (canonical folder). Creates
    /// `.exposition/graph.sqlite3` when missing. Never runs project code.
    /// Failures leave the graph `Unavailable` with recovery options; the
    /// project itself stays usable.
    pub fn attach(&self, root: &Path) -> GraphStatus {
        let status = {
            let mut inner = self.lock();
            if inner.open.as_ref().is_some_and(|o| o.root == root) {
                return inner.status.clone();
            }
            inner.open = None;
            inner.failed_root = None;
            match open_project_graph(root, &self.config.data_dir) {
                Ok(mut open) => {
                    if let Err(e) = ensure_project_entity(&mut open) {
                        inner.status = unavailable_status(
                            root,
                            format!("The project graph could not be prepared: {e}"),
                            vec![RecoveryOption::Retry],
                        );
                        inner.failed_root = Some(root.to_path_buf());
                    } else {
                        inner.open = Some(open);
                        inner.status = self.compute_status(&inner);
                    }
                }
                Err((reason, recovery)) => {
                    inner.status = unavailable_status(root, reason, recovery);
                    inner.failed_root = Some(root.to_path_buf());
                }
            }
            inner.status.clone()
        };
        self.proposals_clear();
        self.emit(GraphEvent::Status { status: status.clone() });
        status
    }

    /// Detach (project closed or switched). Pending proposals are dropped.
    pub fn detach(&self) {
        {
            let mut inner = self.lock();
            inner.open = None;
            inner.failed_root = None;
            inner.status = GraphStatus::detached();
        }
        self.proposals_clear();
        *self.sync.lock().unwrap_or_else(|p| p.into_inner()) =
            GraphFreshness { state: SyncState::Pending, pending: 0, checkpoint_ms: None, overlay: Vec::new() };
        self.emit(GraphEvent::Status { status: GraphStatus::detached() });
    }

    /// Recover an unavailable graph with an explicit option. `StartNew` keeps
    /// the damaged database aside (renamed), never deleted.
    pub fn recover(&self, option: &RecoveryOption) -> GraphResult<GraphStatus> {
        let root = {
            let inner = self.lock();
            inner
                .failed_root
                .clone()
                .ok_or_else(|| GraphError::new("graph.noRecovery", "The project graph does not need recovery."))?
        };
        match option {
            RecoveryOption::Retry => {}
            RecoveryOption::StartNew => {
                let db = root.join(PROJECT_DIR).join(DATABASE_FILE);
                if db.exists() {
                    let aside = root.join(PROJECT_DIR).join(format!("graph.damaged-{}.sqlite3", now_ms()));
                    std::fs::rename(&db, &aside).map_err(|e| {
                        GraphError::new("graph.recovery", format!("The damaged graph could not be set aside: {e}"))
                    })?;
                    for suffix in ["-journal", "-wal", "-shm"] {
                        let side = root.join(PROJECT_DIR).join(format!("{DATABASE_FILE}{suffix}"));
                        if side.exists() {
                            let _ = std::fs::rename(&side, aside.with_extension(format!("sqlite3{suffix}")));
                        }
                    }
                }
            }
            RecoveryOption::RestoreBackup { .. } => {
                return Err(GraphError::new("graph.recovery", "Restoring a backup is not available for this graph."));
            }
        }
        {
            let mut inner = self.lock();
            inner.failed_root = None;
        }
        Ok(self.attach(&root))
    }

    /// The open project folder, if a graph context is attached.
    pub fn root(&self) -> Option<PathBuf> {
        let inner = self.lock();
        inner.open.as_ref().map(|o| o.root.clone()).or_else(|| inner.failed_root.clone())
    }

    pub fn status(&self) -> GraphStatus {
        let inner = self.lock();
        if inner.open.is_some() { self.compute_status(&inner) } else { inner.status.clone() }
    }

    fn compute_status(&self, inner: &Inner) -> GraphStatus {
        let Some(open) = &inner.open else { return inner.status.clone() };
        let (entities, relations) = open.store.counts().unwrap_or((0, 0));
        let coverage = open.store.coverage_summary().unwrap_or_default();
        GraphStatus {
            state: match &open.temporary {
                Some(reason) => GraphState::TemporaryReadOnly { reason: reason.clone() },
                None => GraphState::Attached,
            },
            project: Some(open.store.project().clone()),
            root: Some(open.root.to_string_lossy().into_owned()),
            schema_version: Some(crate::store::SCHEMA_VERSION),
            revision: open.store.revision().unwrap_or(0),
            entities,
            relations,
            freshness: self.freshness(),
            coverage,
        }
    }

    /// Synchronization state, as last reported by the reconciler.
    pub fn freshness(&self) -> GraphFreshness {
        self.sync.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Reconciler hook: report synchronization progress (emits `reconciled`
    /// when it reaches `Synchronized`).
    pub fn set_freshness(&self, freshness: GraphFreshness) {
        let synchronized = freshness.state == SyncState::Synchronized;
        *self.sync.lock().unwrap_or_else(|p| p.into_inner()) = freshness.clone();
        if synchronized {
            let revision = self.with_store(|s| Ok(s.revision()?)).unwrap_or(0);
            self.emit(GraphEvent::Reconciled { revision, freshness });
        }
    }

    /// Run `f` against the open store, or fail with `graph.unavailable`.
    pub fn with_store<T>(&self, f: impl FnOnce(&mut GraphStore) -> GraphResult<T>) -> GraphResult<T> {
        let mut inner = self.lock();
        match inner.open.as_mut() {
            Some(open) => f(&mut open.store),
            None => Err(match &inner.status.state {
                GraphState::Unavailable { reason, .. } => GraphError::unavailable(reason.clone()),
                _ => GraphError::new("graph.noProject", "No project is open."),
            }),
        }
    }

    // ----- reads -----

    pub fn snapshot(&self, query: &GraphQuery) -> GraphResult<GraphSnapshot> {
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
        if let Some(t) = &query.text
            && t.len() > 200
        {
            return Err(GraphError::new("graph.invalidQuery", "the search text is too long"));
        }
        let freshness = self.freshness();
        self.with_store(|store| {
            let (entities, total) = match &query.around {
                Some(n) => neighborhood(store, n, query, limit)?,
                None => {
                    store.find_entities(query.kinds.as_deref(), query.text.as_deref(), query.include_inactive, limit)?
                }
            };
            let ids: BTreeSet<EntityId> = entities.iter().map(|e| e.id.clone()).collect();
            let relations = store.relations_among(&ids)?;
            Ok(GraphSnapshot {
                project: Some(store.project().clone()),
                revision: store.revision()?,
                truncated: total > entities.len(),
                total: total as u32,
                entities,
                relations,
                coverage: store.coverage_summary()?,
                freshness,
            })
        })
    }

    pub fn entity_detail(&self, id: &EntityId) -> GraphResult<EntityDetail> {
        self.with_store(|store| {
            let entity = store
                .entity(id)?
                .ok_or_else(|| GraphError::new("graph.unknownEntity", format!("entity {id} does not exist")))?;
            let bindings = store.bindings(id)?;
            let outgoing = store.relations_from(id)?;
            let incoming = store.relations_to(id)?;
            let mut neighbor_ids: Vec<EntityId> =
                outgoing.iter().map(|r| r.to.clone()).chain(incoming.iter().map(|r| r.from.clone())).collect();
            neighbor_ids.sort();
            neighbor_ids.dedup();
            neighbor_ids.truncate(500);
            let neighbors = store.entities(&neighbor_ids)?;
            let mut evidence = store.evidence_for_subject(id)?;
            for r in outgoing.iter().chain(incoming.iter()) {
                for e in &r.evidence {
                    if evidence.len() >= 200 {
                        break;
                    }
                    if !evidence.iter().any(|x| &x.id == e)
                        && let Some(rec) = store.evidence(e)?
                    {
                        evidence.push(rec);
                    }
                }
            }
            let evidence_freshness = evidence.iter().map(|e| (e.id.clone(), evidence_freshness(store, e))).collect();
            let coverage = match (&entity.kind, bindings.first()) {
                (EntityKind::Resource, Some(b)) => store.coverage(&b.path)?,
                _ => None,
            };
            Ok(EntityDetail {
                entity,
                bindings,
                outgoing,
                incoming,
                neighbors,
                evidence,
                evidence_freshness,
                coverage,
                graph_revision: store.revision()?,
            })
        })
    }

    /// Entities bound at a source position (or in the whole file).
    pub fn inspect_at_source(&self, req: &InspectAtSourceRequest) -> GraphResult<SourceInspection> {
        let freshness = self.freshness();
        let unsaved =
            req.revision.is_some_and(|rev| freshness.overlay.iter().any(|o| o.path == req.path && o.revision == rev));
        self.with_store(|store| {
            let resource = store
                .entities_by_key(EntityKind::Resource, req.path.as_str())?
                .into_iter()
                .find(|e| e.lifecycle != Lifecycle::Tombstoned);
            let mut hits: Vec<(u32, InspectedEntity)> = Vec::new();
            for b in store.bindings_at_path(&req.path)? {
                let Some(entity) = store.entity(&b.entity)? else { continue };
                if entity.kind == EntityKind::Resource {
                    continue;
                }
                let width = match (req.offset, b.range) {
                    (Some(o), Some(r)) if r.start <= o && o <= r.end => r.len(),
                    (Some(_), _) => continue,
                    (None, Some(r)) => r.len(),
                    (None, None) => u32::MAX,
                };
                let mut relations = store.relations_from(&entity.id)?;
                relations.extend(store.relations_to(&entity.id)?);
                relations.truncate(200);
                let mut related_ids: Vec<EntityId> =
                    relations.iter().map(|r| if r.from == entity.id { r.to.clone() } else { r.from.clone() }).collect();
                related_ids.sort();
                related_ids.dedup();
                let related = store.entities(&related_ids)?;
                hits.push((width, InspectedEntity { entity, binding: b, relations, related }));
                if hits.len() >= 200 {
                    break;
                }
            }
            hits.sort_by_key(|(w, _)| *w);
            Ok(SourceInspection {
                path: req.path.clone(),
                revision: req.revision,
                unsaved,
                resource,
                entities: hits.into_iter().map(|(_, h)| h).collect(),
                coverage: store.coverage(&req.path)?,
                freshness: freshness.clone(),
                graph_revision: store.revision()?,
            })
        })
    }

    /// Change impact: who must review or is stale if `entities` change.
    /// Follows each relation kind in the direction its meaning propagates,
    /// bounded in depth and work; cycles are reported, not followed forever.
    pub fn impact(&self, req: &ImpactRequest) -> GraphResult<ImpactReport> {
        let max_depth = req.max_depth.unwrap_or(4).clamp(1, MAX_DEPTH);
        let max_items = req.max_items.unwrap_or(200).clamp(1, 1000) as usize;
        if req.entities.is_empty() || req.entities.len() > 100 {
            return Err(GraphError::new("graph.invalidQuery", "name 1–100 entities"));
        }
        self.with_store(|store| {
            let roots: BTreeSet<EntityId> = req.entities.iter().cloned().collect();
            let mut items: Vec<ImpactItem> = Vec::new();
            let mut seen: BTreeSet<EntityId> = roots.clone();
            let mut cycles: Vec<Vec<EntityId>> = Vec::new();
            let mut queue: VecDeque<(EntityId, u32, Vec<ImpactStep>)> =
                roots.iter().map(|r| (r.clone(), 0, Vec::new())).collect();
            let mut work = 0usize;
            let mut work_limit_reached = false;
            let mut truncated = false;
            let mut unknown: BTreeSet<WorkspacePath> = BTreeSet::new();
            while let Some((current, depth, path)) = queue.pop_front() {
                if depth >= max_depth {
                    continue;
                }
                let mut edges: Vec<(Relation, EntityId)> = Vec::new();
                for r in store.relations_to(&current)? {
                    if propagates_reverse(r.kind) && r.status != AssertionStatus::Rejected {
                        let other = r.from.clone();
                        edges.push((r, other));
                    }
                }
                for r in store.relations_from(&current)? {
                    if propagates_forward(r.kind) && r.status != AssertionStatus::Rejected {
                        let other = r.to.clone();
                        edges.push((r, other));
                    }
                }
                for (r, other) in edges {
                    work += 1;
                    if work > 20_000 {
                        work_limit_reached = true;
                        break;
                    }
                    let mut step_path = path.clone();
                    step_path.push(ImpactStep {
                        relation: r.id.clone(),
                        kind: r.kind,
                        from: r.from.clone(),
                        to: r.to.clone(),
                    });
                    if roots.contains(&other) || path.iter().any(|s| s.from == other || s.to == other) {
                        if r.kind.allows_cycles() && cycles.len() < 50 {
                            let mut cycle: Vec<EntityId> = step_path.iter().map(|s| s.from.clone()).collect();
                            cycle.push(other.clone());
                            cycle.dedup();
                            if !cycles.contains(&cycle) {
                                cycles.push(cycle);
                            }
                        }
                        continue;
                    }
                    if !seen.insert(other.clone()) {
                        continue;
                    }
                    let Some(entity) = store.entity(&other)? else { continue };
                    if entity.lifecycle == Lifecycle::Tombstoned {
                        continue;
                    }
                    for b in store.bindings(&entity.id)? {
                        if let Some(c) = store.coverage(&b.path)?
                            && c.state != CoverageState::Complete
                        {
                            unknown.insert(b.path.clone());
                        }
                    }
                    let (action, reason) = impact_reason(&r, &other, store)?;
                    if items.len() >= max_items {
                        truncated = true;
                        break;
                    }
                    items.push(ImpactItem { entity, depth: depth + 1, path: step_path.clone(), action, reason });
                    queue.push_back((other, depth + 1, step_path));
                }
                if truncated || work_limit_reached {
                    break;
                }
            }
            Ok(ImpactReport {
                roots: req.entities.clone(),
                items,
                cycles,
                truncated,
                work_limit_reached,
                unknown_coverage: unknown.into_iter().take(200).collect(),
                graph_revision: store.revision()?,
            })
        })
    }

    /// The Mathematics projection: explicit claims, assumptions, proofs, and
    /// formal declarations with separate informal state, formal association,
    /// exact verifier verdict, and freshness.
    pub fn math_overview(&self) -> GraphResult<MathOverview> {
        self.with_store(|store| {
            let kinds: Vec<EntityKind> =
                EntityKind::ALL.into_iter().filter(|k| k.family() == EntityFamily::Mathematics).collect();
            let (entities, total) = store.find_entities(Some(&kinds), None, false, 2000)?;
            let ids: BTreeSet<EntityId> = entities.iter().map(|e| e.id.clone()).collect();
            let relations: Vec<Relation> =
                store.relations_among(&ids)?.into_iter().filter(|r| r.status != AssertionStatus::Rejected).collect();
            let mut rows = Vec::with_capacity(entities.len());
            for entity in entities {
                let binding = store.bindings(&entity.id)?.into_iter().next();
                let informal = if entity.kind.is_statement() {
                    let proofs: Vec<&Relation> =
                        relations.iter().filter(|r| r.kind == RelationKind::ProofOf && r.to == entity.id).collect();
                    if proofs.iter().any(|r| r.status != AssertionStatus::Suggested) {
                        InformalState::ProofLinked
                    } else if !proofs.is_empty() {
                        InformalState::ProofSuggested
                    } else {
                        InformalState::NoProof
                    }
                } else {
                    InformalState::NotApplicable
                };
                let formal_links: Vec<&Relation> = relations
                    .iter()
                    .filter(|r| r.kind == RelationKind::Formalizes && (r.to == entity.id || r.from == entity.id))
                    .collect();
                let formal_association = if formal_links.iter().any(|r| r.status == AssertionStatus::Accepted) {
                    FormalAssociationState::Reviewed
                } else if !formal_links.is_empty() {
                    FormalAssociationState::Suggested
                } else {
                    FormalAssociationState::None
                };
                let formal_declarations: Vec<EntityId> = if entity.kind == EntityKind::FormalDeclaration {
                    vec![entity.id.clone()]
                } else {
                    formal_links.iter().filter(|r| r.to == entity.id).map(|r| r.from.clone()).collect()
                };
                // Latest formal evidence of the (associated) declaration(s).
                let mut latest: Option<Evidence> = None;
                for d in &formal_declarations {
                    for ev in store.evidence_for_subject(d)? {
                        if ev.kind == EvidenceKind::FormalCheck
                            && latest.as_ref().is_none_or(|l| ev.recorded_at_ms > l.recorded_at_ms)
                        {
                            latest = Some(ev);
                        }
                    }
                }
                let verdict = latest
                    .as_ref()
                    .and_then(|e| e.formal.as_ref())
                    .map(|f| f.declaration_verdict.unwrap_or(f.file_verdict));
                let freshness = latest.as_ref().map(|e| evidence_freshness(store, e));
                let formal_dependencies =
                    latest.as_ref().and_then(|e| e.formal.as_ref()).map(|f| f.dependencies.clone());
                rows.push(MathRow {
                    entity,
                    binding,
                    informal,
                    formal_association,
                    formal_declarations,
                    verdict,
                    freshness,
                    formal_dependencies,
                    evidence: latest,
                });
            }
            Ok(MathOverview {
                truncated: total > rows.len(),
                rows,
                relations,
                coverage: store.coverage_summary()?,
                graph_revision: store.revision()?,
            })
        })
    }

    // ----- changes -----

    /// Validate a change without applying it; valid proposals are kept
    /// (bounded) for `apply_change`.
    pub fn propose_change(&self, origin: Origin, request: &ChangeRequest) -> GraphResult<ChangeProposal> {
        if let Some(s) = &request.summary
            && s.len() > 500
        {
            return Err(GraphError::new("graph.invalidRequest", "the summary is too long"));
        }
        let (project, run) = self.with_store(|store| {
            let project = store.project().clone();
            let w = Writer::new(store.transaction()?);
            let run = validate::run_operations(&w, &origin, &request.operations, false)?;
            // Dropped without commit: nothing changes.
            Ok((project, run))
        })?;
        let proposal = ChangeProposal {
            id: ProposalId::mint(),
            base_revision: request.base_revision,
            origin,
            operations: request.operations.clone(),
            valid: run.valid(),
            diagnostics: run.diagnostics,
            effects: run.effects,
            created_at_ms: now_ms(),
        };
        if proposal.valid {
            let mut cache = self.proposals.lock().unwrap_or_else(|p| p.into_inner());
            cache.order.push_back(proposal.id.clone());
            cache.map.insert(proposal.id.clone(), (proposal.clone(), Some(project)));
            while cache.order.len() > MAX_PROPOSALS {
                if let Some(old) = cache.order.pop_front() {
                    cache.map.remove(&old);
                }
            }
        }
        Ok(proposal)
    }

    /// Look up a stored proposal (for review surfaces).
    pub fn proposal(&self, id: &ProposalId) -> Option<ChangeProposal> {
        self.proposals.lock().unwrap_or_else(|p| p.into_inner()).map.get(id).map(|(p, _)| p.clone())
    }

    /// Apply a stored proposal as one graph transaction. Only the origin that
    /// proposed it may apply it, in the same project; it is re-validated
    /// against the current graph and applied entirely or not at all.
    pub fn apply_change(&self, origin: &Origin, request: &ApplyChangeRequest) -> GraphResult<ApplyOutcome> {
        let (proposal, project) = {
            let cache = self.proposals.lock().unwrap_or_else(|p| p.into_inner());
            cache.map.get(&request.proposal).cloned().ok_or_else(|| {
                GraphError::new("graph.unknownProposal", "That change is no longer pending; propose it again.")
            })?
        };
        if &proposal.origin != origin {
            return Err(GraphError::new(
                "graph.unknownProposal",
                "That change is no longer pending; propose it again.",
            ));
        }
        let outcome = self.with_store(|store| {
            if Some(store.project()) != project.as_ref() {
                return Err(GraphError::new(
                    "graph.projectChanged",
                    "The project changed since this change was proposed.",
                ));
            }
            let current = store.revision()?;
            let w = Writer::new(store.transaction()?);
            let mut run = validate::run_operations(&w, origin, &proposal.operations, true)?;
            if !run.valid() {
                // Dropping `w` rolls everything back.
                return Ok(if run.conflict {
                    ApplyOutcome::Conflict { current_revision: current, diagnostics: run.diagnostics }
                } else {
                    ApplyOutcome::Invalid { current_revision: current, diagnostics: run.diagnostics }
                });
            }
            let summary = run.effects.join("; ");
            let revision = w.record_change(origin, &summary, &mut run.delta)?;
            w.commit()?;
            Ok(ApplyOutcome::Applied {
                revision,
                rebased: current != proposal.base_revision || request.expected_revision != current,
                created: run.created,
                delta: run.delta,
            })
        })?;
        if let ApplyOutcome::Applied { delta, .. } = &outcome {
            self.proposals.lock().unwrap_or_else(|p| p.into_inner()).map.remove(&request.proposal);
            self.emit(GraphEvent::Changed { delta: delta.clone(), origin: origin.clone() });
        }
        Ok(outcome)
    }

    /// Propose and immediately apply (used where the caller already holds the
    /// person's approval of exactly these operations, e.g. a validated preset).
    pub fn propose_and_apply(
        &self,
        origin: Origin,
        request: &ChangeRequest,
    ) -> GraphResult<(ChangeProposal, ApplyOutcome)> {
        let proposal = self.propose_change(origin.clone(), request)?;
        if !proposal.valid {
            let current = self.with_store(|s| Ok(s.revision()?))?;
            let diagnostics = proposal.diagnostics.clone();
            return Ok((proposal, ApplyOutcome::Invalid { current_revision: current, diagnostics }));
        }
        let outcome = self.apply_change(
            &origin,
            &ApplyChangeRequest { proposal: proposal.id.clone(), expected_revision: request.base_revision },
        )?;
        Ok((proposal, outcome))
    }

    /// Internal write path for services (indexing, research, Lean,
    /// attachment, blueprint materialization). Not exposed over IPC.
    pub fn apply_service_batch(&self, origin: &Origin, batch: &ServiceBatch) -> GraphResult<BatchOutcome> {
        if matches!(origin, Origin::User | Origin::Agent { .. }) {
            return Err(GraphError::new("graph.notService", "service batches are not available to people or agents"));
        }
        let outcome = self.with_store(|store| {
            let w = Writer::new(store.transaction()?);
            let (mut run, relation_ids) =
                validate::run_batch(&w, origin, batch).map_err(|m| GraphError::new("graph.invalidBatch", m))?;
            let entity_ids: Vec<EntityId> = run.created.iter().map(|c| c.id.clone()).collect();
            if run.delta.is_empty()
                && batch.evidence.is_empty()
                && batch.coverage.is_empty()
                && batch.remove_coverage.is_empty()
            {
                let revision = store_revision_in(&w)?;
                return Ok(BatchOutcome { revision, delta: run.delta, entity_ids, relation_ids });
            }
            let summary = if batch.summary.is_empty() { "Service update".to_owned() } else { batch.summary.clone() };
            let revision = w.record_change(origin, &summary, &mut run.delta)?;
            w.commit()?;
            Ok(BatchOutcome { revision, delta: run.delta, entity_ids, relation_ids })
        })?;
        if !outcome.delta.is_empty() {
            self.emit(GraphEvent::Changed { delta: outcome.delta.clone(), origin: origin.clone() });
        }
        Ok(outcome)
    }

    /// Record one immutable evidence record (service-owned kinds included).
    pub fn register_evidence(&self, origin: &Origin, evidence: Evidence) -> GraphResult<EvidenceId> {
        let id = evidence.id.clone();
        let batch = ServiceBatch { evidence: vec![evidence], summary: "Record evidence".into(), ..Default::default() };
        self.apply_service_batch(origin, &batch)?;
        Ok(id)
    }

    // ----- resources -----

    /// The resource entity of a project file, if the graph knows it (any
    /// lifecycle except tombstoned).
    pub fn resource(&self, path: &WorkspacePath) -> GraphResult<Option<Entity>> {
        self.with_store(|store| {
            Ok(store
                .entities_by_key(EntityKind::Resource, path.as_str())?
                .into_iter()
                .find(|e| e.lifecycle != Lifecycle::Tombstoned))
        })
    }

    /// Resolve the resource identity of an existing project file, admitting
    /// it when the inclusion policy allows (`explicit` also admits generated
    /// outputs that are being retained or attached). The file must exist in
    /// the project (callers resolve the path through the workspace first and
    /// pass its current content hash when known). Excluded paths fail with
    /// `graph.excluded` and the policy's reason.
    pub fn ensure_resource(
        &self,
        path: &WorkspacePath,
        content_hash: Option<&str>,
        explicit: bool,
        service: &str,
    ) -> GraphResult<Entity> {
        let roles = match crate::admission::classify(path) {
            crate::admission::Admission::Admit(roles) => roles,
            crate::admission::Admission::OnRequest(roles) if explicit => roles,
            crate::admission::Admission::OnRequest(_) => {
                return Err(GraphError::new(
                    "graph.excluded",
                    format!(
                        "{path} is a generated output; it becomes a project resource when it is retained or attached"
                    ),
                ));
            }
            crate::admission::Admission::Exclude(reason) => {
                return Err(GraphError::new("graph.excluded", format!("{path} is not a project resource: {reason}")));
            }
        };
        if let Some(existing) = self.resource(path)? {
            return Ok(existing);
        }
        let origin = Origin::Service { service: service.to_owned() };
        let binding = crate::model::SourceBinding {
            entity: EntityId::mint(),
            path: path.clone(),
            selector: crate::model::SourceSelector::WholeFile,
            range: None,
            version: content_hash.map(|h| crate::model::SourceVersion {
                content_hash: h.to_owned(),
                revision: None,
                saved: true,
            }),
            fingerprint: None,
            state: if content_hash.is_some() { BindingState::Resolved } else { BindingState::Unresolved },
            detail: None,
        };
        let batch = ServiceBatch {
            entities: vec![validate::EntityUpsert {
                id: None,
                kind: EntityKind::Resource,
                roles,
                label: crate::store::truncate(path.file_name(), 200),
                key: Some(path.as_str().to_owned()),
                attributes: BTreeMap::new(),
                lifecycle: None,
                tombstone_reason: None,
                bindings: Some(vec![binding]),
            }],
            summary: format!("Admit {path}"),
            ..Default::default()
        };
        let outcome = self.apply_service_batch(&origin, &batch)?;
        let id =
            outcome.entity_ids.first().cloned().ok_or_else(|| GraphError::new("graph.storage", "admission failed"))?;
        self.with_store(|s| s.entity(&id)?.ok_or_else(|| GraphError::new("graph.storage", "admission failed")))
    }

    // ----- views -----

    pub fn views(&self) -> GraphResult<Vec<GraphViewSummary>> {
        self.with_store(|s| Ok(s.views()?))
    }

    pub fn view(&self, id: &ViewId) -> GraphResult<GraphView> {
        self.with_store(|s| {
            s.view(id)?.ok_or_else(|| GraphError::new("graph.unknownView", "That view no longer exists."))
        })
    }

    /// Save a view (presentation only; never touches entities or relations).
    pub fn save_view(&self, input: &GraphViewInput) -> GraphResult<GraphView> {
        validate_view(input)?;
        self.with_store(|store| {
            let existing = match &input.id {
                Some(id) => store.view(id)?,
                None => None,
            };
            let revision = match (&existing, input.expected_revision) {
                (Some(v), Some(expected)) if v.revision == expected => v.revision + 1,
                (Some(v), _) => {
                    return Err(GraphError::new(
                        "graph.staleView",
                        format!("The view changed since it was opened (revision {}).", v.revision),
                    ));
                }
                (None, Some(_)) if input.id.is_some() => {
                    return Err(GraphError::new("graph.unknownView", "That view no longer exists."));
                }
                (None, _) => 1,
            };
            let view = GraphView {
                id: input.id.clone().unwrap_or_else(ViewId::mint),
                name: input.name.trim().to_owned(),
                scope: input.scope.clone(),
                nodes: input.nodes.clone(),
                viewport: input.viewport.clone(),
                filters: input.filters.clone(),
                hidden: input.hidden.clone(),
                collapsed: input.collapsed.clone(),
                revision,
                updated_at_ms: now_ms(),
            };
            store.put_view(&view)?;
            Ok(view)
        })
    }

    pub fn delete_view(&self, id: &ViewId) -> GraphResult<bool> {
        self.with_store(|s| Ok(s.delete_view(id)?))
    }

    fn proposals_clear(&self) {
        let mut cache = self.proposals.lock().unwrap_or_else(|p| p.into_inner());
        cache.order.clear();
        cache.map.clear();
    }
}

// ----- lane E hooks (blueprint/workflow modules; additive) -----

impl GraphService {
    /// Announce a change applied outside `apply_change`/`apply_service_batch`
    /// (saving a workflow revision admits/updates its entity).
    pub(crate) fn emit_changed(&self, delta: crate::model::GraphDelta, origin: Origin) {
        if !delta.is_empty() {
            self.emit(GraphEvent::Changed { delta, origin });
        }
    }
}

fn store_revision_in(w: &Writer<'_>) -> GraphResult<u64> {
    Ok(w.revision()?)
}

fn unavailable_status(root: &Path, reason: String, recovery: Vec<RecoveryOption>) -> GraphStatus {
    GraphStatus {
        state: GraphState::Unavailable { reason, recovery },
        root: Some(root.to_string_lossy().into_owned()),
        ..GraphStatus::detached()
    }
}

/// Open the project's graph database, or a temporary in-memory graph for a
/// folder that cannot hold one.
fn open_project_graph(root: &Path, _data_dir: &Path) -> Result<OpenGraph, (String, Vec<RecoveryOption>)> {
    let dir = root.join(PROJECT_DIR);
    let temporary = |reason: String| -> Result<OpenGraph, (String, Vec<RecoveryOption>)> {
        let store = GraphStore::open_in_memory().map_err(|e| (e.to_string(), vec![RecoveryOption::Retry]))?;
        Ok(OpenGraph { root: root.to_path_buf(), store, temporary: Some(reason) })
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return temporary(format!(
            "The folder cannot hold project metadata ({e}); graph changes are not saved with the project."
        ));
    }
    // `.exposition` must be a real folder inside the project, and the
    // database a regular file (no links leading elsewhere).
    match (std::fs::canonicalize(root), std::fs::canonicalize(&dir), std::fs::symlink_metadata(&dir)) {
        (Ok(r), Ok(d), Ok(meta)) if d.starts_with(&r) && !meta.file_type().is_symlink() => {}
        _ => {
            return Err((
                "The project's .exposition folder is a link or leaves the project; the graph was not opened.".into(),
                vec![RecoveryOption::Retry],
            ));
        }
    }
    let db = dir.join(DATABASE_FILE);
    let exists = match std::fs::symlink_metadata(&db) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            return Err((
                "The project graph is not a regular file; it was not opened.".into(),
                vec![RecoveryOption::Retry],
            ));
        }
        Ok(_) => true,
        Err(_) => false,
    };
    if !exists && std::fs::metadata(&dir).map(|m| m.permissions().readonly()).unwrap_or(false) {
        return temporary("The project folder is read-only; graph changes are not saved with the project.".into());
    }
    match GraphStore::open(&db, !exists) {
        Ok(store) => Ok(OpenGraph { root: root.to_path_buf(), store, temporary: None }),
        Err(StoreError::NewerSchema { found, supported }) => Err((
            format!(
                "The project graph was written by a newer version of Exposition (schema {found}; this version reads {supported}). It was left unchanged."
            ),
            vec![RecoveryOption::Retry],
        )),
        Err(e @ (StoreError::Corrupt(_) | StoreError::Untrusted(_))) => {
            Err((format!("{e}. It was left unchanged."), vec![RecoveryOption::StartNew, RecoveryOption::Retry]))
        }
        Err(StoreError::Sqlite(e)) if !exists => temporary(format!(
            "The project graph could not be created ({e}); graph changes are not saved with the project."
        )),
        Err(e) => Err((
            format!("The project graph could not be opened: {e}"),
            vec![RecoveryOption::StartNew, RecoveryOption::Retry],
        )),
    }
}

/// Every graph has exactly one project entity.
fn ensure_project_entity(open: &mut OpenGraph) -> Result<(), StoreError> {
    let existing = open.store.find_entities(Some(&[EntityKind::Project]), None, true, 1)?;
    if !existing.0.is_empty() {
        return Ok(());
    }
    let name = open.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".into());
    let batch = ServiceBatch {
        entities: vec![validate::EntityUpsert {
            id: None,
            kind: EntityKind::Project,
            roles: Vec::new(),
            label: crate::store::truncate(&name, 200),
            key: None,
            attributes: BTreeMap::new(),
            lifecycle: None,
            tombstone_reason: None,
            bindings: None,
        }],
        summary: "Create the project graph".into(),
        ..Default::default()
    };
    let origin = Origin::Service { service: "workspace".into() };
    let w = Writer::new(open.store.transaction()?);
    let (mut run, _) = validate::run_batch(&w, &origin, &batch).map_err(StoreError::Corrupt)?;
    w.record_change(&origin, "Create the project graph", &mut run.delta)?;
    w.commit()
}

fn neighborhood(
    store: &GraphStore,
    n: &crate::query::Neighborhood,
    query: &GraphQuery,
    limit: usize,
) -> Result<(Vec<Entity>, usize), StoreError> {
    let depth = n.depth.min(MAX_DEPTH);
    let Some(start) = store.entity(&n.entity)? else { return Ok((Vec::new(), 0)) };
    let mut seen: BTreeSet<EntityId> = BTreeSet::from([start.id.clone()]);
    let mut out = vec![start];
    let mut frontier = vec![n.entity.clone()];
    let mut total = 1usize;
    for _ in 0..depth {
        let mut next = Vec::new();
        for id in &frontier {
            let mut rels = Vec::new();
            if matches!(n.direction, Direction::Outgoing | Direction::Both) {
                rels.extend(store.relations_from(id)?.into_iter().map(|r| (r.to, r.kind)));
            }
            if matches!(n.direction, Direction::Incoming | Direction::Both) {
                rels.extend(store.relations_to(id)?.into_iter().map(|r| (r.from, r.kind)));
            }
            for (other, kind) in rels {
                if let Some(kinds) = &n.relation_kinds
                    && !kinds.contains(&kind)
                {
                    continue;
                }
                if !seen.insert(other.clone()) {
                    continue;
                }
                let Some(e) = store.entity(&other)? else { continue };
                if !query.include_inactive && e.lifecycle != Lifecycle::Active {
                    continue;
                }
                if let Some(kinds) = &query.kinds
                    && !kinds.contains(&e.kind)
                {
                    continue;
                }
                total += 1;
                if out.len() < limit {
                    out.push(e);
                }
                next.push(other);
            }
        }
        frontier = next;
        if frontier.is_empty() || total > 20_000 {
            break;
        }
    }
    Ok((out, total))
}

/// Relation kinds whose meaning propagates from `to` back to `from` when `to` changes.
fn propagates_reverse(kind: RelationKind) -> bool {
    use RelationKind::*;
    matches!(
        kind,
        DependsOn | Assumes | ProofOf | References | UsesAsset | Imports | DerivedFrom | EvidenceFor | Formalizes
    )
}

/// Relation kinds whose meaning propagates from `from` to `to` when `from` changes.
fn propagates_forward(kind: RelationKind) -> bool {
    use RelationKind::*;
    matches!(kind, ProofOf | EvidenceFor | Formalizes)
}

fn impact_reason(r: &Relation, affected: &EntityId, store: &GraphStore) -> Result<(ImpactAction, String), StoreError> {
    let other = if &r.from == affected { &r.to } else { &r.from };
    let other_label = store.entity(other)?.map(|e| e.label).unwrap_or_else(|| other.to_string());
    let reverse = &r.from == affected;
    let suggested = if r.status == AssertionStatus::Suggested { " (suggested relationship)" } else { "" };
    use RelationKind::*;
    let (action, text) = match (r.kind, reverse) {
        (DependsOn, _) => (
            ImpactAction::NeedsReview,
            format!(
                "depends on “{other_label}”, which changed; review it (a changed dependency does not make it false)"
            ),
        ),
        (Assumes, _) => (ImpactAction::NeedsReview, format!("assumes “{other_label}”, which changed")),
        (ProofOf, true) => {
            (ImpactAction::NeedsReview, format!("is a proof of “{other_label}”, whose statement changed"))
        }
        (ProofOf, false) => (ImpactAction::NeedsReview, format!("its proof “{other_label}” changed")),
        (References, _) => {
            (ImpactAction::CitationReview, format!("references “{other_label}”, which changed; check the citation"))
        }
        (UsesAsset, _) | (Imports, _) => {
            (ImpactAction::Stale, format!("uses “{other_label}”, which changed; its build is out of date"))
        }
        (DerivedFrom, _) => {
            (ImpactAction::Stale, format!("was derived from “{other_label}”, which changed; it reflects the old input"))
        }
        (EvidenceFor, true) => (
            ImpactAction::NeedsReview,
            format!("is evidence for “{other_label}”, which changed; the evidence applies to the old version"),
        ),
        (EvidenceFor, false) => (ImpactAction::NeedsReview, format!("its evidence “{other_label}” changed")),
        (Formalizes, true) => {
            (ImpactAction::NeedsReview, format!("formalizes “{other_label}”, which changed; review the correspondence"))
        }
        (Formalizes, false) => (
            ImpactAction::NeedsReview,
            format!("its formal declaration “{other_label}” changed; review the correspondence"),
        ),
        _ => (ImpactAction::Informational, format!("related to “{other_label}”")),
    };
    Ok((action, format!("{text}{suggested}")))
}

/// Whether evidence still applies to the current sources: its inputs' hashes
/// against the indexed versions. Missing data is Unknown, never Current.
pub fn evidence_freshness(store: &GraphStore, evidence: &Evidence) -> Freshness {
    let mut stale = Vec::new();
    let mut unknown = None;
    let mut inputs: Vec<(WorkspacePath, Option<String>)> =
        evidence.inputs.iter().map(|i| (i.path.clone(), i.content_hash.clone())).collect();
    if let Some(f) = &evidence.formal {
        inputs.push((f.source_path.clone(), Some(f.source_digest.clone())));
    }
    for (path, hash) in inputs {
        let Some(hash) = hash else {
            unknown = Some(format!("{path}: the input version was not recorded"));
            continue;
        };
        match store.coverage(&path) {
            Ok(Some(c)) => match c.content_hash {
                Some(current) if current == hash => {}
                Some(_) => stale.push(format!("{path} changed since it was checked")),
                None => unknown = Some(format!("{path} has not been indexed")),
            },
            Ok(None) => unknown = Some(format!("{path} is not indexed (deleted or never seen)")),
            Err(_) => unknown = Some(format!("{path}: unreadable index")),
        }
    }
    if !stale.is_empty() {
        stale.dedup();
        return Freshness::Stale { reasons: stale };
    }
    if let Some(reason) = unknown {
        return Freshness::Unknown { reason };
    }
    if let Some(f) = &evidence.formal
        && f.dependency_fingerprints.is_none()
    {
        return Freshness::Unknown {
            reason: "the audit does not fingerprint the declaration's dependencies; changes in imported modules are not detected".into(),
        };
    }
    Freshness::Current
}

fn finite(v: f64, bound: f64) -> bool {
    v.is_finite() && v.abs() <= bound
}

fn validate_view(input: &GraphViewInput) -> GraphResult<()> {
    let bad = |m: &str| Err(GraphError::new("graph.invalidView", m));
    if let Err(m) = validate::check_label(&input.name) {
        return Err(GraphError::new("graph.invalidView", m));
    }
    if input.nodes.len() > 5000 || input.hidden.len() > 5000 || input.collapsed.len() > 5000 {
        return bad("a view can hold at most 5000 nodes");
    }
    for n in &input.nodes {
        if !finite(n.x, 1e7) || !finite(n.y, 1e7) {
            return bad("node positions must be finite numbers");
        }
        for d in [n.width, n.height].into_iter().flatten() {
            if !d.is_finite() || !(1.0..=10_000.0).contains(&d) {
                return bad("node sizes must be between 1 and 10000");
            }
        }
    }
    let vp = &input.viewport;
    if !finite(vp.x, 1e8) || !finite(vp.y, 1e8) || !vp.zoom.is_finite() || !(0.05..=8.0).contains(&vp.zoom) {
        return bad("the viewport is invalid");
    }
    if let Some(t) = &input.filters.text
        && t.len() > 200
    {
        return bad("the filter text is too long");
    }
    if let ViewScope::Local { depth, .. } = input.scope
        && depth > MAX_DEPTH
    {
        return bad("the local depth is too large");
    }
    Ok(())
}

/// A binding's state is `Resolved` only when reconciliation confirmed it.
pub fn is_resolved(state: BindingState) -> bool {
    state == BindingState::Resolved
}
