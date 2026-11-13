//! forEach values reach batch code (M9): a forEach over a batch's `value`
//! parameter runs the code once per value, each instance with
//! `EXPOSITION_PARAM_VALUE`, `EXPOSITION_ITERATION`, and its own
//! `EXPOSITION_SEED`; the instances' outputs are collected in order into the
//! consumer's `multiple` port.
//!
//! - The restricted Python test is `#[ignore]`d: Docker Desktop with the local
//!   image `python:3.13-slim` (never pulled). Run it with
//!   `EXPOSITION_DOCKER_TESTS=1 cargo test -p exposition-workflow --test foreach_values -- --ignored`.
//! - The native R/Julia test runs when research discovery finds `Rscript` /
//!   `julia` (otherwise it prints `SKIPPED`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_execution::{DockerSettings, ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_graph::workflow::WorkflowDefinition;
use exposition_graph::{
    GraphConfig, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowId,
};
use exposition_research::batch::{BatchLanguage, BatchMode, BatchRunner};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::{ResearchConfig, ResearchService};
use exposition_workflow::adapters::batch::BatchAdapter;
use exposition_workflow::adapters::{Adapters, rj, runner_ids};
use exposition_workflow::{
    StartWorkflowRunRequest, WorkflowHost, WorkflowOutputValue, WorkflowRun, WorkflowRunConfig, WorkflowRunError,
    WorkflowRunService, WorkflowRunState, WorkflowStepState,
};
use exposition_workspace::DocumentService;
use serde_json::json;

const PYTHON_IMAGE: &str = "python:3.13-slim";
const VALUES: [&str; 3] = ["alpha", "beta 2", "gamma"];

struct Host {
    root: PathBuf,
    graph: Arc<GraphService>,
    execution: ExecutionService,
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
    graph: Arc<GraphService>,
    research: Arc<ResearchService>,
    service: WorkflowRunService,
    runner: Arc<dyn BatchRunner>,
}

fn fixture(python_image: Option<&str>, trusted: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("project");
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/obs.csv"), "x,y\n1,2\n").unwrap();
    let data = base.join("appdata");
    let graph = GraphService::new(GraphConfig { data_dir: data.join("graph") });
    assert!(graph.attach(&root).is_usable());
    let execution = ExecutionService::with_config(ExecutionConfig {
        data_dir: data.clone(),
        host_env: exposition_execution::env::HostEnv::capture(),
        settings: Some(ExecutionSettings {
            docker: DockerSettings { image: python_image.map(str::to_owned), ..Default::default() },
            ..Default::default()
        }),
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: None,
    })
    .unwrap();
    if trusted {
        execution.grant_trust(&root.display().to_string()).unwrap();
    }
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        data.clone(),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::new(DocumentService::new()),
    )));
    research.set_workspace(&root, trusted).unwrap();
    let runner = exposition_research::sandbox::batch_runner(research.clone(), execution.clone());
    let mut adapters = Adapters::new();
    adapters.insert(
        runner_ids::RESTRICTED_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::Restricted)),
    );
    adapters.insert(
        runner_ids::NATIVE_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::TrustedNative)),
    );
    rj::install(&mut adapters, Some(runner.clone()));
    let host: Arc<dyn WorkflowHost> = Arc::new(Host { root: root.clone(), graph: graph.clone(), execution });
    let service =
        WorkflowRunService::new(WorkflowRunConfig { database: None, host, adapters, lean: None, research: None })
            .unwrap();
    Fixture { _tmp: tmp, root, graph, research, service, runner }
}

/// `sweep` (forEach over `value`) → `join` (its `data` port collects the
/// instances' datasets), both running `operation` with `language` code.
fn sweep_and_join(operation: &str, language: &str, sweep: &str, join: &str) -> WorkflowDefinition {
    let native = operation.contains("native");
    let step = |id: &str, code: &str| {
        json!({"id": id, "label": format!("Step {id}"), "operation": {"id": operation, "version": 1},
               "parameters": {}, "payload": {"kind": "code", "language": language, "source": code}})
    };
    let mut sweep_step = step("sweep", sweep);
    sweep_step["repeat"] = json!({"kind": "forEach", "parameter": "value",
        "values": VALUES.iter().map(|v| json!({"kind": "text", "value": v})).collect::<Vec<_>>()});
    serde_json::from_value(json!({
        "schema": "exposition.workflow/2",
        "title": "Sweep",
        "description": "",
        "preset": null,
        "inputs": [],
        "steps": [sweep_step, step("join", join)],
        "connections": [{"kind": "data", "from": {"kind": "step", "step": "sweep", "port": "dataset"},
                         "to": {"step": "join", "port": "data"}}],
        "outcomes": [{"id": "joined", "kind": "transformedDataset", "label": "Joined",
                      "from": {"step": "join", "port": "dataset"}, "optional": false}],
        "ceiling": {"execution": if native { "trustedNative" } else { "restricted" },
                    "modelRequests": false, "retainResults": false, "proposeChanges": false},
        "execution": {"maxParallel": 2}
    }))
    .unwrap()
}

fn run(f: &Fixture, definition: WorkflowDefinition) -> WorkflowRun {
    let id = f
        .graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: None, expected_revision: None, definition },
            &ValidationContext::default(),
        )
        .unwrap()
        .id;
    let plan = f.service.plan(&id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert!(plan.notices.iter().any(|n| n.contains("runs once for each of 3 values of value")), "{:?}", plan.notices);
    let run = f.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(f.service.wait_idle(Duration::from_secs(300)), "the run did not finish");
    f.service.get(&run.id).unwrap()
}

