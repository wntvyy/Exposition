//! Trigger engine tests: a real project graph, the real run service and plan
//! builder (fake batch runner / reviewer behind the real adapters), the real
//! trigger store, and a manual clock. Starts are recorded by default (the
//! engine's decision is what is tested); `real_run_records_the_trigger_cause`
//! starts a real run through the run service.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use exposition_graph::{
    GraphConfig, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowDefinition,
    WorkflowId, WorkflowRunId,
};
use exposition_research::ResearchError;
use exposition_research::batch::{BatchControl, BatchMode, BatchPortOutput, BatchRequest, BatchResult, BatchRunner};
use serde_json::{Value, json};

use super::*;
use crate::adapters::agent::{AgentReviewAdapter, AgentReviewer, ReviewOutcome, ReviewRequest};
use crate::adapters::batch::BatchAdapter;
use crate::adapters::{Adapters, StepFailure, runner_ids, sha256_hex};
use crate::model::{WorkflowRunCause, WorkflowRunError, WorkflowRunPlan, WorkflowRunState};
use crate::service::{WorkflowHost, WorkflowRunConfig, WorkflowRunService};

// ---------------------------------------------------------------- fakes ----

const IMAGE_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IMAGE_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SECOND: u64 = 1000;
const MINUTE: u64 = 60 * SECOND;

struct Host {
    root: Mutex<Option<PathBuf>>,
    trusted: AtomicBool,
    image_id: Mutex<Option<String>>,
    graph: Arc<GraphService>,
}

impl WorkflowHost for Host {
    fn project_root(&self) -> Option<PathBuf> {
        self.root.lock().unwrap().clone()
    }
    fn trusted(&self, _root: &Path) -> bool {
        self.trusted.load(Ordering::SeqCst)
    }
    fn restricted(&self) -> (Option<String>, Option<String>) {
        (Some("docker".into()), Some("python:3.13-slim".into()))
    }
    fn restricted_image_id(&self) -> Option<String> {
        self.image_id.lock().unwrap().clone()
    }
    fn workflow(
        &self,
        _root: &Path,
        id: &WorkflowId,
        revision: Option<u64>,
    ) -> Result<SavedWorkflow, WorkflowRunError> {
        self.graph.workflow(id, revision).map_err(|e| WorkflowRunError::new(&e.code, e.message))
    }
}

struct FakeBatch {
    available: Mutex<Result<(), String>>,
    runs: AtomicUsize,
}

impl BatchRunner for FakeBatch {
    fn availability(&self, _root: &Path, _mode: BatchMode) -> Result<(), String> {
        self.available.lock().unwrap().clone()
    }
    fn run(
        &self,
        root: &Path,
        _request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError> {
        let run = format!("run-{}", self.runs.fetch_add(1, Ordering::SeqCst) + 1);
        (control.on_dispatched)(&run);
        let rel = format!(".exposition/artifacts/{run}/dataset.csv");
        std::fs::create_dir_all(root.join(format!(".exposition/artifacts/{run}"))).unwrap();
        std::fs::write(root.join(&rel), b"x\n1\n").unwrap();
        Ok(BatchResult {
            research_run: run,
            status: RunStatus::Succeeded,
            exit_code: Some(0),
            ports: vec![BatchPortOutput {
                port: "dataset".into(),
                artifact: ArtifactRecord {
                    id: ArtifactId("a-dataset".into()),
                    kind: ArtifactKind::Data,
                    path: Some(WorkspacePath::parse(&rel).unwrap()),
                    scratch_path: None,
                    media_type: "text/csv".into(),
                    size_bytes: 4,
                    content_hash: format!("sha256:{}", sha256_hex(b"x\n1\n")),
                },
            }],
            extra: Vec::new(),
            rejected: Vec::new(),
            log: "ok".into(),
            environment: Vec::new(),
        })
    }
}

struct FakeReviewer;

impl AgentReviewer for FakeReviewer {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn review(
        &self,
        _request: &ReviewRequest,
        _canceled: &(dyn Fn() -> bool + Sync),
        _on_task: &(dyn Fn(&str) + Sync),
    ) -> Result<ReviewOutcome, StepFailure> {
        Ok(ReviewOutcome { text: "ok".into(), task: "task-1".into(), requests: 1 })
    }
}

struct ManualClock(AtomicU64);

impl TriggerClock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// The real run-service starter, with starts recorded instead of run (unless
/// `real_start`) and an optional override of "a run of the workflow is active".
struct TestStarter {
    real: RunServiceStarter,
    real_start: AtomicBool,
    active: AtomicBool,
    started: Mutex<Vec<(WorkflowRunPlan, WorkflowRunCause)>>,
    plans: AtomicUsize,
    /// Other workflows hold every run slot: plans carry `workflow.activeRun`.
    cap: AtomicBool,
    /// How often the engine asked whether the workflow can start now.
    busy_checks: AtomicUsize,
}

impl TestStarter {
    fn capped(&self, mut plan: WorkflowRunPlan) -> WorkflowRunPlan {
        if self.cap.load(Ordering::SeqCst) {
            plan.blockers.push(crate::model::WorkflowPlanBlocker {
                code: "workflow.activeRun".into(),
                message: "3 workflow runs are active (at most 3 at once).".into(),
                step: None,
                input: None,
            });
        }
        plan
    }
}

impl TriggerRunStarter for TestStarter {
    fn project_root(&self) -> Option<PathBuf> {
        self.real.project_root()
    }
    fn preview(&self, workflow: &WorkflowId) -> Result<WorkflowRunPlan, WorkflowRunError> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        self.real.preview(workflow).map(|p| self.capped(p))
    }
    fn plan_to_start(&self, workflow: &WorkflowId) -> Result<WorkflowRunPlan, WorkflowRunError> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        self.real.plan_to_start(workflow).map(|p| self.capped(p))
    }
    fn busy(&self, workflow: &WorkflowId) -> bool {
        self.busy_checks.fetch_add(1, Ordering::SeqCst);
        self.active.load(Ordering::SeqCst) || self.real.busy(workflow)
    }
    fn start(&self, plan: &WorkflowRunPlan, cause: WorkflowRunCause) -> Result<WorkflowRunId, WorkflowRunError> {
        self.started.lock().unwrap().push((plan.clone(), cause.clone()));
        if self.real_start.load(Ordering::SeqCst) { self.real.start(plan, cause) } else { Ok(WorkflowRunId::mint()) }
    }
}

// ---------------------------------------------------------------- setup ----

