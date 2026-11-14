//! M9 bounded control flow: conditions (verdict, JSON comparison, output
//! present, negation, skip propagation, unreadable sources), repeats (forEach
//! instances in parallel and collected in order, until iterations with
//! exhaustion), and composite expansion in the plan.

use exposition_graph::workflow::{
    COMPOSITE_OPERATION, CompareOp, CompositeRef, ConditionTest, ConditionValue, Exhausted, StepCondition, StepRepeat,
    WORKFLOW_SCHEMA_V2, WorkflowExecutionOptions,
};

use super::*;
use crate::adapters::StepIteration;

fn v2(mut def: WorkflowDefinition, max_parallel: u32) -> WorkflowDefinition {
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.execution = Some(WorkflowExecutionOptions { max_parallel });
    def
}

fn when(step: &str, port: &str, test: ConditionTest, negate: bool) -> Option<StepCondition> {
    Some(StepCondition { source: StepPort { step: step.into(), port: port.into() }, test, negate })
}

fn json_ge(pointer: &str, value: &str) -> ConditionTest {
    ConditionTest::JsonCompare {
        pointer: pointer.into(),
        op: CompareOp::Ge,
        value: ConditionValue::Number(value.into()),
    }
}

/// Restricted steps `ids` reading the `data` input; with `join`, a step
/// consuming every one's dataset on its `multiple` port.
fn fan(ids: &[&str], join: Option<&str>) -> WorkflowDefinition {
    let mut def = numerical(false);
    def.steps = ids.iter().map(|id| python_step(id, false)).collect();
    def.connections = ids.iter().map(|id| data_edge(PortSource::Input { input: "data".into() }, id)).collect();
    def.outcomes = ids.iter().map(|id| dataset_outcome(id)).collect();
    if let Some(join) = join {
        def.steps.push(python_step(join, false));
        def.connections.extend(ids.iter().map(|id| data_edge(out(id), join)));
        def.outcomes = vec![dataset_outcome(join)];
    }
    def
}

fn step_of<'a>(run: &'a WorkflowRun, id: &str) -> &'a WorkflowRunStep {
    run.steps.iter().find(|s| s.step == id).unwrap()
}

fn state_of(run: &WorkflowRun, id: &str) -> WorkflowStepState {
    step_of(run, id).state
}

/// Each batch writes `dataset.json` with the JSON its step's code names
/// after `# json:` (default `{}`), and a figure only when the code says so.
fn json_behavior() -> Arc<BatchBehavior> {
    Arc::new(|root, req, _control, run| {
        let code = match &req.code {
            BatchCode::Inline { source } => source.clone(),
            BatchCode::Script { .. } => String::new(),
        };
        let json = code.lines().find_map(|l| l.strip_prefix("# json:")).unwrap_or("{}").trim().to_owned();
        let mut ports = vec![artifact(root, run, "dataset", "dataset.json", json.as_bytes(), ArtifactKind::Data)];
        if code.contains("# figure") {
            ports.push(artifact(root, run, "figure", "figure.svg", b"<svg/>", ArtifactKind::Figure));
        }
        Ok(succeeded(run, ports))
    })
}

fn code_step(id: &str, code: &str) -> WorkflowStep {
    let mut s = python_step(id, false);
    s.payload = Some(StepPayload::Code { language: CodeLanguage::Python, source: format!("# {id}\n{code}\n") });
    s
}

// ------------------------------------------------------------ conditions ----

