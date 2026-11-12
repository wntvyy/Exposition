//! Crash-recovery journal for unsaved buffers.
//!
//! Layout: `<data dir>/recovery/<workspace key>/`.
//! - `live-<path key>.json` — the current session's journal for one dirty
//!   buffer, rewritten (atomically) shortly after changes and removed when the
//!   buffer is saved, reverted to its saved state, or explicitly discarded.
//! - `rec-<path key>-<millis>.json` — a journal left behind by an earlier
//!   session. When a workspace opens, its `live-*` files are renamed to `rec-*`
//!   and offered for restore; they are removed only after an explicit restore
//!   (which first re-journals the restored buffer) or discard.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use exposition_core::document::TextEncoding;
use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};

use crate::fsutil::{now_millis, sha256_hex, write_atomic};

pub const JOURNAL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub version: u32,
    pub path: WorkspacePath,
    /// Hash of the file on disk the edits were based on (`None`: file did not exist).
    pub base_hash: Option<String>,
    pub revision: u64,
    pub encoding: TextEncoding,
    pub saved_at: u64,
    pub text: String,
}

/// Stable key for a workspace root (case-insensitive on Windows).
pub fn workspace_key(display_root: &str) -> String {
    let normalized = if cfg!(windows) { display_root.to_lowercase() } else { display_root.to_owned() };
    sha256_hex(normalized.as_bytes())[..16].to_owned()
}

/// Journal key of a document. Documents are recorded under their canonical
/// workspace path (one document per physical file), so the exact path is the
/// identity; it is not case-folded, so distinct files in case-sensitive folders
/// never share a journal.
fn path_key(path: &WorkspacePath) -> String {
    sha256_hex(path.as_str().as_bytes())[..16].to_owned()
}

#[derive(Debug, Clone)]
pub struct RecoveryStore {
    dir: PathBuf,
}

