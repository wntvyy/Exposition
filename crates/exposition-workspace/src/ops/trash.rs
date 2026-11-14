//! The OS Trash / Recycle Bin.
//!
//! Ordinary deletion moves an entry to the OS trash and NEVER falls back to
//! permanent removal: a failure is reported (`ops.trashUnavailable`) and the
//! entry stays where it was. Undo is offered only when the trashed item can
//! be identified reliably afterwards: the trash is listed before and after
//! the move, and exactly one NEW item with the entry's original location
//! must have appeared (Windows and freedesktop; elsewhere no Undo).
//!
//! Every call runs on a fresh thread: the `trash` crate initializes COM per
//! thread in apartment mode and panics if a thread was initialized
//! differently (e.g. a blocking-pool thread used by another library).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A trashed entry the app can find again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashedItem {
    /// The OS's identifier of the item inside the trash.
    pub id: String,
    /// Name and folder it had before deletion (as the OS reports them).
    pub name: String,
    pub original_parent: PathBuf,
    /// Seconds since the Unix epoch, as the OS recorded the deletion.
    pub deleted_at: i64,
}

/// The trash as project operations use it (a real one, or a test double).
pub trait TrashBin: Send + Sync {
    /// Human name for messages ("the Recycle Bin").
    fn name(&self) -> &'static str;
    /// Move these absolute paths (each with a canonical parent) to the trash,
    /// one by one. Returns, per path, whether it was moved and, when it can
    /// be identified reliably, the trashed item.
    fn delete(&self, paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, String>>;
    /// Is the item still in the trash (same identity and deletion time)?
    fn contains(&self, item: &TrashedItem) -> Result<bool, String>;
    /// Move a trashed item to `parent/name` (the caller guarantees that name
    /// is a fresh, unused staging name).
    fn restore_to(&self, item: &TrashedItem, parent: &Path, name: &str) -> Result<(), String>;
    /// Remove items from the trash for good (test cleanup).
    fn purge(&self, items: &[TrashedItem]) -> Result<(), String>;
}

/// The operating system's trash.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsTrash;

fn on_fresh_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    std::thread::Builder::new()
        .name("exposition-trash".into())
        .spawn(move || std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)))
        .map_err(|e| format!("could not start the trash operation: {e}"))?
        .join()
        .map_err(|_| "the trash operation stopped unexpectedly".to_owned())?
        .map_err(|_| "the trash operation stopped unexpectedly".to_owned())
}

/// Path text as the OS trash reports original locations (no `\\?\` prefix).
fn display(path: &Path) -> String {
    crate::root::display_path(path)
}

fn same_location(a: &Path, b: &Path) -> bool {
    let (a, b) = (display(a), display(b));
    if cfg!(windows) { a.eq_ignore_ascii_case(&b) } else { a == b }
}

#[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
mod listing {
    use super::*;

    pub fn list() -> Result<Vec<trash::TrashItem>, String> {
        trash::os_limited::list().map_err(|e| describe(&e))
    }

    pub fn to_record(item: &trash::TrashItem) -> TrashedItem {
        TrashedItem {
            id: item.id.to_string_lossy().into_owned(),
            name: item.name.to_string_lossy().into_owned(),
            original_parent: item.original_parent.clone(),
            deleted_at: item.time_deleted,
        }
    }

    pub fn find(items: &[trash::TrashItem], record: &TrashedItem) -> Option<trash::TrashItem> {
        items.iter().find(|i| i.id.to_string_lossy() == record.id && i.time_deleted == record.deleted_at).cloned()
    }
}

