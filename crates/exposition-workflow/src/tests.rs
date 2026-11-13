//! Runner tests with fake services (batch runner, Lean auditor, research
//! actions) over a real project graph and run store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::presets::{FORMAL_AUDIT, NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use exposition_graph::workflow::registry::{PortType, RESEARCH_NATIVE_PYTHON};
use exposition_graph::workflow::{
    CodeLanguage, ExecutionCeiling, InputSource, InputVersion, OutcomeKind, ParamValue, PortSource, StepOperation,
    StepPayload, StepPort, WorkflowConnection, WorkflowDefinition, WorkflowOutcome, WorkflowStep,
};
use exposition_graph::{
    EntityId, EvidenceId, GraphConfig, GraphService, OperationRef, Origin, SaveWorkflowRequest, SavedWorkflow,
    ValidationContext, WorkflowId,
};
use exposition_lean::types::{AuditedDeclaration, DeclarationVerdict, LeanAuditRecord, LeanStatus};
use exposition_research::ResearchError;
use exposition_research::batch::{
    BatchCode, BatchControl, BatchMode, BatchPortOutput, BatchRequest, BatchResult, BatchRunner,
};

use crate::adapters::agent::{AgentReviewAdapter, AgentReviewer, ReviewOutcome, ReviewRequest};
use crate::adapters::associate::AssociateAdapter;
use crate::adapters::batch::BatchAdapter;
use crate::adapters::lean::{LeanAuditAdapter, LeanAuditor};
use crate::adapters::research::{
    PreparedPromotion, PromotionAdapter, PromotionReview, ResearchActions, RetainAdapter, RetainedRecord,
    markers_present,
};
use crate::adapters::{
    AGENT_REVIEW_UNAVAILABLE, Adapters, StepAdapter, StepControl, StepFailure, StepRequest, StepResult, Unavailable,
    runner_ids, sha256_hex,
};
use crate::model::*;
use crate::service::{WorkflowHost, WorkflowRunConfig, WorkflowRunService};

// ---------------------------------------------------------------- fakes ----

type RootHook = Box<dyn Fn() + Send>;

struct Host {
    root: Mutex<Option<PathBuf>>,
    trusted: AtomicBool,
    image: Mutex<Option<String>>,
    /// The configured image's identity as the engine reports it.
    image_id: Mutex<Option<String>>,
    graph: Arc<GraphService>,
    /// Called whenever the runner asks for the open project.
    on_root: Mutex<Option<RootHook>>,
}

