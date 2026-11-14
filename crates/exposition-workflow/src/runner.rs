//! The bounded runner.
//!
//! A run's steps execute in dependency order: a step becomes ready once
//! every step it consumes data from, every explicit prerequisite, and the
//! source of its condition finished; ready steps start in plan order, at most
//! the plan's `max_parallel` at a time (1: exactly one step at a time, in plan
//! order) and only while an application-wide slot of the step's kind is free
//! (shared by every active run, see [`crate::slots`]).
//!
//! Conditions and repeats (bounded, no expression evaluator): a step whose
//! condition does not hold is `conditionNotMet` (with its reason) and every
//! step that needs its output is skipped (with its reason); an unreadable
//! condition source fails the step. A forEach step runs one instance per
//! value (in parallel within the limits; consumers receive the instances'
//! outputs in index order); an until step runs iterations one after another
//! until a predicate on its own output holds (on exhaustion it fails or uses
//! the last iteration). Every instance is dispatched like a step: it counts
//! against `maxSteps`, reserves budget, and holds a slot.
//!
//! Before each dispatch: the run's project is still the open one, the run was
//! not canceled, budgets remain — the step's output bytes and model requests
//! are reserved from what the run has left (a step whose reservation does not
//! fit waits for running steps to release theirs; its time limit is capped by
//! the run's remaining wall-clock time) —, every external input and upstream
//! output the step consumes still has the digest recorded for it, a native
//! step's folder is still trusted, and the runner is still available (a
//! restricted backend is never downgraded). The attempt's dispatch intent is
//! persisted before the adapter is called and the service operation as soon
//! as it is known; every change of the run record is made and written through
//! under one lock, so concurrent steps never lose each other's updates. Every
//! returned output is validated (rule 5/6) before a consumer may use it.
//! No retries. A failed step stops the workflow: nothing new starts, steps
//! already running finish and are recorded, the remaining steps are skipped.
//! Cancellation (the user, project close, trust revocation) stops every
//! running step through its service.

// A failed attempt carries its log and refusals (see adapters); deliberate.
#![allow(clippy::result_large_err)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use exposition_graph::OperationRef;
use exposition_graph::workflow::registry::{self, Containment, OperationContract, PortType};
use exposition_graph::workflow::{ConditionTest, Exhausted, PortSource, StepRepeat};

use crate::adapters::{
    BoundInput, StepAdapter, StepControl, StepFailure, StepIteration, StepRequest, StepResult, bare_hash,
};
use crate::model::{
    MAX_PARALLEL, WorkflowAttemptState, WorkflowOutputValue, WorkflowPlanStep, WorkflowRejectedOutput, WorkflowRun,
    WorkflowRunState, WorkflowStepAttempt, WorkflowStepInstance, WorkflowStepOutput, WorkflowStepState,
};
use crate::plan::{hash_project_file, instance_parameters, resolve, time_limit_of};
use crate::service::Inner;
use crate::slots::{SlotGuard, SlotKind};

/// How often a run waiting for a free slot tries again.
const SLOT_POLL: Duration = Duration::from_millis(20);
/// How often a run with running steps re-checks cancellation.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// Cancellation of one run, with the reason the run reports.
#[derive(Default)]
pub struct CancelToken {
    canceled: AtomicBool,
    reason: Mutex<Option<(String, String)>>,
}

impl CancelToken {
    pub fn cancel(&self, code: &str, message: &str) {
        let mut reason = self.reason.lock().unwrap_or_else(|p| p.into_inner());
        if reason.is_none() {
            *reason = Some((code.to_owned(), message.to_owned()));
        }
        self.canceled.store(true, Ordering::SeqCst);
    }

    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::SeqCst)
    }

    fn reason(&self) -> (String, String) {
        self.reason
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_else(|| ("workflow.canceled".into(), "The run was canceled.".into()))
    }
}

/// Cancels the run if the scheduler itself panics, so running steps stop
/// before the scope waits for them.
struct CancelOnPanic<'a>(&'a CancelToken);

impl Drop for CancelOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.cancel("workflow.internal", "The workflow runner stopped unexpectedly.");
        }
    }
}

/// How the run ended (when it did not succeed).
struct Stop {
    state: WorkflowRunState,
    code: String,
    message: String,
}

impl Stop {
    fn failed(code: &str, message: impl Into<String>) -> Self {
        Self { state: WorkflowRunState::Failed, code: code.to_owned(), message: message.into() }
    }

    fn canceled(token: &CancelToken) -> Self {
        let (code, message) = token.reason();
        Self { state: WorkflowRunState::Canceled, code, message }
    }
}

fn step_index(run: &WorkflowRun, step: &str) -> usize {
    run.steps.iter().position(|s| s.step == step).expect("plan steps are run steps")
}

/// What is dispatched: a plan step (by index) and, for a repeated step, the
/// instance.
type Unit = (usize, Option<u32>);

/// The attempts of a unit (an until iteration's instance is created here).
fn attempts_mut(run: &mut WorkflowRun, (i, instance): Unit) -> &mut Vec<WorkflowStepAttempt> {
    match instance {
        None => &mut run.steps[i].attempts,
        Some(k) => &mut instance_mut(run, i, k).attempts,
    }
}

fn instance_mut(run: &mut WorkflowRun, i: usize, k: u32) -> &mut WorkflowStepInstance {
    let step = &mut run.steps[i];
    if !step.instances.iter().any(|x| x.index == k) {
        step.instances.push(WorkflowStepInstance {
            index: k,
            parameter: None,
            state: WorkflowStepState::Pending,
            attempts: Vec::new(),
            reason: None,
        });
    }
    step.instances.iter_mut().find(|x| x.index == k).expect("just ensured")
}

/// A dispatched attempt: its slot and what it reserved from the budgets.
struct Flight {
    _slot: SlotGuard,
    bytes: u64,
    model_requests: u32,
}

/// What a finished attempt reports back to the scheduler.
struct Completion {
    unit: Unit,
    attempt: u32,
    reported: Option<OperationRef>,
    result: Result<StepResult, StepFailure>,
    /// The runner's validation of the outputs (when the step returned some).
    checked: Option<Result<Vec<WorkflowStepOutput>, Invalid>>,
}

enum Dispatch {
    Started(Flight),
    /// A limit or reservation does not fit now; try again later.
    Wait,
    Stop(Stop),
}

/// What a granted dispatch may use.
struct Grant {
    time_limit: Duration,
    max_output_bytes: u64,
    model_requests: u32,
}

/// Validated outputs by (step, port): one value, or a forEach step's values
/// in instance order.
type Outputs = HashMap<(String, String), Vec<WorkflowStepOutput>>;

