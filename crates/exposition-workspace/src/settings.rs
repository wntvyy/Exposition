//! Typed, versioned settings.
//!
//! - Application settings: `<data dir>/settings.json`.
//! - Recent projects: `<data dir>/recent-projects.json`.
//! - Project settings: `<project>/.exposition/project.json` (portable, `{ "version": 1, ... }`).
//!
//! Every file is validated on read and written atomically. A missing file means
//! defaults; an unreadable, invalid, or newer-version file is preserved under a
//! `.invalid-<millis>` name (never silently overwritten) and defaults are used.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use exposition_core::build::{DocumentFormat, Engine};
use exposition_core::path::WorkspacePath;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{WorkspaceError, WorkspaceResult};
use crate::fsutil::{now_millis, write_atomic};
use crate::types::RecentProject;

pub const APP_SETTINGS_VERSION: u32 = 1;
pub const PROJECT_SETTINGS_VERSION: u32 = 1;
const MAX_RECENT: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum NewFileEol {
    Lf,
    Crlf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct EditorSettings {
    /// Source editor font size in CSS pixels (8–40).
    pub font_size: u32,
    /// Spaces per indentation level (1–16).
    pub tab_size: u32,
    pub insert_spaces: bool,
    pub word_wrap: bool,
    pub line_numbers: bool,
    pub minimap: bool,
    /// Line ending typed into documents that have none yet.
    pub default_eol: NewFileEol,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: 13,
            tab_size: 2,
            insert_spaces: true,
            word_wrap: true,
            line_numbers: true,
            minimap: false,
            default_eol: NewFileEol::Lf,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct FilesSettings {
    /// Reopen the most recent project at startup.
    pub reopen_last_project: bool,
}

/// Application settings (`settings.json` in the application data directory).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub version: u32,
    pub editor: EditorSettings,
    pub files: FilesSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { version: APP_SETTINGS_VERSION, editor: EditorSettings::default(), files: FilesSettings::default() }
    }
}

impl AppSettings {
    pub fn validate(&self) -> WorkspaceResult<()> {
        let invalid = |m: String| Err(WorkspaceError::new("settings.invalid", m));
        if self.version != APP_SETTINGS_VERSION {
            return invalid(format!(
                "Unsupported settings version {} (expected {APP_SETTINGS_VERSION}).",
                self.version
            ));
        }
        if !(8..=40).contains(&self.editor.font_size) {
            return invalid(format!("Editor font size must be 8–40 (got {}).", self.editor.font_size));
        }
        if !(1..=16).contains(&self.editor.tab_size) {
            return invalid(format!("Tab size must be 1–16 (got {}).", self.editor.tab_size));
        }
        Ok(())
    }
}

/// A named build target in project settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTarget {
    pub name: String,
    pub format: DocumentFormat,
    pub engine: Engine,
    pub entry: WorkspacePath,
}

/// Portable project settings (`.exposition/project.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettings {
    pub version: u32,
    /// Main document of the project.
    #[serde(default)]
    pub entry: Option<WorkspacePath>,
    #[serde(default)]
    pub targets: Vec<ProjectTarget>,
    /// Name of the target built by default.
    #[serde(default)]
    pub default_target: Option<String>,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self { version: PROJECT_SETTINGS_VERSION, entry: None, targets: Vec::new(), default_target: None }
    }
}

impl ProjectSettings {
    pub fn validate(&self) -> WorkspaceResult<()> {
        let invalid = |m: String| Err(WorkspaceError::new("settings.invalid", m));
        if self.version != PROJECT_SETTINGS_VERSION {
            return invalid(format!(
                "Unsupported project settings version {} (expected {PROJECT_SETTINGS_VERSION}).",
                self.version
            ));
        }
        let mut names = Vec::new();
        for target in &self.targets {
            let name = target.name.trim();
            if name.is_empty() || name.len() > 64 {
                return invalid("Build target names must be 1–64 characters.".into());
            }
            if names.contains(&name) {
                return invalid(format!("Duplicate build target name {name:?}."));
            }
            names.push(name);
            let compatible = matches!(
                (target.format, target.engine),
                (DocumentFormat::Typst, Engine::Typst)
                    | (DocumentFormat::Latex, Engine::PdfLatex | Engine::LuaLatex)
                    | (DocumentFormat::Markdown, Engine::MarkdownPrint)
            );
            if !compatible {
                return invalid(format!("Target {name:?} uses an engine that cannot build its format."));
            }
        }
        if let Some(default) = &self.default_target
            && !names.contains(&default.as_str())
        {
            return invalid(format!("Default target {default:?} is not defined."));
        }
        Ok(())
    }
}

