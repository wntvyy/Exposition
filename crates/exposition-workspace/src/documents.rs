//! The document service: the single in-memory authority for open documents.
//!
//! Every accepted change produces exactly one new revision and one
//! [`DocumentEvent::Changed`], emitted in revision order while the service lock
//! is held. Views (Monaco, the page editor) apply those patches; they never hold
//! a second authoritative copy.
//!
//! Undo history is per document and shared by every edit origin. Each undo
//! group stores inverse patches relative to the current text as it would be
//! once every group above it in the stack had been undone, so undoing never
//! needs to replay history. Unrecorded (`UndoBehavior::Skip`) changes are
//! transformed down the stack when they happen; a group such a change overlaps
//! is blocked, and undoing it is refused rather than guessed.
//!
//! Both histories of a document are bounded by count and by bytes. Over the
//! undo budget, whole steps are dropped: the oldest undo steps first, then
//! the redo steps furthest from the current text; the step made last always
//! stays. The rebase history keeps where each patch of a revision was and how
//! long its text is, never the text; over its budget the oldest revisions are
//! dropped, and an edit based on a dropped revision is refused
//! (`HistoryUnavailable`) exactly like one older than the revision window.
//!
//! A document whose text mixes line endings (LF, CRLF, lone CR) is read-only
//! for every origin until it is normalized: a change (edit, undo, or redo) is
//! accepted only when its result no longer mixes them, or when it restores
//! content (an `external` reload from disk, a `recovery`). Views cannot
//! mirror mixed text exactly, so nothing edits it silently.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use exposition_core::document::ConflictPolicy;
use exposition_core::document::{
    AppliedEdit, ChangeKind, DocumentChanged, DocumentEdit, DocumentEvent, DocumentId, DocumentInfo, DocumentState,
    EditOrigin, LineEnding, TextEncoding, TextPatch, TransactionError, TransactionOutcome, TransactionRequest,
    UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, utf16_len};
use tokio::sync::broadcast;

/// Revisions retained for rebasing stale edits.
const HISTORY_LIMIT: usize = 2048;
/// Undo groups retained per document.
const UNDO_LIMIT: usize = 1000;
/// Bytes the undo and redo stacks of one document may hold together (the
/// texts their patches put back, and their records: [`held`]). Four times the
/// largest file that can be opened, and 1 MiB for records and the small steps
/// in between: four full replacements of a 10 MiB document stay undoable, and
/// a thousand steps that each put back less than 41 KiB reach the count limit
/// first. The step made last is kept even when it alone is larger.
pub const UNDO_BYTE_BUDGET: usize = 4 * crate::encoding::MAX_TEXT_BYTES as usize + 1024 * 1024;
/// Bytes the rebase history of one document may hold (records only: [`held`]).
/// The full window of 2,048 revisions of typing holds 155,648 bytes (about
/// 0.15 MiB); the budget is reached only by revisions of thousands of patches (replace all:
/// a revision of 20,000 patches holds 0.23 MiB, so the 17 newest stay). The
/// newest revision is kept even when it alone is larger.
pub const HISTORY_BYTE_BUDGET: usize = 4 * 1024 * 1024;

/// Bytes charged for the records of the histories, next to the bytes of the
/// strings they own (the texts of undo and redo patches, the merge key of a
/// step, the task or run identifier of an origin). A history of many small
/// patches is bounded like one of few large ones.
#[doc(hidden)]
pub mod held {
    use super::{HistoryEntry, PatchShape, TextPatch, TextRange, UndoGroup};

    /// One undo or redo step.
    pub const UNDO_STEP: usize = size_of::<UndoGroup>();
    /// One patch of a step, without its text.
    pub const UNDO_PATCH: usize = size_of::<TextPatch>();
    /// One range of the selections a step restores.
    pub const SELECTION_RANGE: usize = size_of::<TextRange>();
    /// One revision of the rebase history.
    pub const REVISION: usize = size_of::<HistoryEntry>();
    /// One patch of a revision.
    pub const REVISION_PATCH: usize = size_of::<PatchShape>();
}

/// The bounds of the histories of each document of a service. The shipped
/// values are the default; tests lower them to reach the bounds with small
/// texts.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    pub undo_groups: usize,
    pub undo_bytes: usize,
    pub history_entries: usize,
    pub history_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            undo_groups: UNDO_LIMIT,
            undo_bytes: UNDO_BYTE_BUDGET,
            history_entries: HISTORY_LIMIT,
            history_bytes: HISTORY_BYTE_BUDGET,
        }
    }
}

/// Deterministic work counters: bytes of document text copied and scanned by
/// the service on the calling thread. For the scale benchmarks
/// (`tests/perf_workspace.rs`) and the tests that keep per-edit work bounded;
/// they do not depend on the build profile or the machine. One thread-local
/// add per pass over the text.
#[doc(hidden)]
pub mod work {
    use std::cell::Cell;

    thread_local! {
        static COPIED: Cell<u64> = const { Cell::new(0) };
        static SCANNED: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn copied(bytes: usize) {
        COPIED.with(|c| c.set(c.get() + bytes as u64));
    }

    pub(crate) fn scanned(bytes: usize) {
        SCANNED.with(|c| c.set(c.get() + bytes as u64));
    }

    /// Reset the counters of the calling thread.
    pub fn reset() {
        COPIED.with(|c| c.set(0));
        SCANNED.with(|c| c.set(0));
    }

    /// `(bytes copied, bytes scanned)` on the calling thread since the last reset.
    pub fn totals() -> (u64, u64) {
        (COPIED.with(Cell::get), SCANNED.with(Cell::get))
    }
}

/// What the histories of one document hold (see
/// [`DocumentService::retained`]). Every field but the two running totals is
/// recounted from the histories when asked.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetainedHistory {
    pub undo_groups: usize,
    /// Text bytes of the undo patches.
    pub undo_bytes: usize,
    pub redo_groups: usize,
    /// Text bytes of the redo patches.
    pub redo_bytes: usize,
    pub history_entries: usize,
    /// Bytes of the rebase history (records; it holds no text).
    pub history_bytes: usize,
    /// Bytes of the undo and redo stacks: their text bytes and their records.
    pub undo_redo_held: usize,
    /// The running total the undo budget is compared with; equals `undo_redo_held`.
    pub undo_redo_charged: usize,
    /// The running total the history budget is compared with; equals `history_bytes`.
    pub history_charged: usize,
}

/// Parameters for opening a document.
#[derive(Debug, Clone)]
pub struct OpenDocument {
    pub path: Option<WorkspacePath>,
    pub text: String,
    pub language: String,
    pub encoding: TextEncoding,
    /// `Some(reason)` opens the document read-only.
    pub read_only: Option<String>,
}

/// An immutable view of a document at one revision.
#[derive(Debug, Clone)]
pub struct DocumentSnapshot {
    pub id: DocumentId,
    pub path: Option<WorkspacePath>,
    pub revision: u64,
    pub text: Arc<str>,
    pub language: String,
}

pub struct DocumentService {
    inner: Mutex<Inner>,
    events: broadcast::Sender<DocumentEvent>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    documents: HashMap<DocumentId, DocState>,
    limits: HistoryLimits,
}

struct DocState {
    id: DocumentId,
    path: Option<WorkspacePath>,
    language: String,
    encoding: TextEncoding,
    line_ending: LineEnding,
    read_only: Option<String>,
    revision: u64,
    saved_revision: Option<u64>,
    text: Arc<str>,
    /// The patches of each recent revision, relative to the revision before it.
    history: VecDeque<HistoryEntry>,
    /// Bytes held by `history` ([`HistoryEntry::held`] of each entry).
    history_held: usize,
    /// Oldest step first; the next step to undo or redo is at the back.
    undo: VecDeque<UndoGroup>,
    redo: VecDeque<UndoGroup>,
    /// Bytes held by `undo` and by `redo` ([`UndoGroup::held`] of each group).
    undo_held: usize,
    redo_held: usize,
}

struct HistoryEntry {
    revision: u64,
    patches: Vec<PatchShape>,
    /// Who made the change (for an undo or redo: who stepped the history).
    origin: EditOrigin,
}

impl HistoryEntry {
    /// Bytes this revision holds: its record, its patches, and the
    /// identifier its origin carries.
    fn held(&self) -> usize {
        held::REVISION + self.patches.len() * held::REVISION_PATCH + origin_bytes(&self.origin)
    }
}

/// Bytes of the identifier an origin owns on the heap.
fn origin_bytes(origin: &EditOrigin) -> usize {
    match origin {
        EditOrigin::Agent { task_id } => task_id.len(),
        EditOrigin::Research { run_id } => run_id.len(),
        EditOrigin::Source | EditOrigin::Page | EditOrigin::External | EditOrigin::Recovery | EditOrigin::System => 0,
    }
}

/// A patch as rebasing needs it: the range it replaced and the UTF-16 length
/// of the text it put there.
#[derive(Debug, Clone, Copy)]
struct PatchShape {
    range: TextRange,
    inserted: u32,
}

/// What mapping a range through a patch reads of the patch.
trait Shaped {
    fn range(&self) -> TextRange;
    /// UTF-16 length of the patch's text.
    fn inserted(&self) -> u32;
}

impl Shaped for PatchShape {
    fn range(&self) -> TextRange {
        self.range
    }
    fn inserted(&self) -> u32 {
        self.inserted
    }
}

impl Shaped for TextPatch {
    fn range(&self) -> TextRange {
        self.range
    }
    fn inserted(&self) -> u32 {
        utf16_len(&self.text) as u32
    }
}

fn shapes(patches: &[TextPatch]) -> Vec<PatchShape> {
    patches.iter().map(|p| PatchShape { range: p.range, inserted: p.inserted() }).collect()
}

struct UndoGroup {
    key: Option<String>,
    origin: EditOrigin,
    /// Restores the text at the start of the group. Relative to the current
    /// text once every group above this one in its stack has been applied.
    inverse: Vec<TextPatch>,
    /// Revision at the end of the group; typing may merge into the group only
    /// while the document is still at this revision.
    end_revision: u64,
    /// An unrecorded change overlapped this group; it can no longer be applied.
    blocked: bool,
    selection_before: Option<Vec<TextRange>>,
    selection_after: Option<Vec<TextRange>>,
}