impl WorkflowHost for Host {
    fn project_root(&self) -> Option<PathBuf> {
        if let Some(hook) = self.on_root.lock().unwrap().as_ref() {
            hook();
        }
        self.root.lock().unwrap().clone()
    }
    fn trusted(&self, _root: &Path) -> bool {
        self.trusted.load(Ordering::SeqCst)
    }
    fn restricted(&self) -> (Option<String>, Option<String>) {
        (Some("docker".into()), self.image.lock().unwrap().clone())
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

type BatchBehavior =
    dyn Fn(&Path, &BatchRequest, &BatchControl<'_>, &str) -> Result<BatchResult, ResearchError> + Send + Sync;

#[derive(Default)]
struct Calls {
    log: Mutex<Vec<String>>,
    running: AtomicUsize,
    max_running: AtomicUsize,
}

impl Calls {
    fn enter(&self, what: String) {
        self.log.lock().unwrap().push(what);
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_running.fetch_max(now, Ordering::SeqCst);
    }
    fn leave(&self) {
        self.running.fetch_sub(1, Ordering::SeqCst);
    }
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

type BatchCall = (BatchMode, BatchCode, Vec<String>);

struct FakeBatch {
    available: Mutex<Result<(), String>>,
    behavior: Mutex<Arc<BatchBehavior>>,
    requests: Mutex<Vec<BatchCall>>,
    /// The image identity each batch was pinned to.
    pinned: Mutex<Vec<Option<String>>>,
    calls: Arc<Calls>,
    counter: AtomicUsize,
}

impl BatchRunner for FakeBatch {
    fn availability(&self, _root: &Path, _mode: BatchMode) -> Result<(), String> {
        self.available.lock().unwrap().clone()
    }
    fn run(
        &self,
        root: &Path,
        request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError> {
        self.calls.enter(format!("batch:{}", request.owner.step));
        self.pinned.lock().unwrap().push(request.restricted_image_id.clone());
        self.requests.lock().unwrap().push((
            request.mode,
            request.code.clone(),
            request.inputs.iter().map(|i| format!("{}@{}", i.path, &i.sha256[..8])).collect(),
        ));
        let run = format!("run-{}", self.counter.fetch_add(1, Ordering::SeqCst));
        (control.on_dispatched)(&run);
        let behavior = self.behavior.lock().unwrap().clone();
        let result = behavior(root, request, control, &run);
        self.calls.leave();
        result
    }
}

/// Write `name` into the run's artifact area and describe it.
fn artifact(root: &Path, run: &str, port: &str, name: &str, content: &[u8], kind: ArtifactKind) -> BatchPortOutput {
    let rel = format!(".exposition/artifacts/{run}/{name}");
    let abs = root.join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, content).unwrap();
    BatchPortOutput {
        port: port.into(),
        artifact: ArtifactRecord {
            id: ArtifactId(format!("a-{port}")),
            kind,
            path: Some(WorkspacePath::parse(&rel).unwrap()),
            scratch_path: None,
            media_type: "application/octet-stream".into(),
            size_bytes: content.len() as u64,
            content_hash: format!("sha256:{}", sha256_hex(content)),
        },
    }
}

fn succeeded(run: &str, ports: Vec<BatchPortOutput>) -> BatchResult {
    BatchResult {
        research_run: run.into(),
        status: RunStatus::Succeeded,
        exit_code: Some(0),
        ports,
        extra: Vec::new(),
        rejected: Vec::new(),
        log: "ok".into(),
        environment: Vec::new(),
    }
}

/// The default batch: writes figure.svg, table.csv, dataset.csv.
fn default_behavior() -> Arc<BatchBehavior> {
    Arc::new(|root, _req, _control, run| {
        Ok(succeeded(
            run,
            vec![
                artifact(root, run, "figure", "figure.svg", b"<svg/>", ArtifactKind::Figure),
                artifact(root, run, "table", "table.csv", b"a,b\n1,2\n", ArtifactKind::Table),
                artifact(root, run, "dataset", "dataset.csv", format!("x\n{run}\n").as_bytes(), ArtifactKind::Data),
            ],
        ))
    })
}

/// Blocks until canceled (or `release` is set), then reports cancellation.
fn blocking_behavior(started: Arc<AtomicBool>, release: Arc<AtomicBool>) -> Arc<BatchBehavior> {
    Arc::new(move |_root, _req, control, run| {
        started.store(true, Ordering::SeqCst);
        while !(control.canceled)() && !release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut r = succeeded(run, Vec::new());
        r.status = RunStatus::Canceled;
        Ok(r)
    })
}

struct FakeLean {
    root: PathBuf,
    calls: Arc<Calls>,
    recorded: Mutex<Vec<Vec<String>>>,
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
        self.calls.enter(format!("lean:{path}"));
        let text = std::fs::read_to_string(self.root.join(path.as_str())).unwrap();
        let declarations: Vec<AuditedDeclaration> = text
            .lines()
            .filter_map(|l| l.strip_prefix("theorem "))
            .map(|rest| AuditedDeclaration {
                name: rest.split_whitespace().next().unwrap().to_owned(),
                kind: "theorem".into(),
                statement: "1 + 1 = 2".into(),
                range: None,
                line: None,
                axioms: vec!["propext".into()],
                verdict: DeclarationVerdict::Checked,
            })
            .collect();
        self.calls.leave();
        Ok(LeanAuditRecord {
            document: exposition_core::document::DocumentId("doc-1".into()),
            path: path.clone(),
            revision: 1,
            content_hash: sha256_hex(text.as_bytes()),
            toolchain: "leanprover/lean4:v4.35.0".into(),
            toolchain_version: None,
            status: LeanStatus::Unverified { revision: 1, toolchain: "x".into(), reason: "test".into() },
            declaration_count: declarations.len() as u32,
            declarations,
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
        self.recorded.lock().unwrap().push(names.to_vec());
        Ok(names.iter().map(|_| EvidenceId::mint()).collect())
    }
}

struct FakeResearch {
    root: PathBuf,
    calls: Arc<Calls>,
    /// The research service's view of prepared reviews (default: still pending).
    reviews: Mutex<BTreeMap<String, PromotionReview>>,
}

impl ResearchActions for FakeResearch {
    fn available(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn retain(&self, research_run: &str, artifact: &str) -> Result<RetainedRecord, StepFailure> {
        self.calls.enter(format!("retain:{research_run}/{artifact}"));
        let name = if artifact == "a-figure" { "figure.svg" } else { "table.csv" };
        let rel = format!(".exposition/artifacts/{research_run}/{name}");
        let bytes = std::fs::read(self.root.join(&rel)).map_err(|e| StepFailure::new("research.io", e.to_string()))?;
        self.calls.leave();
        Ok(RetainedRecord {
            entity: EntityId::mint(),
            path: WorkspacePath::parse(&rel).unwrap(),
            sha256: sha256_hex(&bytes),
            evidence: Some(EvidenceId::mint()),
        })
    }
    fn prepare_promotion(
        &self,
        research_run: &str,
        _artifact: &str,
        document: &WorkspacePath,
        _caption: Option<String>,
    ) -> Result<PreparedPromotion, StepFailure> {
        self.calls.enter(format!("promote:{research_run}->{document}"));
        self.calls.leave();
        Ok(PreparedPromotion {
            promotion: "promo-1".into(),
            attachment_key: "fig-run".into(),
            expires_at_ms: crate::now_ms() + 60_000,
        })
    }
    fn promotion_applied(&self, root: &Path, document: &WorkspacePath, key: &str) -> bool {
        std::fs::read_to_string(root.join(document.as_str())).is_ok_and(|t| markers_present(&t, key))
    }
    fn promotion_review(&self, promotion: &str) -> PromotionReview {
        self.reviews.lock().unwrap().get(promotion).copied().unwrap_or(PromotionReview::Pending)
    }
}

type Claim = Box<dyn Fn(&StepRequest<'_>) -> Vec<WorkflowStepOutput> + Send + Sync>;

/// A Python adapter that claims whatever the closure returns (forgery tests).
struct Claiming(Claim);

impl StepAdapter for Claiming {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let op = OperationRef { kind: "researchRun".into(), id: "run-forged".into() };
        (control.on_dispatched)(&op);
        Ok(StepResult { operation: Some(op), outputs: (self.0)(request), ..Default::default() })
    }
}

// ---------------------------------------------------------------- setup ----

struct Fixture {
    _data: tempfile::TempDir,
    _project: tempfile::TempDir,
    _runs: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    host: Arc<Host>,
    graph: Arc<GraphService>,
    batch: Arc<FakeBatch>,
    lean: Arc<FakeLean>,
    research: Arc<FakeResearch>,
    calls: Arc<Calls>,
    service: WorkflowRunService,
}

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn adapters(batch: &Arc<FakeBatch>, lean: &Arc<FakeLean>, research: &Arc<FakeResearch>) -> Adapters {
    let mut a: Adapters = BTreeMap::new();
    let runner: Arc<dyn BatchRunner> = batch.clone();
    a.insert(
        runner_ids::RESTRICTED_PYTHON.into(),
        Arc::new(BatchAdapter::new(Some(runner.clone()), BatchMode::Restricted)),
    );
    a.insert(runner_ids::NATIVE_PYTHON.into(), Arc::new(BatchAdapter::new(Some(runner), BatchMode::TrustedNative)));
    let auditor: Arc<dyn LeanAuditor> = lean.clone();
    a.insert(runner_ids::LEAN_AUDIT.into(), Arc::new(LeanAuditAdapter { auditor: Some(auditor) }));
    a.insert(runner_ids::GRAPH_PROPOSAL.into(), Arc::new(AssociateAdapter));
    let r: Arc<dyn ResearchActions> = research.clone();
    a.insert(runner_ids::RESEARCH_RETENTION.into(), Arc::new(RetainAdapter { research: Some(r.clone()) }));
    a.insert(runner_ids::REVIEWED_PROMOTION.into(), Arc::new(PromotionAdapter { research: Some(r) }));
    a.insert(runner_ids::READ_ONLY_AGENT.into(), Arc::new(Unavailable(AGENT_REVIEW_UNAVAILABLE.into())));
    a
}

fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/obs.csv"), "x\n1\n2\n3\n").unwrap();
    std::fs::write(root.join("main.typ"), "= Paper\n").unwrap();
    std::fs::create_dir_all(root.join("Proofs")).unwrap();
    std::fs::write(
        root.join("Proofs/Main.lean"),
        "theorem main_theorem : 1 + 1 = 2 := rfl\ntheorem helper : True := trivial\n",
    )
    .unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    assert!(graph.attach(&root).is_usable());
    let calls = Arc::new(Calls::default());
    let batch = Arc::new(FakeBatch {
        available: Mutex::new(Ok(())),
        behavior: Mutex::new(default_behavior()),
        requests: Mutex::new(Vec::new()),
        pinned: Mutex::new(Vec::new()),
        calls: calls.clone(),
        counter: AtomicUsize::new(1),
    });
    let lean = Arc::new(FakeLean { root: root.clone(), calls: calls.clone(), recorded: Mutex::new(Vec::new()) });
    let research = Arc::new(FakeResearch { root: root.clone(), calls: calls.clone(), reviews: Mutex::default() });
    let host = Arc::new(Host {
        root: Mutex::new(Some(root.clone())),
        trusted: AtomicBool::new(true),
        image: Mutex::new(Some("python:3.13-slim".into())),
        image_id: Mutex::new(Some(IMAGE_A.into())),
        graph: graph.clone(),
        on_root: Mutex::new(None),
    });
    let db = runs.path().join("workflow-runs.sqlite3");
    let service = new_service(&db, &host, &batch, &lean, &research);
    Fixture {
        _data: data,
        _project: project,
        _runs: runs,
        root,
        db,
        host,
        graph,
        batch,
        lean,
        research,
        calls,
        service,
    }
}

fn new_service(
    db: &Path,
    host: &Arc<Host>,
    batch: &Arc<FakeBatch>,
    lean: &Arc<FakeLean>,
    research: &Arc<FakeResearch>,
) -> WorkflowRunService {
    let auditor: Arc<dyn LeanAuditor> = lean.clone();
    let h: Arc<dyn WorkflowHost> = host.clone();
    WorkflowRunService::new(WorkflowRunConfig {
        database: Some(db.to_path_buf()),
        host: h,
        adapters: adapters(batch, lean, research),
        lean: Some(auditor),
        research: Some(research.clone()),
    })
    .unwrap()
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
        saved.id
    }

    fn start(&self, id: &WorkflowId) -> WorkflowRun {
        let plan = self.service.plan(id, None).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        self.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap()
    }

    fn run_to_end(&self, id: &WorkflowId) -> WorkflowRun {
        let run = self.start(id);
        assert!(self.service.wait_idle(Duration::from_secs(20)), "run did not finish");
        self.service.get(&run.id).unwrap()
    }

    fn set_behavior(&self, b: Arc<BatchBehavior>) {
        *self.batch.behavior.lock().unwrap() = b;
    }

    fn start_err(&self, plan: WorkflowRunPlan) -> WorkflowRunError {
        self.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap_err()
    }
}

fn numerical(manuscript: bool) -> WorkflowDefinition {
    draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        data: Some(path("data/obs.csv")),
        manuscript: manuscript.then(|| path("main.typ")),
        ..Default::default()
    })
    .unwrap()
}

fn formal(declarations: Option<&str>) -> WorkflowDefinition {
    let mut def = draft(&WorkflowPresetRequest {
        preset: FORMAL_AUDIT.into(),
        lean_file: Some(path("Proofs/Main.lean")),
        ..Default::default()
    })
    .unwrap();
    if let Some(d) = declarations {
        def.steps[1].parameters.insert("declarations".into(), ParamValue::Text(d.into()));
    }
    def
}

fn python_step(id: &str, native: bool) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Python {id}"),
        operation: StepOperation {
            id: if native { RESEARCH_NATIVE_PYTHON.into() } else { "research.pythonBatch".into() },
            version: 1,
        },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Code { language: CodeLanguage::Python, source: format!("# {id}\n") }),
    }
}

