//! The bounded runner: one step at a time, in plan order.
//!
//! Before each dispatch: the run's project is still the open one, the run was
//! not canceled, budgets remain, every external input and upstream output the
//! step consumes still has the digest recorded for it, a native step's folder
//! is still trusted, and the runner is still available (a restricted backend
//! is never downgraded). The attempt's dispatch intent is persisted before the
//! adapter is called and the service operation as soon as it is known. Every
//! returned output is validated here (rule 5/6) before a consumer may use it.
//! No retries, loops, conditions, or parallel steps; a failed step stops the
//! workflow and the remaining steps are skipped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use exposition_graph::OperationRef;
use exposition_graph::workflow::PortSource;
use exposition_graph::workflow::registry::{self, Containment, OperationContract, PortType};

use crate::adapters::{BoundInput, StepControl, StepFailure, StepRequest, StepResult, bare_hash};
use crate::model::{
    WorkflowAttemptState, WorkflowOutputValue, WorkflowPlanStep, WorkflowRejectedOutput, WorkflowRun, WorkflowRunState,
    WorkflowStepAttempt, WorkflowStepOutput, WorkflowStepState,
};
use crate::plan::{hash_project_file, resolve};
use crate::service::Inner;

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
}

fn step_index(run: &WorkflowRun, step: &str) -> usize {
    run.steps.iter().position(|s| s.step == step).expect("plan steps are run steps")
}

