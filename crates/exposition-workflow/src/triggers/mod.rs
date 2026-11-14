//! Explicit, user-only, debounced workflow triggers (M9).
//!
//! A trigger starts runs of one workflow without asking, but only within a
//! *standing plan* the user reviewed and enabled ([`TriggerService::plan`],
//! [`TriggerService::enable`]): the exact revision, runners, restricted image
//! identity, trust, and budgets, with only the declared watched inputs allowed
//! to change ([`standing`]). Nothing else can enable one: there is no agent
//! tool for triggers, imported or generated definitions cannot carry them
//! (the definition schema has no such field), and the records live in the
//! application's data directory ([`store`]), never in the project.
//!
//! The engine:
//! - listens only while the trigger's project is open AND finished indexing
//!   ([`TriggerService::project_opening`] → [`TriggerService::project_indexed`]
//!   → the next [`TriggerService::tick`]). At that moment it re-reads the
//!   watched inputs' content as its baseline: opening or indexing never
//!   starts a run, and changes made while the project was closed never do;
//! - maps saved-file changes ([`TriggerService::files_changed`]) to watched
//!   inputs and waits for a quiet period (debounce) per trigger;
//! - at fire time recomputes the plan and its standing hash: equal → starts
//!   the run through the run service's normal approval check with the cause
//!   (trigger, changed inputs old → new SHA-256); different → the trigger is
//!   suspended with what changed. A change to any other input of the plan
//!   (code above all) is checked the same way, so it suspends the trigger
//!   promptly instead of waiting for the next watched change;
//! - coalesces: while a run of the workflow is active, at most one follow-up
//!   waits (`queued`), and it uses whatever is saved when it starts;
//! - applies rate limits (runs per hour/day, model requests per day),
//!   deferring the next check until a run is allowed again;
//! - interval triggers fire on schedule only while the project is open;
//! - trust revocation suspends the folder's triggers; closing the project
//!   stops every timer and drops pending work.
//!
//! The clock is injectable ([`TriggerClock`]) and [`TriggerService::tick`]
//! does all the work synchronously, so the tests are deterministic; the
//! application runs [`TriggerService::spawn_driver`].

pub mod model;
pub mod standing;
pub mod starter;
pub mod store;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use exposition_core::path::WorkspacePath;
use exposition_graph::{WorkflowId, WorkflowPlanId, WorkflowRunId};
use tokio::sync::broadcast;

pub use model::*;
pub use starter::{RunServiceStarter, TriggerRunStarter};
use store::{FireRecord, StoredTrigger};

use crate::model::{ChangedInput, WorkflowRunCause, WorkflowRunError, WorkflowRunPlan};
use crate::plan::{self, project_key};

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 24 * HOUR_MS;
/// A queued follow-up first checks this soon whether it can start ...
const QUEUE_POLL_MS: u64 = 1000;
/// ... then backs off (doubling) to at most this while other runs block it.
/// A run ending makes it check at once ([`TriggerService::runs_changed`]).
const MAX_QUEUE_POLL_MS: u64 = 60_000;
/// Consecutive start refusals before the trigger is suspended.
const MAX_START_REFUSALS: u32 = 3;
/// Unconsumed standing-plan reviews kept.
const MAX_REVIEWS: usize = 16;
/// Plan errors that are temporary (the trigger stays enabled).
const TRANSIENT: &[&str] = &["workflow.noProject", "workflow.graphUnavailable"];

/// The time source (injectable for tests).
pub trait TriggerClock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// The system clock.
pub struct SystemClock;

impl TriggerClock for SystemClock {
    fn now_ms(&self) -> u64 {
        crate::now_ms()
    }
}

pub struct TriggerConfig {
    /// `<data dir>/workflow-triggers.json`; `None` keeps triggers in memory.
    pub store: Option<PathBuf>,
    pub starter: Arc<dyn TriggerRunStarter>,
    pub clock: Arc<dyn TriggerClock>,
}

