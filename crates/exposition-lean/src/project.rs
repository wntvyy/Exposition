//! Lean project detection.
//!
//! A Lake project is a directory with `lakefile.lean` or `lakefile.toml`; its
//! `lean-toolchain` pins the Lean version. A `.lean` file belongs to the
//! nearest enclosing Lake project inside the workspace. Files outside any Lake
//! project are *standalone*: they are checked with `lean --server`, using the
//! nearest `lean-toolchain` above them (inside the workspace) or elan's
//! default toolchain.
//!
//! Detection only reads file names and the small `lean-toolchain` file; it
//! never runs anything (a `lakefile.lean` is Lean code executed by Lake, so
//! starting a server on a project is trusted execution).

use std::path::{Path, PathBuf};

/// Directories never searched for projects (build output, VCS, dependencies).
const SKIP_DIRS: &[&str] = &[".git", ".lake", "lake-packages", "node_modules", "target", ".exposition"];
const MAX_SCAN_DEPTH: usize = 6;
const MAX_SCAN_ENTRIES: usize = 20_000;
const MAX_TOOLCHAIN_FILE: u64 = 4096;

pub const LAKEFILES: &[&str] = &["lakefile.lean", "lakefile.toml"];
pub const TOOLCHAIN_FILE: &str = "lean-toolchain";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectKind {
    /// A Lake package; `lakefile` is the file name found in the root.
    Lake { lakefile: String },
    /// No Lake package encloses the file.
    Standalone,
}

/// Where a toolchain request came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinSource {
    /// A `lean-toolchain` file (absolute path).
    File(PathBuf),
    /// elan's configured default toolchain.
    ElanDefault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolchainPin {
    /// The requested toolchain exactly as written (e.g. `leanprover/lean4:v4.35.0`).
    pub requested: String,
    pub source: PinSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeanProject {
    /// Absolute project root: the Lake package directory, or the workspace
    /// root for standalone files.
    pub root: PathBuf,
    pub kind: ProjectKind,
    /// `Err` holds why the pin could not be read (invalid file).
    pub pin: Result<Option<ToolchainPin>, String>,
}

/// The project a file belongs to. `file` must be inside `workspace_root`
/// (both absolute, same form).
pub fn project_for(workspace_root: &Path, file: &Path) -> LeanProject {
    let mut dir = file.parent();
    let mut pin_dir: Option<PathBuf> = None;
    while let Some(current) = dir {
        if !current.starts_with(workspace_root) {
            break;
        }
        if pin_dir.is_none() && current.join(TOOLCHAIN_FILE).is_file() {
            pin_dir = Some(current.to_path_buf());
        }
        if let Some(lakefile) = lakefile_in(current) {
            return LeanProject {
                root: current.to_path_buf(),
                kind: ProjectKind::Lake { lakefile },
                pin: read_pin(&current.join(TOOLCHAIN_FILE)),
            };
        }
        if current == workspace_root {
            break;
        }
        dir = current.parent();
    }
    LeanProject {
        root: workspace_root.to_path_buf(),
        kind: ProjectKind::Standalone,
        pin: match pin_dir {
            Some(d) => read_pin(&d.join(TOOLCHAIN_FILE)),
            None => Ok(None),
        },
    }
}

fn lakefile_in(dir: &Path) -> Option<String> {
    LAKEFILES.iter().find(|name| dir.join(name).is_file()).map(|name| (*name).to_owned())
}

/// Read a `lean-toolchain` file: `Ok(None)` when absent.
pub fn read_pin(path: &Path) -> Result<Option<ToolchainPin>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if !meta.is_file() || meta.len() > MAX_TOOLCHAIN_FILE {
        return Err(format!("{} is not a small text file", path.display()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{} could not be read: {e}", path.display()))?;
    let requested = parse_toolchain_file(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(ToolchainPin { requested, source: PinSource::File(path.to_path_buf()) }))
}

/// The toolchain named by a `lean-toolchain` file (first non-empty line).
pub fn parse_toolchain_file(text: &str) -> Result<String, String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| "lean-toolchain is empty".to_owned())?;
    validate_toolchain_name(line)?;
    Ok(line.to_owned())
}

/// Toolchain names are passed to elan as one argument; keep them to the
/// characters toolchain names use so they can never smuggle options or paths.
pub fn validate_toolchain_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('-')
        && !name.contains("..")
        && !name.starts_with('/')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '+'));
    if valid { Ok(()) } else { Err(format!("`{name}` is not a valid toolchain name")) }
}

