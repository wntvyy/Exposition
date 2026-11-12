//! Research commands and event forwarding (M4).
//!
//! Domain logic lives in `exposition-research` (sessions, runs, provenance) and
//! `exposition-attach` (format-owned figure/table insertion). Every request is
//! validated in Rust: paths are `WorkspacePath`s resolved under the canonical
//! workspace root, run/artifact ids are looked up rather than trusted, and only
//! discovered environments can be launched.

use std::path::PathBuf;

use exposition_core::artifact::{
    ArtifactKind, AttachArtifactOutcome, AttachArtifactRequest, AttachmentPlacement, RunId,
};
use std::sync::Arc;

use exposition_core::document::{DocumentEvent, DocumentId, EditOrigin};
use exposition_core::path::WorkspacePath;
use exposition_graph::{EntityId, OperationId};
use exposition_research::project::types::{
    CreateSpaceFileRequest, ExperimentDetail, ExperimentInput, ImportOutcome, ImportRequest, PromotionOutcome,
    PromotionPreview, PromotionRequest, PromotionsState, ReconcileOutcome, ResearchListing, ResearchSpaceInfo,
    RetainOutcome, RetainRequest, SetResultStateRequest, SpaceFile,
};
use exposition_research::types::{
    EnvironmentReport, ExecuteRequest, RunManifest, RunOutputPage, RunSource, RunSummary, SessionInfo, SessionOwner,
    WorkspaceStatus,
};
use exposition_research::{ResearchError, ResearchProject, StartSessionRequest};
use exposition_workspace::types::WorkspaceEvent;
use serde::{Deserialize, Serialize};
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;
use ts_rs::TS;

use crate::error::CommandError;
use crate::state::AppState;

/// Event carrying every [`exposition_research::types::ResearchEvent`], in order.
pub const RESEARCH_EVENT: &str = "research:event";
/// Emitted if the frontend fell behind and must re-read research state.
pub const RESEARCH_RESYNC: &str = "research:resync";

impl From<ResearchError> for CommandError {
    fn from(error: ResearchError) -> Self {
        CommandError::new(error.code(), error.to_string())
    }
}

impl From<exposition_attach::AttachError> for CommandError {
    fn from(error: exposition_attach::AttachError) -> Self {
        CommandError::new(error.code(), error.to_string())
    }
}

/// Result of inserting/updating an artifact in a document.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AttachArtifactResult {
    pub outcome: AttachArtifactOutcome,
    /// Non-fatal advice, e.g. a LaTeX package the preamble should load.
    pub warnings: Vec<String>,
}

/// Start a research session from the webview. Sessions started here always
/// belong to the user (an `owner` sent by the webview is ignored); agent
/// sessions are created only by the agents service in Rust.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ResearchStartRequest {
    pub environment_id: String,
}

/// Run code from the webview, always as the user (a `requester` sent by the
/// webview is ignored).
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ResearchExecuteRequest {
    pub session_id: String,
    pub source: RunSource,
    /// Project files the code reads (e.g. an experiment's declared inputs),
    /// pinned by content in the run's inputs when it is dispatched.
    #[serde(default)]
    #[ts(optional)]
    pub declared_inputs: Option<Vec<WorkspacePath>>,
}

/// Insert or update a run's figure/table in an open document. The webview
/// names the run and artifact; the record (path, kind, hash) is read from the
/// run's stored manifest in Rust, never taken from the caller.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttachRunArtifactRequest {
    pub document: DocumentId,
    pub base_revision: u64,
    pub run_id: String,
    pub artifact_id: String,
    pub placement: AttachmentPlacement,
    pub caption: Option<String>,
    pub attachment_key: Option<String>,
}

/// Follow the workspace service's open folder: research runs in the open
/// project (registered with the execution service by `workspace_open`); trust
/// is always the execution service's decision.
fn sync_workspace(state: &AppState) -> Result<(), CommandError> {
    match state.workspace.current() {
        Some(info) => {
            let root = PathBuf::from(&info.root);
            let current = state.research.workspace_root();
            let same = current.as_ref().is_some_and(|c| std::fs::canonicalize(&root).map(|r| &r == c).unwrap_or(false));
            if !same {
                state.research.set_workspace(&root, false)?;
            }
        }
        None => state.research.clear_workspace(),
    }
    Ok(())
}

