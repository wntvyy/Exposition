//! The workflow run service: plans, start/cancel, durable run records,
//! restart reconciliation, association decisions, and runner availability.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use exposition_graph::workflow::registry::{self, OperationContract};
use exposition_graph::workflow::{WorkflowDefinition, WorkflowValidation};
use exposition_graph::{SavedWorkflow, WorkflowId, WorkflowPlanId, WorkflowRunId};
use tokio::sync::broadcast;

use crate::adapters::Adapters;
use crate::adapters::lean::LeanAuditor;
use crate::model::{
    MAX_ACTIVE_RUNS, StartWorkflowRunRequest, WorkflowAssociationDecision, WorkflowOutputValue, WorkflowProposalStatus,
    WorkflowRun, WorkflowRunError, WorkflowRunEvent, WorkflowRunPlan, WorkflowRunState, WorkflowRunStep,
    WorkflowRunSummary, WorkflowRunUsage, WorkflowStepState,
};
use crate::plan::{self, BuiltPlan, PlanFacts, project_key};
use crate::runner::{self, CancelToken};
use crate::store::{self, RunStore};

/// At most this many unstarted plans are kept.
const MAX_PLANS: usize = 32;
/// Code and message of a run canceled because its project closed.
pub const PROJECT_CLOSED: &str = "workflow.projectClosed";
const PROJECT_CLOSED_MESSAGE: &str =
    "The project was closed or switched, so the run was canceled. Nothing resumes when the project is reopened.";

/// What the service needs from the application.
pub trait WorkflowHost: Send + Sync {
    /// Canonical root of the open project, if any.
    fn project_root(&self) -> Option<PathBuf>;
    /// The folder is trusted for native execution.
    fn trusted(&self, root: &Path) -> bool;
    /// Configured restricted backend (`docker`) and image.
    fn restricted(&self) -> (Option<String>, Option<String>);
    /// A saved revision (latest when `revision` is none) of a workflow of the
    /// project at `root`.
    fn workflow(&self, root: &Path, id: &WorkflowId, revision: Option<u64>) -> Result<SavedWorkflow, WorkflowRunError>;
}

pub struct WorkflowRunConfig {
    /// `<data dir>/workflow-runs.sqlite3`; `None` keeps runs in memory.
    pub database: Option<PathBuf>,
    pub host: Arc<dyn WorkflowHost>,
    /// Runner id → adapter (see [`crate::adapters::runner_ids`]).
    pub adapters: Adapters,
    /// Records evidence for accepted association proposals.
    pub lean: Option<Arc<dyn LeanAuditor>>,
}

struct CachedPlan {
    built: BuiltPlan,
    root: PathBuf,
    requested_revision: Option<u64>,
}

struct ActiveRun {
    root: PathBuf,
    token: Arc<CancelToken>,
    /// The run has steps that execute natively (they need a trusted folder).
    native: bool,
}

pub(crate) struct Inner {
    pub(crate) host: Arc<dyn WorkflowHost>,
    pub(crate) adapters: Adapters,
    lean: Option<Arc<dyn LeanAuditor>>,
    store: Mutex<RunStore>,
    plans: Mutex<HashMap<WorkflowPlanId, CachedPlan>>,
    active: Mutex<HashMap<WorkflowRunId, ActiveRun>>,
    events: broadcast::Sender<WorkflowRunEvent>,
}

impl Inner {
    /// Write a run through and announce the change.
    pub(crate) fn persist(&self, key: &str, run: &WorkflowRun, step: Option<&str>) -> Result<(), WorkflowRunError> {
        let result = self.store.lock().unwrap_or_else(|p| p.into_inner()).put(key, run);
        let step_state = step.and_then(|s| run.steps.iter().find(|x| x.step == s)).map(|s| s.state);
        let _ = self.events.send(WorkflowRunEvent {
            run: run.id.clone(),
            workflow: run.workflow.clone(),
            state: run.state,
            step: step.map(str::to_owned),
            step_state,
        });
        result
    }

    pub(crate) fn finished(&self, run: &WorkflowRunId) {
        self.active.lock().unwrap_or_else(|p| p.into_inner()).remove(run);
    }
}

