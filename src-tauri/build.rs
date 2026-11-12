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
    // Lean (src/lean.rs).
    "lean_documents",
    "lean_projects",
    "lean_goals",
    "lean_hover",
    "lean_definition",
    "lean_restart",
    "lean_restart_file",
    "lean_install_toolchain",
    "lean_check",
];

/// Debug-build-only commands. They are compiled only with `debug_assertions`,
/// granted only by a capability added at runtime in debug builds, and are not
/// part of release `COMMANDS` or `capabilities/`.
const DEV_COMMANDS: &[&str] = &["dev_start_probe_job", "dev_start_workspace_job"];

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
