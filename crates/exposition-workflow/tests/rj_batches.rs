//! R and Julia workflow steps through the workflow runner (M9, lane F).
//!
//! - Fake-runner tests check what the adapters hand the research runner and
//!   what they refuse (no R, Julia, or Docker needed).
//! - Native tests run real R and Julia batches (discovered `Rscript`/`julia`)
//!   through plan → approval → run → validated outputs; they print `SKIPPED`
//!   when the language is not installed.
//! - Restricted tests are `#[ignore]`d: Docker Desktop with the local images
//!   `r-base:4.6.2`, `julia:1.13`, and `python:3.13-slim` (never pulled; see
//!   `EXPOSITION_TEST_R_IMAGE` / `EXPOSITION_TEST_JULIA_IMAGE`). Run them with
//!   `EXPOSITION_DOCKER_TESTS=1 cargo test -p exposition-workflow --test rj_batches -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use exposition_execution::{DockerSettings, ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_graph::workflow::WorkflowDefinition;
use exposition_graph::workflow::registry::{
    RESEARCH_JULIA_BATCH, RESEARCH_NATIVE_JULIA, RESEARCH_NATIVE_R, RESEARCH_R_BATCH,
};
use exposition_graph::{
    GraphConfig, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowId,
};
use exposition_research::batch::{
    BatchCode, BatchControl, BatchLanguage, BatchMode, BatchPortOutput, BatchRequest, BatchResult, BatchRunner,
};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::types::RunExecution;
use exposition_research::{ResearchConfig, ResearchError, ResearchService};
use exposition_workflow::adapters::{Adapters, rj, sha256_hex};
use exposition_workflow::{
    StartWorkflowRunRequest, WorkflowHost, WorkflowOutputValue, WorkflowRun, WorkflowRunConfig, WorkflowRunError,
    WorkflowRunPlan, WorkflowRunService, WorkflowRunState, WorkflowStepState,
};
use exposition_workspace::DocumentService;
use serde_json::json;

const IMAGE_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IMAGE_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PYTHON_IMAGE: &str = "python:3.13-slim";

// ----------------------------------------------------------------- host ----

struct Host {
    root: PathBuf,
    graph: Arc<GraphService>,
    execution: Option<ExecutionService>,
    /// The restricted image (reference, identity) the plan binds.
    image: Mutex<(Option<String>, Option<String>)>,
}

impl WorkflowHost for Host {
    fn project_root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }
    fn trusted(&self, root: &Path) -> bool {
        match &self.execution {
            Some(execution) => execution.trust_status(&root.to_string_lossy()).is_ok_and(|s| s.trusted),
            None => true,
        }
    }
    fn restricted(&self) -> (Option<String>, Option<String>) {
        (Some("docker".into()), self.image.lock().unwrap().0.clone())
    }
    fn restricted_image_id(&self) -> Option<String> {
        self.image.lock().unwrap().1.clone()
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

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    graph: Arc<GraphService>,
}

fn project() -> Project {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("project");
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/obs.csv"), "x,y\n1,2.5\n2,3.5\n").unwrap();
    let data = base.join("appdata");
    let graph = GraphService::new(GraphConfig { data_dir: data.join("graph") });
    assert!(graph.attach(&root).is_usable());
    Project { _tmp: tmp, root, data, graph }
}

fn service(
    p: &Project,
    execution: Option<ExecutionService>,
    runner: Arc<dyn BatchRunner>,
    image: (Option<&str>, Option<&str>),
) -> (WorkflowRunService, Arc<Host>) {
    let host = Arc::new(Host {
        root: p.root.clone(),
        graph: p.graph.clone(),
        execution,
        image: Mutex::new((image.0.map(str::to_owned), image.1.map(str::to_owned))),
    });
    let mut adapters = Adapters::new();
    rj::install(&mut adapters, Some(runner));
    let h: Arc<dyn WorkflowHost> = host.clone();
    let service =
        WorkflowRunService::new(WorkflowRunConfig { database: None, host: h, adapters, lean: None, research: None })
            .unwrap();
    (service, host)
}

// ----------------------------------------------------------- definitions ----

