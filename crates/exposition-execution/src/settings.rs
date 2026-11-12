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
    /// The settings file was invalid when the application started: what was
    /// wrong, that defaults replace the invalid values, and where the
    /// original file was kept.
    pub warning: Option<String>,
}

/// Smallest container memory limit Docker accepts.
const MIN_MEMORY_BYTES: u64 = 6 * 1024 * 1024;
const MAX_PIDS_LIMIT: u32 = 4_194_304;
const MAX_CONCURRENT_JOBS: u32 = 64;

/// `docker.memory`: a positive whole number with an optional unit (`b`, `k`,
/// `m`, `g`, also `kb`, `mb`, `gb`), at least 6 MiB, as `docker run
/// --memory` takes it.
pub fn validate_memory(value: &str) -> Result<(), String> {
    let invalid = || Err(format!("docker.memory {value:?} is not a memory size such as 2g or 512m"));
    let owned = value.to_ascii_lowercase();
    let lower = owned.strip_suffix('b').filter(|s| s.ends_with(['k', 'm', 'g'])).unwrap_or(owned.as_str());
    let (digits, unit) = match lower.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&lower[..i], c),
        _ => (lower, 'b'),
    };
    if digits.is_empty() || digits.len() > 15 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return invalid();
    }
    let scale: u64 = match unit {
        'b' => 1,
        'k' => 1 << 10,
        'm' => 1 << 20,
        'g' => 1 << 30,
        _ => return invalid(),
    };
    let Some(bytes) = digits.parse::<u64>().ok().and_then(|n| n.checked_mul(scale)) else { return invalid() };
    if bytes < MIN_MEMORY_BYTES {
        return Err(format!("docker.memory {value:?} is below Docker's minimum of 6m"));
    }
    Ok(())
}

/// `docker.cpus`: a decimal number of CPUs from 0.01 to 1024 (`--cpus`).
pub fn validate_cpus(value: &str) -> Result<(), String> {
    let invalid = || Err(format!("docker.cpus {value:?} is not a number of CPUs such as 2 or 0.5"));
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = |s: &str, max: usize| s.len() <= max && s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole, 4) || !digits(fraction, 2) || (value.contains('.') && fraction.is_empty()) {
        return invalid();
    }
    match value.parse::<f64>() {
        Ok(cpus) if (0.01..=1024.0).contains(&cpus) => Ok(()),
        _ => invalid(),
    }
}

/// `docker.pidsLimit`: 1 to 4194304 processes (0 would mean "unlimited").
pub fn validate_pids_limit(value: u32) -> Result<(), String> {
    if (1..=MAX_PIDS_LIMIT).contains(&value) {
        Ok(())
    } else {
        Err(format!("docker.pidsLimit {value} must be between 1 and {MAX_PIDS_LIMIT}"))
    }
}

