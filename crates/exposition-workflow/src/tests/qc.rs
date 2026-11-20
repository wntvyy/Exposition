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

/// A database trigger that refuses writes of runs matching `when` (a fault
/// in the real store: no seam in the service).
fn refuse_writes(f: &Fixture, when: &str) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    conn.execute_batch(&format!(
        "CREATE TRIGGER qc_refuse_update BEFORE UPDATE ON runs WHEN {when}
         BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;"
    ))
    .unwrap();
    conn
}

fn allow_writes(conn: &rusqlite::Connection) {
    conn.execute_batch("DROP TRIGGER qc_refuse_update;").unwrap();
}

fn stored_state(f: &Fixture, run: &WorkflowRunId) -> String {
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    conn.query_row("SELECT state FROM runs WHERE id = ?1", [run.as_str()], |r| r.get(0)).unwrap()
}

/// WF-5: a run whose final write fails is not left "running" (and later
/// reported as interrupted by a shutdown that never happened): this session
/// reads its real terminal state, and the record is written as soon as the
/// store accepts it.
#[test]
fn a_failed_final_write_does_not_leave_the_run_running() {
    let f = fixture();
    let id = f.save(numerical(false));
    let conn = refuse_writes(&f, "NEW.state != 'running'");
    let run = f.start(&id);
    assert!(f.service.wait_idle(Duration::from_secs(20)));
    assert_eq!(stored_state(&f, &run.id), "running", "the injected fault kept the terminal write out");
    let read = f.service.get(&run.id).unwrap();
    assert_eq!(read.state, WorkflowRunState::Succeeded, "this session reads the state the run ended in");
    assert_eq!(read.steps.iter().filter(|s| s.state == WorkflowStepState::Succeeded).count(), 2);
    // The store works again: the terminal record becomes durable.
    allow_writes(&conn);
    drop(conn);
    assert_eq!(f.service.get(&run.id).unwrap().state, WorkflowRunState::Succeeded);
    assert_eq!(stored_state(&f, &run.id), "succeeded");
    let listed = f.service.runs(Some(&id)).unwrap();
    assert_eq!(listed[0].state, WorkflowRunState::Succeeded);
    // A restart does not turn it into an interrupted run.
    let restarted = new_service(&f.db, &f.host, &f.batch, &f.lean, &f.research);
    assert_eq!(restarted.get(&run.id).unwrap().state, WorkflowRunState::Succeeded);
}

/// WF-5: a terminal record the store refuses for its size is written
/// without its attempt logs and reviewed code text, and says so.
#[test]
fn a_terminal_record_that_is_too_large_is_written_reduced() {
    let f = fixture();
    let id = f.save(numerical(false));
    let mut run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    // The same run ending with a log beyond what one record may hold.
    let step = run.steps.iter_mut().find(|s| s.step == "run").unwrap();
    step.attempts.last_mut().unwrap().log = Some("x".repeat(33 * 1024 * 1024));
    f.service.inner().persist_terminal(&crate::plan::project_key(&f.root), &mut run);
    assert!(last_attempt(&run, "run").log.is_none(), "the record handed back is the one that was stored");
    let restarted = new_service(&f.db, &f.host, &f.batch, &f.lean, &f.research);
    let read = restarted.get(&run.id).unwrap();
    assert_eq!(read.state, WorkflowRunState::Succeeded);
    assert!(last_attempt(&read, "run").log.is_none());
    assert_eq!(last_attempt(&read, "run").outputs.len(), 3, "outputs and provenance are kept");
    assert!(read.message.as_deref().is_some_and(|m| m.contains("could not be written in full")), "{:?}", read.message);
}

