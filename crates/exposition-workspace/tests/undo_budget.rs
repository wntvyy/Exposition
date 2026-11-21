//! Byte budgets of the undo/redo stacks and the rebase history of a document.
//!
//! - The measured scenario with the shipped budgets: a 2 MiB document
//!   rewritten in full again and again keeps what its histories hold within
//!   the budgets, and the newest steps undo to the exact texts.
//! - Scripted cases with small budgets: a step larger than the whole budget,
//!   a stale edit against a dropped and against a kept revision, undo back to
//!   the saved text, several documents in one transaction.
//! - Random sequences with small budgets against two independent oracles: a
//!   model that keeps full-text snapshots and applies the same trimming rule
//!   (texts, what can be undone and redone, the saved state, what is held),
//!   and a second service without byte budgets (the result of every edit,
//!   rebased ones included).

use std::collections::VecDeque;

use exposition_core::document::{
    ChangeKind, ConflictPolicy, DocumentChanged, DocumentEdit, DocumentEvent, DocumentId, EditOrigin, SelectionHint,
    TextEncoding, TextPatch, TransactionError, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::text::TextRange;
use exposition_workspace::diff::diff_patches;
use exposition_workspace::documents::{
    DocumentService, HISTORY_BYTE_BUDGET, HistoryLimits, OpenDocument, RetainedHistory, UNDO_BYTE_BUDGET, held,
};
use exposition_workspace::encoding::MAX_TEXT_BYTES;
use tokio::sync::broadcast::Receiver;

const MIB: usize = 1024 * 1024;

// ----- helpers ----------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0.max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() >> 11) as usize % n }
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Text of about `bytes` bytes without carriage returns (documents stay
/// editable), with characters of every UTF-8 width.
fn random_text(rng: &mut Rng, bytes: usize) -> String {
    const PIECES: &[&str] = &["a", "b", "the ", " ", "é", "中", "😀", "\n", "x", "lemma "];
    let mut out = String::new();
    while out.len() < bytes {
        out.push_str(rng.pick(PIECES));
    }
    out
}

/// Deterministic ASCII text of exactly `bytes` bytes; different for each seed
/// from the first byte to the last.
fn filler(bytes: usize, seed: u64) -> String {
    let line = format!("{seed:04} lemma proof theorem hence sum integral since {seed:04}\n");
    let mut out = String::with_capacity(bytes + line.len());
    while out.len() < bytes {
        out.push_str(&line);
    }
    out.truncate(bytes);
    out
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

fn boundaries(text: &str) -> Vec<u32> {
    let mut at = 0u32;
    let mut out = vec![0];
    for ch in text.chars() {
        at += ch.len_utf16() as u32;
        out.push(at);
    }
    out
}

/// Apply valid, sorted patches the obvious way: the new text and the text
/// each patch replaced.
fn naive_apply(text: &str, patches: &[TextPatch]) -> (String, Vec<String>) {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out: Vec<u16> = Vec::new();
    let mut replaced = Vec::new();
    let mut copied = 0usize;
    for patch in patches {
        let (start, end) = (patch.range.start as usize, patch.range.end as usize);
        assert!(copied <= start && start <= end && end <= units.len(), "patch {:?} does not fit", patch.range);
        out.extend_from_slice(&units[copied..start]);
        out.extend(patch.text.encode_utf16());
        replaced.push(String::from_utf16(&units[start..end]).expect("patches lie on character boundaries"));
        copied = end;
    }
    out.extend_from_slice(&units[copied..]);
    (String::from_utf16(&out).unwrap(), replaced)
}

/// The parts of `a` and `b` between their common prefix and the common
/// suffix of what follows it (whole characters).
fn differing_middles<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    let prefix: usize = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(c, _)| c.len_utf8()).sum();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let suffix: usize =
        a.chars().rev().zip(b.chars().rev()).take_while(|(x, y)| x == y).map(|(c, _)| c.len_utf8()).sum();
    (&a[..a.len() - suffix], &b[..b.len() - suffix])
}

fn open(service: &DocumentService, text: &str) -> DocumentId {
    service
        .open(OpenDocument {
            path: None,
            text: text.to_owned(),
            language: "plaintext".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        })
        .id
}

fn request(id: &DocumentId, base: u64, patches: Vec<TextPatch>) -> TransactionRequest {
    TransactionRequest {
        id: TransactionId(format!("budget-{base}")),
        origin: EditOrigin::Source,
        edits: vec![DocumentEdit { document: id.clone(), base_revision: base, patches }],
        undo: UndoBehavior::NewGroup,
        conflict: ConflictPolicy::Reject,
        label: None,
        selection: None,
    }
}

fn text(service: &DocumentService, id: &DocumentId) -> String {
    service.shared_state(id).unwrap().1.to_string()
}

fn revision(service: &DocumentService, id: &DocumentId) -> u64 {
    service.info(id).unwrap().revision
}

/// Replace the whole text in one undo step.
fn rewrite(service: &DocumentService, id: &DocumentId, next: &str) {
    let (info, current) = service.shared_state(id).unwrap();
    let patch = TextPatch::replace(0, utf16_len(&current), next);
    service.apply(request(id, info.revision, vec![patch])).unwrap();
}

/// The running totals equal a recount of what is held.
fn assert_counted(retained: &RetainedHistory, context: &str) {
    assert_eq!(retained.undo_redo_charged, retained.undo_redo_held, "{context}: undo/redo running total");
    assert_eq!(retained.history_charged, retained.history_bytes, "{context}: history running total");
}

fn limits(undo_bytes: usize, history_bytes: usize) -> HistoryLimits {
    HistoryLimits { undo_bytes, history_bytes, ..HistoryLimits::default() }
}

// ----- the shipped budgets ------------------------------------------------------------

#[test]
fn the_budgets_are_a_few_times_the_largest_document() {
    assert_eq!(UNDO_BYTE_BUDGET as u64, 4 * MAX_TEXT_BYTES + MIB as u64);
    assert_eq!(HISTORY_BYTE_BUDGET, 4 * MIB);
    let shipped = HistoryLimits::default();
    assert_eq!((shipped.undo_groups, shipped.history_entries), (1000, 2048), "the count limits stay");
    assert_eq!((shipped.undo_bytes, shipped.history_bytes), (UNDO_BYTE_BUDGET, HISTORY_BYTE_BUDGET));
}

/// The measured scenario: fifty full rewrites of a 2 MiB document. Bytes are
/// asserted, not time.
#[test]
fn fifty_rewrites_of_a_two_mebibyte_document_stay_within_the_budgets() {
    const REWRITES: u64 = 50;
    const STEPS: u64 = 20;
    let size = 2 * MIB;
    let service = DocumentService::new();
    let id = open(&service, &filler(size, 0));
    for i in 1..=REWRITES {
        rewrite(&service, &id, &filler(size, i));
        let retained = service.retained(&id).unwrap();
        assert_counted(&retained, &format!("rewrite {i}"));
        assert!(
            retained.undo_redo_held <= UNDO_BYTE_BUDGET,
            "rewrite {i}: undo holds {} bytes, budget {UNDO_BYTE_BUDGET}",
            retained.undo_redo_held
        );
        assert!(
            retained.history_bytes <= HISTORY_BYTE_BUDGET,
            "rewrite {i}: history holds {} bytes",
            retained.history_bytes
        );
    }
    let retained = service.retained(&id).unwrap();
    // A step holds the 2 MiB it replaced and its records: 20 fit in 41 MiB.
    assert_eq!(retained.undo_groups, STEPS as usize, "{retained:?}");
    assert!(retained.undo_bytes > UNDO_BYTE_BUDGET - 2 * size, "the budget is used, not undercut: {retained:?}");
    // The rebase history keeps every revision (where each patch was and how
    // long its text is, not the text).
    assert_eq!(retained.history_entries, REWRITES as usize);
    assert!(retained.history_bytes < 16 * 1024, "{retained:?}");
    assert!(service.info(&id).unwrap().can_undo);

    // The newest steps undo to the exact texts, newest first; then nothing is left.
    for step in 1..=STEPS {
        let event = service.undo(&id, EditOrigin::Source).unwrap();
        assert_eq!(event.can_undo, step < STEPS, "undo {step}");
        assert!(event.can_redo);
        assert!(
            text(&service, &id) == filler(size, REWRITES - step),
            "undo {step} restores rewrite {}",
            REWRITES - step
        );
        let retained = service.retained(&id).unwrap();
        assert_counted(&retained, &format!("undo {step}"));
        assert!(retained.undo_redo_held <= UNDO_BYTE_BUDGET, "undo {step}: {retained:?}");
    }
    let info = service.info(&id).unwrap();
    assert!(!info.can_undo && info.can_redo);
    let refused = service.undo(&id, EditOrigin::Source).unwrap_err();
    assert!(matches!(refused, TransactionError::NothingToUndo { .. }), "{refused:?}");
    // And redo returns to the newest text.
    for step in 1..=STEPS {
        service.redo(&id, EditOrigin::Source).unwrap();
        assert!(text(&service, &id) == filler(size, REWRITES - STEPS + step), "redo {step}");
    }
    assert!(!service.info(&id).unwrap().can_redo);
    let retained = service.retained(&id).unwrap();
    assert_counted(&retained, "after redo");
    assert!(retained.undo_redo_held <= UNDO_BYTE_BUDGET && retained.history_bytes <= HISTORY_BYTE_BUDGET);
}

/// Replacing all of a document of the largest size that can be opened is
/// far below the budget: four such steps stay undoable.
#[test]
fn full_replacements_of_the_largest_document_stay_undoable() {
    let size = MAX_TEXT_BYTES as usize;
    let service = DocumentService::new();
    let id = open(&service, &filler(size, 0));
    for i in 1..=5 {
        rewrite(&service, &id, &filler(size, i));
    }
    let retained = service.retained(&id).unwrap();
    assert_counted(&retained, "five replacements");
    assert_eq!(retained.undo_groups, 4, "four 10 MiB steps and their records fit: {retained:?}");
    assert!(retained.undo_redo_held <= UNDO_BYTE_BUDGET);
    for i in (1..=4).rev() {
        service.undo(&id, EditOrigin::Source).unwrap();
        assert!(text(&service, &id) == filler(size, i), "undo to replacement {i}");
    }
    assert!(!service.info(&id).unwrap().can_undo);
}

// ----- scripted cases with small budgets ------------------------------------------------

#[test]
fn a_step_larger_than_the_whole_budget_is_undoable_once() {
    let service = DocumentService::with_limits(limits(4096, 4096));
    let original = filler(64 * 1024, 1);
    let id = open(&service, &original);
    let mut events = service.subscribe();
    // A small step first: it is the oldest and is dropped for the large one.
    service.apply(request(&id, 1, vec![TextPatch::insert(0, "small ")])).unwrap();
    let before = text(&service, &id);
    let replacement = filler(48 * 1024, 2);
    rewrite(&service, &id, &replacement);
    let retained = service.retained(&id).unwrap();
    assert_counted(&retained, "large step");
    assert_eq!(retained.undo_groups, 1, "only the newest step is kept: {retained:?}");
    assert!(retained.undo_redo_held > 4096, "it alone exceeds the budget: {retained:?}");
    let last = last_change(&mut events).unwrap();
    assert!(last.can_undo && !last.can_redo);

    // Undo once: the text before the large step, exactly. The redo step is
    // then the newest one and is kept although it exceeds the budget too.
    let undone = service.undo(&id, EditOrigin::Source).unwrap();
    assert_eq!(text(&service, &id), before);
    assert!(!undone.can_undo && undone.can_redo, "the small step below it was dropped");
    assert!(matches!(service.undo(&id, EditOrigin::Source), Err(TransactionError::NothingToUndo { .. })));
    assert!(!service.info(&id).unwrap().can_undo);
    let redone = service.redo(&id, EditOrigin::Source).unwrap();
    assert_eq!(text(&service, &id), replacement);
    assert!(redone.can_undo && !redone.can_redo);
    assert_counted(&service.retained(&id).unwrap(), "after redo");

    // A following small step drops the oversized one: the budget holds again.
    let base = revision(&service, &id);
    service.apply(request(&id, base, vec![TextPatch::insert(0, "next ")])).unwrap();
    let retained = service.retained(&id).unwrap();
    assert_eq!(retained.undo_groups, 1);
    assert!(retained.undo_redo_held <= 4096, "{retained:?}");
    service.undo(&id, EditOrigin::Source).unwrap();
    assert_eq!(text(&service, &id), replacement);
}

/// Typing merged into one step that grows past the budget stays one whole
/// step: it is never cut, and undo returns to the text before the typing.
#[test]
fn a_merged_step_that_outgrows_the_budget_stays_whole() {
    let service = DocumentService::with_limits(limits(600, 1 << 20));
    let original = "x".repeat(2000);
    let id = open(&service, &original);
    service.apply(request(&id, 1, vec![TextPatch::insert(0, "first ")])).unwrap();
    // Deleting backwards, one character at a time, in one typing group.
    for i in 0..1500u32 {
        let end = 2006 - i;
        let mut delete = request(&id, 2 + u64::from(i), vec![TextPatch::delete(end - 1, end)]);
        delete.undo = UndoBehavior::Merge { key: "typing".into() };
        service.apply(delete).unwrap();
        assert_counted(&service.retained(&id).unwrap(), "merged delete");
    }
    let retained = service.retained(&id).unwrap();
    assert_eq!((retained.undo_groups, retained.undo_bytes), (1, 1500), "{retained:?}");
    service.undo(&id, EditOrigin::Source).unwrap();
    assert_eq!(text(&service, &id), format!("first {original}"));
    assert!(!service.info(&id).unwrap().can_undo, "the step before the typing was dropped");
}

fn stale(id: &DocumentId, base: u64, patches: Vec<TextPatch>) -> TransactionRequest {
    TransactionRequest { conflict: ConflictPolicy::RebaseDisjoint, ..request(id, base, patches) }
}

/// A revision of many patches, each replacing one "a" by "b" or back.
fn flip_all(service: &DocumentService, id: &DocumentId, count: u32, to: &str) {
    let patches: Vec<TextPatch> = (0..count).map(|i| TextPatch::replace(2 * i, 2 * i + 1, to)).collect();
    let mut flip = request(id, revision(service, id), patches);
    flip.undo = UndoBehavior::Skip;
    service.apply(flip).unwrap();
}

#[test]
fn a_stale_edit_against_a_dropped_revision_is_refused_like_one_beyond_the_revision_window() {
    let kind = |error: &TransactionError| serde_json::to_value(error).unwrap()["kind"].clone();
    // What an edit older than the 2,048-revision window gets.
    let by_count = DocumentService::new();
    let counted = open(&by_count, "tail");
    for base in 1..=2049u64 {
        let mut edit = request(&counted, base, vec![TextPatch::insert(0, "z")]);
        edit.undo = UndoBehavior::Skip;
        by_count.apply(edit).unwrap();
    }
    assert_eq!(by_count.retained(&counted).unwrap().history_entries, 2048);
    let by_count_error = by_count.apply(stale(&counted, 1, vec![TextPatch::insert(4, "!")])).unwrap_err();
    assert_eq!(by_count_error, TransactionError::HistoryUnavailable { document: counted.clone(), base_revision: 1 });
    assert_eq!(kind(&by_count_error), "historyUnavailable");
    assert!(by_count.apply(stale(&counted, 2, vec![TextPatch::insert(4, "!")])).is_ok(), "revision 2 is in the window");

    // The same refusal when the byte budget dropped the revision: 400 patches
    // per revision, a budget that holds two such revisions but not three.
    let per_revision = held::REVISION + 400 * held::REVISION_PATCH;
    let unbounded = HistoryLimits { undo_bytes: usize::MAX, history_bytes: usize::MAX, ..HistoryLimits::default() };
    let bounded = HistoryLimits { history_bytes: 2 * per_revision + per_revision / 2, ..unbounded };
    let start = format!("{}tail", "a ".repeat(400));
    let flipped = |limits: HistoryLimits| {
        let service = DocumentService::with_limits(limits);
        let id = open(&service, &start);
        for round in 0..4 {
            flip_all(&service, &id, 400, if round % 2 == 0 { "b" } else { "a" });
        }
        (service, id)
    };
    // `old` takes every accepted edit too; `probe` shows what the refused ones would have done.
    let ((new, id), (old, _), (probe, _)) = (flipped(bounded), flipped(unbounded), flipped(unbounded));
    let mut events = new.subscribe();
    // Revisions 2..=5 exist; the budget keeps 4 and 5, so bases 3, 4, 5 can be rebased.
    let retained = new.retained(&id).unwrap();
    assert_counted(&retained, "bounded history");
    assert_eq!((retained.history_entries, retained.history_bytes), (2, 2 * per_revision));
    assert_eq!(old.retained(&id).unwrap().history_entries, 4);
    assert_eq!(
        new.origins_since(&id, 2).unwrap(),
        None,
        "who changed the text since a dropped revision cannot be told"
    );
    assert_eq!(new.origins_since(&id, 3).unwrap(), Some(vec![EditOrigin::Source, EditOrigin::Source]));

    let tail_end = 800 + 4;
    let before = text(&new, &id);
    for base in [1, 2] {
        let refused = new.apply(stale(&id, base, vec![TextPatch::insert(tail_end, "!")])).unwrap_err();
        assert_eq!(refused, TransactionError::HistoryUnavailable { document: id.clone(), base_revision: base });
        assert_eq!(kind(&refused), kind(&by_count_error));
        assert!(text(&new, &id) == before, "a refused edit changes nothing");
        assert_eq!(revision(&new, &id), 5);
        assert!(events.try_recv().is_err(), "and announces nothing");
        // Without the byte budget the same edit is rebased: the refusal comes
        // from the dropped revision, not from the edit.
        let rebased = probe.apply(stale(&id, base, vec![TextPatch::insert(tail_end, "!")])).unwrap();
        assert!(rebased.applied[0].rebased);
        probe.undo(&id, EditOrigin::Source).unwrap();
    }
    // Against kept revisions: the same result as without the budget, for
    // edits that rebase and for edits that conflict.
    let (mut applied, mut conflicts) = (0, 0);
    for base in [3, 4, 5] {
        for patches in [vec![TextPatch::insert(tail_end, "!")], vec![TextPatch::replace(0, 3, "c")]] {
            let a = new.apply(stale(&id, base, patches.clone()));
            let b = old.apply(stale(&id, base, patches));
            assert_eq!(a, b, "base {base}");
            match a {
                Ok(outcome) => applied += usize::from(outcome.applied[0].rebased),
                Err(TransactionError::Conflict { .. }) => conflicts += 1,
                Err(other) => panic!("base {base}: {other:?}"),
            }
            assert!(text(&new, &id) == text(&old, &id), "base {base}");
            assert_eq!(revision(&new, &id), revision(&old, &id));
        }
    }
    assert!(applied >= 3 && conflicts >= 2, "{applied} rebased and applied, {conflicts} conflicts");
}

/// Whether a document is unsaved is decided by revisions alone (an undo is a
/// new revision), so dropping steps cannot change it.
#[test]
fn the_saved_state_does_not_depend_on_dropped_steps() {
    let dirty = |service: &DocumentService, id: &DocumentId| {
        let info = service.info(id).unwrap();
        info.saved_revision != Some(info.revision)
    };
    for budget in [usize::MAX, 300] {
        let service = DocumentService::with_limits(limits(budget, usize::MAX));
        let saved_text = "saved text\n".to_owned();
        let id = open(&service, &saved_text);
        assert!(!dirty(&service, &id));
        // One step away from the saved text, then steps that push it out of the budget.
        service.apply(request(&id, 1, vec![TextPatch::insert(0, "edit ")])).unwrap();
        for i in 0..4u64 {
            let base = revision(&service, &id);
            service.apply(request(&id, base, vec![TextPatch::insert(0, filler(120, i))])).unwrap();
        }
        assert!(dirty(&service, &id));
        let mut undone = 0;
        while service.undo(&id, EditOrigin::Source).is_ok() {
            undone += 1;
            assert!(dirty(&service, &id), "budget {budget}: an undo is a new revision, never the saved one");
            assert_eq!(service.info(&id).unwrap().saved_revision, Some(1));
        }
        if budget == usize::MAX {
            assert_eq!((undone, text(&service, &id)), (5, saved_text.clone()));
        } else {
            assert!(undone < 5, "the step that led back to the saved text was dropped");
            assert_ne!(text(&service, &id), saved_text);
        }
        // Saving what is there now makes exactly that revision the saved one.
        let now = revision(&service, &id);
        service.mark_saved(&id, now).unwrap();
        assert!(!dirty(&service, &id));
        if service.redo(&id, EditOrigin::Source).is_ok() {
            assert!(dirty(&service, &id));
        }
    }
}

/// One transaction over two documents records one step in each document's own
/// history (undo is per document). Each history is trimmed by its own budget;
/// a step is kept or dropped whole, and the newest one of each document stays.
#[test]
fn a_transaction_over_two_documents_trims_each_history_on_its_own() {
    let service = DocumentService::with_limits(limits(1000, usize::MAX));
    let (a, b) = (open(&service, &filler(4000, 1)), open(&service, &"b".repeat(10)));
    // Fill both histories.
    for i in 0..3u64 {
        for id in [&a, &b] {
            let base = revision(&service, id);
            service.apply(request(id, base, vec![TextPatch::insert(0, filler(40, i))])).unwrap();
        }
    }
    let (a_before, b_before) = (text(&service, &a), text(&service, &b));
    let both = TransactionRequest {
        edits: vec![
            // 3000 bytes replaced: more than the budget of document a.
            DocumentEdit { document: a.clone(), base_revision: 4, patches: vec![TextPatch::replace(0, 3000, "A")] },
            DocumentEdit { document: b.clone(), base_revision: 4, patches: vec![TextPatch::insert(0, "B")] },
        ],
        ..request(&a, 4, Vec::new())
    };
    service.apply(both).unwrap();
    let (ra, rb) = (service.retained(&a).unwrap(), service.retained(&b).unwrap());
    assert_counted(&ra, "a");
    assert_counted(&rb, "b");
    assert_eq!(ra.undo_groups, 1, "document a keeps the step of the transaction alone");
    assert_eq!(rb.undo_groups, 4, "document b is far below its budget and loses nothing");
    // Both halves of the transaction undo, each to the exact text before it.
    service.undo(&a, EditOrigin::Source).unwrap();
    service.undo(&b, EditOrigin::Source).unwrap();
    assert_eq!((text(&service, &a), text(&service, &b)), (a_before, b_before));
    assert!(!service.info(&a).unwrap().can_undo && service.info(&b).unwrap().can_undo);
}

// ----- random sequences against a model -------------------------------------------------

struct Step {
    key: Option<String>,
    origin: std::mem::Discriminant<EditOrigin>,
    start: String,
    end: String,
    end_revision: u64,
    patches: usize,
    /// Bytes of the texts that undoing / redoing this step puts back.
    undo_text: usize,
    redo_text: usize,
    selection_before: usize,
    selection_after: usize,
    /// Bytes of the identifier of the origin (the merge key is `key`).
    origin_bytes: usize,
}

/// Bytes of the task or run identifier an origin carries.
fn origin_bytes(origin: &EditOrigin) -> usize {
    match origin {
        EditOrigin::Agent { task_id } => task_id.len(),
        EditOrigin::Research { run_id } => run_id.len(),
        _ => 0,
    }
}

impl Step {
    fn held(&self, undo: bool) -> usize {
        held::UNDO_STEP
            + self.key.as_ref().map_or(0, String::len)
            + self.origin_bytes
            + self.patches * held::UNDO_PATCH
            + if undo { self.undo_text } else { self.redo_text }
            + (self.selection_before + self.selection_after) * held::SELECTION_RANGE
    }
}

/// Full-text snapshots per step and the trimming rule, nothing else.
struct Model {
    limits: HistoryLimits,
    /// The text of every revision; index 0 is revision 1.
    texts: Vec<String>,
    saved: Option<u64>,
    undo: VecDeque<Step>,
    redo: VecDeque<Step>,
    /// Revision and bytes held of each revision the rebase history keeps.
    history: VecDeque<(u64, usize)>,
    /// Steps dropped for the byte budget so far.
    dropped: usize,
}

impl Model {
    fn new(limits: HistoryLimits, text: String) -> Self {
        Self {
            limits,
            texts: vec![text],
            saved: Some(1),
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            history: VecDeque::new(),
            dropped: 0,
        }
    }

    fn revision(&self) -> u64 {
        self.texts.len() as u64
    }

    fn text(&self) -> &str {
        self.texts.last().unwrap()
    }

    fn held_undo_redo(&self) -> usize {
        self.undo.iter().map(|s| s.held(true)).sum::<usize>() + self.redo.iter().map(|s| s.held(false)).sum::<usize>()
    }

    fn held_history(&self) -> usize {
        self.history.iter().map(|(_, held)| held).sum()
    }

    /// The trimming rule: over budget, the oldest undo steps go first, then
    /// the redo steps furthest from the current text; the step made last
    /// (on top of the undo stack after an edit or a redo, on top of the redo
    /// stack after an undo) always stays.
    fn trim(&mut self, newest_is_undo: bool) {
        while self.held_undo_redo() > self.limits.undo_bytes {
            if self.undo.len() > usize::from(newest_is_undo) {
                self.undo.pop_front();
            } else if self.redo.len() > usize::from(!newest_is_undo) {
                self.redo.pop_front();
            } else {
                break;
            }
            self.dropped += 1;
        }
    }

    /// A new revision of `patches` patches: the oldest revisions leave the
    /// rebase history over its count or its budget; the newest stays.
    fn revise(&mut self, text: String, patches: usize, origin: &EditOrigin) -> u64 {
        self.texts.push(text);
        let held = held::REVISION + patches * held::REVISION_PATCH + origin_bytes(origin);
        self.history.push_back((self.revision(), held));
        while self.history.len() > self.limits.history_entries
            || (self.held_history() > self.limits.history_bytes && self.history.len() > 1)
        {
            self.history.pop_front();
        }
        self.revision()
    }

    /// The oldest revision an edit may be based on.
    fn oldest_base(&self) -> u64 {
        self.history.front().map_or(self.revision(), |(revision, _)| revision - 1)
    }

    /// A recorded edit whose applied patches are `patches`; `selection` is
    /// the number of ranges before and after.
    fn edit(&mut self, patches: &[TextPatch], behavior: &UndoBehavior, origin: &EditOrigin, selection: (usize, usize)) {
        let start = self.text().to_owned();
        let (next, replaced) = naive_apply(&start, patches);
        let previous = self.revision();
        let revision = self.revise(next.clone(), patches.len(), origin);
        let origin_bytes = origin_bytes(origin);
        let key = match behavior {
            UndoBehavior::Merge { key } => Some(key.clone()),
            _ => None,
        };
        let origin = std::mem::discriminant(origin);
        self.redo.clear();
        match self.undo.back_mut() {
            Some(top) if key.is_some() && top.key == key && top.origin == origin && top.end_revision == previous => {
                // A merged step restores its start text by the one patch
                // between the differing middles (none when the texts are equal).
                let (start_middle, end_middle) = differing_middles(&top.start, &next);
                top.patches = usize::from(top.start != next);
                (top.undo_text, top.redo_text) = (start_middle.len(), end_middle.len());
                top.end = next;
                top.end_revision = revision;
                // The selection before stays that of the first edit.
                top.selection_after = selection.1;
            }
            _ => {
                self.undo.push_back(Step {
                    key,
                    origin,
                    start,
                    end: next,
                    end_revision: revision,
                    patches: patches.len(),
                    undo_text: replaced.iter().map(String::len).sum(),
                    redo_text: patches.iter().map(|p| p.text.len()).sum(),
                    selection_before: selection.0,
                    selection_after: selection.1,
                    origin_bytes,
                });
                if self.undo.len() > self.limits.undo_groups {
                    self.undo.pop_front();
                }
            }
        }
        self.trim(true);
    }

    /// Undo or redo one step; false when there is none.
    fn step(&mut self, undo: bool) -> bool {
        let Some(mut step) = (if undo { self.undo.pop_back() } else { self.redo.pop_back() }) else {
            return false;
        };
        let text = if undo { step.start.clone() } else { step.end.clone() };
        step.key = None;
        // Stepped by the source editor.
        step.end_revision = self.revise(text, step.patches, &EditOrigin::Source);
        if undo {
            self.redo.push_back(step);
        } else {
            self.undo.push_back(step);
        }
        self.trim(!undo);
        true
    }
}

fn last_change(events: &mut Receiver<DocumentEvent>) -> Option<DocumentChanged> {
    let mut last = None;
    while let Ok(event) = events.try_recv() {
        if let DocumentEvent::Changed(changed) = event {
            last = Some(changed);
        }
    }
    last
}

/// The service under test, the model, and a service without byte budgets
/// that takes every accepted operation too.
struct Trial {
    seed: u64,
    service: DocumentService,
    unbounded: DocumentService,
    id: DocumentId,
    events: Receiver<DocumentEvent>,
    model: Model,
    log: Vec<String>,
    counter: u64,
    /// What the run reached, so the test can tell it exercised the bounds.
    oversized: usize,
    refused_stale: usize,
    rebased: usize,
}

impl Trial {
    fn fail(&self, what: &str) -> String {
        let tail: Vec<&String> = self.log.iter().rev().take(10).rev().collect();
        format!("seed {} after {} operations: {what}\nlast operations:\n{tail:#?}", self.seed, self.log.len())
    }

    fn request(
        &mut self,
        base: u64,
        patches: Vec<TextPatch>,
        undo: UndoBehavior,
        origin: EditOrigin,
        selection: Option<SelectionHint>,
    ) -> TransactionRequest {
        self.counter += 1;
        TransactionRequest {
            id: TransactionId(format!("trial-{}", self.counter)),
            origin,
            edits: vec![DocumentEdit { document: self.id.clone(), base_revision: base, patches }],
            undo,
            conflict: ConflictPolicy::RebaseDisjoint,
            label: None,
            selection,
        }
    }

    /// An edit based on the current revision, or on an older one the model
    /// says the rebase history still reaches.
    fn apply(&mut self, request: TransactionRequest) {
        self.log.push(format!(
            "apply base {} {:?} {:?} {} patches",
            request.edits[0].base_revision,
            request.undo,
            request.origin,
            request.edits[0].patches.len()
        ));
        let result = self.service.apply(request.clone());
        let expected = self.unbounded.apply(request.clone());
        assert_eq!(result, expected, "{}", self.fail("the result differs from the service without byte budgets"));
        let changed = last_change(&mut self.events);
        match result {
            Ok(outcome) => {
                let selection = request.selection.as_ref().map_or((0, 0), |s| (s.before.len(), s.after.len()));
                self.rebased += usize::from(outcome.applied[0].rebased);
                self.model.edit(&outcome.applied[0].patches, &request.undo, &request.origin, selection);
                let changed = changed.unwrap_or_else(|| panic!("{}", self.fail("an accepted edit announced nothing")));
                assert!(changed.can_undo, "{}", self.fail("the step just made can be undone"));
                assert!(!changed.can_redo, "{}", self.fail("an edit clears redo"));
            }
            Err(_) => assert!(changed.is_none(), "{}", self.fail("a refused edit announced a change")),
        }
        self.check();
    }

    /// An edit based on a revision the model says was dropped.
    fn apply_beyond_history(&mut self, request: TransactionRequest) {
        let base = request.edits[0].base_revision;
        self.log.push(format!("apply base {base} (dropped; oldest base {})", self.model.oldest_base()));
        let result = self.service.apply(request);
        assert_eq!(
            result,
            Err(TransactionError::HistoryUnavailable { document: self.id.clone(), base_revision: base }),
            "{}",
            self.fail("an edit based on a dropped revision must be refused")
        );
        assert!(last_change(&mut self.events).is_none(), "{}", self.fail("a refused edit announced a change"));
        self.refused_stale += 1;
        self.check();
    }

    fn step(&mut self, undo: bool) {
        self.log.push(if undo { "undo".into() } else { "redo".into() });
        let available = self.model.step(undo);
        let result = if undo {
            self.service.undo(&self.id, EditOrigin::Source)
        } else {
            self.service.redo(&self.id, EditOrigin::Source)
        };
        // The step is announced exactly as it is returned; a refusal announces nothing.
        let announced = last_change(&mut self.events);
        assert_eq!(announced.as_ref(), result.as_ref().ok(), "{}", self.fail("the announced step"));
        if !available {
            assert!(
                matches!(result, Err(TransactionError::NothingToUndo { .. })),
                "{}",
                self.fail(&format!("the model has nothing to undo/redo: {result:?}"))
            );
            self.check();
            return;
        }
        let event = result.unwrap_or_else(|e| panic!("{}", self.fail(&format!("undo/redo refused: {e:?}"))));
        // The same step exists in the service without byte budgets.
        let other = if undo {
            self.unbounded.undo(&self.id, EditOrigin::Source)
        } else {
            self.unbounded.redo(&self.id, EditOrigin::Source)
        }
        .unwrap_or_else(|e| panic!("{}", self.fail(&format!("the service without byte budgets refused: {e:?}"))));
        assert_eq!(
            (&event.patches, &event.selection, event.revision, event.change),
            (&other.patches, &other.selection, other.revision, other.change),
            "{}",
            self.fail("the step differs from the service without byte budgets")
        );
        assert_eq!(event.change, if undo { ChangeKind::Undo } else { ChangeKind::Redo });
        assert_eq!(
            (event.can_undo, event.can_redo),
            (!self.model.undo.is_empty(), !self.model.redo.is_empty()),
            "{}",
            self.fail("what the event says can be undone and redone")
        );
        self.check();
    }

    fn save(&mut self) {
        let revision = self.model.revision();
        self.log.push(format!("saved at {revision}"));
        self.service.mark_saved(&self.id, revision).unwrap();
        self.unbounded.mark_saved(&self.id, revision).unwrap();
        self.model.saved = Some(revision);
        self.check();
    }

    /// Everything observable equals the model.
    fn check(&mut self) {
        let model = &self.model;
        let (info, shared) = self.service.shared_state(&self.id).unwrap();
        assert!(&*shared == model.text(), "{}", self.fail("text"));
        assert!(
            &*self.unbounded.shared_state(&self.id).unwrap().1 == model.text(),
            "{}",
            self.fail("text without budgets")
        );
        assert_eq!(info.revision, model.revision(), "{}", self.fail("revision"));
        assert_eq!(
            (info.can_undo, info.can_redo),
            (!model.undo.is_empty(), !model.redo.is_empty()),
            "{}",
            self.fail("what can be undone and redone")
        );
        assert_eq!(info.saved_revision, model.saved, "{}", self.fail("saved revision"));
        assert_eq!(
            info.saved_revision != Some(info.revision),
            model.saved != Some(model.revision()),
            "{}",
            self.fail("unsaved")
        );
        let retained = self.service.retained(&self.id).unwrap();
        let expected = RetainedHistory {
            undo_groups: model.undo.len(),
            undo_bytes: model.undo.iter().map(|s| s.undo_text).sum(),
            redo_groups: model.redo.len(),
            redo_bytes: model.redo.iter().map(|s| s.redo_text).sum(),
            history_entries: model.history.len(),
            history_bytes: model.held_history(),
            undo_redo_held: model.held_undo_redo(),
            undo_redo_charged: model.held_undo_redo(),
            history_charged: model.held_history(),
        };
        assert_eq!(retained, expected, "{}", self.fail("what the histories hold"));
        // Within the budgets, except for the one newest step or revision.
        let oversized = retained.undo_redo_held > model.limits.undo_bytes;
        if oversized {
            assert_eq!(retained.undo_groups + retained.redo_groups, 1, "{}", self.fail("over the undo budget"));
        }
        assert!(
            retained.undo_groups + retained.redo_groups <= model.limits.undo_groups,
            "{}",
            self.fail("group count")
        );
        if retained.history_bytes > model.limits.history_bytes {
            assert_eq!(retained.history_entries, 1, "{}", self.fail("over the history budget"));
        }
        assert!(retained.history_entries <= model.limits.history_entries, "{}", self.fail("revision count"));
        self.oversized += usize::from(oversized);
    }
}

/// Up to `count` sorted, non-overlapping, non-touching patches on `text`.
fn random_patches(rng: &mut Rng, text: &str, count: usize, piece: usize) -> Vec<TextPatch> {
    let bounds = boundaries(text);
    let mut picks: Vec<u32> = (0..2 * count).map(|_| *rng.pick(&bounds)).collect();
    picks.sort_unstable();
    let mut patches: Vec<TextPatch> = Vec::new();
    for pair in picks.chunks(2) {
        if patches.last().is_some_and(|p| p.range.end >= pair[0]) {
            continue;
        }
        // Keep most ranges short so the text is not emptied at once.
        let end = if rng.chance(80) {
            let index = bounds.binary_search(&pair[0]).unwrap();
            bounds[(index + rng.below(4)).min(bounds.len() - 1)].min(pair[1])
        } else {
            pair[1]
        };
        let length = rng.below(piece + 1);
        let inserted = random_text(rng, length);
        if end == pair[0] && inserted.is_empty() {
            continue;
        }
        patches.push(TextPatch { range: TextRange::new(pair[0], end), text: inserted });
    }
    patches
}

fn random_selection(rng: &mut Rng) -> Option<SelectionHint> {
    rng.chance(40).then(|| SelectionHint {
        before: (0..rng.below(4)).map(|i| TextRange::empty(i as u32)).collect(),
        after: (0..rng.below(4)).map(|i| TextRange::empty(i as u32)).collect(),
    })
}

fn run_trial(seed: u64, steps: usize) -> Trial {
    let mut rng = Rng::new(seed);
    let limits = HistoryLimits {
        undo_groups: *rng.pick(&[5, 1000, 1000]),
        undo_bytes: *rng.pick(&[600, 1500, 4000, 20_000]),
        history_entries: *rng.pick(&[4, 2048, 2048]),
        history_bytes: *rng.pick(&[3, 6, 12]) * held::REVISION + 40,
    };
    let unbounded_limits = HistoryLimits {
        undo_groups: usize::MAX,
        undo_bytes: usize::MAX,
        history_entries: usize::MAX,
        history_bytes: usize::MAX,
    };
    let length = rng.below(300);
    let initial = random_text(&mut rng, length);
    let service = DocumentService::with_limits(limits);
    let unbounded = DocumentService::with_limits(unbounded_limits);
    let id = open(&service, &initial);
    assert_eq!(open(&unbounded, &initial), id);
    let events = service.subscribe();
    let mut trial = Trial {
        seed,
        service,
        unbounded,
        id,
        events,
        model: Model::new(limits, initial),
        log: Vec::new(),
        counter: 0,
        oversized: 0,
        refused_stale: 0,
        rebased: 0,
    };
    trial.check();
    let mut cursor = 0u32;
    for _ in 0..steps {
        let text = trial.model.text().to_owned();
        let revision = trial.model.revision();
        let bounds = boundaries(&text);
        if bounds.binary_search(&cursor).is_err() || rng.chance(8) {
            cursor = *rng.pick(&bounds);
        }
        match rng.below(100) {
            // Typing and backspace at the cursor, merged into one step.
            0..=29 => {
                let index = bounds.binary_search(&cursor).unwrap();
                let patch = if index > 0 && rng.chance(30) {
                    TextPatch::delete(bounds[index - 1], cursor)
                } else {
                    TextPatch::insert(cursor, *rng.pick(&["a", "é", "😀", " ", "\n", "中"]))
                };
                cursor = patch.range.start + utf16_len(&patch.text);
                let selection = random_selection(&mut rng);
                let key = if rng.chance(90) { "typing" } else { "other" };
                let request = trial.request(
                    revision,
                    vec![patch],
                    UndoBehavior::Merge { key: key.into() },
                    EditOrigin::Source,
                    selection,
                );
                trial.apply(request);
            }
            // A few small patches as one step, from any recorded origin.
            30..=47 => {
                let count = 1 + rng.below(3);
                let patches = random_patches(&mut rng, &text, count, 12);
                if patches.is_empty() {
                    continue;
                }
                let origin = match rng.below(4) {
                    0 => EditOrigin::Page,
                    1 => EditOrigin::Agent { task_id: "t".into() },
                    _ => EditOrigin::Source,
                };
                let behavior =
                    if rng.chance(25) { UndoBehavior::Merge { key: "typing".into() } } else { UndoBehavior::NewGroup };
                let selection = random_selection(&mut rng);
                let request = trial.request(revision, patches, behavior, origin, selection);
                trial.apply(request);
            }
            // A huge step: everything replaced, a large paste, or most of the text deleted.
            48..=57 => {
                let end = *bounds.last().unwrap();
                let (at, length) = (*rng.pick(&bounds), rng.below(4000));
                let patch = match rng.below(4) {
                    _ if text.len() > 8000 => TextPatch::replace(0, end, random_text(&mut rng, 50)),
                    0 => TextPatch::replace(0, end, random_text(&mut rng, 200 + length)),
                    1 => TextPatch::insert(at, random_text(&mut rng, 500 + length)),
                    2 => TextPatch::delete(bounds[bounds.len() / 8], bounds[bounds.len() - 1 - bounds.len() / 8]),
                    _ => TextPatch::replace(0, end, ""),
                };
                if patch.range.is_empty() && patch.text.is_empty() {
                    continue;
                }
                let origin =
                    if rng.chance(50) { EditOrigin::Agent { task_id: "t".into() } } else { EditOrigin::Source };
                let request = trial.request(revision, vec![patch], UndoBehavior::NewGroup, origin, None);
                trial.apply(request);
            }
            // The file changed on disk: reloaded as one step, then the buffer equals the file.
            58..=63 => {
                let next = if rng.chance(30) {
                    let length = rng.below(2500);
                    random_text(&mut rng, length)
                } else {
                    let edits = random_patches(&mut rng, &text, 2, 30);
                    naive_apply(&text, &edits).0
                };
                let patches = diff_patches(&text, &next);
                if patches.is_empty() {
                    continue;
                }
                let request = trial.request(revision, patches, UndoBehavior::NewGroup, EditOrigin::External, None);
                trial.apply(request);
                assert!(trial.model.text() == next, "{}", trial.fail("a reload produces the file's text"));
                trial.save();
            }
            // Saved.
            64..=68 => trial.save(),
            69..=84 => trial.step(true),
            85..=93 => trial.step(false),
            // An edit based on an older revision: rebased like the service
            // without byte budgets while the history reaches the base, refused
            // once it does not.
            _ => {
                let base = revision.saturating_sub(1 + rng.below(8) as u64).max(1);
                if base == revision {
                    continue;
                }
                let base_text = trial.model.texts[base as usize - 1].clone();
                let count = 1 + rng.below(2);
                let patches = random_patches(&mut rng, &base_text, count, 6);
                if patches.is_empty() {
                    continue;
                }
                let origin = EditOrigin::Agent { task_id: "t".into() };
                let request = trial.request(base, patches, UndoBehavior::NewGroup, origin, None);
                if base < trial.model.oldest_base() {
                    trial.apply_beyond_history(request);
                } else {
                    trial.apply(request);
                }
            }
        }
    }
    trial
}

#[test]
fn random_sequences_match_a_model_with_the_same_trimming_rule() {
    let (mut dropped, mut oversized, mut refused, mut rebased) = (0, 0, 0, 0);
    for seed in 0..300u64 {
        let trial = run_trial(seed, 220);
        dropped += trial.model.dropped;
        oversized += trial.oversized;
        refused += trial.refused_stale;
        rebased += trial.rebased;
    }
    // The runs reach the bounds they are about.
    assert!(dropped > 1000, "steps were dropped {dropped} times");
    assert!(oversized > 100, "a single step exceeded the budget {oversized} times");
    assert!(refused > 100, "{refused} edits based on a dropped revision");
    assert!(rebased > 100, "{rebased} edits were rebased over kept revisions");
}
