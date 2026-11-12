//! Canonical workspace roots and path resolution.
//!
//! [`WorkspacePath`] guarantees lexical safety (relative, no `..`, valid names).
//! This module adds the filesystem rules: every resolved path is canonicalized
//! (following symlinks and junctions) and must stay under the canonical root.
//! For a path that does not exist yet, the nearest existing ancestor is
//! canonicalized and checked instead, so a link inside the folder can never be
//! used to create or read files outside it.
//!
//! Limitation: resolution and the subsequent file operation are separate steps,
//! so a concurrent process that swaps a directory for a link in between could
//! still race the check. Exposition does not defend against a hostile local
//! process with the user's own privileges.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use exposition_core::path::WorkspacePath;

use crate::error::{WorkspaceError, WorkspaceResult};

#[derive(Debug, Clone)]
pub struct WorkspaceRoot {
    /// Canonical root (on Windows this is a `\\?\` verbatim path).
    canonical: PathBuf,
}

impl WorkspaceRoot {
    /// Open a folder as a workspace root. The path must be absolute, exist, be a
    /// directory, and be listable.
    pub fn open(path: &Path) -> WorkspaceResult<Self> {
        let shown = path.display().to_string();
        if path.as_os_str().is_empty() || !path.is_absolute() {
            return Err(WorkspaceError::new(
                "workspace.invalidPath",
                format!("{shown} is not an absolute folder path."),
            ));
        }
        let canonical = fs::canonicalize(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => WorkspaceError::new("workspace.notFound", format!("{shown} does not exist.")),
            _ => WorkspaceError::new("workspace.unreadable", format!("{shown} cannot be opened: {e}.")),
        })?;
        let meta = fs::metadata(&canonical)
            .map_err(|e| WorkspaceError::new("workspace.unreadable", format!("{shown} cannot be opened: {e}.")))?;
        if !meta.is_dir() {
            return Err(WorkspaceError::new("workspace.notDirectory", format!("{shown} is not a folder.")));
        }
        fs::read_dir(&canonical)
            .map_err(|e| WorkspaceError::new("workspace.unreadable", format!("{shown} cannot be listed: {e}.")))?;
        if let Some(reason) = too_broad_reason(&canonical) {
            return Err(WorkspaceError::new(
                "workspace.tooBroad",
                format!(
                    "{} cannot be opened as a project folder: {reason}. Open a specific project folder instead.",
                    display_path(&canonical)
                ),
            ));
        }
        Ok(Self { canonical })
    }

    /// The canonical root path.
    pub fn path(&self) -> &Path {
        &self.canonical
    }

    /// The root as a user-facing absolute path (without the `\\?\` prefix).
    pub fn display_path(&self) -> String {
        display_path(&self.canonical)
    }

    /// Folder name shown in the UI.
    pub fn name(&self) -> String {
        self.canonical.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.display_path())
    }

    /// Join `path` lexically under the root without touching the filesystem.
    pub fn join(&self, path: &WorkspacePath) -> PathBuf {
        let mut joined = self.canonical.clone();
        for part in path.components() {
            joined.push(part);
        }
        joined
    }

    /// Resolve an existing file or directory, following links, and verify that
    /// the target is inside the root.
    pub fn resolve_existing(&self, path: &WorkspacePath) -> WorkspaceResult<PathBuf> {
        let joined = self.join(path);
        let canonical = fs::canonicalize(&joined).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => {
                // A dangling link exists as an entry but cannot be followed.
                if fs::symlink_metadata(&joined).is_ok() {
                    WorkspaceError::new("path.brokenLink", format!("{path} is a link whose target does not exist."))
                } else {
                    // Report an escape (not "missing") when an existing ancestor
                    // leads outside, so paths outside cannot be probed.
                    match self.resolve_for_write(path) {
                        Err(e) if e.code == "path.outsideWorkspace" => e,
                        _ => WorkspaceError::not_found(path),
                    }
                }
            }
            _ => WorkspaceError::io(path, &e),
        })?;
        self.ensure_inside(&canonical, path)?;
        Ok(canonical)
    }

    /// Resolve a path that is about to be written. Existing targets are
    /// resolved as in [`Self::resolve_existing`]; for a new file the nearest
    /// existing ancestor is canonicalized and must be inside the root, and the
    /// missing components are appended to it.
    pub fn resolve_for_write(&self, path: &WorkspacePath) -> WorkspaceResult<PathBuf> {
        let joined = self.join(path);
        match fs::symlink_metadata(&joined) {
            Ok(_) => return self.resolve_existing(path),
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(WorkspaceError::io(path, &e)),
            Err(_) => {}
        }
        let mut missing: Vec<String> = Vec::new();
        let mut ancestor: Option<WorkspacePath> = Some(path.clone());
        while let Some(current) = ancestor {
            let candidate = self.join(&current);
            match fs::symlink_metadata(&candidate) {
                Ok(_) => {
                    let canonical = self.resolve_existing(&current)?;
                    if !fs::metadata(&canonical).map(|m| m.is_dir()).unwrap_or(false) {
                        return Err(WorkspaceError::new(
                            "path.notDirectory",
                            format!("{current} is not a folder, so {path} cannot be created."),
                        ));
                    }
                    return Ok(append(canonical, &missing));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    missing.insert(0, current.file_name().to_owned());
                    ancestor = current.parent();
                }
                Err(e) => return Err(WorkspaceError::io(&current, &e)),
            }
        }
        // No component exists: the parent is the root itself.
        Ok(append(self.canonical.clone(), &missing))
    }

    /// The workspace path of a canonical (or root-prefixed) absolute path, if it
    /// lies strictly inside the root.
    pub fn relativize(&self, absolute: &Path) -> Option<WorkspacePath> {
        let rest = match absolute.strip_prefix(&self.canonical) {
            Ok(rest) => rest.to_path_buf(),
            // Watchers and dialogs may report non-verbatim paths; compare display forms.
            Err(_) => PathBuf::from(display_path(absolute)).strip_prefix(self.display_path()).ok()?.to_path_buf(),
        };
        let mut parts = Vec::new();
        for component in rest.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
                _ => return None,
            }
        }
        if parts.is_empty() {
            return None;
        }
        WorkspacePath::parse(&parts.join("/")).ok()
    }

    fn ensure_inside(&self, canonical: &Path, path: &WorkspacePath) -> WorkspaceResult<()> {
        if canonical.starts_with(&self.canonical) { Ok(()) } else { Err(WorkspaceError::outside(path)) }
    }

    /// True if `canonical` (already canonicalized) lies under the root.
    pub fn contains(&self, canonical: &Path) -> bool {
        canonical.starts_with(&self.canonical)
    }
}