fn resource(id: &str, label: &str, port_type: &str, path: &str) -> serde_json::Value {
    json!({"id": id, "label": label, "portType": port_type,
           "source": {"kind": "resource", "path": path, "version": {"kind": "atRunStart"}}})
}

fn feed(input: &str, port: &str) -> serde_json::Value {
    json!({"kind": "data", "from": {"kind": "input", "input": input}, "to": {"step": "run", "port": port}})
}

/// One `operation` step fed `data/obs.csv` (and `script`, when given, instead
/// of inline `code`), whose dataset is the workflow's outcome.
fn definition(operation: &str, language: &str, code: Option<&str>, script: Option<&str>) -> WorkflowDefinition {
    let native = operation.contains("native");
    let mut inputs = vec![resource("data", "Input data", "dataset", "data/obs.csv")];
    let mut connections = vec![feed("data", "data")];
    if let Some(script) = script {
        inputs.push(resource("script", "Script", "codeSource", script));
        connections.push(feed("script", "script"));
    }
    let payload = code.map(|c| json!({"kind": "code", "language": language, "source": c}));
    serde_json::from_value(json!({
        "schema": "exposition.workflow/1",
        "title": format!("{language} batch"),
        "description": "",
        "preset": null,
        "inputs": inputs,
        "steps": [{"id": "run", "label": "Run the analysis", "operation": {"id": operation, "version": 1},
                   "parameters": {}, "payload": payload}],
        "connections": connections,
        "outcomes": [{"id": "dataset", "kind": "transformedDataset", "label": "Dataset",
                      "from": {"step": "run", "port": "dataset"}, "optional": false}],
        "ceiling": {"execution": if native { "trustedNative" } else { "restricted" },
                    "modelRequests": false, "retainResults": false, "proposeChanges": false}
    }))
    .unwrap()
}

fn save(p: &Project, definition: WorkflowDefinition) -> WorkflowId {
    p.graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: None, expected_revision: None, definition },
            &ValidationContext::default(),
        )
        .unwrap()
        .id
}

fn plan(service: &WorkflowRunService, id: &WorkflowId) -> WorkflowRunPlan {
    service.plan(id, None).unwrap()
}

fn run_to_end(service: &WorkflowRunService, plan: &WorkflowRunPlan, timeout: Duration) -> WorkflowRun {
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let run = service
        .start(&StartWorkflowRunRequest { plan: plan.plan.clone(), approval_hash: plan.approval_hash.clone() })
        .unwrap();
    assert!(service.wait_idle(timeout), "the run did not finish");
    service.get(&run.id).unwrap()
}

fn only_attempt(run: &WorkflowRun) -> &exposition_workflow::WorkflowStepAttempt {
    let step = run.steps.iter().find(|s| s.step == "run").unwrap();
    assert_eq!(step.attempts.len(), 1, "one attempt, never repeated");
    &step.attempts[0]
}

// ----------------------------------------------------------------- fakes ----

type Call = (BatchLanguage, BatchMode, BatchCode, Option<String>);

/// Records each call and writes a dataset (and figure) like a batch would.
struct FakeRunner {
    calls: Mutex<Vec<Call>>,
    /// What `restricted_image` reports for every language.
    image: Mutex<Result<(String, Option<String>), String>>,
    counter: AtomicUsize,
}

impl FakeRunner {
    fn new(image_id: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            image: Mutex::new(Ok(("r-or-julia:local".into(), image_id.map(str::to_owned)))),
            counter: AtomicUsize::new(1),
        })
    }
}

fn artifact(root: &Path, run: &str, port: &str, name: &str, content: &[u8]) -> BatchPortOutput {
    let rel = format!(".exposition/artifacts/{run}/{name}");
    let abs = root.join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, content).unwrap();
    BatchPortOutput {
        port: port.into(),
        artifact: ArtifactRecord {
            id: ArtifactId(format!("{run}/{name}")),
            kind: ArtifactKind::Data,
            path: Some(WorkspacePath::parse(&rel).unwrap()),
            scratch_path: None,
            media_type: "text/csv".into(),
            size_bytes: content.len() as u64,
            content_hash: format!("sha256:{}", sha256_hex(content)),
        },
    }
}

