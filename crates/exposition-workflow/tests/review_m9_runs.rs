//! R-m9 (independent review of M9): run-level probes over the real run
//! service, the real graph store, and the real batch adapter, with a fake
//! batch runner standing in for the research sandbox (no Docker needed).
//!
//! Product-failing reproductions are `#[ignore = "R-m9 Fn: ..."]` so the
//! suite stays green; run them with `-- --ignored`. The other tests are
//! behaviour the review exercised and found to hold.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::presets::{NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use exposition_graph::workflow::{
    AuthorityCeiling, CodeLanguage, CompareOp, CompositeRef, ConditionTest, ConditionValue, InputSource, InputVersion,
    OutcomeKind, ParamValue, PortSource, StepCondition, StepOperation, StepPayload, StepPort, StepRepeat,
    WORKFLOW_SCHEMA_V2, WorkflowConnection, WorkflowDefinition, WorkflowExecutionOptions, WorkflowInput,
    WorkflowOutcome, WorkflowStep,
};
use exposition_graph::{
    GraphConfig, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowId,
};
use exposition_research::ResearchError;
use exposition_research::batch::{
    BatchControl, BatchLanguage, BatchMode, BatchPortOutput, BatchRequest, BatchResult, BatchRunner,
    environment_keys as keys,
};
use exposition_workflow::adapters::batch::BatchAdapter;
use exposition_workflow::adapters::{Adapters, runner_ids, sha256_hex};
use exposition_workflow::{
    StartWorkflowRunRequest, WorkflowAttemptState, WorkflowHost, WorkflowRun, WorkflowRunConfig, WorkflowRunError,
    WorkflowRunService, WorkflowRunState, WorkflowStepState, manifest, reproduce,
};

const IMAGE_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IMAGE_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Host {
    root: PathBuf,
    graph: Arc<GraphService>,
}

impl WorkflowHost for Host {
    fn project_root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }
    fn trusted(&self, _root: &Path) -> bool {
        true
    }
    fn restricted(&self) -> (Option<String>, Option<String>) {
        (Some("docker".into()), Some("python:3.13-slim".into()))
    }
    fn restricted_image_id(&self) -> Option<String> {
        Some(IMAGE_A.into())
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

/// What a fake batch writes for one step: (file name, content).
type Writes = HashMap<String, (String, String)>;
/// A side effect a step's batch performs while it runs (a native process can
/// write anywhere in the project).
type SideEffect = Box<dyn Fn(&Path, &HashMap<String, String>) + Send + Sync>;

/// A batch runner that writes one `dataset` file per call, reports the
/// environment of image A, and can run a side effect for a step.
struct FakeBatch {
    writes: Mutex<Writes>,
    side_effects: Mutex<HashMap<String, SideEffect>>,
    /// step id → project-relative path of the dataset it produced last.
    produced: Mutex<HashMap<String, String>>,
    /// What `current_environment` reports (the environment a step would get now).
    current: Mutex<Vec<(String, String)>>,
    counter: AtomicUsize,
}

fn env_pairs(id: &str) -> Vec<(String, String)> {
    [
        (keys::IMAGE, "python:3.13-slim"),
        (keys::IMAGE_ID, id),
        (keys::OS, "linux"),
        (keys::ARCH, "x86_64"),
        (keys::IMPLEMENTATION, "cpython"),
        (keys::PYTHON_VERSION, "3.13.9"),
        (keys::PACKAGE, "numpy==2.3.1"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect()
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
        if let Some(effect) = self.side_effects.lock().unwrap().get(&request.owner.step) {
            effect(root, &self.produced.lock().unwrap());
        }
        let (name, content) = self
            .writes
            .lock()
            .unwrap()
            .get(&request.owner.step)
            .cloned()
            .unwrap_or_else(|| ("dataset.csv".into(), format!("x\n{run}\n")));
        let rel = format!(".exposition/artifacts/{run}/{name}");
        std::fs::create_dir_all(root.join(format!(".exposition/artifacts/{run}"))).unwrap();
        std::fs::write(root.join(&rel), &content).unwrap();
        self.produced.lock().unwrap().insert(request.owner.step.clone(), rel.clone());
        let mut environment = env_pairs(IMAGE_A);
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
                    media_type: "application/json".into(),
                    size_bytes: content.len() as u64,
                    content_hash: format!("sha256:{}", sha256_hex(content.as_bytes())),
                },
            }],
            extra: Vec::new(),
            rejected: Vec::new(),
            log: "ok".into(),
            environment,
        })
    }
    fn current_environment(&self, _root: &Path, _mode: BatchMode) -> Vec<(String, String)> {
        self.current.lock().unwrap().clone()
    }
    // Lane F: R and Julia batches go through the same fake (native only).
    fn language_availability(&self, _root: &Path, language: BatchLanguage, mode: BatchMode) -> Result<(), String> {
        match (language, mode) {
            (BatchLanguage::Python, _) | (_, BatchMode::TrustedNative) => Ok(()),
            (_, BatchMode::Restricted) => Err("no R/Julia image in this fake".into()),
        }
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
    _data: tempfile::TempDir,
    _project: tempfile::TempDir,
    root: PathBuf,
    graph: Arc<GraphService>,
    batch: Arc<FakeBatch>,
    service: WorkflowRunService,
}

fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/obs.csv"), "x\n1\n2\n3\n").unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().join("graph") });
    assert!(graph.attach(&root).is_usable());
    let batch = Arc::new(FakeBatch {
        writes: Mutex::new(HashMap::new()),
        side_effects: Mutex::new(HashMap::new()),
        produced: Mutex::new(HashMap::new()),
        current: Mutex::new(env_pairs(IMAGE_A)),
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
    let host = Arc::new(Host { root: root.clone(), graph: graph.clone() });
    let service = WorkflowRunService::new(WorkflowRunConfig {
        database: Some(data.path().join("workflow-runs.sqlite3")),
        host,
        adapters,
        lean: None,
        research: None,
    })
    .unwrap();
    Fixture { _data: data, _project: project, root, graph, batch, service }
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

    fn run_to_end(&self, id: &WorkflowId) -> WorkflowRun {
        let plan = self.service.plan(id, None).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let run = self
            .service
            .start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash })
            .unwrap();
        assert!(self.service.wait_idle(Duration::from_secs(30)), "run did not finish");
        self.service.get(&run.id).unwrap()
    }
}

fn python(id: &str, native: bool) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Python {id}"),
        operation: StepOperation {
            id: if native { "research.nativePython".into() } else { "research.pythonBatch".into() },
            version: 1,
        },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Code { language: CodeLanguage::Python, source: format!("# step {id}\n") }),
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

fn outcome(step: &str) -> WorkflowOutcome {
    WorkflowOutcome {
        id: format!("o-{step}"),
        kind: OutcomeKind::TransformedDataset,
        label: format!("Dataset of {step}"),
        from: StepPort { step: step.into(), port: "dataset".into() },
        optional: false,
    }
}

/// A v2 definition from the numerical preset (its `data` input) with `steps`.
fn v2(steps: Vec<WorkflowStep>, connections: Vec<WorkflowConnection>, outcome_step: &str) -> WorkflowDefinition {
    let mut def = draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        data: Some(WorkspacePath::parse("data/obs.csv").unwrap()),
        ..Default::default()
    })
    .unwrap();
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.execution = Some(WorkflowExecutionOptions { max_parallel: 2 });
    def.ceiling = AuthorityCeiling::ALL;
    def.steps = steps;
    def.connections = connections;
    def.outcomes = vec![outcome(outcome_step)];
    def
}

