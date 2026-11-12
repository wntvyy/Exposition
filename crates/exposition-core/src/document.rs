//! Document transaction contract.
//!
//! Every change to document text — from the source editor, the page editor, an
//! agent, a research artifact update, an external reload, or recovery — is a
//! [`TransactionRequest`] accepted (or rejected) by the one document service.
//! Patches are expressed in UTF-16 ranges relative to the edit's base revision.
//! Stale edits are rejected or explicitly rebased over disjoint concurrent
//! changes; they are never blindly applied.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::path::WorkspacePath;
use crate::text::TextRange;

/// Opaque identifier of an open document.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DocumentId(pub String);

/// Client-generated transaction identifier, echoed in outcomes and events so
/// views can reconcile optimistic updates.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TransactionId(pub String);

/// Who produced an edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EditOrigin {
    /// The source editor.
    Source,
    /// The exact-page editor.
    Page,
    /// An agent task.
    Agent { task_id: String },
    /// A research run inserting or updating an artifact.
    Research { run_id: String },
    /// A reload after the file changed outside Exposition.
    External,
    /// Restoration of unsaved content after a crash.
    Recovery,
    /// Internal application maintenance (e.g. format-owned rewrites).
    System,
}

/// Replace `range` (relative to the base revision) with `text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TextPatch {
    pub range: TextRange,
    pub text: String,
}

impl TextPatch {
    pub fn insert(at: u32, text: impl Into<String>) -> Self {
        Self { range: TextRange::empty(at), text: text.into() }
    }

    pub fn replace(start: u32, end: u32, text: impl Into<String>) -> Self {
        Self { range: TextRange::new(start, end), text: text.into() }
    }

    pub fn delete(start: u32, end: u32) -> Self {
        Self { range: TextRange::new(start, end), text: String::new() }
    }
}

/// Patches against one document. Patches must be sorted by position and
/// non-overlapping; all ranges are relative to `base_revision`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DocumentEdit {
    pub document: DocumentId,
    pub base_revision: u64,
    pub patches: Vec<TextPatch>,
}

/// How a transaction joins the undo history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UndoBehavior {
    /// Start a new undo group.
    NewGroup,
    /// Merge into the most recent group if it has the same key and origin kind
    /// and nothing else intervened (e.g. continuous typing); otherwise start a
    /// new group with this key.
    Merge { key: String },
    /// Do not record (only for changes that must not be undone independently).
    Skip,
}

/// What to do when an edit's base revision is older than the current revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ConflictPolicy {
    /// Reject any stale edit.
    Reject,
    /// Rebase over intervening changes when they are disjoint from every patch;
    /// reject on overlap.
    RebaseDisjoint,
}

/// Editor selection to restore around undo/redo, in UTF-16 ranges.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SelectionHint {
    pub before: Vec<TextRange>,
    pub after: Vec<TextRange>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRequest {
    pub id: TransactionId,
    pub origin: EditOrigin,
    /// One or more documents. A multi-document transaction is applied to all
    /// documents or to none (in memory; this is not filesystem atomicity).
    pub edits: Vec<DocumentEdit>,
    pub undo: UndoBehavior,
    pub conflict: ConflictPolicy,
    pub label: Option<String>,
    pub selection: Option<SelectionHint>,
}

