//! Workspace trust.
//!
//! Trusted native execution runs with the user's full account privileges, so it
//! requires the user to have explicitly trusted the workspace. Trust is stored
//! as canonical directory paths in application data (never in the project).
//! A workspace is trusted when it equals or lies inside a trusted directory.
//! Opening a project never grants trust; only `grant` (called from an explicit
//! user dialog) does.
//!
//! Because trust covers descendants, a trusted directory must be a specific
//! folder: [`exposition_core::roots::too_broad_reason`] refuses drive roots,
//! anything equal to or above the users folder, profile, AppData, TEMP,
//! Desktop/Documents/Downloads, and anything in or above system folders.
//! Entries persisted by older versions that break this rule are ignored.
//!
//! Trust is not a sandbox: trusted native code runs with the user's privileges
//! and can start processes Exposition cannot track (e.g. through WMI or the
//! task scheduler), so it may outlive cancellation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use exposition_core::roots::{BroadDirs, too_broad_reason_in};

use crate::paths::{PathError, canonical_directory, is_within, same_path};
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

/// A trust grant or revoke, announced by
/// [`crate::ExecutionService::subscribe_trust`] whichever surface made it.
///
/// It is only a hint to re-check: trust covers descendants, so a consumer
/// asks [`crate::ExecutionService::trust_status`] for its own folder rather
/// than comparing paths, and the execution service still checks trust when
/// it launches anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TrustChanged {
    /// The directory trusted or revoked (canonical; as stored for a removed
    /// folder whose trust was revoked).
    pub root: String,
    /// `true` for a grant, `false` for a revoke.
    pub granted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrustError {
    #[error(transparent)]
    Path(#[from] crate::paths::PathError),
    #[error("{0}; trust a specific project folder instead")]
    TooBroad(String),
    #[error("could not save trust settings: {0}")]
    Io(String),
}

/// One trusted directory as listed for the settings UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TrustedRoot {
    /// Canonical directory, as stored.
    pub path: String,
    /// When trust was granted (ms since the Unix epoch); `None` for entries
    /// saved before grant times were recorded.
    pub granted_at_ms: Option<u64>,
    /// The directory currently exists.
    pub exists: bool,
    /// Set when the entry is ignored because the current rules consider it
    /// too broad (e.g. saved by an older version); it then trusts nothing.
    pub ignored_reason: Option<String>,
}

/// A location the trust rules refuse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TrustRuleLocation {
    pub description: String,
    pub path: String,
}

/// The rules deciding which directories may be trusted, with the concrete
/// locations they refer to on this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TrustRules {
    pub summary: String,
    /// Refused, as is anything that contains them (folders inside are fine).
    pub personal: Vec<TrustRuleLocation>,
    /// Refused, as is anything inside them or containing them.
    pub system: Vec<TrustRuleLocation>,
}

/// Plain-language summary of [`exposition_core::roots::too_broad_reason`].
pub const TRUST_RULE_SUMMARY: &str = "Trust covers a folder and everything inside it, so only a specific project folder can be trusted. Drive and network-share roots are refused. The personal folders below are refused, as is any folder that contains them; project folders inside them are fine. The system folders below are refused, as is anything inside them or containing them.";

#[derive(Default, Serialize, Deserialize)]
struct TrustFile {
    version: u32,
    roots: Vec<String>,
    /// When each root (keyed as written in `roots`) was trusted, in ms since
    /// the Unix epoch. Older files have no times; older readers ignore them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    granted_at_ms: BTreeMap<String, u64>,
}

/// A stored trusted directory.
#[derive(Clone, Debug)]
struct Entry {
    path: PathBuf,
    granted_at_ms: Option<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub struct TrustStore {
    path: Option<PathBuf>,
    roots: RwLock<Vec<Entry>>,
    /// Broad personal/system locations (from the environment at load time).
    broad: BroadDirs,
    /// Additional directories that may never be trusted, nor anything above them.
    too_broad: Vec<PathBuf>,
}

impl TrustStore {
    pub fn load(path: PathBuf, too_broad: Vec<PathBuf>) -> Self {
        let roots = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<TrustFile>(&b).ok())
            .map(|f| {
                let granted = f.granted_at_ms;
                f.roots
                    .into_iter()
                    .map(|r| Entry { granted_at_ms: granted.get(&r).copied(), path: PathBuf::from(r) })
                    .collect()
            })
            .unwrap_or_default();
        Self { path: Some(path), roots: RwLock::new(roots), broad: BroadDirs::from_env(), too_broad }
    }

