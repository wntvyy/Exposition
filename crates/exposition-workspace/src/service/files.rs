//! New File and Rename from the explorer (QOL E).
//!
//! - [`WorkspaceService::create_file`] creates an EMPTY file in an existing
//!   project folder under a validated name, never replacing anything
//!   (create-new semantics, so a case variant of an existing name is taken
//!   too on Windows). The editor opens it afterwards through the normal path.
//! - [`WorkspaceService::plan_rename`] / [`WorkspaceService::apply_rename`]
//!   rename a file or folder within its folder. The move never replaces an
//!   entry (`MoveFileExW` without replace); a case-only change of the same
//!   entry is allowed. Under the I/O lock (so the watcher's disk checks see
//!   either the old state or the new one, never a document whose file is
//!   "gone"), open documents at or under the old path move to the new path:
//!   the same document (id, text, revisions, undo history, saved state), the
//!   language of the new name, the recovery journal under the new path.
//!   Nothing else is rewritten: references elsewhere are the caller's to
//!   report. The caller makes the move and the graph record one serialized
//!   step (`GraphService::reconcile_after`), so the resource keeps its
//!   identity.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use exposition_core::document::{DocumentId, DocumentInfo};
use exposition_core::path::WorkspacePath;

use super::{WorkspaceService, lock};
use crate::encoding::language_for;
use crate::error::{WorkspaceError, WorkspaceResult};
use crate::explorer::validate_name;
use crate::fsutil::move_no_replace;
use crate::ops::{fsops, locate, names, policy};

/// A validated rename (see [`WorkspaceService::plan_rename`]).
#[derive(Clone, Debug)]
pub struct RenamePlan {
    pub from: WorkspacePath,
    pub to: WorkspacePath,
    pub is_dir: bool,
    /// Only the letter case changes (the same entry on a case-insensitive file system).
    pub case_only: bool,
    root: PathBuf,
    from_abs: PathBuf,
    to_abs: PathBuf,
}

/// A rename that happened on disk.
#[derive(Clone, Debug)]
pub struct Renamed {
    pub from: WorkspacePath,
    pub to: WorkspacePath,
    pub is_dir: bool,
    pub documents: Vec<DocumentInfo>,
}

fn located_error(code: &'static str, message: String) -> WorkspaceError {
    let code = match code {
        "ops.notFound" => "path.notFound",
        "ops.outsideProject" => "path.outsideWorkspace",
        "ops.notSupported" => "name.invalid",
        _ => "file.io",
    };
    WorkspaceError::new(code, message)
}

/// The folder as the start of a sentence: `figures/` or "The project folder".
fn folder_label(folder: Option<&WorkspacePath>) -> String {
    folder.map(|f| format!("{f}/")).unwrap_or_else(|| "The project folder".into())
}

fn same_name_ignoring_case(a: &str, b: &str) -> bool {
    cfg!(windows) && a != b && a.to_lowercase() == b.to_lowercase()
}

