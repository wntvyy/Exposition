//! Environment capture, manifests, and reproductions against the real
//! services (M9 lane D): the research batch sandbox (restricted Docker and
//! trusted native Python) and the Lean service.
//!
//! - Restricted (Docker) tests are `#[ignore]`d: they need Docker Desktop
//!   with the local image `python:3.13-slim` (never pulled). Run with
//!   `EXPOSITION_DOCKER_TESTS=1 cargo test -p exposition-workflow --test environment_real -- --ignored`.
//! - The native test runs when a Python interpreter is discovered.
//! - The Lean test runs when `leanprover/lean4:v4.35.0` is installed through
//!   elan (required with `EXPOSITION_REQUIRE_LEAN=1`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use exposition_core::path::WorkspacePath;
use exposition_execution::env::HostEnv;
use exposition_execution::{DockerSettings, ExecutionConfig, ExecutionService, ExecutionSettings};
use exposition_graph::workflow::presets::{FORMAL_AUDIT, NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use exposition_graph::workflow::{
    CodeLanguage, ExecutionCeiling, OutcomeKind, PortSource, StepOperation, StepPayload, StepPort, WorkflowConnection,
    WorkflowDefinition, WorkflowOutcome, WorkflowStep,
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
use exposition_workflow::environment::{self, WorkflowEnvironment};
use exposition_workflow::manifest::{self, ManifestScrub};
use exposition_workflow::reproduce;
use exposition_workflow::{
    StartWorkflowRunRequest, WorkflowHost, WorkflowRun, WorkflowRunCause, WorkflowRunConfig, WorkflowRunError,
    WorkflowRunPlan, WorkflowRunService, WorkflowRunState,
};
use exposition_workspace::DocumentService;

const IMAGE: &str = "python:3.13-slim";
const FAKE_KEY: &str = "sk-ant-api03-FAKEFAKEFAKE-env-capture-0123456789";
/// The batch writes the seed it was given and its interpreter version.
const CODE: &str = "import json, os, sys\nwith open(os.path.join(os.environ['EXPOSITION_OUTPUT_DIR'], 'dataset.json'), 'w') as f:\n    json.dump({'seed': os.environ.get('EXPOSITION_SEED'), 'python': '.'.join(map(str, sys.version_info[:3])), 'key': 'ANTHROPIC_API_KEY' in os.environ}, f)\n";

fn docker_enabled() -> bool {
    let on = std::env::var("EXPOSITION_DOCKER_TESTS").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("SKIPPED: set EXPOSITION_DOCKER_TESTS=1 (Docker Desktop with local {IMAGE})");
    }
    on
}

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
    data: PathBuf,
    execution: ExecutionService,
    graph: Arc<GraphService>,
    runner: Arc<dyn BatchRunner>,
    service: WorkflowRunService,
}

fn fixture(image: Option<&str>, trusted: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let root = base.join("project");
    std::fs::create_dir_all(root.join("data")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    std::fs::write(root.join("data/obs.csv"), "x\n1\n2\n").unwrap();
    let data = base.join("appdata");
    // The host environment carries a credential; nothing may pass it on.
    let host = HostEnv::from_pairs(
        std::env::vars()
            .filter(|(k, _)| k != "ANTHROPIC_API_KEY")
            .chain([("ANTHROPIC_API_KEY".to_string(), FAKE_KEY.to_string())]),
    );
    let execution = ExecutionService::with_config(ExecutionConfig {
        data_dir: data.clone(),
        host_env: host,
        settings: Some(ExecutionSettings {
            docker: DockerSettings { image: image.map(str::to_owned), ..Default::default() },
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
    let runner = batch_runner(research, execution.clone());
    let graph = GraphService::new(GraphConfig { data_dir: data.join("graph") });
    assert!(graph.attach(&root).is_usable());
    let mut adapters = Adapters::new();
    adapters.insert(
        runner_ids::RESTRICTED_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::Restricted)),
    );
    adapters.insert(
        runner_ids::NATIVE_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::TrustedNative)),
    );
    let service = WorkflowRunService::new(WorkflowRunConfig {
        database: Some(data.join("workflow-runs.sqlite3")),
        host: Arc::new(Host { root: root.clone(), graph: graph.clone(), execution: execution.clone() }),
        adapters,
        lean: None,
        research: None,
    })
    .unwrap();
    Fixture { _tmp: tmp, root, data, execution, graph, runner, service }
}

fn python(native: bool) -> WorkflowDefinition {
    let mut def = draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        data: Some(WorkspacePath::parse("data/obs.csv").unwrap()),
        ..Default::default()
    })
    .unwrap();
    def.steps = vec![WorkflowStep {
        id: "fit".into(),
        label: "Fit".into(),
        operation: StepOperation {
            id: if native { "research.nativePython" } else { "research.pythonBatch" }.into(),
            version: 1,
        },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Code { language: CodeLanguage::Python, source: CODE.into() }),
        composite: None,
        condition: None,
        repeat: None,
    }];
    if native {
        def.ceiling.execution = ExecutionCeiling::TrustedNative;
    }
    def.connections = vec![WorkflowConnection::Data {
        from: PortSource::Input { input: "data".into() },
        to: StepPort { step: "fit".into(), port: "data".into() },
    }];
    def.outcomes = vec![WorkflowOutcome {
        id: "o-fit".into(),
        kind: OutcomeKind::TransformedDataset,
        label: "Dataset".into(),
        from: StepPort { step: "fit".into(), port: "dataset".into() },
        optional: false,
    }];
    def
}

