//! Project permissions: the user's standing grant for Auto tasks in one
//! project (D3).
//!
//! A record is app-local (`<data>/execution/project-permissions.json`, never
//! in the project, never imported, never inferred from graph labels) and is
//! bound to the project's canonical folder AND its project-graph identity, so
//! a copy, a move, or a different project put in the same place has no
//! record. Only the webview's composer and Settings set or revoke records
//! (through the agent service); no agent tool, prompt, file, or tool output
//! can. Every change gets a new `version`, larger than every version before
//! it (also across a lost or reset file: versions are at least the current
//! time in milliseconds), so an authorization issued under an older version
//! is recognized as stale.
//!
//! Writes are atomic (temporary file, flush, rename). An unreadable or invalid
//! file grants nothing: it is copied aside as `<file>.invalid-<ms>` before
//! anything overwrites it, and if that copy fails the store refuses to write
//! (the original stays untouched).
//!
//! Native code that runs with the user's account privileges can still change
//! this file like any other file the account can write: storing grants
//! outside the project keeps them away from agent tools and project content,
//! it does not protect them from arbitrary native code.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::TaskId;
use crate::store::now_ms;

/// What Auto tasks may do to the project's files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FilesAccess {
    /// Every change asks first.
    Off,
    /// Changes in scope apply directly (unsaved) and stay reviewable.
    Edit,
}

/// How Auto tasks may run code. Ordered from narrow to broad.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CodeAccess {
    /// Every launch asks first.
    Off,
    /// Unattended inside the restricted container only (no network, read-only
    /// project, non-root, no host environment; never falls back to native).
    Restricted,
    /// Natively on this computer with the user's account privileges (not a
    /// sandbox); requires a trusted folder.
    Native,
}

/// One project's permissions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPermissions {
    /// Changes with every update; larger than every earlier version.
    pub version: u64,
    pub granted_at_ms: u64,
    pub updated_at_ms: u64,
    pub files: FilesAccess,
    pub code: CodeAccess,
    /// Project-local package installation (requires `code: native`).
    pub setup: bool,
}

/// What the user chose (composer "Use these permissions", Settings › Change).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PermissionChoice {
    pub files: FilesAccess,
    pub code: CodeAccess,
    pub setup: bool,
}

impl PermissionChoice {
    /// The profile offered when a project has no record: files edit, code
    /// restricted when a container image is configured (else off), setup off.
    /// Native is never pre-selected.
    pub fn defaults(restricted_available: bool) -> Self {
        Self {
            files: FilesAccess::Edit,
            code: if restricted_available { CodeAccess::Restricted } else { CodeAccess::Off },
            setup: false,
        }
    }

    pub fn validate(&self) -> Result<(), PermissionError> {
        if self.setup && self.code != CodeAccess::Native {
            return Err(PermissionError::Invalid(
                "Package setup runs installers natively, so it needs code to run natively on this computer.".into(),
            ));
        }
        Ok(())
    }
}

impl ProjectPermissions {
    pub fn choice(&self) -> PermissionChoice {
        PermissionChoice { files: self.files, code: self.code, setup: self.setup }
    }
}

/// One record, as Settings › Agents lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPermissionsEntry {
    /// The project folder (canonical, without verbatim prefixes).
    pub root: String,
    /// The project-graph identity the record is bound to.
    pub project: Option<String>,
    pub permissions: ProjectPermissions,
    /// The folder still exists.
    pub exists: bool,
    /// It is the open project (and the open project graph is this one).
    pub open: bool,
}

/// The open project's permissions and the facts the composer states about
/// the execution environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPermissionsState {
    pub root: String,
    pub project: Option<String>,
    /// `None`: no record (Auto needs one first).
    pub permissions: Option<ProjectPermissions>,
    /// The profile to offer when there is no record.
    pub defaults: PermissionChoice,
    /// A restricted container image is configured (restricted runs possible).
    pub restricted_available: bool,
    /// The folder is trusted (native runs possible).
    pub trusted: bool,
}

/// Announced after every change of a project's permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsChange {
    pub root: String,
    pub project: Option<String>,
    /// The record now (`None`: revoked).
    pub permissions: Option<ProjectPermissions>,
    /// What became narrower ("Code: native → restricted"); empty for a grant
    /// or a widening.
    pub narrowed: Vec<String>,
    /// Active Auto tasks of the project that were stopped.
    pub stopped: Vec<TaskId>,
    /// Cleanup Exposition could not confirm (a task still stopping, a
    /// research session still running). Never claims effects were undone.
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PermissionError {
    #[error("{0}")]
    Invalid(String),
    #[error("could not save project permissions: {0}")]
    Io(String),
}

