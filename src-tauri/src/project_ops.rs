//! P1 project operations: ONE orchestrator for delete / duplicate / paste /
//! restore across project files (the workspace service's file engine,
//! `exposition_workspace::ops`) and project data (the `project_data` seam:
//! graph, workflows, runs, tasks, research records).
//!
//! - `plan` merges the file side and the data side into one [`OpPlan`]: one
//!   list of effects, refusals and concrete impacts, one combined decision
//!   (`needs_decision`) and a hash over everything the decision rests on
//!   (entries, sizes, times, open buffers, the data side's fingerprint).
//!   Plans are kept for [`PLAN_TTL_MS`] and are single-use.
//! - `apply` re-plans from the stored request and refuses (`ops.planStale`,
//!   nothing applied) when anything material changed; then it records the
//!   intent, lets the data side stop/disable what the user chose
//!   (`prepare_paths`), applies each target independently, applies the data
//!   targets, and hands the ACTUAL outcomes to the data side (`after_paths`).
//!   Partial success is reported as it is.
//! - `restore` brings back a trashed entry the app identified reliably, never
//!   overwriting anything.
//!
//! Interruption: the intent and each target's outcome are written to the
//! app's operation journal (the graph's cross-service journal in app data)
//! before and while files change. A record still pending after a restart is
//! reconciled against the files actually present: the data side receives
//! the outcomes that really happened, staging leftovers are removed, and the
//! record is resolved. Nothing is replayed. The watcher and the graph's own
//! reconciliation keep file facts current in the meantime.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use exposition_core::ops::{
    OpActor, OpApply, OpError, OpImpact, OpKind, OpOutcome, OpPlan, OpRecovery, OpRefusal, OpRequest, OpRestore,
    OpResult, OpStatus, OpTarget, StorageOverview,
};
use exposition_core::path::WorkspacePath;
use exposition_graph::{OperationId, OperationJournal, OperationKind, OperationRecord, OperationState};
use exposition_workspace::WorkspaceService;
use exposition_workspace::fsutil::{now_millis, sha256_hex};
use exposition_workspace::ops::{ApplyOptions, OsTrash, PathOutcome, PathPlan, PlanHooks, TrashBin, TrashedItem};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::project_data::{self, DataPlan};
use crate::state::AppState;

/// Event after every apply/restore (payload [`OpResult`]) so views refresh.
pub const PROJECT_OP_EVENT: &str = "project-op";
/// How long a plan can be applied.
pub const PLAN_TTL_MS: u64 = 15 * 60 * 1000;
/// Targets per operation (keeps the intent record bounded).
pub const MAX_TARGETS: usize = 1000;
/// Restore tokens kept in memory per session (oldest dropped first).
const MAX_RESTORES: usize = 2000;
/// Restore records kept in the operation journal per project (newest kept).
pub const MAX_RESTORABLE: usize = 500;
/// Path outcomes journaled per write (bounds journal rewrites for large selections).
const JOURNAL_BATCH: usize = 16;
/// Marks the operation journal records this module writes.
const RECORD_PREFIX: &str = "Project operation";

/// The data side as the orchestrator uses it: the app forwards to
/// `project_data` (lane L2); tests use a double.
pub trait DataSide {
    fn impacts_for_paths(&self, paths: &[WorkspacePath], kind: &OpKind) -> DataPlan;
    fn plan_data(&self, request: &OpRequest) -> DataPlan;
    fn apply_data(&self, plan: &OpPlan, choices: &BTreeMap<String, String>) -> Vec<OpOutcome>;
    fn prepare_paths(&self, plan: &OpPlan, choices: &BTreeMap<String, String>) -> Vec<OpRefusal>;
    fn after_paths(&self, outcomes: &[OpOutcome]);
    /// Restore an app-managed recoverable deletion by its `<kind>:<id>`
    /// token; `None` when the token is not the data side's.
    fn restore_data(&self, restore: &OpRestore) -> Option<OpOutcome>;
    /// The project graph keeps `path` as a resource (a retained research
    /// output under `.exposition/artifacts/` is then deletable as a file).
    fn graph_resource(&self, path: &WorkspacePath) -> bool;
}

/// The application's data side.
pub struct AppData<'a>(pub &'a AppState);

impl DataSide for AppData<'_> {
    fn impacts_for_paths(&self, paths: &[WorkspacePath], kind: &OpKind) -> DataPlan {
        project_data::impacts_for_paths(self.0, paths, kind)
    }
    fn plan_data(&self, request: &OpRequest) -> DataPlan {
        project_data::plan_data(self.0, request)
    }
    fn apply_data(&self, plan: &OpPlan, choices: &BTreeMap<String, String>) -> Vec<OpOutcome> {
        project_data::apply_data(self.0, plan, choices)
    }
    fn prepare_paths(&self, plan: &OpPlan, choices: &BTreeMap<String, String>) -> Vec<OpRefusal> {
        project_data::prepare_paths(self.0, plan, choices)
    }
    fn after_paths(&self, outcomes: &[OpOutcome]) {
        project_data::after_paths(self.0, outcomes)
    }
    fn restore_data(&self, restore: &OpRestore) -> Option<OpOutcome> {
        project_data::restore_data(self.0, restore)
    }
    fn graph_resource(&self, path: &WorkspacePath) -> bool {
        self.0.graph.resource(path).ok().flatten().is_some()
    }
}