impl UndoGroup {
    /// Bytes this group holds: its record and everything the record owns
    /// (the patches and their texts, the selections, the merge key, and the
    /// identifier of the origin).
    fn held(&self) -> usize {
        let ranges = |selection: &Option<Vec<TextRange>>| selection.as_ref().map_or(0, Vec::len);
        held::UNDO_STEP
            + self.key.as_ref().map_or(0, String::len)
            + origin_bytes(&self.origin)
            + self.inverse.iter().map(|p| held::UNDO_PATCH + p.text.len()).sum::<usize>()
            + (ranges(&self.selection_before) + ranges(&self.selection_after)) * held::SELECTION_RANGE
    }
}

impl Default for DocumentService {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentService {
    pub fn new() -> Self {
        Self::with_limits(HistoryLimits::default())
    }

    /// A service whose documents keep their histories within `limits`.
    #[doc(hidden)]
    pub fn with_limits(limits: HistoryLimits) -> Self {
        let (events, _) = broadcast::channel(4096);
        Self { inner: Mutex::new(Inner { limits, ..Inner::default() }), events }
    }

    /// Subscribe to document events. Events for one document arrive in revision order.
    pub fn subscribe(&self) -> broadcast::Receiver<DocumentEvent> {
        self.events.subscribe()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves documents consistent: every
        // mutation validates fully before writing state.
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn emit(&self, event: DocumentEvent) {
        // No receivers is not an error.
        let _ = self.events.send(event);
    }

    /// Open a document. Opening the same workspace path twice returns the
    /// existing document unchanged.
    pub fn open(&self, spec: OpenDocument) -> DocumentInfo {
        let mut inner = self.lock();
        if let Some(path) = &spec.path
            && let Some(existing) = inner.documents.values().find(|d| d.path.as_ref() == Some(path))
        {
            return existing.info();
        }
        inner.next_id += 1;
        let id = DocumentId(format!("doc-{}", inner.next_id));
        let line_ending = detect_line_ending(&spec.text);
        work::copied(spec.text.len());
        let state = DocState {
            id: id.clone(),
            path: spec.path,
            language: spec.language,
            encoding: spec.encoding,
            line_ending,
            read_only: spec.read_only,
            revision: 1,
            saved_revision: Some(1),
            text: Arc::from(spec.text),
            history: VecDeque::new(),
            history_held: 0,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            undo_held: 0,
            redo_held: 0,
        };
        let info = state.info();
        inner.documents.insert(id, state);
        self.emit(DocumentEvent::Opened { info: info.clone() });
        info
    }

    pub fn close(&self, id: &DocumentId) -> Result<(), TransactionError> {
        let mut inner = self.lock();
        inner.documents.remove(id).ok_or_else(|| TransactionError::UnknownDocument { document: id.clone() })?;
        self.emit(DocumentEvent::Closed { document: id.clone() });
        Ok(())
    }

    pub fn find_by_path(&self, path: &WorkspacePath) -> Option<DocumentId> {
        self.lock().documents.values().find(|d| d.path.as_ref() == Some(path)).map(|d| d.id.clone())
    }

    pub fn info(&self, id: &DocumentId) -> Result<DocumentInfo, TransactionError> {
        let inner = self.lock();
        Ok(get(&inner, id)?.info())
    }

    pub fn list(&self) -> Vec<DocumentInfo> {
        let mut infos: Vec<_> = self.lock().documents.values().map(DocState::info).collect();
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        infos
    }

    pub fn state(&self, id: &DocumentId) -> Result<DocumentState, TransactionError> {
        let inner = self.lock();
        let doc = get(&inner, id)?;
        work::copied(doc.text.len());
        Ok(DocumentState { info: doc.info(), text: doc.text.to_string() })
    }

    /// What the undo, redo, and rebase histories of a document hold.
    #[doc(hidden)]
    pub fn retained(&self, id: &DocumentId) -> Result<RetainedHistory, TransactionError> {
        let inner = self.lock();
        let doc = get(&inner, id)?;
        let bytes = |patches: &[TextPatch]| patches.iter().map(|p| p.text.len()).sum::<usize>();
        Ok(RetainedHistory {
            undo_groups: doc.undo.len(),
            undo_bytes: doc.undo.iter().map(|g| bytes(&g.inverse)).sum(),
            redo_groups: doc.redo.len(),
            redo_bytes: doc.redo.iter().map(|g| bytes(&g.inverse)).sum(),
            history_entries: doc.history.len(),
            history_bytes: doc.history.iter().map(HistoryEntry::held).sum(),
            undo_redo_held: doc.undo.iter().chain(&doc.redo).map(UndoGroup::held).sum(),
            undo_redo_charged: doc.undo_held + doc.redo_held,
            history_charged: doc.history_held,
        })
    }

    /// The document's info and text at one revision, sharing the buffer
    /// (no copy of the text, unlike [`Self::state`]).
    pub fn shared_state(&self, id: &DocumentId) -> Result<(DocumentInfo, Arc<str>), TransactionError> {
        let inner = self.lock();
        let doc = get(&inner, id)?;
        Ok((doc.info(), Arc::clone(&doc.text)))
    }

    pub fn snapshot(&self, id: &DocumentId) -> Result<DocumentSnapshot, TransactionError> {
        let inner = self.lock();
        Ok(get(&inner, id)?.snapshot())
    }

    /// Snapshots of every open document, taken atomically (for build snapshots).
    pub fn snapshots(&self) -> Vec<DocumentSnapshot> {
        self.lock().documents.values().map(DocState::snapshot).collect()
    }

    /// Who made each change after `revision`, up to the current revision
    /// (edits, undos and redos, oldest first). `None` when the document's
    /// history no longer reaches back to `revision`, so it cannot be told.
    pub fn origins_since(&self, id: &DocumentId, revision: u64) -> Result<Option<Vec<EditOrigin>>, TransactionError> {
        let inner = self.lock();
        let doc = get(&inner, id)?;
        if revision > doc.revision {
            return Err(TransactionError::FutureRevision { document: id.clone(), base_revision: revision });
        }
        let origins: Vec<EditOrigin> =
            doc.history.iter().filter(|e| e.revision > revision).map(|e| e.origin.clone()).collect();
        Ok((origins.len() as u64 == doc.revision - revision).then_some(origins))
    }

    /// Record that `revision` was written to disk (or the buffer matches disk).
    pub fn mark_saved(&self, id: &DocumentId, revision: u64) -> Result<DocumentInfo, TransactionError> {
        let mut inner = self.lock();
        let doc = get_mut(&mut inner, id)?;
        if revision > doc.revision {
            return Err(TransactionError::FutureRevision { document: id.clone(), base_revision: revision });
        }
        doc.saved_revision = Some(revision);
        let info = doc.info();
        self.emit(DocumentEvent::InfoChanged { info: info.clone() });
        Ok(info)
    }

    /// Update the saved encoding (e.g. after "save with encoding").
    pub fn set_encoding(&self, id: &DocumentId, encoding: TextEncoding) -> Result<DocumentInfo, TransactionError> {
        let mut inner = self.lock();
        let doc = get_mut(&mut inner, id)?;
        doc.encoding = encoding;
        let info = doc.info();
        self.emit(DocumentEvent::InfoChanged { info: info.clone() });
        Ok(info)
    }

    pub fn set_read_only(&self, id: &DocumentId, reason: Option<String>) -> Result<DocumentInfo, TransactionError> {
        let mut inner = self.lock();
        let doc = get_mut(&mut inner, id)?;
        doc.read_only = reason;
        let info = doc.info();
        self.emit(DocumentEvent::InfoChanged { info: info.clone() });
        Ok(info)
    }

    /// A renamed or moved file: the same document (text, revisions, undo
    /// history, saved state) under its new path, with the language of its new
    /// name. One `InfoChanged` event.
    pub fn relocate(
        &self,
        id: &DocumentId,
        path: WorkspacePath,
        language: String,
    ) -> Result<DocumentInfo, TransactionError> {
        let mut inner = self.lock();
        let doc = get_mut(&mut inner, id)?;
        doc.path = Some(path);
        doc.language = language;
        let info = doc.info();
        self.emit(DocumentEvent::InfoChanged { info: info.clone() });
        Ok(info)
    }

    /// Validate and apply a transaction. Either every edit applies or none does.
    pub fn apply(&self, request: TransactionRequest) -> Result<TransactionOutcome, TransactionError> {
        if request.edits.is_empty() || request.edits.iter().all(|e| e.patches.is_empty()) {
            return Err(TransactionError::Empty);
        }
        let mut inner = self.lock();

        // Phase 1: validate and compute every new text without mutating state.
        let mut seen = Vec::with_capacity(request.edits.len());
        let mut planned = Vec::with_capacity(request.edits.len());
        for edit in &request.edits {
            if seen.contains(&&edit.document) {
                return Err(TransactionError::DuplicateDocument { document: edit.document.clone() });
            }
            seen.push(&edit.document);
            let doc = get(&inner, &edit.document)?;
            planned.push(plan_edit(doc, edit, request.conflict, &request.origin)?);
        }

        // Phase 2: commit.
        let limits = inner.limits;
        let mut applied = Vec::with_capacity(planned.len());
        for plan in planned {
            let doc = get_mut(&mut inner, &plan.document).expect("validated above");
            let previous_revision = doc.revision;
            let shapes = shapes(&plan.patches);
            let inverse = inverse_patches(&doc.text, &plan.spans, &shapes);
            let line_ending_changed = doc.commit(plan.text, shapes, request.origin.clone(), plan.line_ending, &limits);
            doc.redo.clear();
            doc.redo_held = 0;
            let edit = RecordedEdit {
                inverse,
                applied: &plan.patches,
                spans: &plan.spans,
                previous_revision,
                before_text: &plan.before_text,
            };
            record_undo(doc, &request, edit, &limits);
            let event = DocumentChanged {
                document: doc.id.clone(),
                transaction: Some(request.id.clone()),
                origin: request.origin.clone(),
                change: ChangeKind::Edit,
                previous_revision,
                revision: doc.revision,
                patches: plan.patches.clone(),
                selection: request.selection.as_ref().map(|s| s.after.clone()),
                can_undo: !doc.undo.is_empty(),
                can_redo: !doc.redo.is_empty(),
            };
            applied.push(AppliedEdit {
                document: doc.id.clone(),
                previous_revision,
                revision: doc.revision,
                patches: plan.patches,
                rebased: plan.rebased,
            });
            self.emit(DocumentEvent::Changed(event));
            if line_ending_changed {
                self.emit(DocumentEvent::InfoChanged { info: doc.info() });
            }
        }
        Ok(TransactionOutcome { id: request.id, applied })
    }

    /// Undo the most recent undo group of a document.
    pub fn undo(&self, id: &DocumentId, origin: EditOrigin) -> Result<DocumentChanged, TransactionError> {
        self.step_history(id, origin, ChangeKind::Undo)
    }

    /// Redo the most recently undone group of a document.
    pub fn redo(&self, id: &DocumentId, origin: EditOrigin) -> Result<DocumentChanged, TransactionError> {
        self.step_history(id, origin, ChangeKind::Redo)
    }

    fn step_history(
        &self,
        id: &DocumentId,
        origin: EditOrigin,
        kind: ChangeKind,
    ) -> Result<DocumentChanged, TransactionError> {
        let mut inner = self.lock();
        let limits = inner.limits;
        let doc = get_mut(&mut inner, id)?;
        if let Some(reason) = &doc.read_only {
            return Err(TransactionError::ReadOnly { document: id.clone(), reason: reason.clone() });
        }
        let stack = if kind == ChangeKind::Undo { &doc.undo } else { &doc.redo };
        let Some(group) = stack.back() else {
            let action = if kind == ChangeKind::Undo { "undo" } else { "redo" };
            return Err(TransactionError::NothingToUndo { document: id.clone(), action: action.into() });
        };
        if group.blocked {
            return Err(TransactionError::Conflict {
                document: id.clone(),
                base_revision: group.end_revision,
                current_revision: doc.revision,
            });
        }
        // The group's inverse is already relative to the current text.
        let patches = group.inverse.clone();
        let spans = locate(&doc.text, &patches).map_err(|range| invalid_range(doc, range))?;
        let text = splice(&doc.text, &spans, &patches);
        let line_ending = line_ending_after(doc.line_ending, &doc.text, &spans, &patches)
            .unwrap_or_else(|| detect_line_ending(&text));
        if doc.line_ending == LineEnding::Mixed && line_ending == LineEnding::Mixed {
            return Err(TransactionError::MixedLineEndings { document: id.clone() });
        }
        let group = if kind == ChangeKind::Undo {
            let group = doc.undo.pop_back().expect("checked above");
            doc.undo_held -= group.held();
            group
        } else {
            let group = doc.redo.pop_back().expect("checked above");
            doc.redo_held -= group.held();
            group
        };
        let previous_revision = doc.revision;
        let shapes = shapes(&patches);
        let inverse = inverse_patches(&doc.text, &spans, &shapes);
        let line_ending_changed = doc.commit(text, shapes, origin.clone(), line_ending, &limits);
        let selection =
            if kind == ChangeKind::Undo { group.selection_before.clone() } else { group.selection_after.clone() };
        // The opposite group keeps the original selections: undo restores
        // `selection_before`, redo restores `selection_after`.
        let opposite = UndoGroup {
            key: None,
            origin: group.origin.clone(),
            inverse,
            end_revision: doc.revision,
            blocked: false,
            selection_before: group.selection_before,
            selection_after: group.selection_after,
        };
        // The stacks hold at most the group limit together: this group came
        // off the other stack.
        if kind == ChangeKind::Undo {
            doc.redo_held += opposite.held();
            doc.redo.push_back(opposite);
            doc.trim_undo(Newest::Redo, &limits);
        } else {
            doc.undo_held += opposite.held();
            doc.undo.push_back(opposite);
            doc.trim_undo(Newest::Undo, &limits);
        }
        let event = DocumentChanged {
            document: doc.id.clone(),
            transaction: None,
            origin,
            change: kind,
            previous_revision,
            revision: doc.revision,
            patches,
            selection,
            can_undo: !doc.undo.is_empty(),
            can_redo: !doc.redo.is_empty(),
        };
        self.emit(DocumentEvent::Changed(event.clone()));
        if line_ending_changed {
            self.emit(DocumentEvent::InfoChanged { info: doc.info() });
        }
        Ok(event)
    }
}

impl DocState {
    fn info(&self) -> DocumentInfo {
        DocumentInfo {
            id: self.id.clone(),
            path: self.path.clone(),
            revision: self.revision,
            saved_revision: self.saved_revision,
            language: self.language.clone(),
            line_ending: self.line_ending,
            encoding: self.encoding,
            read_only: self.read_only.clone(),
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    fn snapshot(&self) -> DocumentSnapshot {
        DocumentSnapshot {
            id: self.id.clone(),
            path: self.path.clone(),
            revision: self.revision,
            text: Arc::clone(&self.text),
            language: self.language.clone(),
        }
    }

    /// Record a new revision whose text has the line-ending convention
    /// `line_ending`. Returns true when that convention changed (the caller
    /// announces the new info).
    fn commit(
        &mut self,
        text: String,
        patches: Vec<PatchShape>,
        origin: EditOrigin,
        line_ending: LineEnding,
        limits: &HistoryLimits,
    ) -> bool {
        self.revision += 1;
        work::copied(text.len());
        self.text = Arc::from(text);
        let entry = HistoryEntry { revision: self.revision, patches, origin };
        self.history_held += entry.held();
        self.history.push_back(entry);
        // Oldest revisions first; the newest one always stays. An edit based
        // on a revision dropped here is refused by `rebase`.
        while self.history.len() > limits.history_entries
            || (self.history_held > limits.history_bytes && self.history.len() > 1)
        {
            let Some(dropped) = self.history.pop_front() else { break };
            self.history_held -= dropped.held();
        }
        let changed = line_ending != self.line_ending;
        self.line_ending = line_ending;
        changed
    }

    /// Bring the undo and redo stacks back within the byte budget by dropping
    /// whole groups: the oldest undo groups first, then the redo groups
    /// furthest from the current text (the bottom of each stack, which no
    /// other group is relative to). The group made last, on top of the
    /// `newest` stack, always stays, so the last step can be taken back even
    /// when it alone exceeds the budget. Costs the dropped groups only.
    fn trim_undo(&mut self, newest: Newest, limits: &HistoryLimits) {
        while self.undo_held + self.redo_held > limits.undo_bytes {
            if self.undo.len() > usize::from(newest == Newest::Undo) {
                let Some(dropped) = self.undo.pop_front() else { break };
                self.undo_held -= dropped.held();
            } else if self.redo.len() > usize::from(newest == Newest::Redo) {
                let Some(dropped) = self.redo.pop_front() else { break };
                self.redo_held -= dropped.held();
            } else {
                break;
            }
        }
    }
}

/// The stack whose top group is the step made last.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Newest {
    Undo,
    Redo,
}

struct PlannedEdit {
    document: DocumentId,
    patches: Vec<TextPatch>,
    /// Byte span of each patch in the text before the edit.
    spans: Vec<Span>,
    text: String,
    /// Line-ending convention of `text`.
    line_ending: LineEnding,
    rebased: bool,
    /// Text before the edit, kept so merged undo groups can be recomputed.
    before_text: Arc<str>,
}

fn plan_edit(
    doc: &DocState,
    edit: &DocumentEdit,
    policy: ConflictPolicy,
    origin: &EditOrigin,
) -> Result<PlannedEdit, TransactionError> {
    if let Some(reason) = &doc.read_only {
        return Err(TransactionError::ReadOnly { document: doc.id.clone(), reason: reason.clone() });
    }
    if edit.base_revision > doc.revision {
        return Err(TransactionError::FutureRevision { document: doc.id.clone(), base_revision: edit.base_revision });
    }
    check_sorted(doc, &edit.patches)?;
    let (patches, rebased) = if edit.base_revision == doc.revision {
        (edit.patches.clone(), false)
    } else {
        match policy {
            ConflictPolicy::Reject => {
                return Err(TransactionError::StaleRevision {
                    document: doc.id.clone(),
                    base_revision: edit.base_revision,
                    current_revision: doc.revision,
                });
            }
            ConflictPolicy::RebaseDisjoint => (rebase(doc, edit.base_revision, &edit.patches)?, true),
        }
    };
    let spans = locate(&doc.text, &patches).map_err(|range| invalid_range(doc, range))?;
    let text = splice(&doc.text, &spans, &patches);
    let line_ending =
        line_ending_after(doc.line_ending, &doc.text, &spans, &patches).unwrap_or_else(|| detect_line_ending(&text));
    // Mixed line endings: only a change that makes them consistent, or one
    // that restores content from disk or a journal, is accepted.
    let restores = matches!(origin, EditOrigin::External | EditOrigin::Recovery);
    if doc.line_ending == LineEnding::Mixed && !restores && line_ending == LineEnding::Mixed {
        return Err(TransactionError::MixedLineEndings { document: doc.id.clone() });
    }
    Ok(PlannedEdit {
        document: doc.id.clone(),
        patches,
        spans,
        text,
        line_ending,
        rebased,
        before_text: Arc::clone(&doc.text),
    })
}

fn invalid_range(doc: &DocState, range: TextRange) -> TransactionError {
    let length = u32::try_from(utf16_len(&doc.text)).unwrap_or(u32::MAX);
    if range.end <= length && range.start <= range.end {
        TransactionError::NotCharBoundary { document: doc.id.clone(), range }
    } else {
        TransactionError::InvalidRange { document: doc.id.clone(), range, length }
    }
}

fn check_sorted(doc: &DocState, patches: &[TextPatch]) -> Result<(), TransactionError> {
    for patch in patches {
        if !patch.range.is_ordered() {
            return Err(TransactionError::InvalidRange {
                document: doc.id.clone(),
                range: patch.range,
                length: u32::try_from(utf16_len(&doc.text)).unwrap_or(u32::MAX),
            });
        }
    }
    for pair in patches.windows(2) {
        if pair[0].range.end > pair[1].range.start {
            return Err(TransactionError::OverlappingPatches { document: doc.id.clone() });
        }
    }
    Ok(())
}

/// Forward-only UTF-16 to byte offset conversion. Converting the sorted
/// offsets of a whole patch list costs one pass over the text, not one pass
/// per patch.
struct Utf16Cursor<'a> {
    text: &'a str,
    byte: usize,
    unit: usize,
}

/// Bytes whose UTF-16 length is counted at once by [`Utf16Cursor::seek`].
const SEEK_BLOCK: usize = 4096;

/// UTF-16 units of the characters that start in `bytes` (UTF-8): one per
/// lead byte, two for the lead of a four-byte character, none for a
/// continuation byte.
fn utf16_units_started(bytes: &[u8]) -> usize {
    bytes.iter().map(|&b| usize::from((b as i8) >= -0x40) + usize::from(b >= 0xF0)).sum()
}

impl<'a> Utf16Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, byte: 0, unit: 0 }
    }

