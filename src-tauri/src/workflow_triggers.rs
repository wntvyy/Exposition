//! Workflow trigger commands (M9): review a standing plan, enable, disable,
//! list, and the `workflow-trigger` event, plus the wiring that feeds the
//! trigger engine (`exposition-workflow::triggers`).
//!
//! Triggers are user-only: these commands are reachable from the webview
//! only, no agent tool creates or enables one, and the records live in
//! `<data dir>/workflow-triggers.json` (never in a project, definition, graph,
//! or bundle). The engine listens to saved-file changes of the open project
//! only after the project finished opening and indexing (the project graph's
//! first synchronization); closing the project stops it, and trust
//! revocation suspends the folder's triggers. Triggered runs start through
//! the run service's normal approval check with their cause recorded.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_graph::{GraphEvent, GraphService, SyncState, WorkflowId};
use exposition_workflow::triggers::{
    EnableWorkflowTriggerRequest, RunServiceStarter, SystemClock, TriggerConfig, TriggerService, WorkflowTrigger,
    WorkflowTriggerId, WorkflowTriggerPlan, WorkflowTriggerPlanRequest, store,
};
use exposition_workflow::{WorkflowHost, WorkflowRunError, WorkflowRunService};
use exposition_workspace::types::WorkspaceEvent;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::CommandError;
use crate::state::AppState;

/// Event carrying every [`exposition_workflow::triggers::WorkflowTriggerEvent`].
pub const WORKFLOW_TRIGGER_EVENT: &str = "workflow-trigger";

/// The trigger service over the application's run service. The store lives
/// in the application data directory.
pub fn build(data_dir: &Path, runs: WorkflowRunService, host: Arc<dyn WorkflowHost>) -> TriggerService {
    TriggerService::new(TriggerConfig {
        store: Some(data_dir.join(store::STORE_FILE)),
        starter: Arc::new(RunServiceStarter { runs, host }),
        clock: Arc::new(SystemClock),
    })
}

async fn blocking<T, F>(state: &State<'_, AppState>, f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&TriggerService) -> Result<T, WorkflowRunError> + Send + 'static,
{
    let service = state.workflow_triggers.clone();
    tauri::async_runtime::spawn_blocking(move || f(&service))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Workflow trigger task failed: {e}")))?
        .map_err(CommandError::from)
}

/// Review a standing plan for a trigger of the latest revision: the complete
/// run plan, the standing hash `workflow_trigger_enable` requires, exactly
/// what may change without asking, and what suspends the trigger.
#[tauri::command]
pub async fn workflow_trigger_plan(
    state: State<'_, AppState>,
    request: WorkflowTriggerPlanRequest,
) -> Result<WorkflowTriggerPlan, CommandError> {
    blocking(&state, move |s| s.plan(&request)).await
}

/// Enable a trigger from a reviewed standing plan (refused with
/// `trigger.planStale` if the plan changed since it was reviewed).
#[tauri::command]
pub async fn workflow_trigger_enable(
    state: State<'_, AppState>,
    request: EnableWorkflowTriggerRequest,
) -> Result<WorkflowTrigger, CommandError> {
    blocking(&state, move |s| s.enable(&request)).await
}

#[tauri::command]
pub async fn workflow_trigger_disable(
    state: State<'_, AppState>,
    trigger: WorkflowTriggerId,
) -> Result<WorkflowTrigger, CommandError> {
    blocking(&state, move |s| s.disable(&trigger)).await
}

/// Triggers of the open project (of one workflow when given).
#[tauri::command]
pub async fn workflow_trigger_list(
    state: State<'_, AppState>,
    workflow: Option<WorkflowId>,
) -> Result<Vec<WorkflowTrigger>, CommandError> {
    blocking(&state, move |s| s.list(workflow.as_ref())).await
}

fn canonical(root: &str) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root))
}

/// The graph finished synchronizing the project it is attached to.
fn indexed(graph: &GraphService, triggers: &TriggerService) {
    if graph.freshness().state == SyncState::Synchronized
        && let Some(root) = graph.root()
    {
        triggers.project_indexed(&root);
    }
}

/// Start the engine's driver and feed it: project open/close and saved-file
/// changes (workspace events), indexing finished (graph events), trust
/// revocation, and run ends (to release a queued follow-up promptly). Trigger
/// changes are forwarded to the webview.
pub fn start(app: AppHandle) {
    let (triggers, mut workspace, graph, mut trust, mut runs) = {
        let state = app.state::<AppState>();
        (
            state.workflow_triggers.clone(),
            state.workspace.subscribe(),
            Arc::clone(&state.graph),
            state.execution.subscribe_trust(),
            state.workflow_runs.subscribe(),
        )
    };
    if let Err(e) = triggers.spawn_driver() {
        eprintln!("the workflow trigger engine could not start: {e}");
    }

    let mut graph_events = graph.subscribe();
    let g = Arc::clone(&graph);
    let t = triggers.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match workspace.recv().await {
                Ok(WorkspaceEvent::Opened { info }) => {
                    t.project_opening(&canonical(&info.root));
                    // Indexing may already be complete (e.g. the same graph).
                    indexed(&g, &t);
                }
                Ok(WorkspaceEvent::Closed) => t.project_closed(),
                Ok(WorkspaceEvent::FilesChanged { changes, rescan }) => {
                    let paths: Vec<_> = changes.into_iter().map(|c| c.path).collect();
                    t.files_changed(&paths, rescan);
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => t.files_changed(&[], true),
                Err(RecvError::Closed) => break,
            }
        }
    });

    let t = triggers.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match graph_events.recv().await {
                Ok(GraphEvent::Reconciled { freshness, .. }) if freshness.state == SyncState::Synchronized => {
                    indexed(&graph, &t)
                }
                Ok(GraphEvent::Status { .. }) | Err(RecvError::Lagged(_)) => indexed(&graph, &t),
                Ok(_) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });

    let t = triggers.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match trust.recv().await {
                Ok(change) if !change.granted => t.trust_revoked(Path::new(&change.root)),
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    let t = triggers.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match runs.recv().await {
                Ok(event) if event.state.is_terminal() => t.runs_changed(),
                Ok(_) | Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    let mut events = triggers.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = app.emit(WORKFLOW_TRIGGER_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}