impl Fixture {
    fn save(&self, def: WorkflowDefinition) -> WorkflowId {
        self.graph
            .save_workflow(
                &Origin::User,
                &SaveWorkflowRequest { id: None, expected_revision: None, definition: def },
                &ValidationContext::default(),
            )
            .unwrap()
            .id
    }

    fn start(&self, plan: WorkflowRunPlan) -> WorkflowRun {
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let run = self
            .service
            .start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash })
            .unwrap();
        assert!(self.service.wait_idle(Duration::from_secs(300)), "run did not finish");
        self.service.get(&run.id).unwrap()
    }

    /// The content the run's `dataset` output holds.
    fn dataset(&self, run: &WorkflowRun) -> serde_json::Value {
        let out = &manifest::build_manifest(run).outputs[0];
        let path = out.path.as_ref().unwrap();
        let file = path.components().fold(self.root.clone(), |p, c| p.join(c));
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
    }

    fn scrubbed_manifest(&self, run: &WorkflowRun) -> String {
        let redact = |t: &str| self.execution.redact(t);
        let scrub = ManifestScrub { redact: &redact, folders: vec![(self.root.clone(), "<project>".into())] };
        manifest::manifest_json(&self.service.run_manifest(&run.id, &scrub).unwrap())
    }

    fn reproduce(&self, run: &WorkflowRun) -> reproduce::WorkflowReproducePlan {
        let sources = reproduce::environment_sources(Some(self.runner.clone()), None);
        let current = |plan: &WorkflowRunPlan, step: &exposition_workflow::WorkflowPlanStep| {
            reproduce::current_environment(&self.root, &sources, plan, step)
        };
        self.service.reproduce_plan(&run.id, &current).unwrap()
    }

    fn probe_jobs(&self) -> usize {
        self.execution.list().iter().filter(|j| j.info.label.starts_with("Restricted image environment probe")).count()
    }
}

fn env_of(run: &WorkflowRun) -> &WorkflowEnvironment {
    run.steps[0].attempts.last().unwrap().environment.as_ref().expect("an environment is recorded")
}