#[tauri::command]
pub async fn research_workspace(state: State<'_, AppState>) -> Result<Option<WorkspaceStatus>, CommandError> {
    sync_workspace(&state)?;
    Ok(state.research.workspace())
}

#[tauri::command]
pub async fn research_environments(
    state: State<'_, AppState>,
    refresh: bool,
) -> Result<EnvironmentReport, CommandError> {
    sync_workspace(&state)?;
    Ok(state.research.environments(refresh).await?)
}

#[tauri::command]
pub async fn research_session_start(
    state: State<'_, AppState>,
    request: ResearchStartRequest,
) -> Result<SessionInfo, CommandError> {
    sync_workspace(&state)?;
    Ok(state
        .research
        .start_session(StartSessionRequest { environment_id: request.environment_id, owner: SessionOwner::User })
        .await?)
}

#[tauri::command]
pub async fn research_sessions(state: State<'_, AppState>) -> Result<Vec<SessionInfo>, CommandError> {
    Ok(state.research.sessions())
}

#[tauri::command]
pub async fn research_session_interrupt(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionInfo, CommandError> {
    Ok(state.research.interrupt(&session_id).await?)
}

#[tauri::command]
pub async fn research_session_restart(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionInfo, CommandError> {
    Ok(state.research.restart(&session_id).await?)
}

#[tauri::command]
pub async fn research_session_shutdown(state: State<'_, AppState>, session_id: String) -> Result<(), CommandError> {
    Ok(state.research.shutdown(&session_id).await?)
}

#[tauri::command]
pub async fn research_execute(
    state: State<'_, AppState>,
    request: ResearchExecuteRequest,
) -> Result<RunSummary, CommandError> {
    sync_workspace(&state)?;
    let declared = request.declared_inputs.unwrap_or_default();
    Ok(state.research.execute_with_inputs(
        ExecuteRequest { session_id: request.session_id, source: request.source, requester: SessionOwner::User },
        &declared,
    )?)
}

#[tauri::command]
pub async fn research_run_cancel(state: State<'_, AppState>, run_id: String) -> Result<(), CommandError> {
    Ok(state.research.cancel_run(&run_id).await?)
}

#[tauri::command]
pub async fn research_runs(state: State<'_, AppState>) -> Result<Vec<RunSummary>, CommandError> {
    sync_workspace(&state)?;
    Ok(state.research.runs()?)
}

#[tauri::command]
pub async fn research_run(state: State<'_, AppState>, run_id: String) -> Result<RunManifest, CommandError> {
    sync_workspace(&state)?;
    Ok(state.research.run_manifest(&run_id)?)
}

#[tauri::command]
pub async fn research_run_output(
    state: State<'_, AppState>,
    run_id: String,
    after: Option<u32>,
    limit: Option<u32>,
) -> Result<RunOutputPage, CommandError> {
    sync_workspace(&state)?;
    Ok(state.research.run_output(&run_id, after, limit.unwrap_or(200))?)
}

/// Raw artifact bytes (fetched on demand for display; never pushed in events).
#[tauri::command]
pub async fn research_artifact(
    state: State<'_, AppState>,
    run_id: String,
    artifact_id: String,
) -> Result<Response, CommandError> {
    sync_workspace(&state)?;
    let (_, bytes) = state.research.artifact_bytes(&run_id, &artifact_id)?;
    Ok(Response::new(bytes))
}

/// Format-owned attachment command (`AttachArtifactRequest`). Belongs to the
/// typesetting module once it lands; research only invokes it.
#[tauri::command]
pub async fn artifact_attach(
    state: State<'_, AppState>,
    request: AttachRunArtifactRequest,
) -> Result<AttachArtifactResult, CommandError> {
    sync_workspace(&state)?;
    let root = state.research.workspace_root().ok_or_else(|| CommandError::from(ResearchError::NoWorkspace))?;
    // The artifact record comes from the run's stored manifest (in the open workspace).
    let (artifact, _) = state.research.artifact(&request.run_id, &request.artifact_id)?;
    if !matches!(artifact.kind, ArtifactKind::Figure | ArtifactKind::Table) {
        return Err(CommandError::new("attach.unsupported", "Only figures and tables can be attached to documents."));
    }
    let request = AttachArtifactRequest {
        document: request.document,
        base_revision: request.base_revision,
        artifact,
        run: Some(RunId(request.run_id.clone())),
        placement: request.placement,
        caption: request.caption,
        attachment_key: request.attachment_key,
        origin: EditOrigin::Research { run_id: request.run_id },
    };
    let (outcome, warnings) = exposition_attach::attach_artifact(&state.documents, &root, &request)?;
    Ok(AttachArtifactResult { outcome, warnings })
}

// ---- research in the project graph ------------------------------------------

/// Run a blocking research-project operation off the async executor.
async fn project<T, F>(state: &State<'_, AppState>, f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&ResearchProject) -> Result<T, ResearchError> + Send + 'static,
{
    sync_workspace(state)?;
    let project = Arc::clone(&state.research_project);
    tauri::async_runtime::spawn_blocking(move || f(&project))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Research task failed: {e}")))?
        .map_err(CommandError::from)
}

/// The project's research space, if Research is enabled (no side effects).
#[tauri::command]
pub async fn research_space(state: State<'_, AppState>) -> Result<Option<ResearchSpaceInfo>, CommandError> {
    project(&state, |p| p.space()).await
}

/// Enable Research: one typed research space and its folders. Launches nothing.
#[tauri::command]
pub async fn research_space_enable(state: State<'_, AppState>) -> Result<ResearchSpaceInfo, CommandError> {
    project(&state, |p| p.enable()).await
}

/// The Research explorer: groups over actual resource identities.
#[tauri::command]
pub async fn research_listing(state: State<'_, AppState>) -> Result<ResearchListing, CommandError> {
    project(&state, |p| p.listing()).await
}

/// Create an empty research file (through the document service).
#[tauri::command]
pub async fn research_space_create_file(
    state: State<'_, AppState>,
    request: CreateSpaceFileRequest,
) -> Result<SpaceFile, CommandError> {
    let workspace = Arc::clone(&state.workspace);
    project(&state, move |p| p.create_file(&workspace, &request)).await
}

/// Import a file the person chose: an explicit copy (with recorded origin) or
/// a read-only reference.
#[tauri::command]
pub async fn research_space_import(
    state: State<'_, AppState>,
    request: ImportRequest,
) -> Result<ImportOutcome, CommandError> {
    project(&state, move |p| p.import(&request)).await
}

/// Keep / archive / discard a run's figure, table, or data output.
#[tauri::command]
pub async fn research_retain(
    state: State<'_, AppState>,
    request: RetainRequest,
) -> Result<RetainOutcome, CommandError> {
    project(&state, move |p| p.retain(&request)).await
}

/// Change the lifecycle of a retained result (never deletes anything).
#[tauri::command]
pub async fn research_result_state(
    state: State<'_, AppState>,
    request: SetResultStateRequest,
) -> Result<RetainOutcome, CommandError> {
    project(&state, move |p| p.set_result_state(&request)).await
}

#[tauri::command]
pub async fn research_experiment_save(
    state: State<'_, AppState>,
    input: ExperimentInput,
) -> Result<ExperimentDetail, CommandError> {
    project(&state, move |p| p.save_experiment(&input)).await
}

#[tauri::command]
pub async fn research_experiment(
    state: State<'_, AppState>,
    entity: EntityId,
) -> Result<ExperimentDetail, CommandError> {
    project(&state, move |p| p.experiment(&entity)).await
}

/// Review step of a promotion: the exact markup, target revision, and digest.
#[tauri::command]
pub async fn research_promotion_prepare(
    state: State<'_, AppState>,
    request: PromotionRequest,
) -> Result<PromotionPreview, CommandError> {
    project(&state, move |p| p.prepare_promotion(&request)).await
}

/// Apply a reviewed promotion (refused if the document or artifact changed).
#[tauri::command]
pub async fn research_promotion_approve(
    state: State<'_, AppState>,
    promotion: String,
) -> Result<PromotionOutcome, CommandError> {
    project(&state, move |p| p.approve_promotion(&promotion)).await
}

#[tauri::command]
pub async fn research_promotion_cancel(state: State<'_, AppState>, promotion: String) -> Result<bool, CommandError> {
    project(&state, move |p| Ok(p.cancel_promotion(&promotion))).await
}

/// Promotions needing reconciliation and attachments research follows.
#[tauri::command]
pub async fn research_promotions(state: State<'_, AppState>) -> Result<PromotionsState, CommandError> {
    project(&state, |p| p.promotions()).await
}

/// Reconcile an interrupted promotion against the actual document.
#[tauri::command]
pub async fn research_promotion_reconcile(
    state: State<'_, AppState>,
    operation: OperationId,
) -> Result<ReconcileOutcome, CommandError> {
    project(&state, move |p| p.reconcile_promotion(&operation)).await
}

/// Debug builds only (tests): make the next promotion's graph write fail, to
/// exercise the pending-reconciliation path in the real application. The
/// command is compiled, listed (`DEV_COMMANDS`), and granted only in debug builds.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn research_dev_fail_next_graph_write(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.research_project.fail_next_graph_write();
    Ok(())
}

/// Grant the debug-only research command (never present in release builds).
#[cfg(debug_assertions)]
pub fn add_dev_capability(app: &AppHandle) -> tauri::Result<()> {
    use tauri::ipc::CapabilityBuilder;
    app.add_capability(
        CapabilityBuilder::new("dev-research-faults")
            .window("main")
            .permission("allow-research-dev-fail-next-graph-write"),
    )
}

/// Follow promoted attachments as documents change (undo, redo, edits).
fn follow_documents(app: AppHandle) {
    let mut rx = app.state::<AppState>().documents.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            let document = match rx.recv().await {
                Ok(DocumentEvent::Changed(change)) => change.document,
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => {
                    let project = Arc::clone(&app.state::<AppState>().research_project);
                    let _ = tauri::async_runtime::spawn_blocking(move || project.load_promotions()).await;
                    continue;
                }
                Err(RecvError::Closed) => break,
            };
            let state = app.state::<AppState>();
            let Ok(snapshot) = state.documents.snapshot(&document) else { continue };
            let Some(path) = snapshot.path.clone() else { continue };
            if !state.research_project.follows(&path) {
                continue;
            }
            let project = Arc::clone(&state.research_project);
            let text = snapshot.text.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || project.observe_text(&path, &text, false)).await;
        }
    });
}

/// Forward research events to the webview in order, and follow the workspace
/// service's open folder.
pub fn forward_events(app: AppHandle) {
    follow_documents(app.clone());
    let mut workspace_rx = app.state::<AppState>().workspace.subscribe();
    let workspace_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match workspace_rx.recv().await {
                Ok(WorkspaceEvent::Opened { .. } | WorkspaceEvent::Closed) | Err(RecvError::Lagged(_)) => {
                    let state = workspace_app.state::<AppState>();
                    if sync_workspace(&state).is_ok() {
                        // Interrupted promotions are marked for reconciliation
                        // (never re-run); followed attachments are re-checked.
                        let project = Arc::clone(&state.research_project);
                        let _ = tauri::async_runtime::spawn_blocking(move || project.load_promotions()).await;
                        let _ = workspace_app.emit(RESEARCH_RESYNC, ());
                    }
                }
                Ok(_) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
    let mut rx = app.state::<AppState>().research.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(RESEARCH_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(RESEARCH_RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}