/// What became narrower from `old` to `new` (`None`: revoked).
pub fn narrowed(old: Option<&ProjectPermissions>, new: Option<&ProjectPermissions>) -> Vec<String> {
    let Some(old) = old else { return Vec::new() };
    let Some(new) = new else { return vec!["revoked".to_owned()] };
    let mut out = Vec::new();
    if new.files < old.files {
        out.push(format!("Files: {} → {}", files_label(old.files), files_label(new.files)));
    }
    if new.code < old.code {
        out.push(format!("Code: {} → {}", code_label(old.code), code_label(new.code)));
    }
    if old.setup && !new.setup {
        out.push("Setup: on → off".to_owned());
    }
    out
}

pub fn files_label(files: FilesAccess) -> &'static str {
    match files {
        FilesAccess::Off => "off",
        FilesAccess::Edit => "edit",
    }
}

pub fn code_label(code: CodeAccess) -> &'static str {
    match code {
        CodeAccess::Off => "off",
        CodeAccess::Restricted => "restricted",
        CodeAccess::Native => "native",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredRecord {
    root: String,
    project: Option<String>,
    version: u64,
    granted_at_ms: u64,
    updated_at_ms: u64,
    files: FilesAccess,
    code: CodeAccess,
    setup: bool,
}

impl StoredRecord {
    fn permissions(&self) -> ProjectPermissions {
        ProjectPermissions {
            version: self.version,
            granted_at_ms: self.granted_at_ms,
            updated_at_ms: self.updated_at_ms,
            files: self.files,
            code: self.code,
            setup: self.setup,
        }
    }

    fn matches(&self, root: &Path, project: Option<&str>) -> bool {
        same_folder(Path::new(&self.root), root) && self.project.as_deref() == project
    }
}

const FILE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PermissionsFile {
    version: u32,
    /// The largest record version ever issued (revocations included).
    last_version: u64,
    projects: Vec<StoredRecord>,
}

struct State {
    file: PermissionsFile,
    /// An invalid file could not be copied aside: never overwrite it.
    frozen: Option<String>,
}

/// The project-permission records of this installation.
pub struct PermissionStore {
    path: Option<PathBuf>,
    state: Mutex<State>,
    /// Why the stored file was set aside at load, if it was.
    warning: Mutex<Option<String>>,
}

/// The same folder: equal paths (ignoring verbatim prefixes; ASCII case on
/// Windows), or the same canonical folder.
pub fn same_folder(a: &Path, b: &Path) -> bool {
    let plain = |p: &Path| PathBuf::from(crate::workspace::display_path(p));
    exposition_execution::paths::same_path(&plain(a), &plain(b))
        || (std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b))
}

/// `path` is `root` or inside it (ignoring verbatim prefixes).
pub fn within_folder(path: &Path, root: &Path) -> bool {
    let plain = |p: &Path| PathBuf::from(crate::workspace::display_path(p));
    exposition_execution::paths::is_within(&plain(path), &plain(root)) || same_folder(path, root)
}

fn canonical_text(root: &Path) -> String {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    crate::workspace::display_path(&canonical)
}