/// The scheduler's own state (only its thread touches it).
#[derive(Default)]
struct Sched {
    outputs: Outputs,
    /// forEach instance outputs: step index → instance → outputs.
    collected: HashMap<usize, BTreeMap<u32, Vec<WorkflowStepOutput>>>,
    /// Steps whose condition held.
    cleared: HashSet<usize>,
    flights: HashMap<Unit, Flight>,
    stop: Option<Stop>,
}

/// What each plan step waits for.
struct Deps {
    /// Steps it needs (data producers, prerequisites).
    needs: Vec<Vec<usize>>,
    /// Of those, the steps whose outputs it consumes.
    data: Vec<Vec<usize>>,
    /// The step its condition reads.
    condition: Vec<Option<usize>>,
}

impl Deps {
    fn of(steps: &[WorkflowPlanStep]) -> Self {
        let index = |id: &str| steps.iter().position(|s| s.step == id);
        let data = steps
            .iter()
            .map(|s| {
                s.bindings
                    .iter()
                    .filter_map(|b| match &b.from {
                        PortSource::Step { step, .. } => index(step),
                        PortSource::Input { .. } => None,
                    })
                    .collect()
            })
            .collect();
        let needs = steps
            .iter()
            .map(|s| {
                let mut deps: Vec<usize> = s
                    .bindings
                    .iter()
                    .filter_map(|b| match &b.from {
                        PortSource::Step { step, .. } => index(step),
                        PortSource::Input { .. } => None,
                    })
                    .chain(s.prerequisites.iter().filter_map(|p| index(p)))
                    .collect();
                deps.sort_unstable();
                deps.dedup();
                deps
            })
            .collect();
        let condition = steps.iter().map(|s| s.condition.as_ref().and_then(|c| index(&c.source.step))).collect();
        Self { needs, data, condition }
    }
}

fn done(state: WorkflowStepState) -> bool {
    !matches!(state, WorkflowStepState::Pending | WorkflowStepState::Running)
}

fn did_not_run(state: WorkflowStepState) -> bool {
    matches!(state, WorkflowStepState::Skipped | WorkflowStepState::ConditionNotMet)
}

/// Everything the scheduler and its step threads share.
struct Ctx<'a> {
    inner: &'a Inner,
    key: &'a str,
    root: &'a Path,
    token: &'a CancelToken,
    cell: &'a Mutex<WorkflowRun>,
    /// The run as it started: id, plan, and outcomes (never changes).
    frozen: &'a WorkflowRun,
    started: Instant,
    /// Seeds derive from this run id (the run, or the original it reproduces).
    seed_basis: &'a str,
}