fn dataset_outcome(step: &str) -> WorkflowOutcome {
    WorkflowOutcome {
        id: format!("o-{step}"),
        kind: OutcomeKind::TransformedDataset,
        label: format!("Dataset of {step}"),
        from: StepPort { step: step.into(), port: "dataset".into() },
        optional: false,
    }
}

fn data_edge(from: PortSource, step: &str) -> WorkflowConnection {
    WorkflowConnection::Data { from, to: StepPort { step: step.into(), port: "data".into() } }
}

fn out(step: &str) -> PortSource {
    PortSource::Step { step: step.into(), port: "dataset".into() }
}

/// a → b → c chained through `dataset`, with the external `data` input fed to `b`.
fn chain() -> WorkflowDefinition {
    let mut def = numerical(false);
    def.steps = vec![python_step("a", false), python_step("b", false), python_step("c", false)];
    def.connections = vec![
        data_edge(out("a"), "b"),
        data_edge(PortSource::Input { input: "data".into() }, "b"),
        data_edge(out("b"), "c"),
    ];
    def.outcomes = vec![dataset_outcome("c")];
    def
}

fn states(run: &WorkflowRun) -> Vec<(String, WorkflowStepState)> {
    run.steps.iter().map(|s| (s.step.clone(), s.state)).collect()
}

fn step_states(run: &WorkflowRun) -> Vec<WorkflowStepState> {
    run.steps.iter().map(|s| s.state).collect()
}

fn last_attempt<'a>(run: &'a WorkflowRun, step: &str) -> &'a WorkflowStepAttempt {
    run.steps.iter().find(|s| s.step == step).unwrap().attempts.last().unwrap()
}

fn wait_for(flag: &AtomicBool) {
    for _ in 0..4000 {
        if flag.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out");
}

const IMAGE_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// ---------------------------------------------------------------- tests ----

#[test]
fn numerical_preset_runs_end_to_end_one_step_at_a_time() {
    let f = fixture();
    let id = f.save(numerical(true));
    let plan = f.service.plan(&id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let order: Vec<&str> = plan.steps.iter().map(|s| s.step.as_str()).collect();
    assert_eq!(order, vec!["run", "retain", "promote"]);
    assert!(matches!(plan.steps[0].payload, Some(StepPayload::Code { .. })), "full payload in the plan");
    assert_eq!(plan.inputs.len(), 1);
    assert_eq!(plan.inputs[0].sha256, sha256_hex(b"x\n1\n2\n3\n"));
    assert_eq!(plan.execution.restricted_image.as_deref(), Some("python:3.13-slim"));
    assert!(plan.notices.iter().any(|n| n.contains("restricted container")));
    assert_eq!(plan.approval_hash.len(), 64);

    let run = f
        .service
        .start(&StartWorkflowRunRequest { plan: plan.plan.clone(), approval_hash: plan.approval_hash })
        .unwrap();
    assert_eq!(run.state, WorkflowRunState::Running);
    assert!(f.service.wait_idle(Duration::from_secs(20)));
    let run = f.service.get(&run.id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?} {:?}", run.code, run.message);
    assert_eq!(step_states(&run), vec![WorkflowStepState::Succeeded; 3]);
    // One step at a time, in plan order.
    assert_eq!(f.calls.max_running.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.calls.log(),
        vec!["batch:run".to_owned(), "retain:run-1/a-figure".into(), "promote:run-1->main.typ".into()]
    );
    // The restricted batch got the inline code and exactly the declared input.
    {
        let requests = f.batch.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, BatchMode::Restricted);
        assert!(matches!(&requests[0].1, BatchCode::Inline { source } if source.contains("EXPOSITION_OUTPUT_DIR")));
        assert_eq!(requests[0].2.len(), 1);
        assert!(requests[0].2[0].starts_with("data/obs.csv@"));
    }
    // Durable attempts with service operations and validated outputs.
    let a = last_attempt(&run, "run");
    assert_eq!(a.state, WorkflowAttemptState::Succeeded);
    assert_eq!(a.operation.as_ref().unwrap().id, "run-1");
    assert!(a.dispatched_at_ms.is_some());
    assert_eq!(a.outputs.len(), 3);
    assert!(a.outputs.iter().all(|o| o.provenance.run == run.id && o.provenance.step == "run"));
    let retained = &last_attempt(&run, "retain").outputs[0];
    assert!(matches!(retained.value, WorkflowOutputValue::Retained { .. }));
    let promotion = &last_attempt(&run, "promote").outputs[0];
    assert!(matches!(
        &promotion.value,
        WorkflowOutputValue::PromotionProposal { status: WorkflowProposalStatus::Pending, .. }
    ));
    // The promotion review is reflected when the run is read.
    std::fs::write(
        f.root.join("main.typ"),
        "= Paper\n// exposition:begin fig-run\n#figure(image(\"x.svg\"))\n// exposition:end fig-run\n",
    )
    .unwrap();
    let reread = f.service.get(&run.id).unwrap();
    assert!(matches!(
        &last_attempt(&reread, "promote").outputs[0].value,
        WorkflowOutputValue::PromotionProposal { status: WorkflowProposalStatus::Accepted, .. }
    ));
    assert!(markers_present("<!-- exposition:begin k -->\nx\n<!-- exposition:end k -->", "k"));
    assert!(!markers_present("% exposition:end k\n% exposition:begin k", "k"));
    let figure = run.outcomes.iter().find(|o| o.id == "figure").unwrap();
    assert!(figure.value.is_some());
    assert_eq!(run.usage.steps, 3);
    assert!(run.usage.output_bytes > 0);
    // Listed for the project, newest first.
    let list = f.service.runs(Some(&id)).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].steps_done, 3);
    // The plan was consumed.
    let again =
        f.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: run.plan.approval_hash.clone() });
    assert_eq!(again.unwrap_err().code, "workflow.planStale");
}

#[test]
fn a_discarded_or_lost_promotion_review_is_not_shown_as_awaiting_review() {
    let f = fixture();
    let id = f.save(numerical(true));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let status = |run: &WorkflowRun| match &last_attempt(run, "promote").outputs[0].value {
        WorkflowOutputValue::PromotionProposal { status, .. } => *status,
        other => panic!("{other:?}"),
    };
    assert_eq!(status(&f.service.get(&run.id).unwrap()), WorkflowProposalStatus::Pending);
    // The user discarded the prepared review: it reads as rejected, not pending.
    f.research.reviews.lock().unwrap().insert("promo-1".into(), PromotionReview::Discarded);
    let reread = f.service.get(&run.id).unwrap();
    assert_eq!(status(&reread), WorkflowProposalStatus::Rejected);
    let outcome = reread.outcomes.iter().find(|o| o.step == "promote").and_then(|o| o.value.clone());
    assert!(
        matches!(
            outcome,
            Some(WorkflowOutputValue::PromotionProposal { status: WorkflowProposalStatus::Rejected, .. })
        ),
        "the outcome reflects it too: {outcome:?}"
    );
    // The prepared review is gone (e.g. the app restarted): it can no longer be approved.
    f.research.reviews.lock().unwrap().insert("promo-1".into(), PromotionReview::Gone);
    assert_eq!(status(&f.service.get(&run.id).unwrap()), WorkflowProposalStatus::Expired);
    // An applied insertion wins over either.
    std::fs::write(
        f.root.join("main.typ"),
        "= Paper
// exposition:begin fig-run
#figure(image(\"x.svg\"))
// exposition:end fig-run
",
    )
    .unwrap();
    assert_eq!(status(&f.service.get(&run.id).unwrap()), WorkflowProposalStatus::Accepted);
}

