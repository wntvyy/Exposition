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

/// One validated change to [`ExecutionSettings`] (the only way the UI changes
/// them). Deliberately narrow: container limits and the container user are not
/// changeable from the webview, so a compromised page cannot weaken isolation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExecutionSettingsChange {
    /// Set (`Some`, a Docker image reference already present locally) or clear
    /// the restricted-execution image.
    DockerImage { image: Option<String> },
    /// Set (`Some`, an absolute path to an existing executable) or clear
    /// (`None`, back to discovery) the location of a catalog tool.
    ToolPath { tool: String, path: Option<String> },
}

/// Execution settings plus what the settings UI may change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSettingsState {
    pub settings: ExecutionSettings,
    /// Tool ids whose location can be overridden (kernel components follow
    /// their interpreter and are not listed).
    pub overridable_tools: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("{0}")]
    InvalidImage(String),
    #[error("{0}")]
    InvalidToolPath(String),
    #[error("could not save execution settings: {0}")]
    Io(String),
}

impl SettingsError {
    /// Stable machine-readable code for IPC errors.
    pub fn code(&self) -> &'static str {
        match self {
            SettingsError::UnknownTool(_) => "settings.unknownTool",
            SettingsError::InvalidImage(_) => "settings.invalidImage",
            SettingsError::InvalidToolPath(_) => "settings.invalidToolPath",
            SettingsError::Io(_) => "settings.io",
        }
    }
}

/// Validate a tool location override: an absolute path without `..` to an
/// existing file (not a folder) that the execution service would run. On
/// Windows that means a `.exe`; `.bat`/`.cmd` (re-parsed by cmd.exe), `cmd.exe`
/// itself, and scripts such as `.ps1` are refused. Returns the trimmed path.
pub fn check_tool_override(raw: &str) -> Result<PathBuf, SettingsError> {
    let invalid = |message: String| Err(SettingsError::InvalidToolPath(message));
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return invalid("Enter the full path of the tool's executable.".into());
    }
    if trimmed.len() > 4096 || trimmed.contains('\0') {
        return invalid("That is not a valid file path.".into());
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return invalid(format!(
            "{trimmed} is not an absolute path. Enter the full path of the executable{}.",
            if cfg!(windows) { r", e.g. C:\Tools\bin\tool.exe" } else { ", e.g. /usr/local/bin/tool" }
        ));
    }
    if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return invalid(format!("{trimmed} may not contain `..`; enter the executable's direct path."));
    }
    #[cfg(windows)]
    {
        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if ext == "bat" || ext == "cmd" || name == "cmd.exe" {
            return invalid(format!(
                "{trimmed} is run by cmd.exe, which re-parses arguments as a shell command line. Choose the tool's real .exe instead."
            ));
        }
        if ext == "ps1" {
            return invalid(format!(
                "{trimmed} is a PowerShell script. Exposition runs programs directly, never through a shell; choose the tool's real .exe instead."
            ));
        }
        if ext != "exe" {
            return invalid(format!("{trimmed} is not a Windows executable (.exe)."));
        }
    }
    let Ok(link) = std::fs::symlink_metadata(&path) else {
        return invalid(format!("{trimmed} does not exist."));
    };
    if link.is_dir() || std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
        return invalid(format!("{trimmed} is a folder; choose the executable file inside it."));
    }
    if !crate::toolchains::is_executable_file(&path) {
        return invalid(format!("{trimmed} is not a file."));
    }
    Ok(path)
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

    fn refused(raw: &str) -> String {
        match check_tool_override(raw) {
            Err(SettingsError::InvalidToolPath(message)) => message,
            other => panic!("{raw:?} should be refused, got {other:?}"),
        }
    }

    #[test]
    fn tool_override_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join(if cfg!(windows) { "tool.exe" } else { "tool" });
        std::fs::write(&exe, "x").unwrap();
        assert_eq!(check_tool_override(&format!("  {}  ", exe.display())).unwrap(), exe);

        assert!(refused("").contains("full path"));
        assert!(refused("tool.exe").contains("not an absolute path"));
        assert!(refused("a\0b").contains("not a valid"));
        let missing = tmp.path().join(if cfg!(windows) { "missing.exe" } else { "missing" });
        assert!(refused(&missing.display().to_string()).contains("does not exist"));
        let dotdot =
            format!("{}{}..{}{}", tmp.path().display(), std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR, "x");
        assert!(refused(&dotdot).contains(".."));
        let folder = tmp.path().join(if cfg!(windows) { "dir.exe" } else { "dir" });
        std::fs::create_dir(&folder).unwrap();
        assert!(refused(&folder.display().to_string()).contains("folder"));

        #[cfg(windows)]
        {
            for (name, expect) in [
                ("run.cmd", "cmd.exe"),
                ("run.bat", "cmd.exe"),
                ("RUN.CMD", "cmd.exe"),
                ("script.ps1", "PowerShell"),
                ("script.py", "not a Windows executable"),
                ("noext", "not a Windows executable"),
            ] {
                let file = tmp.path().join(name);
                std::fs::write(&file, "x").unwrap();
                let message = refused(&file.display().to_string());
                assert!(message.contains(expect), "{name}: {message}");
            }
            let cmd = std::path::Path::new(&std::env::var("SystemRoot").unwrap()).join("System32").join("cmd.exe");
            assert!(refused(&cmd.display().to_string()).contains("cmd.exe"));
        }
    }

    #[test]
    fn settings_change_wire_format() {
        let change: ExecutionSettingsChange =
            serde_json::from_str(r#"{"kind":"toolPath","tool":"python","path":null}"#).unwrap();
        assert_eq!(change, ExecutionSettingsChange::ToolPath { tool: "python".into(), path: None });
        let change: ExecutionSettingsChange =
            serde_json::from_str(r#"{"kind":"dockerImage","image":"python:3.13-slim"}"#).unwrap();
        assert_eq!(change, ExecutionSettingsChange::DockerImage { image: Some("python:3.13-slim".into()) });
        // Container limits and the user cannot be changed through this type.
        assert!(serde_json::from_str::<ExecutionSettingsChange>(r#"{"kind":"dockerUser","user":"0:0"}"#).is_err());
    }
}
