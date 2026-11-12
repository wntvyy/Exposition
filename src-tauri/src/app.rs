use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::state::AppState;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub arch: String,
    pub debug_build: bool,
    pub data_dir: String,
    /// The Typst compiler built into the application (e.g. "typst 0.15.1").
    pub typst_compiler: String,
    /// The Markdown print profile (Markdown is laid out through Typst).
    pub markdown_profile: String,
}

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo, crate::CommandError> {
    Ok(AppInfo {
        name: "Exposition".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        debug_build: cfg!(debug_assertions),
        data_dir: state.data_dir.display().to_string(),
        typst_compiler: exposition_typesetting::typst_adapter::COMPILER.into(),
        markdown_profile: exposition_typesetting::markdown::PROFILE_ID.into(),
    })
}
