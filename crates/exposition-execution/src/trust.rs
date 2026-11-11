//! Workspace trust.
//!
//! Trusted native execution runs with the user's full account privileges, so it
//! requires the user to have explicitly trusted the workspace. Trust is stored
//! as canonical directory paths in application data (never in the project).
//! A workspace is trusted when it equals or lies inside a trusted directory.
//! Opening a project never grants trust; only `grant` (called from an explicit
//! user dialog) does.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::paths::{canonical_directory, is_within, same_path};
use crate::settings::write_json_atomic;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TrustStatus {
    /// Canonical form of the queried directory.
    pub root: String,
    pub trusted: bool,
    /// The trusted directory that covers `root` (itself or an ancestor).
    pub trusted_via: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrustError {
    #[error(transparent)]
    Path(#[from] crate::paths::PathError),
    #[error("{0} is too broad to trust; trust a specific project folder instead")]
    TooBroad(String),
    #[error("could not save trust settings: {0}")]
    Io(String),
}

#[derive(Default, Serialize, Deserialize)]
struct TrustFile {
    version: u32,
    roots: Vec<String>,
}

pub struct TrustStore {
    path: Option<PathBuf>,
    roots: RwLock<Vec<PathBuf>>,
    /// Directories that may never be trusted wholesale.
    too_broad: Vec<PathBuf>,
}

impl TrustStore {
    pub fn load(path: PathBuf, too_broad: Vec<PathBuf>) -> Self {
        let roots = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<TrustFile>(&b).ok())
            .map(|f| f.roots.into_iter().map(PathBuf::from).collect())
            .unwrap_or_default();
        Self { path: Some(path), roots: RwLock::new(roots), too_broad }
    }

    pub fn in_memory(too_broad: Vec<PathBuf>) -> Self {
        Self { path: None, roots: RwLock::new(Vec::new()), too_broad }
    }

    /// Trust status of a canonical directory.
    pub fn status_of(&self, canonical: &Path) -> TrustStatus {
        let roots = self.roots.read().expect("trust lock");
        let via = roots.iter().filter(|r| is_within(canonical, r)).min_by_key(|r| r.components().count());
        TrustStatus {
            root: canonical.display().to_string(),
            trusted: via.is_some(),
            trusted_via: via.map(|p| p.display().to_string()),
        }
    }

    pub fn is_trusted(&self, canonical: &Path) -> bool {
        self.status_of(canonical).trusted
    }

    pub fn status(&self, raw: &str) -> Result<TrustStatus, TrustError> {
        Ok(self.status_of(&canonical_directory(raw)?))
    }

    pub fn grant(&self, raw: &str) -> Result<TrustStatus, TrustError> {
        let canonical = canonical_directory(raw)?;
        if canonical.parent().is_none() || self.too_broad.iter().any(|b| same_path(b, &canonical)) {
            return Err(TrustError::TooBroad(canonical.display().to_string()));
        }
        {
            let mut roots = self.roots.write().expect("trust lock");
            if !roots.iter().any(|r| same_path(r, &canonical)) {
                roots.push(canonical.clone());
            }
            self.persist(&roots)?;
        }
        Ok(self.status_of(&canonical))
    }

    /// Remove trust for the directory and anything trusted beneath it. An
    /// ancestor that is itself trusted keeps covering it (see `trusted_via`).
    pub fn revoke(&self, raw: &str) -> Result<TrustStatus, TrustError> {
        let canonical = canonical_directory(raw)?;
        {
            let mut roots = self.roots.write().expect("trust lock");
            roots.retain(|r| !is_within(r, &canonical));
            self.persist(&roots)?;
        }
        Ok(self.status_of(&canonical))
    }

    fn persist(&self, roots: &[PathBuf]) -> Result<(), TrustError> {
        if let Some(path) = &self.path {
            let file = TrustFile { version: 1, roots: roots.iter().map(|p| p.display().to_string()).collect() };
            write_json_atomic(path, &file).map_err(|e| TrustError::Io(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::canonicalize;

    #[test]
    fn grant_revoke_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("papers").join("p1");
        std::fs::create_dir_all(&project).unwrap();
        let file = tmp.path().join("trust.json");
        let store = TrustStore::load(file.clone(), vec![]);
        let raw = project.display().to_string();
        assert!(!store.status(&raw).unwrap().trusted);
        let granted = store.grant(&raw).unwrap();
        assert!(granted.trusted);
        assert_eq!(PathBuf::from(&granted.root), canonicalize(&project).unwrap());
        // Persisted and reloaded.
        let reloaded = TrustStore::load(file.clone(), vec![]);
        assert!(reloaded.status(&raw).unwrap().trusted);
        // Descendants are covered; siblings and ancestors are not.
        let sub = project.join("chapter");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(reloaded.status(&sub.display().to_string()).unwrap().trusted);
        assert!(!reloaded.status(&tmp.path().join("papers").display().to_string()).unwrap().trusted);
        let revoked = reloaded.revoke(&raw).unwrap();
        assert!(!revoked.trusted);
        assert!(!TrustStore::load(file, vec![]).status(&raw).unwrap().trusted);
    }

    #[test]
    fn refuses_broad_or_invalid_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let home = canonicalize(tmp.path()).unwrap();
        let store = TrustStore::in_memory(vec![home.clone()]);
        assert!(matches!(store.grant(&home.display().to_string()), Err(TrustError::TooBroad(_))));
        let root = home.ancestors().last().unwrap().to_path_buf();
        assert!(matches!(store.grant(&root.display().to_string()), Err(TrustError::TooBroad(_))));
        assert!(matches!(store.grant("relative"), Err(TrustError::Path(_))));
    }
}