    /// A cursor at a known position: `byte` is a character boundary of `text`
    /// at UTF-16 offset `unit`.
    fn at(text: &'a str, byte: usize, unit: usize) -> Self {
        debug_assert!(text.is_char_boundary(byte));
        Self { text, byte, unit }
    }

    /// Byte offset of UTF-16 offset `target`; `None` if it lies before the
    /// previous target, inside a surrogate pair, or past the end.
    fn seek(&mut self, target: u32) -> Option<usize> {
        let from = self.byte;
        let found = self.seek_chars(target as usize);
        work::scanned(self.byte - from);
        found
    }

    fn seek_chars(&mut self, target: usize) -> Option<usize> {
        if target < self.unit {
            return None;
        }
        let text = self.text;
        // Whole blocks that end before the target are counted bytewise, not
        // decoded. A block may end inside a character: its units are already
        // counted, so the bytes up to the next boundary add nothing.
        // A block is only counted when the target is at least a block's
        // length in units away: a nearer target is reached by decoding, so a
        // transaction of many close patches does not count a block per patch.
        let bytes = text.as_bytes();
        while target - self.unit >= SEEK_BLOCK {
            let Some(block) = bytes.get(self.byte..self.byte + SEEK_BLOCK) else { break };
            let units = utf16_units_started(block);
            if self.unit + units >= target {
                work::scanned(SEEK_BLOCK);
                break;
            }
            self.unit += units;
            self.byte += SEEK_BLOCK;
        }
        while !text.is_char_boundary(self.byte) {
            self.byte += 1;
        }
        for ch in text[self.byte..].chars() {
            if self.unit == target {
                return Some(self.byte);
            }
            let width = ch.len_utf16();
            if self.unit + width > target {
                return None;
            }
            self.unit += width;
            self.byte += ch.len_utf8();
        }
        (self.unit == target).then_some(self.byte)
    }
}

