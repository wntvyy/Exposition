//! Scale measurements of workflow runs (run on purpose, never in a
//! validation plan):
//!
//! `cargo test -p exposition-workflow -p exposition --test perf_runs -- --ignored --nocapture --test-threads=1`
//!
//! The real run service, graph store, batch adapter, and an on-disk run
//! store, with a fake batch runner standing in for the research sandbox (it
//! reports a realistic environment and log). Each test prints `PERF {...}`
//! lines. Scale: `EXPOSITION_PERF_SCALE` = small (default) | large | stress.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::presets::{NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use exposition_graph::workflow::{
    AuthorityCeiling, CodeLanguage, CompositeRef, OutcomeKind, ParamValue, PortSource, StepOperation, StepPayload,
    StepPort, StepRepeat, WORKFLOW_SCHEMA_V2, WorkflowConnection, WorkflowDefinition, WorkflowExecutionOptions,
    WorkflowOutcome, WorkflowStep,
};
use exposition_graph::{
    GraphConfig, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowId,
    WorkflowRunId,
};
use exposition_research::ResearchError;
use exposition_research::batch::{
    BatchControl, BatchLanguage, BatchMode, BatchPortOutput, BatchRequest, BatchResult, BatchRunner,
    environment_keys as keys,
};
use exposition_workflow::adapters::batch::BatchAdapter;
use exposition_workflow::adapters::{Adapters, runner_ids, sha256_hex};
use exposition_workflow::store::RunStore;
use exposition_workflow::{
    StartWorkflowRunRequest, WorkflowConcurrencyLimits, WorkflowHost, WorkflowRun, WorkflowRunConfig,
    WorkflowRunError, WorkflowRunService, WorkflowRunState,
};

const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn scale() -> String {
    std::env::var("EXPOSITION_PERF_SCALE").unwrap_or_else(|_| "small".into())
}

fn pick<T: Copy>(small: T, large: T, stress: T) -> T {
    match scale().as_str() {
        "large" => large,
        "stress" => stress,
        _ => small,
    }
}

fn perf(bench: &str, scale: serde_json::Value, samples_ms: &[f64], counters: serde_json::Value) {
    println!(
        "PERF {}",
        serde_json::json!({"bench": bench, "scale": scale, "samples_ms": samples_ms, "counters": counters})
    );
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// `n` timed samples of `f` after one warm-up call.
fn sample<T>(n: usize, mut f: impl FnMut() -> T) -> Vec<f64> {
    f();
    (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            ms(t)
        })
        .collect()
}

struct Host {
    root: PathBuf,
    graph: Arc<GraphService>,
    root_reads: AtomicUsize,
}

