//! Workflow run contract (shared with the UI through generated bindings).
//!
//! A run executes one saved, well-formed workflow revision:
//! 1. `workflow_run_plan` resolves the revision's inputs (paths + SHA-256),
//!    runner availability, trust, and budgets into a [`WorkflowRunPlan`] the
//!    user reviews in full (every code/prompt payload, every effect, the
//!    containment of every runner). `blockers` non-empty: it cannot start.
//!    Composite steps are expanded into their pinned children's steps
//!    (`<composite>/<child>`), so the plan is flat and shows everything.
//! 2. `workflow_run_start` takes the plan id and its `approval_hash`; the
//!    backend recomputes the plan and refuses (`workflow.planStale`) if
//!    anything the user approved changed (revision, definition hash, input
//!    hashes, trust, runner settings, project).
//! 3. The runner starts a step once everything it depends on finished, at
//!    most `max_parallel` steps of the run at a time (1: one at a time in plan
//!    order) and within application-wide limits shared by all runs (at most 3
//!    active runs, one per workflow). Before each dispatch it re-checks the
//!    project is current, cancellation, budgets (reserving the step's output
//!    bytes and model requests), the step's input hashes, trust (native
//!    steps), and backend availability (never downgraded). It persists the
//!    attempt's dispatch intent before dispatching and the service operation
//!    id once known.
//! 4. Every returned output is validated by the runner (declared port type,
//!    recomputed SHA-256, producer provenance, service ownership) before a
//!    consumer may use it. A step's own claim of validity is not trusted.
//! 5. After a crash, attempts that were dispatched but not finished are
//!    `interrupted`; the run is `interrupted` until the user acknowledges it.
//!    Nothing is repeated automatically; a new run must be started.
//!
//! Bounded control flow only: a step condition is a fixed predicate over one
//! validated upstream output (`conditionNotMet`, never an expression), a
//! repeat is a bounded forEach (instances) or until (iterations). No
//! retries; triggers start runs only through a standing plan the user
//! approved, recorded as the run's `cause`.

use std::collections::BTreeMap;

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::registry::{Containment, PortType, WorkflowEffect};
use exposition_graph::workflow::{
    AuthorityCeiling, ParamValue, PortSource, StepCondition, StepOperation, StepPayload, StepRepeat,
};
use exposition_graph::{EntityId, EvidenceId, OperationRef, WorkflowId, WorkflowPlanId, WorkflowRunId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// At most this many runs are active at once (application-wide; at most one
/// run of each workflow).
pub const MAX_ACTIVE_RUNS: usize = 3;
/// A run never executes more than this many steps at the same time.
pub const MAX_PARALLEL: u32 = 4;
/// A plan must be started within this long after it was made.
pub const PLAN_TTL_MS: u64 = 30 * 60 * 1000;
/// Bounded text kept per attempt log (stdout/stderr tail, redacted).
pub const MAX_ATTEMPT_LOG_CHARS: usize = 64 * 1024;
/// Bounded report text kept for an agent review step.
pub const MAX_REPORT_CHARS: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRunState {
    Running,
    Succeeded,
    Failed,
    Canceled,
    /// The application stopped while the run was active (or a dispatched
    /// attempt's result is unknown). Needs the user's acknowledgement; never
    /// resumed or replayed.
    Interrupted,
}

impl WorkflowRunState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, WorkflowRunState::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowStepState {
    /// Not reached yet.
    Pending,
    Running,
    Succeeded,
    Failed,
    Canceled,
    /// Not run: an earlier step failed, was canceled, or interrupted, or a
    /// step whose output it needs did not run (`reason` says which).
    Skipped,
    Interrupted,
    /// Not run because its own condition did not hold (`reason` says why).
    ConditionNotMet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowAttemptState {
    /// Dispatch intent persisted; the service was not called yet.
    Intent,
    /// The service accepted the operation (`operation` set).
    Dispatched,
    Succeeded,
    Failed,
    Canceled,
    /// Dispatched (or intended) when the application stopped; outcome unknown.
    Interrupted,
}

/// Aggregate limits of one run (child steps receive what remains).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunBudgets {
    pub max_steps: u32,
    pub max_total_seconds: u32,
    pub max_output_bytes: u64,
    pub max_model_requests: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunUsage {
    pub steps: u32,
    pub seconds: u32,
    pub output_bytes: u64,
    pub model_requests: u32,
}

/// An external input exactly as the run will read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanInput {
    pub id: String,
    pub label: String,
    pub port_type: PortType,
    pub path: WorkspacePath,
    /// SHA-256 (hex) of the content the run will read.
    pub sha256: String,
    pub bytes: u64,
    /// The definition pinned this hash (otherwise: current at plan time).
    pub pinned: bool,
    /// The complete text of a code input, exactly the content `sha256`
    /// names (the user reviews the code a step runs, never a summary).
    pub text: Option<String>,
}

/// A data binding of a planned step's input port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanBinding {
    pub port: String,
    pub from: PortSource,
}