#[test]
fn explicit_prerequisites_order_independent_steps() {
    let f = fixture();
    let mut def = numerical(false);
    def.steps = vec![python_step("a", false), python_step("b", false)];
    def.connections = vec![WorkflowConnection::Prerequisite { before: "b".into(), after: "a".into() }];
    def.inputs.clear();
    def.outcomes = vec![dataset_outcome("a"), dataset_outcome("b")];
    let id = f.save(def);
    let plan = f.service.plan(&id, None).unwrap();
    assert_eq!(plan.steps.iter().map(|s| s.step.as_str()).collect::<Vec<_>>(), vec!["b", "a"]);
    assert_eq!(plan.steps[1].prerequisites, vec!["b".to_owned()]);
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(f.calls.log(), vec!["batch:b".to_owned(), "batch:a".into()]);
}

#[test]
fn cancel_mid_step_cancels_the_job_and_skips_the_rest() {
    let f = fixture();
    let started = Arc::new(AtomicBool::new(false));
    f.set_behavior(blocking_behavior(started.clone(), Arc::new(AtomicBool::new(false))));
    let id = f.save(numerical(true));
    let run = f.start(&id);
    wait_for(&started);
    let canceled = f.service.cancel(&run.id).unwrap();
    assert_eq!(canceled.state, WorkflowRunState::Canceled);
    assert_eq!(canceled.code.as_deref(), Some("workflow.canceled"));
    assert_eq!(
        states(&canceled),
        vec![
            ("run".into(), WorkflowStepState::Canceled),
            ("retain".into(), WorkflowStepState::Skipped),
            ("promote".into(), WorkflowStepState::Skipped)
        ]
    );
    let a = last_attempt(&canceled, "run");
    assert_eq!(a.state, WorkflowAttemptState::Canceled);
    assert_eq!(a.operation.as_ref().unwrap().kind, "researchRun", "the operation was persisted");
    assert!(f.service.active_runs().is_empty());
    assert_eq!(f.calls.log(), vec!["batch:run".to_owned()], "nothing after the canceled step");
}

#[test]
fn time_budget_exhausted_stops_before_the_next_dispatch() {
    let f = fixture();
    let mut def = chain();
    for s in &mut def.steps {
        s.parameters.insert("timeoutSeconds".into(), ParamValue::Integer(1));
    }
    let id = f.save(def);
    f.set_behavior(Arc::new(|root, req, _c, run| {
        if req.owner.step == "a" {
            std::thread::sleep(Duration::from_millis(3100));
        }
        Ok(succeeded(run, vec![artifact(root, run, "dataset", "dataset.csv", b"x\n", ArtifactKind::Data)]))
    }));
    let plan = f.service.plan(&id, None).unwrap();
    assert_eq!(plan.budgets.max_total_seconds, 3);
    assert!(plan.steps.iter().all(|s| s.time_limit_seconds == 1));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert_eq!(run.code.as_deref(), Some("workflow.budgetExhausted"), "{:?}", run.message);
    assert_eq!(
        step_states(&run),
        vec![WorkflowStepState::Succeeded, WorkflowStepState::Failed, WorkflowStepState::Skipped]
    );
    assert!(last_attempt(&run, "b").dispatched_at_ms.is_none(), "never dispatched");
    assert_eq!(f.calls.log(), vec!["batch:a".to_owned()]);
}

#[test]
fn a_crash_interrupts_the_run_and_nothing_is_replayed() {
    let f = fixture();
    let started = Arc::new(AtomicBool::new(false));
    // Never released: the "crashed" session's step never returns.
    f.set_behavior(blocking_behavior(started.clone(), Arc::new(AtomicBool::new(false))));
    let id = f.save(numerical(false));
    let run = f.start(&id);
    wait_for(&started);
    std::thread::sleep(Duration::from_millis(50));
    let before = f.service.get(&run.id).unwrap();
    assert_eq!(last_attempt(&before, "run").state, WorkflowAttemptState::Dispatched);
    assert_eq!(last_attempt(&before, "run").operation.as_ref().unwrap().id, "run-1");
    // Restart: a new service over the same store (the old one is abandoned).
    let fresh_batch = Arc::new(FakeBatch {
        available: Mutex::new(Ok(())),
        behavior: Mutex::new(default_behavior()),
        requests: Mutex::new(Vec::new()),
        pinned: Mutex::new(Vec::new()),
        calls: f.calls.clone(),
        counter: AtomicUsize::new(100),
    });
    let restarted = new_service(&f.db, &f.host, &fresh_batch, &f.lean, &f.research);
    let after = restarted.get(&run.id).unwrap();
    assert_eq!(after.state, WorkflowRunState::Interrupted);
    assert_eq!(after.code.as_deref(), Some("workflow.interrupted"));
    assert!(!after.acknowledged);
    assert_eq!(
        states(&after),
        vec![("run".into(), WorkflowStepState::Interrupted), ("retain".into(), WorkflowStepState::Skipped)]
    );
    let a = last_attempt(&after, "run");
    assert_eq!(a.state, WorkflowAttemptState::Interrupted);
    assert_eq!(a.operation.as_ref().unwrap().id, "run-1", "the operation id survived");
    // Nothing was re-dispatched.
    std::thread::sleep(Duration::from_millis(100));
    assert!(fresh_batch.requests.lock().unwrap().is_empty());
    assert!(restarted.active_runs().is_empty());
    // Acknowledge marks it reviewed; still nothing runs.
    let acked = restarted.acknowledge(&run.id).unwrap();
    assert!(acked.acknowledged);
    assert_eq!(acked.state, WorkflowRunState::Interrupted);
    assert!(fresh_batch.requests.lock().unwrap().is_empty());
    assert!(restarted.runs(None).unwrap()[0].acknowledged);
}

#[test]
fn acknowledging_needs_an_interrupted_run() {
    let f = fixture();
    let id = f.save(numerical(false));
    let run = f.run_to_end(&id);
    assert_eq!(f.service.acknowledge(&run.id).unwrap_err().code, "workflow.notInterrupted");
}

#[test]
fn stale_plans_are_refused() {
    let f = fixture();
    let id = f.save(numerical(false));
    // Input changed between plan and start.
    let plan = f.service.plan(&id, None).unwrap();
    std::fs::write(f.root.join("data/obs.csv"), "x\n9\n").unwrap();
    let e = f.start_err(plan);
    assert_eq!(e.code, "workflow.planStale");
    assert!(e.message.contains("Input data") && e.message.contains("changed"), "{}", e.message);
    // A new revision saved between plan and start.
    let plan = f.service.plan(&id, None).unwrap();
    let mut def = f.graph.workflow(&id, None).unwrap().definition;
    def.title = "Numerical experiment v2".into();
    f.graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: Some(id.clone()), expected_revision: None, definition: def },
            &ValidationContext::default(),
        )
        .unwrap();
    let e = f.start_err(plan);
    assert_eq!(e.code, "workflow.planStale");
    assert!(e.message.contains("saved again"), "{}", e.message);
    // The restricted image is part of the approval too.
    let plan = f.service.plan(&id, None).unwrap();
    *f.host.image.lock().unwrap() = Some("python:3.12-slim".into());
    let e = f.start_err(plan);
    assert!(e.message.contains("image"), "{}", e.message);
    // A wrong approval hash and an unknown plan.
    let mut plan = f.service.plan(&id, None).unwrap();
    plan.approval_hash = "0".repeat(64);
    assert_eq!(f.start_err(plan.clone()).code, "workflow.planStale");
    plan.plan = exposition_graph::WorkflowPlanId::mint();
    assert_eq!(f.start_err(plan).code, "workflow.planStale");
    assert!(f.calls.log().is_empty(), "nothing ran");
}

