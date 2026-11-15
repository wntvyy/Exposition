//! Guided environment setup and the remembered environment choice (A1).
//!
//! Domain logic lives in `exposition_research::setup`: plans are built in
//! Rust from discovered environments and validated package names (the
//! webview names a discovered environment, never an executable), starting
//! re-plans and refuses a changed plan, and every step is a trusted project
//! job of the execution service. Setups started here belong to the user.

use std::path::PathBuf;

use exposition_research::ResearchError;
use exposition_research::setup::{SetupPlan, SetupRequest, SetupStatus};
use exposition_research::types::SessionOwner;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::error::CommandError;
use crate::state::AppState;

/// Event carrying every [`exposition_research::setup::SetupEvent`], in order.
pub const SETUP_EVENT: &str = "research:setup";
/// Emitted if the frontend fell behind and must re-read the setup status.
pub const SETUP_RESYNC: &str = "research:setup-resync";

fn project_root(state: &AppState) -> Result<PathBuf, CommandError> {
    crate::research::sync_workspace(state)?;
    state.research.workspace_root().ok_or_else(|| CommandError::from(ResearchError::NoWorkspace))
}

/// Describe a setup for the open project (runs nothing).
#[tauri::command]
pub async fn research_setup_plan(state: State<'_, AppState>, request: SetupRequest) -> Result<SetupPlan, CommandError> {
    let root = project_root(&state)?;
    Ok(state.research.setup_plan(&root, &request).await?)
}

/// Start a reviewed plan as the user's explicit action. Rust builds the plan
/// again and refuses one that changed (`setup.planStale`).
#[tauri::command]
pub async fn research_setup_start(state: State<'_, AppState>, plan: SetupPlan) -> Result<SetupStatus, CommandError> {
    project_root(&state)?;
    Ok(state.research.setup_start(&plan, SessionOwner::User).await?)
}

/// Cancel a running setup (false when it is not running).
#[tauri::command]
pub async fn research_setup_cancel(state: State<'_, AppState>, setup_id: String) -> Result<bool, CommandError> {
    Ok(state.research.setup_cancel(&setup_id))
}

/// The running setup of the open project, else its last finished one.
#[tauri::command]
pub async fn research_setup_status(state: State<'_, AppState>) -> Result<Option<SetupStatus>, CommandError> {
    crate::research::sync_workspace(&state)?;
    Ok(state.research.setup_status())
}

/// The environment remembered for the open project (it may be stale; the
/// webview falls back to the first usable environment).
#[tauri::command]
pub async fn research_selection_get(state: State<'_, AppState>) -> Result<Option<String>, CommandError> {
    project_root(&state)?;
    Ok(state.research.selected_environment()?)
}

/// Remember the environment chosen for the open project (`null` forgets it).
#[tauri::command]
pub async fn research_selection_set(
    state: State<'_, AppState>,
    environment_id: Option<String>,
) -> Result<(), CommandError> {
    project_root(&state)?;
    Ok(state.research.select_environment(environment_id).await?)
}

/// Forward setup events to the webview in order.
pub fn forward_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().research.subscribe_setup();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(SETUP_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(SETUP_RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}
