//! How the trigger engine plans and starts runs.
//!
//! The engine never builds or starts a run by itself: it asks the run service
//! for a fresh plan of the latest revision, compares its standing hash with
//! the approved one, and starts that plan through the run service's normal
//! approval check ([`WorkflowRunService::start_with_cause`]: the plan is
//! recomputed once more and refused if anything differs), recording the
//! cause [`WorkflowRunCause::Trigger`]. [`RunServiceStarter`] is the real
//! implementation; the engine's unit tests use fakes.

use std::path::PathBuf;
use std::sync::Arc;

use exposition_graph::{WorkflowId, WorkflowRunId};

use crate::model::{StartWorkflowRunRequest, WorkflowRunCause, WorkflowRunError, WorkflowRunPlan};
use crate::service::{WorkflowHost, WorkflowRunService};

/// What the trigger engine needs from the run service.
pub trait TriggerRunStarter: Send + Sync {
    /// Canonical root of the open project, if any.
    fn project_root(&self) -> Option<PathBuf>;
    /// A fresh reviewable plan of the latest revision of `workflow` in the
    /// open project (the run service keeps it for [`Self::start`]).
    fn plan(&self, workflow: &WorkflowId) -> Result<WorkflowRunPlan, WorkflowRunError>;
    /// Whether a run of `workflow` is active now.
    fn workflow_active(&self, workflow: &WorkflowId) -> bool;
    /// Start `plan` (just returned by [`Self::plan`]) through the run
    /// service's approval check, recording `cause`.
    fn start(&self, plan: &WorkflowRunPlan, cause: WorkflowRunCause) -> Result<WorkflowRunId, WorkflowRunError>;
}

/// Start codes that mean "busy now, try after the active run ends".
pub fn is_busy(code: &str) -> bool {
    matches!(code, "workflow.activeRun" | "workflow.runActive" | "workflow.workflowActive")
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

    fn plan(&self, workflow: &WorkflowId) -> Result<WorkflowRunPlan, WorkflowRunError> {
        self.runs.plan(workflow, None)
    }

    fn workflow_active(&self, workflow: &WorkflowId) -> bool {
        self.runs.workflow_active(workflow)
    }

    fn start(&self, plan: &WorkflowRunPlan, cause: WorkflowRunCause) -> Result<WorkflowRunId, WorkflowRunError> {
        let request = StartWorkflowRunRequest { plan: plan.plan.clone(), approval_hash: plan.approval_hash.clone() };
        self.runs.start_with_cause(&request, cause).map(|run| run.id)
    }
}