#[test]
fn an_input_changed_between_steps_fails_the_consumer() {
    let f = fixture();
    let id = f.save(chain());
    let root = f.root.clone();
    f.set_behavior(Arc::new(move |r, req, _c, run| {
        if req.owner.step == "a" {
            // Something else changes the declared input while step a runs.
            std::fs::write(root.join("data/obs.csv"), "x\n42\n").unwrap();
        }
        Ok(succeeded(run, vec![artifact(r, run, "dataset", "dataset.csv", b"x\n", ArtifactKind::Data)]))
    }));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert_eq!(run.code.as_deref(), Some("workflow.inputChanged"), "{:?}", run.message);
    assert_eq!(
        step_states(&run),
        vec![WorkflowStepState::Succeeded, WorkflowStepState::Failed, WorkflowStepState::Skipped]
    );
    assert_eq!(f.calls.log(), vec!["batch:a".to_owned()]);
}

#[test]
fn an_altered_upstream_output_is_refused() {
    let f = fixture();
    let id = f.save(numerical(false));
    let root = f.root.clone();
    let produced = Arc::new(AtomicBool::new(false));
    let p = produced.clone();
    f.set_behavior(Arc::new(move |r, _req, _c, run| {
        p.store(true, Ordering::SeqCst);
        Ok(succeeded(run, vec![artifact(r, run, "figure", "figure.svg", b"<svg/>", ArtifactKind::Figure)]))
    }));
    let altered = Arc::new(AtomicBool::new(false));
    let a = altered.clone();
    *f.host.on_root.lock().unwrap() = Some(Box::new(move || {
        // After the figure was produced and validated, before the next step.
        if produced.load(Ordering::SeqCst) && !a.swap(true, Ordering::SeqCst) {
            std::fs::write(root.join(".exposition/artifacts/run-1/figure.svg"), "<svg>tampered</svg>").unwrap();
        }
    }));
    let run = f.run_to_end(&id);
    assert!(altered.load(Ordering::SeqCst));
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert_eq!(run.code.as_deref(), Some("workflow.outputChanged"), "{:?}", run.message);
    assert_eq!(last_attempt(&run, "retain").code.as_deref(), Some("workflow.outputChanged"));
    assert!(!f.calls.log().iter().any(|c| c.starts_with("retain")), "retention never saw the altered file");
}

#[test]
fn forged_service_owned_outputs_are_refused() {
    for forged in ["formal", "retained", "relabel"] {
        let f = fixture();
        let mut a = adapters(&f.batch, &f.lean, &f.research);
        a.insert(
            runner_ids::RESTRICTED_PYTHON.into(),
            Arc::new(Claiming(Box::new(move |req| {
                let op = OperationRef { kind: "researchRun".into(), id: "run-forged".into() };
                let value = match forged {
                    "formal" => WorkflowOutputValue::FormalEvidence {
                        file: WorkspacePath::parse("Proofs/Main.lean").unwrap(),
                        file_sha256: "a".repeat(64),
                        verdict: "Checked".into(),
                        declarations: Vec::new(),
                        evidence: Vec::new(),
                    },
                    _ => WorkflowOutputValue::Retained {
                        entity: EntityId::mint(),
                        path: WorkspacePath::parse("main.typ").unwrap(),
                        sha256: "a".repeat(64),
                        evidence: None,
                        research_run: None,
                        artifact: None,
                    },
                };
                let port_type = match forged {
                    "formal" => PortType::FormalEvidence,
                    "retained" => PortType::RetainedArtifact,
                    _ => PortType::Figure,
                };
                vec![req.output("figure", port_type, value, &op)]
            }))),
        );
        let h: Arc<dyn WorkflowHost> = f.host.clone();
        let service = WorkflowRunService::new(WorkflowRunConfig {
            database: None,
            host: h,
            adapters: a,
            lean: None,
            research: None,
        })
        .unwrap();
        let id = f.save(numerical(false));
        let plan = service.plan(&id, None).unwrap();
        let run =
            service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
        assert!(service.wait_idle(Duration::from_secs(10)));
        let run = service.get(&run.id).unwrap();
        assert_eq!(run.state, WorkflowRunState::Failed, "{forged}");
        assert_eq!(run.code.as_deref(), Some("workflow.forgedOutput"), "{forged}: {:?}", run.message);
        let attempt = last_attempt(&run, "run");
        assert!(attempt.outputs.is_empty(), "no forged value reaches a consumer");
        assert_eq!(attempt.rejected.len(), 1);
        assert_eq!(states(&run)[1].1, WorkflowStepState::Skipped);
    }
}

#[test]
fn python_json_that_looks_like_evidence_is_only_a_file() {
    let f = fixture();
    let mut def = numerical(false);
    def.outcomes = vec![dataset_outcome("run")];
    def.steps.truncate(1);
    def.connections.retain(|c| matches!(c, WorkflowConnection::Data { to, .. } if to.step == "run"));
    let id = f.save(def);
    f.set_behavior(Arc::new(|root, _req, _c, run| {
        let fake = br#"{"kind":"formalEvidence","verdict":"Checked","declarations":[{"name":"main_theorem"}]}"#;
        Ok(succeeded(run, vec![artifact(root, run, "dataset", "dataset.json", fake, ArtifactKind::Data)]))
    }));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let out = &last_attempt(&run, "run").outputs[0];
    assert_eq!(out.port_type, PortType::Dataset);
    assert!(matches!(out.value, WorkflowOutputValue::File { .. }));
}

#[test]
fn outputs_of_another_run_or_missing_ports_are_refused() {
    let f = fixture();
    let id = f.save(numerical(false));
    f.set_behavior(Arc::new(|root, _req, _c, run| {
        // Claims a file of a different research run.
        let other = artifact(root, "run-other", "figure", "figure.svg", b"<svg/>", ArtifactKind::Figure);
        Ok(succeeded(run, vec![other]))
    }));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert_eq!(run.code.as_deref(), Some("workflow.invalidOutput"), "{:?}", run.message);
    // A missing port the next step consumes fails the producer.
    let f = fixture();
    let id = f.save(numerical(false));
    f.set_behavior(Arc::new(|root, _req, _c, run| {
        Ok(succeeded(run, vec![artifact(root, run, "table", "table.csv", b"a\n", ArtifactKind::Table)]))
    }));
    let run = f.run_to_end(&id);
    assert_eq!(run.code.as_deref(), Some("workflow.missingOutput"), "{:?}", run.message);
}

#[test]
fn closing_the_project_cancels_the_run_and_nothing_resumes() {
    let f = fixture();
    let started = Arc::new(AtomicBool::new(false));
    f.set_behavior(blocking_behavior(started.clone(), Arc::new(AtomicBool::new(false))));
    let id = f.save(numerical(false));
    let run = f.start(&id);
    wait_for(&started);
    f.service.project_closed(Some(&f.root));
    *f.host.root.lock().unwrap() = None;
    assert!(f.service.wait_idle(Duration::from_secs(5)));
    // Reopen.
    *f.host.root.lock().unwrap() = Some(f.root.clone());
    let run = f.service.get(&run.id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Canceled);
    assert_eq!(run.code.as_deref(), Some("workflow.projectClosed"));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(f.calls.log(), vec!["batch:run".to_owned()]);
    assert!(f.service.active_runs().is_empty());
}

struct FakeReviewer {
    prompts: Mutex<Vec<(String, Vec<String>, u32)>>,
    /// Fail after this many model requests (e.g. the budget ran out).
    fail_after: Option<u32>,
}

impl AgentReviewer for FakeReviewer {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn review(
        &self,
        request: &ReviewRequest,
        _canceled: &(dyn Fn() -> bool + Sync),
        on_task: &(dyn Fn(&str) + Sync),
    ) -> Result<ReviewOutcome, StepFailure> {
        on_task("task-1");
        self.prompts.lock().unwrap().push((
            request.prompt.clone(),
            request.files.iter().map(|f| f.to_string()).collect(),
            request.max_requests,
        ));
        if let Some(requests) = self.fail_after {
            return Err(StepFailure {
                model_requests: requests,
                ..StepFailure::new("workflow.budgetExhausted", "The review ran out of budget.")
            });
        }
        Ok(ReviewOutcome { text: "The data look consistent.".into(), task: "task-1".into(), requests: 3 })
    }
}

