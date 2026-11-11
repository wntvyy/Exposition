use std::path::PathBuf;
use std::sync::Arc;

use exposition_agents::credentials::KeyringStore;
use exposition_agents::tools::execution::ExecutionToolServices;
use exposition_agents::{AgentConfig, AgentService};
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_workspace::{DocumentService, WorkspaceService};
use tauri::{AppHandle, Manager};

/// Application-wide services. One instance is managed by Tauri.
pub struct AppState {
    pub documents: Arc<DocumentService>,
    /// Open folder, files, saves, recovery, watcher, and settings.
    pub workspace: Arc<WorkspaceService>,
    /// Supervised process execution (jobs, toolchains, trust).
    pub execution: ExecutionService,
    /// Application data directory (settings, recovery, session metadata).
    pub data_dir: PathBuf,
    /// Agent tasks (provider, task loop, review); task history in `agents.sqlite3`.
    pub agents: AgentService,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = resolve_data_dir(app)?;
        std::fs::create_dir_all(&data_dir)?;
        let documents = Arc::new(DocumentService::new());
        let mut execution_config = ExecutionConfig::new(&data_dir);
        // Test/dev override so isolated data dirs can share the extraction cache
        // of self-extracting tools (e.g. biber) instead of re-extracting.
        if let Some(dir) = std::env::var_os("EXPOSITION_TOOL_CACHE_DIR").filter(|v| !v.is_empty()) {
            execution_config.tool_cache_dir = Some(PathBuf::from(dir));
        }
        let execution = ExecutionService::with_config(execution_config)?;
        let workspace = WorkspaceService::new(Arc::clone(&documents), data_dir.clone());
        let agents = AgentService::new(AgentConfig {
            database: Some(data_dir.join("agents.sqlite3")),
            documents: Arc::clone(&documents),
            workspace: Some(Arc::clone(&workspace)),
            credentials: Arc::new(KeyringStore::new(credential_namespace().as_deref())),
            // Agent commands and Python run as jobs on the shared execution
            // service; builds and Lean checks report "not available" until the
            // typesetting and Lean services are integrated.
            services: Arc::new(ExecutionToolServices::new(execution.clone())),
            runtime: tauri::async_runtime::handle().inner().clone(),
            allow_scripted: cfg!(debug_assertions),
            anthropic_base_url: None,
            retry: None,
        })?;
        Ok(Self { documents, workspace, execution, data_dir, agents })
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

/// Development builds may isolate the credential entry (e2e runs) with
/// `EXPOSITION_CREDENTIAL_NAMESPACE`; release builds always use the real entry.
fn credential_namespace() -> Option<String> {
    #[cfg(debug_assertions)]
    {
        std::env::var("EXPOSITION_CREDENTIAL_NAMESPACE")
            .ok()
            .filter(|ns| !ns.is_empty() && ns.len() <= 64 && ns.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}
