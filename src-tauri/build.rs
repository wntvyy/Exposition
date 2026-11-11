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
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
