//! Lane D (reproducibility) tests with fake services: environment capture
//! per adapter, run manifests (determinism, secret- and path-freedom), and
//! reproductions (pinned inputs, changed-input refusal, drift notices, the
//! normal start with the Reproduce cause, same seeds).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use exposition_execution::credentials::Redactor;
use exposition_graph::workflow::presets::{FORMAL_AUDIT, NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use exposition_graph::workflow::registry::Containment;
use exposition_graph::workflow::{
    CodeLanguage, OutcomeKind, PortSource, StepOperation, StepPayload, StepPort, WorkflowConnection,
    WorkflowDefinition, WorkflowOutcome, WorkflowStep,
};
use exposition_graph::{
    EvidenceId, GraphConfig, GraphService, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext, WorkflowId,
    WorkflowRunId,
};
use exposition_lean::types::{AuditedDeclaration, DeclarationVerdict, LeanAuditRecord, LeanStatus};
use exposition_research::ResearchError;
use exposition_research::batch::{
    BatchControl, BatchMode, BatchPortOutput, BatchRequest, BatchResult, BatchRunner, environment_keys as keys,
};

use crate::adapters::agent::{AgentReviewAdapter, AgentReviewer, ReviewModel, ReviewOutcome, ReviewRequest};
use crate::adapters::associate::AssociateAdapter;
use crate::adapters::batch::BatchAdapter;
use crate::adapters::lean::{LeanAuditAdapter, LeanAuditor};
use crate::adapters::{Adapters, StepFailure, runner_ids, sha256_hex};
use crate::environment::{self, WorkflowEnvironment, WorkflowEnvironmentImage};
use crate::manifest::{self, ManifestScrub};
use crate::model::*;
use crate::reproduce;
use crate::service::{WorkflowHost, WorkflowRunConfig, WorkflowRunService};

const IMAGE_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IMAGE_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const FAKE_KEY: &str = "sk-ant-api03-REPRO-FAKE-KEY-0123456789abcdefghij";
const MANIFEST: &str = r#"{"version": "1.1.0", "packagesDir": ".lake/packages", "packages": [{"url": "https://github.com/leanprover-community/mathlib4", "type": "git", "subDir": null, "scope": "leanprover-community", "rev": "0123456789abcdef0123456789abcdef01234567", "name": "mathlib", "manifestFile": "lake-manifest.json", "inputRev": "master", "inherited": false, "configFile": "lakefile.lean"}], "name": "Paper", "lakeDir": ".lake"}"#;

// ---------------------------------------------------------------- fakes ----

struct Host {
    root: PathBuf,
    image_id: Mutex<Option<String>>,
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

/// A batch runner that reports a fixed environment (plus the seed it got)
/// and writes `dataset.csv`, or fails before running.
struct FakeBatch {
    pairs: Mutex<Vec<(String, String)>>,
    current: Mutex<Vec<(String, String)>>,
    fail: Mutex<Option<String>>,
    log: Mutex<String>,
    seeds: Mutex<Vec<Option<u64>>>,
    counter: AtomicUsize,
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn image_pairs(id: &str, numpy: &str) -> Vec<(String, String)> {
    pairs(&[
        ("mode", "restricted container"),
        (keys::IMAGE, "python:3.13-slim"),
        (keys::IMAGE_ID, id),
        (keys::OS, "linux"),
        (keys::ARCH, "x86_64"),
        (keys::IMPLEMENTATION, "cpython"),
        (keys::PYTHON_VERSION, "3.13.9"),
        (keys::PACKAGES_SOURCE, "every installed distribution, listed inside the image by identity"),
        (keys::PACKAGE, "pip==25.2"),
        (keys::PACKAGE, &format!("numpy=={numpy}")),
    ])
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
        self.seeds.lock().unwrap().push(request.seed);
        if let Some(code) = self.fail.lock().unwrap().clone() {
            return Err(ResearchError::coded(&code, format!("refused before running ({FAKE_KEY})")));
        }
        let run = format!("run-{}", self.counter.fetch_add(1, Ordering::SeqCst));
        (control.on_dispatched)(&run);
        let rel = format!(".exposition/artifacts/{run}/dataset.csv");
        std::fs::create_dir_all(root.join(format!(".exposition/artifacts/{run}"))).unwrap();
        let content = format!("x\n{}\n", request.seed.unwrap_or(0));
        std::fs::write(root.join(&rel), &content).unwrap();
        let mut environment = self.pairs.lock().unwrap().clone();
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
            log: self.log.lock().unwrap().clone(),
            environment,
        })
    }
    fn current_environment(&self, _root: &Path, _mode: BatchMode) -> Vec<(String, String)> {
        self.current.lock().unwrap().clone()
    }
}