/// Outcome of loading a settings file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<T> {
    pub value: T,
    /// The file existed.
    pub exists: bool,
    /// Why the file was set aside, if it was invalid.
    pub warning: Option<String>,
}

trait Validate {
    fn check(&self) -> WorkspaceResult<()>;
}
impl Validate for AppSettings {
    fn check(&self) -> WorkspaceResult<()> {
        self.validate()
    }
}
impl Validate for ProjectSettings {
    fn check(&self) -> WorkspaceResult<()> {
        self.validate()
    }
}
impl Validate for RecentFile {
    fn check(&self) -> WorkspaceResult<()> {
        if self.version == 1 { Ok(()) } else { Err(WorkspaceError::new("settings.invalid", "unsupported version")) }
    }
}

fn load<T: DeserializeOwned + Default + Validate>(file: &Path) -> Loaded<T> {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Loaded { value: T::default(), exists: false, warning: None };
        }
        Err(e) => {
            return Loaded { value: T::default(), exists: true, warning: Some(format!("could not be read ({e})")) };
        }
    };
    let problem = match serde_json::from_slice::<T>(&bytes) {
        Ok(value) => match value.check() {
            Ok(()) => return Loaded { value, exists: true, warning: None },
            Err(e) => e.message,
        },
        Err(e) => format!("is not valid ({e})"),
    };
    // Keep the original for the user; never overwrite it silently.
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let backup = file.with_file_name(format!("{name}.invalid-{}", now_millis()));
    let _ = fs::copy(file, &backup);
    let _ = fs::remove_file(file);
    Loaded { value: T::default(), exists: true, warning: Some(problem) }
}

fn store<T: Serialize>(file: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    json.push(b'\n');
    write_atomic(file, &json)
}