#[test]
fn json_conditions_run_or_skip_steps_and_skips_propagate() {
    let f = fixture();
    f.set_behavior(json_behavior());
    let mut def = numerical(false);
    def.steps = vec![
        code_step("fit", "# json: {\"r2\": 0.95, \"model\": \"linear\"}"),
        code_step("good", ""),
        code_step("refit", ""),
        code_step("after", ""),
    ];
    def.steps[1].condition = when("fit", "dataset", json_ge("/r2", "0.9"), false);
    // Runs only when the fit is poor: not this time.
    def.steps[2].condition = when("fit", "dataset", json_ge("/r2", "0.9"), true);
    def.connections = vec![
        data_edge(PortSource::Input { input: "data".into() }, "fit"),
        data_edge(out("fit"), "good"),
        data_edge(out("fit"), "refit"),
        data_edge(out("refit"), "after"),
    ];
    def.outcomes = vec![dataset_outcome("good"), dataset_outcome("after")];
    let id = f.save(v2(def, 1));
    let plan = f.service.plan(&id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert!(plan.steps.iter().find(|s| s.step == "good").unwrap().condition.is_some(), "the plan shows the condition");
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(state_of(&run, "good"), WorkflowStepState::Succeeded);
    assert_eq!(state_of(&run, "refit"), WorkflowStepState::ConditionNotMet);
    let reason = step_of(&run, "refit").reason.clone().unwrap();
    assert!(reason.contains("/r2") && reason.contains("0.95") && reason.contains("does not hold"), "{reason}");
    assert!(step_of(&run, "refit").attempts.is_empty(), "never dispatched");
    assert_eq!(state_of(&run, "after"), WorkflowStepState::Skipped);
    assert!(step_of(&run, "after").reason.as_deref().unwrap().contains("“Python refit”"));
    let after = run.outcomes.iter().find(|o| o.step == "after").unwrap();
    assert!(after.value.is_none() && after.reason.as_deref().unwrap().contains("did not run"), "{after:?}");
    assert_eq!(f.calls.log(), vec!["batch:fit".to_owned(), "batch:good".into()]);
    assert_eq!(run.usage.steps, 2, "steps not run use no budget");
}

#[test]
fn an_unreadable_condition_source_fails_the_step_clearly() {
    for (pointer, code) in [("/missing", "workflow.conditionUnreadable"), ("/model", "workflow.conditionUnreadable")] {
        let f = fixture();
        f.set_behavior(json_behavior());
        let mut def = numerical(false);
        def.steps = vec![code_step("fit", "# json: {\"model\": \"linear\"}"), code_step("next", "")];
        def.steps[1].condition = when("fit", "dataset", json_ge(pointer, "1"), false);
        def.connections =
            vec![data_edge(PortSource::Input { input: "data".into() }, "fit"), data_edge(out("fit"), "next")];
        def.outcomes = vec![dataset_outcome("next")];
        let run = f.run_to_end(&f.save(v2(def, 1)));
        assert_eq!(run.state, WorkflowRunState::Failed);
        assert_eq!(run.code.as_deref(), Some(code), "{pointer}: {:?}", run.message);
        assert_eq!(state_of(&run, "next"), WorkflowStepState::Failed);
        let attempt = last_attempt(&run, "next");
        assert!(attempt.dispatched_at_ms.is_none(), "never dispatched");
        assert!(attempt.message.as_deref().unwrap().contains("condition could not be evaluated"));
        assert_eq!(f.calls.log(), vec!["batch:fit".to_owned()]);
    }
}

#[test]
fn a_verdict_condition_reads_the_audit_and_never_a_report() {
    for (verdicts, negate, runs) in
        [(vec!["Checked"], false, false), (vec!["Unverified"], false, true), (vec!["Checked"], true, true)]
    {
        let f = fixture();
        let mut def = formal(None);
        def.steps.push(code_step("followup", ""));
        let n = def.steps.len() - 1;
        def.steps[n].condition = when(
            "audit",
            "evidence",
            ConditionTest::VerdictIn { verdicts: verdicts.iter().map(|v| v.to_string()).collect() },
            negate,
        );
        def.outcomes.push(dataset_outcome("followup"));
        def.ceiling.execution = ExecutionCeiling::TrustedNative;
        let run = f.run_to_end(&f.save(v2(def, 1)));
        assert_eq!(run.state, WorkflowRunState::Succeeded, "{verdicts:?}: {:?}", run.message);
        let expected = if runs { WorkflowStepState::Succeeded } else { WorkflowStepState::ConditionNotMet };
        assert_eq!(state_of(&run, "followup"), expected, "{verdicts:?} negate {negate}");
        if !runs {
            let reason = step_of(&run, "followup").reason.clone().unwrap();
            assert!(reason.contains("one of “Checked”") && reason.contains("is “Unverified: test”"), "{reason}");
        }
        // The audit runs first even though nothing else orders them.
        assert_eq!(f.calls.log()[0], "lean:Proofs/Main.lean");
    }
}

#[test]
fn output_present_conditions_and_their_negation() {
    let f = fixture();
    f.set_behavior(json_behavior());
    let mut def = numerical(false);
    def.steps = vec![code_step("a", "# no figure"), code_step("plot", ""), code_step("fallback", "")];
    def.steps[1].condition = when("a", "figure", ConditionTest::OutputPresent, false);
    def.steps[2].condition = when("a", "figure", ConditionTest::OutputPresent, true);
    def.connections = vec![
        data_edge(PortSource::Input { input: "data".into() }, "a"),
        data_edge(PortSource::Input { input: "data".into() }, "plot"),
        data_edge(PortSource::Input { input: "data".into() }, "fallback"),
    ];
    def.outcomes = vec![dataset_outcome("a"), dataset_outcome("plot"), dataset_outcome("fallback")];
    let run = f.run_to_end(&f.save(v2(def, 2)));
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(state_of(&run, "plot"), WorkflowStepState::ConditionNotMet);
    assert!(step_of(&run, "plot").reason.as_deref().unwrap().contains("was not produced"));
    assert_eq!(state_of(&run, "fallback"), WorkflowStepState::Succeeded);
}

#[test]
fn a_condition_orders_its_step_after_its_source() {
    let f = fixture();
    f.set_behavior(json_behavior());
    let mut def = numerical(false);
    // `late` reads `early`'s output only through its condition.
    def.steps = vec![code_step("late", ""), code_step("early", "# json: {\"go\": 1}")];
    def.steps[0].condition = when("early", "dataset", json_ge("/go", "1"), false);
    def.connections = vec![
        data_edge(PortSource::Input { input: "data".into() }, "late"),
        data_edge(PortSource::Input { input: "data".into() }, "early"),
    ];
    def.outcomes = vec![dataset_outcome("late"), dataset_outcome("early")];
    let run = f.run_to_end(&f.save(v2(def, 2)));
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(f.calls.log(), vec!["batch:early".to_owned(), "batch:late".into()]);
}

// --------------------------------------------------------------- repeats ----

/// The instance, time limit, and parameters an attempt was given.
type Seen = (Option<StepIteration>, Duration, BTreeMap<String, ParamValue>);

/// Wraps an adapter and records the instance every attempt was given.
struct Recording {
    inner: Arc<dyn StepAdapter>,
    seen: Mutex<Vec<Seen>>,
}

impl StepAdapter for Recording {
    fn availability(&self, root: &Path) -> Result<(), String> {
        self.inner.availability(root)
    }
    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        self.seen.lock().unwrap().push((
            request.iteration.clone(),
            request.time_limit,
            request.step.parameters.clone(),
        ));
        self.inner.run(request, control)
    }
}

