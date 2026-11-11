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
    "typesetting_watch",
    "typesetting_unwatch",
    "typesetting_build",
    "typesetting_page_svg",
    "typesetting_export_pdf",
    "page_hit_test",
    "page_caret",
    "page_navigate",
    "page_selection",
    "page_edit",
];

/// Debug-build-only commands (granted by `capabilities/debug/`), removed at
/// integration once the workspace service opens projects.
const DEBUG_COMMANDS: &[&str] = &["dev_open_document"];

fn main() {
    let debug = std::env::var("PROFILE").as_deref() == Ok("debug");
    let commands: Vec<&str> = COMMANDS.iter().chain(if debug { DEBUG_COMMANDS } else { &[] }).copied().collect();
    println!("cargo:rerun-if-changed=capabilities");
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(Box::leak(commands.into_boxed_slice())))
        // Release builds load only the top-level capability files.
        .capabilities_path_pattern(if debug { "./capabilities/**/*.json" } else { "./capabilities/*.json" });
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