/// Byte span `(start, end)` of a patch range in the text it applies to.
type Span = (usize, usize);

/// Byte spans of sorted, non-overlapping patches in `text`, found in one
/// forward pass that continues from `cursor`. Returns the offending range on
/// failure.
fn locate_from(cursor: &mut Utf16Cursor<'_>, patches: &[TextPatch]) -> Result<Vec<Span>, TextRange> {
    let mut spans = Vec::with_capacity(patches.len());
    for patch in patches {
        if !patch.range.is_ordered() {
            return Err(patch.range);
        }
        let start = cursor.seek(patch.range.start).ok_or(patch.range)?;
        let end = cursor.seek(patch.range.end).ok_or(patch.range)?;
        spans.push((start, end));
    }
    Ok(spans)
}

fn locate(text: &str, patches: &[TextPatch]) -> Result<Vec<Span>, TextRange> {
    locate_from(&mut Utf16Cursor::new(text), patches)
}

/// `text` with each span replaced by the text of its patch.
fn splice(text: &str, spans: &[Span], patches: &[TextPatch]) -> String {
    let mut out = String::with_capacity(text.len() + patches.iter().map(|p| p.text.len()).sum::<usize>());
    let mut copied = 0usize;
    for (&(start, end), patch) in spans.iter().zip(patches) {
        out.push_str(&text[copied..start]);
        out.push_str(&patch.text);
        copied = end;
    }
    out.push_str(&text[copied..]);
    work::copied(out.len());
    out
}

/// Apply sorted, non-overlapping patches to `text` in one pass. Returns the
/// offending range on failure.
fn apply_patches(text: &str, patches: &[TextPatch]) -> Result<String, TextRange> {
    Ok(splice(text, &locate(text, patches)?, patches))
}

/// Patches that undo the patches of shape `patches` (sorted, relative to
/// `old`, at byte `spans`), relative to the new text.
fn inverse_patches(old: &str, spans: &[Span], patches: &[PatchShape]) -> Vec<TextPatch> {
    let mut shift: i64 = 0;
    spans
        .iter()
        .zip(patches)
        .map(|(&(from, to), patch)| {
            let start = (i64::from(patch.range.start) + shift) as u32;
            shift += i64::from(patch.inserted) - i64::from(patch.range.len());
            TextPatch { range: TextRange::new(start, start + patch.inserted), text: old[from..to].to_owned() }
        })
        .collect()
}

/// Map `patches` (relative to `base_revision`) over every revision applied since.
fn rebase(doc: &DocState, base_revision: u64, patches: &[TextPatch]) -> Result<Vec<TextPatch>, TransactionError> {
    if base_revision == doc.revision {
        return Ok(patches.to_vec());
    }
    let oldest_available = doc.history.front().map(|e| e.revision - 1).unwrap_or(doc.revision);
    if base_revision < oldest_available {
        return Err(TransactionError::HistoryUnavailable { document: doc.id.clone(), base_revision });
    }
    let mut mapped = patches.to_vec();
    for entry in doc.history.iter().filter(|e| e.revision > base_revision) {
        for patch in &mut mapped {
            patch.range = map_range(patch.range, &entry.patches).ok_or_else(|| TransactionError::Conflict {
                document: doc.id.clone(),
                base_revision,
                current_revision: doc.revision,
            })?;
        }
    }
    Ok(mapped)
}

/// Map a range through one revision's patches; `None` when they overlap.
///
/// A concurrent insertion at the start of `range` lands before it; one at the
/// end of a non-empty range lands after it; one strictly inside conflicts.
fn map_range(range: TextRange, concurrent: &[impl Shaped]) -> Option<TextRange> {
    map_range_biased(range, concurrent, true)
}

/// [`map_range`] with an explicit tie rule: `ties_before` decides whether a
/// concurrent insertion at exactly the point of an empty `range` (another
/// insertion) lands before it. Transforming two edits against each other must
/// use opposite rules so both agree on the order.
fn map_range_biased(range: TextRange, concurrent: &[impl Shaped], ties_before: bool) -> Option<TextRange> {
    let mut shift: i64 = 0;
    for q in concurrent {
        let q = PatchShape { range: q.range(), inserted: q.inserted() };
        let delta = i64::from(q.inserted) - i64::from(q.range.len());
        if q.range.is_empty() {
            let at = q.range.start;
            if at < range.start || (at == range.start && (ties_before || !range.is_empty())) {
                shift += delta;
            } else if at < range.end {
                return None;
            }
        } else if q.range.end <= range.start {
            shift += delta;
        } else if q.range.start >= range.end {
            // After the range (including a replacement starting where an insertion sits).
        } else {
            return None;
        }
    }
    let start = i64::from(range.start) + shift;
    let end = i64::from(range.end) + shift;
    Some(TextRange::new(u32::try_from(start).ok()?, u32::try_from(end).ok()?))
}

/// The ranges of `patches` mapped through `through`; `None` when any overlaps.
fn map_ranges(patches: &[impl Shaped], through: &[impl Shaped], ties_before: bool) -> Option<Vec<TextRange>> {
    patches.iter().map(|p| map_range_biased(p.range(), through, ties_before)).collect()
}

/// Re-express an undo stack after an unrecorded change `applied` (relative to
/// the text before it). Each group's inverse is relative to the text with every
/// group above it undone, so the change is transformed down the stack alongside
/// the groups. The first group it overlaps is blocked; groups below a blocked
/// group are unreachable by linear undo and are left as they are. Only ranges
/// move: no text is copied and no group changes the bytes it holds.
fn transform_stack(stack: &mut VecDeque<UndoGroup>, applied: &[TextPatch]) {
    let mut concurrent = shapes(applied);
    for group in stack.iter_mut().rev() {
        if group.blocked {
            return;
        }
        match (map_ranges(&group.inverse, &concurrent, true), map_ranges(&concurrent, &group.inverse, false)) {
            (Some(inverse), Some(next)) => {
                for (patch, range) in group.inverse.iter_mut().zip(inverse) {
                    patch.range = range;
                }
                for (shape, range) in concurrent.iter_mut().zip(next) {
                    shape.range = range;
                }
            }
            _ => {
                group.blocked = true;
                return;
            }
        }
    }
}

/// An edit that was just committed, as the undo stack records it.
struct RecordedEdit<'a> {
    /// Restores the text before the edit; relative to the new text.
    inverse: Vec<TextPatch>,
    /// The edit's patches, at byte `spans` of `before_text`.
    applied: &'a [TextPatch],
    spans: &'a [Span],
    previous_revision: u64,
    before_text: &'a Arc<str>,
}

fn record_undo(doc: &mut DocState, request: &TransactionRequest, edit: RecordedEdit<'_>, limits: &HistoryLimits) {
    let RecordedEdit { inverse, applied, spans, previous_revision, before_text } = edit;
    let (key, merge) = match &request.undo {
        UndoBehavior::Skip => {
            transform_stack(&mut doc.undo, applied);
            return;
        }
        UndoBehavior::NewGroup => (None, false),
        UndoBehavior::Merge { key } => (Some(key.clone()), true),
    };
    let selection_before = request.selection.as_ref().map(|s| s.before.clone());
    let selection_after = request.selection.as_ref().map(|s| s.after.clone());
    if merge
        && let Some(top) = doc.undo.back_mut()
        && top.key == key
        && !top.blocked
        && std::mem::discriminant(&top.origin) == std::mem::discriminant(&request.origin)
        && top.end_revision == previous_revision
    {
        // The merged group's inverse is the single patch from the new text
        // to the group's start text.
        let held_before = top.held();
        top.inverse = merged_inverse(before_text, &doc.text, &top.inverse, applied, spans)
            .unwrap_or_else(|| merged_inverse_by_rebuilding(before_text, &doc.text, &top.inverse));
        top.end_revision = doc.revision;
        top.selection_after = selection_after;
        doc.undo_held = doc.undo_held - held_before + top.held();
        // A group that grows is never cut: older groups make room for it.
        doc.trim_undo(Newest::Undo, limits);
        return;
    }
    let group = UndoGroup {
        key,
        origin: request.origin.clone(),
        inverse,
        end_revision: doc.revision,
        blocked: false,
        selection_before,
        selection_after,
    };
    doc.undo_held += group.held();
    doc.undo.push_back(group);
    if doc.undo.len() > limits.undo_groups
        && let Some(dropped) = doc.undo.pop_front()
    {
        doc.undo_held -= dropped.held();
    }
    doc.trim_undo(Newest::Undo, limits);
}