struct FakeLean {
    root: PathBuf,
}

impl LeanAuditor for FakeLean {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn audit(
        &self,
        path: &WorkspacePath,
        _timeout: Duration,
        _canceled: &(dyn Fn() -> bool + Sync),
    ) -> Result<LeanAuditRecord, StepFailure> {
        let text = std::fs::read_to_string(self.root.join(path.as_str())).unwrap();
        Ok(LeanAuditRecord {
            document: exposition_core::document::DocumentId("doc-1".into()),
            path: path.clone(),
            revision: 1,
            content_hash: sha256_hex(text.as_bytes()),
            toolchain: "leanprover/lean4:v4.35.0".into(),
            toolchain_version: Some("Lean (version 4.35.0, x86_64-w64-windows-gnu, commit abc, Release)".into()),
            status: LeanStatus::Unverified { revision: 1, toolchain: "x".into(), reason: "test".into() },
            declaration_count: 1,
            declarations: vec![AuditedDeclaration {
                name: "main_theorem".into(),
                kind: "theorem".into(),
                statement: "1 + 1 = 2".into(),
                range: None,
                line: None,
                axioms: Vec::new(),
                verdict: DeclarationVerdict::Checked,
            }],
            truncated: false,
            finished_at_ms: 0,
        })
    }
    fn record_evidence(
        &self,
        _path: &WorkspacePath,
        _content_hash: &str,
        names: &[String],
    ) -> Result<Vec<EvidenceId>, WorkflowRunError> {
        Ok(names.iter().map(|_| EvidenceId::mint()).collect())
    }
}

struct FakeReviewer {
    model: Mutex<ReviewModel>,
}

impl AgentReviewer for FakeReviewer {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn review(
        &self,
        _request: &ReviewRequest,
        _canceled: &(dyn Fn() -> bool + Sync),
        on_task: &(dyn Fn(&str) + Sync),
    ) -> Result<ReviewOutcome, StepFailure> {
        on_task("task-1");
        Ok(ReviewOutcome { text: format!("Consistent. ({FAKE_KEY})"), task: "task-1".into(), requests: 2 })
    }
    fn model(&self, task: Option<&str>) -> Option<ReviewModel> {
        let mut m = self.model.lock().unwrap().clone();
        if task.is_none() {
            m.served_by = None;
        }
        Some(m)
    }
}

// ---------------------------------------------------------------- setup ----

struct Fixture {
    _data: tempfile::TempDir,
    _project: tempfile::TempDir,
    root: PathBuf,
    host: Arc<Host>,
    graph: Arc<GraphService>,
    batch: Arc<FakeBatch>,
    reviewer: Arc<FakeReviewer>,
    adapters: Adapters,
    service: WorkflowRunService,
}

fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    let write = |rel: &str, text: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write("data/obs.csv", "x\n1\n2\n");
    write("lakefile.toml", "name = \"Paper\"\n");
    write("lean-toolchain", "leanprover/lean4:v4.35.0\n");
    write("lake-manifest.json", MANIFEST);
    write("Proofs/Main.lean", "theorem main_theorem : 1 + 1 = 2 := rfl\n");
    let graph = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    assert!(graph.attach(&root).is_usable());
    let batch = Arc::new(FakeBatch {
        pairs: Mutex::new(image_pairs(IMAGE_A, "2.3.1")),
        current: Mutex::new(Vec::new()),
        fail: Mutex::new(None),
        log: Mutex::new(String::new()),
        seeds: Mutex::new(Vec::new()),
        counter: AtomicUsize::new(1),
    });
    let reviewer = Arc::new(FakeReviewer {
        model: Mutex::new(ReviewModel {
            provider: "anthropic".into(),
            configured: "claude-opus-5-5".into(),
            served_by: Some("claude-opus-5-5-20261101".into()),
            effort: Some("medium".into()),
        }),
    });
    let mut adapters = Adapters::new();
    let runner: Arc<dyn BatchRunner> = batch.clone();
    adapters.insert(
        runner_ids::RESTRICTED_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::Restricted)),
    );
    adapters
        .insert(runner_ids::NATIVE_PYTHON.into(), Arc::new(BatchAdapter::new(Some(runner), BatchMode::TrustedNative)));
    let auditor: Arc<dyn LeanAuditor> = Arc::new(FakeLean { root: root.clone() });
    adapters.insert(runner_ids::LEAN_AUDIT.into(), Arc::new(LeanAuditAdapter { auditor: Some(auditor.clone()) }));
    adapters.insert(runner_ids::GRAPH_PROPOSAL.into(), Arc::new(AssociateAdapter));
    let r: Arc<dyn AgentReviewer> = reviewer.clone();
    adapters.insert(runner_ids::READ_ONLY_AGENT.into(), Arc::new(AgentReviewAdapter { reviewer: Some(r) }));
    let host = Arc::new(Host { root: root.clone(), image_id: Mutex::new(Some(IMAGE_A.into())), graph: graph.clone() });
    let h: Arc<dyn WorkflowHost> = host.clone();
    let service = WorkflowRunService::new(WorkflowRunConfig {
        database: None,
        host: h,
        adapters: adapters.clone(),
        lean: Some(auditor),
        research: None,
    })
    .unwrap();
    Fixture { _data: data, _project: project, root, host, graph, batch, reviewer, adapters, service }
}

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn step(id: &str, operation: &str, payload: StepPayload) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Step {id}"),
        operation: StepOperation { id: operation.into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: Some(payload),
        composite: None,
        condition: None,
        repeat: None,
    }
}

/// One restricted Python step `fit` reading `data`, realizing `dataset`.
fn python(native: bool) -> WorkflowDefinition {
    let mut def = draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        data: Some(path("data/obs.csv")),
        ..Default::default()
    })
    .unwrap();
    let operation = if native { "research.nativePython" } else { "research.pythonBatch" };
    def.steps =
        vec![step("fit", operation, StepPayload::Code { language: CodeLanguage::Python, source: "print(1)\n".into() })];
    if native {
        def.ceiling.execution = exposition_graph::workflow::ExecutionCeiling::TrustedNative;
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

fn review() -> WorkflowDefinition {
    let mut def = python(false);
    def.ceiling.model_requests = true;
    def.steps = vec![step("review", "agent.review", StepPayload::Prompt { text: "Summarize.".into() })];
    def.connections = vec![WorkflowConnection::Data {
        from: PortSource::Input { input: "data".into() },
        to: StepPort { step: "review".into(), port: "material".into() },
    }];
    def.outcomes = vec![WorkflowOutcome {
        id: "report".into(),
        kind: OutcomeKind::InvestigationReport,
        label: "Report".into(),
        from: StepPort { step: "review".into(), port: "report".into() },
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

    fn start(&self, plan: WorkflowRunPlan) -> WorkflowRun {
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let run = self
            .service
            .start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash })
            .unwrap();
        assert!(self.service.wait_idle(Duration::from_secs(20)), "run did not finish");
        self.service.get(&run.id).unwrap()
    }

    fn run(&self, id: &WorkflowId) -> WorkflowRun {
        self.start(self.service.plan(id, None).unwrap())
    }

    fn current(&self) -> impl Fn(&WorkflowRunPlan, &WorkflowPlanStep) -> Option<WorkflowEnvironment> + '_ {
        move |plan, step| reproduce::current_environment(&self.root, &self.adapters, plan, step)
    }

    fn reproduce(&self, of: &WorkflowRunId) -> Result<reproduce::WorkflowReproducePlan, WorkflowRunError> {
        self.service.reproduce_plan(of, &self.current())
    }
}