struct Fx {
    _data: tempfile::TempDir,
    _project: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
    host: Arc<Host>,
    graph: Arc<GraphService>,
    batch: Arc<FakeBatch>,
    runs: WorkflowRunService,
    starter: Arc<TestStarter>,
    clock: Arc<ManualClock>,
    triggers: TriggerService,
}

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

const SCRIPT: &str = "import os\nprint('experiment')\n";

fn fixture() -> Fx {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/obs.csv"), "x\n1\n2\n").unwrap();
    std::fs::write(root.join("data/extra.csv"), "y\n5\n").unwrap();
    std::fs::write(root.join("experiment.py"), SCRIPT).unwrap();
    std::fs::write(root.join("notes.md"), "# Notes\n").unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    assert!(graph.attach(&root).is_usable());
    let host = Arc::new(Host {
        root: Mutex::new(Some(root.clone())),
        trusted: AtomicBool::new(true),
        image_id: Mutex::new(Some(IMAGE_A.into())),
        graph: graph.clone(),
    });
    let batch = Arc::new(FakeBatch { available: Mutex::new(Ok(())), runs: AtomicUsize::new(0) });
    let mut adapters: Adapters = BTreeMap::new();
    let runner: Arc<dyn BatchRunner> = batch.clone();
    adapters.insert(
        runner_ids::RESTRICTED_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::Restricted)),
    );
    adapters
        .insert(runner_ids::NATIVE_PYTHON.into(), Arc::new(BatchAdapter::new(Some(runner), BatchMode::TrustedNative)));
    adapters.insert(
        runner_ids::READ_ONLY_AGENT.into(),
        Arc::new(AgentReviewAdapter { reviewer: Some(Arc::new(FakeReviewer)) }),
    );
    let h: Arc<dyn WorkflowHost> = host.clone();
    let runs = WorkflowRunService::new(WorkflowRunConfig {
        database: Some(data.path().join("workflow-runs.sqlite3")),
        host: h.clone(),
        adapters,
        lean: None,
        research: None,
    })
    .unwrap();
    let starter = Arc::new(TestStarter {
        real: RunServiceStarter { runs: runs.clone(), host: h },
        real_start: AtomicBool::new(false),
        active: AtomicBool::new(false),
        started: Mutex::new(Vec::new()),
        plans: AtomicUsize::new(0),
        cap: AtomicBool::new(false),
        busy_checks: AtomicUsize::new(0),
    });
    let clock = Arc::new(ManualClock(AtomicU64::new(crate::now_ms())));
    let store = data.path().join(store::STORE_FILE);
    let triggers = TriggerService::new(TriggerConfig {
        store: Some(store.clone()),
        starter: starter.clone(),
        clock: clock.clone(),
    });
    Fx { _data: data, _project: project, root, store, host, graph, batch, runs, starter, clock, triggers }
}

/// One restricted Python step reading a script (code), the observations
/// (watched in most tests), and extra data; `native`: the trusted native
/// runner; `review`: plus a read-only agent review of the result.
fn definition(native: bool, review: bool) -> Value {
    let mut steps = vec![json!({
        "id": "run", "label": "Run the experiment",
        "operation": {"id": if native { "research.nativePython" } else { "research.pythonBatch" }, "version": 1},
        "parameters": {}, "payload": null
    })];
    let mut connections = vec![
        json!({"kind": "data", "from": {"kind": "input", "input": "script"}, "to": {"step": "run", "port": "script"}}),
        json!({"kind": "data", "from": {"kind": "input", "input": "data"}, "to": {"step": "run", "port": "data"}}),
        json!({"kind": "data", "from": {"kind": "input", "input": "extra"}, "to": {"step": "run", "port": "data"}}),
    ];
    let mut outcomes = vec![
        json!({"id": "out", "kind": "transformedDataset", "label": "Result", "from": {"step": "run", "port": "dataset"}, "optional": false}),
    ];
    if review {
        steps.push(json!({
            "id": "review", "label": "Review the result",
            "operation": {"id": "agent.review", "version": 1},
            "parameters": {}, "payload": {"kind": "prompt", "text": "Summarize the result."}
        }));
        connections.push(
            json!({"kind": "data", "from": {"kind": "step", "step": "run", "port": "dataset"}, "to": {"step": "review", "port": "material"}}),
        );
        outcomes.push(json!({"id": "report", "kind": "investigationReport", "label": "Review", "from": {"step": "review", "port": "report"}, "optional": false}));
    }
    let resource = |id: &str, label: &str, ty: &str, p: &str| json!({"id": id, "label": label, "portType": ty, "source": {"kind": "resource", "path": p, "version": {"kind": "atRunStart"}}});
    json!({
        "schema": "exposition.workflow/1",
        "title": "Trigger test",
        "description": "",
        "preset": null,
        "inputs": [
            resource("script", "Python script", "codeSource", "experiment.py"),
            resource("data", "Observations", "dataset", "data/obs.csv"),
            resource("extra", "Extra data", "dataset", "data/extra.csv"),
        ],
        "steps": steps,
        "connections": connections,
        "outcomes": outcomes,
        "ceiling": {
            "execution": if native { "trustedNative" } else { "restricted" },
            "modelRequests": review, "retainResults": false, "proposeChanges": false
        }
    })
}

fn watch(inputs: &[&str]) -> TriggerKind {
    TriggerKind::InputsChanged { inputs: inputs.iter().map(|s| s.to_string()).collect() }
}

fn settings() -> WorkflowTriggerSettings {
    WorkflowTriggerSettings {
        debounce_seconds: 2,
        max_runs_per_hour: 12,
        max_runs_per_day: 100,
        allow_native: false,
        max_model_requests_per_day: 0,
    }
}

impl Fx {
    fn save(&self, definition: Value) -> WorkflowId {
        let definition: WorkflowDefinition = serde_json::from_value(definition).unwrap();
        let saved = self
            .graph
            .save_workflow(
                &Origin::User,
                &SaveWorkflowRequest { id: None, expected_revision: None, definition },
                &ValidationContext::default(),
            )
            .unwrap();
        saved.id
    }

    fn save_revision(&self, id: &WorkflowId, definition: Value) {
        let current = self.graph.workflow(id, None).unwrap();
        let definition: WorkflowDefinition = serde_json::from_value(definition).unwrap();
        self.graph
            .save_workflow(
                &Origin::User,
                &SaveWorkflowRequest { id: Some(id.clone()), expected_revision: Some(current.revision), definition },
                &ValidationContext::default(),
            )
            .unwrap();
    }

