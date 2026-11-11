//! Exposition application wiring: state, command registration, and event
//! forwarding. Domain logic lives in the `crates/` services; this crate adapts
//! them to Tauri commands and events.

mod app;
mod documents;
mod error;
mod execution;
mod state;

use tauri::{Manager, RunEvent};

pub use error::CommandError;
pub use state::AppState;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = AppState::initialize(app.handle())?;
            app.manage(state);
            documents::forward_events(app.handle().clone());
            execution::forward_events(app.handle().clone());
            #[cfg(debug_assertions)]
            execution::add_dev_capability(app.handle())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app::app_info,
            documents::document_list,
            documents::document_state,
            documents::document_apply,
            documents::document_undo,
            documents::document_redo,
            execution::job_list,
            execution::job_output,
            execution::job_cancel,
            execution::toolchains_list,
            execution::toolchains_refresh,
            execution::trust_status,
            execution::trust_grant,
            execution::trust_revoke,
            #[cfg(debug_assertions)]
            execution::dev_start_probe_job,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Exposition")
        .run(|app, event| {
            // Kill every supervised process tree before exiting. (If the app
            // crashes instead, job-object kill-on-close does the same.)
            if let RunEvent::Exit = event {
                app.state::<AppState>().execution.shutdown();
            }
        });
}