/// Directories a project folder may not equal or contain (system, program,
/// profile, and per-user application data folders).
fn protected_directories() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        for var in [
            "SystemRoot",
            "windir",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
            "USERPROFILE",
            "PUBLIC",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            if let Some(value) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                dirs.push(PathBuf::from(value));
            }
        }
        // The folder holding every user profile (C:\Users).
        if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty())
            && let Some(parent) = Path::new(&profile).parent()
        {
            dirs.push(parent.to_path_buf());
        }
    }
    #[cfg(unix)]
    {
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            let home = PathBuf::from(home);
            if let Some(parent) = home.parent() {
                dirs.push(parent.to_path_buf());
            }
            dirs.push(home);
        }
        for dir in ["/usr", "/etc", "/bin", "/sbin", "/lib", "/var", "/opt", "/System", "/Library", "/Applications"] {
            dirs.push(PathBuf::from(dir));
        }
    }
    dirs.into_iter().filter_map(|d| fs::canonicalize(d).ok()).collect()
}

/// Directories a project folder may not be inside (operating system and
/// installed programs).
fn system_directories() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    for var in ["SystemRoot", "windir", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData"] {
        if let Some(value) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            dirs.push(PathBuf::from(value));
        }
    }
    #[cfg(unix)]
    for dir in ["/usr", "/etc", "/bin", "/sbin", "/lib", "/System", "/proc", "/sys", "/dev"] {
        dirs.push(PathBuf::from(dir));
    }
    dirs.into_iter().filter_map(|d| fs::canonicalize(d).ok()).collect()
}

/// Why `canonical` is too broad to open as a project folder, if it is: a
/// drive/file-system root, a folder that equals or contains a system, program,
/// or profile folder, or a folder inside a system or program folder. This is
/// the same rule workspace trust applies (to be shared as
/// `exposition_core::roots::too_broad_reason`).
pub fn too_broad_reason(canonical: &Path) -> Option<String> {
    if canonical.parent().is_none() {
        return Some("it is the root of a drive".into());
    }
    if let Some(dir) = protected_directories().into_iter().find(|dir| dir.starts_with(canonical)) {
        return Some(if dir == canonical {
            "it is a system or profile folder".into()
        } else {
            format!("it contains the system or profile folder {}", display_path(&dir))
        });
    }
    if let Some(dir) = system_directories().into_iter().find(|dir| canonical.starts_with(dir)) {
        return Some(format!("it is inside the system folder {}", display_path(&dir)));
    }
    None
}

fn append(mut base: PathBuf, parts: &[String]) -> PathBuf {
    for part in parts {
        base.push(part);
    }
    base
}