#[test]
#[ignore = "needs Docker with the local image python:3.13-slim (EXPOSITION_DOCKER_TESTS=1)"]
fn restricted_run_records_the_image_packages_and_seed_and_reproduces_with_the_same_seed() {
    if !docker_enabled() {
        return;
    }
    let f = fixture(Some(IMAGE), false);
    let id = f.save(python(false));
    let run = f.start(f.service.plan(&id, None).unwrap());
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?} {:?}", run.message, run.steps[0].attempts);
    let env = env_of(&run);
    println!("restricted environment: {}", serde_json::to_string_pretty(env).unwrap());
    let image = env.image.as_ref().expect("image");
    assert_eq!(image.name, IMAGE);
    assert_eq!(image.id, run.plan.execution.restricted_image_id, "the approved identity");
    assert_eq!(env.os, "linux", "the image's platform, listed inside it");
    let python = env.interpreter.as_ref().and_then(|i| i.version.clone()).unwrap();
    assert!(python.starts_with("3.13."), "{python}");
    assert!(env.packages.iter().any(|p| p.name == "pip"), "{:?}", env.packages);
    assert_eq!(env.packages_digest.as_deref(), Some(environment::packages_digest(&env.packages).as_str()));
    let seed = environment::step_seed(run.id.as_str(), "fit", 0);
    assert_eq!(env.seed, Some(seed));
    let out = f.dataset(&run);
    assert_eq!(out["seed"], seed.to_string(), "the batch saw EXPOSITION_SEED");
    assert_eq!(out["python"], python, "the recorded version is the one that ran");
    assert_eq!(out["key"], false, "no credential reached the container");
    assert_eq!(f.probe_jobs(), 1, "one probe for the image identity");

    // The manifest names the environment, never the key or the project path.
    let json = f.scrubbed_manifest(&run);
    assert!(json.contains("\"packagesDigest\"") && json.contains(&format!("\"seed\": {seed}")), "{json}");
    assert!(!json.contains(FAKE_KEY), "no secret in the manifest");
    assert!(!json.to_lowercase().contains(&f.root.display().to_string().to_lowercase()), "no project path");

    // Reproduce: no drift (same identity, cached packages), same seed and output.
    let r = f.reproduce(&run);
    assert!(r.drift.is_empty(), "{:?}", r.drift);
    assert!(r.plan.inputs.iter().all(|i| i.pinned));
    let copy = f.start(r.plan);
    assert_eq!(copy.cause, WorkflowRunCause::Reproduce { of: run.id.clone() });
    assert_eq!(copy.state, WorkflowRunState::Succeeded, "{:?}", copy.message);
    assert_eq!(env_of(&copy).seed, Some(seed), "the original's seed");
    assert_eq!(env_of(&copy).packages_digest, env.packages_digest);
    assert_eq!(f.dataset(&copy), out, "the reproduction reproduced the output");
    assert_eq!(f.probe_jobs(), 1, "the identity's package list came from the cache");
    assert!(
        f.data.join("research").join("image-environments").read_dir().unwrap().count() == 1,
        "cached per identity in the app data folder"
    );
}

#[test]
fn native_run_records_the_discovered_interpreter_and_packages_without_probing_again() {
    let f = fixture(None, true);
    if let Err(reason) = f.runner.availability(&f.root, BatchMode::TrustedNative) {
        eprintln!("SKIPPED: no Python interpreter for trusted native batches: {reason}");
        return;
    }
    let id = f.save(python(true));
    let run = f.start(f.service.plan(&id, None).unwrap());
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?} {:?}", run.message, run.steps[0].attempts);
    let env = env_of(&run);
    println!("native environment: {}", serde_json::to_string_pretty(env).unwrap());
    assert_eq!(env.image, None);
    assert_eq!((env.os.as_str(), env.arch.as_str()), (std::env::consts::OS, std::env::consts::ARCH));
    let out = f.dataset(&run);
    let version = env.interpreter.as_ref().and_then(|i| i.version.clone()).unwrap();
    assert_eq!(out["python"], version, "the discovered version is the one that ran");
    assert_eq!(out["seed"], environment::step_seed(run.id.as_str(), "fit", 0).to_string());
    assert!(env.notes.iter().any(|n| n.contains("environment report")), "{:?}", env.notes);
    let json = serde_json::to_string(env).unwrap();
    assert!(!json.contains(":\\\\") && !json.contains(FAKE_KEY), "no interpreter path, no key: {json}");

    // The current environment comes from the cached report: no new job.
    let jobs = f.execution.list().len();
    let r = f.reproduce(&run);
    assert_eq!(f.execution.list().len(), jobs, "describing the current environment ran nothing");
    assert!(r.drift.is_empty(), "{:?}", r.drift);
}

// ---------------------------------------------------------------- Lean ----

mod lean {
    use std::time::Instant;

    use exposition_lean::{LeanConfig, LeanService};
    use exposition_workflow::adapters::associate::AssociateAdapter;
    use exposition_workflow::adapters::lean::{LeanAuditAdapter, LeanAuditor, LeanServiceAuditor};
    use exposition_workspace::WorkspaceService;

    use super::*;

    const TOOLCHAIN: &str = "leanprover/lean4:v4.35.0";