/// Review finding 3: a final write that fails for a reason other than size
/// does not cost the record its logs. The whole record waits and is written
/// whole once the store works, although a reduced record would have been
/// accepted meanwhile.
#[test]
fn a_terminal_record_refused_for_another_reason_is_not_reduced() {
    let f = fixture();
    let id = f.save(numerical(false));
    // The full terminal record carries the attempt log "ok"; a reduced one would not.
    let conn = refuse_writes(&f, "NEW.state != 'running' AND instr(NEW.body, '\"log\":\"ok\"') > 0");
    let run = f.start(&id);
    assert!(f.service.wait_idle(Duration::from_secs(20)));
    assert_eq!(stored_state(&f, &run.id), "running", "nothing reduced was written in its place");
    let read = f.service.get(&run.id).unwrap();
    assert_eq!(read.state, WorkflowRunState::Succeeded);
    assert_eq!(last_attempt(&read, "run").log.as_deref(), Some("ok"));
    allow_writes(&conn);
    drop(conn);
    assert_eq!(f.service.get(&run.id).unwrap().state, WorkflowRunState::Succeeded);
    let restarted = new_service(&f.db, &f.host, &f.batch, &f.lean, &f.research);
    let read = restarted.get(&run.id).unwrap();
    assert_eq!(read.state, WorkflowRunState::Succeeded);
    assert_eq!(last_attempt(&read, "run").log.as_deref(), Some("ok"), "the whole record became durable");
    assert!(read.message.is_none(), "{:?}", read.message);
}

/// WF-11: an association decision whose record could not be written stays
/// undecided, and deciding it again is accepted once. (The evidence itself
/// is recorded idempotently by the Lean service: one record per audit and
/// declaration, `exposition-lean` `evidence_is_service_owned_idempotent…`.)
#[test]
fn an_association_decision_that_could_not_be_written_can_be_made_again() {
    let f = fixture();
    let id = f.save(formal(None));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let decide = || {
        f.service.decide_association(&WorkflowAssociationDecision {
            run: run.id.clone(),
            step: "associate".into(),
            accept: vec!["main_theorem".into()],
        })
    };
    let conn = refuse_writes(&f, "1");
    assert_eq!(decide().unwrap_err().code, "workflow.store");
    allow_writes(&conn);
    let status = |run: &WorkflowRun| match &last_attempt(run, "associate").outputs[0].value {
        WorkflowOutputValue::AssociationProposal { status, .. } => *status,
        other => panic!("{other:?}"),
    };
    assert_eq!(status(&f.service.get(&run.id).unwrap()), WorkflowProposalStatus::Pending, "nothing was decided");
    let decided = decide().unwrap();
    assert_eq!(status(&decided), WorkflowProposalStatus::Accepted);
    assert_eq!(decide().unwrap_err().code, "workflow.decided");
    // The Lean service was asked twice for the same audit and declaration.
    assert_eq!(f.lean.recorded.lock().unwrap().len(), 2);
}

/// WF-4: a plan made for a named revision is a plan of that revision. It
/// starts exactly what was reviewed even when the workflow was saved again
/// meanwhile (Run again and Reproduce rely on this); only a plan of "the
/// latest revision" becomes stale when a newer one is saved.
#[test]
fn a_plan_of_a_named_revision_runs_that_revision_after_a_newer_save() {
    let f = fixture();
    let id = f.save(numerical(false));
    let named = f.service.plan(&id, Some(1)).unwrap();
    let latest = f.service.plan(&id, None).unwrap();
    assert_eq!(named.approval_hash, latest.approval_hash);
    let mut def = f.graph.workflow(&id, None).unwrap().definition;
    def.title = "Numerical experiment v2".into();
    f.graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: Some(id.clone()), expected_revision: None, definition: def },
            &ValidationContext::default(),
        )
        .unwrap();
    let e = f.start_err(latest);
    assert_eq!(e.code, "workflow.planStale");
    assert!(e.message.contains("saved again (revision 1 → 2)"), "{}", e.message);
    let run =
        f.service.start(&StartWorkflowRunRequest { plan: named.plan, approval_hash: named.approval_hash }).unwrap();
    assert_eq!(run.revision, 1, "exactly the reviewed revision runs");
    assert!(f.service.wait_idle(Duration::from_secs(20)));
}

