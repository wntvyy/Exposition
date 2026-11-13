//! Workflow run commands (M8): reviewable run plans, manual start/cancel,
//! run records, acknowledgement of interrupted runs, and evidence-association
//! decisions, plus the `workflow-run` event.
//!
//! The run service (`exposition-workflow`) enforces every rule: plans are
//! recomputed and compared with the approved hash at start, one step runs at
//! a time with budgets, outputs are validated before consumers see them, and
//! runs interrupted by a shutdown are never resumed. Closing or switching the
//! project cancels its active run (src/workspace.rs).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_execution::ExecutionService;
use exposition_graph::{GraphService, SavedWorkflow, WorkflowId, WorkflowRunId};
use exposition_lean::LeanService;
use exposition_research::ResearchProject;
use exposition_research::batch::{BatchMode, BatchRunner};
use exposition_workflow::adapters::agent::{AgentReviewAdapter, AgentReviewer, AgentServiceReviewer};
use exposition_workflow::adapters::associate::AssociateAdapter;
use exposition_workflow::adapters::batch::BatchAdapter;
use exposition_workflow::adapters::lean::{LeanAuditAdapter, LeanAuditor, LeanServiceAuditor};
use exposition_workflow::adapters::research::{ProjectResearch, PromotionAdapter, ResearchActions, RetainAdapter};
use exposition_workflow::adapters::{Adapters, runner_ids};
use exposition_workflow::{
    StartWorkflowRunRequest, WorkflowAssociationDecision, WorkflowHost, WorkflowRun, WorkflowRunConfig,
    WorkflowRunError, WorkflowRunPlan, WorkflowRunService, WorkflowRunSummary,
};
use exposition_workspace::{DocumentService, WorkspaceService};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::CommandError;
use crate::state::AppState;

/// Event carrying every [`exposition_workflow::WorkflowRunEvent`].
pub const WORKFLOW_RUN_EVENT: &str = "workflow-run";

impl From<WorkflowRunError> for CommandError {
    fn from(e: WorkflowRunError) -> Self {
        CommandError::new(e.code, e.message)
    }
}

/// The application as the run service sees it.
pub struct AppWorkflowHost {
    pub workspace: Arc<WorkspaceService>,
    pub graph: Arc<GraphService>,
    pub execution: ExecutionService,
}

impl WorkflowHost for AppWorkflowHost {
    fn project_root(&self) -> Option<PathBuf> {
        let root = self.workspace.open_root()?;
        Some(std::fs::canonicalize(&root).unwrap_or(root))
    }

    fn trusted(&self, root: &Path) -> bool {
        self.execution.trust_status(&root.to_string_lossy()).is_ok_and(|s| s.trusted)
    }

    fn restricted(&self) -> (Option<String>, Option<String>) {
        (Some("docker".to_owned()), self.execution.settings().docker.image)
    }

    fn restricted_image_id(&self) -> Option<String> {
        // The engine's identity of the configured image (never pulls).
        self.execution.restricted_preflight().ok().and_then(|t| t.image_id)
    }

    fn workflow(&self, root: &Path, id: &WorkflowId, revision: Option<u64>) -> Result<SavedWorkflow, WorkflowRunError> {
        if self.graph.root().as_deref() != Some(root) {
            return Err(WorkflowRunError::new(
                "workflow.graphUnavailable",
                "The project graph is not available for this project; workflows cannot be planned.",
            ));
        }
        self.graph.workflow(id, revision).map_err(|e| WorkflowRunError::new(&e.code, e.message))
    }
}

/// Services the step adapters call.
pub struct RunServices {
    pub workspace: Arc<WorkspaceService>,
    pub documents: Arc<DocumentService>,
    pub graph: Arc<GraphService>,
    pub execution: ExecutionService,
    pub lean: LeanService,
    pub research_project: Arc<ResearchProject>,
    /// Read-only agent review steps run as agent tasks.
    pub agents: exposition_agents::AgentService,
    /// The research batch runner (restricted Docker and trusted-native Python
    /// batches). `None`: Python steps report themselves unavailable.
    pub batch: Option<Arc<dyn BatchRunner>>,
    pub runtime: tokio::runtime::Handle,
}

