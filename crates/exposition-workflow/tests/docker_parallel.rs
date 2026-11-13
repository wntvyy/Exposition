//! M9 parallel workflow runs against the real restricted (Docker) batch
//! adapter: two independent branches execute in two containers at the same
//! time, a join receives both outputs, and cancel stops both containers.
//!
//! `#[ignore]`d: they need Docker Desktop with the local image
//! `python:3.13-slim` (never pulled). Run with
//! `EXPOSITION_DOCKER_TESTS=1 cargo test -p exposition-workflow --test docker_parallel -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::path::WorkspacePath;
use exposition_execution::env::HostEnv;
use exposition_execution::{DockerSettings, ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_graph::workflow::presets::{NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use exposition_graph::workflow::{
    CodeLanguage, PortSource, StepOperation, StepPayload, StepPort, WORKFLOW_SCHEMA_V2, WorkflowConnection,
    WorkflowDefinition, WorkflowExecutionOptions, WorkflowOutcome, WorkflowStep,
};
use exposition_graph::{
    GraphConfig, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowId,
};
use exposition_research::batch::{BatchMode, BatchRunner};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::sandbox::batch_runner;
use exposition_research::{ResearchConfig, ResearchService};
use exposition_workflow::adapters::batch::BatchAdapter;
use exposition_workflow::adapters::{Adapters, runner_ids};
use exposition_workflow::{
    StartWorkflowRunRequest, WorkflowAttemptState, WorkflowHost, WorkflowOutputValue, WorkflowRun, WorkflowRunConfig,
    WorkflowRunError, WorkflowRunService, WorkflowRunState, WorkflowStepAttempt, WorkflowStepState,
};
use exposition_workspace::DocumentService;

const IMAGE: &str = "python:3.13-slim";

fn docker_enabled() -> bool {
    let on = std::env::var("EXPOSITION_DOCKER_TESTS").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("SKIPPED: set EXPOSITION_DOCKER_TESTS=1 (Docker Desktop with local {IMAGE})");
    }
    on
}

struct Host {
    root: PathBuf,
    execution: ExecutionService,
    graph: Arc<GraphService>,
}

impl WorkflowHost for Host {
    fn project_root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }
    fn trusted(&self, root: &Path) -> bool {
        self.execution.trust_status(&root.to_string_lossy()).is_ok_and(|s| s.trusted)
    }
    fn restricted(&self) -> (Option<String>, Option<String>) {
        (Some("docker".into()), self.execution.settings().docker.image)
    }
    fn restricted_image_id(&self) -> Option<String> {
        self.execution.restricted_preflight().ok().and_then(|t| t.image_id)
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

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    execution: ExecutionService,
    graph: Arc<GraphService>,
    service: WorkflowRunService,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("project");
    std::fs::create_dir_all(root.join("data")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    std::fs::write(root.join("data/obs.csv"), "x,y\n1,2\n2,4\n").unwrap();
    let data = base.join("appdata");
    let host_env = HostEnv::from_pairs(std::env::vars().filter(|(k, _)| k != "ANTHROPIC_API_KEY"));
    let settings = ExecutionSettings {
        docker: DockerSettings { image: Some(IMAGE.into()), ..Default::default() },
        ..Default::default()
    };
    let execution = ExecutionService::with_config(ExecutionConfig {
        data_dir: data.clone(),
        host_env,
        settings: Some(settings),
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: None,
    })
    .unwrap();
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        data.clone(),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::new(DocumentService::new()),
    )));
    research.set_workspace(&root, false).unwrap();
    let runner: Arc<dyn BatchRunner> = batch_runner(research, execution.clone());
    let graph = GraphService::new(GraphConfig { data_dir: data.join("graph") });
    assert!(graph.attach(&root).is_usable());
    let mut adapters = Adapters::new();
    adapters
        .insert(runner_ids::RESTRICTED_PYTHON.into(), Arc::new(BatchAdapter::new(Some(runner), BatchMode::Restricted)));
    let host = Arc::new(Host { root: root.clone(), execution: execution.clone(), graph: graph.clone() });
    let service = WorkflowRunService::new(WorkflowRunConfig {
        database: Some(data.join("workflow-runs.sqlite3")),
        host,
        adapters,
        lean: None,
        research: None,
    })
    .unwrap();
    Fixture { _tmp: tmp, root, execution, graph, service }
}