/// Workflow runs of the application (one instance).
#[derive(Clone)]
pub struct WorkflowRunService {
    inner: Arc<Inner>,
}

fn no_project() -> WorkflowRunError {
    WorkflowRunError::new("workflow.noProject", "Open a project first.")
}

fn unknown_run() -> WorkflowRunError {
    WorkflowRunError::new("workflow.unknownRun", "That workflow run does not exist in this project.")
}

impl WorkflowRunService {
    /// Open the run store and reconcile runs an earlier session left active
    /// (they become interrupted; nothing is resumed or repeated).
    pub fn new(config: WorkflowRunConfig) -> Result<Self, WorkflowRunError> {
        let store = RunStore::open(config.database.as_deref())?;
        let interrupted = store.reconcile(crate::now_ms())?;
        if interrupted > 0 {
            tracing::info!("{interrupted} workflow run(s) were interrupted by the last shutdown");
        }
        let (events, _) = broadcast::channel(256);
        Ok(Self {
            inner: Arc::new(Inner {
                host: config.host,
                adapters: config.adapters,
                lean: config.lean,
                store: Mutex::new(store),
                plans: Mutex::new(HashMap::new()),
                active: Mutex::new(HashMap::new()),
                events,
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorkflowRunEvent> {
        self.inner.events.subscribe()
    }

    fn root(&self) -> Result<PathBuf, WorkflowRunError> {
        self.inner.host.project_root().ok_or_else(no_project)
    }

    /// Whether `runner_id` can run in the open project now (`Err`: why not).
    pub fn runner_available(&self, runner_id: &str) -> Result<(), String> {
        let root = self.root().map_err(|e| e.message)?;
        let trusted = self.inner.host.trusted(&root);
        plan::runner_availability(runner_id, &root, trusted, &self.inner.adapters).map_err(|(_, reason)| reason)
    }

    /// The registered contracts with each runner's availability in the open project.
    pub fn operations(&self) -> Vec<OperationContract> {
        match self.root() {
            Ok(root) => {
                let trusted = self.inner.host.trusted(&root);
                registry::with_availability(&|id| {
                    plan::runner_availability(id, &root, trusted, &self.inner.adapters).map_err(|(_, r)| r)
                })
            }
            Err(e) => registry::with_availability(&|_| Err(e.message.clone())),
        }
    }

    /// Each runner's availability in the open project now (runner id →
    /// `Ok` or why not), computed once for many drafts.
    pub fn availability(&self) -> std::collections::BTreeMap<String, Result<(), String>> {
        self.operations()
            .into_iter()
            .map(|o| {
                let state = if o.runner.available { Ok(()) } else { Err(o.runner.availability) };
                (o.runner.id, state)
            })
            .collect()
    }

    /// Runnable = well-formed AND every step's runner available now.
    pub fn apply_runnability(&self, validation: &mut WorkflowValidation, definition: &WorkflowDefinition) {
        match self.root() {
            Ok(root) => {
                let trusted = self.inner.host.trusted(&root);
                exposition_graph::workflow::apply_runnability(validation, definition, &|id| {
                    plan::runner_availability(id, &root, trusted, &self.inner.adapters).map_err(|(_, r)| r)
                });
            }
            Err(e) => {
                let message = e.message;
                exposition_graph::workflow::apply_runnability(validation, definition, &|_| Err(message.clone()))
            }
        }
    }

    fn active_run(&self) -> bool {
        self.inner.active.lock().unwrap_or_else(|p| p.into_inner()).len() >= MAX_ACTIVE_RUNS
    }

    fn build(&self, root: &Path, saved: &SavedWorkflow) -> BuiltPlan {
        let (restricted_backend, restricted_image) = self.inner.host.restricted();
        let facts = PlanFacts {
            root,
            trusted: self.inner.host.trusted(root),
            restricted_backend,
            restricted_image,
            adapters: &self.inner.adapters,
            active_run: self.active_run(),
        };
        plan::build_plan(saved, &facts, crate::now_ms())
    }

    /// A reviewable plan of a saved revision (latest when `revision` is none).
    pub fn plan(&self, id: &WorkflowId, revision: Option<u64>) -> Result<WorkflowRunPlan, WorkflowRunError> {
        let root = self.root()?;
        let saved = self.inner.host.workflow(&root, id, revision)?;
        let built = self.build(&root, &saved);
        let plan = built.plan.clone();
        let now = crate::now_ms();
        let mut plans = self.inner.plans.lock().unwrap_or_else(|p| p.into_inner());
        plans.retain(|_, p| p.built.plan.expires_at_ms > now);
        if plans.len() >= MAX_PLANS
            && let Some(oldest) = plans.iter().min_by_key(|(_, p)| p.built.plan.created_at_ms).map(|(k, _)| k.clone())
        {
            plans.remove(&oldest);
        }
        plans.insert(plan.plan.clone(), CachedPlan { built, root, requested_revision: revision });
        Ok(plan)
    }

    /// Start a run of an approved plan (recomputed now; refused if anything
    /// the user approved changed or a blocker appeared).
    pub fn start(&self, request: &StartWorkflowRunRequest) -> Result<WorkflowRun, WorkflowRunError> {
        let stale = |m: String| WorkflowRunError::new("workflow.planStale", m);
        let cached = self.inner.plans.lock().unwrap_or_else(|p| p.into_inner()).remove(&request.plan);
        let Some(cached) = cached else {
            return Err(stale("This plan is unknown or was already used; review a new plan.".into()));
        };
        let approved = &cached.built.plan;
        if approved.expires_at_ms <= crate::now_ms() {
            return Err(stale("This plan expired; review a new plan.".into()));
        }
        if request.approval_hash != approved.approval_hash {
            return Err(stale("The approval does not match the plan that was reviewed; review it again.".into()));
        }
        let root = self.root()?;
        if root != cached.root {
            return Err(stale("The open project changed since the plan was made; review a new plan.".into()));
        }
        let saved = self.inner.host.workflow(&root, &approved.workflow, cached.requested_revision)?;
        let fresh = self.build(&root, &saved);
        if fresh.plan.approval_hash != approved.approval_hash {
            return Err(stale(plan::describe_changes(approved, &fresh.plan)));
        }
        if let Some(blocker) = fresh.plan.blockers.first() {
            return Err(WorkflowRunError::new(&blocker.code, blocker.message.clone()));
        }
        let key = project_key(&root);
        let token = Arc::new(CancelToken::default());
        let run = {
            let mut active = self.inner.active.lock().unwrap_or_else(|p| p.into_inner());
            if active.len() >= MAX_ACTIVE_RUNS {
                return Err(WorkflowRunError::new(
                    "workflow.activeRun",
                    "Another workflow run is active; wait for it to finish or cancel it first.",
                ));
            }
            // The run carries the plan exactly as approved.
            let approved = cached.built.plan;
            let run = WorkflowRun {
                id: WorkflowRunId::mint(),
                workflow: approved.workflow.clone(),
                revision: approved.revision,
                definition_hash: approved.definition_hash.clone(),
                title: approved.title.clone(),
                state: WorkflowRunState::Running,
                code: None,
                message: None,
                started_at_ms: crate::now_ms(),
                finished_at_ms: None,
                budgets: approved.budgets.clone(),
                usage: WorkflowRunUsage::default(),
                steps: approved
                    .steps
                    .iter()
                    .map(|s| WorkflowRunStep {
                        step: s.step.clone(),
                        label: s.label.clone(),
                        operation_title: s.operation_title.clone(),
                        runner_title: s.runner_title.clone(),
                        containment: s.containment,
                        state: WorkflowStepState::Pending,
                        attempts: Vec::new(),
                    })
                    .collect(),
                outcomes: cached.built.outcomes,
                acknowledged: false,
                plan: approved,
            };
            self.inner.persist(&key, &run, None)?;
            active.insert(
                run.id.clone(),
                ActiveRun {
                    root: root.clone(),
                    token: Arc::clone(&token),
                    native: !run.plan.execution.native_steps.is_empty(),
                },
            );
            run
        };
        let inner = Arc::clone(&self.inner);
        let first = run.clone();
        let spawned = std::thread::Builder::new().name("workflow-run".into()).spawn(move || {
            runner::execute(inner, run, root, key, token);
        });
        if let Err(e) = spawned {
            self.inner.finished(&first.id);
            let mut failed = first;
            failed.state = WorkflowRunState::Failed;
            failed.code = Some("workflow.internal".into());
            failed.message = Some(format!("The run could not start: {e}"));
            failed.finished_at_ms = Some(crate::now_ms());
            for step in &mut failed.steps {
                step.state = WorkflowStepState::Skipped;
            }
            let _ = self.inner.persist(&project_key(&self.root()?), &failed, None);
            return Ok(failed);
        }
        Ok(first)
    }

    fn load(&self, id: &WorkflowRunId) -> Result<(String, WorkflowRun), WorkflowRunError> {
        let root = self.root()?;
        let found = self.inner.store.lock().unwrap_or_else(|p| p.into_inner()).get(id)?;
        match found {
            Some((project, run)) if project == project_key(&root) => Ok((project, run)),
            _ => Err(unknown_run()),
        }
    }

    pub fn get(&self, id: &WorkflowRunId) -> Result<WorkflowRun, WorkflowRunError> {
        self.load(id).map(|(_, run)| run)
    }

    /// Runs of the open project, newest first (bounded).
    pub fn runs(&self, workflow: Option<&WorkflowId>) -> Result<Vec<WorkflowRunSummary>, WorkflowRunError> {
        let root = self.root()?;
        self.inner.store.lock().unwrap_or_else(|p| p.into_inner()).list(&project_key(&root), workflow, 100)
    }

    /// Cancel a run: its current step's job is canceled through its service;
    /// later steps are skipped. Waits briefly for the run to stop.
    pub fn cancel(&self, id: &WorkflowRunId) -> Result<WorkflowRun, WorkflowRunError> {
        let (key, run) = self.load(id)?;
        let token = self.inner.active.lock().unwrap_or_else(|p| p.into_inner()).get(id).map(|a| Arc::clone(&a.token));
        match token {
            Some(token) => {
                token.cancel("workflow.canceled", "You canceled the run.");
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline
                    && self.inner.active.lock().unwrap_or_else(|p| p.into_inner()).contains_key(id)
                {
                    std::thread::sleep(Duration::from_millis(25));
                }
                self.get(id)
            }
            None if run.state == WorkflowRunState::Running => {
                // No runner holds it (should not happen after reconciliation).
                let mut run = run;
                store::interrupt(&mut run, crate::now_ms());
                self.inner.persist(&key, &run, None)?;
                Ok(run)
            }
            None => Ok(run),
        }
    }

    /// The project at `root` closed or was switched (`None`: whatever is
    /// open): cancel its active runs. Nothing resumes on reopen.
    pub fn project_closed(&self, root: Option<&Path>) {
        let active = self.inner.active.lock().unwrap_or_else(|p| p.into_inner());
        for run in active.values() {
            if root.is_none_or(|r| project_key(r) == project_key(&run.root)) {
                run.token.cancel(PROJECT_CLOSED, PROJECT_CLOSED_MESSAGE);
            }
        }
    }

    /// Trust was revoked for `revoked` (and everything inside it): cancel
    /// active runs there that execute natively. Their current native job is
    /// stopped by the execution service itself; nothing later is dispatched.
    pub fn trust_revoked(&self, revoked: &Path) {
        let revoked = std::fs::canonicalize(revoked).unwrap_or_else(|_| revoked.to_path_buf());
        let revoked = PathBuf::from(project_key(&revoked));
        let active = self.inner.active.lock().unwrap_or_else(|p| p.into_inner());
        for run in active.values() {
            if run.native && PathBuf::from(project_key(&run.root)).starts_with(&revoked) {
                run.token.cancel(
                    "workflow.untrustedFolder",
                    "Trust was revoked for the project folder, so the run was canceled: its native steps run with your account's privileges and need a trusted folder.",
                );
            }
        }
    }

    /// Mark an interrupted run as reviewed. Nothing is resumed or repeated.
    pub fn acknowledge(&self, id: &WorkflowRunId) -> Result<WorkflowRun, WorkflowRunError> {
        let (key, mut run) = self.load(id)?;
        if run.state != WorkflowRunState::Interrupted {
            return Err(WorkflowRunError::new(
                "workflow.notInterrupted",
                "Only an interrupted run needs acknowledging.",
            ));
        }
        run.acknowledged = true;
        self.inner.persist(&key, &run, None)?;
        Ok(run)
    }

    /// Accept (record evidence for) some declarations of a run's association
    /// proposal and reject the rest. Only declarations in the proposal — which
    /// lists only audited declarations — are accepted.
    pub fn decide_association(&self, decision: &WorkflowAssociationDecision) -> Result<WorkflowRun, WorkflowRunError> {
        if decision.accept.len() > crate::adapters::associate::MAX_PROPOSED {
            return Err(WorkflowRunError::new("workflow.tooLarge", "Too many declarations."));
        }
        let (key, mut run) = self.load(&decision.run)?;
        if self.inner.active.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&decision.run) {
            return Err(WorkflowRunError::new("workflow.runActive", "Wait for the run to finish first."));
        }
        let Some(step_index) = run.steps.iter().position(|s| s.step == decision.step) else {
            return Err(WorkflowRunError::new("workflow.unknownStep", "That step is not part of the run."));
        };
        let Some(planned) = run.plan.steps.iter().find(|s| s.step == decision.step).cloned() else {
            return Err(WorkflowRunError::new("workflow.unknownStep", "That step is not part of the run."));
        };
        // The audit result the proposal was made from.
        let audit = planned.bindings.iter().find_map(|b| match &b.from {
            exposition_graph::workflow::PortSource::Step { step, port } => run
                .steps
                .iter()
                .find(|s| &s.step == step)
                .and_then(|s| s.attempts.last())
                .and_then(|a| a.outputs.iter().find(|o| &o.port == port))
                .map(|o| o.value.clone()),
            _ => None,
        });
        let Some(WorkflowOutputValue::FormalEvidence { file, file_sha256, declarations: audited, .. }) = audit else {
            return Err(WorkflowRunError::new("workflow.noProposal", "The step has no audit result to decide on."));
        };
        let step = &mut run.steps[step_index];
        let Some(attempt) = step.attempts.last_mut() else {
            return Err(WorkflowRunError::new("workflow.noProposal", "The step produced no proposal."));
        };
        let Some(output) =
            attempt.outputs.iter_mut().find(|o| matches!(o.value, WorkflowOutputValue::AssociationProposal { .. }))
        else {
            return Err(WorkflowRunError::new("workflow.noProposal", "The step produced no association proposal."));
        };
        let WorkflowOutputValue::AssociationProposal { declarations, status, accepted, evidence, .. } =
            &mut output.value
        else {
            unreachable!("matched above");
        };
        if *status != WorkflowProposalStatus::Pending {
            return Err(WorkflowRunError::new("workflow.decided", "This proposal was already decided."));
        }
        let mut accept: Vec<String> = Vec::new();
        for name in &decision.accept {
            let name = name.trim();
            if !declarations.iter().any(|d| d == name) || !audited.iter().any(|d| d.name == name) {
                return Err(WorkflowRunError::new(
                    "workflow.declarationNotAudited",
                    format!("“{name}” is not one of the audited declarations this proposal lists."),
                ));
            }
            if !accept.iter().any(|a| a == name) {
                accept.push(name.to_owned());
            }
        }
        if !accept.is_empty() {
            let lean = self.inner.lean.as_ref().ok_or_else(|| {
                WorkflowRunError::new("workflow.runnerUnavailable", "The Lean verification service is not available.")
            })?;
            *evidence = lean.record_evidence(&file, &file_sha256, &accept)?;
        }
        *status = if accept.is_empty() { WorkflowProposalStatus::Rejected } else { WorkflowProposalStatus::Accepted };
        *accepted = accept;
        let value = output.value.clone();
        let port = output.port.clone();
        for outcome in &mut run.outcomes {
            if outcome.step == decision.step && outcome.port == port {
                outcome.value = Some(value.clone());
            }
        }
        self.inner.persist(&key, &run, Some(&decision.step))?;
        Ok(run)
    }

    /// Ids of active runs (for diagnostics and tests).
    pub fn active_runs(&self) -> Vec<WorkflowRunId> {
        self.inner.active.lock().unwrap_or_else(|p| p.into_inner()).keys().cloned().collect()
    }

    /// Wait until no run is active (tests, shutdown).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.inner.active.lock().unwrap_or_else(|p| p.into_inner()).is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}
