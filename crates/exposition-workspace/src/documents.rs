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
        Ok(DocumentState { info: doc.info(), text: doc.text.to_string() })
    }

    pub fn snapshot(&self, id: &DocumentId) -> Result<DocumentSnapshot, TransactionError> {
        let inner = self.lock();
        Ok(get(&inner, id)?.snapshot())
    }

    /// Snapshots of every open document, taken atomically (for build snapshots).
    pub fn snapshots(&self) -> Vec<DocumentSnapshot> {
        self.lock().documents.values().map(DocState::snapshot).collect()
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

    /// Change the workspace path of a document (rename/move/save-as).
    pub fn set_path(&self, id: &DocumentId, path: Option<WorkspacePath>) -> Result<DocumentInfo, TransactionError> {
        let mut inner = self.lock();
        let doc = get_mut(&mut inner, id)?;
        doc.path = path;
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
            let inverse = inverse_patches(&doc.text, &plan.patches);
            let line_ending_changed = doc.commit(plan.text, plan.patches.clone());
            doc.redo.clear();
            record_undo(doc, &request, inverse, &plan.patches, previous_revision, &plan.before_text);
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
        let text = apply_patches(&doc.text, &patches).map_err(|range| invalid_range(doc, range))?;
        if doc.line_ending == LineEnding::Mixed && detect_line_ending(&text) == LineEnding::Mixed {
            return Err(TransactionError::MixedLineEndings { document: id.clone() });
        }
        let group = if kind == ChangeKind::Undo { doc.undo.pop() } else { doc.redo.pop() }.expect("checked above");
        let previous_revision = doc.revision;
        let inverse = inverse_patches(&doc.text, &patches);
        let line_ending_changed = doc.commit(text, patches.clone());
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

    /// Record a new revision. Returns true when the text's line-ending
    /// convention changed (the caller announces the new info).
    fn commit(&mut self, text: String, patches: Vec<TextPatch>) -> bool {
        self.revision += 1;
        self.text = Arc::from(text);
        self.history.push_back(HistoryEntry { revision: self.revision, patches });
        while self.history.len() > HISTORY_LIMIT {
            self.history.pop_front();
        }
        let line_ending = detect_line_ending(&self.text);
        let changed = line_ending != self.line_ending;
        self.line_ending = line_ending;
        changed
    }
}

struct PlannedEdit {
    document: DocumentId,
    patches: Vec<TextPatch>,
    text: String,
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
    let text = apply_patches(&doc.text, &patches).map_err(|range| invalid_range(doc, range))?;
    // Mixed line endings: only a change that makes them consistent, or one
    // that restores content from disk or a journal, is accepted.
    let restores = matches!(origin, EditOrigin::External | EditOrigin::Recovery);
    if doc.line_ending == LineEnding::Mixed && !restores && detect_line_ending(&text) == LineEnding::Mixed {
        return Err(TransactionError::MixedLineEndings { document: doc.id.clone() });
    }
    Ok(PlannedEdit { document: doc.id.clone(), patches, text, rebased, before_text: Arc::clone(&doc.text) })
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

impl<'a> Utf16Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, byte: 0, unit: 0 }
    }

    /// Byte offset of UTF-16 offset `target`; `None` if it lies before the
    /// previous target, inside a surrogate pair, or past the end.
    fn seek(&mut self, target: u32) -> Option<usize> {
        let target = target as usize;
        if target < self.unit {
            return None;
        }
        let text = self.text;
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

/// Apply sorted, non-overlapping patches to `text` in one pass. Returns the
/// offending range on failure.
fn apply_patches(text: &str, patches: &[TextPatch]) -> Result<String, TextRange> {
    let mut out = String::with_capacity(text.len() + patches.iter().map(|p| p.text.len()).sum::<usize>());
    let mut cursor = Utf16Cursor::new(text);
    let mut copied = 0usize;
    for patch in patches {
        if !patch.range.is_ordered() {
            return Err(patch.range);
        }
        let start = cursor.seek(patch.range.start).ok_or(patch.range)?;
        let end = cursor.seek(patch.range.end).ok_or(patch.range)?;
        out.push_str(&text[copied..start]);
        out.push_str(&patch.text);
        copied = end;
    }
    out.push_str(&text[copied..]);
    Ok(out)
}

/// Patches that undo `patches` (sorted, relative to `old`), relative to the new text.
fn inverse_patches(old: &str, patches: &[TextPatch]) -> Vec<TextPatch> {
    let mut cursor = Utf16Cursor::new(old);
    let mut shift: i64 = 0;
    patches
        .iter()
        .map(|patch| {
            let from = cursor.seek(patch.range.start).expect("validated patch");
            let to = cursor.seek(patch.range.end).expect("validated patch");
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
        // Recompute the merged group's inverse: restore the group's start text
        // from the text before this edit, then diff against the new text.
        let start_text = apply_patches(before_text, &top.inverse).expect("undo group matches its end revision");
        top.inverse = diff_patch(&doc.text, &start_text).into_iter().collect();
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
    let start = utf16_len(&from[..prefix]) as u32;
    let end = start + utf16_len(&from_rest[..from_rest.len() - suffix]) as u32;
    Some(TextPatch { range: TextRange::new(start, end), text: to_rest[..to_rest.len() - suffix].to_owned() })
}

/// The text's line-ending convention, in one pass (run on every revision).
fn detect_line_ending(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
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
        service.apply(tx(&id, 1, patches, UndoBehavior::NewGroup, ConflictPolicy::Reject)).unwrap();
        service.undo(&id, EditOrigin::Source).unwrap();
        let elapsed = started.elapsed();
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
}