    fn review(&self, id: &WorkflowId, kind: TriggerKind) -> WorkflowTriggerPlan {
        self.triggers.plan(&WorkflowTriggerPlanRequest { workflow: id.clone(), kind }).unwrap()
    }

    fn request(&self, review: &WorkflowTriggerPlan, settings: WorkflowTriggerSettings) -> EnableWorkflowTriggerRequest {
        EnableWorkflowTriggerRequest {
            plan: review.plan.plan.clone(),
            standing_hash: review.standing_hash.clone(),
            settings,
        }
    }

    fn enable_with(&self, id: &WorkflowId, kind: TriggerKind, settings: WorkflowTriggerSettings) -> WorkflowTrigger {
        let review = self.review(id, kind);
        assert!(review.blockers.is_empty(), "{:?}", review.blockers);
        self.triggers.enable(&self.request(&review, settings)).unwrap()
    }

    fn enable(&self, id: &WorkflowId, kind: TriggerKind) -> WorkflowTrigger {
        self.enable_with(id, kind, settings())
    }

    /// Open the project and let it finish indexing.
    fn open_ready(&self) {
        self.triggers.project_opening(&self.root);
        self.triggers.project_indexed(&self.root);
        self.triggers.tick();
    }

    /// Save new content (as the watcher reports it).
    fn save_file(&self, rel: &str, content: &str) {
        std::fs::write(self.root.join(rel), content).unwrap();
        self.triggers.files_changed(&[path(rel)], false);
    }

    fn advance(&self, ms: u64) {
        self.clock.0.fetch_add(ms, Ordering::SeqCst);
        self.triggers.tick();
    }

    fn now(&self) -> u64 {
        self.clock.0.load(Ordering::SeqCst)
    }

    fn started(&self) -> Vec<(WorkflowRunPlan, WorkflowRunCause)> {
        self.starter.started.lock().unwrap().clone()
    }

    fn trigger(&self, id: &WorkflowTriggerId) -> WorkflowTrigger {
        self.triggers.list(None).unwrap().into_iter().find(|t| &t.id == id).unwrap()
    }

    fn suspended(&self, id: &WorkflowTriggerId) -> (String, String) {
        match self.trigger(id).state {
            TriggerState::Suspended { code, message } => (code, message),
            other => panic!("expected a suspended trigger, got {other:?}"),
        }
    }
}

fn digest(s: &str) -> String {
    sha256_hex(s.as_bytes())
}

fn trigger_cause(t: &WorkflowTrigger, input: &str, before: &str, after: &str) -> WorkflowRunCause {
    WorkflowRunCause::Trigger {
        trigger: t.id.to_string(),
        changed: vec![ChangedInput { input: input.into(), before: Some(digest(before)), after: Some(digest(after)) }],
    }
}

// ------------------------------------------------- open / close / no catch-up

#[test]
fn opening_and_indexing_never_fire() {
    let f = fixture();
    let id = f.save(definition(false, false));
    let t = f.enable(&id, watch(&["data"]));
    assert!(!t.listening, "nothing listens before the project is open and indexed");
    // Opening: saved changes while the project is still indexing are ignored.
    f.triggers.project_opening(&f.root);
    f.save_file("data/obs.csv", "x\n10\n");
    f.advance(10 * SECOND);
    assert!(!f.trigger(&t.id).listening);
    // Indexing finished: listening starts, but nothing runs because of it.
    f.triggers.project_indexed(&f.root);
    f.triggers.tick();
    assert!(f.trigger(&t.id).listening);
    f.advance(HOUR_MS);
    f.triggers.files_changed(&[], true); // a watcher rescan right after opening
    f.advance(10 * SECOND);
    assert!(f.started().is_empty(), "opening or indexing started a run: {:?}", f.started());
    assert_eq!(f.starter.plans.load(Ordering::SeqCst), 3, "only the review, the enable, and the rescan check planned");
}

#[test]
fn changes_made_while_closed_never_fire_on_reopen() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    f.triggers.project_closed();
    // Saved while the project was closed: no event reaches the engine, and
    // the file differs from the trigger's baseline.
    std::fs::write(f.root.join("data/obs.csv"), "x\nclosed\n").unwrap();
    f.open_ready();
    f.triggers.files_changed(&[], true);
    f.triggers.files_changed(&[path("data/obs.csv")], false); // touched, same content
    f.advance(MINUTE);
    assert!(f.started().is_empty(), "a change made while closed started a run: {:?}", f.started());
    // A later saved change fires, and its "before" is what was there on reopen.
    f.save_file("data/obs.csv", "x\nafter\n");
    f.advance(3 * SECOND);
    let started = f.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].1, trigger_cause(&t, "data", "x\nclosed\n", "x\nafter\n"));
}

#[test]
fn closing_the_project_drops_pending_checks_and_the_queued_follow_up() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    f.save_file("data/obs.csv", "x\n2\n");
    f.triggers.project_closed();
    f.advance(MINUTE);
    assert!(f.started().is_empty());
    assert_eq!(f.trigger(&t.id).next_check_ms, None);
    // A queued follow-up, then close: dropped too.
    f.open_ready();
    f.starter.active.store(true, Ordering::SeqCst);
    f.save_file("data/obs.csv", "x\n3\n");
    f.advance(3 * SECOND);
    assert!(f.trigger(&t.id).queued);
    f.triggers.project_closed();
    f.starter.active.store(false, Ordering::SeqCst);
    f.advance(MINUTE);
    assert!(f.started().is_empty(), "a dropped follow-up ran after the project closed");
}

// ------------------------------------------- debounce / coalesce / limits ---

#[test]
fn saves_are_debounced_into_one_run_with_its_cause() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    assert!(t.listening && t.state == TriggerState::Active);
    for (i, content) in ["x\n1\n", "x\n1\n2\n", "x\n1\n2\n3\n"].iter().enumerate() {
        if i > 0 {
            f.advance(500);
        }
        f.save_file("data/obs.csv", content);
    }
    f.advance(1900);
    assert!(f.started().is_empty(), "fired before the quiet period ended");
    f.advance(100);
    let started = f.started();
    assert_eq!(started.len(), 1, "one run for a burst of saves");
    // "before" is the content when the trigger was enabled.
    assert_eq!(started[0].1, trigger_cause(&t, "data", "x\n1\n2\n", "x\n1\n2\n3\n"));
    // The run reads exactly the latest content.
    assert_eq!(started[0].0.inputs.iter().find(|i| i.id == "data").unwrap().sha256, digest("x\n1\n2\n3\n"));
    let after = f.trigger(&t.id);
    assert!(after.last_fired_ms.is_some());
    assert!(matches!(after.history.last().unwrap().event, TriggerHistoryEvent::Fired { .. }));
    // Unrelated files and content-free saves start nothing.
    f.save_file("notes.md", "# Notes\nmore\n");
    f.triggers.files_changed(&[path("data/obs.csv")], false);
    f.advance(MINUTE);
    assert_eq!(f.started().len(), 1);
}