/// `docker.user`: a numeric `uid:gid` that is not root. Names are refused
/// because the image decides what a name means (any name may be uid 0), and a
/// missing gid would run with the root group.
pub fn validate_user(value: &str) -> Result<(), String> {
    let id = |s: &str| {
        (!s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
    };
    match value.split_once(':').map(|(uid, gid)| (id(uid), id(gid))) {
        Some((Some(0), Some(_)) | (Some(_), Some(0))) => {
            Err(format!("docker.user {value:?} would run restricted containers as root"))
        }
        Some((Some(_), Some(_))) => Ok(()),
        _ => Err(format!("docker.user {value:?} must be a numeric non-root uid:gid such as 65534:65534")),
    }
}

/// `maxConcurrentJobs`: unset (automatic) or 1 to 64.
pub fn validate_max_concurrent_jobs(value: Option<u32>) -> Result<(), String> {
    match value {
        Some(n) if !(1..=MAX_CONCURRENT_JOBS).contains(&n) => {
            Err(format!("maxConcurrentJobs {n} must be between 1 and {MAX_CONCURRENT_JOBS}"))
        }
        _ => Ok(()),
    }
}

impl ExecutionSettings {
    /// Replace every invalid container limit, the container user, and the job
    /// limit with its default. Returns what was wrong (empty: all valid).
    /// These values become single `docker run` arguments, so nothing that
    /// was hand-edited into the file reaches a launch unchecked.
    pub fn sanitize(&mut self) -> Vec<String> {
        let defaults = DockerSettings::default();
        let mut problems = Vec::new();
        let docker = &mut self.docker;
        if let Err(problem) = validate_memory(&docker.memory) {
            problems.push(format!("{problem} (using {})", defaults.memory));
            docker.memory = defaults.memory;
        }
        if let Err(problem) = validate_cpus(&docker.cpus) {
            problems.push(format!("{problem} (using {})", defaults.cpus));
            docker.cpus = defaults.cpus;
        }
        if let Err(problem) = validate_pids_limit(docker.pids_limit) {
            problems.push(format!("{problem} (using {})", defaults.pids_limit));
            docker.pids_limit = defaults.pids_limit;
        }
        if let Err(problem) = validate_user(&docker.user) {
            problems.push(format!("{problem} (using {})", defaults.user));
            docker.user = defaults.user;
        }
        if let Err(problem) = validate_max_concurrent_jobs(self.max_concurrent_jobs) {
            problems.push(format!("{problem} (using the automatic limit)"));
            self.max_concurrent_jobs = None;
        }
        problems
    }
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
///
/// A file that cannot be read or parsed, or that holds invalid values, is
/// copied to `settings.json.invalid-<millis>` before anything overwrites it;
/// defaults replace what was invalid and [`Self::warning`] says so.
pub struct SettingsStore {
    path: Option<PathBuf>,
    current: RwLock<Current>,
    warning: Option<String>,
}

struct Current {
    settings: ExecutionSettings,
    /// The file on disk is an invalid original that could not be backed up
    /// yet: the next write backs it up first, or is refused.
    unsaved_original: bool,
}

impl SettingsStore {
    pub fn load(path: PathBuf) -> Self {
        let (mut settings, mut problems) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<ExecutionSettings>(&bytes) {
                Ok(settings) => (settings, Vec::new()),
                Err(e) => (ExecutionSettings::default(), vec![format!("the file is not valid ({e}); using defaults")]),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (ExecutionSettings::default(), Vec::new()),
            Err(e) => (ExecutionSettings::default(), vec![format!("the file could not be read ({e}); using defaults")]),
        };
        problems.extend(settings.sanitize());
        let mut unsaved_original = false;
        let warning = (!problems.is_empty()).then(|| {
            let kept = match backup_invalid(&path) {
                Ok(backup) => {
                    // Store the corrected settings so the warning is shown once.
                    let _ = write_json_atomic(&path, &settings);
                    format!("The original file was kept as {}.", backup.display())
                }
                Err(e) => {
                    unsaved_original = true;
                    format!("The original file could not be backed up ({e}); it stays unchanged until it can be.")
                }
            };
            format!("Execution settings in {}: {}. {kept}", path.display(), problems.join("; "))
        });
        Self { path: Some(path), current: RwLock::new(Current { settings, unsaved_original }), warning }
    }

    /// Settings that are never persisted (tests).
    pub fn in_memory(settings: ExecutionSettings) -> Self {
        Self { path: None, current: RwLock::new(Current { settings, unsaved_original: false }), warning: None }
    }

    pub fn get(&self) -> ExecutionSettings {
        self.current.read().expect("settings lock").settings.clone()
    }

    /// Why the settings file was set aside at load, if it was.
    pub fn warning(&self) -> Option<String> {
        self.warning.clone()
    }

    pub fn update(&self, change: impl FnOnce(&mut ExecutionSettings)) -> std::io::Result<ExecutionSettings> {
        let mut guard = self.current.write().expect("settings lock");
        let mut next = guard.settings.clone();
        change(&mut next);
        if let Some(path) = &self.path {
            if guard.unsaved_original {
                backup_invalid(path).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("the invalid settings file could not be backed up before replacing it ({e})"),
                    )
                })?;
                guard.unsaved_original = false;
            }
            write_json_atomic(path, &next)?;
        }
        guard.settings = next.clone();
        Ok(next)
    }
}

/// Copy an invalid settings file to `<name>.invalid-<millis>` beside it.
fn backup_invalid(path: &Path) -> std::io::Result<PathBuf> {
    let millis =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let backup = path.with_file_name(format!("{name}.invalid-{millis}"));
    std::fs::copy(path, &backup)?;
    Ok(backup)
}