impl WorkflowHost for Host {
    fn project_root(&self) -> Option<PathBuf> {
        self.root_reads.fetch_add(1, Ordering::Relaxed);
        Some(self.root.clone())
    }
    fn trusted(&self, _root: &Path) -> bool {
        true
    }
    fn restricted(&self) -> (Option<String>, Option<String>) {
        (Some("docker".into()), Some("python:3.13-slim".into()))
    }
    fn restricted_image_id(&self) -> Option<String> {
        Some(IMAGE.into())
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

/// Writes one `dataset` file per call and reports an environment with
/// `packages` packages and a log of `log_chars` characters. While `hold` is
/// set, a batch waits (until released or canceled).
struct FakeBatch {
    packages: usize,
    log_chars: usize,
    hold: AtomicBool,
    running: AtomicUsize,
    counter: AtomicUsize,
}

impl BatchRunner for FakeBatch {
    fn availability(&self, _root: &Path, _mode: BatchMode) -> Result<(), String> {
        Ok(())
    }
    fn run(
        &self,
        root: &Path,
        request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError> {
        let run = format!("run-{}", self.counter.fetch_add(1, Ordering::SeqCst));
        (control.on_dispatched)(&run);
        self.running.fetch_add(1, Ordering::SeqCst);
        while self.hold.load(Ordering::SeqCst) && !(control.canceled)() {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.running.fetch_sub(1, Ordering::SeqCst);
        let content = format!("x\n{run}\n");
        let rel = format!(".exposition/artifacts/{run}/dataset.csv");
        std::fs::create_dir_all(root.join(format!(".exposition/artifacts/{run}"))).unwrap();
        std::fs::write(root.join(&rel), &content).unwrap();
        let mut environment: Vec<(String, String)> = [
            (keys::IMAGE, "python:3.13-slim"),
            (keys::IMAGE_ID, IMAGE),
            (keys::OS, "linux"),
            (keys::ARCH, "x86_64"),
            (keys::IMPLEMENTATION, "cpython"),
            (keys::PYTHON_VERSION, "3.13.9"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        for i in 0..self.packages {
            environment.push((keys::PACKAGE.into(), format!("scientific-package-{i:04}=={}.{}.{}", i % 7, i % 13, i % 5)));
        }
        if let Some(seed) = request.seed {
            environment.push((keys::SEED.into(), seed.to_string()));
        }
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
                    size_bytes: content.len() as u64,
                    content_hash: format!("sha256:{}", sha256_hex(content.as_bytes())),
                },
            }],
            extra: Vec::new(),
            rejected: Vec::new(),
            log: "iteration 0001 loss=0.123456 accuracy=0.9876\n".repeat(self.log_chars / 44 + 1),
            environment,
        })
    }
    fn language_availability(&self, _root: &Path, _language: BatchLanguage, _mode: BatchMode) -> Result<(), String> {
        Ok(())
    }
    fn run_language(
        &self,
        root: &Path,
        _language: BatchLanguage,
        request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError> {
        self.run(root, request, control)
    }
}

struct Fixture {
    data: tempfile::TempDir,
    _project: tempfile::TempDir,
    root: PathBuf,
    graph: Arc<GraphService>,
    host: Arc<Host>,
    batch: Arc<FakeBatch>,
    service: WorkflowRunService,
}

fn fixture(packages: usize, log_chars: usize, limits: WorkflowConcurrencyLimits) -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/obs.csv"), "x\n1\n2\n3\n").unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().join("graph") });
    assert!(graph.attach(&root).is_usable());
    let batch = Arc::new(FakeBatch {
        packages,
        log_chars,
        hold: AtomicBool::new(false),
        running: AtomicUsize::new(0),
        counter: AtomicUsize::new(1),
    });
    let runner: Arc<dyn BatchRunner> = batch.clone();
    let mut adapters = Adapters::new();
    adapters.insert(
        runner_ids::RESTRICTED_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::Restricted)),
    );
    adapters.insert(
        runner_ids::NATIVE_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::TrustedNative)),
    );
    exposition_workflow::adapters::rj::install(&mut adapters, Some(runner));
    let host = Arc::new(Host { root: root.clone(), graph: graph.clone(), root_reads: AtomicUsize::new(0) });
    let service = WorkflowRunService::new_with_limits(
        WorkflowRunConfig {
            database: Some(data.path().join("workflow-runs.sqlite3")),
            host: host.clone(),
            adapters,
            lean: None,
            research: None,
        },
        limits,
    )
    .unwrap();
    Fixture { data, _project: project, root, graph, host, batch, service }
}

impl Fixture {
    fn save(&self, definition: WorkflowDefinition) -> WorkflowId {
        let saved = self
            .graph
            .save_workflow(
                &Origin::User,
                &SaveWorkflowRequest { id: None, expected_revision: None, definition },
                &ValidationContext::default(),
            )
            .unwrap();
        assert_eq!(saved.validation.errors, 0, "{:?}", saved.validation.diagnostics);
        saved.id
    }

    fn start(&self, id: &WorkflowId) -> WorkflowRun {
        let plan = self.service.plan(id, None).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        self.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap()
    }