impl RecoveryStore {
    pub fn new(data_dir: &Path, workspace_key: &str) -> Self {
        Self { dir: data_dir.join("recovery").join(workspace_key) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn live_file(&self, path: &WorkspacePath) -> PathBuf {
        self.dir.join(format!("live-{}.json", path_key(path)))
    }

    /// Atomically write the live journal for one buffer.
    pub fn write_live(&self, journal: &Journal) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_vec(journal).map_err(io::Error::other)?;
        write_atomic(&self.live_file(&journal.path), &json)
    }

    pub fn remove_live(&self, path: &WorkspacePath) -> io::Result<()> {
        match fs::remove_file(self.live_file(path)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Turn every live journal of an earlier session into a recovery entry.
    pub fn promote_live(&self) -> io::Result<()> {
        let Ok(read) = fs::read_dir(&self.dir) else { return Ok(()) };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_prefix("live-").and_then(|n| n.strip_suffix(".json")) {
                let target = self.dir.join(format!("rec-{stem}-{}.json", now_millis()));
                fs::rename(entry.path(), target)?;
            } else if name.ends_with(".tmp") {
                // Interrupted journal write: the previous journal is intact.
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    /// Recovery entries (journals of earlier sessions). A journal that cannot be
    /// read (corrupt, truncated, invalid path, or from a newer version) is
    /// quarantined (renamed `bad-...`, never parsed again, bytes kept) and
    /// reported as unreadable so the user can inspect or discard it.
    pub fn entries(&self) -> Vec<StoredEntry> {
        let Ok(read) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json").filter(|id| is_entry_id(id)) else { continue };
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if id.starts_with("bad-") {
                let reason = fs::read_to_string(self.dir.join(format!("{id}.reason")))
                    .unwrap_or_else(|_| "The recovery data is unreadable.".into());
                let path = fs::read(entry.path()).ok().and_then(|bytes| salvage_path(&bytes));
                out.push(StoredEntry::Unreadable { id: id.to_owned(), path, reason, location: entry.path(), modified });
                continue;
            }
            let bytes = fs::read(entry.path()).unwrap_or_default();
            match parse(&bytes) {
                Ok(journal) => out.push(StoredEntry::Valid { id: id.to_owned(), journal }),
                Err(reason) => {
                    let path = salvage_path(&bytes);
                    // Quarantine: keep the bytes, never parse them again.
                    let bad_id = format!("bad-{}", &id["rec-".len()..]);
                    let quarantined = self.dir.join(format!("{bad_id}.json"));
                    let (id, location) = match fs::rename(entry.path(), &quarantined) {
                        Ok(()) => {
                            let _ = fs::write(self.dir.join(format!("{bad_id}.reason")), &reason);
                            (bad_id, quarantined)
                        }
                        Err(_) => (id.to_owned(), entry.path()),
                    };
                    out.push(StoredEntry::Unreadable { id, path, reason, location, modified });
                }
            }
        }
        out.sort_by(|a, b| b.saved_at().cmp(&a.saved_at()).then(a.id().cmp(b.id())));
        out
    }

    pub fn read_entry(&self, id: &str) -> Option<Journal> {
        if !is_entry_id(id) || id.starts_with("bad-") {
            return None;
        }
        fs::read(self.dir.join(format!("{id}.json"))).ok().and_then(|bytes| parse(&bytes).ok())
    }

    pub fn remove_entry(&self, id: &str) -> io::Result<()> {
        if !is_entry_id(id) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid recovery id"));
        }
        let _ = fs::remove_file(self.dir.join(format!("{id}.reason")));
        match fs::remove_file(self.dir.join(format!("{id}.json"))) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// A recovery journal on disk.
#[derive(Debug, Clone)]
pub enum StoredEntry {
    Valid {
        id: String,
        journal: Journal,
    },
    Unreadable {
        id: String,
        /// The file the journal belonged to, when it can still be read from the damaged data.
        path: Option<WorkspacePath>,
        reason: String,
        location: PathBuf,
        modified: u64,
    },
}

impl StoredEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Valid { id, .. } | Self::Unreadable { id, .. } => id,
        }
    }

    fn saved_at(&self) -> u64 {
        match self {
            Self::Valid { journal, .. } => journal.saved_at,
            Self::Unreadable { modified, .. } => *modified,
        }
    }
}

/// Best effort: the workspace path named in damaged journal data (validated).
fn salvage_path(bytes: &[u8]) -> Option<WorkspacePath> {
    let text = String::from_utf8_lossy(bytes);
    let start = text.find("\"path\":")? + "\"path\":".len();
    let rest = text[start..].trim_start();
    // Deserialize just the JSON string that follows (handles escapes).
    let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<String>();
    let raw = stream.next()?.ok()?;
    WorkspacePath::parse(&raw).ok()
}

fn parse(bytes: &[u8]) -> Result<Journal, String> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    match serde_json::from_slice::<Version>(bytes) {
        Ok(v) if v.version > JOURNAL_VERSION => {
            return Err(format!("It was written by a newer version of Exposition (format {}).", v.version));
        }
        Ok(v) if v.version != JOURNAL_VERSION => return Err(format!("Unsupported recovery format {}.", v.version)),
        _ => {}
    }
    serde_json::from_slice::<Journal>(bytes).map_err(|e| format!("The recovery data is damaged ({e})."))
}

/// `rec-<16 hex>-<digits>` (or `bad-...` once quarantined): ids are never used as raw paths.
pub fn is_entry_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("rec-").or_else(|| id.strip_prefix("bad-")) else { return false };
    let Some((key, millis)) = rest.split_once('-') else { return false };
    key.len() == 16
        && key.bytes().all(|b| b.is_ascii_hexdigit())
        && !millis.is_empty()
        && millis.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(path: &str, text: &str) -> Journal {
        Journal {
            version: JOURNAL_VERSION,
            path: WorkspacePath::parse(path).unwrap(),
            base_hash: Some("abc".into()),
            revision: 3,
            encoding: TextEncoding::Utf8,
            saved_at: now_millis(),
            text: text.into(),
        }
    }

