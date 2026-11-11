//! Exposition application wiring: state, command registration, and event
//! forwarding. Domain logic lives in the `crates/` services; this crate adapts
//! them to Tauri commands and events.

mod agents;
mod app;
mod documents;
mod error;
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
            agents::forward_events(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::app_info,
            documents::document_list,
            documents::document_state,
            documents::document_apply,
            documents::document_undo,
            documents::document_redo,
            agents::agents_provider_status,
            agents::agents_provider_update,
            agents::agents_provider_set_key,
            agents::agents_provider_clear_key,
            agents::agents_task_list,
            agents::agents_task_detail,
            agents::agents_task_create,
            agents::agents_task_continue,
            agents::agents_task_cancel,
            agents::agents_approval_resolve,
            agents::agents_review,
            agents::agents_review_diff,
            agents::agents_review_revert,
            agents::agents_review_accept,
            agents::agents_dev_load_script,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Exposition");
}