/// The inverse of an undo group after one more edit merged into it, the
/// plain way: restore the group's start text from the text before the edit
/// (`before`, to which `group` is relative), then diff the new text (`after`)
/// against it. Two full copies and passes of the text.
fn merged_inverse_by_rebuilding(before: &str, after: &str, group: &[TextPatch]) -> Vec<TextPatch> {
    let start_text = apply_patches(before, group).expect("undo group matches its end revision");
    diff_patch(after, &start_text).into_iter().collect()
}

/// The same result as [`merged_inverse_by_rebuilding`], computed from the
/// part of the text that the group and the edit (`applied`, at byte `spans`
/// of `before`) touch: outside it `after` and the group's start text are both
/// `before`. `None` when the group cannot be located in `before` from the
/// edit's position; the caller then rebuilds.
fn merged_inverse(
    before: &str,
    after: &str,
    group: &[TextPatch],
    applied: &[TextPatch],
    spans: &[Span],
) -> Option<Vec<TextPatch>> {
    let (&(edit_start, _), &(_, edit_end)) = (spans.first()?, spans.last()?);
    let edit_unit = applied.first()?.range.start as usize;
    // Where the group's patches lie in `before`, found from the edit's
    // position (typing merges next to its group) instead of from the start.
    let (group_spans, lo, lo_unit, hi) = match group.first() {
        None => (Vec::new(), edit_start, edit_unit, edit_end),
        Some(first) => {
            let first_unit = first.range.start as usize;
            let (byte, unit) = if first_unit >= edit_unit {
                (edit_start, edit_unit)
            } else {
                // Walk back from the edit to the group's first offset.
                let mut byte = edit_start;
                let mut unit = edit_unit;
                for ch in before[..edit_start].chars().rev() {
                    if unit <= first_unit {
                        break;
                    }
                    unit = unit.checked_sub(ch.len_utf16())?;
                    byte -= ch.len_utf8();
                }
                work::scanned(edit_start - byte);
                if unit != first_unit {
                    return None;
                }
                (byte, unit)
            };
            let group_spans = locate_from(&mut Utf16Cursor::at(before, byte, unit), group).ok()?;
            let (&(group_start, _), &(_, group_end)) = (group_spans.first()?, group_spans.last()?);
            let (lo, lo_unit) =
                if group_start < edit_start { (group_start, first_unit) } else { (edit_start, edit_unit) };
            (group_spans, lo, lo_unit, group_end.max(edit_end))
        }
    };
    let tail = before.len() - hi;
    // The touched part: as it is now, and as it was at the start of the group.
    let now = after.get(lo..after.len().checked_sub(tail)?)?;
    let mut then = String::with_capacity(hi - lo);
    let mut copied = lo;
    for (&(start, end), patch) in group_spans.iter().zip(group) {
        then.push_str(&before[copied..start]);
        then.push_str(&patch.text);
        copied = end;
    }
    then.push_str(&before[copied..hi]);
    if now == then {
        return Some(Vec::new());
    }
    // Common prefix, compared as `diff_patch` does (it may run on into the
    // unchanged text after the touched part).
    let prefix: usize = after[lo..]
        .chars()
        .zip(then.chars().chain(before[hi..].chars()))
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    let start = (lo_unit + utf16_len(&after[lo..lo + prefix])) as u32;
    if prefix > now.len() || prefix > then.len() {
        // The common prefix ran on into the unchanged text (typing "the"
        // right before "then"). What is left of the shorter side is then a
        // suffix of what is left of the longer one, so the texts differ by
        // the length difference of the touched parts, right after the prefix.
        work::scanned(2 * prefix);
        let at = lo + prefix;
        return Some(vec![if now.len() > then.len() {
            let removed = &after[at..at + (now.len() - then.len())];
            TextPatch { range: TextRange::new(start, start + utf16_len(removed) as u32), text: String::new() }
        } else {
            // The start text from the same point: the rest of its touched
            // part, then the unchanged text.
            let missing = then.len() - now.len();
            let own = then.get(prefix..).unwrap_or("");
            let mut text = String::with_capacity(missing);
            text.push_str(&own[..own.len().min(missing)]);
            let from_tail = hi + prefix.saturating_sub(then.len());
            text.push_str(&before[from_tail..from_tail + (missing - text.len())]);
            TextPatch { range: TextRange::empty(start), text }
        }]);
    }
    let (now_rest, then_rest) = (&now[prefix..], &then[prefix..]);
    // The unchanged text after the touched part is common to both; the
    // common suffix continues from there into the touched part.
    let suffix: usize = now_rest
        .chars()
        .rev()
        .zip(then_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum();
    work::scanned(2 * prefix + suffix);
    let end = start + utf16_len(&now_rest[..now_rest.len() - suffix]) as u32;
    Some(vec![TextPatch { range: TextRange::new(start, end), text: then_rest[..then_rest.len() - suffix].to_owned() }])
}

/// The line-ending convention of a text after `patches` (at byte `spans` of
/// `text`, whose convention is `old`), when the edit itself decides it: no
/// terminator is removed, split, or joined, so the conventions present are
/// the old ones plus those of the inserted text. `None` when only a scan of
/// the result can tell.
fn line_ending_after(old: LineEnding, text: &str, spans: &[Span], patches: &[TextPatch]) -> Option<LineEnding> {
    let bytes = text.as_bytes();
    let is_break = |b: &u8| *b == b'\r' || *b == b'\n';
    let mut added = LineEnding::None;
    let mut previous_end = None;
    for (&(start, end), patch) in spans.iter().zip(patches) {
        // Patches that touch can form a terminator between them.
        if previous_end == Some(start) {
            return None;
        }
        previous_end = Some(end);
        if bytes[start..end].iter().any(is_break) {
            return None;
        }
        let before = start.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(end).copied();
        if patch.text.as_bytes().iter().any(is_break) {
            // An inserted terminator next to an existing `\r` or `\n` can merge with it.
            if before == Some(b'\r') || after == Some(b'\n') {
                return None;
            }
            added = merge_line_endings(added, detect_line_ending(&patch.text));
        } else if before == Some(b'\r') && after == Some(b'\n') {
            // Splits a CRLF (insertion) or joins a CR and an LF (deletion).
            return None;
        }
    }
    Some(merge_line_endings(old, added))
}

/// The convention of a text that has the terminators of both arguments.
fn merge_line_endings(a: LineEnding, b: LineEnding) -> LineEnding {
    match (a, b) {
        (LineEnding::None, other) | (other, LineEnding::None) => other,
        (a, b) if a == b => a,
        _ => LineEnding::Mixed,
    }
}

/// Single patch turning `from` into `to` (common prefix/suffix trimmed).
fn diff_patch(from: &str, to: &str) -> Option<TextPatch> {
    if from == to {
        return None;
    }
    let prefix: usize = from.chars().zip(to.chars()).take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum();
    let from_rest = &from[prefix..];
    let to_rest = &to[prefix..];
    let suffix: usize = from_rest
        .chars()
        .rev()
        .zip(to_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>()
        .min(from_rest.len())
        .min(to_rest.len());
    work::scanned(2 * prefix + suffix);
    let start = utf16_len(&from[..prefix]) as u32;
    let end = start + utf16_len(&from_rest[..from_rest.len() - suffix]) as u32;
    Some(TextPatch { range: TextRange::new(start, end), text: to_rest[..to_rest.len() - suffix].to_owned() })
}

/// The text's line-ending convention, in one pass (run on every revision).
fn detect_line_ending(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
    work::scanned(bytes.len());
    let (mut lf, mut crlf, mut cr) = (false, false, false);
    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'\n' if i > 0 && bytes[i - 1] == b'\r' => crlf = true,
            b'\n' => lf = true,
            b'\r' if bytes.get(i + 1) != Some(&b'\n') => cr = true,
            _ => continue,
        }
        if u8::from(lf) + u8::from(crlf) + u8::from(cr) > 1 {
            return LineEnding::Mixed;
        }
    }
    match (lf, crlf, cr) {
        (false, false, false) => LineEnding::None,
        (true, false, false) => LineEnding::Lf,
        (false, true, false) => LineEnding::Crlf,
        _ => LineEnding::Mixed,
    }
}

fn get<'a>(inner: &'a Inner, id: &DocumentId) -> Result<&'a DocState, TransactionError> {
    inner.documents.get(id).ok_or_else(|| TransactionError::UnknownDocument { document: id.clone() })
}

