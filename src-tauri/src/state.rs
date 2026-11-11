use std::path::PathBuf;
use std::sync::Arc;

use exposition_research::launcher::StdProcessLauncher;
use exposition_research::{ResearchConfig, ResearchService};
use exposition_workspace::DocumentService;
use tauri::{AppHandle, Manager};

/// Application-wide services. One instance is managed by Tauri.
pub struct AppState {
    pub documents: Arc<DocumentService>,
    /// Research sessions, runs, and provenance (M4).
    pub research: Arc<ResearchService>,
    /// Application data directory (settings, recovery, session metadata).
    pub data_dir: PathBuf,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = resolve_data_dir(app)?;
        std::fs::create_dir_all(&data_dir)?;
        let documents = Arc::new(DocumentService::new());
        // TEMPORARY launcher until the shared ExecutionService offers interactive jobs.
        let research = Arc::new(ResearchService::new(ResearchConfig::new(
            data_dir.clone(),
            Arc::new(StdProcessLauncher),
            documents.clone(),
        )));
        Ok(Self { documents, research, data_dir })
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