    fn database_bytes(&self) -> u64 {
        let size = |name: &str| std::fs::metadata(self.data.path().join(name)).map(|m| m.len()).unwrap_or(0);
        size("workflow-runs.sqlite3") + size("workflow-runs.sqlite3-wal")
    }
}

fn python(id: &str, source_bytes: usize) -> WorkflowStep {
    let line = "result = compute(values)  # a reviewed line of step code\n";
    WorkflowStep {
        id: id.into(),
        label: format!("Python {id}"),
        operation: StepOperation { id: "research.pythonBatch".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Code {
            language: CodeLanguage::Python,
            source: format!("# step {id}\n{}", line.repeat(source_bytes / line.len() + 1)),
        }),
        composite: None,
        condition: None,
        repeat: None,
    }
}

fn data_to(from: PortSource, step: &str) -> WorkflowConnection {
    WorkflowConnection::Data { from, to: StepPort { step: step.into(), port: "data".into() } }
}

fn out(step: &str) -> PortSource {
    PortSource::Step { step: step.into(), port: "dataset".into() }
}

fn input() -> PortSource {
    PortSource::Input { input: "data".into() }
}

fn v2(steps: Vec<WorkflowStep>, connections: Vec<WorkflowConnection>, outcome: (&str, &str)) -> WorkflowDefinition {
    let mut def = draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        data: Some(WorkspacePath::parse("data/obs.csv").unwrap()),
        ..Default::default()
    })
    .unwrap();
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.execution = Some(WorkflowExecutionOptions { max_parallel: 4 });
    def.ceiling = AuthorityCeiling::ALL;
    def.steps = steps;
    def.connections = connections;
    def.outcomes = vec![WorkflowOutcome {
        id: "o-last".into(),
        kind: OutcomeKind::TransformedDataset,
        label: "Final dataset".into(),
        from: StepPort { step: outcome.0.into(), port: outcome.1.into() },
        optional: false,
    }];
    def
}

/// `sweep` (forEach over `values` time limits) → `join`.
fn sweep_and_join(values: usize, source_bytes: usize) -> WorkflowDefinition {
    let mut sweep = python("sweep", source_bytes);
    sweep.repeat = Some(StepRepeat::ForEach {
        parameter: "timeoutSeconds".into(),
        values: (0..values).map(|i| ParamValue::Integer(10 + i as i64)).collect(),
    });
    v2(
        vec![sweep, python("join", source_bytes)],
        vec![data_to(input(), "sweep"), data_to(out("sweep"), "join")],
        ("join", "dataset"),
    )
}

/// A chain of `n` steps reading the workflow's `data` input.
fn chain(prefix: &str, n: usize, source_bytes: usize) -> (Vec<WorkflowStep>, Vec<WorkflowConnection>, String) {
    let mut steps = Vec::new();
    let mut connections = Vec::new();
    for i in 0..n {
        let id = format!("{prefix}{i}");
        connections.push(data_to(if i == 0 { input() } else { out(&format!("{prefix}{}", i - 1)) }, &id));
        steps.push(python(&id, source_bytes));
    }
    (steps, connections, format!("{prefix}{}", n - 1))
}

