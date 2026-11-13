//! M9 parallel scheduler: overlap, per-run and global limits, budget
//! reservation, cancellation/restart with several steps in flight, durable
//! concurrent updates, multi-agent reviews.

use std::time::Instant;

use exposition_graph::workflow::{WORKFLOW_SCHEMA_V2, WorkflowExecutionOptions};

use super::*;
use crate::store::RunStore;

/// Meets `need` parties (true), or gives up after 10 s (false: they never
/// executed at the same time).
struct Rendezvous {
    arrived: AtomicUsize,
    need: usize,
}

impl Rendezvous {
    fn new(need: usize) -> Arc<Self> {
        Arc::new(Self { arrived: AtomicUsize::new(0), need })
    }

    fn meet(&self) -> bool {
        self.arrived.fetch_add(1, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.arrived.load(Ordering::SeqCst) >= self.need {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

/// Current and highest number of concurrent calls per key.
#[derive(Default)]
struct Gauge(Mutex<(BTreeMap<String, usize>, BTreeMap<String, usize>)>);

impl Gauge {
    fn enter(&self, key: &str) {
        let mut g = self.0.lock().unwrap();
        let now = {
            let e = g.0.entry(key.to_owned()).or_default();
            *e += 1;
            *e
        };
        let max = g.1.entry(key.to_owned()).or_default();
        *max = (*max).max(now);
    }

    fn leave(&self, key: &str) {
        *self.0.lock().unwrap().0.get_mut(key).unwrap() -= 1;
    }

    fn max(&self, key: &str) -> usize {
        self.0.lock().unwrap().1.get(key).copied().unwrap_or(0)
    }
}

fn parallel(mut def: WorkflowDefinition, max_parallel: u32) -> WorkflowDefinition {
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.execution = Some(WorkflowExecutionOptions { max_parallel });
    def
}

/// Independent restricted steps `ids`, each reading the `data` input; with
/// `join`, a step consuming every one's dataset on its `multiple` port.
fn fan(ids: &[&str], join: Option<&str>, max_parallel: u32) -> WorkflowDefinition {
    let mut def = numerical(false);
    def.steps = ids.iter().map(|id| python_step(id, false)).collect();
    def.connections = ids.iter().map(|id| data_edge(PortSource::Input { input: "data".into() }, id)).collect();
    def.outcomes = ids.iter().map(|id| dataset_outcome(id)).collect();
    if let Some(join) = join {
        def.steps.push(python_step(join, false));
        def.connections.extend(ids.iter().map(|id| data_edge(out(id), join)));
        def.outcomes = vec![dataset_outcome(join)];
    }
    parallel(def, max_parallel)
}

/// The same steps run natively (trusted folder).
fn native_fan(ids: &[&str], max_parallel: u32) -> WorkflowDefinition {
    let mut def = fan(ids, None, max_parallel);
    for s in &mut def.steps {
        *s = python_step(&s.id, true);
    }
    def.ceiling.execution = ExecutionCeiling::TrustedNative;
    def
}

fn service_limits(f: &Fixture, a: Adapters, limits: WorkflowConcurrencyLimits) -> WorkflowRunService {
    let h: Arc<dyn WorkflowHost> = f.host.clone();
    WorkflowRunService::new_with_limits(
        WorkflowRunConfig { database: None, host: h, adapters: a, lean: None, research: None },
        limits,
    )
    .unwrap()
}

fn wide_limits() -> WorkflowConcurrencyLimits {
    WorkflowConcurrencyLimits { restricted: 4, native: 4, ..Default::default() }
}

fn start_on(service: &WorkflowRunService, id: &WorkflowId) -> WorkflowRun {
    let plan = service.plan(id, None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    service.start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash }).unwrap()
}

fn run_on(service: &WorkflowRunService, id: &WorkflowId) -> WorkflowRun {
    let run = start_on(service, id);
    assert!(service.wait_idle(Duration::from_secs(30)), "run did not finish");
    service.get(&run.id).unwrap()
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// Blocks until canceled (counting the batches that started), then reports
/// the cancellation.
fn blocking_counted(started: Arc<AtomicUsize>) -> Arc<BatchBehavior> {
    Arc::new(move |_root, _req, control, run| {
        started.fetch_add(1, Ordering::SeqCst);
        while !(control.canceled)() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut r = succeeded(run, Vec::new());
        r.status = RunStatus::Canceled;
        Ok(r)
    })
}

fn overlap(a: &WorkflowStepAttempt, b: &WorkflowStepAttempt) -> bool {
    a.dispatched_at_ms.unwrap() < b.finished_at_ms.unwrap() && b.dispatched_at_ms.unwrap() < a.finished_at_ms.unwrap()
}

// ------------------------------------------------------------- overlap ----

#[test]
fn independent_steps_run_at_the_same_time_and_a_join_receives_both_in_binding_order() {
    let f = fixture();
    let rendezvous = Rendezvous::new(2);
    let met = Arc::new(Mutex::new(Vec::new()));
    let (r, m) = (rendezvous.clone(), met.clone());
    let inner = default_behavior();
    f.set_behavior(Arc::new(move |root, req, control, run| {
        if req.owner.step != "join" {
            let together = r.meet();
            m.lock().unwrap().push((req.owner.step.clone(), together));
        }
        inner(root, req, control, run)
    }));
    let id = f.save(fan(&["a", "b"], Some("join"), 2));
    let plan = f.service.plan(&id, None).unwrap();
    assert_eq!(plan.max_parallel, 2);
    assert_eq!(plan.limits, WorkflowConcurrencyLimits::default());
    assert!(plan.notices[0].contains("Up to 2 independent steps run at the same time"), "{}", plan.notices[0]);
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let met = met.lock().unwrap().clone();
    assert_eq!(met.len(), 2);
    assert!(met.iter().all(|(_, together)| *together), "both branches executed at the same time: {met:?}");
    assert_eq!(f.calls.max_running.load(Ordering::SeqCst), 2);
    assert!(overlap(last_attempt(&run, "a"), last_attempt(&run, "b")));
    // The join started only after both branches, and got both datasets in
    // the order of its bindings (a, then b).
    assert_eq!(f.calls.log().last().map(String::as_str), Some("batch:join"));
    let join = last_attempt(&run, "join");
    for branch in ["a", "b"] {
        assert!(last_attempt(&run, branch).finished_at_ms.unwrap() <= join.dispatched_at_ms.unwrap());
    }
    let dataset =
        |step: &str| match &last_attempt(&run, step).outputs.iter().find(|o| o.port == "dataset").unwrap().value {
            WorkflowOutputValue::File { path, .. } => path.to_string(),
            other => panic!("{other:?}"),
        };
    let requests = f.batch.requests.lock().unwrap();
    let inputs: Vec<String> = requests[2].2.iter().map(|i| i.split('@').next().unwrap().to_owned()).collect();
    assert_eq!(inputs, vec![dataset("a"), dataset("b")]);
}

#[test]
fn max_parallel_one_keeps_the_one_step_at_a_time_runner() {
    let f = fixture();
    let id = f.save(fan(&["a", "b"], Some("join"), 1));
    let plan = f.service.plan(&id, None).unwrap();
    assert_eq!(plan.max_parallel, 1);
    assert!(plan.notices[0].contains("Steps run one at a time in the order shown"));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(f.calls.max_running.load(Ordering::SeqCst), 1);
    assert_eq!(f.calls.log(), vec!["batch:a".to_owned(), "batch:b".into(), "batch:join".into()]);
    // A v1 definition (no execution options) is the same runner.
    let id = f.save(chain());
    assert_eq!(f.service.plan(&id, None).unwrap().max_parallel, 1);
}

#[test]
fn a_run_never_exceeds_its_max_parallel() {
    let f = fixture();
    let gauge = Arc::new(Gauge::default());
    let rendezvous = Rendezvous::new(3);
    let together = Arc::new(Mutex::new(Vec::new()));
    let (g, r, t) = (gauge.clone(), rendezvous.clone(), together.clone());
    let inner = default_behavior();
    f.set_behavior(Arc::new(move |root, req, control, run| {
        g.enter("all");
        let met = r.meet();
        t.lock().unwrap().push(met);
        std::thread::sleep(Duration::from_millis(60));
        g.leave("all");
        inner(root, req, control, run)
    }));
    let service = service_limits(&f, adapters(&f.batch, &f.lean, &f.research), wide_limits());
    let id = f.save(fan(&["a", "b", "c", "d", "e"], None, 3));
    let run = run_on(&service, &id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert!(together.lock().unwrap().iter().all(|t| *t), "three steps executed at the same time");
    assert_eq!(gauge.max("all"), 3, "never more than maxParallel");
    assert_eq!(service.slot_counts(), crate::SlotCounts::default(), "every slot released");
}

#[test]
fn global_limits_are_shared_by_every_run() {
    let f = fixture();
    let gauge = Arc::new(Gauge::default());
    let rendezvous = Rendezvous::new(4);
    let g = gauge.clone();
    let r = rendezvous.clone();
    let inner = default_behavior();
    f.set_behavior(Arc::new(move |root, req, control, run| {
        let kind = format!("{:?}", req.mode);
        g.enter(&kind);
        g.enter("all");
        r.meet();
        std::thread::sleep(Duration::from_millis(80));
        g.leave("all");
        g.leave(&kind);
        inner(root, req, control, run)
    }));
    // Two runs that would each run three steps at once.
    let restricted = f.save(fan(&["a", "b", "c"], None, 3));
    let native = f.save(native_fan(&["n1", "n2", "n3"], 3));
    let first = f.start(&restricted);
    let second = f.start(&native);
    let during = Arc::new(Mutex::new(Vec::new()));
    {
        let (service, during) = (f.service.clone(), during.clone());
        std::thread::spawn(move || {
            for _ in 0..200 {
                during.lock().unwrap().push(service.slot_counts());
                std::thread::sleep(Duration::from_millis(3));
            }
        });
    }
    assert!(f.service.wait_idle(Duration::from_secs(30)));
    for id in [&first.id, &second.id] {
        let run = f.service.get(id).unwrap();
        assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    }
    assert_eq!(gauge.max("Restricted"), 2, "restricted limit 2 across runs");
    assert_eq!(gauge.max("TrustedNative"), 2, "native limit 2 across runs");
    assert_eq!(gauge.max("all"), 4, "four steps in flight across both runs, never more");
    for c in during.lock().unwrap().iter() {
        assert!(c.in_flight <= 4 && c.restricted <= 2 && c.native <= 2, "{c:?}");
    }
}

#[test]
fn at_most_three_runs_and_one_per_workflow_are_active() {
    let f = fixture();
    let started = Arc::new(AtomicUsize::new(0));
    f.set_behavior(blocking_counted(started.clone()));
    let ids: Vec<WorkflowId> = (0..4).map(|_| f.save(numerical(false))).collect();
    let runs: Vec<WorkflowRun> = ids[..3].iter().map(|id| f.start(id)).collect();
    wait_until("three runs dispatching", || started.load(Ordering::SeqCst) == 2);
    // The second run's step waits for a restricted slot (limit 2); the run is active.
    assert_eq!(f.service.active_runs().len(), 3);
    let plan = f.service.plan(&ids[3], None).unwrap();
    let blocker = plan.blockers.iter().find(|b| b.code == "workflow.activeRun").expect("blocked");
    assert!(blocker.message.contains("3 workflow runs are active"), "{}", blocker.message);
    assert_eq!(f.start_err(plan).code, "workflow.activeRun");
    // The same workflow again: one run per workflow.
    let again = f.service.plan(&ids[0], None).unwrap();
    let blocker = again.blockers.iter().find(|b| b.code == "workflow.activeRun").expect("blocked");
    assert!(blocker.message.contains("A run of this workflow is active"), "{}", blocker.message);
    // A finished run frees a place.
    f.service.cancel(&runs[0].id).unwrap();
    let plan = f.service.plan(&ids[3], None).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    for run in &runs[1..] {
        f.service.cancel(&run.id).unwrap();
    }
    assert!(f.service.wait_idle(Duration::from_secs(10)));
}

// ---------------------------------------------------------- reservation ----

#[test]
fn reserved_output_bytes_keep_concurrent_steps_within_the_budget() {
    let f = fixture();
    // a → (b, c): a's run leaves too little budget for b and c to both
    // reserve their limit; c waits for b's reservation to be released.
    let mut def = fan(&["b", "c"], None, 2);
    def.steps.insert(0, python_step("a", false));
    def.connections = vec![
        data_edge(PortSource::Input { input: "data".into() }, "a"),
        data_edge(out("a"), "b"),
        data_edge(out("a"), "c"),
    ];
    let id = f.save(def);
    let gauge = Arc::new(Gauge::default());
    let grants = Arc::new(Mutex::new(BTreeMap::new()));
    let (g, gr) = (gauge.clone(), grants.clone());
    const MIB: u64 = 1024 * 1024;
    f.set_behavior(Arc::new(move |root, req, _control, run| {
        gr.lock().unwrap().insert(req.owner.step.clone(), req.max_output_bytes);
        let mut result =
            succeeded(run, vec![artifact(root, run, "dataset", "dataset.csv", b"x\n1\n", ArtifactKind::Data)]);
        if req.owner.step == "a" {
            // Accepted files that realize no port still count (700 MiB).
            let mut extra = artifact(root, run, "x", "extra.bin", b"!", ArtifactKind::Data).artifact;
            extra.size_bytes = 700 * MIB;
            result.extra.push(extra);
        } else {
            g.enter("bc");
            std::thread::sleep(Duration::from_millis(150));
            g.leave("bc");
        }
        Ok(result)
    }));
    let plan = f.service.plan(&id, None).unwrap();
    assert_eq!(plan.budgets.max_output_bytes, 3 * 256 * MIB);
    assert!(plan.steps.iter().all(|s| s.max_output_bytes == 256 * MIB));
    let run = f.run_to_end(&id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    let grants = grants.lock().unwrap().clone();
    assert_eq!(grants["a"], 256 * MIB);
    // About 68 MiB were left after a: b got all of it, c waited for b's
    // release instead of running beside it with the same 68 MiB.
    // (a's dataset is 4 bytes, b's too.)
    let left = 768 * MIB - 700 * MIB - 4;
    assert_eq!(grants["b"], left);
    assert_eq!(grants["c"], left - 4);
    assert_eq!(gauge.max("bc"), 1, "b and c never ran together");
    assert!(run.usage.output_bytes <= run.budgets.max_output_bytes);
}

// --------------------------------------------- cancel, close, restart -----

#[test]
fn cancel_stops_every_running_step() {
    let f = fixture();
    let started = Arc::new(AtomicUsize::new(0));
    f.set_behavior(blocking_counted(started.clone()));
    let id = f.save(fan(&["a", "b", "c"], Some("join"), 3));
    let run = f.start(&id);
    // Restricted limit 2: two steps run, the third waits for a slot.
    wait_until("two steps running", || started.load(Ordering::SeqCst) == 2);
    let canceled = f.service.cancel(&run.id).unwrap();
    assert_eq!(canceled.state, WorkflowRunState::Canceled);
    assert_eq!(canceled.code.as_deref(), Some("workflow.canceled"));
    let s: BTreeMap<String, WorkflowStepState> = states(&canceled).into_iter().collect();
    assert_eq!(s["a"], WorkflowStepState::Canceled);
    assert_eq!(s["b"], WorkflowStepState::Canceled);
    assert_eq!(s["c"], WorkflowStepState::Skipped, "never dispatched");
    assert_eq!(s["join"], WorkflowStepState::Skipped);
    for step in ["a", "b"] {
        let a = last_attempt(&canceled, step);
        assert_eq!(a.state, WorkflowAttemptState::Canceled);
        assert!(a.operation.is_some(), "the operation was persisted");
    }
    let outcome = &canceled.outcomes[0];
    assert!(outcome.value.is_none());
    assert!(outcome.reason.as_deref().unwrap().contains("did not run"), "{:?}", outcome.reason);
    assert_eq!(f.service.slot_counts(), crate::SlotCounts::default());
    assert_eq!(started.load(Ordering::SeqCst), 2, "nothing started after the cancel");
}

#[test]
fn closing_the_project_or_revoking_trust_cancels_every_running_step() {
    for revoke in [false, true] {
        let f = fixture();
        let started = Arc::new(AtomicUsize::new(0));
        f.set_behavior(blocking_counted(started.clone()));
        let id = f.save(native_fan(&["a", "b"], 2));
        let run = f.start(&id);
        wait_until("both steps running", || started.load(Ordering::SeqCst) == 2);
        if revoke {
            f.host.trusted.store(false, Ordering::SeqCst);
            f.service.trust_revoked(&f.root);
        } else {
            f.service.project_closed(Some(&f.root));
        }
        assert!(f.service.wait_idle(Duration::from_secs(10)));
        let run = f.service.get(&run.id).unwrap();
        assert_eq!(run.state, WorkflowRunState::Canceled);
        let code = if revoke { "workflow.untrustedFolder" } else { "workflow.projectClosed" };
        assert_eq!(run.code.as_deref(), Some(code));
        assert_eq!(step_states(&run), vec![WorkflowStepState::Canceled; 2], "{revoke}");
        assert_eq!(f.service.slot_counts(), crate::SlotCounts::default());
    }
}

#[test]
fn a_crash_with_several_steps_in_flight_interrupts_them_all_and_replays_nothing() {
    let f = fixture();
    let started = Arc::new(AtomicUsize::new(0));
    // Never released: the "crashed" session's steps never return.
    f.set_behavior(blocking_counted(started.clone()));
    let id = f.save(fan(&["a", "b"], Some("join"), 2));
    let run = f.start(&id);
    wait_until("both steps dispatched", || {
        let r = f.service.get(&run.id).unwrap();
        ["a", "b"].iter().all(|s| {
            r.steps.iter().find(|x| x.step == *s).unwrap().attempts.last().map(|a| a.state)
                == Some(WorkflowAttemptState::Dispatched)
        })
    });
    let fresh = Arc::new(FakeBatch {
        available: Mutex::new(Ok(())),
        behavior: Mutex::new(default_behavior()),
        requests: Mutex::new(Vec::new()),
        pinned: Mutex::new(Vec::new()),
        calls: f.calls.clone(),
        counter: AtomicUsize::new(100),
    });
    let restarted = new_service(&f.db, &f.host, &fresh, &f.lean, &f.research);
    let after = restarted.get(&run.id).unwrap();
    assert_eq!(after.state, WorkflowRunState::Interrupted);
    let s: BTreeMap<String, WorkflowStepState> = states(&after).into_iter().collect();
    assert_eq!(s["a"], WorkflowStepState::Interrupted);
    assert_eq!(s["b"], WorkflowStepState::Interrupted);
    assert_eq!(s["join"], WorkflowStepState::Skipped);
    for step in ["a", "b"] {
        let a = last_attempt(&after, step);
        assert_eq!(a.state, WorkflowAttemptState::Interrupted);
        assert!(a.operation.is_some(), "the operation id survived");
    }
    assert!(after.outcomes[0].reason.is_some());
    std::thread::sleep(Duration::from_millis(100));
    assert!(fresh.requests.lock().unwrap().is_empty(), "nothing was re-dispatched");
    assert!(restarted.active_runs().is_empty());
}

#[test]
fn restart_interrupts_intent_and_dispatched_attempts_alike() {
    let f = fixture();
    let id = f.save(fan(&["a", "b"], None, 2));
    let mut run = f.run_to_end(&id);
    // As the store held it when the application stopped: a's intent was
    // persisted (not dispatched yet), b was dispatched.
    run.state = WorkflowRunState::Running;
    run.finished_at_ms = None;
    for step in &mut run.steps {
        step.state = WorkflowStepState::Running;
        let a = step.attempts.last_mut().unwrap();
        a.finished_at_ms = None;
        a.outputs.clear();
        if step.step == "a" {
            a.state = WorkflowAttemptState::Intent;
            a.operation = None;
            a.dispatched_at_ms = None;
        } else {
            a.state = WorkflowAttemptState::Dispatched;
        }
    }
    for o in &mut run.outcomes {
        o.value = None;
    }
    RunStore::open(Some(&f.db)).unwrap().put(&crate::plan::project_key(&f.root), &run).unwrap();
    let restarted = new_service(&f.db, &f.host, &f.batch, &f.lean, &f.research);
    let after = restarted.get(&run.id).unwrap();
    assert_eq!(after.state, WorkflowRunState::Interrupted);
    for step in ["a", "b"] {
        assert_eq!(last_attempt(&after, step).state, WorkflowAttemptState::Interrupted);
    }
    assert!(last_attempt(&after, "a").message.as_deref().unwrap().contains("about to be dispatched"));
    assert!(last_attempt(&after, "b").message.as_deref().unwrap().contains("outcome is unknown"));
}

#[test]
fn a_failure_starts_nothing_new_and_running_steps_finish() {
    let f = fixture();
    let b_started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (bs, rel) = (b_started.clone(), release.clone());
    let inner = default_behavior();
    f.set_behavior(Arc::new(move |root, req, control, run| match req.owner.step.as_str() {
        "a" => {
            while !bs.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            let mut r = succeeded(run, Vec::new());
            r.status = RunStatus::Failed { message: "boom".into() };
            Ok(r)
        }
        "b" => {
            bs.store(true, Ordering::SeqCst);
            while !rel.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            inner(root, req, control, run)
        }
        _ => inner(root, req, control, run),
    }));
    let id = f.save(fan(&["a", "b", "c"], None, 2));
    let run = f.start(&id);
    wait_until("a failed", || f.service.get(&run.id).unwrap().steps[0].state == WorkflowStepState::Failed);
    // c is ready and a slot is free, but nothing new starts after a failure.
    std::thread::sleep(Duration::from_millis(150));
    release.store(true, Ordering::SeqCst);
    assert!(f.service.wait_idle(Duration::from_secs(10)));
    let run = f.service.get(&run.id).unwrap();
    assert_eq!(run.state, WorkflowRunState::Failed);
    assert!(run.message.as_deref().unwrap().contains("boom"), "{:?}", run.message);
    assert_eq!(
        step_states(&run),
        vec![WorkflowStepState::Failed, WorkflowStepState::Succeeded, WorkflowStepState::Skipped]
    );
    assert!(!f.calls.log().contains(&"batch:c".to_owned()));
    let c = run.outcomes.iter().find(|o| o.step == "c").unwrap();
    assert!(c.reason.as_deref().unwrap().contains("did not run"));
    let a = run.outcomes.iter().find(|o| o.step == "a").unwrap();
    assert!(a.reason.as_deref().unwrap().contains("boom"), "{:?}", a.reason);
    assert!(run.outcomes.iter().find(|o| o.step == "b").unwrap().value.is_some(), "b's result is kept");
}

#[test]
fn concurrent_updates_to_the_run_record_are_never_lost() {
    let f = fixture();
    let rendezvous = Rendezvous::new(4);
    let go = Arc::new(AtomicBool::new(false));
    let (r, g) = (rendezvous.clone(), go.clone());
    let inner = default_behavior();
    // on_dispatched was called (by the fake) before this runs.
    f.set_behavior(Arc::new(move |root, req, control, run| {
        r.meet();
        while !g.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        inner(root, req, control, run)
    }));
    let service = {
        let h: Arc<dyn WorkflowHost> = f.host.clone();
        WorkflowRunService::new_with_limits(
            WorkflowRunConfig {
                database: Some(f.db.clone()),
                host: h,
                adapters: adapters(&f.batch, &f.lean, &f.research),
                lean: None,
                research: None,
            },
            wide_limits(),
        )
        .unwrap()
    };
    let id = f.save(fan(&["a", "b", "c", "d"], None, 4));
    let run = start_on(&service, &id);
    // Four dispatches persisted from four threads: all four are on disk.
    wait_until("four dispatches persisted", || {
        let stored = RunStore::open(Some(&f.db)).unwrap().get(&run.id).unwrap().unwrap().1;
        stored.steps.iter().all(|s| {
            s.attempts.last().is_some_and(|a| a.state == WorkflowAttemptState::Dispatched && a.operation.is_some())
        })
    });
    go.store(true, Ordering::SeqCst);
    assert!(service.wait_idle(Duration::from_secs(20)));
    let stored = RunStore::open(Some(&f.db)).unwrap().get(&run.id).unwrap().unwrap().1;
    assert_eq!(stored.state, WorkflowRunState::Succeeded, "{:?}", stored.message);
    let mut operations: Vec<String> = Vec::new();
    for step in &stored.steps {
        let a = step.attempts.last().unwrap();
        assert_eq!(a.state, WorkflowAttemptState::Succeeded);
        assert_eq!(a.outputs.len(), 3, "{}", step.step);
        assert!(a.dispatched_at_ms.is_some() && a.finished_at_ms.is_some());
        operations.push(a.operation.clone().unwrap().id);
    }
    operations.sort();
    operations.dedup();
    assert_eq!(operations.len(), 4, "each step kept its own operation");
    assert_eq!(stored.usage.steps, 4);
    assert!(stored.outcomes.iter().all(|o| o.value.is_some()));
}

// ----------------------------------------------------- plan and approval ----

#[test]
fn the_approval_covers_max_parallel_and_the_limits() {
    let f = fixture();
    let one = f.save(fan(&["a", "b"], None, 1));
    let two = f.save(fan(&["a", "b"], None, 2));
    let p1 = f.service.plan(&one, None).unwrap();
    let p2 = f.service.plan(&two, None).unwrap();
    assert_eq!((p1.max_parallel, p2.max_parallel), (1, 2));
    // Same steps, other concurrency: a different approval.
    let mut def = f.graph.workflow(&two, None).unwrap().definition;
    def.execution = Some(WorkflowExecutionOptions { max_parallel: 1 });
    let p1b = f.service.plan(&f.save(def), None).unwrap();
    assert_ne!(p1b.approval_hash, p2.approval_hash);
    // Saving the workflow with another maxParallel makes an approved plan stale.
    let mut def = f.graph.workflow(&two, None).unwrap().definition;
    def.execution = Some(WorkflowExecutionOptions { max_parallel: 3 });
    f.graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: Some(two.clone()), expected_revision: None, definition: def },
            &ValidationContext::default(),
        )
        .unwrap();
    let e = f.start_err(p2);
    assert_eq!(e.code, "workflow.planStale");
    assert!(e.message.contains("at the same time changed (2 → 3)"), "{}", e.message);
    // Other application-wide limits: another approval.
    let narrow = service_limits(
        &f,
        adapters(&f.batch, &f.lean, &f.research),
        WorkflowConcurrencyLimits { restricted: 1, ..Default::default() },
    );
    let p = narrow.plan(&one, None).unwrap();
    assert_eq!(p.limits.restricted, 1);
    assert_ne!(p.approval_hash, f.service.plan(&one, None).unwrap().approval_hash);
    assert!(f.calls.log().is_empty());
}

// ---------------------------------------------------------- multi-agent ----

/// Read-only reviews that can be made to meet, report a request count, and
/// name their own task.
struct ConcurrentReviewer {
    rendezvous: Arc<Rendezvous>,
    gauge: Gauge,
    counter: AtomicUsize,
    /// (step label, prompt, granted requests, task)
    calls: Mutex<Vec<(String, String, u32, String)>>,
}

impl ConcurrentReviewer {
    fn new(meet: usize) -> Arc<Self> {
        Arc::new(Self {
            rendezvous: Rendezvous::new(meet),
            gauge: Gauge::default(),
            counter: AtomicUsize::new(1),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn call(&self, label: &str) -> (String, String, u32, String) {
        self.calls.lock().unwrap().iter().find(|c| c.0 == label).cloned().unwrap()
    }
}

impl AgentReviewer for ConcurrentReviewer {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }

    fn review(
        &self,
        request: &ReviewRequest,
        _canceled: &(dyn Fn() -> bool + Sync),
        on_task: &(dyn Fn(&str) + Sync),
    ) -> Result<ReviewOutcome, StepFailure> {
        let task = format!("task-{}", self.counter.fetch_add(1, Ordering::SeqCst));
        on_task(&task);
        let label = request.prompt.split('“').nth(1).and_then(|s| s.split('”').next()).unwrap_or("").to_owned();
        self.calls.lock().unwrap().push((label.clone(), request.prompt.clone(), request.max_requests, task.clone()));
        self.gauge.enter("review");
        if request.prompt.contains("MEET") {
            self.rendezvous.meet();
        }
        std::thread::sleep(Duration::from_millis(60));
        self.gauge.leave("review");
        let requests = request
            .prompt
            .split("USE=")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(3);
        Ok(ReviewOutcome { text: format!("Report of {label}."), task, requests })
    }
}

fn review_step(id: &str, prompt: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Review {id}"),
        operation: StepOperation { id: "agent.review".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Prompt { text: prompt.into() }),
        composite: None,
        condition: None,
        repeat: None,
    }
}

fn material(from: PortSource, step: &str) -> WorkflowConnection {
    WorkflowConnection::Data { from, to: StepPort { step: step.into(), port: "material".into() } }
}

fn report(step: &str) -> PortSource {
    PortSource::Step { step: step.into(), port: "report".into() }
}

fn report_outcome(step: &str) -> WorkflowOutcome {
    WorkflowOutcome {
        id: format!("o-{step}"),
        kind: OutcomeKind::InvestigationReport,
        label: format!("Report of {step}"),
        from: StepPort { step: step.into(), port: "report".into() },
        optional: false,
    }
}

fn data() -> PortSource {
    PortSource::Input { input: "data".into() }
}

fn agent_service(
    f: &Fixture,
    reviewer: &Arc<ConcurrentReviewer>,
    limits: WorkflowConcurrencyLimits,
) -> WorkflowRunService {
    let mut a = adapters(&f.batch, &f.lean, &f.research);
    let r: Arc<dyn AgentReviewer> = reviewer.clone();
    a.insert(runner_ids::READ_ONLY_AGENT.into(), Arc::new(AgentReviewAdapter { reviewer: Some(r) }));
    service_limits(f, a, limits)
}

fn reviews(
    steps: Vec<WorkflowStep>,
    connections: Vec<WorkflowConnection>,
    outcome: &str,
    mp: u32,
) -> WorkflowDefinition {
    let mut def = numerical(false);
    def.ceiling.model_requests = true;
    def.steps = steps;
    def.connections = connections;
    def.outcomes = vec![report_outcome(outcome)];
    parallel(def, mp)
}

#[test]
fn concurrent_reviews_are_separate_read_only_tasks_and_a_consumer_receives_both_reports_in_order() {
    let f = fixture();
    let reviewer = ConcurrentReviewer::new(2);
    let service = agent_service(&f, &reviewer, WorkflowConcurrencyLimits::default());
    let def = reviews(
        vec![
            review_step("r1", "Check the method. MEET"),
            review_step("r2", "Check the data. MEET"),
            review_step("sum", "Summarize both reviews."),
        ],
        vec![
            material(data(), "r1"),
            material(data(), "r2"),
            material(report("r1"), "sum"),
            material(report("r2"), "sum"),
            material(data(), "sum"),
        ],
        "sum",
        2,
    );
    let id = f.save(def);
    let plan = service.plan(&id, None).unwrap();
    assert_eq!(plan.budgets.max_model_requests, 75);
    assert!(plan.steps.iter().all(|s| s.max_model_requests == 25));
    let run = run_on(&service, &id);
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(reviewer.gauge.max("review"), 2, "the two reviews ran at the same time");
    assert!(overlap(last_attempt(&run, "r1"), last_attempt(&run, "r2")));
    // Each review is its own task, granted its own reservation.
    let (r1, r2) = (reviewer.call("Review r1"), reviewer.call("Review r2"));
    assert_ne!(r1.3, r2.3);
    assert_eq!((r1.2, r2.2), (25, 25));
    assert_eq!(last_attempt(&run, "r1").operation.as_ref().unwrap().kind, "agentTask");
    assert_ne!(last_attempt(&run, "r1").operation, last_attempt(&run, "r2").operation);
    for c in [&r1, &r2] {
        assert!(c.1.contains("read-only"));
        assert!(c.1.contains("- data/obs.csv"), "scoped to its declared material");
    }
    // The consumer's multiple port received both reports, in binding order.
    let sum = reviewer.call("Review sum").1;
    let (p1, p2) = (sum.find("Report of Review r1.").unwrap(), sum.find("Report of Review r2.").unwrap());
    assert!(p1 < p2, "{sum}");
    assert_eq!(run.usage.model_requests, 9, "shared aggregate budget");
    assert!(run.usage.model_requests <= run.budgets.max_model_requests);
}

#[test]
fn at_most_two_agent_steps_run_at_once_across_the_application() {
    let f = fixture();
    let reviewer = ConcurrentReviewer::new(2);
    let service = agent_service(&f, &reviewer, WorkflowConcurrencyLimits::default());
    let def = reviews(
        vec![
            review_step("r1", "One. MEET"),
            review_step("r2", "Two. MEET"),
            review_step("r3", "Three. MEET"),
            review_step("sum", "Summarize."),
        ],
        vec![
            material(data(), "r1"),
            material(data(), "r2"),
            material(data(), "r3"),
            material(report("r1"), "sum"),
            material(report("r2"), "sum"),
            material(report("r3"), "sum"),
            material(data(), "sum"),
        ],
        "sum",
        3,
    );
    let run = run_on(&service, &f.save(def));
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(reviewer.gauge.max("review"), 2, "agent limit 2");
    let sum = reviewer.call("Review sum").1;
    let at = |s: &str| sum.find(s).unwrap();
    assert!(
        at("Report of Review r1.") < at("Report of Review r2.")
            && at("Report of Review r2.") < at("Report of Review r3.")
    );
}

#[test]
fn reserved_model_requests_keep_concurrent_reviews_within_the_budget() {
    let f = fixture();
    let reviewer = ConcurrentReviewer::new(1);
    let service = agent_service(&f, &reviewer, WorkflowConcurrencyLimits::default());
    // a uses 40 requests (more than its 25); b and c then both want 25 of
    // the 35 left: they must not run together on the same remainder.
    let def = reviews(
        vec![review_step("a", "First. USE=40"), review_step("b", "Second."), review_step("c", "Third.")],
        vec![
            material(data(), "a"),
            material(report("a"), "b"),
            material(data(), "b"),
            material(report("a"), "c"),
            material(data(), "c"),
        ],
        "b",
        2,
    );
    let mut def = def;
    def.outcomes.push(report_outcome("c"));
    let run = run_on(&service, &f.save(def));
    assert_eq!(run.state, WorkflowRunState::Succeeded, "{:?}", run.message);
    assert_eq!(run.budgets.max_model_requests, 75);
    let granted: Vec<u32> = ["Review a", "Review b", "Review c"].iter().map(|l| reviewer.call(l).2).collect();
    assert_eq!(granted, vec![25, 25, 25]);
    assert_eq!(reviewer.gauge.max("review"), 1, "c waited for b's reservation instead of sharing 35 requests");
    assert!(!overlap(last_attempt(&run, "b"), last_attempt(&run, "c")));
    assert_eq!(run.usage.model_requests, 46);
}