struct StoredPlan {
    plan: OpPlan,
    root: PathBuf,
}

struct StoredRestore {
    root: PathBuf,
    path: WorkspacePath,
    item: TrashedItem,
    order: u64,
    /// The journal record holding the persisted copy of this restore record.
    record: Option<OperationId>,
}

/// A trashed entry the app identified reliably, as kept in the operation
/// journal (step `restorable`) so Undo and agent reverts survive a restart.
/// A record is only a pointer: the item is re-identified in the trash (same
/// identity and deletion time) before anything is restored, and a used,
/// vanished or expired record is marked spent (step `restoreSpent`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Restorable {
    token: String,
    /// The original project path (normalized).
    path: WorkspacePath,
    /// The trash item's identity, original location and deletion time.
    item: TrashedItem,
}

/// What a journal record of this module carries to reconcile after a restart.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Intent {
    kind: OpKind,
    tag: String,
    targets: Vec<OpTarget>,
    /// Normalized project paths the operation deletes.
    deleted: Vec<WorkspacePath>,
    /// Folders copies are staged in.
    staging: Vec<PathBuf>,
}

/// One target's actual outcome as journaled.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Done {
    outcome: OpOutcome,
    path: Option<WorkspacePath>,
}

/// Plans, restore tokens and the operation journal (managed by Tauri).
pub struct ProjectOps {
    plans: Mutex<HashMap<String, StoredPlan>>,
    restores: Mutex<HashMap<String, StoredRestore>>,
    journal: OperationJournal,
    trash: Arc<dyn TrashBin>,
    /// Records created before this instant belong to an earlier session.
    session_started_ms: u64,
    reconciled: Mutex<HashSet<PathBuf>>,
    counter: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn err(code: &str, message: impl Into<String>) -> OpError {
    OpError::new(code, message)
}

/// Every operation command takes the user's actor: agents reach the same
/// orchestrator only through their tools (existing grants and gate).
pub fn require_user(request: &OpRequest) -> Result<(), OpError> {
    if matches!(request.actor, OpActor::User) {
        Ok(())
    } else {
        Err(err("ops.invalidRequest", "Agent operations come only from agent tasks, never from the interface."))
    }
}

/// The full plan and its file part (kept for apply).
struct Computed {
    plan: OpPlan,
    paths: PathPlan,
}

impl ProjectOps {
    pub fn new(data_dir: &Path, trash: Arc<dyn TrashBin>) -> Self {
        Self {
            plans: Mutex::new(HashMap::new()),
            restores: Mutex::new(HashMap::new()),
            journal: OperationJournal::new(data_dir),
            trash,
            session_started_ms: now_millis(),
            reconciled: Mutex::new(HashSet::new()),
            counter: AtomicU64::new(0),
        }
    }

    fn mint(&self, prefix: &str) -> String {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(self.counter.fetch_add(1, Ordering::Relaxed));
        hasher.write_u64(now_millis());
        format!("{prefix}-{:x}-{:016x}", now_millis(), hasher.finish())
    }

    fn root(ws: &WorkspaceService) -> Result<PathBuf, OpError> {
        ws.open_root().ok_or_else(|| err("ops.noProject", "No project is open."))
    }

