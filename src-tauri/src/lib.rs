//! Exposition application wiring: state, command registration, and event
//! forwarding. Domain logic lives in the `crates/` services; this crate adapts
//! them to Tauri commands and events.

mod agents;
mod app;
mod documents;
mod error;
mod execution;
mod graph;
mod lean;
mod page;
mod projects;
mod research;
mod settings;
mod state;
mod tex;
mod typesetting;
mod workflow_repro;
mod workflow_runs;
mod workflow_triggers;
mod workflows;
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
            execution::forward_trust(app.handle().clone());
            lean::forward_events(app.handle().clone());
            tex::forward_events(app.handle().clone());
            agents::forward_events(app.handle().clone());
            #[cfg(debug_assertions)]
            execution::add_dev_capability(app.handle())?;
            #[cfg(debug_assertions)]
            agents::add_dev_capability(app.handle())?;
            #[cfg(debug_assertions)]
            research::add_dev_capability(app.handle())?;
            research::forward_events(app.handle().clone());
            graph::forward_events(app.handle().clone());
            workflow_runs::forward_events(app.handle().clone());
            workflow_triggers::start(app.handle().clone());
            app.state::<AppState>().typesetting.start(app.handle().clone());
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
            settings::execution_settings_get,
            settings::execution_settings_update,
            settings::docker_images_list,
            settings::trust_list,
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
            typesetting::typesetting_watch,
            typesetting::typesetting_unwatch,
            typesetting::typesetting_build,
            page::page_image,
            typesetting::typesetting_export_pdf,
            page::page_hit_test,
            page::page_caret,
            page::page_navigate,
            page::page_selection,
            page::page_edit,
            graph::graph_status,
            graph::graph_vocabulary,
            graph::graph_snapshot,
            graph::graph_entity,
            graph::graph_inspect_at_source,
            graph::graph_impact,
            graph::graph_math_overview,
            graph::graph_propose_change,
            graph::graph_apply_change,
            graph::graph_views,
            graph::graph_view_get,
            graph::graph_view_save,
            graph::graph_view_delete,
            graph::graph_recover,
            graph::graph_operations,
            research::research_space,
            research::research_space_enable,
            research::research_listing,
            research::research_space_create_file,
            research::research_space_import,
            research::research_retain,
            research::research_result_state,
            research::research_experiment_save,
            research::research_experiment,
            research::research_promotion_prepare,
            research::research_promotion_approve,
            research::research_promotion_cancel,
            research::research_promotion_preview,
            research::research_promotions,
            research::research_promotion_reconcile,
            #[cfg(debug_assertions)]
            research::research_dev_fail_next_graph_write,
            lean::lean_audit_record,
            lean::lean_record_evidence,
            lean::lean_associate_declaration,
            lean::lean_declaration_evidence,
            graph::graph_reconcile,
            graph::graph_backup,
            graph::graph_backups,
            projects::project_presets,
            projects::project_capabilities,
            projects::project_preset_blueprint,
            projects::project_blueprint_validate,
            projects::project_check_destination,
            projects::project_create,
            projects::project_creation_status,
            projects::project_creation_resolve,
            workflows::workflow_operations,
            workflows::workflow_presets,
            workflows::workflow_from_preset,
            workflows::workflow_validate,
            workflows::workflow_save,
            workflows::workflow_import,
            workflows::workflow_list,
            workflows::workflow_get,
            workflows::workflow_bundle_export,
            workflows::workflow_bundle_import,
            workflow_runs::workflow_run_plan,
            workflow_runs::workflow_run_start,
            workflow_runs::workflow_run_cancel,
            workflow_runs::workflow_run_get,
            workflow_runs::workflow_runs,
            workflow_runs::workflow_run_acknowledge,
            workflow_runs::workflow_association_decide,
            workflow_repro::workflow_run_manifest,
            workflow_repro::workflow_run_manifest_export,
            workflow_repro::workflow_run_reproduce_plan,
            agents::agents_task_keep_investigation,
            agents::agents_task_create_generation,
            workflow_triggers::workflow_trigger_plan,
            workflow_triggers::workflow_trigger_enable,
            workflow_triggers::workflow_trigger_disable,
            workflow_triggers::workflow_trigger_list,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Exposition")
        .run(|app, event| {
            // Kill every supervised process tree before exiting. (If the app
            // crashes instead, job-object kill-on-close does the same.)
            if let RunEvent::Exit = event {
                // An active workflow run stops with the app (its jobs are
                // killed below); the next start marks it interrupted and
                // never resumes or repeats it.
                app.state::<AppState>().tex.shutdown();
                app.state::<AppState>().execution.shutdown();
                app.state::<AppState>().reconciler.shutdown();
            }
        });
}
