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
    "agents_dev_load_script",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
