//! Minimal typed execution settings persisted in application data.
//!
//! These are application-level settings only. Project files can never set tool
//! paths or container images: a project that could point the toolchain at its
//! own executable would execute on open.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct DockerSettings {
    /// Image for `Restricted { backend: "docker" }` jobs. Must already be present
    /// locally (`docker image inspect`); Exposition never pulls images.
    pub image: Option<String>,
    pub memory: String,
    pub cpus: String,
    pub pids_limit: u32,
    /// Non-root `uid:gid` the container process runs as.
    pub user: String,
}

impl Default for DockerSettings {
    fn default() -> Self {
        Self { image: None, memory: "2g".into(), cpus: "2".into(), pids_limit: 256, user: "65534:65534".into() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct ExecutionSettings {
    /// Tool id → absolute executable path, overriding discovery.
    pub tool_paths: BTreeMap<String, String>,
    pub docker: DockerSettings,
    /// Concurrent batch jobs; `None` picks a default from the CPU count.
    pub max_concurrent_jobs: Option<u32>,
}

/// Load/save of [`ExecutionSettings`] (`<data>/execution/settings.json`).
pub struct SettingsStore {
    path: Option<PathBuf>,
    current: RwLock<ExecutionSettings>,
}

impl SettingsStore {
    pub fn load(path: PathBuf) -> Self {
        let current = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ExecutionSettings>(&bytes).ok())
            .unwrap_or_default();
        Self { path: Some(path), current: RwLock::new(current) }
    }

    /// Settings that are never persisted (tests).
    pub fn in_memory(settings: ExecutionSettings) -> Self {
        Self { path: None, current: RwLock::new(settings) }
    }

    pub fn get(&self) -> ExecutionSettings {
        self.current.read().expect("settings lock").clone()
    }

    pub fn update(&self, change: impl FnOnce(&mut ExecutionSettings)) -> std::io::Result<ExecutionSettings> {
        let mut guard = self.current.write().expect("settings lock");
        let mut next = guard.clone();
        change(&mut next);
        if let Some(path) = &self.path {
            write_json_atomic(path, &next)?;
        }
        *guard = next.clone();
        Ok(next)
    }
}

/// Write JSON to a sibling temp file, then rename over the target.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_tolerate_missing_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("execution").join("settings.json");
        let store = SettingsStore::load(path.clone());
        assert_eq!(store.get(), ExecutionSettings::default());
        store
            .update(|s| {
                s.docker.image = Some("node:26-trixie-slim".into());
                s.tool_paths.insert("python".into(), "C:/py/python.exe".into());
            })
            .unwrap();
        let reloaded = SettingsStore::load(path.clone());
        assert_eq!(reloaded.get().docker.image.as_deref(), Some("node:26-trixie-slim"));
        assert_eq!(reloaded.get().docker.pids_limit, 256);
        std::fs::write(&path, r#"{"docker":{"image":"x"}}"#).unwrap();
        let partial = SettingsStore::load(path);
        assert_eq!(partial.get().docker.user, "65534:65534");
        assert_eq!(partial.get().docker.image.as_deref(), Some("x"));
    }
}
