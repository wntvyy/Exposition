//! The bounded runner.
//!
//! A run's steps execute in dependency order: a step becomes ready once
//! every step it consumes data from (and every explicit prerequisite)
//! succeeded; ready steps start in plan order, at most the plan's
//! `max_parallel` at a time (1: exactly one step at a time, in plan order) and
//! only while an application-wide slot of the step's kind is free (shared by
//! every active run, see [`crate::slots`]).
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use exposition_graph::OperationRef;
use exposition_graph::workflow::PortSource;
use exposition_graph::workflow::registry::{self, Containment, OperationContract, PortType};

use crate::adapters::{BoundInput, StepAdapter, StepControl, StepFailure, StepRequest, StepResult, bare_hash};
use crate::model::{
    MAX_PARALLEL, WorkflowAttemptState, WorkflowOutputValue, WorkflowPlanStep, WorkflowRejectedOutput, WorkflowRun,
    WorkflowRunState, WorkflowStepAttempt, WorkflowStepOutput, WorkflowStepState,
};
use crate::plan::{hash_project_file, resolve};
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

/// A dispatched attempt: its slot and what it reserved from the budgets.
struct Flight {
    _slot: SlotGuard,
    bytes: u64,
    model_requests: u32,
}

/// What a finished attempt reports back to the scheduler.
struct Completion {
    index: usize,
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
}