#[test]
fn an_active_run_queues_one_follow_up_and_the_latest_content_wins() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    f.starter.active.store(true, Ordering::SeqCst);
    f.save_file("data/obs.csv", "x\nA\n");
    f.advance(3 * SECOND);
    assert!(f.trigger(&t.id).queued);
    f.save_file("data/obs.csv", "x\nB\n");
    f.advance(3 * SECOND);
    f.save_file("data/obs.csv", "x\nC\n");
    f.advance(3 * SECOND);
    assert!(f.started().is_empty());
    let queued = f.trigger(&t.id).history.iter().filter(|h| h.event == TriggerHistoryEvent::Queued).count();
    assert_eq!(queued, 1, "at most one queued follow-up");
    // The run ends (the app reports it): the follow-up starts at once.
    f.starter.active.store(false, Ordering::SeqCst);
    f.triggers.runs_changed();
    f.advance(SECOND);
    let started = f.started();
    assert_eq!(started.len(), 1, "exactly one follow-up");
    let WorkflowRunCause::Trigger { changed, .. } = &started[0].1 else { panic!("not a trigger cause") };
    assert_eq!(changed[0].after, Some(digest("x\nC\n")), "latest wins");
    assert!(!f.trigger(&t.id).queued);
    f.advance(MINUTE);
    assert_eq!(f.started().len(), 1);
}

#[test]
fn rate_limits_defer_runs_until_the_window_allows_one() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let limits = WorkflowTriggerSettings { max_runs_per_hour: 2, max_runs_per_day: 3, ..settings() };
    let t = f.enable_with(&id, watch(&["data"]), limits);
    let first = f.now() + 2 * SECOND;
    for n in 0..3 {
        f.save_file("data/obs.csv", &format!("x\n{n}\n"));
        f.advance(2 * SECOND);
    }
    assert_eq!(f.started().len(), 2, "two runs per hour");
    let view = f.trigger(&t.id);
    assert_eq!(view.runs_last_hour, 2);
    let until = view
        .history
        .iter()
        .find_map(|h| match h.event {
            TriggerHistoryEvent::RateLimited { until_ms } => Some(until_ms),
            _ => None,
        })
        .expect("rate limit recorded");
    assert_eq!(until, first + HOUR_MS);
    assert_eq!(view.next_check_ms, Some(until));
    f.advance(until - f.now() - SECOND);
    assert_eq!(f.started().len(), 2);
    f.advance(SECOND);
    assert_eq!(f.started().len(), 3, "the deferred change runs once the window allows it");
    // The daily limit (3) now holds for the rest of the day.
    f.advance(2 * HOUR_MS);
    f.save_file("data/obs.csv", "x\nday\n");
    f.advance(3 * SECOND);
    assert_eq!(f.started().len(), 3, "runs per day");
}

#[test]
fn rate_limit_arithmetic() {
    let s = WorkflowTriggerSettings {
        max_runs_per_hour: 2,
        max_runs_per_day: 3,
        max_model_requests_per_day: 50,
        ..settings()
    };
    let fire = |at_ms, model_requests| FireRecord { at_ms, model_requests };
    let now = 10 * DAY_MS;
    assert_eq!(rate_limited(&[], now, &s, 25), None);
    assert_eq!(rate_limited(&[fire(now - 10, 0)], now, &s, 0), None);
    assert_eq!(rate_limited(&[fire(now - 20, 0), fire(now - 10, 0)], now, &s, 0), Some(now - 20 + HOUR_MS));
    let day = [fire(now - 5 * HOUR_MS, 0), fire(now - 3 * HOUR_MS, 0), fire(now - 2 * HOUR_MS, 0)];
    assert_eq!(rate_limited(&day, now, &s, 0), Some(now - 5 * HOUR_MS + DAY_MS));
    // Model requests: 25 + 25 used; another 25 exceeds 50 until the first expires.
    let model = [fire(now - 5 * HOUR_MS, 25), fire(now - 3 * HOUR_MS, 25)];
    assert_eq!(rate_limited(&model, now, &s, 25), Some(now - 5 * HOUR_MS + DAY_MS));
    assert_eq!(rate_limited(&model[..1], now, &s, 25), None);
}

#[test]
fn model_steps_count_against_a_daily_cap() {
    let f = fixture();
    let id = f.save(definition(false, true));
    f.open_ready();
    let review = f.review(&id, watch(&["data"]));
    assert!(review.blockers.is_empty(), "{:?}", review.blockers);
    assert_eq!(review.model_requests_per_run, crate::plan::MODEL_REQUESTS_PER_STEP);
    assert!(review.notices.iter().any(|n| n.contains("model requests")), "{:?}", review.notices);
    // A cap that does not allow one run is refused.
    let low = f
        .triggers
        .enable(&f.request(&review, WorkflowTriggerSettings { max_model_requests_per_day: 24, ..settings() }))
        .unwrap_err();
    assert_eq!(low.code, "trigger.modelCapTooLow");
    // Two runs' worth per day: the third waits.
    let capped = WorkflowTriggerSettings { max_model_requests_per_day: 50, ..settings() };
    let t = f.enable_with(&id, watch(&["data"]), capped);
    for n in 0..3 {
        f.save_file("data/obs.csv", &format!("x\nm{n}\n"));
        f.advance(2 * SECOND);
    }
    assert_eq!(f.started().len(), 2);
    let view = f.trigger(&t.id);
    assert_eq!(view.model_requests_last_day, 50);
    assert!(view.history.iter().any(|h| matches!(h.event, TriggerHistoryEvent::RateLimited { .. })));
}

