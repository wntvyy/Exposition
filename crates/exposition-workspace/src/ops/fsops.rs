//! Filesystem primitives of project operations: inspecting entries without
//! following links, bounded tree walks, copying into hidden staging names,
//! and publishing a staged copy under a free name without ever replacing an
//! existing entry.
//!
//! Publication never uses a plain rename onto the final name: on Windows,
//! `std::fs::rename` of a folder replaces an existing FILE or empty folder of
//! that name. Instead a name is taken atomically first:
//! - a file is published with a hard link (fails if the name exists), or,
//!   where hard links are unsupported, by reserving the name with create-new
//!   and renaming over that reservation;
//! - a folder is published by reserving the name with `create_dir` (fails if
//!   it exists) and renaming the staged folder over that empty reservation.
//!
//! Staging names contain [`TEMP_MARKER`], so the file tree, the watcher and
//! builds ignore them while a copy is in progress.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::fsutil::{TEMP_MARKER, now_millis};

/// Entries visited by one bounded walk (fingerprints, read-only and link checks).
pub const MAX_WALK_ENTRIES: usize = 200_000;

/// What an entry is, without following a final link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// A symbolic link or junction (never followed by these operations).
    Link,
    Other,
}

#[derive(Clone, Debug)]
pub struct EntryInfo {
    pub kind: EntryKind,
    pub len: u64,
    pub modified_ms: Option<u64>,
    pub readonly: bool,
}

pub fn inspect(path: &Path) -> io::Result<EntryInfo> {
    let meta = fs::symlink_metadata(path)?;
    let file_type = meta.file_type();
    let kind = if file_type.is_symlink() {
        EntryKind::Link
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let modified_ms =
        meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
    Ok(EntryInfo { kind, len: meta.len(), modified_ms, readonly: meta.permissions().readonly() })
}

/// A bounded walk of a folder (links are reported, never entered).
#[derive(Clone, Debug, Default)]
pub struct Walk {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// Relative paths (with `/`) of links found inside.
    pub links: Vec<String>,
    /// Relative paths of read-only files inside.
    pub readonly: Vec<String>,
    /// Digest over every entry's relative path, kind, size and modification time.
    pub digest: String,
    /// The walk stopped at [`MAX_WALK_ENTRIES`].
    pub truncated: bool,
}

pub fn walk(dir: &Path) -> Walk {
    let mut out = Walk::default();
    let mut hasher = Sha256::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(dir.to_path_buf(), String::new())];
    let mut seen = 0usize;
    while let Some((abs, rel)) = stack.pop() {
        let Ok(read) = fs::read_dir(&abs) else {
            hasher.update(format!("unreadable:{rel}\n"));
            continue;
        };
        let mut entries: Vec<_> = read.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                out.truncated = true;
                out.digest = format!("{:x}-truncated", hasher.finalize());
                return out;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let Ok(info) = inspect(&entry.path()) else {
                hasher.update(format!("gone:{child_rel}\n"));
                continue;
            };
            hasher
                .update(format!("{child_rel}|{:?}|{}|{:?}|{}\n", info.kind, info.len, info.modified_ms, info.readonly));
            match info.kind {
                EntryKind::Link => out.links.push(child_rel),
                EntryKind::Dir => {
                    out.dirs += 1;
                    stack.push((entry.path(), child_rel));
                }
                EntryKind::File => {
                    out.files += 1;
                    out.bytes += info.len;
                    if info.readonly {
                        out.readonly.push(child_rel);
                    }
                }
                EntryKind::Other => {}
            }
        }
    }
    out.digest = format!("{:x}", hasher.finalize());
    out
}

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A hidden staging name in `parent` for operation `tag` (never an existing entry).
pub fn staging_path(parent: &Path, tag: &str) -> PathBuf {
    let unique =
        format!("{:x}{:x}{:x}", std::process::id(), now_millis(), STAGING_COUNTER.fetch_add(1, Ordering::Relaxed));
    parent.join(format!("{TEMP_MARKER}{tag}-{unique}.tmp"))
}

/// Does `name` belong to the staging area of operation `tag`?
pub fn is_staging_name(name: &str, tag: &str) -> bool {
    name.starts_with(&format!("{TEMP_MARKER}{tag}-")) && name.ends_with(".tmp")
}

