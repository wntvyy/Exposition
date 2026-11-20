//! QC regressions (audits 4.3 and 4.4): the model binding of review steps
//! (WF-2), durable terminal states (WF-5), association decisions after a
//! failed write (WF-11), and the revision a manual plan names (WF-4).

use std::sync::atomic::AtomicU32;

use exposition_graph::WorkflowRunId;

use super::*;
use crate::adapters::agent::ReviewModel;

/// A reviewer whose configured provider and model can change.
struct SwitchingReviewer {
    model: Mutex<(String, String)>,
    reviews: AtomicU32,
    /// What each review was asked to run with (provider, model), if anything.
    asked: Mutex<Vec<Option<(String, String)>>>,
}

impl SwitchingReviewer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            model: Mutex::new(("anthropic".into(), "model-a".into())),
            reviews: AtomicU32::new(0),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn configure(&self, provider: &str, model: &str) {
        *self.model.lock().unwrap() = (provider.into(), model.into());
    }
}

impl AgentReviewer for SwitchingReviewer {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }
    fn review(
        &self,
        request: &ReviewRequest,
        _canceled: &(dyn Fn() -> bool + Sync),
        on_task: &(dyn Fn(&str) + Sync),
    ) -> Result<ReviewOutcome, StepFailure> {
        self.reviews.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(asked_model(request));
        on_task("task-1");
        Ok(ReviewOutcome { text: "Reviewed.".into(), task: "task-1".into(), requests: 1 })
    }
    fn model(&self, _task: Option<&str>) -> Option<ReviewModel> {
        let (provider, configured) = self.model.lock().unwrap().clone();
        Some(ReviewModel { provider, configured, served_by: None, effort: None })
    }
}

fn review_service(f: &Fixture, reviewer: &Arc<SwitchingReviewer>) -> WorkflowRunService {
    let r: Arc<dyn AgentReviewer> = reviewer.clone();
    service_with(f, runner_ids::READ_ONLY_AGENT, Arc::new(AgentReviewAdapter { reviewer: Some(r) }))
}

fn review_step() -> WorkflowStep {
    WorkflowStep {
        id: "review".into(),
        label: "Review the data".into(),
        operation: StepOperation { id: "agent.review".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Prompt { text: "Summarize the observations.".into() }),
        composite: None,
        condition: None,
        repeat: None,
    }
}

fn report_outcome() -> WorkflowOutcome {
    WorkflowOutcome {
        id: "report".into(),
        kind: OutcomeKind::InvestigationReport,
        label: "Report".into(),
        from: StepPort { step: "review".into(), port: "report".into() },
        optional: false,
    }
}

/// One review step over the declared data.
fn review_only() -> WorkflowDefinition {
    let mut def = numerical(false);
    def.ceiling.model_requests = true;
    def.steps = vec![review_step()];
    def.connections = vec![WorkflowConnection::Data {
        from: PortSource::Input { input: "data".into() },
        to: StepPort { step: "review".into(), port: "material".into() },
    }];
    def.outcomes = vec![report_outcome()];
    def
}

/// A batch whose dataset a review then reads.
fn batch_then_review() -> WorkflowDefinition {
    let mut def = review_only();
    def.steps.insert(0, python_step("a", false));
    def.connections = vec![
        data_edge(PortSource::Input { input: "data".into() }, "a"),
        WorkflowConnection::Data { from: out("a"), to: StepPort { step: "review".into(), port: "material".into() } },
    ];
    def
}