/// Per-state-change durable write cost of one run as attempts accumulate:
/// a forEach of 20 instances and a join, each attempt recording an
/// environment with a realistic package list and a log.
#[test]
#[ignore = "perf"]
fn durable_writes_of_a_run() {
    let packages = pick(150usize, 400, 1000);
    let log_chars = pick(2_000usize, 16_000, 64_000);
    let source_bytes = pick(2_000usize, 20_000, 200_000);
    let values = 20;
    let f = fixture(packages, log_chars, WorkflowConcurrencyLimits::default());
    let id = f.save(sweep_and_join(values, source_bytes));
    let mut events = f.service.subscribe();
    let before = f.service.store_counters();
    let t = Instant::now();
    let run = f.start(&id);
    assert!(f.service.wait_idle(Duration::from_secs(600)), "the run did not finish");
    let run_ms = ms(t);
    let after = f.service.store_counters();
    let mut count = 0u64;
    loop {
        match events.try_recv() {
            Ok(_) => count += 1,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => count += n,
            Err(_) => break,
        }
    }
    let finished = f.service.get(&run.id).unwrap();
    assert_eq!(finished.state, WorkflowRunState::Succeeded, "{:?}", finished.message);
    let final_body = serde_json::to_string(&finished).unwrap().len();
    let plan_bytes = serde_json::to_string(&finished.plan).unwrap().len();
    let scale = serde_json::json!({"instances": values, "packages": packages, "log_chars": log_chars, "source_bytes": source_bytes});
    let puts = after.puts - before.puts;
    perf(
        "workflow.run.durable_writes",
        scale.clone(),
        &[run_ms],
        serde_json::json!({
            "record_writes": puts,
            "record_bytes_written": after.put_bytes - before.put_bytes,
            "mean_bytes_per_write": (after.put_bytes - before.put_bytes) / puts.max(1),
            "final_record_bytes": final_body,
            "plan_bytes_in_record": plan_bytes,
            "run_events": count,
            "database_and_wal_bytes": f.database_bytes(),
        }),
    );
    // One whole-record write and one read of it, at the final size.
    let dir = tempfile::tempdir().unwrap();
    let store = RunStore::open(Some(&dir.path().join("runs.sqlite3"))).unwrap();
    let put = sample(20, || store.put("project", &finished).unwrap());
    perf("workflow.store.put", scale.clone(), &put, serde_json::json!({"record_bytes": final_body}));
    // The same durable transaction with almost nothing in it (one small
    // column): what a write costs regardless of the record's size.
    let floor = sample(20, || store.set_deleted(&finished.id, None).unwrap());
    perf("workflow.store.durable_write_floor", scale.clone(), &floor, serde_json::json!({}));
    let encode = sample(20, || serde_json::to_string(&finished).unwrap());
    perf("workflow.store.encode_record", scale.clone(), &encode, serde_json::json!({"record_bytes": final_body}));
    let get = sample(20, || store.get(&finished.id).unwrap().unwrap());
    perf("workflow.store.get", scale.clone(), &get, serde_json::json!({"record_bytes": final_body}));
    let service_get = sample(20, || f.service.get(&finished.id).unwrap());
    perf("workflow.service.get", scale, &service_get, serde_json::json!({"record_bytes": final_body}));
}

/// Run history: the cost of one listing (what every run event makes each
/// mounted history do) with 50 / 500 / 2,000 stored runs.
#[test]
#[ignore = "perf"]
fn run_history_listing() {
    let packages = pick(150usize, 400, 1000);
    let log_chars = pick(2_000usize, 16_000, 64_000);
    let source_bytes = pick(2_000usize, 20_000, 200_000);
    let f = fixture(packages, log_chars, WorkflowConcurrencyLimits::default());
    let id = f.save(sweep_and_join(20, source_bytes));
    let run = f.start(&id);
    assert!(f.service.wait_idle(Duration::from_secs(600)));
    let template = f.service.get(&run.id).unwrap();
    assert_eq!(template.state, WorkflowRunState::Succeeded);
    let record_bytes = serde_json::to_string(&template).unwrap().len();
    let sizes: &[usize] = match scale().as_str() {
        "small" => &[50, 500],
        _ => &[50, 500, 2000],
    };
    for &runs in sizes {
        let dir = tempfile::tempdir().unwrap();
        let store = RunStore::open(Some(&dir.path().join("runs.sqlite3"))).unwrap();
        let fill = Instant::now();
        for i in 0..runs {
            let mut record = template.clone();
            record.id = WorkflowRunId::mint();
            record.started_at_ms = template.started_at_ms + i as u64;
            store.put("project", &record).unwrap();
        }
        let fill_ms = ms(fill);
        for limit in [100usize, 200] {
            let before = store.counters();
            let mut listed = 0;
            let samples = sample(10, || listed = store.list("project", None, limit).unwrap().len());
            let after = store.counters();
            perf(
                "workflow.store.list",
                serde_json::json!({"stored_runs": runs, "limit": limit, "record_bytes": record_bytes}),
                &samples,
                serde_json::json!({
                    "listed": listed,
                    "records_decoded_per_list": (after.bodies_decoded - before.bodies_decoded) / 11,
                    "bytes_decoded_per_list": (after.decoded_bytes - before.decoded_bytes) / 11,
                }),
            );
        }
        perf(
            "workflow.store.fill",
            serde_json::json!({"stored_runs": runs, "record_bytes": record_bytes}),
            &[fill_ms],
            serde_json::json!({"database_bytes": std::fs::metadata(dir.path().join("runs.sqlite3")).map(|m| m.len()).unwrap_or(0)}),
        );
    }
}

