//! Consistent backups of the project graph (`.exposition/graph-backups/`).
//!
//! A backup is written with `VACUUM INTO` from the open connection, so it is a
//! complete, consistent database even while the graph is in use (never a copy
//! of the main file alone). Backups are taken before schema migrations, on
//! attach when the newest one is old, and on request; the newest
//! [`KEEP`] are kept. Restoring validates the backup like any untrusted
//! database (on a copy), keeps the replaced database aside, and never deletes
//! it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::store::{DATABASE_FILE, GraphStore, StoreError, StoreResult, now_ms};

/// Folder (inside `.exposition/`) holding graph backups.
pub const BACKUP_DIR: &str = "graph-backups";
/// Backups kept after rotation.
pub const KEEP: usize = 8;
/// Attach takes a backup when the newest one is older than this.
pub const MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// One graph backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    /// File name inside `.exposition/graph-backups/` (the restore handle).
    pub name: String,
    pub taken_at_ms: u64,
    /// Project graph revision captured.
    pub revision: u64,
    /// Why it was taken: `premigration`, `auto`, `manual`, `restore`.
    pub reason: String,
    pub size_bytes: u64,
}

/// Parse a backup file name `graph-<ms>-r<revision>-<reason>.sqlite3`.
pub fn parse_name(name: &str) -> Option<(u64, u64, String)> {
    let rest = name.strip_prefix("graph-")?.strip_suffix(".sqlite3")?;
    let mut parts = rest.splitn(3, '-');
    let ms: u64 = parts.next()?.parse().ok()?;
    let revision: u64 = parts.next()?.strip_prefix('r')?.parse().ok()?;
    let reason = parts.next()?;
    if reason.is_empty() || reason.len() > 32 || !reason.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    Some((ms, revision, reason.to_owned()))
}

fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(std::io::Error::other("the backup folder is a link or not a folder"));
    }
    Ok(())
}

/// Write a backup of `store` into `dir` and rotate old ones.
pub fn create(store: &GraphStore, dir: &Path, reason: &str) -> StoreResult<BackupInfo> {
    ensure_dir(dir)?;
    let revision = store.revision().unwrap_or(0);
    let mut taken = now_ms();
    let mut name;
    loop {
        name = format!("graph-{taken}-r{revision}-{reason}.sqlite3");
        if !dir.join(&name).exists() {
            break;
        }
        taken += 1;
    }
    let partial = dir.join(format!("{name}.partial"));
    let _ = std::fs::remove_file(&partial);
    store.vacuum_into(&partial)?;
    std::fs::rename(&partial, dir.join(&name))?;
    rotate(dir, KEEP);
    let size_bytes = std::fs::metadata(dir.join(&name)).map(|m| m.len()).unwrap_or(0);
    Ok(BackupInfo { name, taken_at_ms: taken, revision, reason: reason.to_owned(), size_bytes })
}

/// Backups in `dir`, newest first (only well-formed regular files).
pub fn list(dir: &Path) -> Vec<BackupInfo> {
    let mut out: Vec<BackupInfo> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let (taken_at_ms, revision, reason) = parse_name(&name)?;
            let meta = std::fs::symlink_metadata(entry.path()).ok()?;
            if !meta.is_file() {
                return None;
            }
            Some(BackupInfo { name, taken_at_ms, revision, reason, size_bytes: meta.len() })
        })
        .collect();
    out.sort_by(|a, b| b.taken_at_ms.cmp(&a.taken_at_ms).then_with(|| b.name.cmp(&a.name)));
    out
}

/// Delete all but the newest `keep` backups (and stale partial files).
pub fn rotate(dir: &Path, keep: usize) {
    for old in list(dir).into_iter().skip(keep) {
        let _ = std::fs::remove_file(dir.join(old.name));
    }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".partial") || name.starts_with("restore-") {
            let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if old.is_some_and(|age| age.as_secs() > 3600) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Replace the project graph at `project_dir/graph.sqlite3` with the backup
/// `name`. The backup is validated on a copy first (schema, contents,
/// migration); the replaced database (if any) is renamed aside, never
/// deleted. Returns the name the replaced database was kept under.
pub fn restore(project_dir: &Path, name: &str) -> StoreResult<Option<PathBuf>> {
    if parse_name(name).is_none() {
        return Err(StoreError::Untrusted("not a graph backup name".into()));
    }
    let dir = project_dir.join(BACKUP_DIR);
    let source = dir.join(name);
    match std::fs::symlink_metadata(&source) {
        Ok(meta) if meta.is_file() => {}
        _ => return Err(StoreError::Untrusted(format!("the backup {name} does not exist"))),
    }
    let copy = dir.join(format!("restore-{}.sqlite3", now_ms()));
    std::fs::copy(&source, &copy)?;
    // Validate (and migrate) the copy exactly like an untrusted project graph.
    match GraphStore::open(&copy, false) {
        Ok(store) => drop(store),
        Err(e) => {
            let _ = std::fs::remove_file(&copy);
            return Err(e);
        }
    }
    let db = project_dir.join(DATABASE_FILE);
    let mut aside = None;
    if db.exists() {
        let kept = project_dir.join(format!("graph.damaged-{}.sqlite3", now_ms()));
        std::fs::rename(&db, &kept)?;
        for suffix in ["-journal", "-wal", "-shm"] {
            let side = project_dir.join(format!("{DATABASE_FILE}{suffix}"));
            if side.exists() {
                let _ = std::fs::rename(&side, kept.with_extension(format!("sqlite3{suffix}")));
            }
        }
        aside = Some(kept);
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        let _ = std::fs::remove_file(copy.with_extension(format!("sqlite3{suffix}")));
    }
    std::fs::rename(&copy, &db)?;
    Ok(aside)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_hostile_names_are_refused() {
        assert_eq!(parse_name("graph-1700-r42-auto.sqlite3"), Some((1700, 42, "auto".into())));
        for bad in [
            "graph.sqlite3",
            "graph-x-r1-auto.sqlite3",
            "graph-1-r1-../x.sqlite3",
            "graph-1-r1-AUTO.sqlite3",
            "graph-1-r1-.sqlite3",
            "../graph-1-r1-auto.sqlite3",
        ] {
            assert!(parse_name(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn backups_are_consistent_rotated_and_restorable() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join(".exposition");
        std::fs::create_dir_all(&project_dir).unwrap();
        let db = project_dir.join(DATABASE_FILE);
        let store = GraphStore::open(&db, true).unwrap();
        let project = store.project().clone();
        let backups = project_dir.join(BACKUP_DIR);
        let first = create(&store, &backups, "manual").unwrap();
        assert!(first.size_bytes > 0);
        for _ in 0..(KEEP + 3) {
            create(&store, &backups, "auto").unwrap();
        }
        let all = list(&backups);
        assert_eq!(all.len(), KEEP, "rotation keeps the newest");
        assert!(!all.iter().any(|b| b.name == first.name), "the oldest was rotated out");
        drop(store);
        // Damage the live database, then restore the newest backup.
        std::fs::write(&db, b"garbage").unwrap();
        let aside = restore(&project_dir, &all[0].name).unwrap().unwrap();
        assert_eq!(std::fs::read(&aside).unwrap(), b"garbage", "the replaced file is kept");
        let reopened = GraphStore::open(&db, false).unwrap();
        assert_eq!(reopened.project(), &project);
        // Hostile names never reach the file system.
        assert!(restore(&project_dir, "../../etc").is_err());
        assert!(restore(&project_dir, "graph-1-r1-missing.sqlite3").is_err());
    }
}