/// One step of the plan, with everything the user approves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanStep {
    pub step: String,
    pub label: String,
    pub operation: StepOperation,
    pub operation_title: String,
    pub runner: String,
    pub runner_title: String,
    pub containment: Containment,
    pub effects: Vec<WorkflowEffect>,
    /// The complete code or prompt the step runs/sends.
    pub payload: Option<StepPayload>,
    pub parameters: BTreeMap<String, ParamValue>,
    pub bindings: Vec<WorkflowPlanBinding>,
    pub prerequisites: Vec<String>,
    pub time_limit_seconds: u32,
    pub max_output_bytes: u64,
    /// Model requests the step may make (reserved from the run's budget
    /// before it is dispatched; 0 for steps that make none).
    #[serde(default)]
    pub max_model_requests: u32,
    /// Titles of the composite steps this step was expanded from, outermost
    /// first (empty for a step of the workflow itself).
    #[serde(default)]
    pub composite_path: Vec<String>,
    /// The step runs only when this holds (evaluated when it becomes ready).
    #[serde(default)]
    pub condition: Option<StepCondition>,
    /// The step runs as several instances (forEach values, until iterations).
    #[serde(default)]
    pub repeat: Option<StepRepeat>,
    /// Whether the runner is usable now (backend configured, toolchain found,
    /// credential present, folder trusted for native steps).
    pub available: bool,
    pub availability: String,
}

/// Application-wide limits on concurrent work, shared by every active run.
/// A step waits (it is not refused) while a limit it falls under is reached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowConcurrencyLimits {
    /// Active runs (at most one run of each workflow).
    pub max_active_runs: u32,
    /// Steps executing at the same time across all runs.
    pub max_in_flight_steps: u32,
    /// Steps in the restricted container.
    pub restricted: u32,
    /// Steps that run natively with the user's account privileges (Lean
    /// audits included).
    pub native: u32,
    /// Read-only agent (model provider) steps.
    pub agent: u32,
    /// Lean audits.
    pub lean: u32,
}

impl Default for WorkflowConcurrencyLimits {
    fn default() -> Self {
        Self {
            max_active_runs: MAX_ACTIVE_RUNS as u32,
            max_in_flight_steps: 4,
            restricted: 2,
            native: 2,
            agent: 2,
            lean: 1,
        }
    }
}

fn one() -> u32 {
    1
}

/// Why a plan cannot start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanBlocker {
    /// Stable code, e.g. `workflow.notWellFormed`, `workflow.runnerUnavailable`,
    /// `workflow.untrustedFolder`, `workflow.restrictedUnavailable`,
    /// `workflow.inputMissing`, `workflow.inputChanged`, `workflow.activeRun`.
    pub code: String,
    pub message: String,
    pub step: Option<String>,
    pub input: Option<String>,
}

