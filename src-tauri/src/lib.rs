//! Exposition application wiring: state, command registration, and event
//! forwarding. Domain logic lives in the `crates/` services; this crate adapts
//! them to Tauri commands and events.

mod agents;
mod app;
mod documents;
mod error;
mod execution;
mod lean;
mod research;
mod state;
mod tex;
mod workspace;

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
            workspace::start(app.handle().clone());
            execution::forward_events(app.handle().clone());
            lean::forward_events(app.handle().clone());
            tex::forward_events(app.handle().clone());
            agents::forward_events(app.handle().clone());
            #[cfg(debug_assertions)]
            execution::add_dev_capability(app.handle())?;
            #[cfg(debug_assertions)]
            agents::add_dev_capability(app.handle())?;
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
            execution::job_list,
            execution::job_output,
            execution::job_cancel,
            execution::toolchains_list,
            execution::toolchains_refresh,
            execution::trust_status,
            execution::trust_grant,
            execution::trust_revoke,
            lean::lean_documents,
            lean::lean_projects,
            lean::lean_goals,
            lean::lean_hover,
            lean::lean_definition,
            lean::lean_restart,
            lean::lean_restart_file,
            lean::lean_install_toolchain,
            lean::lean_check,
            tex::tex_build,
            tex::tex_status,
            tex::tex_cancel,
            tex::tex_page_image,
            tex::tex_page_map,
            #[cfg(debug_assertions)]
            execution::dev_start_probe_job,
            #[cfg(debug_assertions)]
            execution::dev_start_workspace_job,
            #[cfg(debug_assertions)]
            agents::agents_dev_load_script,
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
        .build(tauri::generate_context!())
        .expect("error while building Exposition")
        .run(|app, event| {
            // Kill every supervised process tree before exiting. (If the app
            // crashes instead, job-object kill-on-close does the same.)
            if let RunEvent::Exit = event {
                app.state::<AppState>().tex.shutdown();
                app.state::<AppState>().execution.shutdown();
            }
        });
}