/// The run service with every adapter wired to the application's services.
pub fn build(data_dir: &Path, services: RunServices) -> Result<WorkflowRunService, WorkflowRunError> {
    let auditor: Arc<dyn LeanAuditor> = Arc::new(LeanServiceAuditor {
        lean: services.lean.clone(),
        workspace: Arc::clone(&services.workspace),
        documents: Arc::clone(&services.documents),
        graph: Arc::clone(&services.graph),
        runtime: services.runtime.clone(),
    });
    let research: Arc<dyn ResearchActions> = Arc::new(ProjectResearch {
        project: Arc::clone(&services.research_project),
        workspace: Arc::clone(&services.workspace),
    });
    let mut adapters: Adapters = Adapters::new();
    adapters.insert(
        runner_ids::RESTRICTED_PYTHON.into(),
        Arc::new(BatchAdapter::new(services.batch.clone(), BatchMode::Restricted)),
    );
    adapters.insert(
        runner_ids::NATIVE_PYTHON.into(),
        Arc::new(BatchAdapter::new(services.batch.clone(), BatchMode::TrustedNative)),
    );
    adapters.insert(runner_ids::LEAN_AUDIT.into(), Arc::new(LeanAuditAdapter { auditor: Some(Arc::clone(&auditor)) }));
    adapters.insert(runner_ids::GRAPH_PROPOSAL.into(), Arc::new(AssociateAdapter));
    adapters.insert(
        runner_ids::RESEARCH_RETENTION.into(),
        Arc::new(RetainAdapter { research: Some(Arc::clone(&research)) }),
    );
    adapters.insert(
        runner_ids::REVIEWED_PROMOTION.into(),
        Arc::new(PromotionAdapter { research: Some(Arc::clone(&research)) }),
    );
    let reviewer: Arc<dyn AgentReviewer> = Arc::new(AgentServiceReviewer { agents: services.agents.clone() });
    adapters.insert(runner_ids::READ_ONLY_AGENT.into(), Arc::new(AgentReviewAdapter { reviewer: Some(reviewer) }));
    WorkflowRunService::new(WorkflowRunConfig {
        database: Some(data_dir.join("workflow-runs.sqlite3")),
        host: Arc::new(AppWorkflowHost {
            workspace: services.workspace,
            graph: services.graph,
            execution: services.execution,
        }),
        adapters,
        lean: Some(auditor),
        research: Some(research),
    })
}

async fn blocking<T, F>(state: &State<'_, AppState>, f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&WorkflowRunService) -> Result<T, WorkflowRunError> + Send + 'static,
{
    let service = state.workflow_runs.clone();
    tauri::async_runtime::spawn_blocking(move || f(&service))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Workflow run task failed: {e}")))?
        .map_err(CommandError::from)
}

/// A reviewable plan of a saved revision (latest when `revision` is none):
/// resolved inputs with SHA-256, every step's full payload, runner,
/// containment, effects and limits, budgets, notices, blockers, and the
/// approval hash `workflow_run_start` requires.
#[tauri::command]
pub async fn workflow_run_plan(
    state: State<'_, AppState>,
    id: WorkflowId,
    revision: Option<u64>,
) -> Result<WorkflowRunPlan, CommandError> {
    blocking(&state, move |s| s.plan(&id, revision)).await
}

/// Start a run of an approved plan (refused with `workflow.planStale` if
/// anything approved changed, or with the first blocker's code).
#[tauri::command]
pub async fn workflow_run_start(
    state: State<'_, AppState>,
    request: StartWorkflowRunRequest,
) -> Result<WorkflowRun, CommandError> {
    blocking(&state, move |s| s.start(&request)).await
}

#[tauri::command]
pub async fn workflow_run_cancel(state: State<'_, AppState>, run: WorkflowRunId) -> Result<WorkflowRun, CommandError> {
    blocking(&state, move |s| s.cancel(&run)).await
}

#[tauri::command]
pub async fn workflow_run_get(state: State<'_, AppState>, run: WorkflowRunId) -> Result<WorkflowRun, CommandError> {
    blocking(&state, move |s| s.get(&run)).await
}

/// Runs of the open project, newest first (bounded).
#[tauri::command]
pub async fn workflow_runs(
    state: State<'_, AppState>,
    workflow: Option<WorkflowId>,
) -> Result<Vec<WorkflowRunSummary>, CommandError> {
    blocking(&state, move |s| s.runs(workflow.as_ref())).await
}

/// Mark an interrupted run as reviewed (nothing is resumed or repeated).
#[tauri::command]
pub async fn workflow_run_acknowledge(
    state: State<'_, AppState>,
    run: WorkflowRunId,
) -> Result<WorkflowRun, CommandError> {
    blocking(&state, move |s| s.acknowledge(&run)).await
}

/// Accept some declarations of a run's association proposal (their evidence
/// is recorded through the Lean evidence path) and reject the rest.
#[tauri::command]
pub async fn workflow_association_decide(
    state: State<'_, AppState>,
    decision: WorkflowAssociationDecision,
) -> Result<WorkflowRun, CommandError> {
    blocking(&state, move |s| s.decide_association(&decision)).await
}

/// Forward run and step changes to the webview, and cancel native runs in a
/// folder whose trust is revoked.
pub fn forward_events(app: AppHandle) {
    let mut trust = app.state::<AppState>().execution.subscribe_trust();
    let runs = app.state::<AppState>().workflow_runs.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match trust.recv().await {
                Ok(change) if !change.granted => runs.trust_revoked(Path::new(&change.root)),
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
    let mut rx = app.state::<AppState>().workflow_runs.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(WORKFLOW_RUN_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}