/// Each instance's dataset says `<value>|<iteration>|<seed>`: the value
/// sweeps in order, every instance has its own seed (recorded as its
/// environment's seed), and the join received the datasets in order.
fn assert_swept(f: &Fixture, run: &WorkflowRun) {
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?} {:?}", run.code, run.message);
    let sweep = run.steps.iter().find(|s| s.step == "sweep").unwrap();
    assert_eq!(sweep.state, WorkflowStepState::Succeeded);
    assert_eq!(sweep.instances.len(), VALUES.len());
    let mut seeds = Vec::new();
    let mut paths = Vec::new();
    for (k, instance) in sweep.instances.iter().enumerate() {
        let attempt = &instance.attempts[0];
        let output = attempt.outputs.iter().find(|o| o.port == "dataset").expect("dataset");
        assert_eq!(output.provenance.instance, Some(k as u32));
        let WorkflowOutputValue::File { path, .. } = &output.value else { panic!("{:?}", output.value) };
        let text = std::fs::read_to_string(f.root.join(path.as_str())).unwrap();
        let fields: Vec<&str> = text.trim().split('|').collect();
        assert_eq!(fields[..2], [VALUES[k], k.to_string().as_str()], "instance {k}: {text}");
        let seed: u64 = fields[2].parse().unwrap();
        assert_eq!(attempt.environment.as_ref().and_then(|e| e.seed), Some(seed), "the given seed is recorded");
        seeds.push(seed);
        paths.push(path.to_string());
    }
    let mut distinct = seeds.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), seeds.len(), "every instance has its own seed: {seeds:?}");
    // The join's research run staged the instances' datasets in index order.
    let join = run.steps.iter().find(|s| s.step == "join").unwrap();
    let operation = join.attempts[0].operation.as_ref().unwrap();
    let manifest = f.research.run_manifest(&operation.id).unwrap();
    let staged: Vec<String> = manifest.record.inputs.iter().map(|i| i.path.to_string()).collect();
    assert_eq!(staged, paths, "collected in order");
}

const PYTHON_SWEEP: &str = r#"
import os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
with open(os.path.join(out, "dataset.txt"), "w") as f:
    f.write("|".join([os.environ["EXPOSITION_PARAM_VALUE"], os.environ["EXPOSITION_ITERATION"], os.environ["EXPOSITION_SEED"]]))
"#;

const PYTHON_JOIN: &str = r#"
import os
open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.txt"), "w").write("joined")
"#;

#[test]
#[ignore = "needs Docker Desktop and the local image python:3.13-slim; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn a_restricted_python_for_each_gives_each_instance_its_value_iteration_and_seed() {
    if !std::env::var("EXPOSITION_DOCKER_TESTS").is_ok_and(|v| v == "1") {
        eprintln!("SKIPPED: set EXPOSITION_DOCKER_TESTS=1 (Docker Desktop with local {PYTHON_IMAGE})");
        return;
    }
    let f = fixture(Some(PYTHON_IMAGE), false);
    let run = run(&f, sweep_and_join("research.pythonBatch", "python", PYTHON_SWEEP, PYTHON_JOIN));
    assert_swept(&f, &run);
}

const R_SWEEP: &str = r#"
out <- Sys.getenv("EXPOSITION_OUTPUT_DIR")
writeLines(paste(Sys.getenv("EXPOSITION_PARAM_VALUE"), Sys.getenv("EXPOSITION_ITERATION"), Sys.getenv("EXPOSITION_SEED"), sep = "|"), file.path(out, "dataset.txt"))
"#;

const R_JOIN: &str = r#"writeLines("joined", file.path(Sys.getenv("EXPOSITION_OUTPUT_DIR"), "dataset.txt"))"#;

const JULIA_SWEEP: &str = r#"
write(joinpath(ENV["EXPOSITION_OUTPUT_DIR"], "dataset.txt"), join([ENV["EXPOSITION_PARAM_VALUE"], ENV["EXPOSITION_ITERATION"], ENV["EXPOSITION_SEED"]], "|"))
"#;

const JULIA_JOIN: &str = r#"write(joinpath(ENV["EXPOSITION_OUTPUT_DIR"], "dataset.txt"), "joined")"#;

#[test]
fn a_native_r_or_julia_for_each_gives_each_instance_its_value_iteration_and_seed() {
    let mut ran = 0;
    for (language, operation, name, sweep, join) in [
        (BatchLanguage::R, "research.nativeR", "r", R_SWEEP, R_JOIN),
        (BatchLanguage::Julia, "research.nativeJulia", "julia", JULIA_SWEEP, JULIA_JOIN),
    ] {
        let f = fixture(None, true);
        if let Err(reason) = f.runner.language_availability(&f.root, language, BatchMode::TrustedNative) {
            eprintln!("SKIPPED {}: {reason}", language.title());
            continue;
        }
        let run = run(&f, sweep_and_join(operation, name, sweep, join));
        assert_swept(&f, &run);
        ran += 1;
    }
    if ran == 0 {
        eprintln!("SKIPPED: neither R nor Julia is installed");
    }
}
