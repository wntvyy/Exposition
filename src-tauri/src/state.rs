use std::path::PathBuf;
use std::sync::Arc;

use exposition_typesetting::{Fonts, TypesettingService};
use exposition_workspace::DocumentService;
use tauri::{AppHandle, Manager};

use crate::typesetting::TypesettingRuntime;

/// Application-wide services. One instance is managed by Tauri.
pub struct AppState {
    pub documents: Arc<DocumentService>,
    /// Typesetting builds, page maps, and rendering (Typst).
    pub typesetting: Arc<TypesettingRuntime>,
    /// Application data directory (settings, recovery, session metadata).
    pub data_dir: PathBuf,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = resolve_data_dir(app)?;
        std::fs::create_dir_all(&data_dir)?;
        let documents = Arc::new(DocumentService::new());
        let typesetting =
            TypesettingRuntime::new(Arc::new(TypesettingService::new(Arc::clone(&documents), Fonts::with_system)));
        // Discover fonts in the background so the first build does not wait for it.
        std::thread::spawn(|| {
            Fonts::with_system();
        });
        Ok(Self { documents, typesetting, data_dir })
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
