use std::path::PathBuf;
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::{ResearchConfig, ResearchService, ResolvedSource, SourceReader};
use exposition_workspace::{DocumentService, WorkspaceService};
use tauri::{AppHandle, Manager};

/// Application-wide services. One instance is managed by Tauri.
pub struct AppState {
    pub documents: Arc<DocumentService>,
    /// Open folder, files, saves, recovery, watcher, and settings.
    pub workspace: Arc<WorkspaceService>,
    /// Supervised process execution (jobs, toolchains, trust).
    pub execution: ExecutionService,
    /// Research sessions, runs, and provenance (M4). Launches through `execution`.
    pub research: Arc<ResearchService>,
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
        let workspace = WorkspaceService::new(Arc::clone(&documents), data_dir.clone());
        let mut research_config = ResearchConfig::new(
            data_dir.clone(),
            Arc::new(ExecutionLauncher::new(execution.clone())),
            documents.clone(),
        );
        research_config.source_reader = Some(Arc::new(WorkspaceSources(Arc::clone(&workspace))));
        let research = Arc::new(ResearchService::new(research_config));
        Ok(Self { documents, workspace, execution, research, data_dir })
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

/// Research reads run inputs through the workspace snapshot, like builds do.
struct WorkspaceSources(Arc<WorkspaceService>);

impl SourceReader for WorkspaceSources {
    fn read(&self, path: &WorkspacePath) -> Result<ResolvedSource, String> {
        let input = self.0.snapshot().and_then(|snapshot| snapshot.read(path)).map_err(|e| e.to_string())?;
        let text = input.text.ok_or_else(|| format!("{path} is not a text file"))?;
        Ok(ResolvedSource { text: text.to_string(), revision: input.revision, content_hash: input.content_hash })
    }
}
