//! Task scope checks on the file a path actually names.
//!
//! A scope is written as workspace paths, but a requested spelling can name a
//! different file: a link (symlink or junction) inside the scope can point
//! outside it, and Windows aliases (case variants, 8.3 short names) name the
//! same file differently. Every path a tool uses is therefore checked twice:
//! the requested spelling against the scope (lexically), and the resolved
//! file (links followed, canonical spelling) against the resolved scope
//! entries. Both must pass.
//!
//! Hard links cannot be told apart from ordinary files by their path; a hard
//! link placed inside the scope by the user is treated as an in-scope file.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;

use crate::model::TaskScope;

/// Why a path is not usable for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    /// The requested spelling, or the file it resolves to, lies outside the scope.
    OutsideScope {
        path: String,
        resolved: Option<String>,
    },
    /// The path resolves outside the project folder.
    OutsideRoot(String),
    /// A link on the path cannot be followed.
    BrokenLink(String),
    Io {
        path: String,
        message: String,
    },
}

impl ScopeError {
    /// Stable code carried in the tool result (`code` field).
    pub fn code(&self) -> &'static str {
        match self {
            ScopeError::OutsideScope { .. } => super::exec::codes::OUT_OF_SCOPE,
            ScopeError::OutsideRoot(_) => super::exec::codes::OUTSIDE_PROJECT,
            ScopeError::BrokenLink(_) | ScopeError::Io { .. } => super::exec::codes::INVALID_PATH,
        }
    }

    pub fn message(&self, scope: &TaskScope) -> String {
        match self {
            ScopeError::OutsideScope { path, resolved: None } => format!(
                "{path} is outside this task's scope ({}). The application refused the access; work within the scope.",
                scope.describe()
            ),
            ScopeError::OutsideScope { path, resolved: Some(target) } => format!(
                "{path} is a link to {target}, which is outside this task's scope ({}). The application refused the \
                 access; work within the scope.",
                scope.describe()
            ),
            ScopeError::OutsideRoot(path) => {
                format!("{path} resolves outside the project folder. The application refused the access.")
            }
            ScopeError::BrokenLink(path) => format!("{path} goes through a link that cannot be followed."),
            ScopeError::Io { path, message } => format!("{path} cannot be checked: {message}"),
        }
    }
}

fn relative(root: &Path, canonical: &Path, path: &WorkspacePath) -> Result<Option<WorkspacePath>, ScopeError> {
    let rest = canonical.strip_prefix(root).map_err(|_| ScopeError::OutsideRoot(path.to_string()))?;
    let mut parts = Vec::new();
    for component in rest.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| ScopeError::Io {
                        path: path.to_string(),
                        message: "the name is not Unicode".into(),
                    })?
                    .to_owned(),
            ),
            _ => return Err(ScopeError::OutsideRoot(path.to_string())),
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    WorkspacePath::parse(&parts.join("/"))
        .map(Some)
        .map_err(|e| ScopeError::Io { path: path.to_string(), message: e.to_string() })
}