pub fn app_settings_file(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

pub fn load_app_settings(data_dir: &Path) -> Loaded<AppSettings> {
    load(&app_settings_file(data_dir))
}

pub fn store_app_settings(data_dir: &Path, settings: &AppSettings) -> WorkspaceResult<()> {
    settings.validate()?;
    store(&app_settings_file(data_dir), settings)
        .map_err(|e| WorkspaceError::new("settings.writeFailed", format!("Settings could not be saved: {e}.")))
}

/// Load project settings from an already resolved `.exposition/project.json`.
pub fn load_project_settings(file: &Path) -> Loaded<ProjectSettings> {
    load(file)
}

pub fn store_project_settings(file: &Path, settings: &ProjectSettings) -> WorkspaceResult<()> {
    settings.validate()?;
    store(file, settings)
        .map_err(|e| WorkspaceError::new("settings.writeFailed", format!("Project settings could not be saved: {e}.")))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentFile {
    version: u32,
    projects: Vec<RecentProject>,
}

fn recent_file(data_dir: &Path) -> PathBuf {
    data_dir.join("recent-projects.json")
}

pub fn recent_projects(data_dir: &Path) -> Vec<RecentProject> {
    load::<RecentFile>(&recent_file(data_dir)).value.projects
}

fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// Move (or add) a project to the front of the recent list.
pub fn touch_recent(data_dir: &Path, path: &str, name: &str) -> io::Result<Vec<RecentProject>> {
    let mut projects = recent_projects(data_dir);
    projects.retain(|p| !same_path(&p.path, path));
    projects.insert(0, RecentProject { path: path.to_owned(), name: name.to_owned(), opened_at: now_millis() });
    projects.truncate(MAX_RECENT);
    store(&recent_file(data_dir), &RecentFile { version: 1, projects: projects.clone() })?;
    Ok(projects)
}

pub fn forget_recent(data_dir: &Path, path: &str) -> io::Result<Vec<RecentProject>> {
    let mut projects = recent_projects(data_dir);
    projects.retain(|p| !same_path(&p.path, path));
    store(&recent_file(data_dir), &RecentFile { version: 1, projects: projects.clone() })?;
    Ok(projects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_use_defaults_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_app_settings(dir.path());
        assert_eq!(loaded, Loaded { value: AppSettings::default(), exists: false, warning: None });
        let mut settings = AppSettings::default();
        settings.editor.font_size = 16;
        store_app_settings(dir.path(), &settings).unwrap();
        assert_eq!(load_app_settings(dir.path()).value, settings);
    }

    #[test]
    fn partial_files_fill_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(app_settings_file(dir.path()), r#"{"version":1,"editor":{"tabSize":4}}"#).unwrap();
        let loaded = load_app_settings(dir.path());
        assert_eq!(loaded.value.editor.tab_size, 4);
        assert_eq!(loaded.value.editor.font_size, 13);
        assert!(loaded.warning.is_none());
    }

    #[test]
    fn invalid_settings_are_set_aside_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(app_settings_file(dir.path()), r#"{"version":1,"editor":{"fontSize":400}}"#).unwrap();
        let loaded = load_app_settings(dir.path());
        assert_eq!(loaded.value, AppSettings::default());
        assert!(loaded.warning.unwrap().contains("font size"));
        let backups: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("settings.json.invalid-"))
            .collect();
        assert_eq!(backups.len(), 1);
        fs::write(app_settings_file(dir.path()), b"{ not json").unwrap();
        assert!(load_app_settings(dir.path()).warning.is_some());

        let mut bad = AppSettings::default();
        bad.editor.tab_size = 0;
        assert_eq!(store_app_settings(dir.path(), &bad).unwrap_err().code, "settings.invalid");
        bad.version = 2;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn project_settings_validate_targets() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".exposition").join("project.json");
        assert!(!load_project_settings(&file).exists);
        let settings = ProjectSettings {
            version: 1,
            entry: Some(WorkspacePath::parse("main.typ").unwrap()),
            targets: vec![ProjectTarget {
                name: "paper".into(),
                format: DocumentFormat::Typst,
                engine: Engine::Typst,
                entry: WorkspacePath::parse("main.typ").unwrap(),
            }],
            default_target: Some("paper".into()),
        };
        store_project_settings(&file, &settings).unwrap();
        let loaded = load_project_settings(&file);
        assert!(loaded.exists);
        assert_eq!(loaded.value, settings);
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");

        let mut bad = settings.clone();
        bad.targets[0].engine = Engine::PdfLatex;
        assert!(bad.validate().is_err());
        let mut bad = settings.clone();
        bad.default_target = Some("missing".into());
        assert!(bad.validate().is_err());
        let mut bad = settings;
        bad.targets.push(bad.targets[0].clone());
        assert!(bad.validate().is_err());
        // Escaping entry paths are rejected while parsing.
        let raw = r#"{"version":1,"entry":"../outside.typ"}"#;
        assert!(serde_json::from_str::<ProjectSettings>(raw).is_err());
    }

    #[test]
    fn recent_projects_are_deduplicated_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..15 {
            touch_recent(dir.path(), &format!(r"C:\p{i}"), &format!("p{i}")).unwrap();
        }
        touch_recent(dir.path(), r"C:\p3", "p3").unwrap();
        let recent = recent_projects(dir.path());
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(recent[0].path, r"C:\p3");
        assert_eq!(recent.iter().filter(|p| p.path == r"C:\p3").count(), 1);
        let recent = forget_recent(dir.path(), r"C:\p3").unwrap();
        assert!(recent.iter().all(|p| p.path != r"C:\p3"));
    }
}