/// `sweep` (forEach over two time limits) → `join` (collects both datasets).
fn sweep_and_join(f: &Fixture) -> WorkflowRun {
    let mut sweep = python("sweep", false);
    sweep.repeat = Some(StepRepeat::ForEach {
        parameter: "timeoutSeconds".into(),
        values: vec![ParamValue::Integer(10), ParamValue::Integer(20)],
    });
    let def =
        v2(vec![sweep, python("join", false)], vec![data_to(input(), "sweep"), data_to(out("sweep"), "join")], "join");
    let id = f.save(def);
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    // The run record itself has everything: two instance attempts, each with
    // its validated dataset and the environment it ran with.
    let sweep = run.steps.iter().find(|s| s.step == "sweep").unwrap();
    assert!(sweep.attempts.is_empty(), "a repeated step's attempts live in its instances");
    assert_eq!(sweep.instances.len(), 2);
    for instance in &sweep.instances {
        let attempt = &instance.attempts[0];
        assert_eq!(attempt.state, WorkflowAttemptState::Succeeded);
        assert_eq!(attempt.outputs.len(), 1);
        let env = attempt.environment.as_ref().expect("the instance recorded its environment");
        assert_eq!(env.image.as_ref().and_then(|i| i.id.as_deref()), Some(IMAGE_A));
    }
    run
}

// ------------------------------------------------------------ findings ----

/// R-m9 F1 (manifest half): the exported run manifest is built from
/// `step.attempts` only, so a forEach/until step — whose attempts live in
/// `step.instances` — appears with no attempts, no environments, and none of
/// its validated outputs.
#[test]
fn review_m9_f1_manifest_lists_the_instances_of_repeated_steps() {
    let f = fixture();
    let run = sweep_and_join(&f);
    let m = manifest::build_manifest(&run);
    let step = m.steps.iter().find(|s| s.step == "sweep").unwrap();
    let sweep_outputs = m.outputs.iter().filter(|o| o.step == "sweep").count();
    // Fixed shape (lane D): a repeated step's attempts live under its
    // instances (`steps[].instances[].attempts`), each with its index.
    let attempts: Vec<_> = step.attempts.iter().chain(step.instances.iter().flat_map(|i| i.attempts.iter())).collect();
    let environments = attempts.iter().filter(|a| a.environment.is_some()).count();
    eprintln!(
        "manifest step sweep: {} attempts ({} instances), {} environments; manifest outputs of sweep: {}",
        attempts.len(),
        step.instances.len(),
        environments,
        sweep_outputs
    );
    assert_eq!(step.instances.iter().map(|i| i.index).collect::<Vec<_>>(), vec![0, 1], "both instances are listed");
    assert_eq!(attempts.len(), 2, "both instance attempts belong in the manifest");
    assert_eq!(environments, 2, "each instance's environment belongs in the manifest");
    assert_eq!(sweep_outputs, 2, "both validated instance datasets belong in the manifest's outputs");
}

/// R-m9 F1 (reproduction half): environment drift is computed from
/// `step.attempts` only, so a repeated step's image change is never shown,
/// not even as "unrecorded" — while the same change on a plain sibling is.
#[test]
fn review_m9_f1_reproduction_shows_drift_of_repeated_steps() {
    let f = fixture();
    let run = sweep_and_join(&f);
    // The configured image is now a different image.
    *f.batch.current.lock().unwrap() = env_pairs(IMAGE_B);
    let runner: Arc<dyn BatchRunner> = f.batch.clone();
    let sources = reproduce::environment_sources(Some(runner), None);
    let root = f.root.clone();
    let current = move |plan: &exposition_workflow::WorkflowRunPlan, step: &exposition_workflow::WorkflowPlanStep| {
        reproduce::current_environment(&root, &sources, plan, step)
    };
    let drift = reproduce::reproduction_drift(&run, &current);
    let of =
        |s: &str| drift.iter().filter(|d| d.step.as_deref() == Some(s)).map(|d| d.aspect.clone()).collect::<Vec<_>>();
    eprintln!("drift of join: {:?}; drift of sweep: {:?}", of("join"), of("sweep"));
    assert!(of("join").contains(&"image".to_owned()), "control: the plain step shows the image change");
    // The full service path says the same in the plan's notices.
    let plan = f.service.reproduce_plan(&run.id, &current).unwrap();
    let sweep_notice = plan.plan.notices.iter().any(|n| n.contains("Python sweep"));
    eprintln!("reproduction notices: {:#?}", plan.plan.notices);
    assert!(
        of("sweep").iter().any(|a| a == "image" || a == "unrecorded"),
        "the repeated step ran in image A and would now run in image B: its drift must be shown (or said to be unknown)"
    );
    assert!(sweep_notice, "the reproduction notices must mention the repeated step's drift");
}