impl Ctx<'_> {
    fn lock(&self) -> std::sync::MutexGuard<'_, WorkflowRun> {
        self.cell.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persist(&self, run: &WorkflowRun, step: &str) -> Result<(), crate::model::WorkflowRunError> {
        self.inner.persist(self.key, run, Some(step))
    }

    fn refuse(&self, planned: &WorkflowPlanStep, code: &str, message: impl Into<String>) -> Dispatch {
        Dispatch::Stop(refuse(self.cell, planned, code, message, self.inner, self.key))
    }

    /// Budget check and reservation (`None` from the inner option: wait).
    fn reserve(&self, planned: &WorkflowPlanStep, flights: &HashMap<usize, Flight>) -> Result<Option<Grant>, String> {
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

    /// Check, reserve, persist the intent of, and start the step at `index`.
    #[allow(clippy::too_many_arguments)]
    fn dispatch<'scope, 'env>(
        &'env self,
        scope: &'scope std::thread::Scope<'scope, 'env>,
        index: usize,
        planned: &'env WorkflowPlanStep,
        outputs: &HashMap<(String, String), WorkflowStepOutput>,
        flights: &HashMap<usize, Flight>,
        done: &mpsc::Sender<Completion>,
    ) -> Dispatch {
        let (inner, root) = (self.inner, self.root);
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
        let grant = match self.reserve(planned, flights) {
            Ok(Some(grant)) => grant,
            Ok(None) => return Dispatch::Wait,
            Err(problem) => {
                return self.refuse(planned, "workflow.budgetExhausted", format!("Not started: {problem}."));
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
                planned,
                "workflow.prerequisite",
                format!("Not started: its prerequisite “{missing}” did not succeed."),
            );
        }
        let inputs = match bind_inputs(root, self.frozen, planned, &contract, outputs) {
            Ok(inputs) => inputs,
            Err((code, message)) => return self.refuse(planned, code, message),
        };
        if planned.containment == Containment::NativeAccount && !inner.host.trusted(root) {
            return self.refuse(
                planned,
                "workflow.untrustedFolder",
                "Not started: the project folder is no longer trusted, and this step runs natively with your account's privileges.",
            );
        }
        let Some(adapter) = inner.adapters.get(&planned.runner).cloned() else {
            return self.refuse(planned, "workflow.runnerUnavailable", "Not started: no runner is installed.");
        };
        let execution = &self.frozen.plan.execution;
        let approved_restricted = (execution.restricted_backend.clone(), execution.restricted_image.clone());
        let approved_image_id = execution.restricted_image_id.clone();
        if planned.containment == Containment::RestrictedContainer {
            if inner.host.restricted() != approved_restricted {
                let now = inner.host.restricted().1;
                return self.refuse(
                    planned,
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
                    planned,
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
                planned,
                code,
                format!(
                    "Not started: the runner is no longer available ({reason}); it is never replaced by another runner."
                ),
            );
        }

        // ----- dispatch intent (persisted before the service is called) -----
        let attempt = {
            let mut run = self.lock();
            let i = step_index(&run, &planned.step);
            let attempt = run.steps[i].attempts.len() as u32 + 1;
            run.steps[i].state = WorkflowStepState::Running;
            run.steps[i].attempts.push(WorkflowStepAttempt {
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
            });
            run.usage.steps += 1;
            if let Err(e) = self.persist(&run, &planned.step) {
                // Without a durable intent nothing is dispatched.
                let attempt = run.steps[i].attempts.last_mut().expect("just pushed");
                attempt.state = WorkflowAttemptState::Failed;
                attempt.code = Some(e.code.clone());
                attempt.message = Some(e.message.clone());
                run.steps[i].state = WorkflowStepState::Failed;
                return Dispatch::Stop(Stop::failed(&e.code, e.message));
            }
            attempt
        };
        let flight = Flight { _slot: slot, bytes: grant.max_output_bytes, model_requests: grant.model_requests };
        let done = done.clone();
        scope.spawn(move || {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.run_attempt(planned, &contract, adapter.as_ref(), attempt, &inputs, &grant, approved_image_id)
            }));
            let completion = ran.unwrap_or_else(|_| Completion {
                index,
                attempt,
                reported: None,
                result: Err(StepFailure::new(
                    "workflow.internal",
                    "The step's service stopped unexpectedly; nothing was repeated.",
                )),
                checked: None,
            });
            let _ = done.send(Completion { index, ..completion });
        });
        Dispatch::Started(flight)
    }

    /// Run one attempt on the current (step) thread and validate its outputs.
    #[allow(clippy::too_many_arguments)]
    fn run_attempt(
        &self,
        planned: &WorkflowPlanStep,
        contract: &OperationContract,
        adapter: &dyn StepAdapter,
        attempt: u32,
        inputs: &[BoundInput],
        grant: &Grant,
        restricted_image_id: Option<String>,
    ) -> Completion {
        let reported: Mutex<Option<OperationRef>> = Mutex::new(None);
        let canceled = || self.token.is_canceled();
        let on_dispatched = |operation: &OperationRef| {
            *reported.lock().unwrap_or_else(|p| p.into_inner()) = Some(operation.clone());
            let mut run = self.lock();
            let i = step_index(&run, &planned.step);
            if let Some(a) = run.steps[i].attempts.iter_mut().find(|a| a.attempt == attempt)
                && a.state == WorkflowAttemptState::Intent
            {
                a.state = WorkflowAttemptState::Dispatched;
                a.operation = Some(operation.clone());
                a.dispatched_at_ms = Some(crate::now_ms());
                if let Err(e) = self.persist(&run, &planned.step) {
                    tracing::warn!("could not persist a workflow dispatch: {e}");
                }
            }
        };
        let control = StepControl { canceled: &canceled, on_dispatched: &on_dispatched };
        let request = StepRequest {
            root: self.root,
            run: &self.frozen.id,
            step: planned,
            attempt,
            inputs,
            time_limit: grant.time_limit,
            max_output_bytes: grant.max_output_bytes,
            model_requests_remaining: grant.model_requests,
            restricted_image_id,
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
            validate_outputs(self.root, self.frozen, planned, contract, attempt, reported.as_ref(), result)
        });
        Completion { index: 0, attempt, reported, result, checked }
    }

    /// Record a finished attempt (and release what it reserved).
    fn complete(
        &self,
        planned: &WorkflowPlanStep,
        done: Completion,
        outputs: &mut HashMap<(String, String), WorkflowStepOutput>,
        stop: &mut Option<Stop>,
    ) {
        let mut run = self.lock();
        let i = step_index(&run, &planned.step);
        let now = crate::now_ms();
        let Some(ai) = run.steps[i].attempts.iter().position(|a| a.attempt == done.attempt) else { return };
        let reported = done.reported;
        match done.result {
            Err(failure) => {
                run.usage.model_requests += failure.model_requests;
                let was_canceled = self.token.is_canceled() || failure.code == "workflow.canceled";
                finish_attempt(&mut run.steps[i].attempts[ai], &failure, reported.as_ref(), now, was_canceled);
                run.steps[i].state = if was_canceled { WorkflowStepState::Canceled } else { WorkflowStepState::Failed };
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
                let attempt = &mut run.steps[i].attempts[ai];
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
                        run.steps[i].state = WorkflowStepState::Succeeded;
                        run.usage.output_bytes += result.extra_output_bytes;
                        for output in valid {
                            if let WorkflowOutputValue::File { bytes, .. } = &output.value {
                                run.usage.output_bytes += bytes;
                            }
                            for outcome in run.outcomes.iter_mut() {
                                if outcome.step == planned.step && outcome.port == output.port {
                                    outcome.value = Some(output.value.clone());
                                }
                            }
                            outputs.insert((planned.step.clone(), output.port.clone()), output);
                        }
                    }
                    Err((code, message, mut rejected)) => {
                        attempt.state = WorkflowAttemptState::Failed;
                        attempt.code = Some(code.to_owned());
                        attempt.message = Some(message.clone());
                        attempt.rejected.append(&mut rejected);
                        run.steps[i].state = WorkflowStepState::Failed;
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

/// Plan indexes of the steps each step depends on (data and prerequisites).
fn dependencies(steps: &[WorkflowPlanStep]) -> Vec<Vec<usize>> {
    let index = |id: &str| steps.iter().position(|s| s.step == id);
    steps
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
        .collect()
}

/// The first pending step in plan order whose dependencies all succeeded.
fn next_ready(run: &WorkflowRun, deps: &[Vec<usize>], flights: &HashMap<usize, Flight>) -> Option<usize> {
    (0..run.steps.len()).find(|&i| {
        run.steps[i].state == WorkflowStepState::Pending
            && !flights.contains_key(&i)
            && deps[i].iter().all(|&d| run.steps[d].state == WorkflowStepState::Succeeded)
    })
}

/// Execute a persisted run on the current thread until it ends.
pub(crate) fn execute(inner: Arc<Inner>, run: WorkflowRun, root: PathBuf, key: String, token: Arc<CancelToken>) {
    let started = Instant::now();
    let frozen = run.clone();
    let cell = Mutex::new(run);
    let ctx = Ctx { inner: &inner, key: &key, root: &root, token: &token, cell: &cell, frozen: &frozen, started };
    let plan_steps = &frozen.plan.steps;
    let deps = dependencies(plan_steps);
    let max_parallel = frozen.plan.max_parallel.clamp(1, MAX_PARALLEL) as usize;
    let mut outputs: HashMap<(String, String), WorkflowStepOutput> = HashMap::new();
    let mut stop: Option<Stop> = None;
    let mut flights: HashMap<usize, Flight> = HashMap::new();
    let (done_tx, done_rx) = mpsc::channel::<Completion>();

    std::thread::scope(|scope| {
        let _cancel_on_panic = CancelOnPanic(&token);
        let ctx = &ctx;
        let finish = |done: Completion,
                      outputs: &mut HashMap<(String, String), WorkflowStepOutput>,
                      flights: &mut HashMap<usize, Flight>,
                      stop: &mut Option<Stop>| {
            let index = done.index;
            ctx.complete(&plan_steps[index], done, outputs, stop);
            // The slot and the reservation are released once the result is recorded.
            flights.remove(&index);
        };
        loop {
            while let Ok(done) = done_rx.try_recv() {
                finish(done, &mut outputs, &mut flights, &mut stop);
            }
            if stop.is_none() && token.is_canceled() {
                stop = Some(Stop::canceled(&token));
            }
            let mut waiting = false;
            while stop.is_none() && flights.len() < max_parallel {
                let next = next_ready(&ctx.lock(), &deps, &flights);
                let Some(index) = next else { break };
                match ctx.dispatch(scope, index, &plan_steps[index], &outputs, &flights, &done_tx) {
                    Dispatch::Started(flight) => {
                        flights.insert(index, flight);
                    }
                    Dispatch::Wait => {
                        waiting = true;
                        break;
                    }
                    Dispatch::Stop(s) => stop = Some(s),
                }
            }
            if flights.is_empty() && (stop.is_some() || !waiting) {
                break;
            }
            if let Ok(done) = done_rx.recv_timeout(if waiting { SLOT_POLL } else { IDLE_POLL }) {
                finish(done, &mut outputs, &mut flights, &mut stop);
            }
        }
    });

    let mut run = cell.into_inner().unwrap_or_else(|p| p.into_inner());
    for step in &mut run.steps {
        if step.state == WorkflowStepState::Pending {
            step.state = WorkflowStepState::Skipped;
        }
    }
    match stop {
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

// Refuse a step before dispatch: a failed attempt that was never dispatched.
fn refuse(
    cell: &Mutex<WorkflowRun>,
    planned: &WorkflowPlanStep,
    code: &str,
    message: impl Into<String>,
    inner: &Inner,
    key: &str,
) -> Stop {
    let message = message.into();
    let mut run = cell.lock().unwrap_or_else(|p| p.into_inner());
    let i = step_index(&run, &planned.step);
    let attempt = run.steps[i].attempts.len() as u32 + 1;
    let now = crate::now_ms();
    run.steps[i].attempts.push(WorkflowStepAttempt {
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
    });
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
    outputs: &HashMap<(String, String), WorkflowStepOutput>,
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
                let Some(output) = outputs.get(&(step.clone(), port.clone())) else {
                    return Err((
                        "workflow.missingOutput",
                        format!("Not started: step “{step}” produced no validated “{port}” output."),
                    ));
                };
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
    attempt: u32,
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
    attempt: u32,
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
    if p.run != run.id || p.step != planned.step || p.attempt != attempt || Some(&p.operation) != operation {
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
