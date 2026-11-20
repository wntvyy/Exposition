//! Agent commands and event forwarding.
//!
//! Model-provider requests happen in Rust (`exposition-agents`); the webview
//! only sends requests to the agent service and receives task events. The API
//! key crosses this boundary once, inbound, when the user saves it; nothing
//! here ever returns it.

use exposition_agents::attachments::{AttachmentId, IngestOutcome};
use exposition_agents::catalog::ModelCatalog;
use exposition_agents::permissions::{
    PermissionChoice, PermissionsChange, ProjectPermissionsEntry, ProjectPermissionsState,
};
use exposition_agents::{
    AgentError, AllowanceKind, ApprovalScope, Budgets, ContinueTaskRequest, CreateTaskRequest, FileDiff,
    GenerationTaskRequest, ProviderChoice, ProviderKind, ProviderSettings, ProviderStatus, QuestionAnswer,
    ReviewActionResult, TaskDetail, TaskId, TaskReview, TaskSummary,
};
use exposition_core::path::WorkspacePath;
use exposition_graph::EntityId;
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
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

/// Store a provider's API key in OS credential storage (Anthropic when no
/// provider is named). Returns only whether keys are configured.
#[tauri::command]
pub async fn agents_provider_set_key(
    state: State<'_, AppState>,
    key: String,
    provider: Option<ProviderKind>,
) -> Result<ProviderStatus, CommandError> {
    if key.len() > 1024 {
        return Err(CommandError::new("agents.invalidKey", "the API key is too long"));
    }
    state.agents.set_provider_key(provider.unwrap_or(ProviderKind::Anthropic), &key).map_err(command_error)
}

#[tauri::command]
pub async fn agents_provider_clear_key(
    state: State<'_, AppState>,
    provider: Option<ProviderKind>,
) -> Result<ProviderStatus, CommandError> {
    state.agents.clear_provider_key(provider.unwrap_or(ProviderKind::Anthropic)).map_err(command_error)
}

/// The models a connection offers (fetched with its credential in Rust).
#[tauri::command]
pub async fn agents_models_list(
    state: State<'_, AppState>,
    provider: ProviderKind,
    refresh: Option<bool>,
) -> Result<ModelCatalog, CommandError> {
    state.agents.models(provider, refresh.unwrap_or(false)).await.map_err(command_error)
}

fn attachment_id(raw: AttachmentId) -> Result<AttachmentId, CommandError> {
    if raw.is_valid() {
        Ok(raw)
    } else {
        Err(CommandError::new("attachments.invalid", "invalid attachment identifier"))
    }
}