fn env_of<'a>(run: &'a WorkflowRun, step: &str) -> &'a WorkflowEnvironment {
    run.steps.iter().find(|s| s.step == step).unwrap().attempts.last().unwrap().environment.as_ref().unwrap()
}

// ------------------------------------------------------------ capture ----

#[test]
fn a_restricted_batch_records_image_interpreter_packages_and_the_seed_it_was_given() {
    let f = fixture();
    let run = f.run(&f.save(python(false)));
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let env = env_of(&run, "fit");
    assert_eq!((env.runner.as_str(), env.containment), ("restrictedPython", Containment::RestrictedContainer));
    assert_eq!(env.image, Some(WorkflowEnvironmentImage { name: "python:3.13-slim".into(), id: Some(IMAGE_A.into()) }));
    assert_eq!((env.os.as_str(), env.arch.as_str()), ("linux", "x86_64"), "the image's platform");
    assert_eq!(env.interpreter.as_ref().and_then(|i| i.version.as_deref()), Some("3.13.9"));
    let names: Vec<&str> = env.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["numpy", "pip"]);
    assert_eq!(env.packages_digest.as_deref(), Some(environment::packages_digest(&env.packages).as_str()));
    let expected = environment::step_seed(run.id.as_str(), "fit", 0);
    assert_eq!(env.seed, Some(expected));
    assert_eq!(f.batch.seeds.lock().unwrap().as_slice(), &[Some(expected)], "the batch was given that seed");
    assert_eq!(env.app_version, environment::APP_VERSION);
}

#[test]
fn a_batch_refused_before_running_still_records_its_seed_and_the_approved_image() {
    let f = fixture();
    *f.batch.fail.lock().unwrap() = Some("research.imageChanged".into());
    let run = f.run(&f.save(python(false)));
    assert_eq!(run.state, WorkflowRunState::Failed);
    let env = env_of(&run, "fit");
    assert!(env.seed.is_some());
    assert_eq!(env.os, "unknown", "the image's platform is not guessed from the host");
    assert!(env.notes.iter().any(|n| n.contains(IMAGE_A)), "{:?}", env.notes);
    assert!(env.notes.iter().any(|n| n.contains("did not report its environment")), "{:?}", env.notes);
}

#[test]
fn a_native_batch_records_the_packages_the_environment_report_lists() {
    let f = fixture();
    *f.batch.pairs.lock().unwrap() = pairs(&[
        ("mode", "trusted native"),
        ("interpreter", r"C:\Users\someone\AppData\Local\Programs\Python\python.exe"),
        (keys::PYTHON_VERSION, "3.14.0"),
        (keys::OS, "windows"),
        (keys::ARCH, "x86_64"),
        (keys::PACKAGES_SOURCE, "the research environment report (the packages it checks)"),
        (keys::PACKAGE, "numpy==2.3.4"),
        (keys::PACKAGE, "jupyter_client==8.10.0"),
    ]);
    let run = f.run(&f.save(python(true)));
    let env = env_of(&run, "fit");
    assert_eq!(env.containment, Containment::NativeAccount);
    assert_eq!(env.image, None);
    assert_eq!(env.interpreter.as_ref().and_then(|i| i.version.as_deref()), Some("3.14.0"));
    assert_eq!(env.packages.len(), 2);
    assert!(env.notes.iter().any(|n| n.contains("environment report")), "{:?}", env.notes);
    let json = serde_json::to_string(env).unwrap();
    assert!(!json.contains("someone") && !json.contains("python.exe"), "no interpreter path: {json}");
}

#[test]
fn a_lean_audit_records_its_toolchain_and_the_lake_manifest() {
    let f = fixture();
    let def = draft(&WorkflowPresetRequest {
        preset: FORMAL_AUDIT.into(),
        lean_file: Some(path("Proofs/Main.lean")),
        ..Default::default()
    })
    .unwrap();
    let run = f.run(&f.save(def));
    let audit = &run.plan.steps[0];
    assert_eq!(audit.runner, runner_ids::LEAN_AUDIT);
    let env = env_of(&run, &audit.step);
    let toolchain = env.toolchain.as_ref().expect("toolchain");
    assert_eq!(toolchain.name, "leanprover/lean4:v4.35.0");
    assert_eq!(toolchain.version.as_deref(), Some("4.35.0"));
    assert_eq!(toolchain.manifest_digest.as_deref(), Some(sha256_hex(MANIFEST.as_bytes()).as_str()));
    assert_eq!(env.packages.len(), 1);
    assert_eq!(env.packages[0].version, "0123456789abcdef0123456789abcdef01234567");
    assert!(env.notes.iter().any(|n| n.contains("mathlib revision 0123456789ab")), "{:?}", env.notes);
    // The association step (an application service) records its base environment.
    let associate = env_of(&run, &run.plan.steps[1].step);
    assert_eq!(associate.containment, Containment::ApplicationService);
}

