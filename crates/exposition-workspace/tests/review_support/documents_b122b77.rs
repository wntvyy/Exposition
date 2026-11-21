// Review oracle: the document service exactly as it is at the base of the change, b122b77
// (crates/exposition-workspace/src/documents.rs, lines 1-1099, unit tests left out).
// Used only by tests/review_undo_bytes*.rs to compare the byte-bounded service with it.
// Not product code; do not edit.

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

/// Revisions retained for rebasing stale edits and undo groups.
const HISTORY_LIMIT: usize = 2048;
/// Undo groups retained per document.
const UNDO_LIMIT: usize = 1000;

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

/// Text bytes retained by the histories of one document (see
/// [`DocumentService::retained`]).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetainedHistory {
    pub undo_groups: usize,
    pub undo_bytes: usize,
    pub redo_groups: usize,
    pub redo_bytes: usize,
    pub history_entries: usize,
    pub history_bytes: usize,
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
    /// Applied patches for each recent revision, relative to the revision before it.
    history: VecDeque<HistoryEntry>,
    undo: Vec<UndoGroup>,
    redo: Vec<UndoGroup>,
}

struct HistoryEntry {
    revision: u64,
    patches: Vec<TextPatch>,
    /// Who made the change (for an undo or redo: who stepped the history).
    origin: EditOrigin,
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

impl Default for DocumentService {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentService {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(4096);
        Self { inner: Mutex::new(Inner::default()), events }
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
            undo: Vec::new(),
            redo: Vec::new(),
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

    /// Text bytes held by the undo, redo, and rebase histories of a document.
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
            history_bytes: doc.history.iter().map(|e| bytes(&e.patches)).sum(),
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
        let mut applied = Vec::with_capacity(planned.len());
        for plan in planned {
            let doc = get_mut(&mut inner, &plan.document).expect("validated above");
            let previous_revision = doc.revision;
            let inverse = inverse_patches(&doc.text, &plan.spans, &plan.patches);
            let line_ending_changed =
                doc.commit(plan.text, plan.patches.clone(), request.origin.clone(), plan.line_ending);
            doc.redo.clear();
            record_undo(doc, &request, inverse, &plan.patches, &plan.spans, previous_revision, &plan.before_text);
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
        let doc = get_mut(&mut inner, id)?;
        if let Some(reason) = &doc.read_only {
            return Err(TransactionError::ReadOnly { document: id.clone(), reason: reason.clone() });
        }
        let stack = if kind == ChangeKind::Undo { &doc.undo } else { &doc.redo };
        let Some(group) = stack.last() else {
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
        let group = if kind == ChangeKind::Undo { doc.undo.pop() } else { doc.redo.pop() }.expect("checked above");
        let previous_revision = doc.revision;
        let inverse = inverse_patches(&doc.text, &spans, &patches);
        let line_ending_changed = doc.commit(text, patches.clone(), origin.clone(), line_ending);
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
        if kind == ChangeKind::Undo {
            doc.redo.push(opposite);
        } else {
            doc.undo.push(opposite);
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
    fn commit(&mut self, text: String, patches: Vec<TextPatch>, origin: EditOrigin, line_ending: LineEnding) -> bool {
        self.revision += 1;
        work::copied(text.len());
        self.text = Arc::from(text);
        self.history.push_back(HistoryEntry { revision: self.revision, patches, origin });
        while self.history.len() > HISTORY_LIMIT {
            self.history.pop_front();
        }
        let changed = line_ending != self.line_ending;
        self.line_ending = line_ending;
        changed
    }
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
        let bytes = text.as_bytes();
        while let Some(block) = bytes.get(self.byte..self.byte + SEEK_BLOCK) {
            let units = utf16_units_started(block);
            if self.unit + units >= target {
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

/// Patches that undo `patches` (sorted, relative to `old`, at byte `spans`),
/// relative to the new text.
fn inverse_patches(old: &str, spans: &[Span], patches: &[TextPatch]) -> Vec<TextPatch> {
    let mut shift: i64 = 0;
    spans
        .iter()
        .zip(patches)
        .map(|(&(from, to), patch)| {
            let start = (i64::from(patch.range.start) + shift) as u32;
            let inserted = utf16_len(&patch.text) as u32;
            shift += i64::from(inserted) - i64::from(patch.range.len());
            TextPatch { range: TextRange::new(start, start + inserted), text: old[from..to].to_owned() }
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
fn map_range(range: TextRange, concurrent: &[TextPatch]) -> Option<TextRange> {
    map_range_biased(range, concurrent, true)
}

/// [`map_range`] with an explicit tie rule: `ties_before` decides whether a
/// concurrent insertion at exactly the point of an empty `range` (another
/// insertion) lands before it. Transforming two edits against each other must
/// use opposite rules so both agree on the order.
fn map_range_biased(range: TextRange, concurrent: &[TextPatch], ties_before: bool) -> Option<TextRange> {
    let mut shift: i64 = 0;
    for q in concurrent {
        let delta = utf16_len(&q.text) as i64 - i64::from(q.range.len());
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

fn map_patches(patches: &[TextPatch], through: &[TextPatch], ties_before: bool) -> Option<Vec<TextPatch>> {
    patches
        .iter()
        .map(|p| map_range_biased(p.range, through, ties_before).map(|range| TextPatch { range, text: p.text.clone() }))
        .collect()
}

/// Re-express an undo stack after an unrecorded change `applied` (relative to
/// the text before it). Each group's inverse is relative to the text with every
/// group above it undone, so the change is transformed down the stack alongside
/// the groups. The first group it overlaps is blocked; groups below a blocked
/// group are unreachable by linear undo and are left as they are.
fn transform_stack(stack: &mut [UndoGroup], applied: &[TextPatch]) {
    let mut concurrent = applied.to_vec();
    for group in stack.iter_mut().rev() {
        if group.blocked {
            return;
        }
        match (map_patches(&group.inverse, &concurrent, true), map_patches(&concurrent, &group.inverse, false)) {
            (Some(inverse), Some(next)) => {
                group.inverse = inverse;
                concurrent = next;
            }
            _ => {
                group.blocked = true;
                return;
            }
        }
    }
}

fn record_undo(
    doc: &mut DocState,
    request: &TransactionRequest,
    inverse: Vec<TextPatch>,
    applied: &[TextPatch],
    spans: &[Span],
    previous_revision: u64,
    before_text: &Arc<str>,
) {
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
        && let Some(top) = doc.undo.last_mut()
        && top.key == key
        && !top.blocked
        && std::mem::discriminant(&top.origin) == std::mem::discriminant(&request.origin)
        && top.end_revision == previous_revision
    {
        // The merged group's inverse is the single patch from the new text
        // to the group's start text.
        top.inverse = merged_inverse(before_text, &doc.text, &top.inverse, applied, spans)
            .unwrap_or_else(|| merged_inverse_by_rebuilding(before_text, &doc.text, &top.inverse));
        top.end_revision = doc.revision;
        top.selection_after = selection_after;
        return;
    }
    doc.undo.push(UndoGroup {
        key,
        origin: request.origin.clone(),
        inverse,
        end_revision: doc.revision,
        blocked: false,
        selection_before,
        selection_after,
    });
    if doc.undo.len() > UNDO_LIMIT {
        doc.undo.remove(0);
    }
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