/// Attach files: the backend opens the native file dialog, so the webview
/// never names a path; only the files the user picks are read (snapshotted).
#[tauri::command]
pub async fn agents_attachments_pick(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<IngestOutcome>, CommandError> {
    let agents = state.agents.clone();
    tokio::task::spawn_blocking(move || {
        let picked = app.dialog().file().set_title("Attach files").blocking_pick_files().unwrap_or_default();
        let paths: Vec<std::path::PathBuf> =
            picked.iter().filter_map(|p| p.as_path().map(|p| p.to_path_buf())).collect();
        agents.add_attachment_files(&paths)
    })
    .await
    .map_err(|e| CommandError::new("attachments.failed", e.to_string()))
}

/// Attach pasted bytes (an image from the clipboard), base64-encoded.
#[tauri::command]
pub async fn agents_attachments_add_bytes(
    state: State<'_, AppState>,
    name: Option<String>,
    media_type: Option<String>,
    data: String,
) -> Result<IngestOutcome, CommandError> {
    let agents = state.agents.clone();
    tokio::task::spawn_blocking(move || agents.add_attachment_bytes(name.as_deref(), media_type.as_deref(), &data))
        .await
        .map_err(|e| CommandError::new("attachments.failed", e.to_string()))
}

/// Files of recent native drops onto the window, as the desktop shell
/// reported them. The webview routes a drop to the chat and names the paths
/// it wants attached; only paths recorded here are read, each once and only
/// shortly after the drop, so the webview alone can never name a file to
/// read (as with the file dialog) and a drop that went elsewhere (the Files
/// view, no target) does not stay attachable for the rest of the session.
#[derive(Default)]
pub struct DroppedFiles(std::sync::Mutex<Vec<(std::path::PathBuf, std::time::Instant)>>);

/// Dropped paths remembered at most (older ones are forgotten first).
const MAX_DROPPED: usize = 4096;

/// How long after a native drop its paths can be attached. The chat claims
/// them as part of handling the same drop; this only has to cover that.
const DROP_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

impl DroppedFiles {
    /// Remember the paths of one native drop.
    pub fn record(&self, paths: &[std::path::PathBuf]) {
        self.record_at(paths, std::time::Instant::now());
    }

    fn record_at(&self, paths: &[std::path::PathBuf], now: std::time::Instant) {
        let mut held = self.0.lock().unwrap_or_else(|p| p.into_inner());
        Self::expire(&mut held, now);
        held.extend(paths.iter().map(|p| (p.clone(), now)));
        let excess = held.len().saturating_sub(MAX_DROPPED);
        held.drain(..excess);
    }

    /// Forget the drops that are older than [`DROP_LIFETIME`] at `now`.
    fn expire(held: &mut Vec<(std::path::PathBuf, std::time::Instant)>, now: std::time::Instant) {
        held.retain(|(_, at)| now.saturating_duration_since(*at) <= DROP_LIFETIME);
    }

    /// Split the requested paths into those a recent native drop delivered
    /// (taken, in order) and those it did not.
    pub fn claim_all(&self, requested: &[String]) -> Vec<Result<std::path::PathBuf, String>> {
        self.claim_all_at(requested, std::time::Instant::now())
    }

    fn claim_all_at(&self, requested: &[String], now: std::time::Instant) -> Vec<Result<std::path::PathBuf, String>> {
        let mut held = self.0.lock().unwrap_or_else(|p| p.into_inner());
        Self::expire(&mut held, now);
        requested
            .iter()
            .map(|raw| {
                let path = std::path::PathBuf::from(raw);
                // Take it if it was dropped and not yet taken.
                match held.iter().position(|(p, _)| p == &path) {
                    Some(i) => {
                        held.remove(i);
                        Ok(path)
                    }
                    None => Err(raw.clone()),
                }
            })
            .collect()
    }
}

/// Files in one drop onto the chat (the per-message limit applies afterwards).
const MAX_DROP_FILES: usize = 100;

/// Attach files dropped onto the chat from the operating system. Only paths
/// the desktop shell itself reported in a native drop are read (snapshotted,
/// like picked files); the originals stay where they are and nothing is
/// imported into the project. A folder is rejected, never read recursively.
#[tauri::command]
pub async fn agents_attachments_add_dropped(
    state: State<'_, AppState>,
    dropped: State<'_, DroppedFiles>,
    paths: Vec<String>,
) -> Result<Vec<IngestOutcome>, CommandError> {
    if paths.len() > MAX_DROP_FILES {
        return Err(CommandError::new(
            "attachments.tooMany",
            format!("At most {MAX_DROP_FILES} files can be dropped at once."),
        ));
    }
    let claimed = dropped.claim_all(&paths);
    let agents = state.agents.clone();
    tokio::task::spawn_blocking(move || {
        claimed
            .into_iter()
            .map(|entry| match entry {
                Ok(path) => agents.add_attachment_files(std::slice::from_ref(&path)).remove(0),
                Err(raw) => IngestOutcome::Rejected {
                    name: exposition_agents::attachments::safe_name(&raw, "file"),
                    code: "attachments.notDropped".into(),
                    message: "This file was not part of a drop onto the window. Drop it again, or choose it with Attach files."
                        .into(),
                },
            })
            .collect()
    })
    .await
    .map_err(|e| CommandError::new("attachments.failed", e.to_string()))
}

/// A thumbnail data URL for an image attachment.
#[tauri::command]
pub async fn agents_attachment_preview(
    state: State<'_, AppState>,
    id: AttachmentId,
) -> Result<Option<String>, CommandError> {
    state.agents.attachment_preview(&attachment_id(id)?).map_err(command_error)
}

/// Forget draft attachments removed before sending.
#[tauri::command]
pub async fn agents_attachments_discard(
    state: State<'_, AppState>,
    ids: Vec<AttachmentId>,
) -> Result<(), CommandError> {
    let ids = ids.into_iter().map(attachment_id).collect::<Result<Vec<_>, _>>()?;
    state.agents.discard_attachments(&ids).map_err(command_error)
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
    attachments: Option<Vec<AttachmentId>>,
    provider: Option<ProviderChoice>,
) -> Result<TaskSummary, CommandError> {
    let attachments = attachments.unwrap_or_default().into_iter().map(attachment_id).collect::<Result<Vec<_>, _>>()?;
    state
        .agents
        .continue_task_with(&task_id(task)?, ContinueTaskRequest { prompt, budgets, attachments, provider })
        .map_err(command_error)
}

#[tauri::command]
pub async fn agents_task_cancel(state: State<'_, AppState>, task: TaskId) -> Result<TaskSummary, CommandError> {
    state.agents.cancel_task(&task_id(task)?).map_err(command_error)
}

/// Answer an approval request. `scope: "task"` ("Approve for this task")
/// also allows later calls of the request's repeatable kinds in this task;
/// absent or `"once"` approves this call only. Only the approval card calls
/// this; rendered content has no IPC access.
#[tauri::command]
pub async fn agents_approval_resolve(
    state: State<'_, AppState>,
    task: TaskId,
    approval: String,
    approved: bool,
    scope: Option<ApprovalScope>,
) -> Result<(), CommandError> {
    if approval.len() > 128 {
        return Err(CommandError::new("agents.invalidApproval", "invalid approval identifier"));
    }
    state
        .agents
        .resolve_approval_scoped(&task_id(task)?, &approval, approved, scope.unwrap_or_default())
        .map_err(command_error)
}

fn question_request(request: &str) -> Result<(), CommandError> {
    let valid =
        !request.is_empty() && request.len() <= 128 && request.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid { Ok(()) } else { Err(CommandError::new("agents.noQuestion", "invalid question identifier")) }
}

/// Answer a structured question (one answer per question, at the request's
/// current revision). The answer is user content, never an authorization;
/// a second identical answer is accepted and does nothing.
#[tauri::command]
pub async fn agents_question_answer(
    state: State<'_, AppState>,
    task: TaskId,
    request: String,
    revision: u32,
    answers: Vec<QuestionAnswer>,
) -> Result<(), CommandError> {
    question_request(&request)?;
    if answers.len() > exposition_agents::questions::MAX_QUESTIONS {
        return Err(CommandError::new("agents.invalidAnswer", "too many answers"));
    }
    state.agents.answer_question(&task_id(task)?, &request, revision, answers).map_err(command_error)
}

/// "Chat about it": one bounded discussion turn about a pending question in
/// the task's own worker (read-only tools; the question's batch stays held).
#[tauri::command]
pub async fn agents_question_discuss(
    state: State<'_, AppState>,
    task: TaskId,
    request: String,
    revision: u32,
    text: String,
) -> Result<(), CommandError> {
    question_request(&request)?;
    if text.len() > 4 * exposition_agents::questions::MAX_DISCUSSION_CHARS {
        return Err(CommandError::new("agents.invalidAnswer", "the message is too long"));
    }
    state.agents.discuss_question(&task_id(task)?, &request, revision, &text).map_err(command_error)
}

/// The bytes of a figure one of the task's own tool calls recorded (read
/// through the research artifact store; the webview shows it only as an
/// image, never inline markup).
#[tauri::command]
pub async fn agents_figure(
    state: State<'_, AppState>,
    task: TaskId,
    run_id: String,
    artifact_id: String,
) -> Result<Response, CommandError> {
    if run_id.len() > 128 || artifact_id.len() > 128 {
        return Err(CommandError::new("agents.figureUnavailable", "invalid figure reference"));
    }
    let task = task_id(task)?;
    let agents = state.agents.clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || agents.figure(&task, &run_id, &artifact_id))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Agent task failed: {e}")))?
        .map_err(command_error)?;
    Ok(Response::new(bytes))
}