    /// Plan without storing it.
    fn compute(&self, ws: &WorkspaceService, data: &dyn DataSide, request: &OpRequest) -> Result<Computed, OpError> {
        if request.targets.is_empty() {
            return Err(err("ops.invalidRequest", "Nothing is selected."));
        }
        if request.targets.len() > MAX_TARGETS {
            return Err(err("ops.invalidRequest", format!("At most {MAX_TARGETS} items can be changed at once.")));
        }
        let root = Self::root(ws)?;
        let retained = |p: &WorkspacePath| data.graph_resource(p);
        let paths = ws.plan_paths_with(
            &request.kind,
            &request.targets,
            &request.actor,
            &PlanHooks { retained_artifact: &retained },
        );
        let consulted: &[WorkspacePath] =
            if matches!(request.kind, OpKind::Delete { .. }) { &paths.deleted } else { &paths.copied };
        let on_paths =
            if consulted.is_empty() { DataPlan::default() } else { data.impacts_for_paths(consulted, &request.kind) };
        let on_data = data.plan_data(request);

        let mut effects = paths.effects.clone();
        let mut refusals = paths.refusals.clone();
        let mut impacts: Vec<OpImpact> = paths.impacts.clone();
        let mut choices = paths.choices.clone();
        let mut fingerprint = paths.fingerprint.clone();
        for part in [on_paths, on_data] {
            effects.extend(part.effects);
            for r in part.refusals {
                if !refusals.iter().any(|x| x.target == r.target) {
                    refusals.push(r);
                }
            }
            impacts.extend(part.impacts);
            for c in part.choices {
                if !choices.iter().any(|x| x.key == c.key) {
                    choices.push(c);
                }
            }
            fingerprint.extend(part.fingerprint);
        }
        // A target refused by the data side is not changed on disk either.
        effects.retain(|e| !refusals.iter().any(|r| r.target == e.target));
        impacts.retain(|i| !refusals.iter().any(|r| r.target == i.target));
        let needs_decision = impacts.iter().any(|i| i.kind.needs_decision());

        #[derive(Serialize)]
        struct Material<'a> {
            request: &'a OpRequest,
            root: String,
            fingerprint: &'a [String],
            refusals: Vec<(&'a OpTarget, &'a str)>,
            impacts: &'a [OpImpact],
            choices: &'a [exposition_core::ops::OpChoice],
            effects: Vec<(&'a OpTarget, &'a OpRecovery)>,
        }
        let material = Material {
            request,
            root: root.display().to_string(),
            fingerprint: &fingerprint,
            refusals: refusals.iter().map(|r| (&r.target, r.code.as_str())).collect(),
            impacts: &impacts,
            choices: &choices,
            effects: effects.iter().map(|e| (&e.target, &e.recovery)).collect(),
        };
        let plan_hash = sha256_hex(&serde_json::to_vec(&material).map_err(|e| err("ops.failed", e.to_string()))?);
        Ok(Computed {
            plan: OpPlan {
                plan: String::new(),
                request: request.clone(),
                effects,
                refusals,
                impacts,
                needs_decision,
                choices,
                plan_hash,
                expires_at_ms: now_millis() + PLAN_TTL_MS,
            },
            paths,
        })
    }

    /// `project_op_plan`.
    pub fn plan(&self, ws: &WorkspaceService, data: &dyn DataSide, request: OpRequest) -> Result<OpPlan, OpError> {
        self.reconcile_interrupted(ws, data);
        let mut computed = self.compute(ws, data, &request)?;
        computed.plan.plan = self.mint("plan");
        let root = Self::root(ws)?;
        let mut plans = lock(&self.plans);
        let now = now_millis();
        plans.retain(|_, p| p.plan.expires_at_ms > now);
        plans.insert(computed.plan.plan.clone(), StoredPlan { plan: computed.plan.clone(), root });
        Ok(computed.plan)
    }

    /// `project_op_apply`.
    pub fn apply(&self, ws: &WorkspaceService, data: &dyn DataSide, apply: OpApply) -> Result<OpResult, OpError> {
        let stored = lock(&self.plans)
            .remove(&apply.plan)
            .ok_or_else(|| err("ops.planUnknown", "This operation is no longer available; select the items again."))?;
        let stale = || err("ops.planStale", "The selection or its consequences changed; review the operation again.");
        if stored.plan.plan_hash != apply.plan_hash || stored.plan.expires_at_ms <= now_millis() {
            return Err(stale());
        }
        if Self::root(ws)? != stored.root {
            return Err(stale());
        }
        let fresh = self.compute(ws, data, &stored.plan.request)?;
        if fresh.plan.plan_hash != stored.plan.plan_hash {
            return Err(stale());
        }
        let mut plan = fresh.plan;
        plan.plan = stored.plan.plan.clone();
        let choices = resolve_choices(&plan, &apply.choices)?;
        self.run(ws, data, &plan, &fresh.paths, &choices, &stored.root)
    }