fn recording(f: &Fixture) -> (WorkflowRunService, Arc<Recording>) {
    let a = adapters(&f.batch, &f.lean, &f.research);
    let rec = Arc::new(Recording { inner: a[runner_ids::RESTRICTED_PYTHON].clone(), seen: Mutex::new(Vec::new()) });
    let service = service_with(f, runner_ids::RESTRICTED_PYTHON, rec.clone());
    (service, rec)
}

fn run_on(service: &WorkflowRunService, id: &WorkflowId) -> WorkflowRun {
    let plan = service.plan(id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let run = service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap();
    assert!(service.wait_idle(Duration::from_secs(30)), "run did not finish");
    service.get(&run.id).unwrap()
}

fn for_each_timeouts(values: &[i64]) -> Option<StepRepeat> {
    Some(StepRepeat::ForEach {
        parameter: "timeoutSeconds".into(),
        values: values.iter().map(|v| ParamValue::Integer(*v)).collect(),
    })
}

#[test]
fn for_each_runs_one_instance_per_value_and_collects_outputs_in_order() {
    let f = fixture();
    let gauge = Arc::new(Mutex::new((0usize, 0usize)));
    let g = gauge.clone();
    let inner = default_behavior();
    f.set_behavior(Arc::new(move |root, req, control, run| {
        if req.owner.step == "sweep" {
            let mut x = g.lock().unwrap();
            x.0 += 1;
            x.1 = x.1.max(x.0);
        }
        std::thread::sleep(Duration::from_millis(60));
        if req.owner.step == "sweep" {
            g.lock().unwrap().0 -= 1;
        }
        inner(root, req, control, run)
    }));
    let (service, rec) = recording(&f);
    let mut def = fan(&["sweep"], Some("join"));
    def.steps[0].repeat = for_each_timeouts(&[30, 10, 20]);
    let id = f.save(v2(def, 4));
    let plan = service.plan(&id, None).unwrap();
    let sweep = plan.steps.iter().find(|s| s.step == "sweep").unwrap();
    assert_eq!(sweep.time_limit_seconds, 30, "the longest instance limit is shown");
    assert_eq!(plan.budgets.max_steps, 4, "every instance counts against maxSteps");
    let run = run_on(&service, &id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let step = step_of(&run, "sweep");
    assert_eq!(step.state, WorkflowStepState::Succeeded);
    assert!(step.attempts.is_empty(), "a repeated step's attempts live in its instances");
    let values: Vec<Option<ParamValue>> = step.instances.iter().map(|i| i.parameter.clone()).collect();
    assert_eq!(
        values,
        vec![Some(ParamValue::Integer(30)), Some(ParamValue::Integer(10)), Some(ParamValue::Integer(20))]
    );
    assert!(step.instances.iter().all(|i| i.state == WorkflowStepState::Succeeded && i.attempts.len() == 1));
    for (k, instance) in step.instances.iter().enumerate() {
        assert!(instance.attempts[0].outputs.iter().all(|o| o.provenance.instance == Some(k as u32)));
    }
    assert_eq!(gauge.lock().unwrap().1, 2, "instances ran in parallel within the restricted limit");
    assert_eq!(run.usage.steps, 4);
    // Adapters were told the instance, with its value applied.
    let mut seen = rec.seen.lock().unwrap().clone();
    seen.retain(|s| s.0.is_some());
    seen.sort_by_key(|s| s.0.as_ref().unwrap().index);
    let limits: Vec<u64> = seen.iter().map(|s| s.1.as_secs()).collect();
    assert_eq!(limits, vec![30, 10, 20]);
    assert_eq!(
        seen[1].0,
        Some(StepIteration { index: 1, parameter: Some(("timeoutSeconds".into(), ParamValue::Integer(10))) })
    );
    assert_eq!(seen[1].2["timeoutSeconds"], ParamValue::Integer(10));
    // The consumer's multiple port got every instance's dataset, in index order.
    let paths: Vec<String> = step
        .instances
        .iter()
        .map(|i| match &i.attempts[0].outputs.iter().find(|o| o.port == "dataset").unwrap().value {
            WorkflowOutputValue::File { path, .. } => path.to_string(),
            other => panic!("{other:?}"),
        })
        .collect();
    let requests = f.batch.requests.lock().unwrap();
    let join: Vec<String> =
        requests.last().unwrap().2.iter().map(|i| i.split('@').next().unwrap().to_owned()).collect();
    assert_eq!(join, paths);
}

#[test]
fn a_failed_instance_fails_the_step_and_later_instances_never_start() {
    let f = fixture();
    let inner = default_behavior();
    f.set_behavior(Arc::new(move |root, req, control, run| {
        if req.time_limit == Duration::from_secs(20) {
            let mut r = succeeded(run, Vec::new());
            r.status = RunStatus::Failed { message: "instance broke".into() };
            return Ok(r);
        }
        inner(root, req, control, run)
    }));
    let mut def = fan(&["sweep"], Some("join"));
    def.steps[0].repeat = for_each_timeouts(&[10, 20, 30, 40]);
    let run = f.run_to_end(&f.save(v2(def, 1)));
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert!(run.message.as_deref().unwrap().contains("instance broke"));
    let step = step_of(&run, "sweep");
    assert_eq!(step.state, WorkflowStepState::Failed);
    let states: Vec<WorkflowStepState> = step.instances.iter().map(|i| i.state).collect();
    assert_eq!(
        states,
        vec![
            WorkflowStepState::Succeeded,
            WorkflowStepState::Failed,
            WorkflowStepState::Skipped,
            WorkflowStepState::Skipped
        ]
    );
    assert!(step.instances[3].reason.as_deref().unwrap().contains("instance"));
    assert_eq!(f.batch.requests.lock().unwrap().len(), 2);
}

#[test]
fn until_iterates_until_its_predicate_holds() {
    let f = fixture();
    let n = Arc::new(AtomicUsize::new(0));
    let c = n.clone();
    f.set_behavior(Arc::new(move |root, _req, _control, run| {
        let i = c.fetch_add(1, Ordering::SeqCst);
        let json = format!("{{\"found\": {}}}", i == 2);
        Ok(succeeded(run, vec![artifact(root, run, "dataset", "dataset.json", json.as_bytes(), ArtifactKind::Data)]))
    }));
    let (service, rec) = recording(&f);
    let mut def = fan(&["search"], Some("use"));
    def.steps[0].repeat = Some(StepRepeat::Until {
        port: "dataset".into(),
        test: ConditionTest::JsonCompare {
            pointer: "/found".into(),
            op: CompareOp::Eq,
            value: ConditionValue::Boolean(true),
        },
        negate: false,
        max_iterations: 5,
        on_exhausted: Exhausted::Fail,
    });
    let id = f.save(v2(def, 1));
    assert_eq!(service.plan(&id, None).unwrap().budgets.max_steps, 6);
    let run = run_on(&service, &id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let step = step_of(&run, "search");
    assert_eq!(step.instances.len(), 3, "stopped at the first iteration whose predicate held");
    assert!(step.instances[0].reason.as_deref().unwrap().contains("did not hold"));
    assert!(step.instances[2].reason.as_deref().unwrap().contains("held"));
    let indexes: Vec<u32> = rec.seen.lock().unwrap().iter().filter_map(|s| s.0.as_ref().map(|i| i.index)).collect();
    assert_eq!(indexes, vec![0, 1, 2], "each iteration is told its index");
    // The consumer got the iteration whose predicate held.
    let used = match &step.instances[2].attempts[0].outputs[0].value {
        WorkflowOutputValue::File { path, .. } => path.to_string(),
        other => panic!("{other:?}"),
    };
    let requests = f.batch.requests.lock().unwrap();
    assert!(requests.last().unwrap().2[0].starts_with(&used));
    assert_eq!(run.usage.steps, 4);
}

#[test]
fn an_exhausted_until_fails_or_uses_its_last_iteration() {
    for exhausted in [Exhausted::Fail, Exhausted::UseLast] {
        let f = fixture();
        f.set_behavior(json_behavior());
        let mut def = fan(&["search"], None);
        def.steps[0] = code_step("search", "# json: {\"found\": false}");
        def.steps[0].repeat = Some(StepRepeat::Until {
            port: "dataset".into(),
            test: ConditionTest::JsonCompare {
                pointer: "/found".into(),
                op: CompareOp::Eq,
                value: ConditionValue::Boolean(true),
            },
            negate: false,
            max_iterations: 3,
            on_exhausted: exhausted,
        });
        let run = f.run_to_end(&f.save(v2(def, 1)));
        let step = step_of(&run, "search");
        assert_eq!(step.instances.len(), 3);
        assert_eq!(f.batch.requests.lock().unwrap().len(), 3, "never more than maxIterations");
        match exhausted {
            Exhausted::Fail => {
                assert_eq!(run.state, WorkflowRunState::Failed);
                assert_eq!(run.code.as_deref(), Some("workflow.untilExhausted"));
                assert_eq!(step.state, WorkflowStepState::Failed);
            }
            Exhausted::UseLast => {
                assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
                assert!(step.reason.as_deref().unwrap().contains("last iteration"));
                assert!(run.outcomes[0].value.is_some(), "the last iteration realizes the outcome");
            }
        }
    }
}

#[test]
fn canceling_or_restarting_with_instances_in_flight() {
    for restart in [false, true] {
        let f = fixture();
        let started = Arc::new(AtomicUsize::new(0));
        let s = started.clone();
        f.set_behavior(Arc::new(move |_root, _req, control, run| {
            s.fetch_add(1, Ordering::SeqCst);
            while !(control.canceled)() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut r = succeeded(run, Vec::new());
            r.status = RunStatus::Canceled;
            Ok(r)
        }));
        let mut def = fan(&["sweep"], Some("join"));
        def.steps[0].repeat = for_each_timeouts(&[10, 20, 30]);
        let run = f.start(&f.save(v2(def, 3)));
        for _ in 0..2000 {
            if started.load(Ordering::SeqCst) == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(started.load(Ordering::SeqCst), 2, "restricted limit 2");
        let after = if restart {
            std::thread::sleep(Duration::from_millis(50));
            new_service(&f.db, &f.host, &f.batch, &f.lean, &f.research).get(&run.id).unwrap()
        } else {
            f.service.cancel(&run.id).unwrap()
        };
        let step = step_of(&after, "sweep");
        let states: Vec<WorkflowStepState> = step.instances.iter().map(|i| i.state).collect();
        let stopped = if restart { WorkflowStepState::Interrupted } else { WorkflowStepState::Canceled };
        assert_eq!(states, vec![stopped, stopped, WorkflowStepState::Skipped], "restart {restart}");
        assert!(step.instances.iter().all(|i| i.reason.is_some() || i.state == WorkflowStepState::Canceled));
        if restart {
            assert_eq!(after.state, WorkflowRunState::Interrupted);
            assert!(step.instances[0].attempts.iter().all(|a| a.state == WorkflowAttemptState::Interrupted));
        } else {
            assert_eq!(after.state, WorkflowRunState::Canceled);
        }
    }
}

// ------------------------------------------------------------ composites ----

/// A saved child workflow: input `raw` → clean → fit; outcome `model`.
fn child(f: &Fixture) -> WorkflowId {
    let mut def = numerical(false);
    def.title = "Model fitting".into();
    def.inputs[0].id = "raw".into();
    def.inputs[0].source = InputSource::Unset;
    def.steps = vec![code_step("clean", "print('clean')"), code_step("fit", "print('fit')")];
    def.connections =
        vec![data_edge(PortSource::Input { input: "raw".into() }, "clean"), data_edge(out("clean"), "fit")];
    def.outcomes = vec![WorkflowOutcome {
        id: "model".into(),
        kind: OutcomeKind::TransformedDataset,
        label: "Model".into(),
        from: StepPort { step: "fit".into(), port: "dataset".into() },
        optional: false,
    }];
    f.save(def)
}

fn composite_step(id: &str, label: &str, workflow: &WorkflowId, revision: u64) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: label.into(),
        operation: StepOperation { id: COMPOSITE_OPERATION.into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
        composite: Some(CompositeRef { workflow: workflow.clone(), revision }),
        condition: None,
        repeat: None,
    }
}

/// prep → comp (the child) → report.
fn parent(child: &WorkflowId) -> WorkflowDefinition {
    let mut def = numerical(false);
    def.steps = vec![code_step("prep", ""), composite_step("comp", "Fit the model", child, 1), code_step("report", "")];
    def.connections = vec![
        data_edge(PortSource::Input { input: "data".into() }, "prep"),
        WorkflowConnection::Data { from: out("prep"), to: StepPort { step: "comp".into(), port: "raw".into() } },
        data_edge(PortSource::Step { step: "comp".into(), port: "model".into() }, "report"),
    ];
    def.outcomes = vec![dataset_outcome("report")];
    v2(def, 1)
}

#[test]
fn a_composite_runs_its_child_steps_in_place() {
    let f = fixture();
    f.set_behavior(json_behavior());
    let child = child(&f);
    let id = f.save(parent(&child));
    let plan = f.service.plan(&id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let ids: Vec<&str> = plan.steps.iter().map(|s| s.step.as_str()).collect();
    assert_eq!(ids, vec!["prep", "comp/clean", "comp/fit", "report"]);
    let clean = &plan.steps[1];
    assert_eq!(clean.composite_path, vec!["Fit the model".to_owned()]);
    assert!(
        matches!(&clean.payload, Some(StepPayload::Code { source, .. }) if source.contains("print('clean')")),
        "the child's code in full"
    );
    assert_eq!(clean.runner, "restrictedPython");
    assert_eq!(clean.bindings[0].from, out("prep"), "the child's input receives what the parent binds");
    assert_eq!(plan.steps[2].bindings[0].from, out("comp/clean"));
    assert_eq!(plan.steps[3].bindings[0].from, out("comp/fit"), "the composite's output is the child's outcome");
    assert!(plan.steps[3].composite_path.is_empty());
    assert_eq!(plan.budgets.max_steps, 4);
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(f.calls.log(), vec!["batch:prep", "batch:comp/clean", "batch:comp/fit", "batch:report"]);
    assert_eq!(last_attempt(&run, "comp/fit").outputs[0].provenance.step, "comp/fit");
    // A new child revision never changes the pinned reference.
    let mut edited = f.graph.workflow(&child, None).unwrap().definition;
    if let Some(StepPayload::Code { source, .. }) = &mut edited.steps[0].payload {
        source.push_str(
            "# changed
",
        );
    }
    f.graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: Some(child.clone()), expected_revision: None, definition: edited },
            &ValidationContext::default(),
        )
        .unwrap();
    let same = f.service.plan(&id, None).unwrap();
    assert_eq!(same.approval_hash, plan.approval_hash);
    // Pinning revision 2 (a parent edit) is another approval with the new code.
    let mut pinned = f.graph.workflow(&id, None).unwrap().definition;
    pinned.steps[1].composite.as_mut().unwrap().revision = 2;
    let two = f.service.plan(&f.save(pinned), None).unwrap();
    assert!(matches!(&two.steps[1].payload, Some(StepPayload::Code { source, .. }) if source.contains("# changed")));
    assert_ne!(two.approval_hash, plan.approval_hash);
}

#[test]
fn composite_refusals_block_the_plan() {
    let f = fixture();
    let child = child(&f);
    let blocked = |def: WorkflowDefinition| {
        let plan = f.service.plan(&f.save(def), None).unwrap();
        assert!(!plan.blockers.is_empty(), "blocked");
        plan.blockers.iter().map(|b| b.message.clone()).collect::<Vec<_>>().join(" | ")
    };
    // A revision that does not exist.
    let mut missing = parent(&child);
    missing.steps[1].composite.as_mut().unwrap().revision = 9;
    let m = blocked(missing);
    assert!(m.contains("revision 9") || m.contains("missing") || m.contains("does not exist"), "{m}");
    // Nested four levels deep.
    let mut inner = child.clone();
    for level in 0..3 {
        let mut wrap = numerical(false);
        wrap.inputs[0].id = "raw".into();
        wrap.inputs[0].source = InputSource::Unset;
        wrap.steps = vec![composite_step("c", &format!("Level {level}"), &inner, 1)];
        wrap.connections = vec![WorkflowConnection::Data {
            from: PortSource::Input { input: "raw".into() },
            to: StepPort { step: "c".into(), port: "raw".into() },
        }];
        wrap.outcomes = vec![WorkflowOutcome {
            id: "model".into(),
            kind: OutcomeKind::TransformedDataset,
            label: "Model".into(),
            from: StepPort { step: "c".into(), port: "model".into() },
            optional: false,
        }];
        inner = f.save(v2(wrap, 1));
    }
    let m = blocked(parent(&inner));
    assert!(m.contains("deep") || m.contains("levels"), "{m}");
    // Nothing ran.
    assert!(f.calls.log().is_empty());
}

#[test]
fn a_preview_plan_is_not_kept_and_cannot_be_started() {
    let f = fixture();
    let id = f.save(numerical(false));
    let preview = f.service.preview_plan(&id, None, &crate::PlanOptions::default()).unwrap();
    let kept = f.service.plan(&id, None).unwrap();
    assert_eq!(preview.approval_hash, kept.approval_hash, "the same plan");
    let request = StartWorkflowRunRequest { plan: preview.plan, approval_hash: preview.approval_hash };
    assert_eq!(f.service.start(&request).unwrap_err().code, "workflow.planStale");
    assert!(f.calls.log().is_empty());
}

#[test]
fn the_plan_states_conditions_and_repeats_in_plain_words() {
    let f = fixture();
    let mut def = fan(&["sweep"], Some("join"));
    def.steps[0].repeat = for_each_timeouts(&[10, 20]);
    def.steps[1].condition = when("sweep", "dataset", ConditionTest::OutputPresent, false);
    // A condition cannot read a forEach step: validation refuses it.
    let refused = f.service.plan(&f.save(v2(def, 2)), None).unwrap();
    assert!(!refused.blockers.is_empty());
    let mut def = fan(&["fit"], Some("next"));
    def.steps[1].condition = when("fit", "dataset", json_ge("/r2", "0.9"), true);
    def.steps[0].repeat = Some(StepRepeat::Until {
        port: "dataset".into(),
        test: json_ge("/r2", "0.9"),
        negate: false,
        max_iterations: 4,
        on_exhausted: Exhausted::UseLast,
    });
    let plan = f.service.plan(&f.save(v2(def, 2)), None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    let text = plan.notices.join("\n");
    assert!(text.contains("“Python next” runs only when the value at /r2 ≥ 0.9 does not hold"), "{text}");
    assert!(text.contains("“Python fit” runs again, at most 4 times, until the value at /r2 ≥ 0.9"), "{text}");
    assert!(text.contains("the last iteration's outputs are used"), "{text}");
}

// ---------------------------------------------------------- audit fixes ----

/// M9 audit 2: an association step fed by a repeated (until) audit can be
/// decided; the audit used is the iteration the step consumed.
#[test]
fn an_association_fed_by_an_until_audit_can_be_decided() {
    let f = fixture();
    let mut def = formal(None);
    def.steps[0].repeat = Some(StepRepeat::Until {
        port: "evidence".into(),
        test: ConditionTest::VerdictIn { verdicts: vec!["Unverified".into()] },
        negate: false,
        max_iterations: 2,
        on_exhausted: Exhausted::Fail,
    });
    def.ceiling.execution = ExecutionCeiling::TrustedNative;
    let run = f.run_to_end(&f.save(v2(def, 1)));
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(step_of(&run, "audit").instances.len(), 1);
    let decided = f
        .service
        .decide_association(&WorkflowAssociationDecision {
            run: run.id.clone(),
            step: "associate".into(),
            accept: vec!["main_theorem".into()],
        })
        .unwrap();
    let WorkflowOutputValue::AssociationProposal { status, accepted, .. } =
        &last_attempt(&decided, "associate").outputs[0].value
    else {
        panic!("proposal")
    };
    assert_eq!(*status, WorkflowProposalStatus::Accepted);
    assert_eq!(accepted, &vec!["main_theorem".to_owned()]);
    assert_eq!(*f.lean.recorded.lock().unwrap(), vec![vec!["main_theorem".to_owned()]]);
}

/// M9 audit 3: plan and run wording is accurate for repeats, conditions,
/// prerequisites, and an until predicate that cannot be evaluated.
#[test]
fn plan_and_run_wording_is_accurate() {
    let f = fixture();
    f.set_behavior(json_behavior());
    // A plan with a repeat does not promise that nothing is repeated.
    let mut def = fan(&["sweep"], Some("join"));
    def.steps[0].repeat = for_each_timeouts(&[10, 20]);
    let plan = f.service.plan(&f.save(v2(def, 1)), None).unwrap();
    assert!(!plan.notices[0].contains("nothing is retried or repeated"), "{}", plan.notices[0]);
    assert!(plan.notices[0].contains("nothing is retried"), "{}", plan.notices[0]);
    let plan = f.service.plan(&f.save(chain()), None).unwrap();
    assert!(plan.notices[0].contains("nothing is retried or repeated"), "M8 wording kept: {}", plan.notices[0]);
    // The condition notice names the state the step gets, and reads well negated.
    let mut def = numerical(false);
    def.steps = vec![code_step("fit", "# json: {\"r2\": 0.5}"), code_step("refit", ""), code_step("after", "")];
    def.steps[1].condition = when("fit", "dataset", json_ge("/r2", "0.9"), true);
    def.connections = vec![
        data_edge(PortSource::Input { input: "data".into() }, "fit"),
        data_edge(out("fit"), "refit"),
        data_edge(PortSource::Input { input: "data".into() }, "after"),
        WorkflowConnection::Prerequisite { before: "refit".into(), after: "after".into() },
    ];
    def.outcomes = vec![dataset_outcome("refit"), dataset_outcome("after")];
    let id = f.save(v2(def, 1));
    let plan = f.service.plan(&id, None).unwrap();
    let notice = plan.notices.iter().find(|n| n.starts_with("“Python refit” runs only")).unwrap();
    assert!(!notice.contains("unless"), "{notice}");
    assert!(notice.contains("does not hold") && notice.contains("condition not met"), "{notice}");
    assert!(!notice.contains("it is skipped"), "{notice}");
    // A dependent that only waits for the step (a prerequisite) is not told
    // it needed that step's output.
    let mut def = f.graph.workflow(&id, None).unwrap().definition;
    def.steps[0] = code_step("fit", "# json: {\"r2\": 0.95}");
    let run = f.run_to_end(&f.save(def));
    assert_eq!(state_of(&run, "refit"), WorkflowStepState::ConditionNotMet);
    assert_eq!(state_of(&run, "after"), WorkflowStepState::Skipped);
    let reason = step_of(&run, "after").reason.clone().unwrap();
    assert!(!reason.contains("output it needs") && reason.contains("runs after"), "{reason}");
    // An until predicate that cannot be evaluated: the iteration does not
    // read as a success while the step fails.
    let mut def = fan(&["search"], Some("use"));
    def.steps[0] = code_step("search", "# json: {\"other\": 1}");
    def.steps[0].repeat = Some(StepRepeat::Until {
        port: "dataset".into(),
        test: json_ge("/found", "1"),
        negate: false,
        max_iterations: 3,
        on_exhausted: Exhausted::Fail,
    });
    let run = f.run_to_end(&f.save(v2(def, 1)));
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert_eq!(run.code.as_deref(), Some("workflow.conditionUnreadable"), "{:?}", run.message);
    let step = step_of(&run, "search");
    assert_eq!(step.state, WorkflowStepState::Failed);
    assert_eq!(step.instances.len(), 1);
    assert_eq!(step.instances[0].state, WorkflowStepState::Failed, "the iteration that could not be judged failed");
    assert_eq!(step.instances[0].attempts[0].state, WorkflowAttemptState::Failed);
    assert!(step.instances[0].attempts[0].message.as_deref().unwrap().contains("predicate"));
}

/// M9 audit 4: a step skipped by its own condition finished as planned.
#[test]
fn run_summaries_count_condition_not_met_steps_as_done() {
    let f = fixture();
    f.set_behavior(json_behavior());
    let mut def = numerical(false);
    def.steps = vec![code_step("fit", "# json: {\"r2\": 0.5}"), code_step("publish", "")];
    def.steps[1].condition = when("fit", "dataset", json_ge("/r2", "0.9"), false);
    def.connections =
        vec![data_edge(PortSource::Input { input: "data".into() }, "fit"), data_edge(out("fit"), "publish")];
    def.outcomes = vec![dataset_outcome("fit"), dataset_outcome("publish")];
    let id = f.save(v2(def, 1));
    let run = f.run_to_end(&id);
    assert_eq!(state_of(&run, "publish"), WorkflowStepState::ConditionNotMet);
    let summary = f.service.runs(Some(&id)).unwrap().into_iter().find(|s| s.id == run.id).unwrap();
    assert_eq!((summary.steps_done, summary.steps_total), (2, 2));
}
