//! Exposition application wiring: state, command registration, and event
//! forwarding. Domain logic lives in the `crates/` services; this crate adapts
//! them to Tauri commands and events.

mod app;
mod documents;
mod error;
mod state;
mod workspace;

use tauri::Manager;

pub use error::CommandError;
pub use state::AppState;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = AppState::initialize(app.handle())?;
            app.manage(state);
            documents::forward_events(app.handle().clone());
            workspace::start(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::app_info,
            documents::document_list,
            documents::document_state,
            documents::document_apply,
            documents::document_undo,
            documents::document_redo,
            workspace::workspace_open,
            workspace::workspace_close,
            workspace::workspace_current,
            workspace::workspace_recent,
            workspace::workspace_forget_recent,
            workspace::workspace_list_dir,
            workspace::document_open,
            workspace::document_close,
            workspace::document_save,
            workspace::document_save_all,
            workspace::document_reload,
            workspace::document_keep_buffer,
            workspace::document_normalize_line_endings,
            workspace::recovery_list,
            workspace::recovery_restore,
            workspace::recovery_discard,
            workspace::settings_get,
            workspace::settings_update,
            workspace::project_settings_get,
            workspace::project_settings_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Exposition");
}