    fn run(
        &self,
        ws: &WorkspaceService,
        data: &dyn DataSide,
        plan: &OpPlan,
        paths: &PathPlan,
        choices: &BTreeMap<String, String>,
        root: &Path,
    ) -> Result<OpResult, OpError> {
        let tag = format!("op{:x}", self.counter.fetch_add(1, Ordering::Relaxed) ^ (now_millis() & 0xffff_ffff));
        let intent = Intent {
            kind: plan.request.kind.clone(),
            tag: tag.clone(),
            targets: plan.request.targets.clone(),
            deleted: paths.deleted.clone(),
            staging: paths.staging_folders(),
        };
        let record = self.begin(root, plan, &intent)?;

        // 1. The data side stops/disables what the user chose (before files change).
        let mut skip = data.prepare_paths(plan, choices);
        skip.extend(plan.refusals.iter().cloned());

        // 2. Files, one target at a time; outcomes journaled as they happen.
        let mut pending: Vec<Done> = Vec::new();
        let mut journal_error = None;
        let path_outcomes = ws.apply_paths(
            paths,
            &ApplyOptions { choices, trash: self.trash.as_ref(), skip: &skip, tag: &tag },
            &mut |o: &PathOutcome| {
                if o.covered {
                    return;
                }
                pending.push(Done { outcome: o.outcome.clone(), path: o.path.clone() });
                if pending.len() >= JOURNAL_BATCH
                    && let Err(e) = self.record_done(root, &record, &std::mem::take(&mut pending))
                {
                    journal_error = Some(e);
                }
            },
        );
        if let Err(e) = self.record_done(root, &record, &pending) {
            journal_error = Some(e);
        }

        // 3. Data targets (workflows, runs, tasks, graph records).
        let data_outcomes = data.apply_data(plan, choices);
        let _ = self.journal.record_step(root, &record, "data", BTreeMap::new(), true, None);

        // 4. Project data follows what actually happened on disk.
        // Targets covered by a selected folder were not acted on separately.
        let actual: Vec<OpOutcome> = path_outcomes.iter().filter(|o| !o.covered).map(normalized).collect();
        data.after_paths(&actual);
        let _ = self.journal.record_step(root, &record, "graph", BTreeMap::new(), true, None);
        let _ = self.journal.complete(root, &record);

        // 5. One outcome per target, in the request's order. Reliably
        // identified trash items get a restore token, kept in memory and in
        // the operation journal (so Undo and agent reverts survive a restart).
        let mut outcomes: Vec<OpOutcome> = Vec::new();
        let mut restorable: Vec<Restorable> = Vec::new();
        for o in path_outcomes {
            let mut outcome = o.outcome;
            if let (Some(item), Some(path)) = (o.trashed, o.path) {
                let token = self.keep_restore(root, path.clone(), item.clone(), Some(record.clone()));
                restorable.push(Restorable { token: token.clone(), path, item });
                outcome.restore = Some(token);
            }
            outcomes.push(outcome);
        }
        if !restorable.is_empty() {
            let kept = serde_json::to_string(&restorable).map_err(|e| e.to_string()).and_then(|items| {
                self.journal
                    .record_step(root, &record, "restorable", BTreeMap::from([("items".to_owned(), items)]), true, None)
                    .map_err(|e| e.to_string())
            });
            if let Err(e) = kept {
                journal_error = Some(err("ops.failed", e));
            }
            self.prune_restorable(root, false);
        }
        outcomes.extend(data_outcomes);
        for r in &plan.refusals {
            if !outcomes.iter().any(|o| o.target == r.target) {
                outcomes.push(OpOutcome {
                    target: r.target.clone(),
                    status: OpStatus::Skipped,
                    code: Some(r.code.clone()),
                    message: Some(r.message.clone()),
                    created: None,
                    recovery: OpRecovery::None,
                    restore: None,
                });
            }
        }
        let order: HashMap<&OpTarget, usize> = plan.request.targets.iter().enumerate().map(|(i, t)| (t, i)).collect();
        outcomes.sort_by_key(|o| order.get(&o.target).copied().unwrap_or(usize::MAX));

        let mut notices: Vec<String> =
            plan.impacts.iter().filter(|i| !i.kind.needs_decision()).map(|i| i.message.clone()).collect();
        if let Some(e) = journal_error {
            notices.push(format!(
                "The operation's progress could not be recorded ({}); if Exposition stops now, check the project afterwards.",
                e.message
            ));
        }
        Ok(OpResult { plan: plan.plan.clone(), outcomes, notices })
    }

    fn begin(&self, root: &Path, plan: &OpPlan, intent: &Intent) -> Result<OperationId, OpError> {
        let what = match &plan.request.kind {
            OpKind::Delete { permanent: false } => "move to the trash",
            OpKind::Delete { permanent: true } => "delete permanently",
            OpKind::Duplicate => "duplicate",
            OpKind::Paste { .. } => "paste",
        };
        let n = plan.request.targets.len();
        let description = format!("{RECORD_PREFIX}: {what} {n} item{}", if n == 1 { "" } else { "s" });
        let steps = plan.request.targets.iter().map(target_text).collect();
        let record = self.journal.begin(OperationKind::CrossService, root, None, &description, steps).map_err(|e| {
            err("ops.failed", format!("The operation could not be recorded, so nothing was changed: {e}."))
        })?;
        let refs = BTreeMap::from([(
            "intent".to_owned(),
            serde_json::to_string(intent).map_err(|e| err("ops.failed", e.to_string()))?,
        )]);
        self.journal.record_step(root, &record.id, "intent", refs, true, None).map_err(|e| {
            err("ops.failed", format!("The operation could not be recorded, so nothing was changed: {e}."))
        })?;
        Ok(record.id)
    }

    fn record_done(&self, root: &Path, record: &OperationId, done: &[Done]) -> Result<(), OpError> {
        if done.is_empty() {
            return Ok(());
        }
        let refs = BTreeMap::from([(
            "done".to_owned(),
            serde_json::to_string(done).map_err(|e| err("ops.failed", e.to_string()))?,
        )]);
        self.journal
            .record_step(root, record, "paths", refs, true, None)
            .map(|_| ())
            .map_err(|e| err("ops.failed", e.to_string()))
    }