// --------------------------------------------------------------- held ----

/// Held: a condition reads its source exactly as validated. A native step
/// that rewrites an upstream output after it was validated (a native process
/// can write anywhere in the project) cannot steer a later condition: the
/// condition's read re-hashes the file and the gated step fails clearly
/// (`workflow.outputChanged`) instead of reading the altered value.
#[test]
fn review_m9_held_a_condition_never_reads_an_output_altered_after_validation() {
    let f = fixture();
    f.batch.writes.lock().unwrap().insert("a".into(), ("dataset.json".into(), r#"{"ok": 0}"#.into()));
    // `b` (native) flips a's validated output to {"ok": 1} while it runs.
    f.batch.side_effects.lock().unwrap().insert(
        "b".into(),
        Box::new(|root: &Path, produced: &HashMap<String, String>| {
            let a = produced.get("a").expect("a ran first");
            std::fs::write(root.join(a), r#"{"ok": 1}"#).unwrap();
        }),
    );
    let mut c = python("c", false);
    c.condition = Some(StepCondition {
        source: StepPort { step: "a".into(), port: "dataset".into() },
        test: ConditionTest::JsonCompare {
            pointer: "/ok".into(),
            op: CompareOp::Eq,
            value: ConditionValue::Number("1".into()),
        },
        negate: false,
    });
    let mut def = v2(
        vec![python("a", false), python("b", true), c],
        vec![data_to(input(), "a"), data_to(out("a"), "b"), data_to(out("b"), "c")],
        "c",
    );
    def.execution = Some(WorkflowExecutionOptions { max_parallel: 1 });
    let id = f.save(def);
    let run = f.run_to_end(&id);
    let c = run.steps.iter().find(|s| s.step == "c").unwrap();
    let code = c.attempts.last().and_then(|a| a.code.clone());
    eprintln!("c: {:?} {:?} {:?}", c.state, code, c.attempts.last().and_then(|a| a.message.clone()));
    assert_eq!(c.state, WorkflowStepState::Failed);
    assert_eq!(code.as_deref(), Some("workflow.outputChanged"));
    assert!(c.attempts.iter().all(|a| a.operation.is_none()), "the gated step was never dispatched");
    assert_eq!(run.state, WorkflowRunState::Failed);
}

/// Held: forEach instances never exceed the plan's step budget and every
/// instance is dispatched exactly once (no replay), with a distinct seed per
/// instance once instances get their own seed (recorded either way).
#[test]
fn review_m9_held_for_each_counts_every_instance_once() {
    let f = fixture();
    let run = sweep_and_join(&f);
    assert_eq!(run.budgets.max_steps, 3);
    assert_eq!(run.usage.steps, 3);
    let calls = f.batch.counter.load(Ordering::SeqCst) - 1;
    assert_eq!(calls, 3, "two instances and the join, each dispatched once");
    let seeds: Vec<Option<u64>> = run
        .steps
        .iter()
        .find(|s| s.step == "sweep")
        .unwrap()
        .instances
        .iter()
        .map(|i| i.attempts[0].environment.as_ref().and_then(|e| e.seed))
        .collect();
    eprintln!("instance seeds: {seeds:?} (lane F gives instances their own seed)");
    assert!(seeds.iter().all(Option::is_some), "every instance records the seed it was given");
}

// ------------------------------------------------------- lane F probes ----

/// R-m9 F6 (runtime half): a `.py` script reaching a native R step through a
/// composite passes validation and the plan (no blocker, approvable), and is
/// refused only when the step is dispatched (`workflow.invalidInput`). The
/// refusal holds (nothing runs); the plan the user approved said it would run.
#[test]
fn review_m9_f6_the_plan_refuses_a_python_script_for_an_r_step_inside_a_composite() {
    let f = fixture();
    std::fs::write(
        f.root.join("experiment.py"),
        "print('python, not R')
",
    )
    .unwrap();
    let code_input = |id: &str, path: &str| WorkflowInput {
        id: id.into(),
        label: format!("Code {id}"),
        port_type: exposition_graph::workflow::PortType::CodeSource,
        source: InputSource::Resource { path: WorkspacePath::parse(path).unwrap(), version: InputVersion::AtRunStart },
    };
    let mut fit = python("fit", true);
    fit.operation = StepOperation { id: "research.nativeR".into(), version: 1 };
    fit.payload = None;
    let mut child = v2(
        vec![fit],
        vec![WorkflowConnection::Data {
            from: PortSource::Input { input: "script".into() },
            to: StepPort { step: "fit".into(), port: "script".into() },
        }],
        "fit",
    );
    child.inputs = vec![code_input("script", "analysis.R")];
    child.execution = None;
    child.schema = exposition_graph::workflow::WORKFLOW_SCHEMA.into();
    let child = f.save(child);
    let composite = WorkflowStep {
        id: "c".into(),
        label: "Run the R analysis".into(),
        operation: StepOperation { id: "workflow.composite".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
        composite: Some(CompositeRef { workflow: child, revision: 1 }),
        condition: None,
        repeat: None,
    };
    let mut parent = v2(
        vec![composite],
        vec![WorkflowConnection::Data {
            from: PortSource::Input { input: "code".into() },
            to: StepPort { step: "c".into(), port: "script".into() },
        }],
        "c",
    );
    parent.inputs = vec![code_input("code", "experiment.py")];
    parent.outcomes[0].from = StepPort { step: "c".into(), port: "o-fit".into() };
    // Saved without asserting it is well-formed (a fixed validator refuses it).
    let id = f
        .graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: None, expected_revision: None, definition: parent },
            &ValidationContext::default(),
        )
        .unwrap()
        .id;
    let plan = f.service.plan(&id, None).unwrap();
    eprintln!("plan blockers: {:?}", plan.blockers);
    let before = f.batch.counter.load(Ordering::SeqCst);
    let run = f
        .service
        .start(&StartWorkflowRunRequest { plan: plan.plan.clone(), approval_hash: plan.approval_hash.clone() })
        .map(|r| {
            assert!(f.service.wait_idle(Duration::from_secs(30)));
            f.service.get(&r.id).unwrap()
        });
    let step = run.as_ref().ok().and_then(|r| r.steps.iter().find(|s| s.step == "c/fit").cloned());
    let attempt = step.as_ref().and_then(|s| s.attempts.last().cloned());
    eprintln!(
        "run: {:?}; c/fit: {:?} {:?} {:?}",
        run.as_ref().map(|r| r.state),
        step.as_ref().map(|s| s.state),
        attempt.as_ref().and_then(|a| a.code.clone()),
        attempt.as_ref().and_then(|a| a.message.clone())
    );
    assert_eq!(f.batch.counter.load(Ordering::SeqCst), before, "held: nothing ran");
    assert!(
        plan.blockers.iter().any(|b| b.code == "workflow.notWellFormed" || b.code == "workflow.inputTypeMismatch"),
        "the plan must say the .py script cannot run in the R step"
    );
}