#[test]
fn an_agent_review_records_the_provider_and_the_model_that_served_it() {
    let f = fixture();
    let run = f.run(&f.save(review()));
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let env = env_of(&run, "review");
    let model = env.model.as_ref().expect("model");
    assert_eq!((model.provider.as_str(), model.model.as_str()), ("anthropic", "claude-opus-5-5-20261101"));
    assert!(env.notes.iter().any(|n| n.contains("Configured model claude-opus-5-5;")), "{:?}", env.notes);
}

// ----------------------------------------------------------- manifests ----

fn scrub_with<'a>(redactor: &'a Redactor, root: &Path) -> ManifestScrub<'a> {
    ManifestScrub {
        redact: Box::leak(Box::new(|t: &str| redactor.redact(t))),
        folders: vec![(root.to_path_buf(), "<project>".into())],
    }
}

#[test]
fn a_manifest_is_deterministic_complete_and_free_of_secrets_and_user_paths() {
    let f = fixture();
    let root_text = f.root.display().to_string();
    *f.batch.log.lock().unwrap() = format!("token {FAKE_KEY}\nwrote {root_text}\\data\\x.csv\n");
    let ok = f.run(&f.save(python(false)));
    *f.batch.fail.lock().unwrap() = Some("research.batchFailed".into());
    let failed = f.run(&f.save(python(false)));
    let reviewed = f.run(&f.save(review()));
    // The host environment as the application captures it, with a planted
    // credential (the application's redactor is built the same way).
    let host = std::env::vars().chain([("EXPOSITION_REPRO_TEST_API_KEY".to_owned(), FAKE_KEY.to_owned())]);
    let redactor = Redactor::from_host_env(host);
    let scrub = scrub_with(&redactor, &f.root);
    for run in [&ok, &failed, &reviewed] {
        let m1 = f.service.run_manifest(&run.id, &scrub).unwrap();
        let m2 = f.service.run_manifest(&run.id, &scrub).unwrap();
        let (a, b) = (manifest::manifest_json(&m1), manifest::manifest_json(&m2));
        assert_eq!(a, b, "the same run gives identical bytes");
        assert!(!a.contains(FAKE_KEY) && !a.contains("REPRO-FAKE"), "no secret: {a}");
        assert!(!a.to_lowercase().contains(&root_text.to_lowercase()), "no project path: {a}");
        assert!(!a.contains("\\Users\\") && !a.contains("/Users/"), "no user path: {a}");
        assert!(!a.contains("[stdout]") && !a.contains("token"), "no logs: {a}");
        // Keys are sorted at every level.
        let value: serde_json::Value = serde_json::from_str(&a).unwrap();
        fn check_sorted(v: &serde_json::Value) {
            if let Some(map) = v.as_object() {
                let keys: Vec<&String> = map.keys().collect();
                let mut sorted = keys.clone();
                sorted.sort();
                assert_eq!(keys, sorted);
                map.values().for_each(check_sorted);
            } else if let Some(items) = v.as_array() {
                items.iter().for_each(check_sorted);
            }
        }
        check_sorted(&value);
        assert_eq!(value["schema"], manifest::MANIFEST_SCHEMA);
        assert_eq!(value["approvalHash"], run.plan.approval_hash.as_str());
        assert_eq!(value["definitionHash"], run.definition_hash.as_str());
        assert_eq!(value["cause"]["kind"], "manual");
        assert_eq!(value["inputs"][0]["path"], "data/obs.csv");
        assert_eq!(value["inputs"][0]["sha256"], sha256_hex(b"x\n1\n2\n"));
        assert!(value["steps"][0]["attempts"][0]["environment"]["runner"].is_string(), "{a}");
        assert!(value["budgets"]["maxSteps"].is_number() && value["usage"]["steps"].is_number());
    }
    let ok_manifest: serde_json::Value =
        serde_json::from_str(&manifest::manifest_json(&f.service.run_manifest(&ok.id, &scrub).unwrap())).unwrap();
    let out = &ok_manifest["outputs"][0];
    assert_eq!(out["port"], "dataset");
    assert!(out["sha256"].as_str().is_some_and(|s| s.len() == 64));
    assert_eq!(out["provenance"]["step"], "fit");
    assert_eq!(ok_manifest["steps"][0]["attempts"][0]["environment"]["seed"], env_of(&ok, "fit").seed.unwrap());
    let report: serde_json::Value =
        serde_json::from_str(&manifest::manifest_json(&f.service.run_manifest(&reviewed.id, &scrub).unwrap())).unwrap();
    assert_eq!(report["outputs"][0]["kind"], "report");
    assert!(report.to_string().find("Consistent").is_none(), "the report's prose stays out of the manifest");

    // Export: canonical bytes to a user-chosen file.
    let out_dir = tempfile::tempdir().unwrap();
    let dest = std::fs::canonicalize(out_dir.path()).unwrap().join("run-manifest.json");
    let export = f.service.export_run_manifest(&ok.id, &dest, &scrub).unwrap();
    let written = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(written, manifest::manifest_json(&f.service.run_manifest(&ok.id, &scrub).unwrap()));
    assert_eq!(export.sha256, sha256_hex(written.as_bytes()));
}