/// Write JSON to a sibling temp file, flush it to disk, then rename over the
/// target, so a crash leaves either the old or the new file, never a torn one.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()
    })();
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    if let Some(dir) = path.parent().and_then(|p| std::fs::File::open(p).ok()) {
        let _ = dir.sync_all();
    }
    Ok(())
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

    fn backups(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("settings.json.invalid-"))
            .collect();
        found.sort();
        found
    }

    #[test]
    fn unparsable_file_is_backed_up_before_anything_overwrites_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        let original = r#"{"docker":{"image":"python:3.13-slim",}"#; // trailing comma
        std::fs::write(&path, original).unwrap();
        let store = SettingsStore::load(path.clone());
        assert_eq!(store.get(), ExecutionSettings::default());
        let warning = store.warning().expect("a warning names the problem");
        let saved = backups(tmp.path());
        assert_eq!(saved.len(), 1, "{warning}");
        assert_eq!(std::fs::read_to_string(&saved[0]).unwrap(), original, "the original is kept byte for byte");
        assert!(warning.contains("not valid") && warning.contains(&saved[0].display().to_string()), "{warning}");
        // The next update overwrites settings.json; the backup is untouched.
        store.update(|s| s.docker.image = Some("node:26".into())).unwrap();
        assert_eq!(std::fs::read_to_string(&saved[0]).unwrap(), original);
        // A clean file loads without a warning or another backup.
        let reloaded = SettingsStore::load(path);
        assert_eq!(reloaded.warning(), None);
        assert_eq!(reloaded.get().docker.image.as_deref(), Some("node:26"));
        assert_eq!(backups(tmp.path()).len(), 1);
    }

    #[test]
    fn hand_edited_limits_and_user_are_validated_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        let original = r#"{"toolPaths":{"python":"C:/py/python.exe"},"docker":{"image":"python:3.13-slim",
            "memory":"2g --privileged","cpus":"all","pidsLimit":0,"user":"0:0"},"maxConcurrentJobs":0}"#;
        std::fs::write(&path, original).unwrap();
        let store = SettingsStore::load(path.clone());
        let settings = store.get();
        assert_eq!(settings.docker, DockerSettings { image: Some("python:3.13-slim".into()), ..Default::default() });
        assert_eq!(settings.max_concurrent_jobs, None);
        assert_eq!(
            settings.tool_paths.get("python").map(String::as_str),
            Some("C:/py/python.exe"),
            "valid values stay"
        );
        let warning = store.warning().unwrap();
        for part in ["docker.memory", "docker.cpus", "docker.pidsLimit", "as root", "maxConcurrentJobs"] {
            assert!(warning.contains(part), "{part}: {warning}");
        }
        let saved = backups(tmp.path());
        assert_eq!(std::fs::read_to_string(&saved[0]).unwrap(), original);
        // The corrected settings were stored: the next start is quiet.
        let reloaded = SettingsStore::load(path);
        assert_eq!(reloaded.warning(), None);
        assert_eq!(reloaded.get().docker.user, "65534:65534");

        // A user that is not a numeric non-root uid:gid never reaches --user.
        for user in
            ["root", "root:root", "0", "1000", "0:1000", "1000:0", "00:00", "1000:abc", "", "1000:1000 --privileged"]
        {
            let mut s = ExecutionSettings::default();
            s.docker.user = user.into();
            assert_eq!(s.sanitize().len(), 1, "{user:?}");
            assert_eq!(s.docker.user, "65534:65534");
        }
        assert!(validate_user("1000:1000").is_ok());
    }

    #[test]
    fn container_limit_formats() {
        for ok in ["2g", "512m", "6m", "1G", "4gb", "8192k", "6291456"] {
            assert!(validate_memory(ok).is_ok(), "{ok}");
        }
        for bad in ["", "2", "5m", "2t", "-2g", "2.5g", "g", "2g ", "2 g", "2gib", "2g;", "99999999999999999g"] {
            assert!(validate_memory(bad).is_err(), "{bad}");
        }
        for ok in ["2", "0.5", "0.01", "16", "1.25", "1024"] {
            assert!(validate_cpus(ok).is_ok(), "{ok}");
        }
        for bad in ["", "0", "0.001", "1025", ".5", "2.", "-1", "1e3", "two", "2 "] {
            assert!(validate_cpus(bad).is_err(), "{bad}");
        }
        assert!(validate_pids_limit(0).is_err());
        assert!(validate_pids_limit(256).is_ok());
        assert!(validate_max_concurrent_jobs(Some(0)).is_err());
        assert!(validate_max_concurrent_jobs(Some(65)).is_err());
        assert!(validate_max_concurrent_jobs(None).is_ok());
        assert!(ExecutionSettings::default().sanitize().is_empty(), "defaults are valid");
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