#[test]
fn interval_triggers_fire_on_schedule_only_while_open() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let short = f
        .triggers
        .plan(&WorkflowTriggerPlanRequest {
            workflow: id.clone(),
            kind: TriggerKind::Interval { minutes: 14, inputs: vec![] },
        })
        .unwrap_err();
    assert_eq!(short.code, "trigger.invalidInterval");
    let t = f.enable(&id, TriggerKind::Interval { minutes: 15, inputs: vec!["data".into()] });
    f.advance(14 * MINUTE);
    assert!(f.started().is_empty());
    f.advance(MINUTE);
    assert_eq!(f.started().len(), 1, "fires on schedule");
    assert_eq!(f.started()[0].1, WorkflowRunCause::Trigger { trigger: t.id.to_string(), changed: vec![] });
    // The allowed input may change without suspending; a change does not fire early.
    f.save_file("data/obs.csv", "x\nnew\n");
    f.advance(5 * MINUTE);
    assert_eq!(f.started().len(), 1);
    assert_eq!(f.trigger(&t.id).state, TriggerState::Active);
    f.advance(10 * MINUTE);
    assert_eq!(f.started().len(), 2);
    // Closed: no schedule. Reopened: no catch-up; the next run one interval after opening.
    f.triggers.project_closed();
    f.advance(3 * HOUR_MS);
    assert_eq!(f.started().len(), 2, "fired while the project was closed");
    f.open_ready();
    f.advance(14 * MINUTE);
    assert_eq!(f.started().len(), 2, "caught up on reopening");
    f.advance(MINUTE);
    assert_eq!(f.started().len(), 3);
}

// ------------------------------------------------------- suspension -------

/// Enable a data-watching trigger, apply `change`, save the watched input,
/// and return the suspension (code, message).
fn suspended_by(native: bool, change: impl FnOnce(&Fx, &WorkflowId)) -> (String, String) {
    let f = fixture();
    let id = f.save(definition(native, false));
    f.open_ready();
    let t = f.enable_with(&id, watch(&["data"]), WorkflowTriggerSettings { allow_native: native, ..settings() });
    change(&f, &id);
    f.save_file("data/obs.csv", "x\nchanged\n");
    f.advance(3 * SECOND);
    assert!(f.started().is_empty(), "a run started although the standing plan changed: {:?}", f.started());
    let suspended = f.suspended(&t.id);
    assert!(!f.trigger(&t.id).listening);
    // Suspended stays suspended: later changes start nothing.
    f.save_file("data/obs.csv", "x\nagain\n");
    f.advance(MINUTE);
    assert!(f.started().is_empty());
    suspended
}

#[test]
fn a_new_revision_suspends() {
    let (code, message) = suspended_by(false, |f, id| {
        let mut def = definition(false, false);
        def["steps"][0]["parameters"] = json!({"timeoutSeconds": {"kind": "integer", "value": 60}});
        f.save_revision(id, def);
    });
    assert_eq!(code, "trigger.planChanged");
    assert!(message.contains("saved again (revision 1 → 2)"), "{message}");
}

#[test]
fn a_code_input_change_suspends_promptly() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    // The code is not watched: its saved change is checked after the quiet
    // period and suspends the trigger without waiting for a data change.
    f.save_file("experiment.py", "import os\nprint('changed')\n");
    f.advance(3 * SECOND);
    let (code, message) = f.suspended(&t.id);
    assert_eq!(code, "trigger.planChanged");
    assert!(message.contains("“Python script” (experiment.py) changed"), "{message}");
    f.save_file("data/obs.csv", "x\nchanged\n");
    f.advance(MINUTE);
    assert!(f.started().is_empty());
}

#[test]
fn another_unwatched_input_change_suspends() {
    let (code, message) = suspended_by(false, |f, _| {
        std::fs::write(f.root.join("data/extra.csv"), "y\n6\n").unwrap();
    });
    assert_eq!(code, "trigger.planChanged");
    assert!(message.contains("“Extra data” (data/extra.csv) changed"), "{message}");
}

#[test]
fn a_different_restricted_image_suspends() {
    let (code, message) = suspended_by(false, |f, _| {
        *f.host.image_id.lock().unwrap() = Some(IMAGE_B.into());
    });
    assert_eq!(code, "trigger.planChanged");
    assert!(message.contains("is now a different image"), "{message}");
}

#[test]
fn runner_unavailability_suspends() {
    let (_, message) = suspended_by(false, |f, _| {
        *f.batch.available.lock().unwrap() = Err("Docker is not running".into());
    });
    assert!(message.contains("is no longer available"), "{message}");
}

#[test]
fn a_trust_change_suspends_a_native_plan() {
    let (code, message) = suspended_by(true, |f, _| {
        f.host.trusted.store(false, Ordering::SeqCst);
    });
    assert_eq!(code, "trigger.planChanged");
    assert!(message.contains("no longer trusted"), "{message}");
}

#[test]
fn a_removed_watched_input_suspends() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    std::fs::remove_file(f.root.join("data/obs.csv")).unwrap();
    f.triggers.files_changed(&[path("data/obs.csv")], false);
    f.advance(3 * SECOND);
    let (_, message) = f.suspended(&t.id);
    assert!(message.contains("no longer available"), "{message}");
    assert!(f.started().is_empty());
}

#[test]
fn trust_revocation_suspends_the_folders_triggers() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    f.save_file("data/obs.csv", "x\npending\n");
    // Revoking a parent folder covers the project.
    f.triggers.trust_revoked(f.root.parent().unwrap());
    let (code, _) = f.suspended(&t.id);
    assert_eq!(code, "trigger.trustRevoked");
    f.advance(MINUTE);
    assert!(f.started().is_empty(), "the pending change ran after trust was revoked");
    // Re-approval makes it active again (same id, history kept).
    let again = f.enable(&id, watch(&["data"]));
    assert_eq!(again.id, t.id);
    assert_eq!(again.state, TriggerState::Active);
    assert!(again.history.iter().any(|h| matches!(h.event, TriggerHistoryEvent::Suspended { .. })));
}

#[test]
fn disabling_stops_pending_checks() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    f.save_file("data/obs.csv", "x\n9\n");
    let off = f.triggers.disable(&t.id).unwrap();
    assert_eq!(off.state, TriggerState::Disabled);
    f.advance(MINUTE);
    assert!(f.started().is_empty());
}

// ------------------------------------------------- refusals at review/enable

fn plan_err(f: &Fx, id: &WorkflowId, kind: TriggerKind) -> WorkflowRunError {
    f.triggers.plan(&WorkflowTriggerPlanRequest { workflow: id.clone(), kind }).unwrap_err()
}