// -------------------------------------------------------- reproductions ----

#[test]
fn a_reproduction_pins_every_input_and_starts_with_its_cause_and_the_original_seeds() {
    let f = fixture();
    let id = f.save(python(false));
    let original = f.run(&id);
    *f.batch.current.lock().unwrap() = image_pairs(IMAGE_A, "2.3.1");
    let reproduction = f.reproduce(&original.id).unwrap();
    assert_eq!(reproduction.reproduces, original.id);
    assert!(reproduction.drift.is_empty(), "{:?}", reproduction.drift);
    let plan = reproduction.plan;
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert_eq!((plan.workflow.clone(), plan.revision), (original.workflow.clone(), original.revision));
    for input in &plan.inputs {
        let recorded = original.plan.inputs.iter().find(|i| i.id == input.id).unwrap();
        assert!(input.pinned, "{} is pinned", input.id);
        assert_eq!(input.sha256, recorded.sha256);
    }
    assert!(plan.notices.iter().any(|n| n.contains(&format!("reproduces run {}", original.id))), "{:?}", plan.notices);
    assert!(plan.notices.iter().any(|n| n.contains("EXPOSITION_SEED")), "{:?}", plan.notices);
    let copy = f.start(plan);
    assert_eq!(copy.cause, WorkflowRunCause::Reproduce { of: original.id.clone() });
    assert_eq!(copy.state, WorkflowRunState::Succeeded, "{:?}", copy.message);
    assert_eq!(env_of(&copy, "fit").seed, env_of(&original, "fit").seed, "the original's seed");
    // The reproduction reproduced the output (the fake batch writes its seed).
    let digest = |run: &WorkflowRun| manifest::build_manifest(run).outputs[0].sha256.clone();
    assert_eq!(digest(&copy), digest(&original));
}

#[test]
fn a_changed_input_blocks_the_reproduction_and_its_start() {
    let f = fixture();
    let original = f.run(&f.save(python(false)));
    // An approved reproduction plan, then the file changes: start is refused.
    let approved = f.reproduce(&original.id).unwrap().plan;
    std::fs::write(f.root.join("data/obs.csv"), "x\n9\n").unwrap();
    let e = f
        .service
        .start(&StartWorkflowRunRequest { plan: approved.plan.clone(), approval_hash: approved.approval_hash.clone() })
        .unwrap_err();
    assert_eq!(e.code, "workflow.planStale", "{e:?}");
    // A new reproduction plan carries the normal blocker.
    let plan = f.reproduce(&original.id).unwrap().plan;
    let blocker = plan.blockers.iter().find(|b| b.code == "workflow.inputChanged").expect("inputChanged blocker");
    assert_eq!(blocker.input.as_deref(), Some("data"));
    assert!(blocker.message.contains(&format!("since run {} read it", original.id)), "{}", blocker.message);
    let e =
        f.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap_err();
    assert_eq!(e.code, "workflow.inputChanged");
    // Restoring the recorded content unblocks it.
    std::fs::write(f.root.join("data/obs.csv"), "x\n1\n2\n").unwrap();
    assert!(f.reproduce(&original.id).unwrap().plan.blockers.is_empty());
}

