//! Exposition application wiring: state, command registration, and event
//! forwarding. Domain logic lives in the `crates/` services; this crate adapts
//! them to Tauri commands and events.

mod app;
mod documents;
mod error;
mod page;
mod state;
mod typesetting;

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
            typesetting::typesetting_watch,
            typesetting::typesetting_unwatch,
            typesetting::typesetting_build,
            typesetting::typesetting_page_svg,
            typesetting::typesetting_export_pdf,
            page::page_hit_test,
            page::page_caret,
            page::page_navigate,
            page::page_selection,
            page::page_edit,
            #[cfg(debug_assertions)]
            page::dev_open_document,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Exposition");
}
