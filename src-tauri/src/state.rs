use std::path::PathBuf;
use std::sync::Arc;

use exposition_agents::credentials::KeyringStore;
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::{AgentConfig, AgentService};
use exposition_workspace::DocumentService;
use tauri::{AppHandle, Manager};

/// Application-wide services. One instance is managed by Tauri.
pub struct AppState {
    pub documents: Arc<DocumentService>,
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
        let agents = AgentService::new(AgentConfig {
            database: Some(data_dir.join("agents.sqlite3")),
            documents: Arc::clone(&documents),
            credentials: Arc::new(KeyringStore::new(credential_namespace().as_deref())),
            // Builds, research sessions, Lean, and execution are wired here
            // when those services are integrated; until then agent process
            // tools report that they are unavailable.
            services: Arc::new(UnavailableServices),
            runtime: tauri::async_runtime::handle().inner().clone(),
            allow_scripted: cfg!(debug_assertions),
            anthropic_base_url: None,
            retry: None,
        })?;
        Ok(Self { documents, data_dir, agents })
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