/// The provider and model a review was asked to run with.
fn asked_model(request: &ReviewRequest) -> Option<(String, String)> {
    request.model.as_ref().map(|m| (m.provider.clone(), m.model.clone()))
}

/// PG-3: the Retain artifact step's `label` parameter (shown in the editor,
/// reviewed in the plan, part of the approval) names the retained result.
#[test]
fn the_retain_label_reaches_the_retention_service() {
    let f = fixture();
    let mut def = numerical(false);
    def.steps[1].parameters.insert("label".into(), ParamValue::Text("  Figure 2: residuals ".into()));
    let run = f.run_to_end(&f.save(def));
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(*f.research.labels.lock().unwrap(), vec![Some("Figure 2: residuals".to_owned())]);
    // Without the parameter (or with a blank one) the result keeps its file name.
    let f = fixture();
    f.run_to_end(&f.save(numerical(false)));
    let mut blank = numerical(false);
    blank.title = "Blank label".into();
    blank.steps[1].parameters.insert("label".into(), ParamValue::Text("   ".into()));
    f.run_to_end(&f.save(blank));
    assert_eq!(*f.research.labels.lock().unwrap(), vec![None, None]);
}

/// PG-3: no registered parameter is decoration. Each one is read by the
/// adapter of its runner (or, for batches, handed to the code and the time
/// limit), so what the editor offers and the plan shows is what runs.
#[test]
fn every_registered_parameter_is_read_by_its_runner() {
    let adapters = [
        include_str!("../adapters/research.rs"),
        include_str!("../adapters/associate.rs"),
        include_str!("../adapters/agent.rs"),
        include_str!("../adapters/lean.rs"),
    ]
    .concat();
    let batch = include_str!("../adapters/batch.rs");
    let plan = include_str!("../plan.rs");
    for operation in exposition_graph::workflow::registry::operations() {
        for parameter in &operation.parameters {
            let name = &parameter.name;
            let code =
                matches!(operation.payload, Some(exposition_graph::workflow::registry::PayloadSpec::Code { .. }));
            let read = if code {
                // A batch gets every parameter as EXPOSITION_PARAM_<NAME>;
                // `timeoutSeconds` is also its time limit.
                batch.contains("parameter_texts(&request.step.parameters)")
                    && (name != "timeoutSeconds" || plan.contains("\"timeoutSeconds\""))
            } else {
                adapters.contains(&format!("text_parameter(\"{name}\")"))
            };
            assert!(read, "{}: parameter `{name}` is registered but nothing reads it", operation.id);
        }
    }
}

/// WF-12: a listener that falls behind loses events (the channel keeps the
/// latest 256), among them a run's final one; the resync events name every
/// run with its state now, so a view that re-reads on events catches up.
#[test]
fn a_listener_that_fell_behind_is_told_every_runs_current_state() {
    use tokio::sync::broadcast::error::TryRecvError;
    let f = fixture();
    let id = f.save(numerical(false));
    let mut events = f.service.subscribe();
    let first = f.run_to_end(&id);
    let mut last = first.id.clone();
    for _ in 0..60 {
        last = f.run_to_end(&id).id;
    }
    // Nothing was read meanwhile: the oldest events are gone.
    assert!(matches!(events.try_recv(), Err(TryRecvError::Lagged(_))), "more than 256 events were sent");
    let mut kept = Vec::new();
    while let Ok(event) = events.try_recv() {
        kept.push(event);
    }
    assert!(kept.iter().all(|e| e.run != first.id), "the first run's final event was lost");
    assert!(kept.iter().any(|e| e.run == last && e.state == WorkflowRunState::Succeeded));
    let resync = f.service.resync_events();
    let state_of = |run: &WorkflowRunId| resync.iter().find(|e| &e.run == run).map(|e| e.state);
    assert_eq!(state_of(&first.id), Some(WorkflowRunState::Succeeded), "the lost final state is announced again");
    assert_eq!(state_of(&last), Some(WorkflowRunState::Succeeded));
    assert!(resync.iter().all(|e| e.step.is_none() && e.workflow == id));
}