impl PermissionStore {
    /// No file: records live for the process only (tests, embeddings).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            state: Mutex::new(State {
                file: PermissionsFile { version: FILE_VERSION, ..Default::default() },
                frozen: None,
            }),
            warning: Mutex::new(None),
        }
    }

    /// Load `path`. A missing file grants nothing; an unreadable or invalid
    /// one grants nothing and is copied aside first.
    pub fn load(path: PathBuf) -> Self {
        let mut warning = None;
        let mut frozen = None;
        let file = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                PermissionsFile { version: FILE_VERSION, ..Default::default() }
            }
            Err(e) => {
                let reason = format!("the project permissions file could not be read ({e}); nothing is granted");
                frozen = Some(reason.clone());
                warning = Some(reason);
                PermissionsFile { version: FILE_VERSION, last_version: now_ms(), projects: Vec::new() }
            }
            Ok(bytes) => match parse(&bytes) {
                Ok(file) => file,
                Err(reason) => {
                    match backup_invalid(&path) {
                        Ok(backup) => {
                            warning = Some(format!(
                                "Invalid project permissions were set aside as {} ({reason}); no project has Auto \
                                 permissions until you choose them again.",
                                backup.display()
                            ));
                        }
                        Err(e) => {
                            let message = format!(
                                "Invalid project permissions ({reason}) could not be set aside ({e}); nothing is \
                                 granted and the file is left unchanged."
                            );
                            frozen = Some(message.clone());
                            warning = Some(message);
                        }
                    }
                    PermissionsFile { version: FILE_VERSION, last_version: now_ms(), projects: Vec::new() }
                }
            },
        };
        Self { path: Some(path), state: Mutex::new(State { file, frozen }), warning: Mutex::new(warning) }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Why the stored file was set aside at load, if it was.
    pub fn warning(&self) -> Option<String> {
        self.warning.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The record for the project in `root` with graph identity `project`.
    pub fn get(&self, root: &Path, project: Option<&str>) -> Option<ProjectPermissions> {
        self.lock().file.projects.iter().find(|r| r.matches(root, project)).map(StoredRecord::permissions)
    }

    /// Every record: (root, project, permissions).
    pub fn list(&self) -> Vec<(String, Option<String>, ProjectPermissions)> {
        self.lock().file.projects.iter().map(|r| (r.root.clone(), r.project.clone(), r.permissions())).collect()
    }

    fn next_version(file: &PermissionsFile) -> u64 {
        (file.last_version + 1).max(now_ms())
    }

    fn save(&self, state: &mut State, next: PermissionsFile) -> Result<(), PermissionError> {
        if let Some(reason) = &state.frozen {
            return Err(PermissionError::Io(reason.clone()));
        }
        if let Some(path) = &self.path {
            exposition_execution::settings::write_json_atomic(path, &next)
                .map_err(|e| PermissionError::Io(e.to_string()))?;
        }
        state.file = next;
        Ok(())
    }

    /// Grant or change the project's permissions. Returns (before, after).
    pub fn set(
        &self,
        root: &Path,
        project: Option<&str>,
        choice: PermissionChoice,
    ) -> Result<(Option<ProjectPermissions>, ProjectPermissions), PermissionError> {
        choice.validate()?;
        let mut state = self.lock();
        let mut next = state.file.clone();
        let version = Self::next_version(&next);
        let now = now_ms();
        let before = next.projects.iter().find(|r| r.matches(root, project)).map(StoredRecord::permissions);
        next.projects.retain(|r| !r.matches(root, project));
        let record = StoredRecord {
            root: canonical_text(root),
            project: project.map(str::to_owned),
            version,
            granted_at_ms: before.map(|b| b.granted_at_ms).unwrap_or(now),
            updated_at_ms: now,
            files: choice.files,
            code: choice.code,
            setup: choice.setup,
        };
        let after = record.permissions();
        next.projects.push(record);
        next.last_version = version;
        self.save(&mut state, next)?;
        Ok((before, after))
    }

    /// Remove the project's record (its versions are never reused).
    pub fn revoke(&self, root: &Path, project: Option<&str>) -> Result<Option<ProjectPermissions>, PermissionError> {
        let mut state = self.lock();
        let mut next = state.file.clone();
        let before = next.projects.iter().find(|r| r.matches(root, project)).map(StoredRecord::permissions);
        if before.is_none() {
            return Ok(None);
        }
        next.projects.retain(|r| !r.matches(root, project));
        next.last_version = Self::next_version(&next);
        self.save(&mut state, next)?;
        Ok(before)
    }
}

fn parse(bytes: &[u8]) -> Result<PermissionsFile, String> {
    let file: PermissionsFile = serde_json::from_slice(bytes).map_err(|e| format!("not valid: {e}"))?;
    if file.version != FILE_VERSION {
        return Err(format!("unsupported version {}", file.version));
    }
    for record in &file.projects {
        if !Path::new(&record.root).is_absolute() {
            return Err(format!("{:?} is not an absolute folder", record.root));
        }
        if record.setup && record.code != CodeAccess::Native {
            return Err("package setup without native code".into());
        }
        if record.version > file.last_version {
            return Err("a record version is larger than the last issued version".into());
        }
    }
    Ok(file)
}