#[test]
fn code_inputs_cannot_be_watched() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let err = plan_err(&f, &id, watch(&["script"]));
    assert_eq!(err.code, "trigger.codeInputWatched", "{}", err.message);
    let err = plan_err(&f, &id, TriggerKind::Interval { minutes: 30, inputs: vec!["script".into()] });
    assert_eq!(err.code, "trigger.codeInputWatched");
    assert_eq!(plan_err(&f, &id, watch(&["nope"])).code, "trigger.unknownInput");
    assert_eq!(plan_err(&f, &id, watch(&[])).code, "trigger.noInputs");
    assert_eq!(plan_err(&f, &id, watch(&["data", "data"])).code, "trigger.duplicateInput");
    // A pinned input can never change without blocking, so it cannot be watched.
    let mut pinned = definition(false, false);
    pinned["inputs"][1]["source"]["version"] = json!({"kind": "pinned", "contentHash": digest("x\n1\n2\n")});
    let pinned = f.save(pinned);
    assert_eq!(plan_err(&f, &pinned, watch(&["data"])).code, "trigger.inputPinned");
    assert!(f.triggers.list(None).unwrap().is_empty());
}

#[test]
fn native_steps_need_explicit_allow_native() {
    let f = fixture();
    let id = f.save(definition(true, false));
    f.open_ready();
    let review = f.review(&id, watch(&["data"]));
    assert_eq!(review.native_steps, vec!["Run the experiment".to_owned()]);
    assert!(review.notices.iter().any(|n| n.contains("natively")), "{:?}", review.notices);
    let err = f.triggers.enable(&f.request(&review, settings())).unwrap_err();
    assert_eq!(err.code, "trigger.nativeNotAllowed", "{}", err.message);
    assert!(f.triggers.list(None).unwrap().is_empty());
    let t = f.enable_with(&id, watch(&["data"]), WorkflowTriggerSettings { allow_native: true, ..settings() });
    assert!(t.allow_native);
    // A restricted-only plan needs no native permission.
    let restricted = f.save(definition(false, false));
    assert!(f.review(&restricted, watch(&["data"])).native_steps.is_empty());
}

#[test]
fn enabling_requires_the_current_standing_plan() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    // A wrong hash; then the consumed review.
    let review = f.review(&id, watch(&["data"]));
    let mut wrong = f.request(&review, settings());
    wrong.standing_hash = "0".repeat(64);
    assert_eq!(f.triggers.enable(&wrong).unwrap_err().code, "trigger.planStale");
    assert_eq!(f.triggers.enable(&f.request(&review, settings())).unwrap_err().code, "trigger.planStale");
    // The code changed after the review.
    let review = f.review(&id, watch(&["data"]));
    std::fs::write(f.root.join("experiment.py"), "print('other')\n").unwrap();
    let err = f.triggers.enable(&f.request(&review, settings())).unwrap_err();
    assert_eq!(err.code, "trigger.planStale");
    assert!(err.message.contains("Python script"), "{}", err.message);
    // A watched input changing after the review is fine (it may change).
    let review = f.review(&id, watch(&["data"]));
    std::fs::write(f.root.join("data/obs.csv"), "x\nreviewed later\n").unwrap();
    f.triggers.enable(&f.request(&review, settings())).unwrap();
    // Out-of-range settings are refused.
    for bad in [
        WorkflowTriggerSettings { debounce_seconds: 1, ..settings() },
        WorkflowTriggerSettings { debounce_seconds: 3601, ..settings() },
        WorkflowTriggerSettings { max_runs_per_hour: 13, ..settings() },
        WorkflowTriggerSettings { max_runs_per_day: 101, ..settings() },
        WorkflowTriggerSettings { max_runs_per_hour: 0, ..settings() },
    ] {
        let review = f.review(&id, watch(&["data"]));
        let err = f.triggers.enable(&f.request(&review, bad.clone())).unwrap_err();
        assert_eq!(err.code, "trigger.invalidSettings", "{bad:?}");
    }
}

#[test]
fn the_standing_plan_states_what_may_change() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let review = f.review(&id, watch(&["data"]));
    assert_eq!(review.watched.len(), 1);
    assert_eq!(review.may_change, vec!["The saved content of “Observations” (data/obs.csv, dataset).".to_owned()]);
    assert!(review.suspends_on.iter().any(|s| s.contains("code in “Python script”")), "{:?}", review.suspends_on);
    assert!(review.suspends_on.iter().any(|s| s.contains("“Extra data”")));
    assert!(review.suspends_on.iter().any(|s| s.contains("revision 1 only")));
    assert!(review.suspends_on.iter().any(|s| s.contains("sha256:aaaaaaaaaaaa")));
    // The standing hash ignores the watched content and binds everything else.
    let plan = &review.plan;
    let ids = vec!["data".to_owned()];
    let mut other = plan.clone();
    other.inputs.iter_mut().find(|i| i.id == "data").unwrap().sha256 = "f".repeat(64);
    assert_eq!(standing::standing_hash(&other, &f.root, &ids), review.standing_hash);
    other.inputs.iter_mut().find(|i| i.id == "script").unwrap().sha256 = "f".repeat(64);
    assert_ne!(standing::standing_hash(&other, &f.root, &ids), review.standing_hash);
    assert_ne!(standing::standing_hash(plan, &f.root, &[]), review.standing_hash, "nothing blanked");
}

// ------------------------------------------- user-only, app-local records ---

#[test]
fn definitions_and_imports_cannot_carry_triggers() {
    let mut def = definition(false, false);
    def["triggers"] = json!([{"kind": "inputsChanged", "inputs": ["data"]}]);
    assert!(serde_json::from_value::<WorkflowDefinition>(def).is_err(), "a definition carried a trigger");
    let mut def = definition(false, false);
    def["trigger"] = json!({"kind": "interval", "minutes": 15});
    assert!(serde_json::from_value::<WorkflowDefinition>(def).is_err());
    let request = json!({
        "id": null, "expectedRevision": null, "definition": definition(false, false),
        "trigger": {"kind": "interval", "minutes": 15}
    });
    assert!(serde_json::from_value::<SaveWorkflowRequest>(request).is_err(), "a save request carried a trigger");
}