#[test]
fn a_read_only_agent_review_reports_and_counts_model_requests() {
    review_run(None);
}

/// R-m8-F3: a review that fails still counts the requests it made.
#[test]
fn a_failed_review_counts_its_model_requests() {
    review_run(Some(25));
}

fn review_run(fail_after: Option<u32>) {
    let f = fixture();
    let reviewer = Arc::new(FakeReviewer { prompts: Mutex::new(Vec::new()), fail_after });
    let mut a = adapters(&f.batch, &f.lean, &f.research);
    let r: Arc<dyn AgentReviewer> = reviewer.clone();
    a.insert(runner_ids::READ_ONLY_AGENT.into(), Arc::new(AgentReviewAdapter { reviewer: Some(r) }));
    let h: Arc<dyn WorkflowHost> = f.host.clone();
    let service =
        WorkflowRunService::new(WorkflowRunConfig { database: None, host: h, adapters: a, lean: None, research: None })
            .unwrap();
    let mut def = numerical(false);
    def.ceiling.model_requests = true;
    def.steps = vec![WorkflowStep {
        id: "review".into(),
        label: "Review the data".into(),
        operation: StepOperation { id: "agent.review".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Prompt { text: "Summarize the observations.".into() }),
    }];
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
    let id = f.save(def);
    let plan = service.plan(&id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert_eq!(plan.execution.model_steps, vec!["review".to_owned()]);
    assert!(plan.notices.iter().any(|n| n.contains("model provider")));
    assert!(matches!(&plan.steps[0].payload, Some(StepPayload::Prompt { text }) if text.contains("Summarize")));
    let run = service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(service.wait_idle(Duration::from_secs(10)));
    let run = service.get(&run.id).unwrap();
    if let Some(requests) = fail_after {
        assert_eq!(run.state, WorkflowRunState::Failed, "{:?}", run.message);
        assert_eq!(run.code.as_deref(), Some("workflow.budgetExhausted"));
        assert_eq!(run.usage.model_requests, requests, "the failed review's requests are counted");
        return;
    }
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(run.usage.model_requests, 3);
    let out = &last_attempt(&run, "review").outputs[0];
    assert!(
        matches!(&out.value, WorkflowOutputValue::Report { text, task: Some(t) } if text.contains("consistent") && t == "task-1")
    );
    let prompts = reviewer.prompts.lock().unwrap();
    assert_eq!(prompts[0].1, vec!["data/obs.csv".to_owned()], "only the declared material is in scope");
    assert!(prompts[0].0.contains("read-only") && prompts[0].0.contains("Summarize the observations."));
    assert_eq!(prompts[0].2, run.budgets.max_model_requests);
}

#[test]
fn revoking_trust_cancels_a_native_run() {
    let f = fixture();
    let started = Arc::new(AtomicBool::new(false));
    f.set_behavior(blocking_behavior(started.clone(), Arc::new(AtomicBool::new(false))));
    let mut native = numerical(false);
    native.steps[0] = python_step("run", true);
    native.ceiling.execution = ExecutionCeiling::TrustedNative;
    let id = f.save(native);
    let run = f.start(&id);
    wait_for(&started);
    assert_eq!(f.batch.requests.lock().unwrap()[0].0, BatchMode::TrustedNative);
    f.host.trusted.store(false, Ordering::SeqCst);
    f.service.trust_revoked(&f.root);
    assert!(f.service.wait_idle(Duration::from_secs(5)));
    let run = f.service.get(&run.id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Canceled);
    assert_eq!(run.code.as_deref(), Some("workflow.untrustedFolder"));
    assert_eq!(states(&run)[1].1, WorkflowStepState::Skipped);
}

#[test]
fn trust_lost_between_steps_refuses_the_next_native_step() {
    let f = fixture();
    let mut def = chain();
    def.steps[1] = python_step("b", true);
    def.ceiling.execution = ExecutionCeiling::TrustedNative;
    let id = f.save(def);
    let host = f.host.clone();
    f.set_behavior(Arc::new(move |r, req, _c, run| {
        if req.owner.step == "a" {
            host.trusted.store(false, Ordering::SeqCst);
        }
        Ok(succeeded(
            run,
            vec![artifact(
                r,
                run,
                "dataset",
                "dataset.csv",
                b"x
",
                ArtifactKind::Data,
            )],
        ))
    }));
    let run = f.run_to_end(&id);
    assert_eq!(run.code.as_deref(), Some("workflow.untrustedFolder"), "{:?}", run.message);
    assert_eq!(f.calls.log(), vec!["batch:a".to_owned()]);
}

#[test]
fn blockers_untrusted_restricted_unavailable_and_invalid_drafts() {
    let f = fixture();
    // Native Python and Lean need a trusted folder.
    f.host.trusted.store(false, Ordering::SeqCst);
    let mut native = numerical(false);
    native.steps[0] = python_step("run", true);
    native.ceiling.execution = ExecutionCeiling::TrustedNative;
    for def in [native, formal(None)] {
        let id = f.save(def);
        let plan = f.service.plan(&id, None).unwrap();
        assert!(plan.blockers.iter().any(|b| b.code == "workflow.untrustedFolder"), "{:?}", plan.blockers);
        assert_eq!(f.start_err(plan).code, "workflow.untrustedFolder");
        let saved = f.graph.workflow(&id, None).unwrap();
        let mut v = saved.validation.clone();
        f.service.apply_runnability(&mut v, &saved.definition);
        assert!(!v.runnable);
        assert!(v.runnable_reason.contains("not trusted"), "{}", v.runnable_reason);
    }
    f.host.trusted.store(true, Ordering::SeqCst);
    // Restricted backend unavailable: blocked, never downgraded to native.
    *f.batch.available.lock().unwrap() = Err("the image python:3.13-slim is not present locally".into());
    let id = f.save(numerical(false));
    let plan = f.service.plan(&id, None).unwrap();
    let blocker = plan.blockers.iter().find(|b| b.code == "workflow.restrictedUnavailable").expect("blocked");
    assert!(blocker.message.contains("never falls back"), "{}", blocker.message);
    assert_eq!(plan.steps[0].runner, "restrictedPython");
    assert_eq!(f.start_err(plan).code, "workflow.restrictedUnavailable");
    assert!(f.batch.requests.lock().unwrap().is_empty(), "nothing ran natively instead");
    let ops = f.service.operations();
    let restricted = ops.iter().find(|o| o.runner.id == "restrictedPython").unwrap();
    assert!(!restricted.runner.available && restricted.runner.availability.contains("not present"));
    let agent = ops.iter().find(|o| o.runner.id == "readOnlyAgent").unwrap();
    assert!(!agent.runner.available);
    *f.batch.available.lock().unwrap() = Ok(());
    // Invalid drafts: orphan step, incompatible ports, unset input.
    let mut orphan = numerical(false);
    orphan.steps.push(python_step("stray", false));
    let mut incompatible = numerical(false);
    incompatible.connections.push(WorkflowConnection::Data {
        from: PortSource::Input { input: "data".into() },
        to: StepPort { step: "retain".into(), port: "artifact".into() },
    });
    let mut unset = formal(None);
    unset.inputs[0].source = InputSource::Unset;
    for def in [orphan, incompatible, unset] {
        let id = f.save(def);
        let plan = f.service.plan(&id, None).unwrap();
        assert!(plan.blockers.iter().any(|b| b.code == "workflow.notWellFormed"), "{:?}", plan.blockers);
        let saved = f.graph.workflow(&id, None).unwrap();
        let mut v = saved.validation.clone();
        f.service.apply_runnability(&mut v, &saved.definition);
        assert!(!v.runnable);
    }
    // A well-formed draft with available runners is runnable.
    let saved = f.graph.workflow(&f.save(numerical(true)), None).unwrap();
    let mut v = saved.validation.clone();
    f.service.apply_runnability(&mut v, &saved.definition);
    assert!(v.runnable, "{}", v.runnable_reason);
    // A pinned input that no longer matches.
    let mut pinned = numerical(false);
    pinned.inputs[0].source = InputSource::Resource {
        path: path("data/obs.csv"),
        version: InputVersion::Pinned { content_hash: "b".repeat(64) },
    };
    let plan = f.service.plan(&f.save(pinned), None).unwrap();
    assert!(plan.blockers.iter().any(|b| b.code == "workflow.inputChanged"));
    assert!(f.calls.log().is_empty());
}

#[test]
fn only_one_run_is_active_at_a_time() {
    let f = fixture();
    let started = Arc::new(AtomicBool::new(false));
    f.set_behavior(blocking_behavior(started.clone(), Arc::new(AtomicBool::new(false))));
    let id = f.save(numerical(false));
    let first = f.start(&id);
    let plan = f.service.plan(&id, None).unwrap();
    assert!(plan.blockers.iter().any(|b| b.code == "workflow.activeRun"));
    assert_eq!(f.start_err(plan).code, "workflow.activeRun");
    f.service.cancel(&first.id).unwrap();
    assert!(f.service.active_runs().is_empty());
}

#[test]
fn formal_audit_then_association_only_for_audited_declarations() {
    let f = fixture();
    // Naming a declaration the audit did not list fails the step.
    let id = f.save(formal(Some("main_theorem\nnot_in_file")));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert_eq!(run.code.as_deref(), Some("workflow.declarationNotAudited"), "{:?}", run.message);
    // All audited declarations by default.
    let id = f.save(formal(None));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let evidence = &last_attempt(&run, "audit").outputs[0];
    let WorkflowOutputValue::FormalEvidence { declarations, file_sha256, .. } = &evidence.value else {
        panic!("formal evidence")
    };
    assert_eq!(declarations.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["main_theorem", "helper"]);
    assert_eq!(declarations[0].axioms, vec!["propext".to_owned()]);
    assert_eq!(file_sha256.len(), 64);
    assert_eq!(run.plan.execution.restricted_image, None, "no restricted step, no image binding");
    assert_eq!(run.plan.execution.native_steps, vec!["audit".to_owned()]);
    assert!(run.plan.notices.iter().any(|n| n.contains("not a sandbox")));
    let proposal = &last_attempt(&run, "associate").outputs[0];
    assert!(matches!(
        &proposal.value,
        WorkflowOutputValue::AssociationProposal { status: WorkflowProposalStatus::Pending, .. }
    ));
    // Deciding: only names from the proposal.
    let decide = |accept: Vec<&str>| {
        f.service.decide_association(&WorkflowAssociationDecision {
            run: run.id.clone(),
            step: "associate".into(),
            accept: accept.into_iter().map(str::to_owned).collect(),
        })
    };
    assert_eq!(decide(vec!["forged_name"]).unwrap_err().code, "workflow.declarationNotAudited");
    assert!(f.lean.recorded.lock().unwrap().is_empty());
    let decided = decide(vec!["main_theorem"]).unwrap();
    let WorkflowOutputValue::AssociationProposal { status, accepted, evidence, .. } =
        &last_attempt(&decided, "associate").outputs[0].value
    else {
        panic!("proposal")
    };
    assert_eq!(*status, WorkflowProposalStatus::Accepted);
    assert_eq!(accepted, &vec!["main_theorem".to_owned()]);
    assert_eq!(evidence.len(), 1);
    assert_eq!(*f.lean.recorded.lock().unwrap(), vec![vec!["main_theorem".to_owned()]]);
    assert_eq!(decide(vec![]).unwrap_err().code, "workflow.decided");
}

#[test]
fn runs_are_isolated_per_project() {
    let f = fixture();
    let id = f.save(numerical(false));
    let run = f.run_to_end(&id);
    let other = tempfile::tempdir().unwrap();
    *f.host.root.lock().unwrap() = Some(std::fs::canonicalize(other.path()).unwrap());
    assert_eq!(f.service.get(&run.id).unwrap_err().code, "workflow.unknownRun");
    assert!(f.service.runs(None).unwrap().is_empty());
    *f.host.root.lock().unwrap() = None;
    assert_eq!(f.service.plan(&id, None).unwrap_err().code, "workflow.noProject");
}

#[test]
fn hostile_store_rows_are_skipped() {
    let f = fixture();
    let id = f.save(numerical(false));
    let run = f.run_to_end(&id);
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    conn.execute("UPDATE runs SET body = '{\"id\": 1}' WHERE id = ?1", [run.id.as_str()]).unwrap();
    conn.execute(
        "INSERT INTO runs (id, project, workflow, state, started_at, body) VALUES ('wrun-x', ?1, 'wf-x', 'running', 1, 'garbage')",
        [crate::plan::project_key(&f.root)],
    )
    .unwrap();
    drop(conn);
    let service = new_service(&f.db, &f.host, &f.batch, &f.lean, &f.research);
    assert_eq!(service.get(&run.id).unwrap_err().code, "workflow.untrusted");
    assert!(service.runs(None).unwrap().is_empty());
    assert!(service.active_runs().is_empty());
}

#[test]
fn an_unreadable_history_is_set_aside_and_the_service_starts() {
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("workflow-runs.sqlite3");
    std::fs::write(&db, b"this is not a database at all, just garbage bytes that fill the header").unwrap();
    let service = new_service(&db, &f.host, &f.batch, &f.lean, &f.research);
    assert!(service.runs(None).unwrap().is_empty());
    let aside = std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().contains(".bad-"));
    assert!(aside, "the unreadable file is kept aside");
    let id = f.save(numerical(false));
    let plan = service.plan(&id, None).unwrap();
    let run = service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(service.wait_idle(Duration::from_secs(10)));
    assert_eq!(service.get(&run.id).unwrap().state, WorkflowRunState::Succeeded);
}

