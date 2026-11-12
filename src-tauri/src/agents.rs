//! Agent commands and event forwarding.
//!
//! Model-provider requests happen in Rust (`exposition-agents`); the webview
//! only sends requests to the agent service and receives task events. The API
//! key crosses this boundary once, inbound, when the user saves it; nothing
//! here ever returns it.

use exposition_agents::{
    AgentError, Budgets, CreateTaskRequest, FileDiff, GenerationTaskRequest, ProviderSettings, ProviderStatus,
    ReviewActionResult, TaskDetail, TaskId, TaskReview, TaskSummary,
};
use exposition_core::path::WorkspacePath;
use exposition_graph::EntityId;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::CommandError;
use crate::state::AppState;

/// Every [`exposition_agents::AgentEvent`], in order.
pub const AGENT_EVENT: &str = "agents:event";
/// Emitted if the frontend fell behind and must re-read agent state.
pub const AGENT_RESYNC: &str = "agents:resync";

fn command_error(e: AgentError) -> CommandError {
    CommandError::new(e.code, e.message)
}

fn task_id(raw: TaskId) -> Result<TaskId, CommandError> {
    let valid =
        raw.0.len() <= 64 && raw.0.starts_with("task-") && raw.0.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid { Ok(raw) } else { Err(CommandError::new("agents.invalidTask", "invalid task identifier")) }
}

#[tauri::command]
pub async fn agents_provider_status(state: State<'_, AppState>) -> Result<ProviderStatus, CommandError> {
    Ok(state.agents.provider_status())
}

#[tauri::command]
pub async fn agents_provider_update(
    state: State<'_, AppState>,
    settings: ProviderSettings,
) -> Result<ProviderStatus, CommandError> {
    state.agents.update_settings(settings).map_err(command_error)
}

/// Store the API key in OS credential storage. Returns only whether a key is configured.
#[tauri::command]
pub async fn agents_provider_set_key(state: State<'_, AppState>, key: String) -> Result<ProviderStatus, CommandError> {
    if key.len() > 1024 {
        return Err(CommandError::new("agents.invalidKey", "the API key is too long"));
    }
    state.agents.set_api_key(&key).map_err(command_error)
}

#[tauri::command]
pub async fn agents_provider_clear_key(state: State<'_, AppState>) -> Result<ProviderStatus, CommandError> {
    state.agents.clear_api_key().map_err(command_error)
}

#[tauri::command]
pub async fn agents_task_list(state: State<'_, AppState>) -> Result<Vec<TaskSummary>, CommandError> {
    state.agents.list_tasks().map_err(command_error)
}

#[tauri::command]
pub async fn agents_task_detail(state: State<'_, AppState>, task: TaskId) -> Result<TaskDetail, CommandError> {
    state.agents.task_detail(&task_id(task)?).map_err(command_error)
}

#[tauri::command]
pub async fn agents_task_create(
    state: State<'_, AppState>,
    request: CreateTaskRequest,
) -> Result<TaskSummary, CommandError> {
    state.agents.create_task(request).map_err(command_error)
}

#[tauri::command]
pub async fn agents_task_continue(
    state: State<'_, AppState>,
    task: TaskId,
    prompt: String,
    budgets: Option<Budgets>,
) -> Result<TaskSummary, CommandError> {
    state.agents.continue_task(&task_id(task)?, &prompt, budgets).map_err(command_error)
}

#[tauri::command]
pub async fn agents_task_cancel(state: State<'_, AppState>, task: TaskId) -> Result<TaskSummary, CommandError> {
    state.agents.cancel_task(&task_id(task)?).map_err(command_error)
}

#[tauri::command]
pub async fn agents_approval_resolve(
    state: State<'_, AppState>,
    task: TaskId,
    approval: String,
    approved: bool,
) -> Result<(), CommandError> {
    if approval.len() > 128 {
        return Err(CommandError::new("agents.invalidApproval", "invalid approval identifier"));
    }
    state.agents.resolve_approval(&task_id(task)?, &approval, approved).map_err(command_error)
}

#[tauri::command]
pub async fn agents_review(state: State<'_, AppState>, task: TaskId) -> Result<TaskReview, CommandError> {
    state.agents.review(&task_id(task)?).map_err(command_error)
}

#[tauri::command]
pub async fn agents_review_diff(
    state: State<'_, AppState>,
    task: TaskId,
    path: WorkspacePath,
) -> Result<FileDiff, CommandError> {
    state.agents.file_diff(&task_id(task)?, &path).map_err(command_error)
}

#[tauri::command]
pub async fn agents_review_revert(
    state: State<'_, AppState>,
    task: TaskId,
    path: Option<WorkspacePath>,
) -> Result<Vec<ReviewActionResult>, CommandError> {
    state.agents.revert(&task_id(task)?, path.as_ref()).map_err(command_error)
}

#[tauri::command]
pub async fn agents_review_accept(
    state: State<'_, AppState>,
    task: TaskId,
    path: Option<WorkspacePath>,
) -> Result<Vec<ReviewActionResult>, CommandError> {
    state.agents.accept(&task_id(task)?, path.as_ref()).map_err(command_error)
}

/// Keep a stopped task as an investigation in the project graph (the user's
/// deliberate admission; returns the investigation entity).
#[tauri::command]
pub async fn agents_task_keep_investigation(
    state: State<'_, AppState>,
    task: TaskId,
) -> Result<EntityId, CommandError> {
    let task = task_id(task)?;
    let agents = state.agents.clone();
    tauri::async_runtime::spawn_blocking(move || agents.keep_as_investigation(&task))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Agent task failed: {e}")))?
        .map_err(command_error)
}

/// Start the project-generation task of a newly created project (New
/// Project › describe an objective): an ordinary whole-project task in the
/// open project, execution disabled unless requested.
#[tauri::command]
pub async fn agents_task_create_generation(
    state: State<'_, AppState>,
    request: GenerationTaskRequest,
) -> Result<TaskSummary, CommandError> {
    if request.objective.len() > 100_000 {
        return Err(CommandError::new("agents.promptTooLong", "The objective is too long."));
    }
    state.agents.create_generation_task(request).map_err(command_error)
}

/// Queue a script for the scripted provider — development builds only. The
/// command is compiled, listed (`DEV_COMMANDS`), and granted only in debug builds.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn agents_dev_load_script(state: State<'_, AppState>, script: serde_json::Value) -> Result<(), CommandError> {
    if script.to_string().len() > 1024 * 1024 {
        return Err(CommandError::new("agents.invalidScript", "the script is too large"));
    }
    state.agents.load_script(script).map_err(command_error)
}

/// Grant the debug-only agent command (never present in release builds).
#[cfg(debug_assertions)]
pub fn add_dev_capability(app: &AppHandle) -> tauri::Result<()> {
    use tauri::ipc::CapabilityBuilder;
    app.add_capability(
        CapabilityBuilder::new("dev-agents-scripted").window("main").permission("allow-agents-dev-load-script"),
    )
}

/// Forward agent events to the webview in order.
pub fn forward_events(app: AppHandle) {
    let mut rx = app.state::<AppState>().agents.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(AGENT_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(AGENT_RESYNC, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}