/// Planning (validation, expansion, input hashing, approval hash) of large
/// valid plans: three composites of chained steps, a forEach fan-out, a
/// fan-in, and a tail chain, close to the 100-step expansion limit at large
/// scale.
#[test]
#[ignore = "perf"]
fn planning_large_plans() {
    let per_composite = pick(4usize, 25, 25);
    let tail = pick(4usize, 20, 20);
    let source_bytes = pick(2_000usize, 20_000, 200_000);
    let f = fixture(10, 100, WorkflowConcurrencyLimits::default());
    let (steps, connections, last) = chain("s", per_composite, source_bytes);
    let child = f.save(v2(steps, connections, (&last, "dataset")));
    let composite = |id: &str| WorkflowStep {
        id: id.into(),
        label: format!("Composite {id}"),
        operation: StepOperation { id: "workflow.composite".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
        composite: Some(CompositeRef { workflow: child.clone(), revision: 1 }),
        condition: None,
        repeat: None,
    };
    let composite_out = |id: &str| PortSource::Step { step: id.into(), port: "o-last".into() };
    let mut steps = vec![composite("c0"), composite("c1"), composite("c2")];
    let mut connections = vec![
        data_to(input(), "c0"),
        data_to(composite_out("c0"), "c1"),
        data_to(composite_out("c1"), "c2"),
        data_to(composite_out("c2"), "sweep"),
        data_to(out("sweep"), "join"),
    ];
    let mut sweep = python("sweep", source_bytes);
    sweep.repeat = Some(StepRepeat::ForEach {
        parameter: "timeoutSeconds".into(),
        values: (0..20).map(|i| ParamValue::Integer(10 + i)).collect(),
    });
    steps.push(sweep);
    steps.push(python("join", source_bytes));
    let mut previous = "join".to_owned();
    for i in 0..tail {
        let id = format!("t{i}");
        connections.push(data_to(out(&previous), &id));
        steps.push(python(&id, source_bytes));
        previous = id;
    }
    let authored = steps.len();
    let parent = f.save(v2(steps, connections, (&previous, "dataset")));
    let first = f.service.plan(&parent, None).unwrap();
    assert!(first.blockers.is_empty(), "{:?}", first.blockers);
    let expanded = first.steps.len();
    let plan_json = serde_json::to_string(&first).unwrap().len();
    let reads_before = f.host.root_reads.load(Ordering::Relaxed);
    let samples = sample(10, || f.service.plan(&parent, None).unwrap());
    let scale = serde_json::json!({"authored_steps": authored, "expanded_steps": expanded, "source_bytes": source_bytes, "for_each": 20});
    perf(
        "workflow.plan.build",
        scale.clone(),
        &samples,
        serde_json::json!({"plan_json_bytes": plan_json, "project_root_reads_per_plan": (f.host.root_reads.load(Ordering::Relaxed) - reads_before) / 11}),
    );
    // Starting recomputes the plan and compares approval hashes; the fake
    // batches return at once, so this is planning plus dispatch overhead.
    let before = f.service.store_counters();
    let wake_before = f.service.scheduler_wakeups();
    let t = Instant::now();
    let run = f.service.start(&StartWorkflowRunRequest { plan: first.plan, approval_hash: first.approval_hash }).unwrap();
    let start_ms = ms(t);
    assert!(f.service.wait_idle(Duration::from_secs(600)));
    let total_ms = ms(t);
    let finished = f.service.get(&run.id).unwrap();
    assert_eq!(finished.state, WorkflowRunState::Succeeded, "{:?}", finished.message);
    let after = f.service.store_counters();
    perf(
        "workflow.run.large_plan",
        scale,
        &[total_ms],
        serde_json::json!({
            "start_ms": start_ms,
            "record_writes": after.puts - before.puts,
            "record_bytes_written": after.put_bytes - before.put_bytes,
            "final_record_bytes": serde_json::to_string(&finished).unwrap().len(),
            "scheduler_wakeups": f.service.scheduler_wakeups() - wake_before,
        }),
    );
}

/// Scheduler wakeups with no work to do: one run blocked on a long step,
/// and a second run waiting for the only native slot.
#[test]
#[ignore = "perf"]
fn scheduler_idle_wakeups() {
    let window = Duration::from_secs(pick(3, 10, 30));
    let limits = WorkflowConcurrencyLimits { restricted: 1, ..Default::default() };
    let f = fixture(10, 100, limits);
    // No run: nothing wakes.
    let w0 = f.service.scheduler_wakeups();
    std::thread::sleep(Duration::from_millis(500));
    let none = f.service.scheduler_wakeups() - w0;
    let (steps, connections, last) = chain("a", 40, 500);
    let long = f.save(v2(steps, connections, (&last, "dataset")));
    let (steps, connections, last) = chain("b", 40, 500);
    let second = f.save(v2(steps, connections, (&last, "dataset")));
    f.batch.hold.store(true, Ordering::SeqCst);
    let first_run = f.start(&long);
    while f.batch.running.load(Ordering::SeqCst) == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    let w1 = f.service.scheduler_wakeups();
    std::thread::sleep(window);
    let one_long_step = f.service.scheduler_wakeups() - w1;
    let second_run = f.start(&second);
    std::thread::sleep(Duration::from_millis(200));
    let w2 = f.service.scheduler_wakeups();
    let reads = f.host.root_reads.load(Ordering::Relaxed);
    std::thread::sleep(window);
    let with_waiting_run = f.service.scheduler_wakeups() - w2;
    let waiting_dispatch_attempts = f.host.root_reads.load(Ordering::Relaxed) - reads;
    // Cancellation latency of the blocked run and of the waiting run.
    let t = Instant::now();
    f.service.cancel(&second_run.id).unwrap();
    let cancel_waiting_ms = ms(t);
    let t = Instant::now();
    f.service.cancel(&first_run.id).unwrap();
    let cancel_running_ms = ms(t);
    f.batch.hold.store(false, Ordering::SeqCst);
    assert!(f.service.wait_idle(Duration::from_secs(60)));
    let seconds = window.as_secs_f64();
    perf(
        "workflow.scheduler.idle",
        serde_json::json!({"plan_steps": 40, "window_ms": window.as_millis() as u64}),
        &[],
        serde_json::json!({
            "wakeups_no_run_500ms": none,
            "wakeups_per_second_one_long_step": (one_long_step as f64 / seconds).round(),
            "wakeups_per_second_long_step_plus_waiting_run": (with_waiting_run as f64 / seconds).round(),
            "waiting_dispatch_attempts_per_second": (waiting_dispatch_attempts as f64 / seconds).round(),
            "cancel_waiting_run_ms": cancel_waiting_ms,
            "cancel_running_run_ms": cancel_running_ms,
        }),
    );
    let _ = &f.root;
}