impl BatchRunner for FakeRunner {
    fn availability(&self, _root: &Path, _mode: BatchMode) -> Result<(), String> {
        Ok(())
    }
    fn run(&self, _: &Path, _: &BatchRequest, _: &BatchControl<'_>) -> Result<BatchResult, ResearchError> {
        panic!("R and Julia steps never run as Python batches");
    }
    fn language_availability(&self, _root: &Path, _language: BatchLanguage, _mode: BatchMode) -> Result<(), String> {
        Ok(())
    }
    fn run_language(
        &self,
        root: &Path,
        language: BatchLanguage,
        request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError> {
        self.calls.lock().unwrap().push((
            language,
            request.mode,
            request.code.clone(),
            request.restricted_image_id.clone(),
        ));
        let run = format!("run-{}", self.counter.fetch_add(1, Ordering::SeqCst));
        (control.on_dispatched)(&run);
        Ok(BatchResult {
            research_run: run.clone(),
            status: RunStatus::Succeeded,
            exit_code: Some(0),
            ports: vec![artifact(root, &run, "dataset", "dataset.csv", format!("n,total\n2,6\n{run}\n").as_bytes())],
            extra: Vec::new(),
            rejected: Vec::new(),
            log: format!("[stdout]\nran {}\n", language.title()),
            environment: Vec::new(),
        })
    }
    fn restricted_image(&self, _language: BatchLanguage) -> Result<(String, Option<String>), String> {
        self.image.lock().unwrap().clone()
    }
}

// ------------------------------------------------------- fake-runner tests ----

#[test]
fn r_and_julia_steps_hand_their_code_to_the_language_runner() {
    for (operation, language, mode, name) in [
        (RESEARCH_NATIVE_R, BatchLanguage::R, BatchMode::TrustedNative, "r"),
        (RESEARCH_R_BATCH, BatchLanguage::R, BatchMode::Restricted, "r"),
        (RESEARCH_NATIVE_JULIA, BatchLanguage::Julia, BatchMode::TrustedNative, "julia"),
        (RESEARCH_JULIA_BATCH, BatchLanguage::Julia, BatchMode::Restricted, "julia"),
    ] {
        let p = project();
        let fake = FakeRunner::new(Some(IMAGE_A));
        let (service, _) = service(&p, None, fake.clone(), (Some("lang:local"), Some(IMAGE_A)));
        let code = format!("# {name} code\n");
        let id = save(&p, definition(operation, name, Some(&code), None));
        let plan = plan(&service, &id);
        let run = run_to_end(&service, &plan, Duration::from_secs(20));
        assert_eq!(run.state, WorkflowRunState::Succeeded, "{operation}: {:?} {:?}", run.code, run.message);
        let calls = fake.calls.lock().unwrap().clone();
        let pinned = (mode == BatchMode::Restricted).then(|| IMAGE_A.to_owned());
        assert_eq!(calls, vec![(language, mode, BatchCode::Inline { source: code.clone() }, pinned)], "{operation}");
        let attempt = only_attempt(&run);
        assert_eq!(attempt.outputs.len(), 1);
        assert!(
            matches!(&attempt.outputs[0].value, WorkflowOutputValue::File { path, .. } if path.as_str().ends_with("dataset.csv"))
        );
        assert!(run.outcomes[0].value.is_some(), "the declared outcome is realized");
        assert_eq!(plan.steps[0].runner, rj::ADAPTERS.iter().find(|(_, l, m)| *l == language && *m == mode).unwrap().0);
    }
}

/// An R step never runs a Python (or Julia) script: refused before the
/// research runner is called, without a retry.
#[test]
fn a_script_of_another_language_is_refused_before_the_runner_is_called() {
    for (operation, script) in [(RESEARCH_NATIVE_R, "analysis/fit.py"), (RESEARCH_JULIA_BATCH, "analysis/fit.R")] {
        let p = project();
        std::fs::create_dir_all(p.root.join("analysis")).unwrap();
        std::fs::write(p.root.join(script), "print(1)\n").unwrap();
        let fake = FakeRunner::new(Some(IMAGE_A));
        let (service, _) = service(&p, None, fake.clone(), (Some("lang:local"), Some(IMAGE_A)));
        let id = save(&p, definition(operation, "r", None, Some(script)));
        let plan = plan(&service, &id);
        assert!(plan.inputs.iter().any(|i| i.text.as_deref() == Some("print(1)\n")), "the script is shown for review");
        let run = run_to_end(&service, &plan, Duration::from_secs(20));
        assert_eq!(run.state, WorkflowRunState::Failed);
        let attempt = only_attempt(&run);
        assert_eq!(attempt.code.as_deref(), Some("workflow.invalidInput"), "{:?}", attempt.message);
        assert!(attempt.message.as_deref().unwrap_or("").contains(script), "{:?}", attempt.message);
        assert!(fake.calls.lock().unwrap().is_empty(), "the research runner was never called");
    }
}

/// A restricted R/Julia step runs only when the identity the plan approved
/// is the language image's identity (the research runner checks again).
#[test]
fn restricted_steps_run_only_in_the_identity_the_plan_approved() {
    for operation in [RESEARCH_R_BATCH, RESEARCH_JULIA_BATCH] {
        let p = project();
        // The plan approved image A; the language's image is B.
        let fake = FakeRunner::new(Some(IMAGE_B));
        let (service, _) = service(&p, None, fake.clone(), (Some("python:3.13-slim"), Some(IMAGE_A)));
        let id = save(
            &p,
            definition(operation, if operation == RESEARCH_R_BATCH { "r" } else { "julia" }, Some("1\n"), None),
        );
        let run = run_to_end(&service, &plan(&service, &id), Duration::from_secs(20));
        assert_eq!(run.state, WorkflowRunState::Failed);
        let attempt = only_attempt(&run);
        assert_eq!(attempt.code.as_deref(), Some("workflow.restrictedUnavailable"), "{:?}", attempt.message);
        let message = attempt.message.clone().unwrap_or_default();
        assert!(message.contains("sha256:bbbbbbbbbbbb") && message.contains("sha256:aaaaaaaaaaaa"), "{message}");
        assert!(message.contains("Nothing runs natively"), "{message}");
        assert!(fake.calls.lock().unwrap().is_empty(), "nothing was dispatched");
        // Restricted R/Julia unavailable: refused the same way, never native.
        *fake.image.lock().unwrap() = Err("no restricted R image is configured.".into());
        let run = run_to_end(&service, &plan(&service, &id), Duration::from_secs(20));
        assert_eq!(only_attempt(&run).code.as_deref(), Some("workflow.restrictedUnavailable"));
        assert!(fake.calls.lock().unwrap().is_empty());
        // The approved identity: runs, pinned to it.
        *fake.image.lock().unwrap() = Ok(("lang:local".into(), Some(IMAGE_A.into())));
        let run = run_to_end(&service, &plan(&service, &id), Duration::from_secs(20));
        assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
        assert_eq!(fake.calls.lock().unwrap()[0].3.as_deref(), Some(IMAGE_A));
    }
}

/// A batch runner without R/Julia support makes the steps unavailable in the
/// plan (never runs them as Python).
#[test]
fn a_runner_without_language_support_makes_the_steps_unavailable() {
    struct PythonOnly;
    impl BatchRunner for PythonOnly {
        fn availability(&self, _root: &Path, _mode: BatchMode) -> Result<(), String> {
            Ok(())
        }
        fn run(&self, _: &Path, _: &BatchRequest, _: &BatchControl<'_>) -> Result<BatchResult, ResearchError> {
            panic!("never called for R or Julia");
        }
    }
    let p = project();
    let (service, _) = service(&p, None, Arc::new(PythonOnly), (Some("python:3.13-slim"), Some(IMAGE_A)));
    for (operation, language) in [
        (RESEARCH_NATIVE_R, "r"),
        (RESEARCH_R_BATCH, "r"),
        (RESEARCH_NATIVE_JULIA, "julia"),
        (RESEARCH_JULIA_BATCH, "julia"),
    ] {
        let id = save(&p, definition(operation, language, Some("1\n"), None));
        let plan = plan(&service, &id);
        assert!(!plan.steps[0].available, "{operation}");
        assert!(
            plan.blockers.iter().any(|b| b.message.contains("not supported by this batch runner")),
            "{operation}: {:?}",
            plan.blockers
        );
        let err = service
            .start(&StartWorkflowRunRequest { plan: plan.plan.clone(), approval_hash: plan.approval_hash.clone() })
            .unwrap_err();
        assert!(!err.code.is_empty());
    }
}

// ------------------------------------------------------------ real runs ----

struct Real {
    project: Project,
    execution: ExecutionService,
    research: Arc<ResearchService>,
    runner: Arc<dyn BatchRunner>,
}

fn real(images: (Option<&str>, Option<&str>, Option<&str>), trusted: bool) -> Real {
    let project = project();
    let settings = ExecutionSettings {
        docker: DockerSettings {
            image: images.0.map(str::to_owned),
            r_image: images.1.map(str::to_owned),
            julia_image: images.2.map(str::to_owned),
            ..Default::default()
        },
        ..Default::default()
    };
    let execution = ExecutionService::with_config(ExecutionConfig {
        data_dir: project.data.clone(),
        host_env: exposition_execution::env::HostEnv::capture(),
        settings: Some(settings),
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: None,
    })
    .unwrap();
    if trusted {
        execution.grant_trust(&project.root.display().to_string()).unwrap();
    }
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        project.data.clone(),
        Arc::new(ExecutionLauncher::new(execution.clone())),
        Arc::new(DocumentService::new()),
    )));
    research.set_workspace(&project.root, trusted).unwrap();
    let runner = exposition_research::sandbox::batch_runner(research.clone(), execution.clone());
    Real { project, execution, research, runner }
}