    fn lean_available() -> bool {
        let home = exposition_lean::toolchain::elan_home(|n| std::env::var(n).ok());
        let installed = matches!(
            exposition_lean::toolchain::resolve(home.as_deref(), TOOLCHAIN),
            exposition_lean::toolchain::Resolution::Installed(_)
        );
        if !installed {
            let message = format!("SKIPPED: Lean toolchain {TOOLCHAIN} is not installed through elan");
            assert!(std::env::var("EXPOSITION_REQUIRE_LEAN").as_deref() != Ok("1"), "{message}");
            eprintln!("{message}");
        }
        installed
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let name = entry.file_name();
            if name == ".lake" || name == "lake-manifest.json" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                copy_dir(&path, &to.join(&name));
            } else {
                std::fs::copy(&path, to.join(&name)).unwrap();
            }
        }
    }

    #[test]
    fn a_real_lean_audit_records_its_toolchain_and_lake_manifest() {
        if !lean_available() {
            return;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("workspace");
        copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lean-basic"), &project);
        std::fs::write(project.join("LeanBasic").join("Env.lean"), "theorem env_check : 2 + 2 = 4 := rfl\n").unwrap();
        let exec = ExecutionService::with_config(ExecutionConfig {
            data_dir: tmp.path().join("data"),
            host_env: HostEnv::capture(),
            settings: Some(ExecutionSettings::default()),
            too_broad_to_trust: Some(vec![]),
            tool_cache_dir: None,
        })
        .unwrap();
        let root = exec.register_workspace_root(&project).unwrap();
        exec.grant_trust(&root.display().to_string()).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let documents = Arc::new(DocumentService::new());
        let workspace = WorkspaceService::new(Arc::clone(&documents), tmp.path().join("app"));
        workspace.open_workspace(&root).unwrap();
        let (lean, actor) = {
            let _guard = runtime.enter();
            runtime.spawn(Arc::clone(&workspace).run());
            LeanService::new(exec.clone(), Arc::clone(&workspace), LeanConfig::default())
        };
        runtime.spawn(actor);
        let graph = GraphService::new(GraphConfig { data_dir: tmp.path().join("graph") });
        assert!(graph.attach(&root).is_usable());
        let auditor: Arc<dyn LeanAuditor> = Arc::new(LeanServiceAuditor {
            lean,
            workspace: Arc::clone(&workspace),
            documents,
            graph: graph.clone(),
            runtime: runtime.handle().clone(),
        });
        let mut adapters = Adapters::new();
        adapters.insert(runner_ids::LEAN_AUDIT.into(), Arc::new(LeanAuditAdapter { auditor: Some(auditor.clone()) }));
        adapters.insert(runner_ids::GRAPH_PROPOSAL.into(), Arc::new(AssociateAdapter));
        let service = WorkflowRunService::new(WorkflowRunConfig {
            database: None,
            host: Arc::new(Host { root: root.clone(), graph: graph.clone(), execution: exec.clone() }),
            adapters,
            lean: Some(auditor),
            research: None,
        })
        .unwrap();
        let def = draft(&WorkflowPresetRequest {
            preset: FORMAL_AUDIT.into(),
            lean_file: Some(WorkspacePath::parse("LeanBasic/Env.lean").unwrap()),
            ..Default::default()
        })
        .unwrap();
        let id = graph
            .save_workflow(
                &Origin::User,
                &SaveWorkflowRequest { id: None, expected_revision: None, definition: def },
                &ValidationContext::default(),
            )
            .unwrap()
            .id;
        let plan = service.plan(&id, None).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let started = Instant::now();
        let run =
            service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
        assert!(service.wait_idle(Duration::from_secs(400)), "the audit did not finish");
        let run = service.get(&run.id).unwrap();
        println!("Lean run {:?} in {:?}: {:?}", run.state, started.elapsed(), run.message);
        let audit = &run.steps[0];
        let env = audit.attempts.last().unwrap().environment.as_ref().expect("environment");
        println!("Lean environment: {}", serde_json::to_string_pretty(env).unwrap());
        assert_eq!(audit.state, exposition_workflow::WorkflowStepState::Succeeded, "{:?}", audit.attempts);
        let toolchain = env.toolchain.as_ref().expect("toolchain");
        assert_eq!(toolchain.name, TOOLCHAIN);
        assert_eq!(toolchain.version.as_deref(), Some("4.35.0"));
        let manifest = root.join("lake-manifest.json");
        match std::fs::read(&manifest) {
            Ok(bytes) => assert_eq!(
                toolchain.manifest_digest.as_deref(),
                Some(exposition_workflow::adapters::sha256_hex(&bytes).as_str()),
                "the digest of the manifest Lake wrote"
            ),
            Err(_) => {
                assert_eq!(toolchain.manifest_digest, None);
                assert!(env.notes.iter().any(|n| n.contains("no lake-manifest.json")), "{:?}", env.notes);
            }
        }
        drop(service);
        runtime.shutdown_timeout(Duration::from_secs(5));
    }
}