/// The execution environment the plan binds to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanExecution {
    /// Restricted backend (`docker`) and its configured image, if any.
    pub restricted_backend: Option<String>,
    pub restricted_image: Option<String>,
    /// The configured image's identity (`sha256:…`) when the plan was made;
    /// restricted steps run only in exactly this image.
    pub restricted_image_id: Option<String>,
    /// The project folder is trusted for native execution.
    pub trusted_folder: bool,
    /// Steps that run natively with the user's account privileges.
    pub native_steps: Vec<String>,
    /// Steps that send material to the model provider.
    pub model_steps: Vec<String>,
    /// Restricted runners with their own image (R and Julia batches): the
    /// image each runs in and its identity, bound exactly like the default
    /// image. `None` when no step uses one (such plans hash as before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runner_images: Option<Vec<WorkflowRunnerImage>>,
}

/// The image of a restricted runner that does not run in the default
/// restricted image (R and Julia batches each have their own).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunnerImage {
    pub runner: String,
    pub image: String,
    /// `sha256:…` when the plan was made; the runner's steps run only in
    /// exactly this image.
    pub image_id: Option<String>,
}

/// A reviewable run plan. Starting it requires its `approval_hash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunPlan {
    pub plan: WorkflowPlanId,
    pub workflow: WorkflowId,
    pub revision: u64,
    pub definition_hash: String,
    pub title: String,
    /// Display name of the project folder the run is bound to.
    pub project: String,
    pub inputs: Vec<WorkflowPlanInput>,
    /// Dependency order. With `max_parallel` 1 the runner executes them one
    /// at a time in this order; otherwise a step starts once everything it
    /// depends on succeeded, ready steps start in this order.
    pub steps: Vec<WorkflowPlanStep>,
    pub ceiling: AuthorityCeiling,
    pub budgets: WorkflowRunBudgets,
    /// Steps of this run that may execute at the same time (1–4; 1 is the
    /// one-step-at-a-time runner).
    #[serde(default = "one")]
    pub max_parallel: u32,
    /// The application-wide limits the run is subject to.
    #[serde(default)]
    pub limits: WorkflowConcurrencyLimits,
    pub execution: WorkflowPlanExecution,
    /// Things the user should know (native privileges, model disclosure, ...).
    pub notices: Vec<String>,
    /// Non-empty: the plan cannot start.
    pub blockers: Vec<WorkflowPlanBlocker>,
    /// SHA-256 over everything above that the user approves.
    pub approval_hash: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartWorkflowRunRequest {
    pub plan: WorkflowPlanId,
    pub approval_hash: String,
}

/// Who produced an output (checked by the runner before a consumer uses it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowOutputProvenance {
    pub run: WorkflowRunId,
    pub step: String,
    pub attempt: u32,
    /// The instance of a repeated step that produced it.
    #[serde(default)]
    pub instance: Option<u32>,
    /// The service operation that produced it (research run, Lean audit, ...).
    pub operation: OperationRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowProposalStatus {
    /// Waiting for the user's review.
    Pending,
    Accepted,
    Rejected,
    /// The review window passed or the run's project closed.
    Expired,
}

/// One declaration of a Lean audit, as the audit returned it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAuditedDeclaration {
    pub name: String,
    /// Exact audit verdict label (Checked, Checked with axioms, ...).
    pub verdict: String,
    pub statement: Option<String>,
    pub axioms: Vec<String>,
}