/// An edit as actually applied: patches are relative to `previous_revision`
/// (after any rebase), so every view can apply them in revision order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AppliedEdit {
    pub document: DocumentId,
    pub previous_revision: u64,
    pub revision: u64,
    pub patches: Vec<TextPatch>,
    pub rebased: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TransactionOutcome {
    pub id: TransactionId,
    pub applied: Vec<AppliedEdit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TransactionError {
    #[error("transaction contains no edits")]
    Empty,
    #[error("document {document:?} is not open")]
    UnknownDocument { document: DocumentId },
    #[error("document {document:?} appears more than once in the transaction")]
    DuplicateDocument { document: DocumentId },
    #[error("document {document:?} is read-only: {reason}")]
    ReadOnly { document: DocumentId, reason: String },
    /// The text mixes line endings (LF, CRLF, lone CR). It cannot be edited
    /// from any origin until the user normalizes it (only a change whose
    /// result is consistent, a reload from disk, or a recovery is accepted).
    #[error("document {document:?} has mixed line endings and is read-only until the user normalizes them")]
    MixedLineEndings { document: DocumentId },
    #[error("edit to {document:?} is based on revision {base_revision} but the document is at {current_revision}")]
    StaleRevision { document: DocumentId, base_revision: u64, current_revision: u64 },
    #[error("edit to {document:?} overlaps changes made since revision {base_revision} (now {current_revision})")]
    Conflict { document: DocumentId, base_revision: u64, current_revision: u64 },
    #[error("revision {base_revision} of {document:?} is too old to rebase")]
    HistoryUnavailable { document: DocumentId, base_revision: u64 },
    #[error("revision {base_revision} of {document:?} does not exist yet")]
    FutureRevision { document: DocumentId, base_revision: u64 },
    #[error("patch range {range:?} is invalid for {document:?} (UTF-16 length {length})")]
    InvalidRange { document: DocumentId, range: TextRange, length: u32 },
    #[error("patch range {range:?} splits a code point in {document:?}")]
    NotCharBoundary { document: DocumentId, range: TextRange },
    #[error("patches for {document:?} are unsorted or overlap")]
    OverlappingPatches { document: DocumentId },
    #[error("nothing to {action} in {document:?}")]
    NothingToUndo { document: DocumentId, action: String },
}

/// Line-ending convention of a document's current text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum LineEnding {
    Lf,
    Crlf,
    /// More than one convention occurs; text is kept exactly as loaded.
    Mixed,
    /// No line terminator in the text.
    None,
}

/// Character encoding a document was decoded from and will be saved in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum TextEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

/// Metadata the document service keeps alongside the text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    pub id: DocumentId,
    /// Workspace-relative path, if the document is backed by a project file.
    pub path: Option<WorkspacePath>,
    pub revision: u64,
    /// Revision last written to (or loaded from) disk.
    pub saved_revision: Option<u64>,
    pub language: String,
    pub line_ending: LineEnding,
    pub encoding: TextEncoding,
    pub read_only: Option<String>,
    pub can_undo: bool,
    pub can_redo: bool,
}

/// Full document state for a view that is attaching to a document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DocumentState {
    pub info: DocumentInfo,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Edit,
    Undo,
    Redo,
}

/// Emitted for every accepted revision, in revision order per document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DocumentChanged {
    pub document: DocumentId,
    pub transaction: Option<TransactionId>,
    pub origin: EditOrigin,
    /// Edit, undo, or redo. (Named `change`, not `kind`: `DocumentEvent` is
    /// internally tagged by `kind`, and a nested `kind` field would collide.)
    pub change: ChangeKind,
    pub previous_revision: u64,
    pub revision: u64,
    pub patches: Vec<TextPatch>,
    /// Selection a view should adopt (undo/redo restore the recorded selection).
    pub selection: Option<Vec<TextRange>>,
    pub can_undo: bool,
    pub can_redo: bool,
}

/// Lifecycle events for the set of open documents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DocumentEvent {
    Changed(DocumentChanged),
    Opened {
        info: DocumentInfo,
    },
    Closed {
        document: DocumentId,
    },
    /// Saved state or other metadata changed without a text change.
    InfoChanged {
        info: DocumentInfo,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DocumentEvent` is internally tagged by `kind`; a variant's payload must
    /// not have its own `kind` field or the JSON object gets two `kind` keys
    /// (and JavaScript keeps the last one).
    #[test]
    fn changed_events_serialize_with_one_kind_tag() {
        let event = DocumentEvent::Changed(DocumentChanged {
            document: DocumentId("doc-1".into()),
            transaction: Some(TransactionId("t-1".into())),
            origin: EditOrigin::Source,
            change: ChangeKind::Undo,
            previous_revision: 1,
            revision: 2,
            patches: vec![TextPatch::insert(0, "x")],
            selection: None,
            can_undo: false,
            can_redo: true,
        });
        let json = serde_json::to_string(&event).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["kind"], "changed", "{json}");
        assert_eq!(value["change"], "undo", "{json}");
        assert_eq!(json.matches("\"kind\":\"changed\"").count(), 1);
        let back: DocumentEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }
}