/// The open project as the engine sees it.
struct Session {
    key: String,
    root: PathBuf,
    epoch: u64,
    /// Indexing finished (reported by the application).
    indexed: bool,
    /// When the engine started listening (after re-reading its baselines).
    ready_at: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct Runtime {
    /// A check that may start a run is due.
    fire_due: Option<u64>,
    /// A check that may only suspend is due (another input of the plan changed).
    check_due: Option<u64>,
    /// A follow-up waits for the active run of the workflow.
    queued: bool,
    /// When the queued follow-up next checks whether it can start, and the
    /// current back-off.
    poll_at: Option<u64>,
    backoff_ms: u64,
    /// The current rate-limit deferral is already in the history.
    rate_noted: bool,
}

#[derive(Clone)]
struct Entry {
    stored: StoredTrigger,
    rt: Runtime,
    /// Bumped on enable/disable/suspend: results of a check made against an
    /// older version are not applied (a started run is still recorded).
    version: u64,
}

#[derive(Clone)]
struct CachedReview {
    review: WorkflowTriggerPlan,
    key: String,
}

#[derive(Default)]
struct Engine {
    entries: Vec<Entry>,
    session: Option<Session>,
    reviews: HashMap<WorkflowPlanId, CachedReview>,
    epoch: u64,
    /// The store was read (or there is none). Until it is, nothing is written.
    loaded: bool,
    /// Why the store cannot be read or was not saved (shown on every trigger).
    store_problem: Option<String>,
    /// Stored records this version cannot read (written back unchanged).
    unreadable: Vec<serde_json::Value>,
}

struct Shared {
    starter: Arc<dyn TriggerRunStarter>,
    clock: Arc<dyn TriggerClock>,
    store: Option<PathBuf>,
    engine: Mutex<Engine>,
    /// Serializes ticks (checks run outside the engine lock).
    ticking: Mutex<()>,
    events: broadcast::Sender<WorkflowTriggerEvent>,
    signal: Mutex<bool>,
    wake: Condvar,
    stopped: AtomicBool,
}

/// The application's triggers (one instance).
#[derive(Clone)]
pub struct TriggerService {
    shared: Arc<Shared>,
}

/// One check, made outside the engine lock.
struct Job {
    id: WorkflowTriggerId,
    version: u64,
    epoch: u64,
    key: String,
    workflow: WorkflowId,
    kind: TriggerKind,
    standing_hash: String,
    standing_plan: WorkflowRunPlan,
    baseline: BTreeMap<String, Option<String>>,
    fires: Vec<FireRecord>,
    settings: WorkflowTriggerSettings,
    /// May start a run (otherwise it only checks for suspension).
    fire: bool,
    /// A queued follow-up polling for the active run to end.
    poll: bool,
}

enum Outcome {
    /// Nothing to do (no change or a temporary problem). A queued follow-up
    /// with nothing to run is dropped.
    Nothing,
    /// A queued follow-up is still blocked by other runs (back off).
    StillBusy,
    Suspend {
        code: String,
        message: String,
    },
    Queue,
    RateLimited {
        until: u64,
    },
    Fired {
        run: WorkflowRunId,
        changed: Vec<ChangedInput>,
        current: BTreeMap<String, Option<String>>,
        model_requests: u32,
    },
    StartRefused {
        code: String,
        message: String,
    },
}

fn no_project() -> WorkflowRunError {
    WorkflowRunError::new("workflow.noProject", "Open a project first.")
}

fn stale(message: impl Into<String>) -> WorkflowRunError {
    WorkflowRunError::new("trigger.planStale", message)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn settings_of(t: &WorkflowTrigger) -> WorkflowTriggerSettings {
    WorkflowTriggerSettings {
        debounce_seconds: t.debounce_seconds,
        max_runs_per_hour: t.max_runs_per_hour,
        max_runs_per_day: t.max_runs_per_day,
        allow_native: t.allow_native,
        max_model_requests_per_day: t.max_model_requests_per_day,
    }
}

fn debounce_ms(t: &WorkflowTrigger) -> u64 {
    u64::from(t.debounce_seconds.clamp(MIN_DEBOUNCE_SECONDS, MAX_DEBOUNCE_SECONDS)) * 1000
}

fn interval_ms(kind: &TriggerKind) -> Option<u64> {
    match kind {
        TriggerKind::Interval { minutes, .. } => {
            Some(u64::from((*minutes).clamp(MIN_INTERVAL_MINUTES, MAX_INTERVAL_MINUTES)) * 60_000)
        }
        TriggerKind::InputsChanged { .. } => None,
    }
}

fn within(now: u64, at: u64, window: u64) -> bool {
    now.saturating_sub(at) < window
}

/// When a run is allowed again (`None`: now).
fn rate_limited(fires: &[FireRecord], now: u64, s: &WorkflowTriggerSettings, model_requests: u32) -> Option<u64> {
    let mut sorted: Vec<&FireRecord> = fires.iter().collect();
    sorted.sort_by_key(|f| f.at_ms);
    let hour: Vec<&FireRecord> = sorted.iter().copied().filter(|f| within(now, f.at_ms, HOUR_MS)).collect();
    let day: Vec<&FireRecord> = sorted.iter().copied().filter(|f| within(now, f.at_ms, DAY_MS)).collect();
    let mut until: Option<u64> = None;
    let mut later = |t: u64| until = Some(until.map_or(t, |u| u.max(t)));
    if hour.len() >= s.max_runs_per_hour as usize {
        later(hour[hour.len() - s.max_runs_per_hour as usize].at_ms + HOUR_MS);
    }
    if day.len() >= s.max_runs_per_day as usize {
        later(day[day.len() - s.max_runs_per_day as usize].at_ms + DAY_MS);
    }
    if model_requests > 0 {
        let mut used: u32 = day.iter().map(|f| f.model_requests).sum();
        if used.saturating_add(model_requests) > s.max_model_requests_per_day {
            let mut freed = None;
            for f in &day {
                used = used.saturating_sub(f.model_requests);
                if used.saturating_add(model_requests) <= s.max_model_requests_per_day {
                    freed = Some(f.at_ms + DAY_MS);
                    break;
                }
            }
            later(freed.unwrap_or(now + DAY_MS));
        }
    }
    until.filter(|t| *t > now)
}

fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// `changed` is `path` or a folder above it.
fn touches(changed: &WorkspacePath, path: &WorkspacePath) -> bool {
    let (c, p) = (changed.as_str(), path.as_str());
    same_path(c, p) || (p.len() > c.len() + 1 && p.as_bytes()[c.len()] == b'/' && same_path(&p[..c.len()], c))
}

impl Engine {
    fn listening(&self, entry: &Entry) -> bool {
        entry.stored.trigger.state == TriggerState::Active
            && self.session.as_ref().is_some_and(|s| s.ready_at.is_some() && s.key == entry.stored.project_key)
    }

    fn view(&self, entry: &Entry, now: u64) -> WorkflowTrigger {
        let mut view = entry.stored.trigger.clone();
        let listening = self.listening(entry);
        view.listening = listening;
        view.queued = listening && entry.rt.queued;
        view.next_check_ms =
            if listening { [entry.rt.fire_due, entry.rt.check_due].into_iter().flatten().min() } else { None };
        let fires = &entry.stored.fires;
        view.runs_last_hour = fires.iter().filter(|f| within(now, f.at_ms, HOUR_MS)).count() as u32;
        view.runs_last_day = fires.iter().filter(|f| within(now, f.at_ms, DAY_MS)).count() as u32;
        view.model_requests_last_day =
            fires.iter().filter(|f| within(now, f.at_ms, DAY_MS)).map(|f| f.model_requests).sum();
        view.store_problem = self.store_problem.clone();
        view
    }

    fn event(&self, entry: &Entry, run: Option<WorkflowRunId>) -> WorkflowTriggerEvent {
        let listening = self.listening(entry);
        WorkflowTriggerEvent {
            trigger: entry.stored.trigger.id.clone(),
            workflow: entry.stored.trigger.workflow.clone(),
            state: entry.stored.trigger.state.clone(),
            queued: listening && entry.rt.queued,
            listening,
            last_fired_ms: entry.stored.trigger.last_fired_ms,
            run,
        }
    }

    fn find(&mut self, id: &WorkflowTriggerId) -> Option<usize> {
        self.entries.iter().position(|e| &e.stored.trigger.id == id)
    }
}

fn record(entry: &mut Entry, at_ms: u64, event: TriggerHistoryEvent) {
    let history = &mut entry.stored.trigger.history;
    history.push(TriggerHistoryEntry { at_ms, event });
    if history.len() > MAX_TRIGGER_HISTORY {
        let excess = history.len() - MAX_TRIGGER_HISTORY;
        history.drain(..excess);
    }
}

fn suspend(entry: &mut Entry, now: u64, code: &str, message: String) {
    entry.stored.trigger.state = TriggerState::Suspended { code: code.to_owned(), message: message.clone() };
    entry.rt = Runtime::default();
    entry.version += 1;
    record(entry, now, TriggerHistoryEvent::Suspended { code: code.to_owned(), message });
}

fn loaded_entry(mut stored: StoredTrigger) -> Entry {
    stored.trigger.queued = false;
    stored.trigger.listening = false;
    stored.trigger.next_check_ms = None;
    stored.trigger.store_problem = None;
    Entry { stored, rt: Runtime::default(), version: 0 }
}

/// The watched inputs' digests in `plan` and those that differ from `baseline`.
fn changes(
    watched: &[String],
    baseline: &BTreeMap<String, Option<String>>,
    plan: &WorkflowRunPlan,
) -> (BTreeMap<String, Option<String>>, Vec<ChangedInput>) {
    let current: BTreeMap<String, Option<String>> = watched
        .iter()
        .map(|id| (id.clone(), plan.inputs.iter().find(|i| &i.id == id).map(|i| i.sha256.clone())))
        .collect();
    let changed = current
        .iter()
        .filter(|(id, after)| baseline.get(*id).cloned().flatten() != **after)
        .map(|(id, after)| ChangedInput {
            input: id.clone(),
            before: baseline.get(id).cloned().flatten(),
            after: after.clone(),
        })
        .collect();
    (current, changed)
}

impl TriggerService {
    /// Load the stored triggers (nothing is listening until a project opens
    /// and finishes indexing). A store that cannot be read is not written
    /// until it can be ([`Self::ensure_store`]).
    pub fn new(config: TriggerConfig) -> Self {
        let mut engine = Engine::default();
        match &config.store {
            None => engine.loaded = true,
            Some(path) => match store::load(path) {
                Ok(loaded) => {
                    engine.entries = loaded.triggers.into_iter().map(loaded_entry).collect();
                    engine.unreadable = loaded.unreadable;
                    engine.loaded = true;
                }
                Err(problem) => {
                    tracing::warn!("{problem}");
                    engine.store_problem = Some(problem);
                }
            },
        }
        let (events, _) = broadcast::channel(256);
        Self {
            shared: Arc::new(Shared {
                starter: config.starter,
                clock: config.clock,
                store: config.store,
                engine: Mutex::new(engine),
                ticking: Mutex::new(()),
                events,
                signal: Mutex::new(false),
                wake: Condvar::new(),
                stopped: AtomicBool::new(false),
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkflowTriggerEvent> {
        self.shared.events.subscribe()
    }

    fn now(&self) -> u64 {
        self.shared.clock.now_ms()
    }

    fn emit(&self, event: WorkflowTriggerEvent) {
        let _ = self.shared.events.send(event);
    }

    /// Write the records. A store that could not be read is never written
    /// (`trigger.storeUnavailable`); a failed write is remembered and shown on
    /// every trigger until a later save succeeds.
    fn persist(&self, engine: &mut Engine) -> Result<(), WorkflowRunError> {
        let Some(path) = &self.shared.store else { return Ok(()) };
        if !engine.loaded {
            let problem = engine.store_problem.clone().unwrap_or_else(|| "The trigger store cannot be read.".into());
            return Err(WorkflowRunError::new("trigger.storeUnavailable", problem));
        }
        let records: Vec<&StoredTrigger> = engine.entries.iter().map(|e| &e.stored).collect();
        match store::save(path, &records, &engine.unreadable) {
            Ok(()) => {
                engine.store_problem = None;
                Ok(())
            }
            Err(e) => {
                tracing::warn!("workflow triggers could not be saved: {e}");
                let message = format!(
                    "The triggers could not be saved ({e}); changes since the last save may not survive a restart."
                );
                engine.store_problem = Some(message.clone());
                Err(WorkflowRunError::new("trigger.storeFailed", message))
            }
        }
    }

    /// Save a change the engine made on its own (a suspension, a run). It
    /// applies whether or not it could be saved; a failure is shown.
    fn persist_background(&self, engine: &mut Engine) {
        if let Err(e) = self.persist(engine) {
            tracing::warn!("workflow trigger change not saved: {}", e.message);
        }
    }

    /// Read the store now if it could not be read before. Until it can be,
    /// the user's changes are refused (`trigger.storeUnavailable`) and nothing
    /// is written over it.
    fn ensure_store(&self, engine: &mut Engine) -> Result<(), WorkflowRunError> {
        if engine.loaded {
            return Ok(());
        }
        let Some(path) = &self.shared.store else {
            engine.loaded = true;
            return Ok(());
        };
        match store::load(path) {
            Ok(loaded) => {
                // Nothing was added meanwhile: every change waits for the store.
                engine.entries = loaded.triggers.into_iter().map(loaded_entry).collect();
                engine.unreadable = loaded.unreadable;
                engine.loaded = true;
                engine.store_problem = None;
                // An open project re-reads the baselines of what was loaded.
                if let Some(session) = engine.session.as_mut()
                    && session.indexed
                {
                    session.ready_at = None;
                }
                Ok(())
            }
            Err(problem) => {
                engine.store_problem = Some(problem.clone());
                Err(WorkflowRunError::new("trigger.storeUnavailable", problem))
            }
        }
    }

    /// A run ended: queued follow-ups check at once whether they can start.
    pub fn runs_changed(&self) {
        let now = self.now();
        let mut engine = lock(&self.shared.engine);
        let mut any = false;
        for entry in engine.entries.iter_mut().filter(|e| e.rt.queued) {
            entry.rt.poll_at = Some(now);
            any = true;
        }
        drop(engine);
        if any {
            self.wake();
        }
    }

    /// Wake the driver (a change arrived or a run ended).
    pub fn wake(&self) {
        *lock(&self.shared.signal) = true;
        self.shared.wake.notify_all();
    }

    // ----- review and enable ------------------------------------------------

    /// Review a standing plan for a trigger of the latest revision of the
    /// workflow in the open project. Refused for a kind that cannot be a
    /// trigger (code or pinned watched inputs, unknown inputs, an interval
    /// shorter than 15 minutes).
    pub fn plan(&self, request: &WorkflowTriggerPlanRequest) -> Result<WorkflowTriggerPlan, WorkflowRunError> {
        let root = self.shared.starter.project_root().ok_or_else(no_project)?;
        let plan = self.shared.starter.preview(&request.workflow)?;
        let watched = standing::watched_inputs(&plan, &request.kind)?;
        let ids: Vec<String> = watched.iter().map(|w| w.input.clone()).collect();
        let review = WorkflowTriggerPlan {
            standing_hash: standing::standing_hash(&plan, &root, &ids),
            may_change: standing::may_change(&watched),
            suspends_on: standing::suspends_on(&plan, &ids),
            native_steps: standing::native_labels(&plan),
            model_requests_per_run: plan.budgets.max_model_requests,
            defaults: standing::default_settings(&plan),
            limits: WorkflowTriggerLimits::current(),
            notices: standing::notices(&plan, &request.kind, &watched),
            blockers: plan.blockers.iter().filter(|b| !starter::is_busy(&b.code)).cloned().collect(),
            expires_at_ms: plan.expires_at_ms,
            kind: request.kind.clone(),
            watched,
            plan,
        };
        // Plan times come from the run service's clock (the system clock).
        let now = crate::now_ms();
        let mut engine = lock(&self.shared.engine);
        engine.reviews.retain(|_, r| r.review.expires_at_ms > now);
        if engine.reviews.len() >= MAX_REVIEWS
            && let Some(oldest) =
                engine.reviews.iter().min_by_key(|(_, r)| r.review.plan.created_at_ms).map(|(k, _)| k.clone())
        {
            engine.reviews.remove(&oldest);
        }
        engine
            .reviews
            .insert(review.plan.plan.clone(), CachedReview { review: review.clone(), key: project_key(&root) });
        Ok(review)
    }

    /// Enable a trigger from a reviewed standing plan. The plan is recomputed
    /// now; refused (`trigger.planStale`) if its standing hash differs from
    /// the reviewed one. Re-enabling a workflow's trigger keeps its id,
    /// history, and rate-limit window.
    pub fn enable(&self, request: &EnableWorkflowTriggerRequest) -> Result<WorkflowTrigger, WorkflowRunError> {
        let now = self.now();
        // Refused settings and an unreadable store leave the review usable
        // for a corrected attempt; a wrong or expired approval uses it up.
        let cached = {
            let mut engine = lock(&self.shared.engine);
            let check = engine.reviews.get(&request.plan).map(|c| {
                if c.review.expires_at_ms <= crate::now_ms() {
                    Err(stale("This standing plan expired; review it again."))
                } else if request.standing_hash != c.review.standing_hash {
                    Err(stale("The approval does not match the standing plan that was reviewed; review it again."))
                } else {
                    Ok(standing::check_settings(&request.settings, &c.review.plan, &c.review.kind))
                }
            });
            match check {
                None => return Err(stale("This standing plan is unknown or was already used; review it again.")),
                Some(Err(e)) => {
                    engine.reviews.remove(&request.plan);
                    return Err(e);
                }
                Some(Ok(Err(e))) => return Err(e),
                Some(Ok(Ok(()))) => {}
            }
            self.ensure_store(&mut engine)?;
            match engine.reviews.remove(&request.plan) {
                Some(cached) => cached,
                None => return Err(stale("This standing plan was already used; review it again.")),
            }
        };
        let kept = cached.clone();
        let review = cached.review;
        let root = self.shared.starter.project_root().ok_or_else(no_project)?;
        let key = project_key(&root);
        if key != cached.key {
            return Err(stale("The open project changed since the standing plan was made; review it again."));
        }
        let ids: Vec<String> = review.watched.iter().map(|w| w.input.clone()).collect();
        let fresh = self.shared.starter.preview(&review.plan.workflow)?;
        if standing::standing_hash(&fresh, &root, &ids) != review.standing_hash {
            return Err(stale(standing::describe(&review.plan, &fresh, &ids).replace(
                "The trigger is suspended until you review and enable it again.",
                "Review the standing plan again.",
            )));
        }
        if let Some(blocker) = fresh.blockers.iter().find(|b| !starter::is_busy(&b.code)) {
            return Err(WorkflowRunError::new(&blocker.code, blocker.message.clone()));
        }
        let baseline: BTreeMap<String, Option<String>> = ids
            .iter()
            .map(|id| (id.clone(), fresh.inputs.iter().find(|i| &i.id == id).map(|i| i.sha256.clone())))
            .collect();
        let mut standing_plan = fresh.clone();
        for input in &mut standing_plan.inputs {
            input.text = None;
        }
        let settings = &request.settings;

        let mut engine = lock(&self.shared.engine);
        self.ensure_store(&mut engine)?;
        let before = engine.entries.clone();
        let existing = engine
            .entries
            .iter()
            .position(|e| e.stored.project_key == key && e.stored.trigger.workflow == fresh.workflow);
        if existing.is_none()
            && engine.entries.iter().filter(|e| e.stored.project_key == key).count() >= MAX_TRIGGERS_PER_PROJECT
        {
            // Make room by forgetting the oldest disabled trigger of the project.
            let oldest = engine
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.stored.project_key == key && e.stored.trigger.state == TriggerState::Disabled)
                .min_by_key(|(_, e)| e.stored.trigger.enabled_at_ms)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    engine.entries.remove(i);
                }
                None => {
                    return Err(WorkflowRunError::new(
                        "trigger.tooMany",
                        format!("A project has at most {MAX_TRIGGERS_PER_PROJECT} triggers; disable one first."),
                    ));
                }
            }
        }
        let (id, created_at_ms, history, fires, version) = match existing {
            Some(i) => {
                let old = &engine.entries[i];
                (
                    old.stored.trigger.id.clone(),
                    old.stored.trigger.created_at_ms,
                    old.stored.trigger.history.clone(),
                    old.stored.fires.clone(),
                    old.version + 1,
                )
            }
            None => (WorkflowTriggerId::mint(), now, Vec::new(), Vec::new(), 0),
        };
        let trigger = WorkflowTrigger {
            id,
            workflow: fresh.workflow.clone(),
            revision: fresh.revision,
            title: fresh.title.clone(),
            project: fresh.project.clone(),
            kind: review.kind.clone(),
            watched: review.watched.clone(),
            debounce_seconds: settings.debounce_seconds,
            max_runs_per_hour: settings.max_runs_per_hour,
            max_runs_per_day: settings.max_runs_per_day,
            allow_native: settings.allow_native,
            max_model_requests_per_day: settings.max_model_requests_per_day,
            model_requests_per_run: fresh.budgets.max_model_requests,
            standing_hash: review.standing_hash.clone(),
            state: TriggerState::Active,
            created_at_ms,
            enabled_at_ms: now,
            last_fired_ms: None,
            queued: false,
            next_check_ms: None,
            listening: false,
            runs_last_hour: 0,
            runs_last_day: 0,
            model_requests_last_day: 0,
            history,
            store_problem: None,
        };
        let mut entry = Entry {
            stored: StoredTrigger {
                trigger,
                project_key: key.clone(),
                standing_plan,
                baseline,
                fires,
                start_refusals: 0,
            },
            rt: Runtime::default(),
            version,
        };
        if let Some(i) = existing {
            entry.stored.trigger.last_fired_ms = engine.entries[i].stored.trigger.last_fired_ms;
        }
        record(&mut entry, now, TriggerHistoryEvent::Enabled { revision: fresh.revision });
        let ready = engine.session.as_ref().is_some_and(|s| s.key == key && s.ready_at.is_some());
        if ready && let Some(every) = interval_ms(&entry.stored.trigger.kind) {
            entry.rt.fire_due = Some(now + every);
        }
        let index = match existing {
            Some(i) => {
                engine.entries[i] = entry;
                i
            }
            None => {
                engine.entries.push(entry);
                engine.entries.len() - 1
            }
        };
        if let Err(e) = self.persist(&mut engine) {
            // Never leave an enabled trigger that was not saved: undo it, and
            // keep the review for another attempt.
            engine.entries = before;
            engine.reviews.insert(request.plan.clone(), kept);
            return Err(e);
        }
        let view = engine.view(&engine.entries[index], now);
        let event = engine.event(&engine.entries[index], None);
        drop(engine);
        self.emit(event);
        self.wake();
        Ok(view)
    }

    /// Turn a trigger off (pending checks and the queued follow-up are dropped).
    pub fn disable(&self, id: &WorkflowTriggerId) -> Result<WorkflowTrigger, WorkflowRunError> {
        let now = self.now();
        let mut engine = lock(&self.shared.engine);
        self.ensure_store(&mut engine)?;
        let Some(i) = engine.find(id) else {
            return Err(WorkflowRunError::new("trigger.unknown", "That trigger does not exist."));
        };
        if engine.entries[i].stored.trigger.state != TriggerState::Disabled {
            let before = engine.entries[i].clone();
            let entry = &mut engine.entries[i];
            entry.stored.trigger.state = TriggerState::Disabled;
            entry.rt = Runtime::default();
            entry.version += 1;
            record(entry, now, TriggerHistoryEvent::Disabled);
            if let Err(e) = self.persist(&mut engine) {
                // Not saved: not applied.
                engine.entries[i] = before;
                return Err(e);
            }
        }
        let view = engine.view(&engine.entries[i], now);
        let event = engine.event(&engine.entries[i], None);
        drop(engine);
        self.emit(event);
        Ok(view)
    }

    /// Triggers of the open project (of one workflow when given).
    pub fn list(&self, workflow: Option<&WorkflowId>) -> Result<Vec<WorkflowTrigger>, WorkflowRunError> {
        let root = self.shared.starter.project_root().ok_or_else(no_project)?;
        let key = project_key(&root);
        let now = self.now();
        let mut engine = lock(&self.shared.engine);
        // A store that cannot be read is shown as such, not as no triggers.
        self.ensure_store(&mut engine)?;
        Ok(engine
            .entries
            .iter()
            .filter(|e| e.stored.project_key == key && workflow.is_none_or(|w| &e.stored.trigger.workflow == w))
            .map(|e| engine.view(e, now))
            .collect())
    }

    // ----- project lifecycle and inputs --------------------------------------

    /// A project was opened (not yet indexed): nothing listens yet.
    pub fn project_opening(&self, root: &Path) {
        let key = project_key(root);
        let mut engine = lock(&self.shared.engine);
        if engine.session.as_ref().is_some_and(|s| s.key == key) {
            return;
        }
        engine.epoch += 1;
        let epoch = engine.epoch;
        engine.session = Some(Session { key, root: root.to_path_buf(), epoch, indexed: false, ready_at: None });
        for entry in &mut engine.entries {
            entry.rt = Runtime::default();
        }
    }

    /// The project at `root` finished indexing; the next tick starts listening.
    pub fn project_indexed(&self, root: &Path) {
        let key = project_key(root);
        let mut engine = lock(&self.shared.engine);
        match engine.session.as_mut() {
            Some(session) if session.key == key && !session.indexed => session.indexed = true,
            _ => return,
        }
        drop(engine);
        self.wake();
    }

    /// The project closed or was switched: stop listening and drop every
    /// pending check and queued follow-up (nothing catches up later).
    pub fn project_closed(&self) {
        let mut engine = lock(&self.shared.engine);
        let Some(session) = engine.session.take() else { return };
        engine.epoch += 1;
        let mut events = Vec::new();
        for entry in &mut engine.entries {
            entry.rt = Runtime::default();
        }
        for entry in engine.entries.iter().filter(|e| e.stored.project_key == session.key) {
            events.push(engine.event(entry, None));
        }
        drop(engine);
        for event in events {
            self.emit(event);
        }
    }

    /// Saved files changed in the open project (`rescan`: the watcher lost
    /// track; every input may have changed). Ignored until the project
    /// finished indexing.
    pub fn files_changed(&self, changes: &[WorkspacePath], rescan: bool) {
        let now = self.now();
        let mut engine = lock(&self.shared.engine);
        let Some(key) = engine.session.as_ref().filter(|s| s.ready_at.is_some()).map(|s| s.key.clone()) else {
            return;
        };
        let mut scheduled = false;
        for entry in engine.entries.iter_mut() {
            if entry.stored.project_key != key || entry.stored.trigger.state != TriggerState::Active {
                continue;
            }
            let trigger = &entry.stored.trigger;
            let hit = |path: &WorkspacePath| rescan || changes.iter().any(|c| touches(c, path));
            let watched_hit = trigger.watched.iter().any(|w| hit(&w.path));
            let other_hit = entry
                .stored
                .standing_plan
                .inputs
                .iter()
                .filter(|i| !trigger.watched.iter().any(|w| w.input == i.id))
                .any(|i| hit(&i.path));
            let due = now + debounce_ms(trigger);
            match trigger.kind {
                TriggerKind::InputsChanged { .. } if watched_hit => {
                    entry.rt.fire_due = Some(due);
                    scheduled = true;
                }
                _ if watched_hit || other_hit => {
                    entry.rt.check_due = Some(due);
                    scheduled = true;
                }
                _ => {}
            }
        }
        drop(engine);
        if scheduled {
            self.wake();
        }
    }

    /// Trust was revoked for `revoked` (and everything inside it): its
    /// triggers are suspended until the user enables them again.
    pub fn trust_revoked(&self, revoked: &Path) {
        let revoked = std::fs::canonicalize(revoked).unwrap_or_else(|_| revoked.to_path_buf());
        let prefix = PathBuf::from(project_key(&revoked));
        let now = self.now();
        let mut engine = lock(&self.shared.engine);
        let mut changed = Vec::new();
        for (i, entry) in engine.entries.iter_mut().enumerate() {
            if entry.stored.trigger.state == TriggerState::Active
                && PathBuf::from(&entry.stored.project_key).starts_with(&prefix)
            {
                suspend(
                    entry,
                    now,
                    "trigger.trustRevoked",
                    "Trust was revoked for the project folder, so the trigger is suspended until you review and enable it again."
                        .into(),
                );
                changed.push(i);
            }
        }
        if changed.is_empty() {
            return;
        }
        self.persist_background(&mut engine);
        let events: Vec<_> = changed.iter().map(|&i| engine.event(&engine.entries[i], None)).collect();
        drop(engine);
        for event in events {
            self.emit(event);
        }
    }

    // ----- the engine ---------------------------------------------------------

    /// Do everything that is due now. Returns when the next check is due
    /// (`None`: nothing pending).
    pub fn tick(&self) -> Option<u64> {
        let _ticking = lock(&self.shared.ticking);
        self.become_ready();
        let now = self.now();
        let jobs = self.due_jobs(now);
        for job in jobs {
            let outcome = self.check(&job);
            self.apply(job, outcome);
        }
        self.next_due()
    }

    /// After indexing: re-read the watched inputs as the baseline (changes
    /// made while the project was closed never start a run) and start
    /// listening; interval triggers schedule their first run one interval on.
    fn become_ready(&self) {
        let (epoch, key, root, watched) = {
            let engine = lock(&self.shared.engine);
            let Some(session) = engine.session.as_ref().filter(|s| s.indexed && s.ready_at.is_none()) else {
                return;
            };
            let watched: Vec<(WorkflowTriggerId, Vec<(String, WorkspacePath)>)> = engine
                .entries
                .iter()
                .filter(|e| e.stored.project_key == session.key)
                .map(|e| {
                    let inputs = e.stored.trigger.watched.iter().map(|w| (w.input.clone(), w.path.clone())).collect();
                    (e.stored.trigger.id.clone(), inputs)
                })
                .collect();
            (session.epoch, session.key.clone(), session.root.clone(), watched)
        };
        let baselines: Vec<(WorkflowTriggerId, BTreeMap<String, Option<String>>)> = watched
            .into_iter()
            .map(|(id, inputs)| {
                let digests = inputs
                    .into_iter()
                    .map(|(input, path)| (input, plan::hash_project_file(&root, &path).ok().map(|(sha, _)| sha)))
                    .collect();
                (id, digests)
            })
            .collect();
        let now = self.now();
        let mut engine = lock(&self.shared.engine);
        if engine.session.as_ref().is_none_or(|s| s.epoch != epoch) {
            return;
        }
        let rebased = !baselines.is_empty();
        for (id, baseline) in baselines {
            if let Some(i) = engine.find(&id) {
                engine.entries[i].stored.baseline = baseline;
            }
        }
        for entry in engine.entries.iter_mut().filter(|e| e.stored.project_key == key) {
            entry.rt = Runtime::default();
            if entry.stored.trigger.state == TriggerState::Active
                && let Some(every) = interval_ms(&entry.stored.trigger.kind)
            {
                let after_last = entry.stored.trigger.last_fired_ms.map_or(0, |t| t + every);
                entry.rt.fire_due = Some((now + every).max(after_last));
            }
        }
        if let Some(session) = engine.session.as_mut() {
            session.ready_at = Some(now);
        }
        if rebased {
            self.persist_background(&mut engine);
        }
        let events: Vec<_> = engine
            .entries
            .iter()
            .filter(|e| e.stored.project_key == key && e.stored.trigger.state == TriggerState::Active)
            .map(|e| engine.event(e, None))
            .collect();
        drop(engine);
        for event in events {
            self.emit(event);
        }
    }

    fn due_jobs(&self, now: u64) -> Vec<Job> {
        let mut engine = lock(&self.shared.engine);
        let Some((key, epoch)) =
            engine.session.as_ref().filter(|s| s.ready_at.is_some()).map(|s| (s.key.clone(), s.epoch))
        else {
            return Vec::new();
        };
        let mut jobs = Vec::new();
        for entry in engine.entries.iter_mut() {
            if entry.stored.project_key != key || entry.stored.trigger.state != TriggerState::Active {
                continue;
            }
            let (fire, poll) = if entry.rt.fire_due.is_some_and(|t| t <= now) {
                entry.rt.fire_due = None;
                entry.rt.check_due = None;
                (true, false)
            } else if entry.rt.check_due.is_some_and(|t| t <= now) {
                entry.rt.check_due = None;
                (false, false)
            } else if entry.rt.queued && entry.rt.fire_due.is_none() && entry.rt.poll_at.is_some_and(|t| t <= now) {
                entry.rt.poll_at = None;
                (true, true)
            } else {
                continue;
            };
            let t = &entry.stored.trigger;
            jobs.push(Job {
                id: t.id.clone(),
                version: entry.version,
                epoch,
                key: key.clone(),
                workflow: t.workflow.clone(),
                kind: t.kind.clone(),
                standing_hash: t.standing_hash.clone(),
                standing_plan: entry.stored.standing_plan.clone(),
                baseline: entry.stored.baseline.clone(),
                fires: entry.stored.fires.clone(),
                settings: settings_of(t),
                fire,
                poll,
            });
        }
        jobs
    }

    /// Why the plan no longer fits the standing plan, if it does not.
    fn standing_problem(&self, job: &Job, plan: &WorkflowRunPlan, root: &Path) -> Option<Outcome> {
        let watched = job.kind.watched();
        if standing::standing_hash(plan, root, watched) != job.standing_hash {
            return Some(Outcome::Suspend {
                code: "trigger.planChanged".into(),
                message: standing::describe(&job.standing_plan, plan, watched),
            });
        }
        if !plan.execution.native_steps.is_empty() && !job.settings.allow_native {
            return Some(Outcome::Suspend {
                code: "trigger.nativeNotAllowed".into(),
                message: "The plan has native steps, which this trigger was not allowed to start.".into(),
            });
        }
        plan.blockers.iter().find(|b| !starter::is_busy(&b.code)).map(|blocker| Outcome::Suspend {
            code: blocker.code.clone(),
            message: format!("{} The trigger is suspended until you review and enable it again.", blocker.message),
        })
    }

    /// Recompute the plan and decide (outside the engine lock). Checks use a
    /// preview the run service does not keep; a startable plan is made only
    /// when a run starts now, and it is checked against the standing plan
    /// once more.
    fn check(&self, job: &Job) -> Outcome {
        let starter = &self.shared.starter;
        if job.poll && starter.busy(&job.workflow) {
            return Outcome::StillBusy;
        }
        let Some(root) = starter.project_root().filter(|r| project_key(r) == job.key) else {
            return Outcome::Nothing;
        };
        let planned = |plan: Result<WorkflowRunPlan, WorkflowRunError>| match plan {
            Ok(plan) => Ok(plan),
            Err(e) if TRANSIENT.contains(&e.code.as_str()) => Err(Outcome::Nothing),
            Err(e) => Err(Outcome::Suspend {
                code: e.code,
                message: format!(
                    "The workflow could not be planned: {} The trigger is suspended until you review and enable it again.",
                    e.message
                ),
            }),
        };
        let plan = match planned(starter.preview(&job.workflow)) {
            Ok(plan) => plan,
            Err(outcome) => return outcome,
        };
        if let Some(problem) = self.standing_problem(job, &plan, &root) {
            return problem;
        }
        let watched = job.kind.watched();
        let (_, changed) = changes(watched, &job.baseline, &plan);
        if !job.fire {
            return Outcome::Nothing;
        }
        if matches!(job.kind, TriggerKind::InputsChanged { .. }) && changed.is_empty() {
            return Outcome::Nothing;
        }
        if plan.blockers.iter().any(|b| starter::is_busy(&b.code)) || starter.busy(&job.workflow) {
            return Outcome::Queue;
        }
        let model_requests = plan.budgets.max_model_requests;
        if let Some(until) = rate_limited(&job.fires, self.now(), &job.settings, model_requests) {
            return Outcome::RateLimited { until };
        }
        // Start now: a startable plan, checked against the standing plan again
        // (the run reads exactly what it records as changed).
        let plan = match planned(starter.plan_to_start(&job.workflow)) {
            Ok(plan) => plan,
            Err(outcome) => return outcome,
        };
        if let Some(problem) = self.standing_problem(job, &plan, &root) {
            return problem;
        }
        let (current, changed) = changes(watched, &job.baseline, &plan);
        if matches!(job.kind, TriggerKind::InputsChanged { .. }) && changed.is_empty() {
            return Outcome::Nothing;
        }
        let cause = WorkflowRunCause::Trigger { trigger: job.id.to_string(), changed: changed.clone() };
        match starter.start(&plan, cause) {
            Ok(run) => Outcome::Fired { run, changed, current, model_requests },
            Err(e) if starter::is_busy(&e.code) => Outcome::Queue,
            Err(e) if e.code == "workflow.planStale" => Outcome::StartRefused { code: e.code, message: e.message },
            Err(e) => Outcome::Suspend {
                code: e.code,
                message: format!(
                    "The run could not start: {} The trigger is suspended until you review and enable it again.",
                    e.message
                ),
            },
        }
    }

    fn apply(&self, job: Job, outcome: Outcome) {
        let now = self.now();
        let mut engine = lock(&self.shared.engine);
        let Some(i) = engine.find(&job.id) else { return };
        let current =
            engine.entries[i].version == job.version && engine.session.as_ref().is_some_and(|s| s.epoch == job.epoch);
        let fired = matches!(outcome, Outcome::Fired { .. });
        if !current && !fired {
            return;
        }
        let entry = &mut engine.entries[i];
        let mut run = None;
        let mut durable = false;
        let mut announce = false;
        match outcome {
            Outcome::Nothing => {
                // A follow-up with nothing (left) to run is dropped.
                if job.fire && entry.rt.queued {
                    entry.rt.queued = false;
                    entry.rt.poll_at = None;
                    entry.rt.backoff_ms = 0;
                    announce = true;
                }
            }
            Outcome::StillBusy => {
                entry.rt.backoff_ms = (entry.rt.backoff_ms * 2).clamp(QUEUE_POLL_MS, MAX_QUEUE_POLL_MS);
                entry.rt.poll_at = Some(now + entry.rt.backoff_ms);
            }
            Outcome::Suspend { code, message } => {
                suspend(entry, now, &code, message);
                durable = true;
                announce = true;
            }
            Outcome::Queue => {
                if entry.rt.queued {
                    entry.rt.backoff_ms = (entry.rt.backoff_ms * 2).clamp(QUEUE_POLL_MS, MAX_QUEUE_POLL_MS);
                } else {
                    entry.rt.queued = true;
                    entry.rt.backoff_ms = QUEUE_POLL_MS;
                    record(entry, now, TriggerHistoryEvent::Queued);
                    durable = true;
                    announce = true;
                }
                entry.rt.poll_at = Some(now + entry.rt.backoff_ms);
            }
            Outcome::RateLimited { until } => {
                entry.rt.fire_due = Some(entry.rt.fire_due.map_or(until, |t| t.max(until)));
                if !entry.rt.rate_noted {
                    entry.rt.rate_noted = true;
                    record(entry, now, TriggerHistoryEvent::RateLimited { until_ms: until });
                    durable = true;
                    announce = true;
                }
            }
            Outcome::Fired { run: id, changed, current: digests, model_requests } => {
                let stored = &mut entry.stored;
                stored.trigger.last_fired_ms = Some(now);
                stored.fires.retain(|f| within(now, f.at_ms, DAY_MS));
                stored.fires.push(FireRecord { at_ms: now, model_requests });
                stored.baseline.extend(digests);
                stored.start_refusals = 0;
                entry.rt.queued = false;
                entry.rt.poll_at = None;
                entry.rt.backoff_ms = 0;
                entry.rt.rate_noted = false;
                record(entry, now, TriggerHistoryEvent::Fired { run: id.clone(), changed });
                run = Some(id);
                durable = true;
                announce = true;
            }
            Outcome::StartRefused { code, message } => {
                entry.stored.start_refusals += 1;
                if entry.stored.start_refusals >= MAX_START_REFUSALS {
                    suspend(entry, now, &code, message);
                } else {
                    record(entry, now, TriggerHistoryEvent::StartRefused { code, message });
                    entry.rt.fire_due = Some(now + debounce_ms(&entry.stored.trigger));
                }
                durable = true;
                announce = true;
            }
        }
        // An interval trigger schedules its next run after each scheduled check.
        if job.fire
            && entry.stored.trigger.state == TriggerState::Active
            && !entry.rt.queued
            && entry.rt.fire_due.is_none()
            && let Some(every) = interval_ms(&entry.stored.trigger.kind)
        {
            entry.rt.fire_due = Some(now + every);
        }
        if durable {
            self.persist_background(&mut engine);
        }
        if announce {
            let event = engine.event(&engine.entries[i], run);
            drop(engine);
            self.emit(event);
        }
    }

    fn next_due(&self) -> Option<u64> {
        let engine = lock(&self.shared.engine);
        let session = engine.session.as_ref()?;
        let now = self.now();
        if session.indexed && session.ready_at.is_none() {
            return Some(now);
        }
        engine
            .entries
            .iter()
            .filter(|e| e.stored.project_key == session.key && e.stored.trigger.state == TriggerState::Active)
            .flat_map(|e| {
                let poll = if e.rt.queued && e.rt.fire_due.is_none() { e.rt.poll_at } else { None };
                [e.rt.fire_due, e.rt.check_due, poll]
            })
            .flatten()
            .min()
    }

    /// Run the engine on its own thread until [`Self::shutdown`].
    pub fn spawn_driver(&self) -> std::io::Result<()> {
        let service = self.clone();
        std::thread::Builder::new().name("workflow-triggers".into()).spawn(move || {
            while !service.shared.stopped.load(Ordering::SeqCst) {
                let next = service.tick();
                let now = service.now();
                let wait = next.map_or(60_000, |t| t.saturating_sub(now)).clamp(20, 60_000);
                let mut signaled = lock(&service.shared.signal);
                if !*signaled {
                    signaled = service
                        .shared
                        .wake
                        .wait_timeout(signaled, Duration::from_millis(wait))
                        .map(|(g, _)| g)
                        .unwrap_or_else(|p| p.into_inner().0);
                }
                *signaled = false;
            }
        })?;
        Ok(())
    }

    /// Stop the driver thread.
    pub fn shutdown(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        self.wake();
    }
}
