//! Workspace IPC types (exported to TypeScript by ts-rs).

use exposition_core::document::{DocumentId, DocumentInfo, DocumentState};
use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The open project folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    /// Absolute folder path for display.
    pub root: String,
    /// Folder name.
    pub name: String,
    /// Unsaved changes from an earlier session awaiting restore or discard.
    pub recovery: Vec<RecoveryEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    Directory,
    Other,
}

/// One directory entry in the file tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: WorkspacePath,
    pub kind: EntryKind,
    /// Size in bytes for files.
    pub size: Option<u64>,
    /// The entry is a symbolic link or junction.
    pub link: bool,
    /// Why the entry cannot be opened (e.g. a link that leaves the folder).
    pub unavailable: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum FileChangeKind {
    Created,
    Removed,
    Modified,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: WorkspacePath,
    pub kind: FileChangeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKind {
    /// The file changed on disk while the buffer has unsaved changes.
    Modified,
    /// The file was deleted or moved away on disk.
    Deleted,
    /// The file on disk is no longer readable as text (binary, undecodable, too large).
    Unreadable,
}

/// A difference between an open buffer and its file that needs a user decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiskConflict {
    pub document: DocumentId,
    pub path: WorkspacePath,
    pub kind: ConflictKind,
    pub message: String,
}

/// Result of opening a project file as a document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DocumentOpened {
    pub state: DocumentState,
    pub conflict: Option<DiskConflict>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryDiskState {
    /// The file on disk is unchanged since the journaled edits were made.
    Unchanged,
    /// The file changed on disk after the journaled edits were made.
    Changed,
    /// The file no longer exists.
    Missing,
    /// The recovery data itself could not be read (see `RecoveryEntry::problem`).
    Unknown,
}

/// Unsaved buffer content journaled by an earlier session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryEntry {
    /// Opaque identifier used to restore or discard the entry.
    pub id: String,
    /// The file the unsaved changes belong to (`None` when unreadable data no longer names it).
    pub path: Option<WorkspacePath>,
    /// When the journal was written (Unix milliseconds).
    pub saved_at: u64,
    /// UTF-16 length of the journaled text.
    pub length: u32,
    pub disk: RecoveryDiskState,
    /// Why the recovery data cannot be restored (corrupt, from a newer version).
    /// Such entries are quarantined; they can only be inspected or discarded.
    pub problem: Option<String>,
    /// Absolute location of the quarantined data, for inspection.
    pub location: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcome {
    pub state: DocumentState,
    /// The file changed (or disappeared) on disk after the journaled edits; the
    /// buffer holds the recovered text and the disk keeps its version.
    pub disk_changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SaveFailure {
    pub document: DocumentId,
    pub path: Option<WorkspacePath>,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SaveAllOutcome {
    pub saved: Vec<DocumentInfo>,
    pub failed: Vec<SaveFailure>,
}

/// A recently opened project folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RecentProject {
    pub path: String,
    pub name: String,
    pub opened_at: u64,
}

/// Workspace events, forwarded to the webview as `workspace:event`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WorkspaceEvent {
    Opened {
        info: WorkspaceInfo,
    },
    Closed,
    /// Files changed on disk. `rescan` means the watcher lost track and every
    /// loaded directory should be re-listed.
    FilesChanged {
        changes: Vec<FileChange>,
        rescan: bool,
    },
    /// A clean buffer was reloaded from disk (an `external` transaction).
    DocumentReloaded {
        document: DocumentId,
        path: WorkspacePath,
    },
    Conflict {
        conflict: DiskConflict,
    },
    ConflictResolved {
        document: DocumentId,
    },
    RecoveryChanged {
        entries: Vec<RecoveryEntry>,
    },
    /// Unsaved changes could not be journaled for crash recovery.
    JournalFailed {
        path: WorkspacePath,
        message: String,
    },
    /// `.exposition/project.json` was invalid when read (by any service): it
    /// was set aside and defaults are in use. `message` names the problem and
    /// the kept original.
    ProjectSettingsInvalid {
        message: String,
    },
}

/// A copy of a document's current text saved to a new file
/// (`document_save_copy`); the document itself is unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SavedCopy {
    /// Where the copy was written (absolute, for display).
    pub location: String,
    /// The copy's project path when it is inside the open project.
    pub path: Option<WorkspacePath>,
    /// Bytes written.
    pub bytes: u64,
}