fn list_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn triggers_live_in_the_app_data_store_and_survive_restarts() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let before = list_files(&f.root);
    let t = f.enable(&id, watch(&["data"]));
    assert_eq!(list_files(&f.root), before, "enabling a trigger wrote into the project folder");
    assert!(f.store.exists());
    let text = std::fs::read_to_string(&f.store).unwrap();
    assert!(text.contains(t.id.as_str()) && text.contains(&t.standing_hash));
    // A new service (application restart) reloads it: active, but not
    // listening until the project is open and indexed again.
    let reloaded = TriggerService::new(TriggerConfig {
        store: Some(f.store.clone()),
        starter: f.starter.clone(),
        clock: f.clock.clone(),
    });
    let list = reloaded.list(None).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].state, TriggerState::Active);
    assert!(!list[0].listening && !list[0].queued);
    // An unreadable store is set aside: nothing is active.
    std::fs::write(&f.store, "{ not json").unwrap();
    let broken = TriggerService::new(TriggerConfig {
        store: Some(f.store.clone()),
        starter: f.starter.clone(),
        clock: f.clock.clone(),
    });
    assert!(broken.list(None).unwrap().is_empty());
    assert!(!f.store.exists(), "the unreadable store was set aside");
}

/// Agents have no trigger tool; an agent calling one (by any name) gets an
/// error and no trigger exists afterwards.
#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agents_cannot_create_or_enable_triggers() {
    use exposition_agents::credentials::MemoryStore;
    use exposition_agents::tools::services::UnavailableServices;
    use exposition_agents::{
        AgentConfig, AgentService, CreateTaskRequest, ExecutionMode, PermissionProfile, ProviderKind, ProviderSettings,
        TaskScope, TranscriptItem,
    };

    for tool in exposition_agents::tools::definitions() {
        let text = format!("{} {}", tool.name, tool.description).to_lowercase();
        assert!(!text.contains("trigger"), "agent tool {} mentions triggers", tool.name);
    }

    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    // A real standing plan the agent might try to use.
    let review = f.review(&id, watch(&["data"]));
    let store_before = std::fs::read(&f.store).ok();
    let data = tempfile::tempdir().unwrap();
    let agents = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::new(exposition_workspace::DocumentService::new()),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(UnavailableServices),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
        graph: None,
    })
    .unwrap();
    agents
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    let enable = json!({"plan": review.plan.plan, "standingHash": review.standing_hash, "settings": settings()});
    let plan = json!({"workflow": id.as_str(), "kind": {"kind": "inputsChanged", "inputs": ["data"]}});
    agents
        .load_script(json!({"turns": [
            {"steps": [
                {"toolUse": {"id": "t1", "name": "workflow_trigger_enable", "input": {"request": enable}}},
                {"toolUse": {"id": "t2", "name": "workflow_trigger_plan", "input": {"request": plan}}},
                {"toolUse": {"id": "t3", "name": "enable_trigger", "input": {}}}
            ]},
            {"steps": [{"text": "I could not enable a trigger."}]}
        ]}))
        .unwrap();
    let task = agents
        .create_task(CreateTaskRequest {
            prompt: "Enable a trigger for the workflow".into(),
            workspace_root: f.root.display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::Disabled,
            consent: None,
            budgets: None,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), agents.join(&task.id)).await.expect("task finished");
    let calls: Vec<(String, bool, String)> = agents
        .task_detail(&task.id)
        .unwrap()
        .transcript
        .into_iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall { name, is_error, result, .. } => {
                Some((name, is_error, result.unwrap_or_default()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 3, "{calls:?}");
    for (name, is_error, result) in &calls {
        assert!(*is_error, "{name} did not fail: {result}");
        assert!(result.contains("unknown tool"), "{name}: {result}");
    }
    assert!(f.triggers.list(None).unwrap().is_empty(), "an agent created a trigger");
    assert_eq!(std::fs::read(&f.store).ok(), store_before, "an agent wrote the trigger store");
}

// ---------------------------------------------------------- the real path --

#[test]
fn real_run_records_the_trigger_cause() {
    let f = fixture();
    f.starter.real_start.store(true, Ordering::SeqCst);
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    f.save_file("data/obs.csv", "x\nreal\n");
    f.advance(2 * SECOND);
    assert_eq!(f.started().len(), 1);
    assert!(f.runs.wait_idle(Duration::from_secs(20)));
    let summaries = f.runs.runs(Some(&id)).unwrap();
    assert_eq!(summaries.len(), 1);
    let run = f.runs.get(&summaries[0].id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?} {:?}", run.code, run.message);
    assert_eq!(run.cause, trigger_cause(&t, "data", "x\n1\n2\n", "x\nreal\n"));
    assert_eq!(run.plan.inputs.iter().find(|i| i.id == "data").unwrap().sha256, digest("x\nreal\n"));
    // The trigger's history names the run.
    let fired = f.trigger(&t.id).history.into_iter().find_map(|h| match h.event {
        TriggerHistoryEvent::Fired { run, .. } => Some(run),
        _ => None,
    });
    assert_eq!(fired, Some(run.id));
}

// ------------------------------------------------- fix round (docs audit) ---

/// Finding 1: a queued follow-up whose watched content returned to what the
/// trigger last saw is dropped (it used to poll every second forever).
#[test]
fn a_queued_follow_up_with_nothing_left_to_run_is_dropped() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    f.starter.active.store(true, Ordering::SeqCst);
    f.save_file("data/obs.csv", "x\nA\n");
    f.advance(3 * SECOND);
    assert!(f.trigger(&t.id).queued);
    f.save_file("data/obs.csv", "x\n1\n2\n"); // back to the baseline
    f.advance(3 * SECOND);
    f.starter.active.store(false, Ordering::SeqCst);
    f.advance(SECOND);
    let plans = f.starter.plans.load(Ordering::SeqCst);
    for _ in 0..60 {
        f.advance(SECOND);
    }
    assert!(!f.trigger(&t.id).queued, "the follow-up stayed queued with nothing to run");
    let polled = f.starter.plans.load(Ordering::SeqCst) - plans;
    assert!(polled <= 1, "planned {polled} times in a minute for a follow-up with nothing to run");
    assert!(f.started().is_empty());
}