fn get_mut<'a>(inner: &'a mut Inner, id: &DocumentId) -> Result<&'a mut DocState, TransactionError> {
    inner.documents.get_mut(id).ok_or_else(|| TransactionError::UnknownDocument { document: id.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use exposition_core::document::{SelectionHint, TransactionId};

    fn service_with(text: &str) -> (DocumentService, DocumentId) {
        let service = DocumentService::new();
        let info = service.open(OpenDocument {
            path: Some(WorkspacePath::parse("main.typ").unwrap()),
            text: text.into(),
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        });
        (service, info.id)
    }

    fn tx(
        doc: &DocumentId,
        base: u64,
        patches: Vec<TextPatch>,
        undo: UndoBehavior,
        conflict: ConflictPolicy,
    ) -> TransactionRequest {
        TransactionRequest {
            id: TransactionId(format!("t-{base}-{}", patches.len())),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit { document: doc.clone(), base_revision: base, patches }],
            undo,
            conflict,
            label: None,
            selection: None,
        }
    }

    fn text(service: &DocumentService, id: &DocumentId) -> String {
        service.state(id).unwrap().text
    }

    #[test]
    fn applies_patches_and_emits_events_in_order() {
        let (service, id) = service_with("hello world");
        let mut rx = service.subscribe();
        let out = service
            .apply(tx(
                &id,
                1,
                vec![TextPatch::replace(0, 5, "HELLO"), TextPatch::insert(11, "!")],
                UndoBehavior::NewGroup,
                ConflictPolicy::Reject,
            ))
            .unwrap();
        assert_eq!(out.applied[0].revision, 2);
        assert_eq!(text(&service, &id), "HELLO world!");
        match rx.try_recv().unwrap() {
            DocumentEvent::Changed(changed) => {
                assert_eq!((changed.previous_revision, changed.revision), (1, 2));
                assert_eq!(changed.patches.len(), 2);
                assert!(changed.can_undo);
            }
            other => panic!("unexpected event {other:?}"),
        }
    }

    #[test]
    fn rejects_stale_edits_under_reject_policy() {
        let (service, id) = service_with("abc");
        service
            .apply(tx(&id, 1, vec![TextPatch::insert(0, "x")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap();
        let err = service
            .apply(tx(&id, 1, vec![TextPatch::insert(3, "y")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap_err();
        assert!(matches!(err, TransactionError::StaleRevision { base_revision: 1, current_revision: 2, .. }));
        assert_eq!(text(&service, &id), "xabc");
    }

    #[test]
    fn rebases_disjoint_stale_edits_and_rejects_overlaps() {
        let (service, id) = service_with("The cat sat.");
        // User edit at revision 1 -> 2: "cat" -> "big cat"
        service
            .apply(tx(&id, 1, vec![TextPatch::insert(4, "big ")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap();
        // Agent edit based on revision 1 replacing "sat" (8..11) is disjoint: rebased to 12..15.
        let out = service
            .apply(tx(
                &id,
                1,
                vec![TextPatch::replace(8, 11, "stood")],
                UndoBehavior::NewGroup,
                ConflictPolicy::RebaseDisjoint,
            ))
            .unwrap();
        assert!(out.applied[0].rebased);
        assert_eq!(text(&service, &id), "The big cat stood.");
        // An edit based on revision 1 touching "cat" (4..7) overlaps the insertion inside? No:
        // insertion at 4 is at the range start, so it is mapped. Replace "The cat" (0..7) overlaps.
        let err = service
            .apply(tx(
                &id,
                1,
                vec![TextPatch::replace(2, 6, "X")],
                UndoBehavior::NewGroup,
                ConflictPolicy::RebaseDisjoint,
            ))
            .unwrap_err();
        assert!(matches!(err, TransactionError::Conflict { .. }), "{err:?}");
        assert_eq!(text(&service, &id), "The big cat stood.");
    }

    #[test]
    fn rejects_invalid_ranges_and_surrogate_splits() {
        let (service, id) = service_with("a😀b");
        let err = service
            .apply(tx(&id, 1, vec![TextPatch::insert(2, "x")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap_err();
        assert!(matches!(err, TransactionError::NotCharBoundary { .. }), "{err:?}");
        let err = service
            .apply(tx(&id, 1, vec![TextPatch::insert(9, "x")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap_err();
        assert!(matches!(err, TransactionError::InvalidRange { .. }), "{err:?}");
        let err = service
            .apply(tx(
                &id,
                1,
                vec![TextPatch::replace(0, 3, "x"), TextPatch::insert(1, "y")],
                UndoBehavior::NewGroup,
                ConflictPolicy::Reject,
            ))
            .unwrap_err();
        assert!(matches!(err, TransactionError::OverlappingPatches { .. }), "{err:?}");
        assert_eq!(service.info(&id).unwrap().revision, 1);
    }

    #[test]
    fn multi_document_transactions_are_all_or_nothing() {
        let service = DocumentService::new();
        let open = |path: &str, text: &str| {
            service
                .open(OpenDocument {
                    path: Some(WorkspacePath::parse(path).unwrap()),
                    text: text.into(),
                    language: "plaintext".into(),
                    encoding: TextEncoding::Utf8,
                    read_only: None,
                })
                .id
        };
        let a = open("a.txt", "aaa");
        let b = open("b.txt", "bbb");
        let request = TransactionRequest {
            id: TransactionId("multi".into()),
            origin: EditOrigin::Agent { task_id: "task-1".into() },
            edits: vec![
                DocumentEdit { document: a.clone(), base_revision: 1, patches: vec![TextPatch::insert(3, "!")] },
                DocumentEdit { document: b.clone(), base_revision: 1, patches: vec![TextPatch::insert(99, "!")] },
            ],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: Some("agent edit".into()),
            selection: None,
        };
        assert!(service.apply(request).is_err());
        assert_eq!(text(&service, &a), "aaa");
        assert_eq!(text(&service, &b), "bbb");
    }

    #[test]
    fn origins_since_lists_who_changed_the_text_or_says_it_cannot_tell() {
        let (service, id) = service_with("abc");
        assert_eq!(service.origins_since(&id, 1).unwrap(), Some(Vec::new()));
        let agent = EditOrigin::Agent { task_id: "t1".into() };
        let mut by_agent = tx(&id, 1, vec![TextPatch::insert(0, "x")], UndoBehavior::NewGroup, ConflictPolicy::Reject);
        by_agent.origin = agent.clone();
        service.apply(by_agent).unwrap();
        service
            .apply(tx(&id, 2, vec![TextPatch::insert(0, "y")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap();
        // An undo is a change by whoever stepped the history.
        service.undo(&id, EditOrigin::Page).unwrap();
        assert_eq!(
            service.origins_since(&id, 1).unwrap(),
            Some(vec![agent.clone(), EditOrigin::Source, EditOrigin::Page])
        );
        assert_eq!(service.origins_since(&id, 3).unwrap(), Some(vec![EditOrigin::Page]));
        assert!(service.origins_since(&id, 9).is_err(), "a revision the document never had");
        // Past the history window it cannot be told.
        for base in 4..(4 + HISTORY_LIMIT as u64) {
            service
                .apply(tx(&id, base, vec![TextPatch::insert(0, "z")], UndoBehavior::Skip, ConflictPolicy::Reject))
                .unwrap();
        }
        assert_eq!(service.origins_since(&id, 1).unwrap(), None);
        assert!(service.origins_since(&id, 10).unwrap().is_some());
    }

    #[test]
    fn undo_and_redo_restore_text_and_selection_across_origins() {
        let (service, id) = service_with("x = 1");
        let mut source_edit =
            tx(&id, 1, vec![TextPatch::replace(4, 5, "2")], UndoBehavior::NewGroup, ConflictPolicy::Reject);
        source_edit.selection =
            Some(SelectionHint { before: vec![TextRange::empty(5)], after: vec![TextRange::empty(5)] });
        service.apply(source_edit).unwrap();
        let mut page_edit =
            tx(&id, 2, vec![TextPatch::insert(0, "let ")], UndoBehavior::NewGroup, ConflictPolicy::Reject);
        page_edit.origin = EditOrigin::Page;
        service.apply(page_edit).unwrap();
        assert_eq!(text(&service, &id), "let x = 2");

        let undone = service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(undone.change, ChangeKind::Undo);
        assert_eq!(text(&service, &id), "x = 2");
        let undone = service.undo(&id, EditOrigin::Page).unwrap();
        assert_eq!(undone.selection, Some(vec![TextRange::empty(5)]));
        assert_eq!(text(&service, &id), "x = 1");
        assert!(service.undo(&id, EditOrigin::Source).is_err());

        service.redo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "x = 2");
        service.redo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "let x = 2");
        assert!(!service.info(&id).unwrap().can_redo);
    }

    #[test]
    fn merged_typing_undoes_as_one_group() {
        let (service, id) = service_with("ab");
        for (i, ch) in (0u32..).zip(["x", "y", "z"]) {
            service
                .apply(tx(
                    &id,
                    1 + u64::from(i),
                    vec![TextPatch::insert(1 + i, ch)],
                    UndoBehavior::Merge { key: "typing-1".into() },
                    ConflictPolicy::Reject,
                ))
                .unwrap();
        }
        assert_eq!(text(&service, &id), "axyzb");
        service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "ab");
        service.redo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "axyzb");
    }

    #[test]
    fn new_edit_clears_redo() {
        let (service, id) = service_with("a");
        service
            .apply(tx(&id, 1, vec![TextPatch::insert(1, "b")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap();
        service.undo(&id, EditOrigin::Source).unwrap();
        assert!(service.info(&id).unwrap().can_redo);
        service
            .apply(tx(&id, 3, vec![TextPatch::insert(1, "c")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap();
        assert!(!service.info(&id).unwrap().can_redo);
        assert_eq!(text(&service, &id), "ac");
    }

    #[test]
    fn undo_rebases_over_unrecorded_changes_or_refuses_on_overlap() {
        let (service, id) = service_with("one two");
        service
            .apply(tx(&id, 1, vec![TextPatch::insert(7, " three")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap();
        // An unrecorded (Skip) change before the group's region.
        service
            .apply(tx(&id, 2, vec![TextPatch::replace(0, 3, "ONE")], UndoBehavior::Skip, ConflictPolicy::Reject))
            .unwrap();
        service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "ONE two");

        service.redo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "ONE two three");
        // An unrecorded change inside the region the undo would restore.
        service
            .apply(tx(&id, 5, vec![TextPatch::replace(9, 12, "HRE")], UndoBehavior::Skip, ConflictPolicy::Reject))
            .unwrap();
        let err = service.undo(&id, EditOrigin::Source).unwrap_err();
        assert!(matches!(err, TransactionError::Conflict { .. }), "{err:?}");
        assert_eq!(text(&service, &id), "ONE two tHREe");
    }

    #[test]
    fn read_only_documents_reject_edits() {
        let (service, id) = service_with("locked");
        service.set_read_only(&id, Some("undecodable bytes".into())).unwrap();
        let err = service
            .apply(tx(&id, 1, vec![TextPatch::insert(0, "x")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap_err();
        assert!(matches!(err, TransactionError::ReadOnly { .. }));
    }

    #[test]
    fn opening_the_same_path_returns_the_existing_document() {
        let (service, id) = service_with("text");
        let again = service.open(OpenDocument {
            path: Some(WorkspacePath::parse("main.typ").unwrap()),
            text: "different".into(),
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        });
        assert_eq!(again.id, id);
        assert_eq!(text(&service, &id), "text");
    }

    #[test]
    fn detects_line_endings() {
        assert_eq!(detect_line_ending("a\nb"), LineEnding::Lf);
        assert_eq!(detect_line_ending("a\r\nb\r\n"), LineEnding::Crlf);
        assert_eq!(detect_line_ending("a\r\nb\n"), LineEnding::Mixed);
        assert_eq!(detect_line_ending("ab"), LineEnding::None);
        assert_eq!(detect_line_ending("a\rb"), LineEnding::Mixed, "a lone CR cannot be mirrored exactly");
        assert_eq!(detect_line_ending("\r\n\r\n"), LineEnding::Crlf);
        assert_eq!(detect_line_ending("\n\r\n"), LineEnding::Mixed);
        assert_eq!(detect_line_ending("é\r\nü\r\n"), LineEnding::Crlf);
    }

    fn with_origin(mut request: TransactionRequest, origin: EditOrigin) -> TransactionRequest {
        request.origin = origin;
        request
    }

    fn line_ending(service: &DocumentService, id: &DocumentId) -> LineEnding {
        service.info(id).unwrap().line_ending
    }

    #[test]
    fn mixed_line_endings_refuse_edits_from_every_origin_until_normalized() {
        // UTF-16 offsets: a0 \r1 \n2 b3 \n4 c5 \r6 \n7
        let mixed = "a\r\nb\nc\r\n";
        let (service, id) = service_with(mixed);
        assert_eq!(line_ending(&service, &id), LineEnding::Mixed);
        let insert =
            |base| tx(&id, base, vec![TextPatch::insert(0, "x")], UndoBehavior::NewGroup, ConflictPolicy::Reject);
        for origin in [
            EditOrigin::Source,
            EditOrigin::Page,
            EditOrigin::Agent { task_id: "t1".into() },
            EditOrigin::Research { run_id: "r1".into() },
            EditOrigin::System,
        ] {
            let err = service.apply(with_origin(insert(1), origin.clone())).unwrap_err();
            assert!(matches!(err, TransactionError::MixedLineEndings { .. }), "{origin:?}: {err:?}");
        }
        assert_eq!(text(&service, &id), mixed, "nothing changed");
        assert_eq!(service.info(&id).unwrap().revision, 1);

        // Normalizing (document_normalize_line_endings sends exactly this) is accepted
        // and announced as an info change.
        let mut rx = service.subscribe();
        let normalize = tx(
            &id,
            1,
            vec![TextPatch::replace(1, 3, "\n"), TextPatch::replace(6, 8, "\n")],
            UndoBehavior::NewGroup,
            ConflictPolicy::Reject,
        );
        service.apply(with_origin(normalize, EditOrigin::System)).unwrap();
        assert_eq!(text(&service, &id), "a\nb\nc\n");
        assert_eq!(line_ending(&service, &id), LineEnding::Lf);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            matches!(events.as_slice(), [DocumentEvent::Changed(_), DocumentEvent::InfoChanged { info }] if info.line_ending == LineEnding::Lf),
            "{events:?}"
        );

        // Editable again, from any origin.
        service.apply(with_origin(insert(2), EditOrigin::Agent { task_id: "t1".into() })).unwrap();
        assert_eq!(text(&service, &id), "xa\nb\nc\n");

        // Undo back past the normalization: mixed (read-only) again; redo normalizes.
        service.undo(&id, EditOrigin::Source).unwrap();
        service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), mixed);
        assert_eq!(line_ending(&service, &id), LineEnding::Mixed);
        let err = service.apply(with_origin(insert(5), EditOrigin::Page)).unwrap_err();
        assert!(matches!(err, TransactionError::MixedLineEndings { .. }));
        service.redo(&id, EditOrigin::Page).unwrap();
        assert_eq!(line_ending(&service, &id), LineEnding::Lf);
        service.redo(&id, EditOrigin::Page).unwrap();
        assert_eq!(text(&service, &id), "xa\nb\nc\n");
    }

    #[test]
    fn mixed_documents_accept_content_restores_but_never_undo_into_mixed_text() {
        let (service, id) = service_with("a\r\nb\n");
        // A reload from disk and a recovery restore replace content even when
        // the result still mixes line endings.
        let reload = tx(&id, 1, vec![TextPatch::insert(0, "disk ")], UndoBehavior::NewGroup, ConflictPolicy::Reject);
        service.apply(with_origin(reload, EditOrigin::External)).unwrap();
        let restore =
            tx(&id, 2, vec![TextPatch::insert(0, "journal ")], UndoBehavior::NewGroup, ConflictPolicy::Reject);
        service.apply(with_origin(restore, EditOrigin::Recovery)).unwrap();
        assert_eq!(text(&service, &id), "journal disk a\r\nb\n");
        // Undoing either would edit mixed text into other mixed text: refused,
        // and the history is left intact.
        let err = service.undo(&id, EditOrigin::Source).unwrap_err();
        assert!(matches!(err, TransactionError::MixedLineEndings { .. }), "{err:?}");
        assert!(service.info(&id).unwrap().can_undo);
        assert_eq!(text(&service, &id), "journal disk a\r\nb\n");

        // A document that became mixed through an edit can be undone back.
        let (service, id) = service_with("a\nb\n");
        service
            .apply(tx(&id, 1, vec![TextPatch::insert(1, "\r\n")], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap();
        assert_eq!(line_ending(&service, &id), LineEnding::Mixed);
        service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "a\nb\n");
        assert_eq!(line_ending(&service, &id), LineEnding::Lf);
    }

    #[test]
    fn map_range_boundaries() {
        let ins = |at: u32, s: &str| TextPatch::insert(at, s);
        // insertion at start of a range shifts it
        assert_eq!(map_range(TextRange::new(5, 8), &[ins(5, "ab")]), Some(TextRange::new(7, 10)));
        // insertion at end of a non-empty range does not
        assert_eq!(map_range(TextRange::new(5, 8), &[ins(8, "ab")]), Some(TextRange::new(5, 8)));
        // insertion strictly inside conflicts
        assert_eq!(map_range(TextRange::new(5, 8), &[ins(6, "ab")]), None);
        // replacement ending where the range starts shifts it
        assert_eq!(map_range(TextRange::new(5, 8), &[TextPatch::replace(2, 5, "")]), Some(TextRange::new(2, 5)));
        // an insertion point at the start of a concurrent replacement stays before it
        assert_eq!(map_range(TextRange::empty(5), &[TextPatch::replace(5, 7, "xyz")]), Some(TextRange::empty(5)));
    }

    fn type_chars(service: &DocumentService, id: &DocumentId, at: u32, chars: &str, key: &str) {
        for (i, ch) in (0u32..).zip(chars.chars()) {
            let base = service.info(id).unwrap().revision;
            service
                .apply(tx(
                    id,
                    base,
                    vec![TextPatch::insert(at + i, ch.to_string())],
                    UndoBehavior::Merge { key: key.into() },
                    ConflictPolicy::Reject,
                ))
                .unwrap();
        }
    }

    /// Review R-m1 #3: fixing a letter inside a just-typed word, then undoing
    /// twice, must undo both groups (the earlier group must not be "rebased"
    /// over the later group's edit and its undo).
    #[test]
    fn undo_after_undoing_an_edit_inside_an_earlier_group() {
        let (service, id) = service_with("");
        type_chars(&service, &id, 0, "helo", "typing-1");
        type_chars(&service, &id, 3, "l", "typing-2");
        assert_eq!(text(&service, &id), "hello");
        service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "helo");
        service.undo(&id, EditOrigin::Source).expect("second undo removes the word");
        assert_eq!(text(&service, &id), "");
        service.redo(&id, EditOrigin::Source).unwrap();
        service.redo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "hello");
    }

    /// Review R-m1 #7: undo must reach the first group of a long session, far
    /// beyond the rebase history window.
    #[test]
    fn undo_reaches_the_start_of_a_long_session() {
        let (service, id) = service_with("");
        let mut offset = 0u32;
        for line in 0..120 {
            let chars = format!("line {line:03} abcdefghijk\n");
            type_chars(&service, &id, offset, &chars, &format!("typing-{line}"));
            offset += chars.chars().count() as u32;
        }
        assert!(service.info(&id).unwrap().revision > HISTORY_LIMIT as u64);
        for _ in 0..120 {
            service.undo(&id, EditOrigin::Source).unwrap();
        }
        assert_eq!(text(&service, &id), "");
    }

    /// An unrecorded change below several groups is transformed down the whole
    /// stack, so every group still undoes exactly its own edit.
    #[test]
    fn unrecorded_changes_are_transformed_down_the_undo_stack() {
        let (service, id) = service_with("abc");
        type_chars(&service, &id, 3, "XY", "typing-1");
        type_chars(&service, &id, 5, "Z", "typing-2");
        assert_eq!(text(&service, &id), "abcXYZ");
        // An unrecorded change at the start (e.g. an external reload of a region).
        service
            .apply(tx(&id, 4, vec![TextPatch::insert(0, ">>")], UndoBehavior::Skip, ConflictPolicy::Reject))
            .unwrap();
        service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), ">>abcXY");
        service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), ">>abc");
    }

    /// Two insertions at the same point: the unrecorded one and the restored
    /// text keep a consistent order through the stack transform.
    #[test]
    fn unrecorded_insertion_at_an_undo_point_keeps_a_consistent_order() {
        let (service, id) = service_with("ab");
        // Group deletes "b" (its inverse re-inserts "b" at 1).
        service
            .apply(tx(&id, 1, vec![TextPatch::delete(1, 2)], UndoBehavior::NewGroup, ConflictPolicy::Reject))
            .unwrap();
        service.apply(tx(&id, 2, vec![TextPatch::insert(1, "!")], UndoBehavior::Skip, ConflictPolicy::Reject)).unwrap();
        service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(text(&service, &id), "a!b");
    }

    /// Review R-m1 #6: a transaction with thousands of patches is linear in the
    /// text size, not quadratic, while holding the service lock.
    #[test]
    fn many_patch_transactions_are_linear() {
        let (service, id) = service_with(&"abcdefghij\n".repeat(100_000));
        let patches: Vec<TextPatch> = (0..20_000u32).map(|i| TextPatch::insert(i * 55, "x")).collect();
        let started = std::time::Instant::now();
        work::reset();
        service.apply(tx(&id, 1, patches, UndoBehavior::NewGroup, ConflictPolicy::Reject)).unwrap();
        service.undo(&id, EditOrigin::Source).unwrap();
        let elapsed = started.elapsed();
        // Bytes of text touched, the same on every machine: a pass, or a
        // block, per patch would be far more than a few times the text.
        let (copied, scanned) = work::totals();
        assert!(copied <= 5_500_000, "copied {copied} bytes of a 1.1 MB text");
        assert!(scanned <= 5_500_000, "scanned {scanned} bytes of a 1.1 MB text");
        assert_eq!(text(&service, &id), "abcdefghij\n".repeat(100_000));
        assert!(elapsed < std::time::Duration::from_secs(2), "20k-patch apply + undo on 1.1 MB took {elapsed:?}");
    }

    #[test]
    fn cursor_rejects_surrogate_splits_and_backwards_seeks() {
        let mut cursor = Utf16Cursor::new("a😀b");
        assert_eq!(cursor.seek(1), Some(1));
        assert_eq!(cursor.seek(2), None);
        let mut cursor = Utf16Cursor::new("a😀b");
        assert_eq!(cursor.seek(3), Some(5));
        assert_eq!(cursor.seek(1), None);
        assert_eq!(cursor.seek(4), Some(6));
        assert_eq!(cursor.seek(5), None);
    }

    // ----- perf WS-2: the shortcuts agree with the plain computations -------------

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % bound.max(1)
        }

        fn text(&mut self, alphabet: &[&str], max_len: usize) -> String {
            (0..self.next(max_len + 1)).map(|_| alphabet[self.next(alphabet.len())]).collect()
        }

        /// Up to three sorted, non-overlapping patches at character boundaries of `text`.
        fn patches(&mut self, text: &str, alphabet: &[&str]) -> Vec<TextPatch> {
            let mut boundaries = vec![0u32];
            let mut unit = 0u32;
            for ch in text.chars() {
                unit += ch.len_utf16() as u32;
                boundaries.push(unit);
            }
            let mut picks: Vec<u32> =
                (0..2 * (1 + self.next(3))).map(|_| boundaries[self.next(boundaries.len())]).collect();
            picks.sort_unstable();
            let mut patches: Vec<TextPatch> = Vec::new();
            for pair in picks.chunks(2) {
                // Two insertions at one offset are not a valid patch list.
                if patches.last().is_some_and(|p| p.range.end == pair[0] && (p.range.is_empty() || pair[0] == pair[1]))
                {
                    continue;
                }
                let text = self.text(alphabet, 3);
                if pair[0] == pair[1] && text.is_empty() {
                    continue;
                }
                patches.push(TextPatch { range: TextRange::new(pair[0], pair[1]), text });
            }
            patches
        }
    }

    #[test]
    fn block_seek_agrees_with_decoding_every_character() {
        let mut rng = Lcg(7);
        let alphabet = ["a", "b", "é", "∀", "😀", "\n"];
        for round in 0..40 {
            let text: String =
                (0..SEEK_BLOCK * 3 / 2 + rng.next(SEEK_BLOCK * 2)).map(|_| alphabet[rng.next(6)]).collect();
            let total = utf16_len(&text) as u32;
            let mut targets: Vec<u32> = (0..12).map(|_| rng.next(total as usize + 3) as u32).collect();
            targets.extend([0, total, total + 1]);
            targets.sort_unstable();
            let mut cursor = Utf16Cursor::new(&text);
            for target in targets {
                let expected = exposition_core::text::utf16_to_byte(&text, target as usize).ok();
                let mut fresh = Utf16Cursor::new(&text);
                assert_eq!(fresh.seek(target), expected, "round {round} target {target} (fresh cursor)");
                if let Some(byte) = expected {
                    assert_eq!(cursor.seek(target), Some(byte), "round {round} target {target} (continued cursor)");
                }
            }
        }
    }

    #[test]
    fn incremental_line_ending_agrees_with_a_scan() {
        let mut rng = Lcg(11);
        let alphabet = ["a", "\r", "\n", "\r\n", "é", "b"];
        let (mut decided, mut rounds) = (0, 0);
        for _ in 0..20_000 {
            let text = rng.text(&alphabet, 10);
            let patches = rng.patches(&text, &alphabet);
            if patches.is_empty() {
                continue;
            }
            let spans = locate(&text, &patches).unwrap();
            let result = splice(&text, &spans, &patches);
            rounds += 1;
            if let Some(ending) = line_ending_after(detect_line_ending(&text), &text, &spans, &patches) {
                decided += 1;
                assert_eq!(ending, detect_line_ending(&result), "{text:?} + {patches:?} = {result:?}");
            }
        }
        assert!(decided * 50 > rounds, "the shortcut decides some of these edits ({decided} of {rounds})");
        // Plain typing never needs a scan.
        let text = "line one\r\nline two\r\n";
        let typed = [TextPatch::insert(4, "x")];
        let spans = locate(text, &typed).unwrap();
        assert_eq!(line_ending_after(LineEnding::Crlf, text, &spans, &typed), Some(LineEnding::Crlf));
        // Typing between the two bytes of a CRLF needs one.
        let split = [TextPatch::insert(9, "x")];
        let spans = locate(text, &split).unwrap();
        assert_eq!(line_ending_after(LineEnding::Crlf, text, &spans, &split), None);
    }

    #[test]
    fn merged_inverse_agrees_with_rebuilding_the_start_text() {
        let mut rng = Lcg(13);
        let alphabets: [&[&str]; 3] = [&["a"], &["a", "b"], &["a", "b", "é", "😀", "\n", "∀"]];
        for round in 0..30_000 {
            let alphabet = alphabets[round % 3];
            let before = rng.text(alphabet, 12);
            let group = rng.patches(&before, alphabet);
            let applied = rng.patches(&before, alphabet);
            if applied.is_empty() {
                continue;
            }
            let spans = locate(&before, &applied).unwrap();
            let after = splice(&before, &spans, &applied);
            let expected = merged_inverse_by_rebuilding(&before, &after, &group);
            let fast = merged_inverse(&before, &after, &group, &applied, &spans);
            assert_eq!(fast, Some(expected), "before {before:?} group {group:?} applied {applied:?}");
        }
        // Far from the start of a text larger than one seek block.
        let before = format!("{}{}", "é😀 lemma\n".repeat(2_000), "tail");
        let at = utf16_len(&before) as u32 - 4;
        let group = [TextPatch::delete(at - 6, at)];
        for applied in [[TextPatch::insert(at, "t")], [TextPatch::insert(at, "x")], [TextPatch::delete(at - 1, at)]] {
            let spans = locate(&before, &applied).unwrap();
            let after = splice(&before, &spans, &applied);
            assert_eq!(
                merged_inverse(&before, &after, &group, &applied, &spans),
                Some(merged_inverse_by_rebuilding(&before, &after, &group)),
            );
        }
    }

    /// Typing and deleting in one merged group, at random places: undo
    /// returns to the text at the start of the group and redo to its end.
    #[test]
    fn merged_groups_undo_to_their_start_text() {
        let mut rng = Lcg(17);
        for round in 0..200 {
            let start = rng.text(&["a", "b", "é", "😀", "\n", "\r\n"], 30);
            if detect_line_ending(&start) == LineEnding::Mixed {
                continue;
            }
            let (service, id) = service_with(&start);
            let mut revision = 1;
            let mut edits = 0;
            for _ in 0..1 + rng.next(8) {
                let current = text(&service, &id);
                let patches = rng.patches(&current, &["x", "y", "é", "😀"]);
                if patches.is_empty() {
                    continue;
                }
                let request =
                    tx(&id, revision, patches, UndoBehavior::Merge { key: "typing".into() }, ConflictPolicy::Reject);
                if service.apply(request).is_ok() {
                    revision += 1;
                    edits += 1;
                }
            }
            if edits == 0 {
                continue;
            }
            let end = text(&service, &id);
            assert_eq!(service.retained(&id).unwrap().undo_groups, 1, "round {round}");
            service.undo(&id, EditOrigin::Source).unwrap();
            assert_eq!(text(&service, &id), start, "round {round}: undo of the merged group");
            service.redo(&id, EditOrigin::Source).unwrap();
            assert_eq!(text(&service, &id), end, "round {round}: redo of the merged group");
        }
    }

    /// Perf WS-2 regression: one typed character costs two copies of the text
    /// (the new buffer and its shared form) and at most one scan up to the
    /// edit. It does not rebuild the undo group's start text, compare the
    /// texts from the start, or scan for line endings.
    #[test]
    fn a_typed_character_does_a_bounded_amount_of_work() {
        for body in ["plain ascii line of text\n", "ünïcödé 😀 ∀x ∈ ℝ line\r\n"] {
            let content = body.repeat(1_000_000 / body.len());
            let size = content.len() as u64;
            let (service, id) = service_with(&content);
            let end = utf16_len(&content) as u32;
            let typing = || UndoBehavior::Merge { key: "typing".into() };
            for (i, at) in [end, end + 1, end + 2].into_iter().enumerate() {
                service
                    .apply(tx(&id, 1 + i as u64, vec![TextPatch::insert(at, "x")], typing(), ConflictPolicy::Reject))
                    .unwrap();
            }
            for (name, patch) in
                [("insert", TextPatch::insert(end + 3, "y")), ("backspace", TextPatch::delete(end + 3, end + 4))]
            {
                let revision = service.info(&id).unwrap().revision;
                work::reset();
                service.apply(tx(&id, revision, vec![patch], typing(), ConflictPolicy::Reject)).unwrap();
                let (copied, scanned) = work::totals();
                assert!(copied <= 2 * size + 4096, "{name}: copied {copied} bytes of a {size}-byte text");
                assert!(scanned <= size + 4096, "{name}: scanned {scanned} bytes of a {size}-byte text");
            }
            assert_eq!(service.retained(&id).unwrap().undo_groups, 1);
            work::reset();
            service.undo(&id, EditOrigin::Source).unwrap();
            let (copied, scanned) = work::totals();
            assert!(copied <= 2 * size + 4096 && scanned <= size + 4096, "undo: copied {copied}, scanned {scanned}");
            assert_eq!(text(&service, &id), content);
        }
    }
}
