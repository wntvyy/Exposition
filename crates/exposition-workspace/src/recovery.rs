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

fn path_key(path: &WorkspacePath) -> String {
    let normalized = if cfg!(windows) { path.as_str().to_lowercase() } else { path.as_str().to_owned() };
    sha256_hex(normalized.as_bytes())[..16].to_owned()
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

    /// Recovery entries (journals of earlier sessions) with their ids.
    pub fn entries(&self) -> Vec<(String, Journal)> {
        let Ok(read) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json").filter(|id| is_entry_id(id)) else { continue };
            if let Some(journal) = fs::read(entry.path()).ok().and_then(|bytes| parse(&bytes)) {
                out.push((id.to_owned(), journal));
            }
        }
        out.sort_by(|a, b| b.1.saved_at.cmp(&a.1.saved_at).then(a.0.cmp(&b.0)));
        out
    }

    pub fn read_entry(&self, id: &str) -> Option<Journal> {
        if !is_entry_id(id) {
            return None;
        }
        fs::read(self.dir.join(format!("{id}.json"))).ok().and_then(|bytes| parse(&bytes))
    }

    pub fn remove_entry(&self, id: &str) -> io::Result<()> {
        if !is_entry_id(id) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid recovery id"));
        }
        match fs::remove_file(self.dir.join(format!("{id}.json"))) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

fn parse(bytes: &[u8]) -> Option<Journal> {
    serde_json::from_slice::<Journal>(bytes).ok().filter(|j| j.version == JOURNAL_VERSION)
}

/// `rec-<16 hex>-<digits>`: ids are never used as raw paths.
pub fn is_entry_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("rec-") else { return false };
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
        assert_eq!(entries[0].1.text, "unsaved a, newer");
        let id = entries[0].0.clone();
        assert!(is_entry_id(&id));
        assert_eq!(store.read_entry(&id).unwrap().path.as_str(), "a.typ");
        store.remove_entry(&id).unwrap();
        assert!(store.entries().is_empty());
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