/// Revoke one allowance of a task (`kind`) or all of them (`null`).
#[tauri::command]
pub async fn agents_allowance_revoke(
    state: State<'_, AppState>,
    task: TaskId,
    kind: Option<AllowanceKind>,
) -> Result<(), CommandError> {
    state.agents.revoke_allowance(&task_id(task)?, kind).map_err(command_error)
}

fn validate_root(root: &str) -> Result<(), CommandError> {
    if root.is_empty() || root.len() > 4096 || root.contains('\0') {
        return Err(CommandError::new("agents.invalidWorkspace", "invalid project folder"));
    }
    Ok(())
}

/// The open project's permissions (Auto) and the execution facts the
/// composer states.
#[tauri::command]
pub async fn agents_permissions_get(
    state: State<'_, AppState>,
    root: String,
) -> Result<ProjectPermissionsState, CommandError> {
    validate_root(&root)?;
    state.agents.project_permissions(&root).map_err(command_error)
}

/// Every project-permission record (Settings › Agents).
#[tauri::command]
pub async fn agents_permissions_list(state: State<'_, AppState>) -> Result<Vec<ProjectPermissionsEntry>, CommandError> {
    Ok(state.agents.list_project_permissions())
}

/// Grant or change the open project's permissions (composer "Use these
/// permissions", Settings › Change). A narrowing stops the project's Auto
/// tasks; that wait runs off the async runtime.
#[tauri::command]
pub async fn agents_permissions_set(
    state: State<'_, AppState>,
    root: String,
    choice: PermissionChoice,
) -> Result<PermissionsChange, CommandError> {
    validate_root(&root)?;
    let agents = state.agents.clone();
    tauri::async_runtime::spawn_blocking(move || agents.set_project_permissions(&root, choice))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Agent task failed: {e}")))?
        .map_err(command_error)
}