    fn keep_restore(&self, root: &Path, path: WorkspacePath, item: TrashedItem, record: Option<OperationId>) -> String {
        // `trash-…`: never a data token (`<kind>:<id>`, no colon here).
        let token = self.mint("trash");
        let mut restores = lock(&self.restores);
        let order = self.counter.fetch_add(1, Ordering::Relaxed);
        if restores.len() >= MAX_RESTORES
            && let Some(oldest) = restores.iter().min_by_key(|(_, r)| r.order).map(|(k, _)| k.clone())
        {
            restores.remove(&oldest);
        }
        restores.insert(token.clone(), StoredRestore { root: root.to_path_buf(), path, item, order, record });
        token
    }

    /// The live restore records of a project's journal, newest first (records
    /// newest first, and the last-deleted item of a record first).
    fn persisted(&self, root: &Path) -> Vec<(OperationId, Restorable)> {
        let records = self.journal.list(root);
        let mut spent: HashSet<String> = HashSet::new();
        for record in &records {
            for step in record.steps.iter().filter(|s| s.name == "restoreSpent") {
                if let Some(tokens) = step.refs.get("tokens").and_then(|t| serde_json::from_str::<Vec<String>>(t).ok())
                {
                    spent.extend(tokens);
                }
            }
        }
        let mut out = Vec::new();
        for record in &records {
            for step in record.steps.iter().filter(|s| s.name == "restorable") {
                let Some(items) = step.refs.get("items").and_then(|t| serde_json::from_str::<Vec<Restorable>>(t).ok())
                else {
                    continue;
                };
                for item in items.into_iter().rev() {
                    if !spent.contains(&item.token) {
                        out.push((record.id.clone(), item));
                    }
                }
            }
        }
        out
    }

    /// Tokens of the open project's live restore records (newest first).
    #[cfg(test)]
    pub fn persisted_tokens(&self, root: &Path) -> Vec<String> {
        self.persisted(root).into_iter().map(|(_, r)| r.token).collect()
    }

    /// Mark restore records spent (used, gone from the trash, or expired).
    fn mark_spent(&self, root: &Path, spent: Vec<(OperationId, String)>) {
        let mut by_record: HashMap<OperationId, Vec<String>> = HashMap::new();
        {
            let mut restores = lock(&self.restores);
            for (record, token) in spent {
                restores.remove(&token);
                by_record.entry(record).or_default().push(token);
            }
        }
        for (record, tokens) in by_record {
            if let Ok(text) = serde_json::to_string(&tokens) {
                let refs = BTreeMap::from([("tokens".to_owned(), text)]);
                let _ = self.journal.record_step(root, &record, "restoreSpent", refs, true, None);
            }
        }
    }

    /// Keep at most [`MAX_RESTORABLE`] live restore records per project
    /// (the newest), and with `check_trash` drop those whose item is no
    /// longer in the trash (restored or emptied elsewhere).
    fn prune_restorable(&self, root: &Path, check_trash: bool) {
        let live = self.persisted(root);
        let mut spent: Vec<(OperationId, String)> = Vec::new();
        let (kept, expired) = live.split_at(live.len().min(MAX_RESTORABLE));
        spent.extend(expired.iter().map(|(record, r)| (record.clone(), r.token.clone())));
        if check_trash && !kept.is_empty() {
            let items: Vec<TrashedItem> = kept.iter().map(|(_, r)| r.item.clone()).collect();
            if let Ok(present) = self.trash.present(&items) {
                for ((record, r), here) in kept.iter().zip(present) {
                    if !here {
                        spent.push((record.clone(), r.token.clone()));
                    }
                }
            }
        }
        if !spent.is_empty() {
            self.mark_spent(root, spent);
        }
    }