/// Execute a persisted run on the current thread until it ends.
pub(crate) fn execute(inner: Arc<Inner>, run: WorkflowRun, root: PathBuf, key: String, token: Arc<CancelToken>) {
    let started = Instant::now();
    let cell = Mutex::new(run);
    let persist = |run: &WorkflowRun, step: &str| inner.persist(&key, run, Some(step));
    let plan_steps: Vec<WorkflowPlanStep> = cell.lock().unwrap_or_else(|p| p.into_inner()).plan.steps.clone();
    let mut outputs: HashMap<(String, String), WorkflowStepOutput> = HashMap::new();
    let mut stop: Option<Stop> = None;

    for planned in &plan_steps {
        // ----- pre-dispatch checks -----
        if token.is_canceled() {
            let (code, message) = token.reason();
            stop = Some(Stop { state: WorkflowRunState::Canceled, code, message });
            break;
        }
        if inner.host.project_root().as_deref() != Some(root.as_path()) {
            stop = Some(Stop {
                state: WorkflowRunState::Canceled,
                code: "workflow.projectClosed".into(),
                message:
                    "The run's project is no longer open; the run was canceled. Nothing resumes when it is reopened."
                        .into(),
            });
            break;
        }
        let Some(contract) = registry::operation(&planned.operation.id, planned.operation.version) else {
            stop =
                Some(Stop::failed("workflow.notWellFormed", format!("“{}” names an unknown operation", planned.label)));
            break;
        };
        let elapsed = started.elapsed().as_secs() as u32;
        let (budget_problem, remaining) = {
            let run = cell.lock().unwrap_or_else(|p| p.into_inner());
            let b = &run.budgets;
            let u = &run.usage;
            let problem = if u.steps >= b.max_steps {
                Some(format!("the run may dispatch at most {} steps", b.max_steps))
            } else if elapsed >= b.max_total_seconds {
                Some(format!("the run's time budget of {} s is used up", b.max_total_seconds))
            } else if u.output_bytes > b.max_output_bytes {
                Some(format!("the run's output budget of {} bytes is used up", b.max_output_bytes))
            } else if planned.containment == Containment::ReadOnlyModel && u.model_requests >= b.max_model_requests {
                Some(format!("the run's budget of {} model requests is used up", b.max_model_requests))
            } else {
                None
            };
            (problem, b.max_total_seconds.saturating_sub(elapsed))
        };
        if let Some(problem) = budget_problem {
            stop = Some(refuse(
                &cell,
                planned,
                "workflow.budgetExhausted",
                format!("Not started: {problem}."),
                &inner,
                &key,
            ));
            break;
        }
        if let Some(missing) = planned.prerequisites.iter().find(|p| {
            let run = cell.lock().unwrap_or_else(|e| e.into_inner());
            run.steps.iter().find(|s| &s.step == *p).is_none_or(|s| s.state != WorkflowStepState::Succeeded)
        }) {
            stop = Some(refuse(
                &cell,
                planned,
                "workflow.prerequisite",
                format!("Not started: its prerequisite “{missing}” did not succeed."),
                &inner,
                &key,
            ));
            break;
        }
        let inputs = {
            let run = cell.lock().unwrap_or_else(|p| p.into_inner());
            bind_inputs(&root, &run, planned, &contract, &outputs)
        };
        let inputs = match inputs {
            Ok(inputs) => inputs,
            Err((code, message)) => {
                stop = Some(refuse(&cell, planned, code, message, &inner, &key));
                break;
            }
        };
        if planned.containment == Containment::NativeAccount && !inner.host.trusted(&root) {
            stop = Some(refuse(
                &cell,
                planned,
                "workflow.untrustedFolder",
                "Not started: the project folder is no longer trusted, and this step runs natively with your account's privileges.",
                &inner,
                &key,
            ));
            break;
        }
        let Some(adapter) = inner.adapters.get(&planned.runner).cloned() else {
            stop = Some(refuse(
                &cell,
                planned,
                "workflow.runnerUnavailable",
                "Not started: no runner is installed.",
                &inner,
                &key,
            ));
            break;
        };
        if let Err(reason) = adapter.availability(&root) {
            let code = if planned.containment == Containment::RestrictedContainer {
                "workflow.restrictedUnavailable"
            } else {
                "workflow.runnerUnavailable"
            };
            stop = Some(refuse(
                &cell,
                planned,
                code,
                format!(
                    "Not started: the runner is no longer available ({reason}); it is never replaced by another runner."
                ),
                &inner,
                &key,
            ));
            break;
        }

        // ----- dispatch intent (persisted before the service is called) -----
        let attempt_no = {
            let mut run = cell.lock().unwrap_or_else(|p| p.into_inner());
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
            if let Err(e) = persist(&run, &planned.step) {
                // Without a durable intent nothing is dispatched.
                let attempt = run.steps[i].attempts.last_mut().expect("just pushed");
                attempt.state = WorkflowAttemptState::Failed;
                attempt.code = Some(e.code.clone());
                attempt.message = Some(e.message.clone());
                run.steps[i].state = WorkflowStepState::Failed;
                stop = Some(Stop::failed(&e.code, e.message));
                break;
            }
            attempt
        };
        let reported: Mutex<Option<OperationRef>> = Mutex::new(None);
        let canceled = || token.is_canceled();
        let on_dispatched = |operation: &OperationRef| {
            *reported.lock().unwrap_or_else(|p| p.into_inner()) = Some(operation.clone());
            let mut run = cell.lock().unwrap_or_else(|p| p.into_inner());
            let i = step_index(&run, &planned.step);
            if let Some(attempt) = run.steps[i].attempts.last_mut()
                && attempt.state == WorkflowAttemptState::Intent
            {
                attempt.state = WorkflowAttemptState::Dispatched;
                attempt.operation = Some(operation.clone());
                attempt.dispatched_at_ms = Some(crate::now_ms());
                if let Err(e) = persist(&run, &planned.step) {
                    tracing::warn!("could not persist a workflow dispatch: {e}");
                }
            }
        };
        let control = StepControl { canceled: &canceled, on_dispatched: &on_dispatched };
        let run_id = cell.lock().unwrap_or_else(|p| p.into_inner()).id.clone();
        let request = StepRequest {
            root: &root,
            run: &run_id,
            step: planned,
            attempt: attempt_no,
            inputs: &inputs,
            time_limit: Duration::from_secs(u64::from(planned.time_limit_seconds.min(remaining).max(1))),
            max_output_bytes: planned.max_output_bytes,
        };
        let result = adapter.run(&request, &control);
        let reported = reported.lock().unwrap_or_else(|p| p.into_inner()).clone();

        // ----- outcome -----
        let mut run = cell.lock().unwrap_or_else(|p| p.into_inner());
        let i = step_index(&run, &planned.step);
        let now = crate::now_ms();
        match result {
            Err(failure) => {
                let was_canceled = token.is_canceled() || failure.code == "workflow.canceled";
                let attempt = run.steps[i].attempts.last_mut().expect("attempt");
                finish_attempt(attempt, &failure, reported.as_ref(), now, was_canceled);
                run.steps[i].state = if was_canceled { WorkflowStepState::Canceled } else { WorkflowStepState::Failed };
                stop = Some(if was_canceled {
                    let (code, message) = token.reason();
                    Stop { state: WorkflowRunState::Canceled, code, message }
                } else {
                    Stop::failed(&failure.code, format!("“{}” failed: {}", planned.label, failure.message))
                });
            }
            Ok(result) => {
                let checked = validate_outputs(&root, &run, planned, &contract, attempt_no, reported.as_ref(), &result);
                let attempt = run.steps[i].attempts.last_mut().expect("attempt");
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
                        run.usage.model_requests += result.model_requests;
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
                        stop = Some(Stop::failed(code, format!("“{}”: {message}", planned.label)));
                    }
                }
            }
        }
        run.usage.seconds = started.elapsed().as_secs() as u32;
        if stop.is_none() && run.usage.output_bytes > run.budgets.max_output_bytes {
            stop = Some(Stop::failed(
                "workflow.budgetExhausted",
                format!("The run's outputs exceed its budget of {} bytes.", run.budgets.max_output_bytes),
            ));
        }
        let _ = persist(&run, &planned.step);
        if stop.is_some() {
            break;
        }
    }

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
    let _ = inner.persist(&key, &run, None);
    inner.finished(&run.id);
}

/// Refuse a step before dispatch: a failed attempt that was never dispatched.
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
    let operation = result.operation.as_ref().or(reported);
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