struct Panicking;

impl StepAdapter for Panicking {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn run(&self, _request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        (control.on_dispatched)(&OperationRef { kind: "researchRun".into(), id: "run-panic".into() });
        panic!("service bug");
    }
}

/// Reports one research run at dispatch and claims another in its result.
struct Switching;

impl StepAdapter for Switching {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        (control.on_dispatched)(&OperationRef { kind: "researchRun".into(), id: "run-a".into() });
        let b = OperationRef { kind: "researchRun".into(), id: "run-b".into() };
        let port = artifact(request.root, "run-b", "figure", "figure.svg", b"<svg/>", ArtifactKind::Figure);
        let value = WorkflowOutputValue::File {
            path: port.artifact.path.clone().unwrap(),
            sha256: sha256_hex(b"<svg/>"),
            bytes: 6,
            media_type: "image/svg+xml".into(),
            research_run: Some("run-b".into()),
            artifact: Some("a-figure".into()),
        };
        Ok(StepResult {
            operation: Some(b.clone()),
            outputs: vec![request.output("figure", PortType::Figure, value, &b)],
            ..Default::default()
        })
    }
}

fn service_with(f: &Fixture, runner: &str, adapter: Arc<dyn StepAdapter>) -> WorkflowRunService {
    let mut a = adapters(&f.batch, &f.lean, &f.research);
    a.insert(runner.into(), adapter);
    let h: Arc<dyn WorkflowHost> = f.host.clone();
    WorkflowRunService::new(WorkflowRunConfig { database: None, host: h, adapters: a, lean: None, research: None })
        .unwrap()
}