/// The workspace path `path` actually names under the canonical `root`:
/// links and aliases resolved. A path that does not exist yet resolves its
/// nearest existing ancestor and keeps the missing components. `None` is the
/// root itself.
pub fn resolve(root: &Path, path: &WorkspacePath) -> Result<Option<WorkspacePath>, ScopeError> {
    let parts: Vec<&str> = path.components().collect();
    for existing in (0..=parts.len()).rev() {
        let prefix = parts[..existing].iter().fold(root.to_path_buf(), |p, c| p.join(c));
        match fs::symlink_metadata(&prefix) {
            Ok(_) => {
                let canonical = fs::canonicalize(&prefix).map_err(|e| match e.kind() {
                    io::ErrorKind::NotFound => ScopeError::BrokenLink(path.to_string()),
                    _ => ScopeError::Io { path: path.to_string(), message: e.kind().to_string() },
                })?;
                if !canonical.starts_with(root) {
                    return Err(ScopeError::OutsideRoot(path.to_string()));
                }
                let base = relative(root, &canonical, path)?;
                let missing = &parts[existing..];
                if missing.is_empty() {
                    return Ok(base);
                }
                let tail = missing.join("/");
                let full = match base {
                    Some(base) => format!("{}/{tail}", base.as_str()),
                    None => tail,
                };
                return WorkspacePath::parse(&full)
                    .map(Some)
                    .map_err(|e| ScopeError::Io { path: path.to_string(), message: e.to_string() });
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(ScopeError::Io { path: path.to_string(), message: e.kind().to_string() }),
        }
    }
    // `existing == 0` is the root itself, which always exists.
    Err(ScopeError::Io { path: path.to_string(), message: "the project folder is not available".into() })
}

fn is_same_or_under(path: &str, allowed: &str) -> bool {
    path == allowed
        || (path.len() > allowed.len() && path.starts_with(allowed) && path.as_bytes()[allowed.len()] == b'/')
}

fn scope_entries(scope: &TaskScope) -> Vec<WorkspacePath> {
    match scope {
        TaskScope::Workspace => Vec::new(),
        TaskScope::Files { paths } => paths.clone(),
        TaskScope::Document { path } | TaskScope::Selection { path, .. } => vec![path.clone()],
    }
}

/// Whether the resolved path lies inside the scope, comparing against the
/// resolved scope entries (so a scope written with a different spelling of a
/// folder still matches its files).
fn resolved_allows(root: &Path, scope: &TaskScope, resolved: Option<&WorkspacePath>) -> bool {
    if *scope == TaskScope::Workspace {
        return true;
    }
    let Some(resolved) = resolved else { return false };
    scope_entries(scope).iter().any(|entry| {
        let entry = resolve(root, entry).ok().flatten().unwrap_or_else(|| entry.clone());
        match scope {
            TaskScope::Files { .. } => is_same_or_under(resolved.as_str(), entry.as_str()),
            _ => resolved.as_str() == entry.as_str(),
        }
    })
}

/// Check that `path` may be used by a task with `scope` in `root` (canonical):
/// the requested spelling is in scope, it resolves inside the project, and the
/// file it resolves to is in scope. Returns the resolved path.
pub fn check(root: &Path, scope: &TaskScope, path: &WorkspacePath) -> Result<Option<WorkspacePath>, ScopeError> {
    if !scope.allows(path) {
        return Err(ScopeError::OutsideScope { path: path.to_string(), resolved: None });
    }
    let resolved = resolve(root, path)?;
    if !resolved_allows(root, scope, resolved.as_ref()) {
        let target = resolved.map(|r| r.to_string()).unwrap_or_else(|| "the project folder".into());
        return Err(ScopeError::OutsideScope { path: path.to_string(), resolved: Some(target) });
    }
    Ok(resolved)
}

/// Whether listing `dir` (the root when `None`) can show anything in scope,
/// checked on the folder it resolves to.
pub fn check_dir(root: &Path, scope: &TaskScope, dir: Option<&WorkspacePath>) -> Result<(), ScopeError> {
    let Some(dir) = dir else { return Ok(()) };
    if *scope == TaskScope::Workspace {
        return resolve(root, dir).map(|_| ());
    }
    let entries = scope_entries(scope);
    let lexical = scope.allows(dir) || entries.iter().any(|e| is_same_or_under(e.as_str(), dir.as_str()));
    if !lexical {
        return Err(ScopeError::OutsideScope { path: dir.to_string(), resolved: None });
    }
    let resolved = resolve(root, dir)?;
    let Some(resolved) = resolved else { return Ok(()) };
    let visible = entries.iter().any(|entry| {
        let entry = resolve(root, entry).ok().flatten().unwrap_or_else(|| entry.clone());
        let allowed = match scope {
            TaskScope::Files { .. } => is_same_or_under(resolved.as_str(), entry.as_str()),
            _ => resolved.as_str() == entry.as_str(),
        };
        allowed || is_same_or_under(entry.as_str(), resolved.as_str())
    });
    if visible {
        Ok(())
    } else {
        Err(ScopeError::OutsideScope { path: dir.to_string(), resolved: Some(resolved.to_string()) })
    }
}

/// A read filter for builds: true when the task may read `path`.
pub type ReadFilter = Arc<dyn Fn(&WorkspacePath) -> bool + Send + Sync>;

/// The files a task's in-process build may read: those in its scope (checked
/// as in [`check`]).
pub fn read_filter(root: PathBuf, scope: TaskScope) -> ReadFilter {
    Arc::new(move |path: &WorkspacePath| check(&root, &scope, path).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    #[test]
    fn resolves_existing_missing_and_root_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(root.join("public")).unwrap();
        fs::write(root.join("public/notes.txt"), "x").unwrap();
        assert_eq!(resolve(&root, &p("public/notes.txt")).unwrap(), Some(p("public/notes.txt")));
        assert_eq!(resolve(&root, &p("public/new/deeper.typ")).unwrap(), Some(p("public/new/deeper.typ")));
        assert_eq!(resolve(&root, &p("missing.typ")).unwrap(), Some(p("missing.typ")));
        let scope = TaskScope::Files { paths: vec![p("public")] };
        assert!(check(&root, &scope, &p("public/notes.txt")).is_ok());
        assert!(check(&root, &scope, &p("public/new.typ")).is_ok());
        assert!(matches!(check(&root, &scope, &p("private/x")), Err(ScopeError::OutsideScope { resolved: None, .. })));
        assert!(check_dir(&root, &scope, None).is_ok());
        assert!(check_dir(&root, &scope, Some(&p("private"))).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_inside_the_scope_to_a_folder_outside_it_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(root.join("public")).unwrap();
        fs::create_dir_all(root.join("private")).unwrap();
        fs::write(root.join("private/secret.txt"), "s").unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("public").join("link"))
            .arg(root.join("private"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !made {
            eprintln!("skipped: could not create a junction");
            return;
        }
        let scope = TaskScope::Files { paths: vec![p("public")] };
        let refused = check(&root, &scope, &p("public/link/secret.txt")).unwrap_err();
        assert_eq!(
            refused,
            ScopeError::OutsideScope {
                path: "public/link/secret.txt".into(),
                resolved: Some("private/secret.txt".into())
            }
        );
        assert!(check(&root, &scope, &p("public/link/new.txt")).is_err(), "creating through the link is refused");
        assert!(check_dir(&root, &scope, Some(&p("public/link"))).is_err(), "listing through the link is refused");
        // A scope that names the link explicitly covers its target.
        let explicit = TaskScope::Files { paths: vec![p("public/link")] };
        assert!(check(&root, &explicit, &p("public/link/secret.txt")).is_ok());
        let filter = read_filter(root.clone(), scope);
        assert!(!filter(&p("public/link/secret.txt")));
        assert!(!filter(&p("private/secret.txt")));
    }
}