    /// `project_op_restore`.
    pub fn restore(&self, ws: &WorkspaceService, data: &dyn DataSide, restore: OpRestore) -> Result<OpResult, OpError> {
        // App-managed recoverable deletions (`<kind>:<id>`) belong to the data side.
        if let Some(outcome) = data.restore_data(&restore) {
            return Ok(OpResult { plan: restore.restore, outcomes: vec![outcome], notices: Vec::new() });
        }
        let unknown =
            || err("ops.restoreUnknown", "Exposition can no longer restore this item; look for it in the Recycle Bin.");
        let in_memory = lock(&self.restores)
            .get(&restore.restore)
            .map(|stored| (stored.root.clone(), stored.path.clone(), stored.item.clone(), stored.record.clone()));
        let (root, path, item, kept_in) = match in_memory {
            Some(found) => found,
            // From an earlier session: the open project's journal.
            None => {
                let root = Self::root(ws)?;
                let (record, found) =
                    self.persisted(&root).into_iter().find(|(_, r)| r.token == restore.restore).ok_or_else(unknown)?;
                (root, found.path, found.item, Some(record))
            }
        };
        if Self::root(ws)? != root {
            return Err(err("ops.noProject", "The project this item was deleted from is not open."));
        }
        let target = OpTarget::Path { path: path.clone() };
        let plan = OpPlan {
            plan: restore.restore.clone(),
            request: OpRequest {
                kind: OpKind::Paste { destination: exposition_core::ops::OpDestination::Project },
                targets: vec![target.clone()],
                actor: OpActor::User,
            },
            effects: Vec::new(),
            refusals: Vec::new(),
            impacts: Vec::new(),
            needs_decision: false,
            choices: Vec::new(),
            plan_hash: String::new(),
            expires_at_ms: 0,
        };
        let tag = format!("rs{:x}", self.counter.fetch_add(1, Ordering::Relaxed) ^ (now_millis() & 0xffff_ffff));
        let record = self
            .journal
            .begin(
                OperationKind::CrossService,
                &root,
                None,
                &format!("{RECORD_PREFIX}: restore {path}"),
                vec![target_text(&target)],
            )
            .map_err(|e| {
                err("ops.failed", format!("The restore could not be recorded, so nothing was changed: {e}."))
            })?;
        let intent = Intent {
            kind: plan.request.kind.clone(),
            tag: tag.clone(),
            targets: vec![target.clone()],
            deleted: Vec::new(),
            staging: vec![absolute(&root, path.parent().as_ref())],
        };
        let _ = self.journal.record_step(
            &root,
            &record.id,
            "intent",
            BTreeMap::from([("intent".to_owned(), serde_json::to_string(&intent).unwrap_or_default())]),
            true,
            None,
        );
        // The item is re-identified in the trash before anything moves.
        let outcome = ws.restore_trashed(&path, &item, restore.on_collision.as_deref(), self.trash.as_ref(), &tag);
        let _ = self.record_done(&root, &record.id, &[Done { outcome: outcome.clone(), path: Some(path.clone()) }]);
        let spent = outcome.status == OpStatus::Succeeded || outcome.code.as_deref() == Some("ops.restoreUnavailable");
        if spent {
            // Used, or gone from the trash: never offered again.
            match kept_in {
                Some(kept_in) => self.mark_spent(&root, vec![(kept_in, restore.restore.clone())]),
                None => {
                    lock(&self.restores).remove(&restore.restore);
                }
            }
        }
        if outcome.status == OpStatus::Succeeded {
            data.after_paths(std::slice::from_ref(&outcome));
        }
        let _ = self.journal.complete(&root, &record.id);
        Ok(OpResult { plan: restore.restore, outcomes: vec![outcome], notices: Vec::new() })
    }

    /// Reconcile operations an earlier session left unfinished in the open
    /// project (once per project per session). Returns what was reconciled,
    /// as results for the `project-op` event.
    pub fn reconcile_interrupted(&self, ws: &WorkspaceService, data: &dyn DataSide) -> Vec<OpResult> {
        let Some(root) = ws.open_root() else { return Vec::new() };
        if !lock(&self.reconciled).insert(root.clone()) {
            return Vec::new();
        }
        // Restore records whose item left the trash (or beyond the cap) go.
        self.prune_restorable(&root, true);
        let mut results = Vec::new();
        for record in self.journal.unresolved_before(&root, self.session_started_ms) {
            if !record.description.starts_with(RECORD_PREFIX)
                || !matches!(record.state, OperationState::NeedsReconciliation { .. })
            {
                continue;
            }
            if let Some(result) = self.reconcile_record(ws, data, &root, &record) {
                results.push(result);
            }
        }
        results
    }

