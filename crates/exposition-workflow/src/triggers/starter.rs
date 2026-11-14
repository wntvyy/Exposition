//! How the trigger engine plans and starts runs.
//!
//! The engine never builds or starts a run by itself. Every review and check
//! uses a *preview* of the latest revision ([`WorkflowRunService::preview_plan`]:
//! not kept, cannot be started), so triggers never touch the plan cache the
//! user's own reviews live in. Only when a run starts now does it ask for a
//! startable plan, compare its standing hash once more, and start it through
//! the run service's normal approval check
//! ([`WorkflowRunService::start_with_cause`]: the plan is recomputed and
//! refused if anything differs), recording [`WorkflowRunCause::Trigger`].
//! [`RunServiceStarter`] is the real implementation; the tests wrap it.

use std::path::PathBuf;
use std::sync::Arc;

use exposition_graph::{WorkflowId, WorkflowRunId};

use crate::model::{MAX_ACTIVE_RUNS, StartWorkflowRunRequest, WorkflowRunCause, WorkflowRunError, WorkflowRunPlan};
use crate::service::{PlanOptions, WorkflowHost, WorkflowRunService};

/// What the trigger engine needs from the run service.
pub trait TriggerRunStarter: Send + Sync {
    /// Canonical root of the open project, if any.
    fn project_root(&self) -> Option<PathBuf>;
    /// The plan of the latest revision of `workflow` as the run service
    /// would make it now, not kept (it cannot be started): reviews and checks.
    fn preview(&self, workflow: &WorkflowId) -> Result<WorkflowRunPlan, WorkflowRunError>;
    /// A startable plan of the latest revision (the run service keeps it
    /// until [`Self::start`] takes it). Asked only when a run starts now.
    fn plan_to_start(&self, workflow: &WorkflowId) -> Result<WorkflowRunPlan, WorkflowRunError>;
    /// Whether other runs keep a run of `workflow` from starting now (one of
    /// its own, or every run slot taken). Cheap: no plan is made.
    fn busy(&self, workflow: &WorkflowId) -> bool;
    /// Start `plan` (just returned by [`Self::plan_to_start`]) through the
    /// run service's approval check, recording `cause`.
    fn start(&self, plan: &WorkflowRunPlan, cause: WorkflowRunCause) -> Result<WorkflowRunId, WorkflowRunError>;
}

/// The plan blocker / start error that means "other runs hold the slot; try
/// after one ends" (the run service's only such code).
pub fn is_busy(code: &str) -> bool {
    code == "workflow.activeRun"
}

/// The real starter: the application's workflow run service.
pub struct RunServiceStarter {
    pub runs: WorkflowRunService,
    /// The same host the run service uses (for the open project's root).
    pub host: Arc<dyn WorkflowHost>,
}

impl TriggerRunStarter for RunServiceStarter {
    fn project_root(&self) -> Option<PathBuf> {
        self.host.project_root()
    }

    fn preview(&self, workflow: &WorkflowId) -> Result<WorkflowRunPlan, WorkflowRunError> {
        self.runs.preview_plan(workflow, None, &PlanOptions::default())
    }

    fn plan_to_start(&self, workflow: &WorkflowId) -> Result<WorkflowRunPlan, WorkflowRunError> {
        self.runs.plan(workflow, None)
    }

    fn busy(&self, workflow: &WorkflowId) -> bool {
        // A lower configured cap shows as the plan's `workflow.activeRun`.
        self.runs.workflow_active(workflow) || self.runs.active_runs().len() >= MAX_ACTIVE_RUNS
    }

    fn start(&self, plan: &WorkflowRunPlan, cause: WorkflowRunCause) -> Result<WorkflowRunId, WorkflowRunError> {
        let request = StartWorkflowRunRequest { plan: plan.plan.clone(), approval_hash: plan.approval_hash.clone() };
        self.runs.start_with_cause(&request, cause).map(|run| run.id)
    }
}
