use std::path::PathBuf;
use std::sync::Arc;

use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_tex::{TexService, TexServiceConfig};
use exposition_workspace::DocumentService;
use tauri::{AppHandle, Manager};

/// Application-wide services. One instance is managed by Tauri.
pub struct AppState {
    pub documents: Arc<DocumentService>,
    /// Supervised process execution (jobs, toolchains, trust).
    pub execution: ExecutionService,
    /// LaTeX/LuaLaTeX builds (through `execution`) and their pages.
    pub tex: TexService,
    /// Application data directory (settings, recovery, session metadata).
    pub data_dir: PathBuf,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = resolve_data_dir(app)?;
        std::fs::create_dir_all(&data_dir)?;
        let mut execution_config = ExecutionConfig::new(&data_dir);
        // Test/dev override so isolated data dirs can share the extraction cache
        // of self-extracting tools (e.g. biber) instead of re-extracting.
        if let Some(dir) = std::env::var_os("EXPOSITION_TOOL_CACHE_DIR").filter(|v| !v.is_empty()) {
            execution_config.tool_cache_dir = Some(PathBuf::from(dir));
        }
        let execution = ExecutionService::with_config(execution_config)?;
        let documents = Arc::new(DocumentService::new());
        let tex = TexService::new(
            execution.clone(),
            documents.clone(),
            tauri::async_runtime::handle().inner().clone(),
            TexServiceConfig::new(&data_dir),
        )?;
        Ok(Self { documents, execution, tex, data_dir })
    }
}

/// `EXPOSITION_DATA_DIR` overrides the platform application data directory so
/// tests and side-by-side profiles never touch the user's real state.
fn resolve_data_dir(app: &AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dir) = std::env::var_os("EXPOSITION_DATA_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    Ok(app.path().app_local_data_dir()?)
}