    #[test]
    fn live_journals_become_entries_on_promotion() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(dir.path(), "k");
        store.write_live(&journal("a.typ", "unsaved a")).unwrap();
        store.write_live(&journal("b/c.tex", "unsaved c")).unwrap();
        store.write_live(&journal("a.typ", "unsaved a, newer")).unwrap();
        store.remove_live(&WorkspacePath::parse("b/c.tex").unwrap()).unwrap();
        assert!(store.entries().is_empty(), "live journals are not entries until promoted");

        store.promote_live().unwrap();
        let entries = store.entries();
        assert_eq!(entries.len(), 1);
        let StoredEntry::Valid { id, journal } = entries[0].clone() else { panic!("valid entry expected") };
        assert_eq!(journal.text, "unsaved a, newer");
        assert!(is_entry_id(&id));
        assert_eq!(store.read_entry(&id).unwrap().path.as_str(), "a.typ");
        store.remove_entry(&id).unwrap();
        assert!(store.entries().is_empty());
    }

    #[test]
    fn unreadable_journals_are_quarantined_and_reported_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(dir.path(), "k");
        store.write_live(&journal("ok.typ", "intact")).unwrap();
        fs::write(store.dir().join("live-0123456789abcdef.json"), b"{not json").unwrap();
        fs::write(
            store.dir().join("rec-1123456789abcdef-5.json"),
            br#"{"version":99,"path":"a.txt","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}"#,
        )
        .unwrap();
        fs::write(
            store.dir().join("rec-2123456789abcdef-5.json"),
            br#"{"version":1,"path":"../../escape.txt","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}"#,
        )
        .unwrap();
        store.promote_live().unwrap();
        let entries = store.entries();
        assert_eq!(entries.len(), 4, "{entries:?}");
        let valid: Vec<_> = entries.iter().filter(|e| matches!(e, StoredEntry::Valid { .. })).collect();
        assert_eq!(valid.len(), 1);
        let bad: Vec<_> = entries
            .iter()
            .filter_map(|e| match e {
                StoredEntry::Unreadable { id, reason, location, .. } => {
                    Some((id.clone(), reason.clone(), location.clone()))
                }
                StoredEntry::Valid { .. } => None,
            })
            .collect();
        assert_eq!(bad.len(), 3);
        for (id, _, location) in &bad {
            assert!(id.starts_with("bad-") && is_entry_id(id), "{id}");
            assert!(location.exists(), "quarantined bytes are kept");
            assert!(store.read_entry(id).is_none());
        }
        assert!(bad.iter().any(|(_, reason, _)| reason.contains("newer version")));
        // The damaged data still names its file where possible; invalid paths are not trusted.
        let named: Vec<String> = entries
            .iter()
            .filter_map(|e| match e {
                StoredEntry::Unreadable { path: Some(path), .. } => Some(path.as_str().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(named, vec!["a.txt".to_owned()]);
        // Listing again neither re-quarantines nor loses anything.
        assert_eq!(store.entries().len(), 4);
        for (id, _, location) in &bad {
            store.remove_entry(id).unwrap();
            assert!(!location.exists());
        }
        assert_eq!(store.entries().len(), 1);
    }

    #[test]
    fn entry_ids_cannot_name_other_files() {
        for bad in ["../x", "rec-..", "rec-0123456789abcdef-", "rec-0123456789abcdeg-1", "live-0123456789abcdef"] {
            assert!(!is_entry_id(bad), "{bad}");
        }
        assert!(is_entry_id("rec-0123456789abcdef-1731000000000"));
        let dir = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(dir.path(), "k");
        assert!(store.remove_entry("../../settings").is_err());
        assert!(store.read_entry("../../settings").is_none());
    }

    #[test]
    fn workspace_keys_are_stable_and_case_insensitive_on_windows() {
        assert_eq!(workspace_key(r"C:\Proj"), workspace_key(r"C:\Proj"));
        if cfg!(windows) {
            assert_eq!(workspace_key(r"C:\Proj"), workspace_key(r"c:\proj"));
        }
        assert_ne!(workspace_key(r"C:\a"), workspace_key(r"C:\b"));
    }
}