/// A validated output value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WorkflowOutputValue {
    /// A file in the project (a research run artifact or a declared input).
    File {
        path: WorkspacePath,
        sha256: String,
        bytes: u64,
        media_type: String,
        /// The research run and artifact that hold it, when it is a run output.
        research_run: Option<String>,
        artifact: Option<String>,
    },
    /// A retained research result (graph entity with computational evidence).
    Retained {
        entity: EntityId,
        path: WorkspacePath,
        sha256: String,
        evidence: Option<EvidenceId>,
        /// The research run and artifact it was retained from.
        research_run: Option<String>,
        artifact: Option<String>,
    },
    /// A Lean audit's result. Only the Lean audit adapter constructs this.
    FormalEvidence {
        file: WorkspacePath,
        file_sha256: String,
        /// Overall audit verdict label.
        verdict: String,
        declarations: Vec<WorkflowAuditedDeclaration>,
        /// Evidence records the Lean service registered, if any.
        evidence: Vec<EvidenceId>,
    },
    /// A read-only agent's report.
    Report { text: String, task: Option<String> },
    /// A prepared manuscript insertion awaiting review.
    /// `status` reflects the existing promotion review when the run is read:
    /// accepted once the insertion is in the manuscript, expired after the
    /// review window.
    PromotionProposal {
        promotion: String,
        document: WorkspacePath,
        status: WorkflowProposalStatus,
        /// The attachment the insertion carries (its markers in the manuscript).
        attachment_key: Option<String>,
        /// When the prepared review expires.
        expires_at_ms: Option<u64>,
    },
    /// Evidence associations awaiting review.
    AssociationProposal {
        proposal: String,
        declarations: Vec<String>,
        status: WorkflowProposalStatus,
        /// Declarations the user accepted (evidence recorded for them).
        accepted: Vec<String>,
        /// Evidence records the Lean evidence path registered on acceptance.
        evidence: Vec<EvidenceId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepOutput {
    pub port: String,
    pub port_type: PortType,
    pub value: WorkflowOutputValue,
    pub provenance: WorkflowOutputProvenance,
}

/// An output the runner or adapter refused (escaping link, unsafe name,
/// too large, wrong type, altered after production, ...).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRejectedOutput {
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepAttempt {
    pub attempt: u32,
    pub state: WorkflowAttemptState,
    pub operation: Option<OperationRef>,
    pub intent_at_ms: u64,
    pub dispatched_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    /// Stable failure code, e.g. `workflow.inputChanged`, `research.batchFailed`.
    pub code: Option<String>,
    pub message: Option<String>,
    pub outputs: Vec<WorkflowStepOutput>,
    pub rejected: Vec<WorkflowRejectedOutput>,
    /// Bounded, redacted stdout/stderr tail or service log.
    pub log: Option<String>,
    /// What the attempt ran with (M9 environment capture): runner,
    /// containment, platform, image, interpreter and packages, toolchain,
    /// model, seed. `None` for attempts refused before dispatch and for runs
    /// recorded before environment capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment: Option<crate::environment::WorkflowEnvironment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunStep {
    pub step: String,
    pub label: String,
    pub operation_title: String,
    pub runner_title: String,
    pub containment: Containment,
    pub state: WorkflowStepState,
    /// Attempts of a step that is not repeated.
    pub attempts: Vec<WorkflowStepAttempt>,
    /// Instances of a repeated step (forEach: one per value, listed when the
    /// run starts; until: one per iteration as it starts), in index order.
    #[serde(default)]
    pub instances: Vec<WorkflowStepInstance>,
    /// Why the step did not run or stopped, in plain language (its condition
    /// did not hold, a step it needs did not run, the run stopped before it,
    /// Exposition stopped while it ran).
    #[serde(default)]
    pub reason: Option<String>,
}

/// One instance of a repeated step (each counts against `maxSteps`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStepInstance {
    pub index: u32,
    /// The forEach value of this instance (none for until iterations).
    pub parameter: Option<ParamValue>,
    pub state: WorkflowStepState,
    pub attempts: Vec<WorkflowStepAttempt>,
    /// Why the instance did not run, or why the run moved past it (an until
    /// iteration whose predicate did not hold).
    #[serde(default)]
    pub reason: Option<String>,
}

/// A declared outcome and what realized it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunOutcome {
    pub id: String,
    pub label: String,
    pub optional: bool,
    pub step: String,
    pub port: String,
    pub value: Option<WorkflowOutputValue>,
    /// Why the outcome was not produced (the step failed, was canceled or
    /// skipped, ...); `None` while it is produced or the run is active.
    #[serde(default)]
    pub reason: Option<String>,
}

/// A watched input whose saved content changed (digests are SHA-256 hex;
/// `None`: the input did not exist or its digest was not known).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ChangedInput {
    /// The workflow input id.
    pub input: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Why a run started. Only the run service sets it: the user's manual start
