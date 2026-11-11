//! Exposition application wiring: state, command registration, and event
//! forwarding. Domain logic lives in the `crates/` services; this crate adapts
//! them to Tauri commands and events.

mod app;
mod documents;
mod error;
mod research;
mod state;

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
            research::forward_events(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::app_info,
            documents::document_list,
            documents::document_state,
            documents::document_apply,
            documents::document_undo,
            documents::document_redo,
            research::research_workspace_set,
            research::research_workspace,
            research::research_environments,
            research::research_session_start,
            research::research_sessions,
            research::research_session_interrupt,
            research::research_session_restart,
            research::research_session_shutdown,
            research::research_execute,
            research::research_run_cancel,
            research::research_runs,
            research::research_run,
            research::research_run_output,
            research::research_artifact,
            research::artifact_attach,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Exposition");
}