const R_CODE: &str = r#"
out <- Sys.getenv("EXPOSITION_OUTPUT_DIR")
d <- read.csv("data/obs.csv")
write.csv(data.frame(n = nrow(d), total = sum(d$y)), file.path(out, "dataset.csv"), row.names = FALSE)
write.csv(data.frame(x = d$x, y2 = 2 * d$y), file.path(out, "table.csv"), row.names = FALSE)
png(file.path(out, "figure.png"), width = 320, height = 240)
plot(d$x, d$y, type = "b", main = "observations")
invisible(dev.off())
cat("R rows", nrow(d), "total", sum(d$y), "\n")
"#;

const JULIA_SCRIPT: &str = r#"
out = ENV["EXPOSITION_OUTPUT_DIR"]
rows = [split(l, ",") for l in readlines("data/obs.csv")[2:end]]
ys = [parse(Float64, r[2]) for r in rows]
open(joinpath(out, "dataset.csv"), "w") do f
    println(f, "n,total")
    println(f, length(rows), ",", sum(ys))
end
open(joinpath(out, "table.csv"), "w") do f
    println(f, "x,y2")
    foreach(r -> println(f, r[1], ",", 2 * parse(Float64, r[2])), rows)
end
bars = join(["<rect x=\"$(20 * (i - 1))\" y=\"0\" width=\"15\" height=\"$(10 * y)\"/>" for (i, y) in enumerate(ys)])
write(joinpath(out, "figure.svg"), "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\">$bars</svg>")
println("Julia rows ", length(rows), " total ", sum(ys))
"#;