    fn reconcile_record(
        &self,
        ws: &WorkspaceService,
        data: &dyn DataSide,
        root: &Path,
        record: &OperationRecord,
    ) -> Option<OpResult> {
        let intent: Intent = record
            .steps
            .iter()
            .find(|s| s.name == "intent")
            .and_then(|s| s.refs.get("intent"))
            .and_then(|text| serde_json::from_str(text).ok())?;
        let mut done: Vec<Done> = record
            .steps
            .iter()
            .filter(|s| s.name == "paths")
            .filter_map(|s| s.refs.get("done"))
            .filter_map(|text| serde_json::from_str::<Vec<Done>>(text).ok())
            .flatten()
            .collect();
        // Deletions without a recorded outcome: what is on disk now says
        // what happened.
        if matches!(intent.kind, OpKind::Delete { .. }) {
            for path in &intent.deleted {
                if done.iter().any(|d| d.path.as_ref() == Some(path)) {
                    continue;
                }
                let gone = matches!(
                    std::fs::symlink_metadata(absolute(root, Some(path))),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound
                );
                let target = OpTarget::Path { path: path.clone() };
                done.push(Done {
                    outcome: OpOutcome {
                        target,
                        status: if gone { OpStatus::Succeeded } else { OpStatus::Failed },
                        code: Some("ops.interrupted".into()),
                        message: Some(if gone {
                            format!("Exposition stopped during this deletion; {path} is no longer in the project.")
                        } else {
                            format!("Exposition stopped before {path} was deleted; it is still in the project.")
                        }),
                        created: None,
                        recovery: OpRecovery::None,
                        restore: None,
                    },
                    path: Some(path.clone()),
                });
            }
        }
        let removed = ws.remove_staging_leftovers(&intent.staging, &intent.tag);
        let actual: Vec<OpOutcome> = done.iter().map(normalized_done).collect();
        data.after_paths(&actual);
        let succeeded = actual.iter().filter(|o| o.status == OpStatus::Succeeded).count();
        let summary = format!(
            "Reconciled after a restart: {succeeded} of {} items were done{}.",
            intent.targets.len(),
            if removed > 0 { format!("; {removed} unfinished copies were removed") } else { String::new() }
        );
        let _ = self.journal.resolve(root, &record.id, &summary);
        let mut notices = vec![format!(
            "An operation was interrupted when Exposition stopped ({}). The project now reflects what was actually done: {}",
            record.description.trim_start_matches(&format!("{RECORD_PREFIX}: ")),
            summary
        )];
        if matches!(intent.kind, OpKind::Duplicate | OpKind::Paste { .. }) {
            notices.push("Copies that were completed appear as new files.".into());
        }
        Some(OpResult { plan: record.id.to_string(), outcomes: done.into_iter().map(|d| d.outcome).collect(), notices })
    }

    /// Absolute display paths of project entries, for the OS clipboard.
    pub fn clipboard_paths(ws: &WorkspaceService, paths: &[WorkspacePath]) -> Result<Vec<PathBuf>, OpError> {
        if paths.is_empty() {
            return Err(err("ops.invalidRequest", "Nothing is selected."));
        }
        let root = Self::root(ws)?;
        let plan = ws.plan_paths(
            &OpKind::Paste { destination: exposition_core::ops::OpDestination::Folder { folder: None } },
            &paths.iter().map(|p| OpTarget::Path { path: p.clone() }).collect::<Vec<_>>(),
            &OpActor::User,
        );
        // Only entries that may be copied go on the clipboard (refusals other
        // than the would-be destination's naming).
        if let Some(r) = plan.refusals.iter().find(|r| r.code != "ops.intoItself") {
            return Err(err(&r.code, r.message.clone()));
        }
        let mut out = Vec::new();
        for path in paths {
            let abs = absolute(&root, Some(path));
            if std::fs::symlink_metadata(&abs).is_err() {
                return Err(err("ops.notFound", format!("{path} does not exist.")));
            }
            out.push(PathBuf::from(exposition_workspace::root::display_path(&abs)));
        }
        Ok(out)
    }
}

/// The lexical absolute path of a project path (`None`: the root itself).
fn absolute(root: &Path, path: Option<&WorkspacePath>) -> PathBuf {
    let mut abs = root.to_path_buf();
    if let Some(path) = path {
        abs.extend(path.components());
    }
    abs
}

/// The outcome with its target as the normalized project path (what the
/// data side should reconcile), keeping the rest.
fn normalized(o: &PathOutcome) -> OpOutcome {
    let mut outcome = o.outcome.clone();
    if let Some(path) = &o.path {
        outcome.target = OpTarget::Path { path: path.clone() };
    }
    outcome
}

fn normalized_done(d: &Done) -> OpOutcome {
    let mut outcome = d.outcome.clone();
    if let Some(path) = &d.path {
        outcome.target = OpTarget::Path { path: path.clone() };
    }
    outcome
}

fn target_text(target: &OpTarget) -> String {
    match target {
        OpTarget::Path { path } => path.to_string(),
        OpTarget::External { path } => path.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The user's choices, completed with defaults; unknown keys or values are refused.
fn resolve_choices(plan: &OpPlan, given: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, OpError> {
    for (key, value) in given {
        let Some(choice) = plan.choices.iter().find(|c| &c.key == key) else {
            return Err(err("ops.invalidRequest", format!("This operation has no choice {key:?}.")));
        };
        if !choice.options.iter().any(|o| &o.value == value) {
            return Err(err("ops.invalidRequest", format!("{value:?} is not an option for {key:?}.")));
        }
    }
    Ok(plan
        .choices
        .iter()
        .map(|c| (c.key.clone(), given.get(&c.key).cloned().unwrap_or_else(|| c.default.clone())))
        .collect())
}

// ------------------------------------------------------------- commands ----

async fn blocking<T, F>(app: AppHandle, f: F) -> Result<T, OpError>
where
    T: Send + 'static,
    F: FnOnce(&ProjectOps, &AppState) -> Result<T, OpError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let ops = app.state::<ProjectOps>();
        f(&ops, &state)
    })
    .await
    .map_err(|e| err("ops.failed", format!("The operation stopped unexpectedly: {e}")))?
}