/// through a reviewed plan, an enabled trigger's standing plan (lane C), or a
/// reproduction of an earlier run (lane D). It never grants anything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WorkflowRunCause {
    /// The user started the run from a reviewed plan.
    #[default]
    Manual,
    /// An enabled trigger fired (its id and the watched inputs that changed).
    Trigger { trigger: String, changed: Vec<ChangedInput> },
    /// The user reproduced an earlier run (pinned to its recorded inputs).
    Reproduce { of: WorkflowRunId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: WorkflowRunId,
    /// Why the run started (runs recorded before M9 read as manual).
    #[serde(default)]
    pub cause: WorkflowRunCause,
    pub workflow: WorkflowId,
    pub revision: u64,
    pub definition_hash: String,
    pub title: String,
    pub state: WorkflowRunState,
    pub code: Option<String>,
    pub message: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub budgets: WorkflowRunBudgets,
    pub usage: WorkflowRunUsage,
    pub steps: Vec<WorkflowRunStep>,
    pub outcomes: Vec<WorkflowRunOutcome>,
    /// For an interrupted run: the user has reviewed it (nothing is resumed).
    pub acknowledged: bool,
    /// The plan exactly as approved.
    pub plan: WorkflowRunPlan,
}

impl WorkflowRun {
    /// Say why each declared outcome that was not produced is missing (once
    /// the run ended).
    pub fn explain_outcomes(&mut self) {
        let steps = &self.steps;
        for outcome in &mut self.outcomes {
            if outcome.value.is_some() || outcome.reason.is_some() {
                continue;
            }
            let Some(step) = steps.iter().find(|s| s.step == outcome.step) else { continue };
            let label = &step.label;
            let last = step
                .attempts
                .iter()
                .chain(step.instances.iter().flat_map(|i| i.attempts.iter()))
                .rfind(|a| a.message.is_some())
                .and_then(|a| a.message.as_deref());
            outcome.reason = Some(match step.state {
                WorkflowStepState::Succeeded if !step.instances.is_empty() => format!(
                    "“{label}” ran {} instances; each instance's output is listed with the step.",
                    step.instances.len()
                ),
                WorkflowStepState::Succeeded => format!("“{label}” succeeded without producing this optional output."),
                WorkflowStepState::Failed => match last {
                    Some(message) => format!("“{label}” failed: {message}"),
                    None => format!("“{label}” failed."),
                },
                WorkflowStepState::Canceled => format!("“{label}” was canceled."),
                WorkflowStepState::Interrupted => {
                    format!("“{label}” was interrupted when Exposition stopped; its result is unknown.")
                }
                WorkflowStepState::Skipped | WorkflowStepState::ConditionNotMet => match &step.reason {
                    Some(reason) => format!("“{label}” did not run. {reason}"),
                    None => format!("“{label}” did not run: the run stopped before it could start."),
                },
                WorkflowStepState::Pending | WorkflowStepState::Running => continue,
            });
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunSummary {
    pub id: WorkflowRunId,
    pub workflow: WorkflowId,
    pub revision: u64,
    pub title: String,
    pub state: WorkflowRunState,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub steps_done: u32,
    pub steps_total: u32,
    pub acknowledged: bool,
    /// Why the run started (history shows trigger / reproduction badges).
    #[serde(default)]
    pub cause: WorkflowRunCause,
}

/// Emitted on `workflow-run` whenever a run or one of its steps changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunEvent {
    pub run: WorkflowRunId,
    pub workflow: WorkflowId,
    pub state: WorkflowRunState,
    pub step: Option<String>,
    pub step_state: Option<WorkflowStepState>,
}

/// Review of a run's evidence-association proposal (formal audit preset).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowAssociationDecision {
    pub run: WorkflowRunId,
    pub step: String,
    /// Declarations (from the proposal) to record evidence for; the rest are rejected.
    pub accept: Vec<String>,
}

/// Stable error of the workflow run commands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct WorkflowRunError {
    pub code: String,
    pub message: String,
}

impl WorkflowRunError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into() }
    }
}