/// Checks a finished R/Julia run: the dataset outcome, the validated
/// dataset/figure/table outputs (digests recomputed here), and the research
/// run behind them with its truthful execution label.
fn assert_produced(r: &Real, run: &WorkflowRun, figure: &str, log: &str) -> exposition_research::types::RunManifest {
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?} {:?}", run.code, run.message);
    assert_eq!(run.steps[0].state, WorkflowStepState::Succeeded);
    let attempt = only_attempt(run);
    assert!(attempt.log.as_deref().unwrap_or("").contains(log), "{:?}", attempt.log);
    let mut ports: Vec<(&str, String)> = attempt
        .outputs
        .iter()
        .map(|o| match &o.value {
            WorkflowOutputValue::File { path, sha256, research_run, .. } => {
                let bytes = std::fs::read(r.project.root.join(path.as_str())).unwrap();
                assert_eq!(sha256, &sha256_hex(&bytes), "{path}");
                assert_eq!(research_run.as_deref(), attempt.operation.as_ref().map(|o| o.id.as_str()));
                (o.port.as_str(), path.file_name().to_owned())
            }
            other => panic!("{other:?}"),
        })
        .collect();
    ports.sort();
    assert_eq!(
        ports,
        [("dataset", "dataset.csv".to_owned()), ("figure", figure.to_owned()), ("table", "table.csv".to_owned())]
    );
    match &run.outcomes[0].value {
        Some(WorkflowOutputValue::File { path, .. }) => {
            let text = std::fs::read_to_string(r.project.root.join(path.as_str())).unwrap();
            assert!(text.contains("2,6"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    let operation = attempt.operation.as_ref().unwrap();
    assert_eq!(operation.kind, "researchRun");
    r.research.run_manifest(&operation.id).unwrap()
}

#[test]
fn native_r_and_julia_workflows_produce_a_dataset_and_a_figure() {
    let r = real((None, None, None), true);
    let mut ran = 0;
    for language in [BatchLanguage::R, BatchLanguage::Julia] {
        if let Err(reason) = r.runner.language_availability(&r.project.root, language, BatchMode::TrustedNative) {
            eprintln!("SKIPPED {}: {reason}", language.title());
            continue;
        }
        ran += 1;
        let (service, _) = service(&r.project, Some(r.execution.clone()), r.runner.clone(), (None, None));
        // R: inline code. Julia: a declared project script (shown in full in the plan).
        let (definition, figure, log) = match language {
            BatchLanguage::R => {
                (definition(RESEARCH_NATIVE_R, "r", Some(R_CODE), None), "figure.png", "R rows 2 total 6")
            }
            _ => {
                std::fs::create_dir_all(r.project.root.join("analysis")).unwrap();
                std::fs::write(r.project.root.join("analysis/fit.jl"), JULIA_SCRIPT).unwrap();
                (
                    definition(RESEARCH_NATIVE_JULIA, "julia", None, Some("analysis/fit.jl")),
                    "figure.svg",
                    "Julia rows 2 total 6.0",
                )
            }
        };
        let id = save(&r.project, definition);
        let plan = plan(&service, &id);
        assert!(
            plan.notices.iter().any(|n| n.contains("natively with your account's privileges")),
            "{:?}",
            plan.notices
        );
        assert_eq!(plan.execution.native_steps, ["run"]);
        if language == BatchLanguage::Julia {
            let script = plan.inputs.iter().find(|i| i.id == "script").unwrap();
            assert_eq!(script.text.as_deref(), Some(JULIA_SCRIPT), "the script is reviewed in full");
        }
        let run = run_to_end(&service, &plan, Duration::from_secs(180));
        let manifest = assert_produced(&r, &run, figure, log);
        match manifest.execution.unwrap() {
            RunExecution::TrustedNative { interpreter } => {
                assert!(
                    interpreter.to_lowercase().contains(if language == BatchLanguage::R { "rscript" } else { "julia" }),
                    "{interpreter}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(manifest.record.inputs.len(), 1 + usize::from(language == BatchLanguage::Julia));
    }
    if ran == 0 {
        eprintln!("SKIPPED: neither R nor Julia is installed");
    }
}

/// Folder trust is checked before a native R/Julia step: an untrusted
/// project's plan is blocked and nothing runs.
#[test]
fn native_r_and_julia_steps_need_a_trusted_folder() {
    let r = real((None, None, None), false);
    let (service, _) = service(&r.project, Some(r.execution.clone()), r.runner.clone(), (None, None));
    for (operation, language) in [(RESEARCH_NATIVE_R, "r"), (RESEARCH_NATIVE_JULIA, "julia")] {
        let id = save(&r.project, definition(operation, language, Some("1\n"), None));
        let plan = plan(&service, &id);
        assert!(plan.blockers.iter().any(|b| b.code == "workflow.untrustedFolder"), "{:?}", plan.blockers);
    }
    assert!(!r.project.root.join(".exposition").join("runs").exists(), "nothing ran");
}

// ----------------------------------------------------------- restricted ----

fn docker_enabled() -> bool {
    let on = std::env::var("EXPOSITION_DOCKER_TESTS").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("SKIPPED: set EXPOSITION_DOCKER_TESTS=1 (Docker Desktop with local r-base and julia images)");
    }
    on
}

fn image(language: BatchLanguage) -> String {
    match language {
        BatchLanguage::R => std::env::var("EXPOSITION_TEST_R_IMAGE").unwrap_or_else(|_| "r-base:4.6.2".into()),
        _ => std::env::var("EXPOSITION_TEST_JULIA_IMAGE").unwrap_or_else(|_| "julia:1.13".into()),
    }
}

fn image_id(image: &str) -> String {
    let out = std::process::Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", image])
        .output()
        .expect("docker image inspect");
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(id.starts_with("sha256:"), "{image} must be present locally: {id}");
    id
}

/// With the plan bound to the language image's identity (as a per-runner
/// image binding provides), restricted R and Julia steps run in that image
/// and record it.
#[test]
#[ignore = "needs Docker Desktop and the local r-base/julia images; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_r_and_julia_workflows_run_in_the_approved_language_image() {
    if !docker_enabled() {
        return;
    }
    for language in [BatchLanguage::R, BatchLanguage::Julia] {
        let (r_image, julia_image) = (image(BatchLanguage::R), image(BatchLanguage::Julia));
        let r = real((Some(PYTHON_IMAGE), Some(&r_image), Some(&julia_image)), false);
        let lang_image = image(language);
        let id_now = image_id(&lang_image);
        let (service, _) =
            service(&r.project, Some(r.execution.clone()), r.runner.clone(), (Some(&lang_image), Some(&id_now)));
        let (definition, figure, log) = match language {
            BatchLanguage::R => {
                (definition(RESEARCH_R_BATCH, "r", Some(R_CODE), None), "figure.png", "R rows 2 total 6")
            }
            _ => (
                definition(RESEARCH_JULIA_BATCH, "julia", Some(JULIA_SCRIPT), None),
                "figure.svg",
                "Julia rows 2 total 6.0",
            ),
        };
        let wf = save(&r.project, definition);
        let plan = plan(&service, &wf);
        assert_eq!(plan.execution.restricted_image_id.as_deref(), Some(id_now.as_str()));
        let run = run_to_end(&service, &plan, Duration::from_secs(240));
        let manifest = assert_produced(&r, &run, figure, log);
        match manifest.execution.unwrap() {
            RunExecution::RestrictedContainer { image, image_id, network, .. } => {
                assert_eq!(
                    (image.as_str(), image_id.as_deref(), network.as_str()),
                    (lang_image.as_str(), Some(id_now.as_str()), "none")
                );
            }
            other => panic!("{other:?}"),
        }
    }
}

/// Today's application binds only the default (Python) image into a plan: a
/// restricted R/Julia step is then refused at dispatch (never run in another
/// image, never natively) with an accurate reason.
#[test]
#[ignore = "needs Docker Desktop and the local r-base/julia/python images; run with EXPOSITION_DOCKER_TESTS=1 -- --ignored"]
fn restricted_r_and_julia_steps_bound_to_the_python_image_are_refused() {
    if !docker_enabled() {
        return;
    }
    let (r_image, julia_image) = (image(BatchLanguage::R), image(BatchLanguage::Julia));
    let r = real((Some(PYTHON_IMAGE), Some(&r_image), Some(&julia_image)), true);
    let python_id = image_id(PYTHON_IMAGE);
    let (service, _) =
        service(&r.project, Some(r.execution.clone()), r.runner.clone(), (Some(PYTHON_IMAGE), Some(&python_id)));
    for (operation, language) in [(RESEARCH_R_BATCH, "r"), (RESEARCH_JULIA_BATCH, "julia")] {
        let wf = save(&r.project, definition(operation, language, Some("cat('ran')\n"), None));
        let run = run_to_end(&service, &plan(&service, &wf), Duration::from_secs(60));
        assert_eq!(run.state, WorkflowRunState::Failed);
        let attempt = only_attempt(&run);
        assert_eq!(attempt.code.as_deref(), Some("workflow.restrictedUnavailable"), "{:?}", attempt.message);
        assert!(attempt.operation.is_none(), "nothing was dispatched");
        let message = attempt.message.clone().unwrap_or_default();
        assert!(message.contains(&python_id[..19]) && message.contains("Nothing runs natively"), "{message}");
    }
    assert!(!r.project.root.join(".exposition").join("runs").exists(), "no research run was started");
}