#[test]
fn a_panicking_step_fails_the_run_and_releases_the_slot() {
    let f = fixture();
    let service = service_with(&f, runner_ids::RESTRICTED_PYTHON, Arc::new(Panicking));
    let id = f.save(numerical(false));
    let plan = service.plan(&id, None).unwrap();
    let run = service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(service.wait_idle(Duration::from_secs(10)), "the slot is released");
    let run = service.get(&run.id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert_eq!(run.code.as_deref(), Some("workflow.internal"), "{:?}", run.message);
    assert_eq!(last_attempt(&run, "run").operation.as_ref().unwrap().id, "run-panic");
    assert!(service.plan(&id, None).unwrap().blockers.is_empty(), "no lingering active run");
}

#[test]
fn provenance_is_checked_against_the_dispatched_operation() {
    let f = fixture();
    let service = service_with(&f, runner_ids::RESTRICTED_PYTHON, Arc::new(Switching));
    let id = f.save(numerical(false));
    let plan = service.plan(&id, None).unwrap();
    let run = service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(service.wait_idle(Duration::from_secs(10)));
    let run = service.get(&run.id).unwrap();
    assert_eq!(run.code.as_deref(), Some("workflow.invalidOutput"), "{:?}", run.message);
    assert_eq!(last_attempt(&run, "run").operation.as_ref().unwrap().id, "run-a");
    assert!(last_attempt(&run, "run").outputs.is_empty());
}

#[test]
fn a_changed_restricted_image_refuses_later_restricted_steps() {
    let f = fixture();
    let id = f.save(chain());
    let host = f.host.clone();
    f.set_behavior(Arc::new(move |r, req, _c, run| {
        if req.owner.step == "a" {
            *host.image.lock().unwrap() = Some("someone/else:latest".into());
        }
        Ok(succeeded(run, vec![artifact(r, run, "dataset", "dataset.csv", b"x\n", ArtifactKind::Data)]))
    }));
    let run = f.run_to_end(&id);
    assert_eq!(run.code.as_deref(), Some("workflow.restrictedUnavailable"), "{:?}", run.message);
    assert!(run.message.as_deref().unwrap().contains("someone/else:latest"));
    assert_eq!(f.calls.log(), vec!["batch:a".to_owned()]);
}

#[test]
fn the_plan_binds_the_restricted_image_identity_not_just_its_name() {
    let f = fixture();
    let id = f.save(numerical(false));
    let plan = f.service.plan(&id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert_eq!(plan.execution.restricted_image_id.as_deref(), Some(IMAGE_A));
    // The same name now names another image (re-tagged or pulled): stale.
    *f.host.image_id.lock().unwrap() = Some(format!("sha256:{}", "b".repeat(64)));
    let err = f
        .service
        .start(&StartWorkflowRunRequest { plan: plan.plan.clone(), approval_hash: plan.approval_hash.clone() })
        .unwrap_err();
    assert_eq!(err.code, "workflow.planStale");
    assert!(err.message.contains("is now a different image"), "{}", err.message);
    assert!(f.calls.log().is_empty(), "nothing ran");
    // A new plan binds the new identity, and the batch is pinned to it.
    let plan = f.service.plan(&id, None).unwrap();
    let run = f.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(f.service.wait_idle(Duration::from_secs(20)));
    assert_eq!(f.service.get(&run.id).unwrap().state, WorkflowRunState::Succeeded);
    let pinned = f.batch.pinned.lock().unwrap().clone();
    assert_eq!(pinned, vec![Some(format!("sha256:{}", "b".repeat(64)))]);
    // An identity Docker cannot report blocks a restricted plan (never native).
    *f.host.image_id.lock().unwrap() = None;
    let plan = f.service.plan(&id, None).unwrap();
    assert!(plan.blockers.iter().any(|b| b.code == "workflow.restrictedUnavailable"), "{:?}", plan.blockers);
}

#[test]
fn an_image_retagged_during_a_run_refuses_later_restricted_steps() {
    let f = fixture();
    let id = f.save(chain());
    let host = f.host.clone();
    f.set_behavior(Arc::new(move |r, req, _c, run| {
        if req.owner.step == "a" {
            *host.image_id.lock().unwrap() = Some(format!("sha256:{}", "c".repeat(64)));
        }
        Ok(succeeded(run, vec![artifact(r, run, "dataset", "dataset.csv", b"x\n", ArtifactKind::Data)]))
    }));
    let run = f.run_to_end(&id);
    assert_eq!(run.code.as_deref(), Some("workflow.restrictedUnavailable"), "{:?}", run.message);
    assert!(run.message.as_deref().unwrap().contains("not the image you approved"), "{:?}", run.message);
    assert_eq!(f.calls.log(), vec!["batch:a".to_owned()]);
}

#[test]
fn the_plan_shows_the_complete_code_of_a_script_input() {
    let f = fixture();
    let script = "import os
# MARKER-LINE
print('experiment')
";
    std::fs::write(f.root.join("experiment.py"), script).unwrap();
    let from_script = |name: &str| {
        draft(&WorkflowPresetRequest {
            preset: NUMERICAL_EXPERIMENT.into(),
            script: Some(path(name)),
            data: Some(path("data/obs.csv")),
            ..Default::default()
        })
        .unwrap()
    };
    let plan = f.service.plan(&f.save(from_script("experiment.py")), None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let input = plan.inputs.iter().find(|i| i.port_type == PortType::CodeSource).unwrap();
    assert_eq!(input.text.as_deref(), Some(script), "the code exactly as hashed");
    assert_eq!(input.sha256, sha256_hex(script.as_bytes()));
    assert!(plan.inputs.iter().filter(|i| i.port_type != PortType::CodeSource).all(|i| i.text.is_none()));
    // Code that is not text, or too large to review in full, blocks the plan.
    std::fs::write(f.root.join("binary.py"), [0xff_u8, 0xfe, 0x00, 0x41]).unwrap();
    let plan = f.service.plan(&f.save(from_script("binary.py")), None).unwrap();
    assert!(plan.blockers.iter().any(|b| b.code == "workflow.codeNotText"), "{:?}", plan.blockers);
    std::fs::write(f.root.join("huge.py"), "#".repeat(crate::plan::MAX_REVIEW_CODE_BYTES as usize + 1)).unwrap();
    let plan = f.service.plan(&f.save(from_script("huge.py")), None).unwrap();
    assert!(plan.blockers.iter().any(|b| b.code == "workflow.codeTooLarge"), "{:?}", plan.blockers);
}

#[test]
fn hostile_pins_and_trust_changes_in_plans() {
    let f = fixture();
    let mut def = numerical(false);
    def.inputs[0].source = InputSource::Resource {
        path: path("data/obs.csv"),
        version: InputVersion::Pinned { content_hash: "é".repeat(20) },
    };
    let plan = f.service.plan(&f.save(def), None).unwrap();
    assert!(plan.blockers.iter().any(|b| b.code == "workflow.notWellFormed"));
    assert!(plan.blockers.iter().any(|b| b.code == "workflow.inputChanged"));
    // Folder trust binds only plans with native steps.
    let id = f.save(numerical(false));
    let plan = f.service.plan(&id, None).unwrap();
    f.host.trusted.store(false, Ordering::SeqCst);
    let run = f.service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash });
    assert!(run.is_ok(), "{run:?}");
    assert!(f.service.wait_idle(Duration::from_secs(10)));
}

#[cfg(windows)]
#[test]
fn project_keys_ignore_case_and_the_verbatim_prefix() {
    use crate::plan::project_key;
    assert_eq!(project_key(Path::new(r"\\?\C:\Users\X\Proj")), project_key(Path::new(r"c:\users\x\proj")));
    assert_ne!(project_key(Path::new(r"\\?\UNC\server\share")), project_key(Path::new(r"\server\share")));
}

#[test]
fn the_plan_hash_covers_payloads() {
    let f = fixture();
    let id = f.save(numerical(false));
    let a = f.service.plan(&id, None).unwrap();
    let b = f.service.plan(&id, None).unwrap();
    assert_eq!(a.approval_hash, b.approval_hash, "deterministic");
    assert_ne!(a.plan, b.plan);
    let mut def = f.graph.workflow(&id, None).unwrap().definition;
    if let Some(StepPayload::Code { source, .. }) = &mut def.steps[0].payload {
        source.push_str("\nprint('changed')\n");
    }
    let c = f.service.plan(&f.save(def), None).unwrap();
    assert_ne!(a.approval_hash, c.approval_hash);
}
