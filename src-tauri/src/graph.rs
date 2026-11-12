//! Project graph commands and event forwarding.
//!
//! The webview reads bounded graph projections and proposes typed changes;
//! every change is validated in Rust (`exposition-graph`), applied as one
//! graph transaction, and attributed to the user. No SQL, no service-owned
//! records (evidence, observed relations), and no execution are reachable
//! from here. Saved views are presentation only.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use exposition_graph::reconcile::ReconcileReport;
use exposition_graph::{
    ApplyChangeRequest, ApplyOutcome, BackupInfo, ChangeProposal, ChangeRequest, EntityDetail, EntityId, GraphError,
    GraphQuery, GraphService, GraphSnapshot, GraphStatus, GraphView, GraphViewInput, GraphViewSummary, ImpactReport,
    ImpactRequest, InspectAtSourceRequest, MathOverview, OperationRecord, Origin, RecoveryOption, SourceInspection,
    ViewId, Vocabulary,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::CommandError;
use crate::state::AppState;

/// Every [`GraphEvent`], in order.
pub const GRAPH_EVENT: &str = "graph:event";
/// Emitted if the frontend fell behind and must re-read graph state.
pub const GRAPH_RESYNC: &str = "graph:resync";

impl From<GraphError> for CommandError {
    fn from(e: GraphError) -> Self {
        CommandError::new(e.code, e.message)
    }
}

/// Run a blocking graph operation off the async executor.
async fn blocking<T, F>(state: &State<'_, AppState>, f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&GraphService) -> Result<T, GraphError> + Send + 'static,
{
    let service = Arc::clone(&state.graph);
    tauri::async_runtime::spawn_blocking(move || f(&service))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Graph task failed: {e}")))?
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn graph_status(state: State<'_, AppState>) -> Result<GraphStatus, CommandError> {
    blocking(&state, |g| Ok(g.status())).await
}

#[tauri::command]
pub async fn graph_vocabulary() -> Result<Vocabulary, CommandError> {
    Ok(exposition_graph::vocabulary())
}

#[tauri::command]
pub async fn graph_snapshot(state: State<'_, AppState>, query: GraphQuery) -> Result<GraphSnapshot, CommandError> {
    blocking(&state, move |g| g.snapshot(&query)).await
}

#[tauri::command]
pub async fn graph_entity(state: State<'_, AppState>, entity: EntityId) -> Result<EntityDetail, CommandError> {
    blocking(&state, move |g| g.entity_detail(&entity)).await
}

#[tauri::command]
pub async fn graph_inspect_at_source(
    state: State<'_, AppState>,
    request: InspectAtSourceRequest,
) -> Result<SourceInspection, CommandError> {
    blocking(&state, move |g| g.inspect_at_source(&request)).await
}

#[tauri::command]
pub async fn graph_impact(state: State<'_, AppState>, request: ImpactRequest) -> Result<ImpactReport, CommandError> {
    blocking(&state, move |g| g.impact(&request)).await
}

#[tauri::command]
pub async fn graph_math_overview(state: State<'_, AppState>) -> Result<MathOverview, CommandError> {
    blocking(&state, |g| g.math_overview()).await
}

/// Validate a change requested from the workbench (origin: the user).
#[tauri::command]
pub async fn graph_propose_change(
    state: State<'_, AppState>,
    request: ChangeRequest,
) -> Result<ChangeProposal, CommandError> {
    blocking(&state, move |g| g.propose_change(Origin::User, &request)).await
}

/// Apply a change the user proposed (re-validated; all or nothing).
#[tauri::command]
pub async fn graph_apply_change(
    state: State<'_, AppState>,
    request: ApplyChangeRequest,
) -> Result<ApplyOutcome, CommandError> {
    blocking(&state, move |g| g.apply_change(&Origin::User, &request)).await
}

#[tauri::command]
pub async fn graph_views(state: State<'_, AppState>) -> Result<Vec<GraphViewSummary>, CommandError> {
    blocking(&state, |g| g.views()).await
}

#[tauri::command]
pub async fn graph_view_get(state: State<'_, AppState>, view: ViewId) -> Result<GraphView, CommandError> {
    blocking(&state, move |g| g.view(&view)).await
}

#[tauri::command]
pub async fn graph_view_save(state: State<'_, AppState>, view: GraphViewInput) -> Result<GraphView, CommandError> {
    blocking(&state, move |g| g.save_view(&view)).await
}

#[tauri::command]
pub async fn graph_view_delete(state: State<'_, AppState>, view: ViewId) -> Result<bool, CommandError> {
    blocking(&state, move |g| g.delete_view(&view)).await
}

/// Recover an unavailable graph with an explicit option the user chose.
#[tauri::command]
pub async fn graph_recover(state: State<'_, AppState>, option: RecoveryOption) -> Result<GraphStatus, CommandError> {
    blocking(&state, move |g| g.recover(&option)).await
}

/// Cross-service operations of the open project that did not complete
/// (interrupted promotions, project creation) and need reconciliation.
#[tauri::command]
pub async fn graph_operations(state: State<'_, AppState>) -> Result<Vec<OperationRecord>, CommandError> {
    blocking(&state, |g| {
        let root: Option<PathBuf> = g.root();
        Ok(match root {
            Some(root) => g.journal().unresolved(&root, false),
            None => Vec::new(),
        })
    })
    .await
}

/// "Reindex project": rebuild the graph's derived projections (resources,
/// extracted declarations, observed links, coverage) from the files, reading
/// every file again (`force`, default true). Curated records (entities and
/// relations people, agents, presets, or services created) are never
/// touched. Runs on the reconciler thread; waits up to five minutes, after
/// which progress continues and is reported through `graph:event`.
#[tauri::command]
pub async fn graph_reconcile(state: State<'_, AppState>, force: Option<bool>) -> Result<ReconcileReport, CommandError> {
    let handle = state.reconciler.clone();
    tauri::async_runtime::spawn_blocking(move || {
        handle.reindex_blocking(force.unwrap_or(true), Duration::from_secs(300))
    })
    .await
    .map_err(|e| CommandError::new("internal.task", format!("Graph task failed: {e}")))?
    .map_err(CommandError::from)
}

/// Take a consistent backup of the project graph now
/// (`.exposition/graph-backups/`, `VACUUM INTO`; the newest are kept).
#[tauri::command]
pub async fn graph_backup(state: State<'_, AppState>) -> Result<BackupInfo, CommandError> {
    blocking(&state, |g| g.backup("manual")).await
}

/// Backups of the project graph, newest first (restorable through
/// `graph_recover` when the graph is damaged).
#[tauri::command]
pub async fn graph_backups(state: State<'_, AppState>) -> Result<Vec<BackupInfo>, CommandError> {
    blocking(&state, |g| g.backups()).await
}

/// Forward graph events to the webview in order.
pub fn forward_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().graph.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(GRAPH_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(GRAPH_RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}