/// What a tree copy left out.
#[derive(Clone, Debug, Default)]
pub struct CopyReport {
    pub files: u64,
    pub bytes: u64,
    /// Links that were not copied (relative paths with `/`).
    pub links_skipped: Vec<String>,
}

/// Copy one regular file's saved bytes to `to` (which must not exist).
pub fn copy_file(from: &Path, to: &Path) -> io::Result<u64> {
    // Refuse to follow a link that appeared since the check.
    if fs::symlink_metadata(from)?.file_type().is_symlink() {
        return Err(io::Error::other("the source became a link"));
    }
    fs::copy(from, to)
}

/// Copy the folder `from` into the new folder `to` (created here). Links
/// inside are never followed or copied; they are reported.
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<CopyReport> {
    let mut report = CopyReport::default();
    fs::create_dir(to)?;
    let mut stack: Vec<(PathBuf, PathBuf, String)> = vec![(from.to_path_buf(), to.to_path_buf(), String::new())];
    while let Some((src, dst, rel)) = stack.pop() {
        for entry in fs::read_dir(&src)? {
            let entry = entry?;
            let name = entry.file_name();
            let child_rel = if rel.is_empty() {
                name.to_string_lossy().into_owned()
            } else {
                format!("{rel}/{}", name.to_string_lossy())
            };
            let info = inspect(&entry.path())?;
            let target = dst.join(&name);
            match info.kind {
                EntryKind::Link | EntryKind::Other => report.links_skipped.push(child_rel),
                EntryKind::Dir => {
                    fs::create_dir(&target)?;
                    stack.push((entry.path(), target, child_rel));
                }
                EntryKind::File => {
                    report.bytes += copy_file(&entry.path(), &target)?;
                    report.files += 1;
                }
            }
        }
    }
    Ok(report)
}

/// Why publishing under a name did not happen.
#[derive(Debug)]
pub enum PublishError {
    /// The name is taken (try another).
    Exists,
    Io(io::Error),
}

fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    if perms.readonly() == readonly {
        return Ok(());
    }
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(readonly);
    fs::set_permissions(path, perms)
}

/// Publish the staged file `staged` as `target` without replacing anything.
/// On success `staged` no longer exists.
pub fn publish_file(staged: &Path, target: &Path) -> Result<(), PublishError> {
    let readonly = fs::metadata(staged).map(|m| m.permissions().readonly()).map_err(PublishError::Io)?;
    if readonly {
        set_readonly(staged, false).map_err(PublishError::Io)?;
    }
    let restore_flag = |path: &Path| {
        if readonly {
            let _ = set_readonly(path, true);
        }
    };
    match fs::hard_link(staged, target) {
        Ok(()) => {
            // Two names for one file now; drop the staging name.
            if let Err(e) = fs::remove_file(staged) {
                let _ = fs::remove_file(target);
                restore_flag(staged);
                return Err(PublishError::Io(e));
            }
            restore_flag(target);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            restore_flag(staged);
            Err(PublishError::Exists)
        }
        Err(_) => {
            // No hard links here (e.g. FAT): reserve the name, then replace
            // only that reservation.
            match fs::OpenOptions::new().write(true).create_new(true).open(target) {
                Ok(file) => drop(file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    restore_flag(staged);
                    return Err(PublishError::Exists);
                }
                Err(e) => {
                    restore_flag(staged);
                    return Err(PublishError::Io(e));
                }
            }
            if let Err(e) = fs::rename(staged, target) {
                let _ = fs::remove_file(target);
                restore_flag(staged);
                return Err(PublishError::Io(e));
            }
            restore_flag(target);
            Ok(())
        }
    }
}

/// Publish the staged folder `staged` as `target` without replacing or
/// merging anything. On success `staged` no longer exists.
pub fn publish_dir(staged: &Path, target: &Path) -> Result<(), PublishError> {
    match fs::create_dir(target) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(PublishError::Exists),
        Err(e) => return Err(PublishError::Io(e)),
    }
    // `target` is our own empty reservation: replace exactly it.
    match fs::rename(staged, target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
            // Something was written into the reservation meanwhile: leave it.
            Err(PublishError::Exists)
        }
        Err(first) => {
            // A file system that cannot replace an empty folder by rename:
            // drop the (still empty) reservation and move into the free name.
            if fs::remove_dir(target).is_err() {
                return Err(PublishError::Exists);
            }
            match fs::symlink_metadata(target) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(PublishError::Exists),
            }
            fs::rename(staged, target).map_err(|_| PublishError::Io(first))
        }
    }
}