fn step(id: &str, source: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Branch {id}"),
        operation: StepOperation { id: "research.pythonBatch".into(), version: 1 },
        parameters: Default::default(),
        payload: Some(StepPayload::Code { language: CodeLanguage::Python, source: source.into() }),
        composite: None,
        condition: None,
        repeat: None,
    }
}

fn data_to(from: PortSource, step: &str) -> WorkflowConnection {
    WorkflowConnection::Data { from, to: StepPort { step: step.into(), port: "data".into() } }
}

fn branch_code(name: &str, seconds: u32) -> String {
    format!(
        r#"import json, os, time
out = os.environ["EXPOSITION_OUTPUT_DIR"]
start = time.time()
time.sleep({seconds})
with open(os.path.join(out, "dataset.json"), "w") as f:
    json.dump({{"branch": "{name}", "start": start, "end": time.time()}}, f)
"#
    )
}

const JOIN: &str = r#"import glob, json, os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
rows = [json.load(open(p)) for p in sorted(glob.glob(".exposition/artifacts/*/dataset.json"))]
with open(os.path.join(out, "dataset.json"), "w") as f:
    json.dump(rows, f)
"#;

/// Branches `a` and `b` (each `seconds` long) from the `data` input, joined.
fn two_branches(seconds: u32) -> WorkflowDefinition {
    let mut def = draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        data: Some(WorkspacePath::parse("data/obs.csv").unwrap()),
        ..Default::default()
    })
    .unwrap();
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.execution = Some(WorkflowExecutionOptions { max_parallel: 2 });
    def.steps = vec![step("a", &branch_code("a", seconds)), step("b", &branch_code("b", seconds)), step("join", JOIN)];
    def.steps[2].label = "Join".into();
    let input = || PortSource::Input { input: "data".into() };
    let out = |s: &str| PortSource::Step { step: s.into(), port: "dataset".into() };
    def.connections =
        vec![data_to(input(), "a"), data_to(input(), "b"), data_to(out("a"), "join"), data_to(out("b"), "join")];
    def.outcomes = vec![WorkflowOutcome {
        id: "joined".into(),
        kind: exposition_graph::workflow::OutcomeKind::TransformedDataset,
        label: "Joined".into(),
        from: StepPort { step: "join".into(), port: "dataset".into() },
        optional: false,
    }];
    def
}

impl Fixture {
    fn save(&self, definition: WorkflowDefinition) -> WorkflowId {
        self.graph
            .save_workflow(
                &Origin::User,
                &SaveWorkflowRequest { id: None, expected_revision: None, definition },
                &ValidationContext::default(),
            )
            .unwrap()
            .id
    }

