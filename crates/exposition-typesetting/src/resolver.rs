//! Safe reading of workspace files that are not open as documents.
//!
//! A [`SourceResolver`] maps a validated [`WorkspacePath`] to bytes. The disk
//! implementation canonicalizes every path and rejects anything that resolves
//! outside the canonical workspace root (symlink or junction escapes). Open
//! buffers never reach the resolver: build snapshots take them from the
//! document service first.
//!
//! The workspace service will provide the application-wide resolver; this
//! minimal implementation keeps typesetting independent until then.

use std::fmt;
use std::path::{Path, PathBuf};

use exposition_core::path::WorkspacePath;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("file not found: {0}")]
    NotFound(WorkspacePath),
    #[error("{0} resolves outside the workspace")]
    OutsideWorkspace(WorkspacePath),
    #[error("{0} is not a file")]
    NotAFile(WorkspacePath),
    #[error("could not read {path}: {message}")]
    Io { path: WorkspacePath, message: String },
}

/// Reads workspace files by workspace-relative path.
pub trait SourceResolver: Send + Sync + fmt::Debug {
    /// Read the exact bytes of a workspace file.
    fn read(&self, path: &WorkspacePath) -> Result<Vec<u8>, ResolveError>;
    /// The canonical workspace root (used for exports and diagnostics).
    fn root(&self) -> &Path;
}

/// Reads files below a canonical workspace root on disk.
#[derive(Debug, Clone)]
pub struct DiskResolver {
    root: PathBuf,
}

impl DiskResolver {
    /// Create a resolver for `root`, which must be an existing directory.
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = dunce_canonicalize(root.as_ref())?;
        if !root.is_dir() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotADirectory, "workspace root is not a directory"));
        }
        Ok(Self { root })
    }

    /// Resolve a workspace path to a canonical path inside the root.
    pub fn resolve(&self, path: &WorkspacePath) -> Result<PathBuf, ResolveError> {
        let mut full = self.root.clone();
        for component in path.components() {
            full.push(component);
        }
        let canonical = dunce_canonicalize(&full).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ResolveError::NotFound(path.clone()),
            _ => ResolveError::Io { path: path.clone(), message: e.kind().to_string() },
        })?;
        if !canonical.starts_with(&self.root) {
            return Err(ResolveError::OutsideWorkspace(path.clone()));
        }
        Ok(canonical)
    }
}

impl SourceResolver for DiskResolver {
    fn read(&self, path: &WorkspacePath) -> Result<Vec<u8>, ResolveError> {
        let canonical = self.resolve(path)?;
        if !canonical.is_file() {
            return Err(ResolveError::NotAFile(path.clone()));
        }
        std::fs::read(&canonical).map_err(|e| ResolveError::Io { path: path.clone(), message: e.kind().to_string() })
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

/// Canonicalize without the Windows `\\?\` verbatim prefix when the plain form
/// is equivalent, so prefix checks and messages use ordinary paths.
pub(crate) fn dunce_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let text = canonical.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\")
            && rest.len() < 260
        {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_inside_and_rejects_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.typ"), "hi").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let resolver = DiskResolver::new(dir.path()).unwrap();
        assert_eq!(resolver.read(&WorkspacePath::parse("a.typ").unwrap()).unwrap(), b"hi");
        assert!(matches!(resolver.read(&WorkspacePath::parse("b.typ").unwrap()), Err(ResolveError::NotFound(_))));
        assert!(matches!(resolver.read(&WorkspacePath::parse("sub").unwrap()), Err(ResolveError::NotAFile(_))));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_junction_escape() {
        // A directory junction inside the workspace pointing outside must not
        // be followed (junctions need no privileges on Windows).
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.typ"), "secret").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("escape");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap();
        if !status.status.success() {
            eprintln!("mklink /J unavailable; skipping");
            return;
        }
        let resolver = DiskResolver::new(dir.path()).unwrap();
        let result = resolver.read(&WorkspacePath::parse("escape/secret.typ").unwrap());
        assert!(matches!(result, Err(ResolveError::OutsideWorkspace(_))), "{result:?}");
    }
}
