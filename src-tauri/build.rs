//! Every application command must be listed here. Tauri generates an
//! `allow-<command>` permission for each, and only commands granted in
//! `capabilities/` are reachable from the webview.

const COMMANDS: &[&str] = &[
    "app_info",
    "document_list",
    "document_state",
    "document_apply",
    "document_undo",
    "document_redo",
    // Workspace, files, saves, recovery, settings (src/workspace.rs).
    "workspace_open",
    "workspace_close",
    "workspace_current",
    "workspace_recent",
    "workspace_forget_recent",
    "workspace_list_dir",
    "document_open",
    "document_close",
    "document_save",
    "document_save_all",
    "document_reload",
    "document_keep_buffer",
    "document_normalize_line_endings",
    "recovery_list",
    "recovery_restore",
    "recovery_discard",
    "settings_get",
    "settings_update",
    "project_settings_get",
    "project_settings_update",
    // Execution (src/execution.rs).
    "job_list",
    "job_output",
    "job_cancel",
    "toolchains_list",
    "toolchains_refresh",
    "trust_status",
    "trust_grant",
    "trust_revoke",
    "agents_provider_status",
    "agents_provider_update",
    "agents_provider_set_key",
    "agents_provider_clear_key",
    "agents_task_list",
    "agents_task_detail",
    "agents_task_create",
    "agents_task_continue",
    "agents_task_cancel",
    "agents_approval_resolve",
    "agents_review",
    "agents_review_diff",
    "agents_review_revert",
    "agents_review_accept",
    // Research (M4) and the format-owned artifact attachment command.
    "research_workspace",
    "research_environments",
    "research_session_start",
    "research_sessions",
    "research_session_interrupt",
    "research_session_restart",
    "research_session_shutdown",
    "research_execute",
    "research_run_cancel",
    "research_runs",
    "research_run",
    "research_run_output",
    "research_artifact",
    "artifact_attach",
];

/// Debug-build-only commands. They are compiled only with `debug_assertions`,
/// granted only by a capability added at runtime in debug builds, and are not
/// part of release `COMMANDS` or `capabilities/`.
const DEV_COMMANDS: &[&str] = &["dev_start_probe_job", "dev_start_workspace_job", "agents_dev_load_script"];

fn main() {
    let debug = std::env::var("PROFILE").as_deref() == Ok("debug");
    let commands: Vec<&str> = COMMANDS.iter().chain(if debug { DEV_COMMANDS } else { &[] }).copied().collect();
    // `AppManifest::commands` needs a `'static` slice.
    let commands: &'static [&'static str] = Box::leak(commands.into_boxed_slice());
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(commands)),
    )
    .expect("failed to run tauri-build");
}
