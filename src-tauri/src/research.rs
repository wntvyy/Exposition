//! Research commands and event forwarding (M4).
//!
//! Domain logic lives in `exposition-research` (sessions, runs, provenance) and
//! `exposition-attach` (format-owned figure/table insertion). Every request is
//! validated in Rust: paths are `WorkspacePath`s resolved under the canonical
//! workspace root, run/artifact ids are looked up rather than trusted, and only
//! discovered environments can be launched.

use std::path::PathBuf;

use exposition_core::artifact::{AttachArtifactOutcome, AttachArtifactRequest};
use exposition_research::types::{
    EnvironmentReport, ExecuteRequest, OutputPage, RunManifest, RunSummary, SessionInfo, WorkspaceStatus,
};
use exposition_research::{ResearchError, StartSessionRequest};
use serde::Serialize;
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

/// TEMPORARY until the workspace service (m1/workspace) owns project roots and
/// trust: the Research panel opens a folder and records the user's explicit
/// trust decision here.
#[tauri::command]
pub async fn research_workspace_set(
    state: State<'_, AppState>,
    root: String,
    trusted: bool,
) -> Result<WorkspaceStatus, CommandError> {
    Ok(state.research.set_workspace(&PathBuf::from(root), trusted)?)
}

#[tauri::command]
pub async fn research_workspace(state: State<'_, AppState>) -> Result<Option<WorkspaceStatus>, CommandError> {
    Ok(state.research.workspace())
}

#[tauri::command]
pub async fn research_environments(
    state: State<'_, AppState>,
    refresh: bool,
    extra_interpreters: Option<Vec<String>>,
) -> Result<EnvironmentReport, CommandError> {
    let extra = extra_interpreters.unwrap_or_default().into_iter().map(PathBuf::from).collect();
    Ok(state.research.environments(refresh, extra).await?)
}

#[tauri::command]
pub async fn research_session_start(
    state: State<'_, AppState>,
    request: StartSessionRequest,
) -> Result<SessionInfo, CommandError> {
    Ok(state.research.start_session(request).await?)
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
pub async fn research_execute(state: State<'_, AppState>, request: ExecuteRequest) -> Result<RunSummary, CommandError> {
    Ok(state.research.execute(request)?)
}

#[tauri::command]
pub async fn research_run_cancel(state: State<'_, AppState>, run_id: String) -> Result<(), CommandError> {
    Ok(state.research.cancel_run(&run_id).await?)
}

#[tauri::command]
pub async fn research_runs(state: State<'_, AppState>) -> Result<Vec<RunSummary>, CommandError> {
    Ok(state.research.runs()?)
}

#[tauri::command]
pub async fn research_run(state: State<'_, AppState>, run_id: String) -> Result<RunManifest, CommandError> {
    Ok(state.research.run_manifest(&run_id)?)
}

#[tauri::command]
pub async fn research_run_output(
    state: State<'_, AppState>,
    run_id: String,
    after: Option<u32>,
    limit: Option<u32>,
) -> Result<OutputPage, CommandError> {
    Ok(state.research.run_output(&run_id, after, limit.unwrap_or(200))?)
}

/// Raw artifact bytes (fetched on demand for display; never pushed in events).
#[tauri::command]
pub async fn research_artifact(
    state: State<'_, AppState>,
    run_id: String,
    artifact_id: String,
) -> Result<Response, CommandError> {
    let (_, bytes) = state.research.artifact_bytes(&run_id, &artifact_id)?;
    Ok(Response::new(bytes))
}

/// Format-owned attachment command (`AttachArtifactRequest`). Belongs to the
/// typesetting module once it lands; research only invokes it.
#[tauri::command]
pub async fn artifact_attach(
    state: State<'_, AppState>,
    request: AttachArtifactRequest,
) -> Result<AttachArtifactResult, CommandError> {
    let root = state
        .research
        .workspace()
        .map(|w| PathBuf::from(w.root))
        .ok_or_else(|| CommandError::from(ResearchError::NoWorkspace))?;
    let (outcome, warnings) = exposition_attach::attach_artifact(&state.documents, &root, &request)?;
    Ok(AttachArtifactResult { outcome, warnings })
}

/// Forward research events to the webview in order.
pub fn forward_events(app: AppHandle) {
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