/// Copy an invalid file to `<name>.invalid-<millis>` beside it.
fn backup_invalid(path: &Path) -> std::io::Result<PathBuf> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let backup = path.with_file_name(format!("{name}.invalid-{}", now_ms()));
    std::fs::copy(path, &backup)?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(files: FilesAccess, code: CodeAccess, setup: bool) -> PermissionChoice {
        PermissionChoice { files, code, setup }
    }

    #[test]
    fn records_bind_to_the_folder_and_the_project_graph_identity() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("paper");
        let copy = dir.path().join("paper-copy");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&copy).unwrap();
        let store = PermissionStore::load(dir.path().join("execution").join("project-permissions.json"));
        let (before, after) =
            store.set(&project, Some("prj-a"), choice(FilesAccess::Edit, CodeAccess::Restricted, false)).unwrap();
        assert!(before.is_none());
        assert_eq!(store.get(&project, Some("prj-a")), Some(after));
        // A copy elsewhere (even with the same graph identity) has no record.
        assert_eq!(store.get(&copy, Some("prj-a")), None);
        // A different project in the same folder has no record.
        assert_eq!(store.get(&project, Some("prj-b")), None);
        assert_eq!(store.get(&project, None), None);
        // The same folder by another spelling is the same folder.
        let upper = PathBuf::from(project.display().to_string().to_uppercase());
        if cfg!(windows) {
            assert_eq!(store.get(&upper, Some("prj-a")), Some(after));
        }
    }

    #[test]
    fn versions_only_grow_across_changes_revocations_and_a_lost_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project-permissions.json");
        let root = dir.path();
        let store = PermissionStore::load(path.clone());
        let (_, a) = store.set(root, Some("prj-a"), choice(FilesAccess::Edit, CodeAccess::Off, false)).unwrap();
        let (_, b) = store.set(root, Some("prj-a"), choice(FilesAccess::Off, CodeAccess::Off, false)).unwrap();
        assert!(b.version > a.version);
        assert_eq!(b.granted_at_ms, a.granted_at_ms, "a change keeps the grant time");
        store.revoke(root, Some("prj-a")).unwrap();
        let (_, c) = store.set(root, Some("prj-a"), choice(FilesAccess::Edit, CodeAccess::Off, false)).unwrap();
        assert!(c.version > b.version, "a re-grant never reuses a version");
        // Reloaded: the record and the counter persist.
        let reloaded = PermissionStore::load(path.clone());
        assert_eq!(reloaded.get(root, Some("prj-a")), Some(c));
        // The file lost: versions still exceed every earlier one.
        std::fs::remove_file(&path).unwrap();
        let fresh = PermissionStore::load(path);
        let (_, d) = fresh.set(root, Some("prj-a"), choice(FilesAccess::Edit, CodeAccess::Off, false)).unwrap();
        assert!(d.version > c.version);
    }

    #[test]
    fn an_unreadable_file_grants_nothing_and_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project-permissions.json");
        let root = dir.path();
        let good = PermissionStore::load(path.clone());
        good.set(root, Some("prj-a"), choice(FilesAccess::Edit, CodeAccess::Native, true)).unwrap();
        for bad in [
            "{not json".to_owned(),
            // A forged record that bypasses the setup rule.
            std::fs::read_to_string(&path).unwrap().replace("\"native\"", "\"off\""),
            // An unknown field (e.g. an attempt to add a capability).
            std::fs::read_to_string(&path).unwrap().replace("\"setup\": true", "\"setup\": true, \"network\": true"),
        ] {
            std::fs::write(&path, &bad).unwrap();
            let store = PermissionStore::load(path.clone());
            assert_eq!(store.get(root, Some("prj-a")), None, "nothing granted from {bad}");
            assert!(store.warning().is_some());
            let backups: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("project-permissions.json.invalid-"))
                .collect();
            assert!(!backups.is_empty(), "the original was copied aside");
            assert!(backups.iter().any(|b| std::fs::read_to_string(b.path()).unwrap() == bad));
            for b in backups {
                std::fs::remove_file(b.path()).unwrap();
            }
            // The original stays until a deliberate change replaces it.
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
            // Restore a valid file for the next case.
            std::fs::remove_file(&path).unwrap();
            let again = PermissionStore::load(path.clone());
            again.set(root, Some("prj-a"), choice(FilesAccess::Edit, CodeAccess::Native, true)).unwrap();
        }
    }

    #[test]
    fn setup_needs_native_code_and_narrowing_is_described() {
        assert!(choice(FilesAccess::Edit, CodeAccess::Restricted, true).validate().is_err());
        assert!(choice(FilesAccess::Edit, CodeAccess::Native, true).validate().is_ok());
        let store = PermissionStore::in_memory();
        let root = Path::new("C:/projects/paper");
        assert!(store.set(root, None, choice(FilesAccess::Edit, CodeAccess::Off, true)).is_err());
        let (_, wide) = store.set(root, None, choice(FilesAccess::Edit, CodeAccess::Native, true)).unwrap();
        let (_, narrow) = store.set(root, None, choice(FilesAccess::Off, CodeAccess::Restricted, false)).unwrap();
        assert_eq!(
            narrowed(Some(&wide), Some(&narrow)),
            vec!["Files: edit → off", "Code: native → restricted", "Setup: on → off"]
        );
        assert!(narrowed(Some(&narrow), Some(&wide)).is_empty(), "a widening narrows nothing");
        assert_eq!(narrowed(Some(&wide), None), vec!["revoked"]);
        assert!(narrowed(None, Some(&wide)).is_empty());
        assert_eq!(PermissionChoice::defaults(true).code, CodeAccess::Restricted);
        assert_eq!(PermissionChoice::defaults(false).code, CodeAccess::Off);
        assert_eq!(PermissionChoice::defaults(true).files, FilesAccess::Edit);
        assert!(!PermissionChoice::defaults(true).setup);
    }
}