/// Strip the Windows verbatim prefix for display (`\\?\C:\x` → `C:\x`,
/// `\\?\UNC\server\share` → `\\server\share`).
pub fn display_path(path: &Path) -> String {
    let raw = path.display().to_string();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn wp(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    /// Create a directory junction (Windows; no administrator rights needed).
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let status = Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J failed");
    }

    #[cfg(unix)]
    fn junction(link: &Path, target: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: WorkspaceRoot,
        outside: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        fs::create_dir_all(project.join("chapters")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(project.join("main.typ"), "= Title\n").unwrap();
        fs::write(project.join("chapters/intro.typ"), "intro\n").unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        junction(&project.join("escape"), &outside);
        junction(&project.join("inner-link"), &project.join("chapters"));
        let root = WorkspaceRoot::open(&project).unwrap();
        Fixture { _dir: dir, root, outside }
    }

    #[test]
    fn opens_only_existing_absolute_directories() {
        let f = fixture();
        assert_eq!(WorkspaceRoot::open(Path::new("relative/dir")).unwrap_err().code, "workspace.invalidPath");
        let missing = f.outside.join("nope");
        assert_eq!(WorkspaceRoot::open(&missing).unwrap_err().code, "workspace.notFound");
        assert_eq!(WorkspaceRoot::open(&f.outside.join("secret.txt")).unwrap_err().code, "workspace.notDirectory");
        assert!(!f.root.display_path().starts_with(r"\\?\"));
        assert_eq!(f.root.name(), "project");
    }

    #[cfg(windows)]
    #[test]
    fn refuses_too_broad_roots() {
        let env = |v: &str| PathBuf::from(std::env::var_os(v).expect(v));
        let system = env("SystemRoot");
        let profile = env("USERPROFILE");
        let drive = PathBuf::from(format!("{}\\", std::env::var("SystemDrive").unwrap()));
        for broad in [
            system.clone(),
            system.join("System32"),
            profile.clone(),
            profile.parent().unwrap().to_path_buf(),
            drive,
            env("ProgramData"),
            env("LOCALAPPDATA"),
        ] {
            let err = WorkspaceRoot::open(&broad).unwrap_err();
            assert_eq!(err.code, "workspace.tooBroad", "{} → {err:?}", broad.display());
        }
        // A project folder inside the profile (here: under the temp directory) is fine.
        let f = fixture();
        assert!(WorkspaceRoot::open(f.root.path()).is_ok());
    }

    #[test]
    fn resolves_files_inside_the_root() {
        let f = fixture();
        let main = f.root.resolve_existing(&wp("main.typ")).unwrap();
        assert_eq!(fs::read_to_string(main).unwrap(), "= Title\n");
        // A link that stays inside the root is allowed.
        let via_link = f.root.resolve_existing(&wp("inner-link/intro.typ")).unwrap();
        assert_eq!(fs::read_to_string(via_link).unwrap(), "intro\n");
        assert_eq!(f.root.resolve_existing(&wp("missing.typ")).unwrap_err().code, "path.notFound");
    }

    #[test]
    fn rejects_junction_escapes_for_reads_and_writes() {
        let f = fixture();
        assert_eq!(f.root.resolve_existing(&wp("escape")).unwrap_err().code, "path.outsideWorkspace");
        assert_eq!(f.root.resolve_existing(&wp("escape/secret.txt")).unwrap_err().code, "path.outsideWorkspace");
        // Missing files behind the junction are reported as escapes, not as missing,
        // so the existence of outside files cannot be probed.
        assert_eq!(f.root.resolve_existing(&wp("escape/absent.txt")).unwrap_err().code, "path.outsideWorkspace");
        // Creating a new file through the junction is refused too.
        assert_eq!(f.root.resolve_for_write(&wp("escape/new.txt")).unwrap_err().code, "path.outsideWorkspace");
        assert_eq!(f.root.resolve_for_write(&wp("escape/deeper/new.txt")).unwrap_err().code, "path.outsideWorkspace");
        assert!(!f.outside.join("new.txt").exists());
    }

    #[test]
    fn resolves_new_files_under_existing_inside_ancestors() {
        let f = fixture();
        let target = f.root.resolve_for_write(&wp("chapters/new/deep.typ")).unwrap();
        assert!(target.starts_with(f.root.path()));
        assert!(target.ends_with(Path::new("chapters").join("new").join("deep.typ")));
        let top = f.root.resolve_for_write(&wp("fresh.typ")).unwrap();
        assert_eq!(top, f.root.path().join("fresh.typ"));
        assert_eq!(f.root.resolve_for_write(&wp("main.typ/child")).unwrap_err().code, "path.notDirectory");
    }

    #[test]
    fn relativizes_paths_under_the_root() {
        let f = fixture();
        let abs = f.root.path().join("chapters").join("intro.typ");
        assert_eq!(f.root.relativize(&abs).unwrap().as_str(), "chapters/intro.typ");
        let shown = PathBuf::from(display_path(&abs));
        assert_eq!(f.root.relativize(&shown).unwrap().as_str(), "chapters/intro.typ");
        assert!(f.root.relativize(f.root.path()).is_none());
        assert!(f.root.relativize(&f.outside.join("secret.txt")).is_none());
    }
}