/// Finding 1: trigger checks never use the run service's plan cache, so a
/// busy trigger cannot evict the plan the user is reviewing.
#[test]
fn trigger_checks_never_evict_a_plan_the_user_is_reviewing() {
    let f = fixture();
    let id = f.save(definition(false, false));
    // The user reviews a workflow that reads other data.
    std::fs::write(f.root.join("data/other.csv"), "z\n1\n").unwrap();
    let mut unrelated = definition(false, false);
    unrelated["inputs"][1]["source"]["path"] = json!("data/other.csv");
    let other = f.save(unrelated);
    f.open_ready();
    f.enable(&id, watch(&["data"]));
    let reviewing = f.runs.plan(&other, None).unwrap();
    assert!(reviewing.blockers.is_empty(), "{:?}", reviewing.blockers);
    // Other workflows hold every run slot: the trigger waits (queued).
    f.starter.cap.store(true, Ordering::SeqCst);
    f.save_file("data/obs.csv", "x\nA\n");
    // More checks than the run service keeps plans (32), over 45 minutes.
    let plans = f.starter.plans.load(Ordering::SeqCst);
    for _ in 0..45 {
        f.advance(MINUTE);
    }
    let checks = f.starter.plans.load(Ordering::SeqCst) - plans;
    assert!(checks > 32, "only {checks} checks; the test needs more than the plan cache holds");
    assert!(f.started().is_empty());
    let started = f.runs.start(&crate::model::StartWorkflowRunRequest {
        plan: reviewing.plan.clone(),
        approval_hash: reviewing.approval_hash.clone(),
    });
    assert!(started.is_ok(), "the user's reviewed plan was evicted: {:?}", started.err());
    assert!(f.runs.wait_idle(Duration::from_secs(20)));
}

/// Finding 1: while a run of the workflow (or the run cap) blocks it, the
/// queued follow-up checks with a bounded back-off, not every second.
#[test]
fn a_blocked_follow_up_backs_off() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    // Its own workflow's run is active.
    f.starter.active.store(true, Ordering::SeqCst);
    f.save_file("data/obs.csv", "x\nA\n");
    f.advance(3 * SECOND);
    assert!(f.trigger(&t.id).queued);
    let checks = f.starter.busy_checks.load(Ordering::SeqCst);
    for _ in 0..300 {
        f.advance(SECOND);
    }
    let polled = f.starter.busy_checks.load(Ordering::SeqCst) - checks;
    assert!(polled <= 15, "checked {polled} times in 5 minutes");
    // The run cap held by other workflows backs off the same way.
    f.starter.active.store(false, Ordering::SeqCst);
    f.starter.cap.store(true, Ordering::SeqCst);
    let plans = f.starter.plans.load(Ordering::SeqCst);
    for _ in 0..300 {
        f.advance(SECOND);
    }
    let planned = f.starter.plans.load(Ordering::SeqCst) - plans;
    assert!(planned <= 15, "planned {planned} times in 5 minutes while the run cap was full");
    assert!(f.trigger(&t.id).queued);
    // Once free, the follow-up runs within the longest back-off.
    f.starter.cap.store(false, Ordering::SeqCst);
    f.advance(61 * SECOND);
    assert_eq!(f.started().len(), 1);
    assert!(!f.trigger(&t.id).queued);
}

/// Finding 2: settings refused at enable leave the review usable.
#[test]
fn refused_settings_do_not_use_up_the_review() {
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let review = f.review(&id, watch(&["data"]));
    let bad = WorkflowTriggerSettings { max_runs_per_hour: 12, max_runs_per_day: 5, ..settings() };
    assert_eq!(f.triggers.enable(&f.request(&review, bad)).unwrap_err().code, "trigger.invalidSettings");
    let enabled = f.triggers.enable(&f.request(&review, settings()));
    assert!(enabled.is_ok(), "the corrected settings were refused: {:?}", enabled.err());
}

/// Finding 3: trust revocation suspends every trigger of the folder, so
/// every standing plan says so (not only native ones).
#[test]
fn every_standing_plan_says_trust_revocation_suspends() {
    let f = fixture();
    let restricted = f.save(definition(false, false));
    let native = f.save(definition(true, false));
    f.open_ready();
    for id in [restricted, native] {
        let review = f.review(&id, watch(&["data"]));
        assert!(review.suspends_on.iter().any(|s| s.contains("Trust being revoked")), "{:?}", review.suspends_on);
    }
}

/// Finding 4: a store that cannot be read is never overwritten; once it can
/// be read again its triggers are kept.
#[cfg(windows)]
#[test]
fn an_unreadable_store_is_never_overwritten() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = fixture();
    let first = f.save(definition(false, false));
    let second = f.save(definition(false, false));
    f.open_ready();
    let t1 = f.enable(&first, watch(&["data"]));
    let original = std::fs::read(&f.store).unwrap();
    // Another process holds the store exclusively while the app starts.
    let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&f.store).unwrap();
    let restarted = TriggerService::new(TriggerConfig {
        store: Some(f.store.clone()),
        starter: f.starter.clone(),
        clock: f.clock.clone(),
    });
    let request = WorkflowTriggerPlanRequest { workflow: second.clone(), kind: watch(&["data"]) };
    let review = restarted.plan(&request).unwrap();
    let refused = restarted.enable(&f.request(&review, settings())).unwrap_err();
    assert_eq!(refused.code, "trigger.storeUnavailable", "{}", refused.message);
    assert_eq!(restarted.list(None).unwrap_err().code, "trigger.storeUnavailable", "shown, not an empty list");
    drop(lock);
    assert_eq!(std::fs::read(&f.store).unwrap(), original, "the unreadable store was changed");
    // Readable again: enabling keeps the trigger that was already there.
    let review = restarted.plan(&request).unwrap();
    let t2 = restarted.enable(&f.request(&review, settings())).unwrap();
    let text = std::fs::read_to_string(&f.store).unwrap();
    assert!(text.contains(t1.id.as_str()), "the earlier trigger was overwritten");
    assert!(text.contains(t2.id.as_str()));
    assert_eq!(restarted.list(None).unwrap().len(), 2);
}

/// Finding 4: a change that could not be saved is not applied, and saving
/// problems of background changes are visible.
#[cfg(windows)]
#[test]
fn unsaved_changes_are_rolled_back_or_surfaced() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = fixture();
    let id = f.save(definition(false, false));
    f.open_ready();
    let t = f.enable(&id, watch(&["data"]));
    let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&f.store).unwrap();
    let refused = f.triggers.disable(&t.id).unwrap_err();
    assert_eq!(refused.code, "trigger.storeFailed");
    assert_eq!(f.trigger(&t.id).state, TriggerState::Active, "a disable that was not saved took effect");
    // A suspension cannot wait for the store: it applies, and the problem shows.
    f.save_file("experiment.py", "print('changed')\n");
    f.advance(3 * SECOND);
    let view = f.trigger(&t.id);
    assert!(matches!(view.state, TriggerState::Suspended { .. }));
    assert!(view.store_problem.is_some(), "the failed save was not surfaced");
    drop(lock);
    f.triggers.disable(&t.id).unwrap();
    assert_eq!(f.trigger(&t.id).store_problem, None, "a later save clears the problem");
}