fn describe(error: &trash::Error) -> String {
    match error {
        trash::Error::Os { description, .. } => description.clone(),
        trash::Error::Unknown { description } => description.clone(),
        trash::Error::TargetedRoot => "a root folder cannot be moved to the trash".into(),
        trash::Error::CouldNotAccess { target } => format!("{target} could not be accessed"),
        trash::Error::CanonicalizePath { original } => format!("{} could not be resolved", display(original)),
        trash::Error::RestoreCollision { path, .. } => format!("{} already exists", display(path)),
        trash::Error::RestoreTwins { path, .. } => format!("several trashed items came from {}", display(path)),
        other => format!("{other:?}"),
    }
}

impl TrashBin for OsTrash {
    fn name(&self) -> &'static str {
        if cfg!(windows) { "the Recycle Bin" } else { "the Trash" }
    }

    fn delete(&self, paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, String>> {
        let paths = paths.to_vec();
        let count = paths.len();
        match on_fresh_thread(move || delete_and_match(&paths)) {
            Ok(results) => results,
            Err(e) => (0..count).map(|_| Err(e.clone())).collect(),
        }
    }

    #[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    fn contains(&self, item: &TrashedItem) -> Result<bool, String> {
        let record = item.clone();
        on_fresh_thread(move || listing::list().map(|items| listing::find(&items, &record).is_some()))?
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
    fn contains(&self, _item: &TrashedItem) -> Result<bool, String> {
        Err("restoring from the trash is not supported on this platform".into())
    }

    #[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    fn restore_to(&self, item: &TrashedItem, parent: &Path, name: &str) -> Result<(), String> {
        let record = item.clone();
        let parent = PathBuf::from(display(parent));
        let name = name.to_owned();
        on_fresh_thread(move || {
            let items = listing::list()?;
            let Some(mut found) = listing::find(&items, &record) else {
                return Err("it is no longer in the trash".to_owned());
            };
            // Restore under the fresh staging name chosen by the caller.
            found.original_parent = parent;
            found.name = name.into();
            trash::os_limited::restore_all([found]).map_err(|e| describe(&e))
        })?
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
    fn restore_to(&self, _item: &TrashedItem, _parent: &Path, _name: &str) -> Result<(), String> {
        Err("restoring from the trash is not supported on this platform".into())
    }

    #[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    fn purge(&self, items: &[TrashedItem]) -> Result<(), String> {
        let records = items.to_vec();
        on_fresh_thread(move || {
            let listed = listing::list()?;
            let found: Vec<trash::TrashItem> = records.iter().filter_map(|r| listing::find(&listed, r)).collect();
            trash::os_limited::purge_all(found).map_err(|e| describe(&e))
        })?
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
    fn purge(&self, _items: &[TrashedItem]) -> Result<(), String> {
        Err("not supported on this platform".into())
    }
}

/// Move each path to the trash and identify the item each one became.
fn delete_and_match(paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, String>> {
    #[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    let before: Option<std::collections::HashSet<String>> =
        listing::list().ok().map(|items| items.iter().map(|i| i.id.to_string_lossy().into_owned()).collect());
    let started = now_secs();
    let moved: Vec<Result<(), String>> = paths.iter().map(|p| trash::delete(p).map_err(|e| describe(&e))).collect();
    let finished = now_secs();
    #[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    {
        let after = if moved.iter().any(Result::is_ok) { listing::list().ok() } else { None };
        paths
            .iter()
            .zip(moved)
            .map(|(path, result)| {
                result.map(|()| {
                    let (Some(before), Some(after)) = (&before, &after) else { return None };
                    let fresh: Vec<&trash::TrashItem> = after
                        .iter()
                        .filter(|i| !before.contains(i.id.to_string_lossy().as_ref()))
                        .filter(|i| same_location(&i.original_path(), path))
                        .filter(|i| i.time_deleted >= started - 2 && i.time_deleted <= finished + 2)
                        .collect();
                    match fresh.as_slice() {
                        [only] => Some(listing::to_record(only)),
                        _ => None,
                    }
                })
            })
            .collect()
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
    {
        let _ = (started, finished);
        moved.into_iter().map(|r| r.map(|()| None)).collect()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}