/// Remove a staging leftover (file or folder) completely, including read-only
/// files inside it. Never follows links.
pub fn remove_staging(path: &Path) -> io::Result<()> {
    let info = match inspect(path) {
        Ok(info) => info,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    match info.kind {
        EntryKind::Dir => {
            for entry in fs::read_dir(path)? {
                remove_staging(&entry?.path())?;
            }
            fs::remove_dir(path)
        }
        EntryKind::Link => remove_link(path),
        _ => {
            if info.readonly {
                let _ = set_readonly(path, false);
            }
            fs::remove_file(path)
        }
    }
}

/// Remove a link itself (never its target).
pub fn remove_link(path: &Path) -> io::Result<()> {
    // Junctions and directory links are removed as folders, file links as files.
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(_) => fs::remove_file(path),
    }
}

/// Permanently remove a project entry (its read-only status was checked at
/// planning). Links inside folders are removed as links, never followed.
/// Returns the number of entries that could not be removed.
pub fn remove_permanently(path: &Path) -> io::Result<u64> {
    let info = inspect(path)?;
    match info.kind {
        EntryKind::Link => remove_link(path).map(|()| 0),
        EntryKind::Dir => {
            let mut left = 0u64;
            for entry in fs::read_dir(path)? {
                let child = entry?.path();
                match remove_permanently(&child) {
                    Ok(n) => left += n,
                    Err(_) => left += 1,
                }
            }
            if left == 0 {
                fs::remove_dir(path)?;
            }
            Ok(left)
        }
        _ => fs::remove_file(path).map(|()| 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_a_file_never_replaces_an_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let staged = staging_path(dir.path(), "t");
        fs::write(&staged, "copy").unwrap();
        let taken = dir.path().join("a.txt");
        fs::write(&taken, "original").unwrap();
        assert!(matches!(publish_file(&staged, &taken), Err(PublishError::Exists)));
        assert_eq!(fs::read_to_string(&taken).unwrap(), "original");
        // A folder of that name is never replaced either.
        let folder = dir.path().join("b.txt");
        fs::create_dir(&folder).unwrap();
        assert!(matches!(publish_file(&staged, &folder), Err(PublishError::Exists)));
        assert!(folder.is_dir());
        let free = dir.path().join("a (2).txt");
        publish_file(&staged, &free).unwrap();
        assert_eq!(fs::read_to_string(&free).unwrap(), "copy");
        assert!(!staged.exists(), "the staging name is gone");
    }

    #[test]
    fn publishing_a_folder_never_replaces_a_file_or_merges_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let staged = staging_path(dir.path(), "t");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("inner.txt"), "copy").unwrap();
        // `std::fs::rename` would replace this file (verified on Windows);
        // publication must not.
        let file = dir.path().join("data");
        fs::write(&file, "keep me").unwrap();
        assert!(matches!(publish_dir(&staged, &file), Err(PublishError::Exists)));
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep me");
        // An existing empty folder is not merged into or replaced.
        let empty = dir.path().join("data (2)");
        fs::create_dir(&empty).unwrap();
        assert!(matches!(publish_dir(&staged, &empty), Err(PublishError::Exists)));
        assert!(fs::read_dir(&empty).unwrap().next().is_none());
        let free = dir.path().join("data (3)");
        publish_dir(&staged, &free).unwrap();
        assert_eq!(fs::read_to_string(free.join("inner.txt")).unwrap(), "copy");
        assert!(!staged.exists());
    }

    #[test]
    fn read_only_copies_publish_and_stay_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("ro.txt");
        fs::write(&src, "ro").unwrap();
        set_readonly(&src, true).unwrap();
        let staged = staging_path(dir.path(), "t");
        copy_file(&src, &staged).unwrap();
        let target = dir.path().join("ro (2).txt");
        publish_file(&staged, &target).unwrap();
        assert!(!staged.exists());
        assert!(fs::metadata(&target).unwrap().permissions().readonly());
        set_readonly(&src, false).unwrap();
        set_readonly(&target, false).unwrap();
    }
}
