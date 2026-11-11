//! Canonical paths, containment, and the registry of allowed execution roots.
//!
//! A job's working directory must be absolute, free of `..` components, exist,
//! and — after canonicalization, which resolves symlinks and junctions — lie
//! inside a registered root. Roots are either workspace roots (opened
//! projects) or application-owned directories (build output, scratch, probe
//! directories). An application-owned directory may be bound to the workspace
//! it serves (e.g. a snapshot build directory) so trust decisions follow the
//! project, not the directory.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path must be absolute: {0}")]
    NotAbsolute(String),
    #[error("path may not contain `..` components: {0}")]
    Traversal(String),
    #[error("directory does not exist or is not accessible: {0}")]
    Missing(String),
    #[error("not a directory: {0}")]
    NotDirectory(String),
}

/// Canonicalize an existing path and strip the Windows `\\?\` prefix when the
/// result is still an ordinary path (many tools mishandle verbatim paths).
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    Ok(simplify(std::fs::canonicalize(path)?))
}

/// Remove a verbatim prefix (`\\?\C:\...` → `C:\...`, `\\?\UNC\s\h` → `\\s\h`)
/// when doing so cannot change the meaning of the path.
pub fn simplify(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = match path.to_str() {
            Some(t) => t,
            None => return path,
        };
        let rest = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            let bytes = rest.as_bytes();
            if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
                rest.to_owned()
            } else {
                return path;
            }
        } else {
            return path;
        };
        // Long paths and names Win32 would reinterpret stay verbatim.
        let reinterpretable =
            rest.len() >= 260 || rest.split('\\').any(|c| c.ends_with('.') || c.ends_with(' ') || is_reserved_name(c));
        if reinterpretable { path } else { PathBuf::from(rest) }
    }
    #[cfg(not(windows))]
    {
        path
    }
}

#[cfg(windows)]
fn is_reserved_name(component: &str) -> bool {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
        "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = component.split('.').next().unwrap_or(component).to_ascii_uppercase();
    RESERVED.contains(&stem.as_str())
}

fn component_eq(a: &Component<'_>, b: &Component<'_>) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// True when `path` equals `root` or lies beneath it (component-wise; ASCII
/// case-insensitive on Windows). Both should be canonical.
pub fn is_within(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    for root_component in root.components() {
        match path_components.next() {
            Some(c) if component_eq(&c, &root_component) => {}
            _ => return false,
        }
    }
    true
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    a.components().count() == b.components().count() && is_within(a, b)
}

/// Validate a requested working directory lexically, then canonicalize it.
pub fn canonical_directory(raw: &str) -> Result<PathBuf, PathError> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(PathError::NotAbsolute(raw.to_owned()));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(PathError::Traversal(raw.to_owned()));
    }
    let canonical = canonicalize(path).map_err(|_| PathError::Missing(raw.to_owned()))?;
    if !canonical.is_dir() {
        return Err(PathError::NotDirectory(raw.to_owned()));
    }
    Ok(canonical)
}

/// What a registered root is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootKind {
    /// An open project directory.
    Workspace,
    /// An application-owned directory. `owner` is the canonical workspace root
    /// it serves, if any (e.g. a snapshot build directory for that project).
    AppOwned { owner: Option<PathBuf> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    pub kind: RootKind,
}

/// The set of directories jobs may run in.
#[derive(Debug, Default)]
pub struct Roots {
    roots: Vec<Root>,
}

impl Roots {
    /// Register a root; returns its canonical path. Re-registering replaces the kind.
    pub fn register(&mut self, path: &Path, kind: RootKind) -> Result<PathBuf, PathError> {
        let canonical = canonical_directory(&path.display().to_string())?;
        self.roots.retain(|r| !same_path(&r.path, &canonical));
        self.roots.push(Root { path: canonical.clone(), kind });
        Ok(canonical)
    }

    pub fn unregister(&mut self, path: &Path) {
        let canonical = canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.roots.retain(|r| !same_path(&r.path, &canonical));
    }

    /// The most specific registered root containing a canonical path.
    pub fn containing(&self, canonical: &Path) -> Option<&Root> {
        self.roots.iter().filter(|r| is_within(canonical, &r.path)).max_by_key(|r| r.path.components().count())
    }

    pub fn workspace_roots(&self) -> impl Iterator<Item = &Path> {
        self.roots.iter().filter(|r| r.kind == RootKind::Workspace).map(|r| r.path.as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_is_component_wise() {
        let root = if cfg!(windows) { PathBuf::from(r"C:\work\proj") } else { PathBuf::from("/work/proj") };
        let inside = root.join("sub").join("dir");
        let sibling =
            if cfg!(windows) { PathBuf::from(r"C:\work\proj-evil") } else { PathBuf::from("/work/proj-evil") };
        assert!(is_within(&inside, &root));
        assert!(is_within(&root, &root));
        assert!(!is_within(&sibling, &root));
        assert!(!is_within(root.parent().unwrap(), &root));
        if cfg!(windows) {
            assert!(is_within(Path::new(r"c:\WORK\Proj\x"), &root), "case-insensitive on Windows");
        }
    }

    #[test]
    fn canonical_directory_rejects_relative_traversal_and_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().display().to_string();
        assert!(matches!(canonical_directory("relative/dir"), Err(PathError::NotAbsolute(_))));
        let traversal = format!("{base}{}..{}x", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR);
        assert!(matches!(canonical_directory(&traversal), Err(PathError::Traversal(_))));
        let missing = tmp.path().join("missing").display().to_string();
        assert!(matches!(canonical_directory(&missing), Err(PathError::Missing(_))));
        let file = tmp.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(canonical_directory(&file.display().to_string()), Err(PathError::NotDirectory(_))));
        let ok = canonical_directory(&base).unwrap();
        assert!(!ok.display().to_string().starts_with(r"\\?\"), "verbatim prefix stripped: {}", ok.display());
    }

    #[test]
    fn roots_pick_most_specific() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let build = ws.join("build");
        std::fs::create_dir_all(&build).unwrap();
        let mut roots = Roots::default();
        let ws_c = roots.register(&ws, RootKind::Workspace).unwrap();
        let build_c = roots.register(&build, RootKind::AppOwned { owner: Some(ws_c.clone()) }).unwrap();
        assert_eq!(roots.containing(&build_c.join("x")).unwrap().path, build_c);
        assert_eq!(roots.containing(&ws_c.join("src")).unwrap().path, ws_c);
        assert!(roots.containing(&canonicalize(tmp.path()).unwrap()).is_none());
        roots.unregister(&build);
        assert_eq!(roots.containing(&build_c).unwrap().path, ws_c);
        assert_eq!(roots.workspace_roots().count(), 1);
    }
}