impl WorkspaceService {
    /// Create an empty file `name` in `folder` (`None`: the project folder).
    pub fn create_file(&self, folder: Option<&WorkspacePath>, name: &str) -> WorkspaceResult<WorkspacePath> {
        validate_name(name)?;
        let root = self.root()?;
        let (parent, parent_abs) = match folder {
            None => (None, root.path().to_path_buf()),
            Some(folder) => {
                let abs = root.resolve_existing(folder)?;
                if !abs.is_dir() {
                    return Err(WorkspaceError::new("path.notDirectory", format!("{folder} is not a folder.")));
                }
                (root.relativize(&abs), abs)
            }
        };
        if let Some(parent) = &parent
            && let Some(message) = policy::refuse_destination(parent)
        {
            return Err(WorkspaceError::new("file.protected", message));
        }
        let path = names::join(parent.as_ref(), name)?;
        let exists = || {
            WorkspaceError::new(
                "file.exists",
                format!("{} already contains “{name}”. Choose another name.", folder_label(parent.as_ref())),
            )
        };
        let _io = lock(&self.io);
        // A document still open under this path (its file was deleted while
        // the text was kept) would otherwise be saved over the new file.
        if !self.documents_under(&path).is_empty() {
            return Err(exists());
        }
        let target = parent_abs.join(name);
        match fs::OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(_) => Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(exists()),
            // Windows reports a folder of that name as access denied.
            Err(_) if fs::symlink_metadata(&target).is_ok() => Err(exists()),
            Err(e) => Err(WorkspaceError::io(&path, &e)),
        }
    }

    /// Validate renaming the file or folder `from` to `name` (in the same
    /// folder). Nothing changes.
    pub fn plan_rename(&self, from: &WorkspacePath, name: &str) -> WorkspaceResult<RenamePlan> {
        validate_name(name)?;
        let root = self.root()?;
        let located = locate(&root, from).map_err(|(code, message)| located_error(code, message))?;
        if let Some(message) = policy::refuse_change(&located.path) {
            return Err(WorkspaceError::new("file.protected", message));
        }
        if let Some(parent) = &located.parent
            && let Some(message) = policy::refuse_destination(parent)
        {
            return Err(WorkspaceError::new("file.protected", message));
        }
        let is_dir = match located.info.kind {
            fsops::EntryKind::Dir => true,
            fsops::EntryKind::File | fsops::EntryKind::Link => false,
            fsops::EntryKind::Other => {
                return Err(WorkspaceError::new("file.unsupported", format!("{from} is not a file or folder.")));
            }
        };
        let old_name = located.path.file_name().to_owned();
        if old_name == name {
            return Err(WorkspaceError::new("name.unchanged", "The name is unchanged."));
        }
        let to = names::join(located.parent.as_ref(), name)?;
        let case_only = same_name_ignoring_case(&old_name, name);
        let to_abs = located.parent_abs.join(name);
        if !case_only {
            if fs::symlink_metadata(&to_abs).is_ok() {
                return Err(WorkspaceError::new(
                    "file.exists",
                    format!(
                        "{} already contains “{name}”. Choose another name.",
                        folder_label(located.parent.as_ref())
                    ),
                ));
            }
            if !self.documents_under(&to).is_empty() {
                return Err(WorkspaceError::new(
                    "file.exists",
                    format!("“{name}” is open in the editor (its file was deleted). Close it or choose another name."),
                ));
            }
        }
        Ok(RenamePlan {
            from: located.path,
            to,
            is_dir,
            case_only,
            root: root.path().to_path_buf(),
            from_abs: located.abs,
            to_abs,
        })
    }

    /// Rename on disk and move open documents to the new path. Nothing is
    /// replaced; on failure the original entry and its documents are unchanged.
    pub fn apply_rename(&self, plan: &RenamePlan) -> WorkspaceResult<Renamed> {
        let _io = lock(&self.io);
        let root = self.root()?;
        if root.path() != plan.root {
            return Err(WorkspaceError::new("workspace.changed", "Another project was opened; nothing was renamed."));
        }
        let changed =
            || WorkspaceError::new("file.changed", format!("{} changed meanwhile; nothing was renamed.", plan.from));
        match fsops::inspect(&plan.from_abs) {
            Ok(info) if (info.kind == fsops::EntryKind::Dir) == plan.is_dir => {}
            Ok(_) => return Err(changed()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(WorkspaceError::not_found(&plan.from));
            }
            Err(e) => return Err(WorkspaceError::io(&plan.from, &e)),
        }
        let exists = || {
            WorkspaceError::new("file.exists", format!("“{}” exists now; nothing was renamed.", plan.to.file_name()))
        };
        let moved = if plan.case_only {
            // The same entry under another spelling: a direct move, else via
            // a hidden staging name (moved back if the second step fails).
            move_no_replace(&plan.from_abs, &plan.to_abs).or_else(|_| {
                let parent = plan.from_abs.parent().map(PathBuf::from).unwrap_or_default();
                let staged = fsops::staging_path(&parent, "rename");
                move_no_replace(&plan.from_abs, &staged)?;
                move_no_replace(&staged, &plan.to_abs).inspect_err(|_| {
                    let _ = move_no_replace(&staged, &plan.from_abs);
                })
            })
        } else {
            move_no_replace(&plan.from_abs, &plan.to_abs)
        };
        match moved {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(exists()),
            Err(e) if cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)) => {
                return Err(WorkspaceError::new(
                    "file.inUse",
                    format!(
                        "{} is open in another program; close it there and try again. Nothing was renamed.",
                        plan.from
                    ),
                ));
            }
            Err(e) => return Err(WorkspaceError::io(&plan.from, &e)),
        }
        // Documents follow the file (case-insensitive prefix on Windows).
        let mut documents = Vec::new();
        let mut moved_ids: HashSet<DocumentId> = HashSet::new();
        let mut old_paths: Vec<WorkspacePath> = Vec::new();
        for doc in self.documents_under(&plan.from) {
            let Some(old) = doc.path.clone() else { continue };
            let Some(suffix) = old.as_str().get(plan.from.as_str().len()..) else { continue };
            let Ok(new) = WorkspacePath::parse(&format!("{}{suffix}", plan.to.as_str())) else { continue };
            let language = language_for(new.file_name()).to_owned();
            if let Ok(info) = self.documents.relocate(&doc.id, new.clone(), language) {
                if let Some(open) = lock(&self.state).open.as_mut()
                    && let Some(conflict) = open.conflicts.get_mut(&doc.id)
                {
                    conflict.public.path = new;
                }
                moved_ids.insert(doc.id.clone());
                old_paths.push(old);
                documents.push(info);
            }
        }
        // Recovery journals: the new path first, then the old one goes, so a
        // crash in between keeps the unsaved text.
        if !moved_ids.is_empty() {
            self.journal_locked(&moved_ids);
            if let Ok(store) = self.recovery_store() {
                for old in &old_paths {
                    let _ = store.remove_live(old);
                }
            }
        }
        Ok(Renamed { from: plan.from.clone(), to: plan.to.clone(), is_dir: plan.is_dir, documents })
    }

    /// [`Self::plan_rename`] then [`Self::apply_rename`].
    pub fn rename_entry(&self, from: &WorkspacePath, name: &str) -> WorkspaceResult<Renamed> {
        let plan = self.plan_rename(from, name)?;
        self.apply_rename(&plan)
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
