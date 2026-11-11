//! Workspace, file, save, recovery, and settings commands, plus workspace event
//! forwarding. Every path argument is a lexically validated `WorkspacePath`
//! (deserialization rejects absolute and traversing paths) and is resolved by
//! the workspace service against the canonical root, rejecting link escapes.
//! File I/O runs on the blocking pool, never on the UI thread.

use std::path::PathBuf;
use std::sync::Arc;

use exposition_core::document::{DocumentId, DocumentInfo, LineEnding, TransactionOutcome};
use exposition_core::path::WorkspacePath;
use exposition_workspace::WorkspaceError;
use exposition_workspace::WorkspaceService;
use exposition_workspace::settings::{AppSettings, ProjectSettings};
use exposition_workspace::types::{
    DocumentOpened, FileEntry, RecentProject, RecoveryEntry, RestoreOutcome, SaveAllOutcome, WorkspaceInfo,
};
use exposition_workspace::{AppSettingsState, ProjectSettingsState};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;

use crate::CommandError;
use crate::state::AppState;

/// Event carrying every [`exposition_workspace::types::WorkspaceEvent`].
pub const WORKSPACE_EVENT: &str = "workspace:event";

impl From<WorkspaceError> for CommandError {
    fn from(error: WorkspaceError) -> Self {
        CommandError::new(error.code, error.message)
    }
}

/// Run a blocking workspace operation off the async executor.
async fn blocking<T, F>(state: &State<'_, AppState>, f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&WorkspaceService) -> Result<T, WorkspaceError> + Send + 'static,
{
    let service = Arc::clone(&state.workspace);
    tauri::async_runtime::spawn_blocking(move || f(&service))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Workspace task failed: {e}")))?
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn workspace_open(state: State<'_, AppState>, path: String) -> Result<WorkspaceInfo, CommandError> {
    blocking(&state, move |ws| ws.open_workspace(&PathBuf::from(path))).await
}

#[tauri::command]
pub async fn workspace_close(state: State<'_, AppState>) -> Result<(), CommandError> {
    blocking(&state, |ws| {
        ws.close_workspace();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn workspace_current(state: State<'_, AppState>) -> Result<Option<WorkspaceInfo>, CommandError> {
    blocking(&state, |ws| Ok(ws.current())).await
}

#[tauri::command]
pub async fn workspace_recent(state: State<'_, AppState>) -> Result<Vec<RecentProject>, CommandError> {
    blocking(&state, |ws| Ok(ws.recent_projects())).await
}

#[tauri::command]
pub async fn workspace_forget_recent(
    state: State<'_, AppState>,
    path: String,
) -> Result<Vec<RecentProject>, CommandError> {
    blocking(&state, move |ws| ws.forget_recent_project(&path)).await
}

#[tauri::command]
pub async fn workspace_list_dir(
    state: State<'_, AppState>,
    path: Option<WorkspacePath>,
) -> Result<Vec<FileEntry>, CommandError> {
    blocking(&state, move |ws| ws.list_dir(path.as_ref())).await
}

#[tauri::command]
pub async fn document_open(state: State<'_, AppState>, path: WorkspacePath) -> Result<DocumentOpened, CommandError> {
    blocking(&state, move |ws| ws.open_document(&path)).await
}

#[tauri::command]
pub async fn document_close(state: State<'_, AppState>, document: DocumentId) -> Result<(), CommandError> {
    blocking(&state, move |ws| ws.close_document(&document)).await
}

#[tauri::command]
pub async fn document_save(
    state: State<'_, AppState>,
    document: DocumentId,
    overwrite: bool,
) -> Result<DocumentInfo, CommandError> {
    blocking(&state, move |ws| ws.save_document(&document, overwrite)).await
}

#[tauri::command]
pub async fn document_save_all(state: State<'_, AppState>) -> Result<SaveAllOutcome, CommandError> {
    blocking(&state, |ws| Ok(ws.save_all())).await
}

#[tauri::command]
pub async fn document_reload(state: State<'_, AppState>, document: DocumentId) -> Result<DocumentInfo, CommandError> {
    blocking(&state, move |ws| ws.reload_document(&document)).await
}

#[tauri::command]
pub async fn document_keep_buffer(state: State<'_, AppState>, document: DocumentId) -> Result<(), CommandError> {
    blocking(&state, move |ws| ws.keep_buffer(&document)).await
}

#[tauri::command]
pub async fn document_normalize_line_endings(
    state: State<'_, AppState>,
    document: DocumentId,
    line_ending: LineEnding,
) -> Result<Option<TransactionOutcome>, CommandError> {
    blocking(&state, move |ws| ws.normalize_line_endings(&document, line_ending)).await
}

#[tauri::command]
pub async fn recovery_list(state: State<'_, AppState>) -> Result<Vec<RecoveryEntry>, CommandError> {
    blocking(&state, |ws| ws.recovery_entries()).await
}

#[tauri::command]
pub async fn recovery_restore(state: State<'_, AppState>, id: String) -> Result<RestoreOutcome, CommandError> {
    blocking(&state, move |ws| ws.restore_recovery(&id)).await
}

#[tauri::command]
pub async fn recovery_discard(state: State<'_, AppState>, id: String) -> Result<Vec<RecoveryEntry>, CommandError> {
    blocking(&state, move |ws| ws.discard_recovery(&id)).await
}

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> Result<AppSettingsState, CommandError> {
    blocking(&state, |ws| Ok(ws.app_settings())).await
}

#[tauri::command]
pub async fn settings_update(state: State<'_, AppState>, settings: AppSettings) -> Result<AppSettings, CommandError> {
    blocking(&state, move |ws| ws.update_app_settings(&settings)).await
}

#[tauri::command]
pub async fn project_settings_get(state: State<'_, AppState>) -> Result<ProjectSettingsState, CommandError> {
    blocking(&state, |ws| ws.project_settings()).await
}

#[tauri::command]
pub async fn project_settings_update(
    state: State<'_, AppState>,
    settings: ProjectSettings,
) -> Result<ProjectSettings, CommandError> {
    blocking(&state, move |ws| ws.update_project_settings(&settings)).await
}

/// Start the workspace background loop (journals, watcher) and forward
/// workspace events to the webview.
pub fn start(app: AppHandle) {
    let service = Arc::clone(&app.state::<AppState>().workspace);
    let mut rx = service.subscribe();
    tauri::async_runtime::spawn(service.run());
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = app.emit(WORKSPACE_EVENT, &event);
                }
                Err(RecvError::Lagged(_)) => {
                    let _ = app.emit(
                        WORKSPACE_EVENT,
                        &exposition_workspace::types::WorkspaceEvent::FilesChanged {
                            changes: Vec::new(),
                            rescan: true,
                        },
                    );
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}
