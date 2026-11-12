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
use exposition_core::document::{DocumentId, EditOrigin};
use exposition_research::types::{
    EnvironmentReport, ExecuteRequest, RunManifest, RunOutputPage, RunSource, RunSummary, SessionInfo, SessionOwner,
    WorkspaceStatus,
};
use exposition_research::{ResearchError, StartSessionRequest};
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
    Ok(state.research.execute(ExecuteRequest {
        session_id: request.session_id,
        source: request.source,
        requester: SessionOwner::User,
    })?)
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

/// Forward research events to the webview in order, and follow the workspace
/// service's open folder.
pub fn forward_events(app: AppHandle) {
    let mut workspace_rx = app.state::<AppState>().workspace.subscribe();
    let workspace_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match workspace_rx.recv().await {
                Ok(WorkspaceEvent::Opened { .. } | WorkspaceEvent::Closed) | Err(RecvError::Lagged(_)) => {
                    let state = workspace_app.state::<AppState>();
                    if sync_workspace(&state).is_ok() {
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