/// WF-2: the provider and model a review step sends its material to are part
/// of what the user approves. A change between review and start refuses the
/// start through the ordinary "changed since review" path.
#[test]
fn a_model_provider_or_model_changed_after_review_refuses_the_start() {
    let f = fixture();
    let reviewer = SwitchingReviewer::new();
    let service = review_service(&f, &reviewer);
    let id = f.save(review_only());
    let start = |plan: WorkflowRunPlan| {
        service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash })
    };
    // Another model of the same provider.
    let plan = service.plan(&id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    reviewer.configure("anthropic", "model-b");
    let e = start(plan).unwrap_err();
    assert_eq!(e.code, "workflow.planStale");
    assert!(e.message.contains("model-a") && e.message.contains("model-b"), "{}", e.message);
    // Another provider.
    let plan = service.plan(&id, None).unwrap();
    reviewer.configure("openai", "model-b");
    let e = start(plan).unwrap_err();
    assert_eq!(e.code, "workflow.planStale");
    assert!(e.message.contains("model provider"), "{}", e.message);
    assert_eq!(reviewer.reviews.load(Ordering::SeqCst), 0, "nothing was sent anywhere");
    // Unchanged: the reviewed plan starts.
    let plan = service.plan(&id, None).unwrap();
    let run = start(plan).unwrap();
    assert!(service.wait_idle(Duration::from_secs(10)));
    let run = service.get(&run.id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Succeeded);
    assert_eq!(reviewer.reviews.load(Ordering::SeqCst), 1);
    // The plan shows what it binds, and the review ran with exactly that.
    let bound = run.plan.execution.model.clone().expect("a plan with a review step names its provider and model");
    assert_eq!((bound.provider.as_str(), bound.model.as_str()), ("openai", "model-b"));
    assert!(
        run.plan.notices.iter().any(|n| n.contains("model provider (openai, model model-b)")),
        "{:?}",
        run.plan.notices
    );
    assert_eq!(*reviewer.asked.lock().unwrap(), vec![Some(("openai".to_owned(), "model-b".to_owned()))]);
}

/// WF-2 compatibility: only plans with a review step gain the binding. A
/// plan without one hashes exactly as before, and a stored plan or run
/// record written before the binding existed still reads.
#[test]
fn plans_without_review_steps_keep_their_approval_hash() {
    let f = fixture();
    let reviewer = SwitchingReviewer::new();
    let service = review_service(&f, &reviewer);
    let id = f.save(numerical(false));
    let plan = service.plan(&id, None).unwrap();
    assert_eq!(plan.execution.model, None, "no review step: nothing is bound");
    let json = serde_json::to_value(&plan.execution).unwrap();
    assert!(json.get("model").is_none(), "{json}");
    reviewer.configure("openai", "model-b");
    assert_eq!(service.plan(&id, None).unwrap().approval_hash, plan.approval_hash);
    // The same plan with a binding hashes differently; without it, as before.
    let mut bound = plan.clone();
    bound.execution.model =
        Some(WorkflowPlanModel { provider: "anthropic".into(), provider_title: "Anthropic".into(), model: "m".into() });
    assert_ne!(crate::plan::approval_hash(&bound, &f.root), plan.approval_hash);
    bound.execution.model = None;
    assert_eq!(crate::plan::approval_hash(&bound, &f.root), plan.approval_hash);
    // An execution record stored before the field existed.
    let read: WorkflowPlanExecution = serde_json::from_value(json).unwrap();
    assert_eq!(read.model, None);
    // A standing plan approved before the binding: the change is named.
    let with_review = service.plan(&f.save(review_only()), None).unwrap();
    let mut earlier = with_review.clone();
    earlier.execution.model = None;
    let text = crate::plan::describe_changes(&earlier, &with_review);
    assert!(text.contains("now names the model provider"), "{text}");
}

/// WF-2: the binding is checked again when the review step is dispatched, so
/// a change during the run sends nothing to the other provider.
#[test]
fn a_model_provider_changed_during_the_run_refuses_the_review_step() {
    let f = fixture();
    let reviewer = SwitchingReviewer::new();
    let service = review_service(&f, &reviewer);
    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    {
        let (started, release) = (started.clone(), release.clone());
        f.set_behavior(Arc::new(move |root, _req, _control, run| {
            started.store(true, Ordering::SeqCst);
            while !release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(succeeded(run, vec![artifact(root, run, "dataset", "dataset.csv", b"x\n1\n", ArtifactKind::Data)]))
        }));
    }
    let id = f.save(batch_then_review());
    let plan = service.plan(&id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let run = service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    wait_for(&started);
    reviewer.configure("openai", "model-z");
    release.store(true, Ordering::SeqCst);
    assert!(service.wait_idle(Duration::from_secs(10)));
    let run = service.get(&run.id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Failed, "{:?}", run.message);
    assert_eq!(run.code.as_deref(), Some("workflow.modelChanged"), "{:?}", run.message);
    assert_eq!(reviewer.reviews.load(Ordering::SeqCst), 0, "the review was never dispatched");
}

/// The provider and model a review was asked to run with.
fn asked_model(request: &ReviewRequest) -> Option<(String, String)> {
    request.model.as_ref().map(|m| (m.provider.clone(), m.model.clone()))
}