    pub fn in_memory(too_broad: Vec<PathBuf>) -> Self {
        Self { path: None, roots: RwLock::new(Vec::new()), broad: BroadDirs::from_env(), too_broad }
    }

    /// Why a canonical directory may not be trusted, if it may not.
    pub fn too_broad_reason(&self, canonical: &Path) -> Option<String> {
        too_broad_reason_in(canonical, &self.broad).or_else(|| {
            self.too_broad.iter().find(|b| is_within(b, canonical)).map(|b| {
                if same_path(b, canonical) {
                    format!("{} may not be trusted", canonical.display())
                } else {
                    format!("{} contains {}, which may not be trusted", canonical.display(), b.display())
                }
            })
        })
    }

    /// Trust status of a canonical directory.
    pub fn status_of(&self, canonical: &Path) -> TrustStatus {
        let roots = self.roots.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Entries that are too broad (e.g. persisted by an older version) never count.
        let via = roots
            .iter()
            .map(|r| &r.path)
            .filter(|r| is_within(canonical, r) && self.too_broad_reason(r).is_none())
            .min_by_key(|r| r.components().count());
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
        if let Some(reason) = self.too_broad_reason(&canonical) {
            return Err(TrustError::TooBroad(reason));
        }
        {
            let mut roots = self.roots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !roots.iter().any(|r| same_path(&r.path, &canonical)) {
                roots.push(Entry { path: canonical.clone(), granted_at_ms: Some(now_ms()) });
            }
            self.persist(&roots)?;
        }
        Ok(self.status_of(&canonical))
    }

    /// Remove trust for the directory and anything trusted beneath it. An
    /// ancestor that is itself trusted keeps covering it (see `trusted_via`).
    /// A stored directory that no longer exists (so cannot be canonicalized)
    /// is removed by its stored path.
    pub fn revoke(&self, raw: &str) -> Result<TrustStatus, TrustError> {
        let canonical = match canonical_directory(raw) {
            Ok(canonical) => canonical,
            Err(error @ (PathError::Missing(_) | PathError::NotDirectory(_))) => {
                let stored = Path::new(raw);
                let mut roots = self.roots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
                let before = roots.len();
                roots.retain(|r| !is_within(&r.path, stored));
                if roots.len() == before {
                    return Err(error.into());
                }
                self.persist(&roots)?;
                return Ok(TrustStatus { root: raw.to_owned(), trusted: false, trusted_via: None });
            }
            Err(error) => return Err(error.into()),
        };
        {
            let mut roots = self.roots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
            roots.retain(|r| !is_within(&r.path, &canonical));
            self.persist(&roots)?;
        }
        Ok(self.status_of(&canonical))
    }

    /// Every stored trusted directory, in the order trust was granted.
    pub fn list(&self) -> Vec<TrustedRoot> {
        let roots = self.roots.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        roots
            .iter()
            .map(|r| TrustedRoot {
                path: r.path.display().to_string(),
                granted_at_ms: r.granted_at_ms,
                exists: r.path.is_dir(),
                ignored_reason: self.too_broad_reason(&r.path),
            })
            .collect()
    }