/// Lake projects under the workspace root (bounded breadth-first scan).
pub fn find_projects(workspace_root: &Path) -> Vec<LeanProject> {
    let mut found = Vec::new();
    let mut queue = vec![(workspace_root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = queue.pop() {
        if let Some(lakefile) = lakefile_in(&dir) {
            found.push(LeanProject {
                pin: read_pin(&dir.join(TOOLCHAIN_FILE)),
                root: dir.clone(),
                kind: ProjectKind::Lake { lakefile },
            });
        }
        if depth >= MAX_SCAN_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_SCAN_ENTRIES {
                return sorted(found);
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Links are not followed: a project must physically live in the workspace.
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir && !SKIP_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                queue.push((entry.path(), depth + 1));
            }
        }
    }
    sorted(found)
}

fn sorted(mut projects: Vec<LeanProject>) -> Vec<LeanProject> {
    projects.sort_by(|a, b| a.root.cmp(&b.root));
    projects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_toolchain_files() {
        assert_eq!(parse_toolchain_file("leanprover/lean4:v4.35.0\n").unwrap(), "leanprover/lean4:v4.35.0");
        assert_eq!(parse_toolchain_file("\n  stable  \n").unwrap(), "stable");
        assert!(parse_toolchain_file("\n\n").is_err());
        for bad in ["--help", "a b", "../x", "/abs", "x;rm", "a\u{0}b", "C:\\x"] {
            assert!(validate_toolchain_name(bad).is_err(), "accepted {bad:?}");
        }
        for good in
            ["leanprover/lean4:nightly-2024-01-01", "leanprover/lean4-pr-releases:pr-release-1234", "v4.9.0", "my-link"]
        {
            validate_toolchain_name(good).unwrap();
        }
    }

    #[test]
    fn finds_enclosing_lake_project_and_standalone_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_path_buf();
        let proj = ws.join("formal");
        std::fs::create_dir_all(proj.join("Formal").join("Deep")).unwrap();
        std::fs::create_dir_all(proj.join(".lake").join("packages").join("dep")).unwrap();
        std::fs::write(proj.join("lakefile.toml"), "name = \"Formal\"\n").unwrap();
        std::fs::write(proj.join("lean-toolchain"), "leanprover/lean4:v4.35.0\n").unwrap();
        // A dependency checkout inside .lake is not a workspace project.
        std::fs::write(proj.join(".lake").join("packages").join("dep").join("lakefile.lean"), "").unwrap();
        std::fs::create_dir_all(ws.join("notes")).unwrap();

        let p = project_for(&ws, &proj.join("Formal").join("Deep").join("A.lean"));
        assert_eq!(p.root, proj);
        assert_eq!(p.kind, ProjectKind::Lake { lakefile: "lakefile.toml".into() });
        assert_eq!(p.pin.unwrap().unwrap().requested, "leanprover/lean4:v4.35.0");

        let s = project_for(&ws, &ws.join("notes").join("Scratch.lean"));
        assert_eq!(s.kind, ProjectKind::Standalone);
        assert_eq!(s.root, ws);
        assert_eq!(s.pin, Ok(None));

        std::fs::write(ws.join("lean-toolchain"), "leanprover/lean4:v4.20.0").unwrap();
        let s = project_for(&ws, &ws.join("notes").join("Scratch.lean"));
        assert_eq!(s.pin.unwrap().unwrap().requested, "leanprover/lean4:v4.20.0");

        let all = find_projects(&ws);
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].root, proj);
    }
}