impl Ctx<'_> {
    fn lock(&self) -> std::sync::MutexGuard<'_, WorkflowRun> {
        self.cell.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persist(&self, run: &WorkflowRun, step: &str) -> Result<(), crate::model::WorkflowRunError> {
        self.inner.persist(self.key, run, Some(step))
    }

    fn refuse(&self, unit: Unit, code: &str, message: impl Into<String>) -> Dispatch {
        Dispatch::Stop(refuse(self.cell, &self.frozen.plan.steps[unit.0], unit, code, message, self.inner, self.key))
    }

    /// Mark a pending step as not run (and its instances), with the reason.
    fn not_run(&self, i: usize, state: WorkflowStepState, reason: String) {
        let mut run = self.lock();
        let step = &mut run.steps[i];
        step.state = state;
        for instance in &mut step.instances {
            if instance.state == WorkflowStepState::Pending {
                instance.state = WorkflowStepState::Skipped;
                instance.reason = Some(reason.clone());
            }
        }
        step.reason = Some(reason);
        let id = step.step.clone();
        let _ = self.persist(&run, &id);
    }

    /// The next unit to dispatch in plan order, after recording the steps
    /// that will not run (a step they need did not run, their condition did
    /// not hold). An unreadable condition fails its step (`sched.stop`).
    fn advance(&self, sched: &mut Sched, deps: &Deps) -> Option<Unit> {
        let plan = &self.frozen.plan.steps;
        for (i, planned) in plan.iter().enumerate() {
            let (state, ready, missing, source) = {
                let run = self.lock();
                let state = run.steps[i].state;
                let waits = deps.needs[i].iter().chain(deps.condition[i].iter()).any(|&d| !done(run.steps[d].state));
                let missing = deps.needs[i]
                    .iter()
                    .find(|&&d| did_not_run(run.steps[d].state))
                    .map(|&d| (run.steps[d].label.clone(), deps.data[i].contains(&d)));
                let source = deps.condition[i].map(|d| (run.steps[d].state, run.steps[d].label.clone()));
                (state, !waits, missing, source)
            };
            match state {
                WorkflowStepState::Pending if ready => {
                    if deps.needs[i].iter().any(|&d| self.lock().steps[d].state != WorkflowStepState::Succeeded)
                        && missing.is_none()
                    {
                        // A step it needs failed: the run is stopping.
                        continue;
                    }
                    if let Some((label, consumes)) = missing {
                        let reason = if consumes {
                            format!("“{label}”, whose output it needs, did not run.")
                        } else {
                            format!("“{label}”, which it runs after, did not run.")
                        };
                        self.not_run(i, WorkflowStepState::Skipped, reason);
                        continue;
                    }
                    if let Some(condition) = &planned.condition
                        && !sched.cleared.contains(&i)
                    {
                        let (source_state, source_label) =
                            source.unwrap_or((WorkflowStepState::Skipped, condition.source.step.clone()));
                        let what = format!("output “{}” of “{source_label}”", condition.source.port);
                        if source_state != WorkflowStepState::Succeeded
                            && condition.test != ConditionTest::OutputPresent
                        {
                            if did_not_run(source_state) {
                                let reason =
                                    format!("“{source_label}”, whose output its condition reads, did not run.");
                                self.not_run(i, WorkflowStepState::Skipped, reason);
                            }
                            continue;
                        }
                        let value = sched
                            .outputs
                            .get(&(condition.source.step.clone(), condition.source.port.clone()))
                            .and_then(|v| v.first())
                            .map(|o| &o.value);
                        match crate::condition::evaluate(self.root, &condition.test, condition.negate, value, &what) {
                            Err((code, message)) => {
                                sched.stop = Some(refuse(
                                    self.cell,
                                    planned,
                                    (i, None),
                                    code,
                                    format!("Not started: its condition could not be evaluated: {message}."),
                                    self.inner,
                                    self.key,
                                ));
                                return None;
                            }
                            Ok((false, seen)) => {
                                let reason = format!(
                                    "Its condition did not hold: it runs only when {}, and {seen}.",
                                    crate::condition::describe_as(&condition.test, condition.negate)
                                );
                                self.not_run(i, WorkflowStepState::ConditionNotMet, reason);
                                continue;
                            }
                            Ok(_) => {
                                sched.cleared.insert(i);
                            }
                        }
                    }
                    if let Some(unit) = self.next_unit(i, sched) {
                        return Some(unit);
                    }
                }
                WorkflowStepState::Running => {
                    if let Some(unit) = self.next_unit(i, sched) {
                        return Some(unit);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The unit of step `i` that can start now, if any.
    fn next_unit(&self, i: usize, sched: &Sched) -> Option<Unit> {
        let planned = &self.frozen.plan.steps[i];
        let run = self.lock();
        let step = &run.steps[i];
        match &planned.repeat {
            None => (!sched.flights.contains_key(&(i, None)) && step.state == WorkflowStepState::Pending)
                .then_some((i, None)),
            Some(StepRepeat::ForEach { .. }) => step
                .instances
                .iter()
                .find(|x| x.state == WorkflowStepState::Pending && !sched.flights.contains_key(&(i, Some(x.index))))
                .map(|x| (i, Some(x.index))),
            Some(StepRepeat::Until { .. }) => {
                // One iteration at a time; the next once the last concluded
                // without its predicate holding (the step is still running).
                let busy = sched.flights.keys().any(|(j, _)| *j == i);
                let concluded = step.instances.iter().all(|x| done(x.state));
                (!busy && concluded).then_some((i, Some(step.instances.len() as u32)))
            }
        }
    }

    /// Budget check and reservation (`None` from the inner option: wait).
    fn reserve(&self, planned: &WorkflowPlanStep, flights: &HashMap<Unit, Flight>) -> Result<Option<Grant>, String> {
        let elapsed = self.started.elapsed().as_secs() as u32;
        let run = self.lock();
        let b = &run.budgets;
        let u = &run.usage;
        let reserved_bytes: u64 = flights.values().map(|f| f.bytes).sum();
        let reserved_requests: u32 = flights.values().map(|f| f.model_requests).sum();
        let bytes_left = b.max_output_bytes.saturating_sub(u.output_bytes).saturating_sub(reserved_bytes);
        let requests_left = b.max_model_requests.saturating_sub(u.model_requests).saturating_sub(reserved_requests);
        let model = planned.containment == Containment::ReadOnlyModel;
        let need_requests = if model { planned.max_model_requests.max(1) } else { 0 };
        if u.steps >= b.max_steps {
            return Err(format!("the run may dispatch at most {} steps", b.max_steps));
        }
        if elapsed >= b.max_total_seconds {
            return Err(format!("the run's time budget of {} s is used up", b.max_total_seconds));
        }
        // Running steps hold reservations: wait for them rather than start
        // with less than the step may use.
        if !flights.is_empty()
            && ((planned.max_output_bytes > 0 && bytes_left < planned.max_output_bytes)
                || requests_left < need_requests)
        {
            return Ok(None);
        }
        if planned.max_output_bytes > 0 && bytes_left == 0 {
            return Err(format!("the run's output budget of {} bytes is used up", b.max_output_bytes));
        }
        if model && requests_left == 0 {
            return Err(format!("the run's budget of {} model requests is used up", b.max_model_requests));
        }
        let remaining = b.max_total_seconds.saturating_sub(elapsed);
        Ok(Some(Grant {
            time_limit: Duration::from_secs(u64::from(planned.time_limit_seconds.min(remaining).max(1))),
            max_output_bytes: planned.max_output_bytes.min(bytes_left),
            model_requests: need_requests.min(requests_left),
        }))
    }

    /// Check, reserve, persist the intent of, and start `unit`.
    fn dispatch<'scope, 'env>(
        &'env self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        unit: Unit,
        sched: &Sched,
        done: &mpsc::Sender<Completion>,
    ) -> Dispatch {
        let (inner, root) = (self.inner, self.root);
        let planned = &self.frozen.plan.steps[unit.0];
        if self.token.is_canceled() {
            return Dispatch::Stop(Stop::canceled(self.token));
        }
        if inner.host.project_root().as_deref() != Some(root) {
            return Dispatch::Stop(Stop {
                state: WorkflowRunState::Canceled,
                code: "workflow.projectClosed".into(),
                message:
                    "The run's project is no longer open; the run was canceled. Nothing resumes when it is reopened."
                        .into(),
            });
        }
        let Some(contract) = registry::operation(&planned.operation.id, planned.operation.version) else {
            return Dispatch::Stop(Stop::failed(
                "workflow.notWellFormed",
                format!("“{}” names an unknown operation", planned.label),
            ));
        };
        // The step as this instance runs it (a forEach value applied).
        let mut step = planned.clone();
        let iteration = unit.1.map(|k| {
            let parameter = match &planned.repeat {
                Some(StepRepeat::ForEach { parameter, values }) => {
                    values.get(k as usize).map(|v| (parameter.clone(), v.clone()))
                }
                _ => None,
            };
            step.parameters = instance_parameters(planned, k);
            step.time_limit_seconds = time_limit_of(&step.parameters, &contract);
            StepIteration { index: k, parameter }
        });
        let grant = match self.reserve(&step, &sched.flights) {
            Ok(Some(grant)) => grant,
            Ok(None) => return Dispatch::Wait,
            Err(problem) => {
                return self.refuse(unit, "workflow.budgetExhausted", format!("Not started: {problem}."));
            }
        };
        // An application-wide slot of the step's kind (held until it ends).
        let Some(slot) = inner.slots.try_acquire(SlotKind::of(planned)) else {
            return Dispatch::Wait;
        };
        if let Some(missing) = planned.prerequisites.iter().find(|p| {
            let run = self.lock();
            run.steps.iter().find(|s| &s.step == *p).is_none_or(|s| s.state != WorkflowStepState::Succeeded)
        }) {
            return self.refuse(
                unit,
                "workflow.prerequisite",
                format!("Not started: its prerequisite “{missing}” did not succeed."),
            );
        }
        let inputs = match bind_inputs(root, self.frozen, &step, &contract, &sched.outputs) {
            Ok(inputs) => inputs,
            Err((code, message)) => return self.refuse(unit, code, message),
        };
        if planned.containment == Containment::NativeAccount && !inner.host.trusted(root) {
            return self.refuse(
                unit,
                "workflow.untrustedFolder",
                "Not started: the project folder is no longer trusted, and this step runs natively with your account's privileges.",
            );
        }
        let Some(adapter) = inner.adapters.get(&planned.runner).cloned() else {
            return self.refuse(unit, "workflow.runnerUnavailable", "Not started: no runner is installed.");
        };
        let execution = &self.frozen.plan.execution;
        let approved_restricted = (execution.restricted_backend.clone(), execution.restricted_image.clone());
        let mut approved_image_id = execution.restricted_image_id.clone();
        // A runner with its own image (R, Julia): exactly the image and the
        // identity the plan bound for it, re-checked now.
        let runner_image = execution.runner_images.iter().flatten().find(|b| b.runner == planned.runner).cloned();
        if let (Some(bound), Containment::RestrictedContainer) = (&runner_image, planned.containment) {
            let now = adapter.restricted_image();
            if bound.image_id.is_none() || now != Some(Ok((bound.image.clone(), bound.image_id.clone()))) {
                let now = match &now {
                    Some(Ok((image, id))) => format!("{image} {}", id.as_deref().unwrap_or("of unknown identity")),
                    Some(Err(reason)) => reason.clone(),
                    None => "the default image".to_owned(),
                };
                return self.refuse(
                    unit,
                    "workflow.restrictedUnavailable",
                    format!(
                        "Not started: the restricted image is not the image you approved (approved {} {}, now {now}); restricted steps run only in the approved image.",
                        bound.image,
                        bound.image_id.as_deref().unwrap_or("of unknown identity")
                    ),
                );
            }
            approved_image_id = bound.image_id.clone();
        } else if planned.containment == Containment::RestrictedContainer {
            if inner.host.restricted() != approved_restricted {
                let now = inner.host.restricted().1;
                return self.refuse(
                    unit,
                    "workflow.restrictedUnavailable",
                    format!(
                        "Not started: the restricted image changed since you approved the plan ({} → {}); restricted steps run only in the approved image.",
                        approved_restricted.1.as_deref().unwrap_or("none"),
                        now.as_deref().unwrap_or("none")
                    ),
                );
            }
            // Same name, different image (re-tagged or pulled since approval).
            let now = inner.host.restricted_image_id();
            if approved_image_id.is_none() || now != approved_image_id {
                return self.refuse(
                    unit,
                    "workflow.restrictedUnavailable",
                    format!(
                        "Not started: the restricted image {} is not the image you approved (approved {}, now {}); restricted steps run only in the approved image.",
                        approved_restricted.1.as_deref().unwrap_or("none"),
                        approved_image_id.as_deref().unwrap_or("unknown"),
                        now.as_deref().unwrap_or("unknown")
                    ),
                );
            }
        }
        if let Err(reason) = adapter.availability(root) {
            let code = if planned.containment == Containment::RestrictedContainer {
                "workflow.restrictedUnavailable"
            } else {
                "workflow.runnerUnavailable"
            };
            return self.refuse(
                unit,
                code,
                format!(
                    "Not started: the runner is no longer available ({reason}); it is never replaced by another runner."
                ),
            );
        }

        // ----- dispatch intent (persisted before the service is called) -----
        let attempt = {
            let mut run = self.lock();
            let i = unit.0;
            run.steps[i].state = WorkflowStepState::Running;
            if let Some(k) = unit.1 {
                let instance = instance_mut(&mut run, i, k);
                instance.state = WorkflowStepState::Running;
                if instance.parameter.is_none() {
                    instance.parameter = iteration.as_ref().and_then(|it| it.parameter.as_ref().map(|p| p.1.clone()));
                }
            }
            let attempts = attempts_mut(&mut run, unit);
            let attempt = attempts.len() as u32 + 1;
            attempts.push(WorkflowStepAttempt {
                attempt,
                state: WorkflowAttemptState::Intent,
                operation: None,
                intent_at_ms: crate::now_ms(),
                dispatched_at_ms: None,
                finished_at_ms: None,
                code: None,
                message: None,
                outputs: Vec::new(),
                rejected: Vec::new(),
                log: None,
                environment: None,
            });
            run.usage.steps += 1;
            if let Err(e) = self.persist(&run, &planned.step) {
                // Without a durable intent nothing is dispatched.
                let a = attempts_mut(&mut run, unit).last_mut().expect("just pushed");
                a.state = WorkflowAttemptState::Failed;
                a.code = Some(e.code.clone());
                a.message = Some(e.message.clone());
                run.steps[i].state = WorkflowStepState::Failed;
                if let Some(k) = unit.1 {
                    instance_mut(&mut run, i, k).state = WorkflowStepState::Failed;
                }
                return Dispatch::Stop(Stop::failed(&e.code, e.message));
            }
            attempt
        };
        let flight = Flight { _slot: slot, bytes: grant.max_output_bytes, model_requests: grant.model_requests };
        let done = done.clone();
        scope.spawn(move || {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.run_attempt(
                    &step,
                    &contract,
                    adapter.as_ref(),
                    (unit, attempt),
                    &inputs,
                    &grant,
                    approved_image_id,
                    iteration,
                )
            }));
            let completion = ran.unwrap_or_else(|_| Completion {
                unit,
                attempt,
                reported: None,
                result: Err(StepFailure::new(
                    "workflow.internal",
                    "The step's service stopped unexpectedly; nothing was repeated.",
                )),
                checked: None,
            });
            let _ = done.send(completion);
        });
        Dispatch::Started(flight)
    }

    /// Run one attempt on the current (step) thread and validate its outputs.
    #[allow(clippy::too_many_arguments)]
    fn run_attempt(
        &self,
        step: &WorkflowPlanStep,
        contract: &OperationContract,
        adapter: &dyn StepAdapter,
        (unit, attempt): (Unit, u32),
        inputs: &[BoundInput],
        grant: &Grant,
        restricted_image_id: Option<String>,
        iteration: Option<StepIteration>,
    ) -> Completion {
        let reported: Mutex<Option<OperationRef>> = Mutex::new(None);
        let canceled = || self.token.is_canceled();
        let on_dispatched = |operation: &OperationRef| {
            *reported.lock().unwrap_or_else(|p| p.into_inner()) = Some(operation.clone());
            let mut run = self.lock();
            if let Some(a) = attempts_mut(&mut run, unit).iter_mut().find(|a| a.attempt == attempt)
                && a.state == WorkflowAttemptState::Intent
            {
                a.state = WorkflowAttemptState::Dispatched;
                a.operation = Some(operation.clone());
                a.dispatched_at_ms = Some(crate::now_ms());
                if let Err(e) = self.persist(&run, &step.step) {
                    tracing::warn!("could not persist a workflow dispatch: {e}");
                }
            }
        };
        let control = StepControl { canceled: &canceled, on_dispatched: &on_dispatched };
        let instance = iteration.as_ref().map(|i| i.index);
        let request = StepRequest {
            root: self.root,
            run: &self.frozen.id,
            step,
            attempt,
            inputs,
            time_limit: grant.time_limit,
            max_output_bytes: grant.max_output_bytes,
            model_requests_remaining: grant.model_requests,
            restricted_image_id,
            seed_basis: self.seed_basis,
            iteration,
        };
        // A panicking service must not leave the run active forever.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| adapter.run(&request, &control)))
            .unwrap_or_else(|_| {
                Err(StepFailure::new(
                    "workflow.internal",
                    "The step's service stopped unexpectedly; nothing was repeated.",
                ))
            });
        let reported = reported.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let checked = result.as_ref().ok().map(|result| {
            validate_outputs(self.root, self.frozen, step, contract, (attempt, instance), reported.as_ref(), result)
        });
        Completion { unit, attempt, reported, result, checked }
    }

    /// Record a finished attempt (its slot and reservation are released by
    /// the caller once this returns).
    fn complete(&self, done: Completion, sched: &mut Sched) {
        let (i, instance) = done.unit;
        let planned = &self.frozen.plan.steps[i];
        // An until iteration's predicate, on its own validated output (read
        // before the run record is locked).
        let until = match (&planned.repeat, &done.checked) {
            (Some(StepRepeat::Until { port, test, negate, .. }), Some(Ok(valid))) => {
                let value = valid.iter().find(|o| &o.port == port).map(|o| &o.value);
                let what = format!("output “{port}” of “{}”", planned.label);
                Some(crate::condition::evaluate(self.root, test, *negate, value, &what))
            }
            _ => None,
        };
        let mut run = self.lock();
        let now = crate::now_ms();
        let Some(ai) = attempts_mut(&mut run, done.unit).iter().position(|a| a.attempt == done.attempt) else {
            return;
        };
        let reported = done.reported;
        let stop = &mut sched.stop;
        // The step fails (unless it already ended): first failure wins.
        let fail_step = |run: &mut WorkflowRun, state: WorkflowStepState| {
            if let Some(k) = instance {
                instance_mut(run, i, k).state = state;
            }
            if run.steps[i].state == WorkflowStepState::Running {
                run.steps[i].state = state;
            }
        };
        match done.result {
            Err(failure) => {
                run.usage.model_requests += failure.model_requests;
                let was_canceled = self.token.is_canceled() || failure.code == "workflow.canceled";
                let attempt = &mut attempts_mut(&mut run, done.unit)[ai];
                finish_attempt(attempt, &failure, reported.as_ref(), now, was_canceled);
                attempt.environment = Some(
                    failure
                        .environment
                        .clone()
                        .unwrap_or_else(|| crate::environment::WorkflowEnvironment::of_step(planned)),
                );
                fail_step(&mut run, if was_canceled { WorkflowStepState::Canceled } else { WorkflowStepState::Failed });
                if stop.is_none() {
                    *stop = Some(if was_canceled {
                        Stop::canceled(self.token)
                    } else {
                        Stop::failed(&failure.code, format!("“{}” failed: {}", planned.label, failure.message))
                    });
                }
            }
            Ok(result) => {
                // Requests were made whether or not the outputs are accepted.
                run.usage.model_requests += result.model_requests;
                let checked = done.checked.unwrap_or_else(|| {
                    Err(("workflow.internal", "the outputs were not validated".to_owned(), Vec::new()))
                });
                let attempt = &mut attempts_mut(&mut run, done.unit)[ai];
                attempt.environment = Some(
                    result
                        .environment
                        .clone()
                        .unwrap_or_else(|| crate::environment::WorkflowEnvironment::of_step(planned)),
                );
                attempt.log = result.log.clone();
                attempt.finished_at_ms = Some(now);
                if attempt.operation.is_none() {
                    attempt.operation = result.operation.clone().or(reported.clone());
                }
                attempt.rejected = result.rejected.clone();
                match checked {
                    Ok(valid) => {
                        attempt.state = WorkflowAttemptState::Succeeded;
                        attempt.outputs = valid.clone();
                        run.usage.output_bytes += result.extra_output_bytes;
                        for output in &valid {
                            if let WorkflowOutputValue::File { bytes, .. } = &output.value {
                                run.usage.output_bytes += bytes;
                            }
                        }
                        produced(
                            &mut run,
                            planned,
                            done.unit,
                            valid,
                            until,
                            &mut sched.outputs,
                            &mut sched.collected,
                            stop,
                        );
                    }
                    Err((code, message, mut rejected)) => {
                        attempt.state = WorkflowAttemptState::Failed;
                        attempt.code = Some(code.to_owned());
                        attempt.message = Some(message.clone());
                        attempt.rejected.append(&mut rejected);
                        fail_step(&mut run, WorkflowStepState::Failed);
                        if stop.is_none() {
                            *stop = Some(Stop::failed(code, format!("“{}”: {message}", planned.label)));
                        }
                    }
                }
            }
        }
        run.usage.seconds = self.started.elapsed().as_secs() as u32;
        if stop.is_none() && run.usage.output_bytes > run.budgets.max_output_bytes {
            *stop = Some(Stop::failed(
                "workflow.budgetExhausted",
                format!("The run's outputs exceed its budget of {} bytes.", run.budgets.max_output_bytes),
            ));
        }
        let _ = self.persist(&run, &planned.step);
    }
}

/// Record the validated outputs of a succeeded attempt of `unit`: a plain
/// step succeeds; a forEach step once every instance succeeded (its outputs
/// in instance order); an until step once its predicate holds, or on
/// exhaustion (fail, or use the last iteration).
#[allow(clippy::too_many_arguments)]
fn produced(
    run: &mut WorkflowRun,
    planned: &WorkflowPlanStep,
    (i, instance): Unit,
    valid: Vec<WorkflowStepOutput>,
    until: Option<Result<(bool, String), crate::condition::Unreadable>>,
    outputs: &mut Outputs,
    collected: &mut HashMap<usize, BTreeMap<u32, Vec<WorkflowStepOutput>>>,
    stop: &mut Option<Stop>,
) {
    let publish = |run: &mut WorkflowRun, outputs: &mut Outputs, valid: &[WorkflowStepOutput]| {
        if run.steps[i].state == WorkflowStepState::Running {
            run.steps[i].state = WorkflowStepState::Succeeded;
        }
        for output in valid {
            for outcome in run.outcomes.iter_mut() {
                if outcome.step == planned.step && outcome.port == output.port {
                    outcome.value = Some(output.value.clone());
                }
            }
            outputs.insert((planned.step.clone(), output.port.clone()), vec![output.clone()]);
        }
    };
    let Some(k) = instance else {
        publish(run, outputs, &valid);
        return;
    };
    instance_mut(run, i, k).state = WorkflowStepState::Succeeded;
    match &planned.repeat {
        Some(StepRepeat::ForEach { .. }) => {
            let all = collected.entry(i).or_default();
            all.insert(k, valid);
            let step = &mut run.steps[i];
            if step.state == WorkflowStepState::Running
                && step.instances.iter().all(|x| x.state == WorkflowStepState::Succeeded)
            {
                step.state = WorkflowStepState::Succeeded;
                // Consumers receive every instance's output, in index order.
                for instance_outputs in all.values() {
                    for o in instance_outputs {
                        outputs.entry((planned.step.clone(), o.port.clone())).or_default().push(o.clone());
                    }
                }
            }
        }
        Some(StepRepeat::Until { max_iterations, on_exhausted, .. }) => {
            let max = (*max_iterations).clamp(1, crate::plan::MAX_UNTIL);
            match until {
                Some(Err((code, message))) => {
                    // The iteration ran, but whether to stop cannot be told:
                    // it failed, and its outputs reach no consumer.
                    let failed = instance_mut(run, i, k);
                    failed.state = WorkflowStepState::Failed;
                    failed.reason = Some(format!("Its predicate could not be evaluated: {message}."));
                    if let Some(a) = failed.attempts.last_mut() {
                        a.state = WorkflowAttemptState::Failed;
                        a.code = Some(code.to_owned());
                        a.message = Some(format!(
                            "The iteration ran, but its predicate could not be evaluated ({message}); its outputs are discarded."
                        ));
                        a.outputs.clear();
                    }
                    if run.steps[i].state == WorkflowStepState::Running {
                        run.steps[i].state = WorkflowStepState::Failed;
                    }
                    if stop.is_none() {
                        *stop = Some(Stop::failed(
                            code,
                            format!("“{}”: its predicate could not be evaluated: {message}", planned.label),
                        ));
                    }
                }
                Some(Ok((true, why))) => {
                    instance_mut(run, i, k).reason = Some(format!("Its predicate held: {why}."));
                    publish(run, outputs, &valid);
                }
                Some(Ok((false, why))) => {
                    instance_mut(run, i, k).reason = Some(format!("Its predicate did not hold: {why}."));
                    if k + 1 >= max {
                        match on_exhausted {
                            Exhausted::Fail => {
                                run.steps[i].reason =
                                    Some(format!("Its predicate did not hold in any of its {max} iterations."));
                                if run.steps[i].state == WorkflowStepState::Running {
                                    run.steps[i].state = WorkflowStepState::Failed;
                                }
                                if stop.is_none() {
                                    *stop = Some(Stop::failed(
                                        "workflow.untilExhausted",
                                        format!(
                                            "“{}” ran {max} iterations and its predicate never held.",
                                            planned.label
                                        ),
                                    ));
                                }
                            }
                            Exhausted::UseLast => {
                                run.steps[i].reason = Some(format!(
                                    "Its predicate did not hold in any of its {max} iterations; the last iteration's outputs are used."
                                ));
                                publish(run, outputs, &valid);
                            }
                        }
                    }
                    // Otherwise the step keeps running: the next iteration starts.
                }
                None => publish(run, outputs, &valid),
            }
        }
        None => publish(run, outputs, &valid),
    }
}

/// Execute a persisted run on the current thread until it ends.
pub(crate) fn execute(inner: Arc<Inner>, run: WorkflowRun, root: PathBuf, key: String, token: Arc<CancelToken>) {
    let started = Instant::now();
    let frozen = run.clone();
    let cell = Mutex::new(run);
    // Seeds derive from the run, or for a reproduction from its original.
    let seed_basis = crate::environment::seed_basis(&frozen, &|id| inner.stored_run(id));
    let ctx = Ctx {
        inner: &inner,
        key: &key,
        root: &root,
        token: &token,
        cell: &cell,
        frozen: &frozen,
        started,
        seed_basis: &seed_basis,
    };
    debug_assert!(frozen.plan.steps.iter().zip(&frozen.steps).all(|(p, s)| p.step == s.step));
    let deps = Deps::of(&frozen.plan.steps);
    let max_parallel = frozen.plan.max_parallel.clamp(1, MAX_PARALLEL) as usize;
    let mut sched = Sched::default();
    let (done_tx, done_rx) = mpsc::channel::<Completion>();

    std::thread::scope(|scope| {
        let _cancel_on_panic = CancelOnPanic(&token);
        let ctx = &ctx;
        let finish = |done: Completion, sched: &mut Sched| {
            let unit = done.unit;
            ctx.complete(done, sched);
            // The slot and the reservation are released once the result is recorded.
            sched.flights.remove(&unit);
        };
        loop {
            while let Ok(done) = done_rx.try_recv() {
                finish(done, &mut sched);
            }
            if sched.stop.is_none() && token.is_canceled() {
                sched.stop = Some(Stop::canceled(&token));
            }
            let mut waiting = false;
            while sched.stop.is_none() && sched.flights.len() < max_parallel {
                let Some(unit) = ctx.advance(&mut sched, &deps) else { break };
                match ctx.dispatch(scope, unit, &sched, &done_tx) {
                    Dispatch::Started(flight) => {
                        sched.flights.insert(unit, flight);
                    }
                    Dispatch::Wait => {
                        waiting = true;
                        break;
                    }
                    Dispatch::Stop(s) => sched.stop = Some(s),
                }
            }
            if sched.flights.is_empty() && (sched.stop.is_some() || !waiting) {
                break;
            }
            if let Ok(done) = done_rx.recv_timeout(if waiting { SLOT_POLL } else { IDLE_POLL }) {
                finish(done, &mut sched);
            }
        }
    });

    let mut run = cell.into_inner().unwrap_or_else(|p| p.into_inner());
    let stopped = match &sched.stop {
        Some(s) if s.state == WorkflowRunState::Canceled => "The run was canceled before this step could start.",
        Some(_) => "The run stopped (a step failed) before this step could start.",
        None => "The run ended before this step could start.",
    };
    for step in &mut run.steps {
        for instance in &mut step.instances {
            if instance.state == WorkflowStepState::Pending {
                instance.state = WorkflowStepState::Skipped;
                instance.reason.get_or_insert_with(|| stopped.replace("this step", "this instance"));
            }
        }
        match step.state {
            WorkflowStepState::Pending => {
                step.state = WorkflowStepState::Skipped;
                step.reason.get_or_insert_with(|| stopped.to_owned());
            }
            // A repeated step between instances when the run stopped.
            WorkflowStepState::Running => {
                step.state = WorkflowStepState::Canceled;
                step.reason.get_or_insert_with(|| "The run stopped before its remaining instances could start.".into());
            }
            _ => {}
        }
    }
    match sched.stop {
        None => {
            run.state = WorkflowRunState::Succeeded;
        }
        Some(stop) => {
            run.state = stop.state;
            run.code = Some(stop.code);
            run.message = Some(stop.message);
        }
    }
    run.usage.seconds = started.elapsed().as_secs() as u32;
    run.finished_at_ms = Some(crate::now_ms());
    run.explain_outcomes();
    let _ = inner.persist(&key, &run, None);
    inner.finished(&run.id);
}

/// Refuse a unit before dispatch: a failed attempt that was never dispatched.
fn refuse(
    cell: &Mutex<WorkflowRun>,
    planned: &WorkflowPlanStep,
    unit: Unit,
    code: &str,
    message: impl Into<String>,
    inner: &Inner,
    key: &str,
) -> Stop {
    let message = message.into();
    let mut run = cell.lock().unwrap_or_else(|p| p.into_inner());
    let i = step_index(&run, &planned.step);
    let unit = (i, unit.1);
    let now = crate::now_ms();
    let attempts = attempts_mut(&mut run, unit);
    let attempt = attempts.len() as u32 + 1;
    attempts.push(WorkflowStepAttempt {
        attempt,
        state: WorkflowAttemptState::Failed,
        operation: None,
        intent_at_ms: now,
        dispatched_at_ms: None,
        finished_at_ms: Some(now),
        code: Some(code.to_owned()),
        message: Some(message.clone()),
        outputs: Vec::new(),
        rejected: Vec::new(),
        log: None,
        environment: None,
    });
    if let Some(k) = unit.1 {
        instance_mut(&mut run, i, k).state = WorkflowStepState::Failed;
    }
    run.steps[i].state = WorkflowStepState::Failed;
    let _ = inner.persist(key, &run, Some(&planned.step));
    Stop::failed(code, format!("“{}”: {message}", planned.label))
}

fn finish_attempt(
    attempt: &mut WorkflowStepAttempt,
    failure: &StepFailure,
    reported: Option<&OperationRef>,
    now: u64,
    canceled: bool,
) {
    attempt.state = if canceled { WorkflowAttemptState::Canceled } else { WorkflowAttemptState::Failed };
    attempt.code = Some(if canceled { "workflow.canceled".to_owned() } else { failure.code.clone() });
    attempt.message = Some(failure.message.clone());
    attempt.finished_at_ms = Some(now);
    if attempt.operation.is_none() {
        attempt.operation = failure.operation.clone().or(reported.cloned());
    }
    attempt.rejected = failure.rejected.clone();
    attempt.log = failure.log.clone();
}

/// The validated values bound to a step's input ports, re-verified now.
fn bind_inputs(
    root: &Path,
    run: &WorkflowRun,
    planned: &WorkflowPlanStep,
    contract: &OperationContract,
    outputs: &Outputs,
) -> Result<Vec<BoundInput>, (&'static str, String)> {
    let mut bound = Vec::new();
    for binding in &planned.bindings {
        if contract.input(&binding.port).is_none() {
            return Err(("workflow.notWellFormed", format!("{} has no input {}", contract.title, binding.port)));
        }
        match &binding.from {
            PortSource::Input { input } => {
                let Some(approved) = run.plan.inputs.iter().find(|i| &i.id == input) else {
                    return Err(("workflow.inputMissing", format!("Not started: input “{input}” was not resolved.")));
                };
                match hash_project_file(root, &approved.path) {
                    Ok((sha256, bytes)) if sha256 == approved.sha256 => bound.push(BoundInput {
                        port: binding.port.clone(),
                        port_type: approved.port_type,
                        value: WorkflowOutputValue::File {
                            path: approved.path.clone(),
                            sha256,
                            bytes,
                            media_type: String::new(),
                            research_run: None,
                            artifact: None,
                        },
                    }),
                    Ok((sha256, _)) => {
                        return Err((
                            "workflow.inputChanged",
                            format!(
                                "Not started: input “{}” ({}) changed since the plan was approved (sha256 {} → {}).",
                                approved.label,
                                approved.path,
                                &approved.sha256[..12],
                                &sha256[..12]
                            ),
                        ));
                    }
                    Err(message) => {
                        return Err((
                            "workflow.inputChanged",
                            format!("Not started: input “{}” is no longer readable: {message}.", approved.label),
                        ));
                    }
                }
            }
            PortSource::Step { step, port } => {
                // One value, or every instance's of a forEach step, in order.
                let produced = outputs.get(&(step.clone(), port.clone())).filter(|v| !v.is_empty());
                let Some(produced) = produced else {
                    return Err((
                        "workflow.missingOutput",
                        format!("Not started: step “{step}” produced no validated “{port}” output."),
                    ));
                };
                for output in produced {
                    if let Some((path, sha256)) = file_of(&output.value) {
                        match hash_project_file(root, path) {
                            Ok((found, _)) if found == bare_hash(sha256) => {}
                            Ok(_) | Err(_) => {
                                return Err((
                                    "workflow.outputChanged",
                                    format!(
                                        "Not started: {path}, the “{port}” output of step “{step}”, was altered or removed after it was validated."
                                    ),
                                ));
                            }
                        }
                    }
                    bound.push(BoundInput {
                        port: binding.port.clone(),
                        port_type: output.port_type,
                        value: output.value.clone(),
                    });
                }
            }
        }
    }
    Ok(bound)
}

fn file_of(value: &WorkflowOutputValue) -> Option<(&exposition_core::path::WorkspacePath, &str)> {
    match value {
        WorkflowOutputValue::File { path, sha256, .. } | WorkflowOutputValue::Retained { path, sha256, .. } => {
            Some((path, sha256.as_str()))
        }
        _ => None,
    }
}

type Invalid = (&'static str, String, Vec<WorkflowRejectedOutput>);

/// Rule 5/6: validate what the adapter claims before any consumer sees it.
fn validate_outputs(
    root: &Path,
    run: &WorkflowRun,
    planned: &WorkflowPlanStep,
    contract: &OperationContract,
    attempt: (u32, Option<u32>),
    reported: Option<&OperationRef>,
    result: &StepResult,
) -> Result<Vec<WorkflowStepOutput>, Invalid> {
    let mut valid: Vec<WorkflowStepOutput> = Vec::new();
    let mut rejected = Vec::new();
    let mut first: Option<(&'static str, String)> = None;
    // Provenance is checked against the operation persisted at dispatch.
    if let (Some(dispatched), Some(claimed)) = (reported, result.operation.as_ref())
        && dispatched != claimed
    {
        let reason = format!(
            "the service reported {} {} at dispatch but returned {} {}",
            dispatched.kind, dispatched.id, claimed.kind, claimed.id
        );
        return Err(("workflow.invalidOutput", reason, Vec::new()));
    }
    let operation = reported.or(result.operation.as_ref());
    for output in &result.outputs {
        let problem = check_output(root, run, planned, contract, attempt, operation, output);
        match problem {
            Ok(()) if valid.iter().any(|v| v.port == output.port) => {
                let reason = format!("port “{}” was returned more than once", output.port);
                rejected.push(WorkflowRejectedOutput { name: output.port.clone(), reason: reason.clone() });
                first.get_or_insert(("workflow.invalidOutput", reason));
            }
            Ok(()) => valid.push(output.clone()),
            Err((code, reason)) => {
                rejected.push(WorkflowRejectedOutput { name: output.port.clone(), reason: reason.clone() });
                first.get_or_insert((code, reason));
            }
        }
    }
    if let Some((code, reason)) = first {
        return Err((code, format!("an output was refused: {reason}; the step's outputs are discarded"), rejected));
    }
    // Ports a consumer or a required outcome needs must be present.
    let mut needed: Vec<String> = run
        .plan
        .steps
        .iter()
        .flat_map(|s| s.bindings.iter())
        .filter_map(|b| match &b.from {
            PortSource::Step { step, port } if step == &planned.step => Some(port.clone()),
            _ => None,
        })
        .collect();
    needed.extend(run.outcomes.iter().filter(|o| !o.optional && o.step == planned.step).map(|o| o.port.clone()));
    needed.sort();
    needed.dedup();
    let missing: Vec<&String> = needed.iter().filter(|p| !valid.iter().any(|v| &&v.port == p)).collect();
    if !missing.is_empty() {
        let names = missing.iter().map(|p| format!("“{p}”")).collect::<Vec<_>>().join(", ");
        return Err((
            "workflow.missingOutput",
            format!(
                "the step did not produce {names}, which the workflow needs (output port p is realized by the file p.<ext>)"
            ),
            rejected,
        ));
    }
    Ok(valid)
}

fn check_output(
    root: &Path,
    run: &WorkflowRun,
    planned: &WorkflowPlanStep,
    contract: &OperationContract,
    (attempt, instance): (u32, Option<u32>),
    operation: Option<&OperationRef>,
    output: &WorkflowStepOutput,
) -> Result<(), (&'static str, String)> {
    // Rule 6 first: service-owned values come only from their owning operation.
    let owned = match &output.value {
        WorkflowOutputValue::FormalEvidence { .. } => Some(PortType::FormalEvidence),
        WorkflowOutputValue::Retained { .. } => Some(PortType::RetainedArtifact),
        _ => None,
    };
    for ty in owned.into_iter().chain([output.port_type].into_iter().filter(|t| t.is_service_owned())) {
        if ty.owner() != Some(contract.id.as_str()) {
            return Err((
                "workflow.forgedOutput",
                format!(
                    "{} can be produced only by {}; {} returned it, so it is refused",
                    ty.title(),
                    ty.owner().unwrap_or("its service"),
                    contract.title
                ),
            ));
        }
    }
    let Some(spec) = contract.output(&output.port) else {
        return Err(("workflow.invalidOutput", format!("{} has no output port “{}”", contract.title, output.port)));
    };
    let declared = spec.types[0];
    if output.port_type != declared {
        return Err((
            "workflow.invalidOutput",
            format!("“{}” must be {}, not {}", output.port, declared.title(), output.port_type.title()),
        ));
    }
    let kind_ok = matches!(
        (&output.value, declared),
        (WorkflowOutputValue::File { .. }, PortType::Dataset | PortType::Figure | PortType::Table)
            | (WorkflowOutputValue::FormalEvidence { .. }, PortType::FormalEvidence)
            | (WorkflowOutputValue::Retained { .. }, PortType::RetainedArtifact)
            | (WorkflowOutputValue::Report { .. }, PortType::ReviewReport)
            | (WorkflowOutputValue::PromotionProposal { .. }, PortType::PromotionProposal)
            | (WorkflowOutputValue::AssociationProposal { .. }, PortType::AssociationProposal)
    );
    if !kind_ok {
        return Err(("workflow.invalidOutput", format!("“{}” is not a {} value", output.port, declared.title())));
    }
    // Provenance: this run, this step, this attempt, the operation it started.
    let p = &output.provenance;
    if p.run != run.id
        || p.step != planned.step
        || p.attempt != attempt
        || p.instance != instance
        || Some(&p.operation) != operation
    {
        return Err(("workflow.invalidOutput", format!("“{}” does not carry this attempt's provenance", output.port)));
    }
    match &output.value {
        WorkflowOutputValue::File { path, sha256, bytes, research_run, .. } => {
            let Some(research_run) = research_run else {
                return Err(("workflow.invalidOutput", format!("{path} is not an output of the step's research run")));
            };
            if operation.is_none_or(|op| op.kind != "researchRun" || &op.id != research_run) {
                return Err((
                    "workflow.invalidOutput",
                    format!("{path} belongs to another research run than this attempt's"),
                ));
            }
            let area = format!(".exposition/artifacts/{research_run}/");
            if !path.as_str().starts_with(&area) {
                return Err(("workflow.invalidOutput", format!("{path} is outside this run's output area")));
            }
            if let Err(reason) = resolve(root, path) {
                return Err(("workflow.invalidOutput", reason));
            }
            let (found, size) = hash_project_file(root, path).map_err(|r| ("workflow.invalidOutput", r))?;
            if found != bare_hash(sha256) || size != *bytes {
                return Err((
                    "workflow.outputChanged",
                    format!("{path} does not match the digest recorded for it (altered after it was produced)"),
                ));
            }
        }
        WorkflowOutputValue::Retained { path, sha256, .. } => {
            let (found, _) = hash_project_file(root, path).map_err(|r| ("workflow.invalidOutput", r))?;
            if found != bare_hash(sha256) {
                return Err(("workflow.outputChanged", format!("{path} does not match its retained digest")));
            }
        }
        WorkflowOutputValue::FormalEvidence { file, file_sha256, declarations, .. } => {
            if file_sha256.len() != 64 || !file_sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(("workflow.invalidOutput", "the audit digest is not a SHA-256".into()));
            }
            // The audit describes exactly the file as it is now.
            let (found, _) = hash_project_file(root, file).map_err(|r| ("workflow.invalidOutput", r))?;
            if found != bare_hash(file_sha256) {
                return Err((
                    "workflow.inputChanged",
                    format!("{file} changed while it was audited; the audit does not describe it"),
                ));
            }
            if declarations.len() > exposition_lean::types::AUDIT_RECORD_LIMIT {
                return Err(("workflow.invalidOutput", "too many audited declarations".into()));
            }
        }
        WorkflowOutputValue::Report { text, .. } => {
            if text.chars().count() > crate::model::MAX_REPORT_CHARS {
                return Err(("workflow.invalidOutput", "the report is too long".into()));
            }
        }
        WorkflowOutputValue::PromotionProposal { .. } | WorkflowOutputValue::AssociationProposal { .. } => {}
    }
    Ok(())
}