    /// The rules applied by [`TrustStore::too_broad_reason`], with this
    /// computer's locations (duplicates removed).
    pub fn rules(&self) -> TrustRules {
        fn locations<'a>(list: impl Iterator<Item = (&'a Path, &'a str)>) -> Vec<TrustRuleLocation> {
            let mut out: Vec<(PathBuf, TrustRuleLocation)> = Vec::new();
            for (path, description) in list {
                if !out.iter().any(|(p, _)| same_path(p, path)) {
                    out.push((
                        path.to_path_buf(),
                        TrustRuleLocation { description: description.to_owned(), path: path.display().to_string() },
                    ));
                }
            }
            out.into_iter().map(|(_, l)| l).collect()
        }
        let personal = locations(self.broad.personal.iter().map(|l| (l.path.as_path(), l.description)));
        let system = locations(
            self.broad
                .system
                .iter()
                .map(|l| (l.path.as_path(), l.description))
                .chain(self.too_broad.iter().map(|p| (p.as_path(), "a protected folder"))),
        );
        TrustRules { summary: TRUST_RULE_SUMMARY.to_owned(), personal, system }
    }

    fn persist(&self, roots: &[Entry]) -> Result<(), TrustError> {
        if let Some(path) = &self.path {
            let file = TrustFile {
                version: 1,
                roots: roots.iter().map(|r| r.path.display().to_string()).collect(),
                granted_at_ms: roots
                    .iter()
                    .filter_map(|r| Some((r.path.display().to_string(), r.granted_at_ms?)))
                    .collect(),
            };
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
    fn refuses_ancestors_of_broad_roots_and_ignores_persisted_broad_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("trust.json");
        let store = TrustStore::load(file.clone(), vec![]);
        let profile = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap();
        let mut broad = vec![profile.clone(), profile.parent().unwrap().to_path_buf()];
        if cfg!(windows) {
            for var in ["TEMP", "LOCALAPPDATA", "APPDATA", "ProgramData"] {
                broad.push(PathBuf::from(std::env::var_os(var).unwrap()));
            }
            broad.push(profile.join("AppData"));
            broad.push(PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"));
        }
        for root in broad.iter().filter(|r| r.is_dir()) {
            let result = store.grant(&root.display().to_string());
            assert!(matches!(result, Err(TrustError::TooBroad(_))), "{} was trusted: {result:?}", root.display());
        }
        // A project inside TEMP is still trustable, and a broad entry written by
        // an older version does not make it trusted.
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let users = profile.parent().unwrap().display().to_string();
        std::fs::write(&file, serde_json::json!({ "version": 1, "roots": [users] }).to_string()).unwrap();
        let legacy = TrustStore::load(file, vec![]);
        assert!(!legacy.status(&project.display().to_string()).unwrap().trusted);
        assert!(legacy.grant(&project.display().to_string()).unwrap().trusted);
    }

    #[test]
    fn list_records_grant_times_and_reads_legacy_files() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("trust.json");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let a_canonical = canonicalize(&a).unwrap().display().to_string();
        // A file written before grant times existed still loads; its entry has no time.
        std::fs::write(&file, serde_json::json!({ "version": 1, "roots": [a_canonical] }).to_string()).unwrap();
        let store = TrustStore::load(file.clone(), vec![]);
        let before = now_ms();
        store.grant(&b.display().to_string()).unwrap();
        let listed = store.list();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].path, a_canonical);
        assert_eq!(listed[0].granted_at_ms, None);
        assert!(listed[0].exists && listed[0].ignored_reason.is_none());
        let granted = listed[1].granted_at_ms.expect("grant time recorded");
        assert!(granted >= before && granted <= now_ms());
        // Times persist, and the file stays readable by the older format (roots list).
        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(raw["roots"].as_array().unwrap().len(), 2);
        assert_eq!(TrustStore::load(file.clone(), vec![]).list(), listed);
        // Re-granting an already trusted folder keeps the original time.
        store.grant(&b.display().to_string()).unwrap();
        assert_eq!(store.list()[1].granted_at_ms, Some(granted));
    }

    #[test]
    fn list_marks_ignored_broad_entries_and_rules_name_locations() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("trust.json");
        let profile = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap();
        let profile = canonicalize(&profile).unwrap().display().to_string();
        std::fs::write(&file, serde_json::json!({ "version": 1, "roots": [profile] }).to_string()).unwrap();
        let store = TrustStore::load(file, vec![]);
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].ignored_reason.is_some(), "{listed:?}");
        let rules = store.rules();
        assert!(rules.summary.contains("project folder"));
        assert!(rules.personal.iter().any(|l| same_path(Path::new(&l.path), Path::new(&profile))), "{rules:?}");
        if cfg!(windows) {
            assert!(rules.system.iter().any(|l| l.description.contains("Windows")), "{rules:?}");
        }
        // No duplicates (canonical forms of the same folder are merged).
        for list in [&rules.personal, &rules.system] {
            for (i, l) in list.iter().enumerate() {
                assert!(!list[i + 1..].iter().any(|m| same_path(Path::new(&m.path), Path::new(&l.path))));
            }
        }
    }

    #[test]
    fn revoke_removes_entries_whose_folder_was_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("trust.json");
        let project = tmp.path().join("gone");
        std::fs::create_dir_all(&project).unwrap();
        let store = TrustStore::load(file.clone(), vec![]);
        let stored = store.grant(&project.display().to_string()).unwrap().root;
        std::fs::remove_dir_all(&project).unwrap();
        assert!(!store.list()[0].exists);
        let revoked = store.revoke(&stored).unwrap();
        assert!(!revoked.trusted);
        assert!(store.list().is_empty());
        assert!(TrustStore::load(file, vec![]).list().is_empty());
        // A missing folder that was never trusted is still an error.
        assert!(matches!(store.revoke(&stored), Err(TrustError::Path(PathError::Missing(_)))));
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