/// Revoke a project's permissions (the project need not be open).
#[tauri::command]
pub async fn agents_permissions_revoke(
    state: State<'_, AppState>,
    root: String,
    project: Option<String>,
) -> Result<PermissionsChange, CommandError> {
    validate_root(&root)?;
    if project.as_ref().is_some_and(|p| p.len() > 128) {
        return Err(CommandError::new("agents.invalidWorkspace", "invalid project identity"));
    }
    let agents = state.agents.clone();
    tauri::async_runtime::spawn_blocking(move || agents.revoke_project_permissions(&root, project.as_deref()))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Agent task failed: {e}")))?
        .map_err(command_error)
}

/// Clear task allowances when folder trust is revoked (from any surface).
pub fn forward_trust_revocations(app: AppHandle) {
    let mut rx = app.state::<AppState>().execution.subscribe_trust();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(change) if !change.granted => {
                    let agents = app.state::<AppState>().agents.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || agents.trust_revoked(&change.root)).await;
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
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

#[cfg(test)]
mod dropped_tests {
    use std::path::PathBuf;

    use super::DroppedFiles;

    #[test]
    fn only_natively_dropped_paths_are_taken_and_each_once() {
        let dropped = DroppedFiles::default();
        dropped.record(&[PathBuf::from("C:/in/a.txt"), PathBuf::from("C:/in/b.png")]);
        let claimed = dropped.claim_all(&["C:/in/b.png".into(), "C:/Users/me/secret.txt".into()]);
        assert_eq!(claimed[0], Ok(PathBuf::from("C:/in/b.png")));
        assert_eq!(claimed[1], Err("C:/Users/me/secret.txt".to_owned()));
        // Taken once: naming it again reads nothing.
        assert!(dropped.claim_all(&["C:/in/b.png".into()])[0].is_err());
        // The same file dropped twice can be taken twice.
        dropped.record(&[PathBuf::from("C:/in/a.txt")]);
        assert!(dropped.claim_all(&["C:/in/a.txt".into(), "C:/in/a.txt".into()]).iter().all(Result::is_ok));
        assert!(dropped.claim_all(&["C:/in/a.txt".into()])[0].is_err());
    }

    #[test]
    fn old_drops_are_forgotten() {
        let dropped = DroppedFiles::default();
        let many: Vec<PathBuf> = (0..super::MAX_DROPPED + 5).map(|i| PathBuf::from(format!("f{i}"))).collect();
        dropped.record(&many);
        assert!(dropped.claim_all(&["f0".into()])[0].is_err());
        assert!(dropped.claim_all(&[format!("f{}", super::MAX_DROPPED + 4)])[0].is_ok());
    }

    /// Review finding 4: a drop that was not attached (it went to the Files
    /// view, or nowhere) cannot be attached long afterwards.
    #[test]
    fn a_drop_can_be_attached_only_shortly_after_it_happened() {
        use std::time::{Duration, Instant};
        let dropped = DroppedFiles::default();
        let t0 = Instant::now();
        dropped.record_at(&[PathBuf::from("C:/in/a.txt"), PathBuf::from("C:/in/b.txt")], t0);
        // Within the lifetime: taken.
        assert!(dropped.claim_all_at(&["C:/in/a.txt".into()], t0 + super::DROP_LIFETIME)[0].is_ok());
        // Past it: no longer attachable, and no longer held.
        let late = t0 + super::DROP_LIFETIME + Duration::from_secs(1);
        assert!(dropped.claim_all_at(&["C:/in/b.txt".into()], late)[0].is_err());
        assert!(dropped.0.lock().unwrap().is_empty());
        // A later drop forgets the stale ones and is itself attachable.
        dropped.record_at(&[PathBuf::from("C:/in/c.txt")], t0);
        dropped.record_at(&[PathBuf::from("C:/in/d.txt")], late);
        assert_eq!(dropped.0.lock().unwrap().len(), 1);
        assert!(dropped.claim_all_at(&["C:/in/c.txt".into()], late)[0].is_err());
        assert!(dropped.claim_all_at(&["C:/in/d.txt".into()], late)[0].is_ok());
    }
}