#[tauri::command]
pub async fn project_op_plan(app: AppHandle, request: OpRequest) -> Result<OpPlan, OpError> {
    require_user(&request)?;
    blocking(app, move |ops, state| ops.plan(&state.workspace, &AppData(state), request)).await
}

#[tauri::command]
pub async fn project_op_apply(app: AppHandle, apply: OpApply) -> Result<OpResult, OpError> {
    let emitter = app.clone();
    let result = blocking(app, move |ops, state| {
        // Only the interface's own (user) plans are applied here.
        let user = lock(&ops.plans).get(&apply.plan).map(|p| matches!(p.plan.request.actor, OpActor::User));
        if user == Some(false) {
            return Err(err("ops.invalidRequest", "Agent operations come only from agent tasks."));
        }
        ops.apply(&state.workspace, &AppData(state), apply)
    })
    .await?;
    let _ = emitter.emit(PROJECT_OP_EVENT, &result);
    Ok(result)
}

#[tauri::command]
pub async fn project_op_restore(
    app: AppHandle,
    restore: OpRestore,
) -> Result<exposition_core::ops::OpOutcome, OpError> {
    let emitter = app.clone();
    let result = blocking(app, move |ops, state| ops.restore(&state.workspace, &AppData(state), restore)).await?;
    let _ = emitter.emit(PROJECT_OP_EVENT, &result);
    result.outcomes.into_iter().next().ok_or_else(|| err("ops.failed", "Nothing was restored."))
}

#[tauri::command]
pub async fn clipboard_files_read() -> Result<Vec<String>, OpError> {
    tauri::async_runtime::spawn_blocking(crate::clipboard::read_files)
        .await
        .map_err(|e| err("ops.failed", e.to_string()))?
        .map_err(|e| err("ops.clipboardUnavailable", e))
}

#[tauri::command]
pub async fn clipboard_files_write(app: AppHandle, paths: Vec<WorkspacePath>) -> Result<(), OpError> {
    blocking(app, move |_, state| {
        let absolute = ProjectOps::clipboard_paths(&state.workspace, &paths)?;
        crate::clipboard::write_files(&absolute).map_err(|e| err("ops.clipboardUnavailable", e))
    })
    .await
}

#[tauri::command]
pub async fn storage_overview(app: AppHandle) -> Result<StorageOverview, OpError> {
    blocking(app, |_, state| Ok(project_data::storage_overview(state))).await
}

/// Manage the orchestrator and reconcile interrupted operations of each
/// project once its graph is attached (the data side needs it).
pub fn start(app: &AppHandle) {
    let data_dir = app.state::<AppState>().data_dir.clone();
    app.manage(ProjectOps::new(&data_dir, Arc::new(OsTrash)));
    // Agent tools use the same orchestrator (their task as the actor).
    let _ = app.state::<AppState>().agent_project_ops.set(Arc::new(AgentOps { app: app.clone() }));
    let mut rx = app.state::<AppState>().graph.subscribe();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(exposition_graph::model::GraphEvent::Status { status }) if status.root.is_some() => {
                    let app = app.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || {
                        let state = app.state::<AppState>();
                        let ops = app.state::<ProjectOps>();
                        for result in ops.reconcile_interrupted(&state.workspace, &AppData(&state)) {
                            let _ = app.emit(PROJECT_OP_EVENT, &result);
                        }
                    })
                    .await;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// The orchestrator as agent tools use it (`delete_paths`, `copy_paths`, and
/// reverting them from the review). The gate has checked scope and path
/// policy; plans keep the request's actor (the agent task, or the user for a
/// review revert); the data side refuses data targets for agents. Plans are
/// applied with every choice's default, never a destructive one.
struct AgentOps {
    app: AppHandle,
}

impl exposition_agents::tools::services::ProjectOperations for AgentOps {
    fn plan(&self, request: OpRequest) -> Result<OpPlan, OpError> {
        let state = self.app.state::<AppState>();
        let ops = self.app.state::<ProjectOps>();
        ops.plan(&state.workspace, &AppData(&state), request)
    }

    fn apply(&self, plan: &OpPlan) -> Result<OpResult, OpError> {
        let state = self.app.state::<AppState>();
        let ops = self.app.state::<ProjectOps>();
        let apply = OpApply { plan: plan.plan.clone(), plan_hash: plan.plan_hash.clone(), choices: BTreeMap::new() };
        let result = ops.apply(&state.workspace, &AppData(&state), apply)?;
        let _ = self.app.emit(PROJECT_OP_EVENT, &result);
        Ok(result)
    }

    fn restore(&self, token: &str) -> Result<OpResult, OpError> {
        let state = self.app.state::<AppState>();
        let ops = self.app.state::<ProjectOps>();
        let result = ops.restore(
            &state.workspace,
            &AppData(&state),
            OpRestore { restore: token.to_owned(), on_collision: None },
        )?;
        let _ = self.app.emit(PROJECT_OP_EVENT, &result);
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
