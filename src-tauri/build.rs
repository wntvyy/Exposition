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
    // Research (M4) and the format-owned artifact attachment command.
    "research_workspace_set",
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

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