    fn start(&self, id: &WorkflowId) -> WorkflowRun {
        let plan = self.service.plan(id, None).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.max_parallel, 2);
        self.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap()
    }

    fn containers(&self) -> String {
        let filter = format!("label=org.exposition.instance={}", self.execution.docker_instance());
        let out = std::process::Command::new("docker").args(["ps", "-aq", "--filter", &filter]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn json_of(&self, attempt: &WorkflowStepAttempt) -> serde_json::Value {
        let output = attempt.outputs.iter().find(|o| o.port == "dataset").expect("dataset output");
        let WorkflowOutputValue::File { path, .. } = &output.value else { panic!("{:?}", output.value) };
        let host = path.components().fold(self.root.clone(), |p, c| p.join(c));
        serde_json::from_slice(&std::fs::read(host).unwrap()).unwrap()
    }
}

fn attempt<'a>(run: &'a WorkflowRun, step: &str) -> &'a WorkflowStepAttempt {
    run.steps.iter().find(|s| s.step == step).unwrap().attempts.last().unwrap()
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn two_restricted_branches_run_in_two_containers_at_the_same_time() {
    if !docker_enabled() {
        return;
    }
    let f = fixture();
    let id = f.save(two_branches(6));
    let began = Instant::now();
    let run = f.start(&id);
    assert!(f.service.wait_idle(Duration::from_secs(180)), "the run finished");
    let elapsed = began.elapsed();
    let run = f.service.get(&run.id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?} {:?}", run.code, run.message);
    let (a, b) = (attempt(&run, "a"), attempt(&run, "b"));
    for x in [a, b] {
        assert_eq!(x.state, WorkflowAttemptState::Succeeded);
        assert_eq!(x.operation.as_ref().unwrap().kind, "researchRun");
    }
    // Host view: each branch was dispatched before the other finished.
    assert!(a.dispatched_at_ms.unwrap() < b.finished_at_ms.unwrap(), "a {a:?} b {b:?}");
    assert!(b.dispatched_at_ms.unwrap() < a.finished_at_ms.unwrap(), "a {a:?} b {b:?}");
    // Container view: the two sleeps overlapped in time.
    let (ja, jb) = (f.json_of(a), f.json_of(b));
    let t = |j: &serde_json::Value, k: &str| j[k].as_f64().unwrap();
    assert!(t(&ja, "start") < t(&jb, "end") && t(&jb, "start") < t(&ja, "end"), "{ja} {jb}");
    let overlap = t(&ja, "end").min(t(&jb, "end")) - t(&ja, "start").max(t(&jb, "start"));
    assert!(overlap > 3.0, "the containers ran together for {overlap:.1} s");
    // The join received both branches' outputs.
    let joined = f.json_of(attempt(&run, "join"));
    let mut branches: Vec<&str> = joined.as_array().unwrap().iter().map(|r| r["branch"].as_str().unwrap()).collect();
    branches.sort();
    assert_eq!(branches, vec!["a", "b"]);
    assert!(run.outcomes[0].value.is_some());
    eprintln!(
        "two 6 s branches + join finished in {:.1} s; containers overlapped {overlap:.1} s",
        elapsed.as_secs_f64()
    );
    assert_eq!(f.containers(), "", "no container is left behind");
    assert_eq!(f.service.slot_counts(), Default::default());
}

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn cancel_stops_both_running_containers() {
    if !docker_enabled() {
        return;
    }
    let f = fixture();
    let id = f.save(two_branches(120));
    let run = f.start(&id);
    // Both branches dispatched, and their containers are up.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let r = f.service.get(&run.id).unwrap();
        let dispatched = ["a", "b"].iter().all(|s| {
            r.steps.iter().find(|x| x.step == *s).unwrap().attempts.last().map(|a| a.state)
                == Some(WorkflowAttemptState::Dispatched)
        });
        if dispatched && f.containers().lines().count() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "both branches never ran: {:?}", r.steps);
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_secs(2));
    let asked = Instant::now();
    let canceled = f.service.cancel(&run.id).unwrap();
    assert!(f.service.wait_idle(Duration::from_secs(30)));
    let stopped_in = asked.elapsed();
    let canceled = if canceled.state == WorkflowRunState::Running { f.service.get(&run.id).unwrap() } else { canceled };
    assert_eq!(canceled.state, WorkflowRunState::Canceled, "{:?}", canceled.message);
    for s in ["a", "b"] {
        let step = canceled.steps.iter().find(|x| x.step == s).unwrap();
        assert_eq!(step.state, WorkflowStepState::Canceled, "{s}");
        assert_eq!(attempt(&canceled, s).state, WorkflowAttemptState::Canceled);
    }
    assert_eq!(canceled.steps.iter().find(|x| x.step == "join").unwrap().state, WorkflowStepState::Skipped);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f.containers().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(f.containers(), "", "both containers were removed");
    eprintln!("cancel stopped both branches in {:.1} s", stopped_in.as_secs_f64());
    assert!(stopped_in < Duration::from_secs(30));
}