#[test]
fn drift_in_image_identity_packages_and_model_is_shown_before_approval() {
    let f = fixture();
    let original = f.run(&f.save(python(false)));
    // Nothing changed: no drift.
    *f.batch.current.lock().unwrap() = image_pairs(IMAGE_A, "2.3.1");
    let r = f.reproduce(&original.id).unwrap();
    assert!(r.drift.is_empty(), "{:?}", r.drift);
    let plan = r.plan;
    assert!(plan.notices.iter().any(|n| n.starts_with("No environment drift")), "{:?}", plan.notices);
    // The image was re-tagged (new identity, newer numpy).
    *f.host.image_id.lock().unwrap() = Some(IMAGE_B.into());
    *f.batch.current.lock().unwrap() = image_pairs(IMAGE_B, "2.4.0");
    let r = f.reproduce(&original.id).unwrap();
    let aspects: Vec<(Option<&str>, &str)> = r.drift.iter().map(|d| (d.step.as_deref(), d.aspect.as_str())).collect();
    assert_eq!(aspects, [(Some("fit"), "image"), (Some("fit"), "packages")]);
    assert_eq!(r.drift[0].recorded.as_deref(), Some(format!("python:3.13-slim@{IMAGE_A}").as_str()));
    assert_eq!(r.drift[0].current.as_deref(), Some(format!("python:3.13-slim@{IMAGE_B}").as_str()));
    let plan = r.plan;
    let drift = plan.notices.iter().find(|n| n.starts_with("Environment drift")).expect("drift notice").clone();
    assert!(drift.contains("same name, different image") && drift.contains("bbbbbbbbbbbb"), "{drift}");
    assert!(drift.contains("numpy 2.3.1 → 2.4.0"), "{drift}");
    assert!(plan.blockers.is_empty(), "drift is shown, not a blocker: {:?}", plan.blockers);
    // Docker unavailable: the current environment is unknown, not guessed.
    f.batch.current.lock().unwrap().clear();
    let r = f.reproduce(&original.id).unwrap();
    let aspects: Vec<&str> = r.drift.iter().map(|d| d.aspect.as_str()).collect();
    assert_eq!(aspects, ["unknown"]);
    // The image's package list is unknown (never listed): said, not guessed.
    *f.batch.current.lock().unwrap() = pairs(&[(keys::IMAGE, "python:3.13-slim"), (keys::IMAGE_ID, IMAGE_B)]);
    let r = f.reproduce(&original.id).unwrap();
    let packages = r.drift.iter().find(|d| d.aspect == "packages").expect("packages drift");
    assert!(packages.recorded.is_some() && packages.current.is_none(), "{packages:?}");
    let plan = r.plan;
    assert!(plan.notices.iter().any(|n| n.contains("not known until the step runs")), "{:?}", plan.notices);

    // Model drift of a review step.
    let reviewed = f.run(&f.save(review()));
    f.reviewer.model.lock().unwrap().configured = "claude-opus-6".into();
    let plan = f.reproduce(&reviewed.id).unwrap().plan;
    assert!(
        plan.notices.iter().any(|n| n.contains("model anthropic/claude-opus-5-5-20261101 → anthropic/claude-opus-6")),
        "{:?}",
        plan.notices
    );
}

#[test]
fn reproducing_needs_a_finished_run_of_the_open_project() {
    let f = fixture();
    let e = f.reproduce(&WorkflowRunId::mint()).unwrap_err();
    assert_eq!(e.code, "workflow.unknownRun");
}
