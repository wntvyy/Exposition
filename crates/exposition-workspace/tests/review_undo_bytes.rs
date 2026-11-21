//! Independent review of the byte budgets of the undo and rebase histories.
//!
//! Oracles:
//! 1. The document service as it is at the base of the change (`b122b77`),
//!    verbatim in `review_support/documents_b122b77.rs`: it keeps the text of
//!    every patch in its rebase history and never trims by bytes. Below the
//!    budgets the two services must agree on everything observable.
//! 2. With tiny limits (`with_limits`): a model that keeps full snapshots of
//!    the text before and after every step and applies the stated trimming
//!    rule to sizes it computes itself from the patches the service reports.
//!    The base service stays the oracle of every step both still hold (its
//!    stacks are a superset at the bottom).

use std::collections::VecDeque;

use exposition_core::document::{
    ChangeKind, ConflictPolicy, DocumentChanged, DocumentEdit, DocumentEvent, DocumentId, EditOrigin, SelectionHint,
    TextEncoding, TextPatch, TransactionError, TransactionId, TransactionOutcome, TransactionRequest, UndoBehavior,
};
use exposition_core::text::TextRange;
use exposition_workspace::diff::diff_patches;
use exposition_workspace::documents as new;
use exposition_workspace::documents::{HistoryLimits, held};
use tokio::sync::broadcast::Receiver;

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "review_support/documents_b122b77.rs"]
mod base;

// ----- deterministic random numbers ---------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA076_1D64_78BD_642F)
    }

    fn next(&mut self) -> u64 {
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

// ----- naive text helpers (UTF-16 vectors; no code shared with the service) -------

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

/// UTF-16 offsets of every character boundary of `text`.
fn bounds(text: &str) -> Vec<u32> {
    let mut at = 0u32;
    let mut out = vec![0];
    for ch in text.chars() {
        at += ch.len_utf16() as u32;
        out.push(at);
    }
    out
}

fn naive_apply(text: &str, patches: &[TextPatch]) -> String {
    let source = units(text);
    let mut out: Vec<u16> = Vec::new();
    let mut copied = 0usize;
    for patch in patches {
        let (start, end) = (patch.range.start as usize, patch.range.end as usize);
        assert!(copied <= start && start <= end && end <= source.len(), "patch {patch:?} does not fit {text:?}");
        out.extend_from_slice(&source[copied..start]);
        out.extend(patch.text.encode_utf16());
        copied = end;
    }
    out.extend_from_slice(&source[copied..]);
    String::from_utf16(&out).expect("patches on character boundaries")
}

/// UTF-8 bytes of the text each patch replaces in `text`.
fn replaced_bytes(text: &str, patches: &[TextPatch]) -> Vec<usize> {
    let source = units(text);
    patches
        .iter()
        .map(|p| String::from_utf16(&source[p.range.start as usize..p.range.end as usize]).unwrap().len())
        .collect()
}

/// Bytes of the part of `start` that differs from `now` (common prefix, then
/// common suffix of what is left, by characters); `None` when they are equal.
fn differing_middle(now: &str, start: &str) -> Option<usize> {
    if now == start {
        return None;
    }
    let a: Vec<char> = now.chars().collect();
    let b: Vec<char> = start.chars().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (ra, rb) = (&a[prefix..], &b[prefix..]);
    let suffix = ra.iter().rev().zip(rb.iter().rev()).take_while(|(x, y)| x == y).count();
    Some(rb[..rb.len() - suffix].iter().map(|c| c.len_utf8()).sum())
}

fn drain(rx: &mut Receiver<DocumentEvent>) -> Vec<DocumentEvent> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    out
}

// ----- the service and its base side by side --------------------------------------

struct Pair {
    new: new::DocumentService,
    base: base::DocumentService,
    new_rx: Receiver<DocumentEvent>,
    base_rx: Receiver<DocumentEvent>,
    counter: u64,
}

struct Edit {
    base_revision: u64,
    patches: Vec<TextPatch>,
    undo: UndoBehavior,
    origin: EditOrigin,
    conflict: ConflictPolicy,
    selection: Option<SelectionHint>,
}

impl Edit {
    fn new(base_revision: u64, patches: Vec<TextPatch>) -> Self {
        Self {
            base_revision,
            patches,
            undo: UndoBehavior::NewGroup,
            origin: EditOrigin::Source,
            conflict: ConflictPolicy::Reject,
            selection: None,
        }
    }
}

impl Pair {
    fn new(limits: HistoryLimits) -> Self {
        let new = new::DocumentService::with_limits(limits);
        let base = base::DocumentService::new();
        let (new_rx, base_rx) = (new.subscribe(), base.subscribe());
        Self { new, base, new_rx, base_rx, counter: 0 }
    }

    /// Open the same pathless document in both services.
    fn open(&mut self, text: &str) -> DocumentId {
        let a = self.new.open(new::OpenDocument {
            path: None,
            text: text.into(),
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        });
        let b = self.base.open(base::OpenDocument {
            path: None,
            text: text.into(),
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        });
        assert_eq!(a, b);
        assert_eq!(drain(&mut self.new_rx), drain(&mut self.base_rx));
        a.id
    }

    fn close(&mut self, id: &DocumentId) {
        self.new.close(id).unwrap();
        self.base.close(id).unwrap();
        assert_eq!(drain(&mut self.new_rx), drain(&mut self.base_rx));
    }

    fn request(&mut self, id: &DocumentId, edit: Edit) -> TransactionRequest {
        self.counter += 1;
        TransactionRequest {
            id: TransactionId(format!("review-{}", self.counter)),
            origin: edit.origin,
            edits: vec![DocumentEdit {
                document: id.clone(),
                base_revision: edit.base_revision,
                patches: edit.patches,
            }],
            undo: edit.undo,
            conflict: edit.conflict,
            label: None,
            selection: edit.selection,
        }
    }

    fn text(&self, id: &DocumentId) -> String {
        self.new.state(id).unwrap().text
    }

    fn revision(&self, id: &DocumentId) -> u64 {
        self.new.info(id).unwrap().revision
    }

    /// Below the budgets everything observable is equal.
    fn assert_identical(&mut self, id: &DocumentId, context: &dyn Fn() -> String) {
        let (new_events, base_events) = (drain(&mut self.new_rx), drain(&mut self.base_rx));
        assert_eq!(new_events, base_events, "events differ: {}", context());
        let (a, b) = (self.new.state(id).unwrap(), self.base.state(id).unwrap());
        assert_eq!(a.text, b.text, "text differs: {}", context());
        assert_eq!(a.info, b.info, "info differs: {}", context());
        let (ra, rb) = (self.new.retained(id).unwrap(), self.base.retained(id).unwrap());
        assert_eq!(
            (ra.undo_groups, ra.undo_bytes, ra.redo_groups, ra.redo_bytes, ra.history_entries),
            (rb.undo_groups, rb.undo_bytes, rb.redo_groups, rb.redo_bytes, rb.history_entries),
            "retained histories differ: {}",
            context()
        );
        assert_eq!(
            (ra.undo_redo_charged, ra.history_charged),
            (ra.undo_redo_held, ra.history_bytes),
            "running totals differ from a recount: {}",
            context()
        );
        let revision = a.info.revision;
        for back in [0, 1, 2, 5, 17, 40, revision] {
            let since = revision.saturating_sub(back);
            assert_eq!(
                self.new.origins_since(id, since),
                self.base.origins_since(id, since),
                "origins_since({since}): {}",
                context()
            );
        }
    }

    /// Apply to both; the results must be equal.
    fn apply_both(
        &mut self,
        id: &DocumentId,
        edit: Edit,
        context: &dyn Fn() -> String,
    ) -> Result<TransactionOutcome, TransactionError> {
        let request = self.request(id, edit);
        let a = self.new.apply(request.clone());
        let b = self.base.apply(request);
        assert_eq!(a, b, "apply result differs: {}", context());
        a
    }

    fn step_both(
        &mut self,
        id: &DocumentId,
        kind: ChangeKind,
        context: &dyn Fn() -> String,
    ) -> Result<DocumentChanged, TransactionError> {
        let (a, b) = if kind == ChangeKind::Undo {
            (self.new.undo(id, EditOrigin::Source), self.base.undo(id, EditOrigin::Source))
        } else {
            (self.new.redo(id, EditOrigin::Source), self.base.redo(id, EditOrigin::Source))
        };
        assert_eq!(a, b, "{kind:?} result differs: {}", context());
        a
    }
}

// =================================================================================
// A. Rebasing with ranges and lengths only: differential against the base
// =================================================================================

const PIECES: &[&str] = &["a", "b", "the", " ", "é", "中", "😀", "𝒳", "\u{301}", "x", "😀😀", ""];

fn random_piece_text(rng: &mut Rng, max_pieces: usize, ending: &str) -> String {
    let mut out: String = (0..rng.below(max_pieces + 1)).map(|_| *rng.pick(PIECES)).collect();
    if !ending.is_empty() && rng.chance(12) {
        out.push_str(ending);
    }
    out
}

/// Sorted, valid patches over `text` (ranges may touch each other).
fn random_valid_patches(rng: &mut Rng, text: &str, max: usize, ending: &str) -> Vec<TextPatch> {
    let b = bounds(text);
    let count = 1 + rng.below(max);
    let mut indexes: Vec<usize> = (0..count * 2).map(|_| rng.below(b.len())).collect();
    indexes.sort_unstable();
    indexes
        .chunks(2)
        .map(|pair| {
            let (i, mut j) = (pair[0], pair[1]);
            if j > i && rng.chance(75) {
                j = j.min(i + 1 + rng.below(3));
            }
            TextPatch { range: TextRange::new(b[i], b[j]), text: random_piece_text(rng, 2, ending) }
        })
        .collect()
}

/// Sort candidate patches and keep those that do not overlap the one before.
fn sorted_disjoint(mut patches: Vec<TextPatch>) -> Vec<TextPatch> {
    patches.sort_by_key(|p| (p.range.start, p.range.end));
    let mut out: Vec<TextPatch> = Vec::new();
    for patch in patches {
        if out.last().is_none_or(|last| last.range.end <= patch.range.start) {
            out.push(patch);
        }
    }
    out
}

/// Patches against `base_text` aimed at the change `concurrent` made to it:
/// the same patches, the same ranges with other text, ranges that touch,
/// straddle, contain, or sit inside the concurrent ranges, or anything.
fn stale_patches(rng: &mut Rng, base_text: &str, concurrent: &[TextPatch], ending: &str) -> Vec<TextPatch> {
    let b = bounds(base_text);
    if concurrent.is_empty() || b.len() < 2 {
        return random_valid_patches(rng, base_text, 3, ending);
    }
    let index = |offset: u32| b.iter().position(|x| *x == offset).expect("a boundary of the base text");
    let prev = |offset: u32| b[index(offset).saturating_sub(1)];
    let next = |offset: u32| b[(index(offset) + 1).min(b.len() - 1)];
    match rng.below(8) {
        0 => concurrent.to_vec(),
        1 => concurrent.iter().map(|p| TextPatch { range: p.range, text: random_piece_text(rng, 2, ending) }).collect(),
        2..=5 => {
            let mut candidates = Vec::new();
            for _ in 0..1 + rng.below(3) {
                let q = rng.pick(concurrent).range;
                let (s, e) = (q.start, q.end);
                let range = match rng.below(12) {
                    0 => TextRange::empty(s),
                    1 => TextRange::empty(e),
                    2 => TextRange::new(prev(s), s),
                    3 => TextRange::new(e, next(e)),
                    4 => TextRange::new(s, next(s)),
                    5 => TextRange::new(prev(e), e),
                    6 => TextRange::new(prev(s), next(e)),
                    7 => TextRange::new(s, e),
                    8 => TextRange::new(prev(prev(s)), prev(s)),
                    9 => TextRange::new(next(e), next(next(e))),
                    10 => TextRange::empty(next(s)),
                    _ => TextRange::empty(prev(e)),
                };
                if range.start <= range.end {
                    candidates.push(TextPatch { range, text: random_piece_text(rng, 2, ending) });
                }
            }
            sorted_disjoint(candidates)
        }
        _ => random_valid_patches(rng, base_text, 3, ending),
    }
}

fn random_origin(rng: &mut Rng) -> EditOrigin {
    match rng.below(8) {
        0..=3 => EditOrigin::Source,
        4 => EditOrigin::Page,
        5 => EditOrigin::Agent { task_id: "t".into() },
        6 => EditOrigin::System,
        _ => EditOrigin::Research { run_id: "r".into() },
    }
}

fn random_undo(rng: &mut Rng, skip_percent: usize) -> UndoBehavior {
    if rng.chance(skip_percent) {
        return UndoBehavior::Skip;
    }
    match rng.below(3) {
        0 => UndoBehavior::Merge { key: "typing".into() },
        _ => UndoBehavior::NewGroup,
    }
}

#[derive(Default, Debug)]
struct RebaseStats {
    stale: usize,
    rebased: usize,
    conflicts: usize,
    identical_resubmitted: usize,
    undone: usize,
    blocked: usize,
    max_distance: u64,
}

/// One random history with stale edits at every distance; everything is
/// compared with the base after every operation.
fn rebase_trial(seed: u64, steps: usize, skip_percent: usize, stats: &mut RebaseStats) {
    let mut rng = Rng::new(seed);
    let ending = *rng.pick(&["", "", "", "\n", "\n", "\r\n"]);
    let mut pair = Pair::new(HistoryLimits::default());
    let initial: String = (0..6 + rng.below(20)).map(|_| *rng.pick(PIECES)).collect();
    let id = pair.open(&initial);
    // texts[r] is the text at revision r; changes[r] turned it into texts[r + 1].
    let mut texts: Vec<String> = vec![String::new(), initial];
    let mut changes: Vec<Vec<TextPatch>> = vec![Vec::new(), Vec::new()];
    let mut log: Vec<String> = Vec::new();
    for _ in 0..steps {
        let revision = pair.revision(&id);
        assert_eq!(revision as usize, texts.len() - 1);
        let context = |log: &Vec<String>| {
            let tail: Vec<&String> = log.iter().rev().take(10).rev().collect();
            format!("seed {seed}, {} operations, last:\n{tail:#?}", log.len())
        };
        let mut advanced: Option<Vec<TextPatch>> = None;
        match rng.below(100) {
            0..=41 if revision >= 2 => {
                let far = revision - 1;
                let distance = match rng.below(10) {
                    0..=4 => 1,
                    5..=7 => 2 + rng.below(3) as u64,
                    _ => 1 + rng.below(far as usize) as u64,
                }
                .min(far);
                let base_revision = revision - distance;
                let concurrent = changes[base_revision as usize].clone();
                let patches = stale_patches(&mut rng, &texts[base_revision as usize], &concurrent, ending);
                if patches.is_empty() {
                    continue;
                }
                let identical = patches == concurrent;
                let edit = Edit {
                    base_revision,
                    patches,
                    undo: random_undo(&mut rng, skip_percent),
                    origin: random_origin(&mut rng),
                    conflict: if rng.chance(93) { ConflictPolicy::RebaseDisjoint } else { ConflictPolicy::Reject },
                    selection: None,
                };
                log.push(format!(
                    "stale base {base_revision} at {revision}: {:?} {:?} {:?}",
                    edit.patches, edit.undo, edit.conflict
                ));
                stats.stale += 1;
                stats.max_distance = stats.max_distance.max(distance);
                let rebasing = edit.conflict == ConflictPolicy::RebaseDisjoint;
                match pair.apply_both(&id, edit, &|| context(&log)) {
                    Ok(outcome) => {
                        assert!(outcome.applied[0].rebased);
                        stats.rebased += 1;
                        stats.identical_resubmitted += usize::from(identical);
                        advanced = Some(outcome.applied[0].patches.clone());
                    }
                    Err(TransactionError::Conflict { .. }) => stats.conflicts += 1,
                    Err(TransactionError::StaleRevision { .. }) => assert!(!rebasing),
                    Err(TransactionError::MixedLineEndings { .. }) => {}
                    Err(other) => panic!("unexpected refusal {other:?}: {}", context(&log)),
                }
            }
            0..=79 => {
                let patches = random_valid_patches(&mut rng, &texts[revision as usize], 3, ending);
                let edit = Edit {
                    undo: random_undo(&mut rng, skip_percent),
                    origin: random_origin(&mut rng),
                    ..Edit::new(revision, patches)
                };
                log.push(format!("edit at {revision}: {:?} {:?}", edit.patches, edit.undo));
                if let Ok(outcome) = pair.apply_both(&id, edit, &|| context(&log)) {
                    advanced = Some(outcome.applied[0].patches.clone());
                }
            }
            80..=91 => {
                log.push(format!("undo at {revision}"));
                match pair.step_both(&id, ChangeKind::Undo, &|| context(&log)) {
                    Ok(event) => {
                        stats.undone += 1;
                        advanced = Some(event.patches);
                    }
                    Err(TransactionError::Conflict { .. }) => stats.blocked += 1,
                    Err(_) => {}
                }
            }
            _ => {
                log.push(format!("redo at {revision}"));
                if let Ok(event) = pair.step_both(&id, ChangeKind::Redo, &|| context(&log)) {
                    advanced = Some(event.patches);
                }
            }
        }
        pair.assert_identical(&id, &|| context(&log));
        let text = pair.text(&id);
        match advanced {
            Some(patches) => {
                // Independent of both services: the new text is the naive
                // application of the patches reported.
                assert_eq!(text, naive_apply(&texts[revision as usize], &patches), "{}", context(&log));
                changes[revision as usize] = patches;
                changes.push(Vec::new());
                texts.push(text);
            }
            None => assert_eq!(text, texts[revision as usize], "a refusal changed the text: {}", context(&log)),
        }
    }
}

#[test]
fn stale_edits_at_every_distance_rebase_exactly_like_the_base() {
    let mut stats = RebaseStats::default();
    for seed in 0..400 {
        rebase_trial(seed, 70, 15, &mut stats);
    }
    println!("{stats:?}");
    assert!(stats.rebased > 1500 && stats.conflicts > 1500, "{stats:?}");
    assert!(stats.identical_resubmitted > 50 && stats.max_distance > 40, "{stats:?}");
}

#[test]
fn stale_edits_over_long_histories_rebase_exactly_like_the_base() {
    let mut stats = RebaseStats::default();
    for seed in 1000..1012 {
        rebase_trial(seed, 600, 10, &mut stats);
    }
    println!("{stats:?}");
    assert!(stats.rebased > 300 && stats.max_distance > 300, "{stats:?}");
}

#[test]
fn the_revision_window_refuses_the_same_stale_edits_as_the_base() {
    let mut pair = Pair::new(HistoryLimits::default());
    let id = pair.open("a😀b");
    for revision in 1..=2100u64 {
        pair.apply_both(&id, Edit::new(revision, vec![TextPatch::insert(4, "x")]), &|| format!("edit {revision}"))
            .unwrap();
    }
    pair.assert_identical(&id, &|| "after 2100 edits".into());
    // Revision 2101; 2,048 revisions are kept, so bases from 53 can be rebased.
    for base_revision in [1u64, 2, 51, 52, 53, 54, 1000, 2099, 2100] {
        for patch in [TextPatch::insert(0, "😀"), TextPatch::replace(1, 3, "é"), TextPatch::insert(4, "y")] {
            let edit = Edit { conflict: ConflictPolicy::RebaseDisjoint, ..Edit::new(base_revision, vec![patch]) };
            let result = pair.apply_both(&id, edit, &|| format!("stale base {base_revision}"));
            if base_revision < 53 {
                assert!(matches!(result, Err(TransactionError::HistoryUnavailable { .. })), "{result:?}");
            }
            pair.assert_identical(&id, &|| format!("stale base {base_revision}"));
        }
    }
}

// =================================================================================
// E. Unrecorded (`Skip`) changes move the ranges of the undo stack
// =================================================================================

/// Undo until refused, then redo until refused, comparing every step.
fn unwind_and_rewind(pair: &mut Pair, id: &DocumentId, context: &dyn Fn() -> String) -> (usize, usize) {
    let (mut undone, mut blocked) = (0, 0);
    for _ in 0..8 {
        match pair.step_both(id, ChangeKind::Undo, context) {
            Ok(_) => undone += 1,
            Err(TransactionError::Conflict { .. }) => {
                blocked += 1;
                break;
            }
            Err(_) => break,
        }
        pair.assert_identical(id, context);
    }
    for _ in 0..8 {
        if pair.step_both(id, ChangeKind::Redo, context).is_err() {
            break;
        }
        pair.assert_identical(id, context);
    }
    pair.assert_identical(id, context);
    (undone, blocked)
}

/// Undo groups of every shape: one patch on an astral character, several
/// patches, a merged typing group, a deletion; optionally one of them undone.
fn build_stack(pair: &mut Pair, id: &DocumentId, variant: usize) {
    let context = || format!("building stack {variant}");
    let mut revision = 1;
    let mut edit = |pair: &mut Pair, patches: Vec<TextPatch>, undo: UndoBehavior| {
        pair.apply_both(id, Edit { undo, ..Edit::new(revision, patches) }, &context).unwrap();
        revision += 1;
    };
    // "ab😀cd𝒳ef" (10 units)
    edit(pair, vec![TextPatch::replace(2, 4, "X")], UndoBehavior::NewGroup); // "abXcd𝒳ef"
    edit(pair, vec![TextPatch::insert(0, "é"), TextPatch::replace(5, 8, "😀😀")], UndoBehavior::NewGroup);
    // "éabXc😀😀ef"
    if variant >= 1 {
        let typing = || UndoBehavior::Merge { key: "typing".into() };
        edit(pair, vec![TextPatch::insert(5, "t")], typing());
        edit(pair, vec![TextPatch::insert(6, "😀")], typing());
        edit(pair, vec![TextPatch::insert(8, "u")], typing());
    }
    if variant >= 2 {
        edit(pair, vec![TextPatch::delete(1, 3)], UndoBehavior::NewGroup);
    }
    if variant >= 3 {
        pair.step_both(id, ChangeKind::Undo, &context).unwrap();
    }
    pair.assert_identical(id, &context);
}

#[test]
fn every_unrecorded_edit_moves_the_undo_stack_like_the_base() {
    let (mut cases, mut undone, mut blocked) = (0usize, 0usize, 0usize);
    for variant in 0..4 {
        // The text the unrecorded edit applies to.
        let text = {
            let mut pair = Pair::new(HistoryLimits::default());
            let id = pair.open("ab😀cd𝒳ef");
            build_stack(&mut pair, &id, variant);
            pair.text(&id)
        };
        let b = bounds(&text);
        for i in 0..b.len() {
            for j in i..b.len() {
                for inserted in ["", "x", "😀", "é\u{301}"] {
                    if i == j && inserted.is_empty() {
                        continue;
                    }
                    for follow_up in [false, true] {
                        let mut pair = Pair::new(HistoryLimits::default());
                        let id = pair.open("ab😀cd𝒳ef");
                        build_stack(&mut pair, &id, variant);
                        let context = || {
                            format!(
                                "stack {variant}, skip {}..{} {inserted:?} on {text:?}, follow-up {follow_up}",
                                b[i], b[j]
                            )
                        };
                        let revision = pair.revision(&id);
                        let skip = Edit {
                            undo: UndoBehavior::Skip,
                            origin: EditOrigin::System,
                            ..Edit::new(revision, vec![TextPatch::replace(b[i], b[j], inserted)])
                        };
                        pair.apply_both(&id, skip, &context).unwrap();
                        pair.assert_identical(&id, &context);
                        if follow_up {
                            // A recorded edit on top of the moved stack.
                            let at = utf16_len(&pair.text(&id));
                            pair.apply_both(&id, Edit::new(revision + 1, vec![TextPatch::insert(at, "z")]), &context)
                                .unwrap();
                            pair.assert_identical(&id, &context);
                        }
                        let (u, bl) = unwind_and_rewind(&mut pair, &id, &context);
                        cases += 1;
                        undone += u;
                        blocked += bl;
                    }
                }
            }
        }
    }
    println!("{cases} unrecorded edits; {undone} undo steps applied, {blocked} refused as blocked");
    assert!(cases > 1500 && undone > 1500 && blocked > 500);
}

#[test]
fn unrecorded_edits_of_several_patches_move_the_undo_stack_like_the_base() {
    let mut stats = RebaseStats::default();
    // Mostly unrecorded changes between the recorded ones.
    for seed in 5000..5300 {
        rebase_trial(seed, 60, 55, &mut stats);
    }
    println!("{stats:?}");
    assert!(stats.undone > 800 && stats.blocked > 200, "{stats:?}");
}

// =================================================================================
// B, C. Trimming: every sequence of operations up to a length, with tiny limits
// =================================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Small,
    Huge,
    Type,
    Undo,
    Redo,
    External,
    SkipInsert,
    SkipDelete,
    Stale,
    Multi,
    Save,
}

const SMALL_TEXT: &str = "a😀bc";
const BIG_BYTES: usize = 600;

fn big_text() -> String {
    format!("😀{}", "H".repeat(BIG_BYTES))
}

/// One step of the model: the snapshots it moves between and what it holds.
struct ModelStep {
    held: usize,
    before: String,
    after: String,
    /// No unrecorded change happened under this step, so undoing and redoing
    /// it must produce exactly the snapshots.
    exact: bool,
    selection_before: usize,
    selection_after: usize,
    /// Bytes of the merge key the step was made with (none once it was
    /// undone or redone) and of the identifier of its origin.
    key_bytes: usize,
    origin_bytes: usize,
}

fn step_held(patch_bytes: &[usize], selection_before: usize, selection_after: usize, owned: usize) -> usize {
    held::UNDO_STEP
        + patch_bytes.iter().map(|bytes| held::UNDO_PATCH + bytes).sum::<usize>()
        + (selection_before + selection_after) * held::SELECTION_RANGE
        + owned
}

/// Bytes of the task or run identifier an origin carries.
fn origin_bytes(origin: &EditOrigin) -> usize {
    match origin {
        EditOrigin::Agent { task_id } => task_id.len(),
        EditOrigin::Research { run_id } => run_id.len(),
        _ => 0,
    }
}

struct Model {
    limits: HistoryLimits,
    /// `texts[r]` is the text at revision `r`.
    texts: Vec<String>,
    undo: VecDeque<ModelStep>,
    redo: VecDeque<ModelStep>,
    /// `(revision, bytes held)` of each revision of the rebase history.
    history: VecDeque<(u64, usize)>,
    /// The key of the typing group the next typed character merges into.
    mergeable: Option<String>,
    saved: Option<u64>,
    dropped_undo: usize,
    dropped_redo: usize,
    dropped_history: usize,
}

impl Model {
    fn new(limits: HistoryLimits, text: &str) -> Self {
        Self {
            limits,
            texts: vec![String::new(), text.to_owned()],
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            history: VecDeque::new(),
            mergeable: None,
            saved: Some(1),
            dropped_undo: 0,
            dropped_redo: 0,
            dropped_history: 0,
        }
    }

    fn revision(&self) -> u64 {
        self.texts.len() as u64 - 1
    }

    fn text(&self) -> &str {
        self.texts.last().unwrap()
    }

    fn held(&self) -> usize {
        self.undo.iter().chain(&self.redo).map(|s| s.held).sum()
    }

    fn history_held(&self) -> usize {
        self.history.iter().map(|(_, held)| held).sum()
    }

    /// The oldest revision an edit can still be based on.
    fn oldest_base(&self) -> u64 {
        self.history.front().map_or(self.revision(), |(revision, _)| revision - 1)
    }

    fn revise(&mut self, text: String, patches: usize, origin: &EditOrigin) {
        self.texts.push(text);
        let held = held::REVISION + patches * held::REVISION_PATCH + origin_bytes(origin);
        self.history.push_back((self.revision(), held));
        while self.history.len() > self.limits.history_entries
            || (self.history_held() > self.limits.history_bytes && self.history.len() > 1)
        {
            self.history.pop_front();
            self.dropped_history += 1;
        }
    }

    /// The stated rule: oldest undo steps first, then the redo steps furthest
    /// from the current text; the step made last stays.
    fn trim(&mut self, last_is_undo: bool) {
        while self.held() > self.limits.undo_bytes {
            if self.undo.len() > usize::from(last_is_undo) {
                self.undo.pop_front();
                self.dropped_undo += 1;
            } else if self.redo.len() > usize::from(!last_is_undo) {
                self.redo.pop_front();
                self.dropped_redo += 1;
            } else {
                break;
            }
        }
    }

    fn edit(&mut self, patches: &[TextPatch], behavior: &UndoBehavior, selection: bool, origin: &EditOrigin) {
        let before = self.text().to_owned();
        let bytes = replaced_bytes(&before, patches);
        let after = naive_apply(&before, patches);
        self.revise(after.clone(), patches.len(), origin);
        self.redo.clear();
        let ranges = usize::from(selection);
        match behavior {
            UndoBehavior::Skip => {
                for step in &mut self.undo {
                    step.exact = false;
                }
                self.mergeable = None;
                return;
            }
            UndoBehavior::Merge { key } if self.mergeable.as_ref() == Some(key) => {
                let top = self.undo.back_mut().expect("the step made last is never dropped");
                top.after = after;
                top.selection_after = ranges;
                let bytes: Vec<usize> = differing_middle(&top.after, &top.before).into_iter().collect();
                let owned = top.key_bytes + top.origin_bytes;
                top.held = step_held(&bytes, top.selection_before, top.selection_after, owned);
            }
            _ => {
                let key_bytes = match behavior {
                    UndoBehavior::Merge { key } => key.len(),
                    _ => 0,
                };
                let origin_bytes = origin_bytes(origin);
                self.undo.push_back(ModelStep {
                    held: step_held(&bytes, ranges, ranges, key_bytes + origin_bytes),
                    before,
                    after,
                    exact: true,
                    selection_before: ranges,
                    selection_after: ranges,
                    key_bytes,
                    origin_bytes,
                });
                if self.undo.len() > self.limits.undo_groups {
                    self.undo.pop_front();
                    self.dropped_undo += 1;
                }
            }
        }
        self.mergeable = match behavior {
            UndoBehavior::Merge { key } => Some(key.clone()),
            _ => None,
        };
        self.trim(true);
    }

    /// An undo or redo that applied `patches`.
    fn step(&mut self, undo: bool, patches: &[TextPatch], context: &dyn Fn() -> String) {
        let step = if undo { self.undo.pop_back() } else { self.redo.pop_back() }.expect("a step to apply");
        let bytes = replaced_bytes(self.text(), patches);
        let text = naive_apply(self.text(), patches);
        if step.exact {
            let expected = if undo { &step.before } else { &step.after };
            assert_eq!(&text, expected, "a step did not restore its snapshot: {}", context());
        }
        // Stepped by the source editor; the opposite step has no merge key.
        self.revise(text, patches.len(), &EditOrigin::Source);
        let held = step_held(&bytes, step.selection_before, step.selection_after, step.origin_bytes);
        let opposite = ModelStep { held, key_bytes: 0, ..step };
        if undo {
            self.redo.push_back(opposite)
        } else {
            self.undo.push_back(opposite)
        }
        self.mergeable = None;
        self.trim(!undo);
    }
}

#[derive(Default, Debug)]
struct Coverage {
    sequences: u64,
    operations: u64,
    dropped_undo: usize,
    dropped_redo: usize,
    dropped_history: usize,
    nothing_left_but_base_has: usize,
    history_refused: usize,
    conflicts: usize,
    blocked: usize,
    over_budget_single_step: usize,
    exact_steps: usize,
}

struct Enumeration {
    limits: HistoryLimits,
    pair: Pair,
    coverage: Coverage,
}

impl Enumeration {
    fn new(limits: HistoryLimits) -> Self {
        Self { limits, pair: Pair::new(limits), coverage: Coverage::default() }
    }

    fn build(op: Op, model: &Model) -> Edit {
        let text = model.text();
        let b = bounds(text);
        let len = *b.last().unwrap();
        let revision = model.revision();
        match op {
            Op::Small => Edit::new(revision, vec![TextPatch::insert(b[1.min(b.len() - 1)], "s")]),
            Op::Huge => {
                let next = if text.len() < BIG_BYTES / 2 { big_text() } else { SMALL_TEXT.to_owned() };
                Edit::new(revision, vec![TextPatch::replace(0, len, next)])
            }
            Op::Type => Edit {
                undo: UndoBehavior::Merge { key: "typing".into() },
                selection: Some(SelectionHint {
                    before: vec![TextRange::empty(len)],
                    after: vec![TextRange::empty(len + 1)],
                }),
                ..Edit::new(revision, vec![TextPatch::insert(len, "t")])
            },
            Op::External => {
                let target = if text.starts_with('E') { SMALL_TEXT } else { "E😀x\u{301}" };
                Edit { origin: EditOrigin::External, ..Edit::new(revision, diff_patches(text, target)) }
            }
            Op::SkipInsert => Edit {
                undo: UndoBehavior::Skip,
                origin: EditOrigin::System,
                ..Edit::new(revision, vec![TextPatch::insert(0, "K")])
            },
            Op::SkipDelete => {
                let patch =
                    if b.len() >= 2 { TextPatch::delete(b[b.len() - 2], len) } else { TextPatch::insert(0, "k") };
                Edit { undo: UndoBehavior::Skip, origin: EditOrigin::System, ..Edit::new(revision, vec![patch]) }
            }
            Op::Stale => {
                let base_revision = if revision >= 3 && revision % 2 == 1 { revision - 2 } else { revision.max(2) - 1 };
                let base_bounds = bounds(&model.texts[base_revision as usize]);
                let patch = if base_bounds.len() >= 2 {
                    TextPatch::replace(0, base_bounds[1], "R")
                } else {
                    TextPatch::insert(0, "R")
                };
                Edit {
                    origin: EditOrigin::Agent { task_id: "t".into() },
                    conflict: ConflictPolicy::RebaseDisjoint,
                    ..Edit::new(base_revision, vec![patch])
                }
            }
            Op::Multi => {
                let mut patches = vec![TextPatch::insert(0, "m")];
                if b.len() >= 3 {
                    patches.push(TextPatch::replace(b[b.len() - 2], len, "M😀"));
                }
                Edit::new(revision, patches)
            }
            Op::Undo | Op::Redo | Op::Save => unreachable!(),
        }
    }

    /// The events of one operation: the service's report the availability the
    /// model expects; apart from that they are the base's.
    fn check_events(&mut self, model: &Model, compare_base: bool, context: &dyn Fn() -> String) {
        let (can_undo, can_redo) = (!model.undo.is_empty(), !model.redo.is_empty());
        let new_events = drain(&mut self.pair.new_rx);
        for event in &new_events {
            let reported = match event {
                DocumentEvent::Changed(changed) => (changed.can_undo, changed.can_redo),
                DocumentEvent::InfoChanged { info } => (info.can_undo, info.can_redo),
                other => panic!("unexpected event {other:?}: {}", context()),
            };
            assert_eq!(reported, (can_undo, can_redo), "availability in {event:?}: {}", context());
        }
        let mut base_events = drain(&mut self.pair.base_rx);
        if !compare_base {
            assert!(new_events.is_empty() && base_events.is_empty(), "a refusal emitted events: {}", context());
            return;
        }
        for event in &mut base_events {
            match event {
                DocumentEvent::Changed(changed) => (changed.can_undo, changed.can_redo) = (can_undo, can_redo),
                DocumentEvent::InfoChanged { info } => (info.can_undo, info.can_redo) = (can_undo, can_redo),
                _ => {}
            }
        }
        assert_eq!(new_events, base_events, "events differ from the base: {}", context());
    }

    fn check_state(&mut self, id: &DocumentId, model: &Model, context: &dyn Fn() -> String) {
        let limits = self.limits;
        let state = self.pair.new.state(id).unwrap();
        let base_state = self.pair.base.state(id).unwrap();
        assert_eq!(state.text, model.text(), "text differs from the model: {}", context());
        assert_eq!(state.text, base_state.text, "text differs from the base: {}", context());
        assert_eq!(
            (state.info.revision, state.info.saved_revision),
            (model.revision(), model.saved),
            "revisions: {}",
            context()
        );
        assert_eq!(
            (state.info.revision, state.info.saved_revision, state.info.line_ending),
            (base_state.info.revision, base_state.info.saved_revision, base_state.info.line_ending),
            "info differs from the base: {}",
            context()
        );
        assert_eq!(
            (state.info.can_undo, state.info.can_redo),
            (!model.undo.is_empty(), !model.redo.is_empty()),
            "availability in info: {}",
            context()
        );
        let retained = self.pair.new.retained(id).unwrap();
        assert_eq!(
            (retained.undo_groups, retained.redo_groups, retained.undo_redo_held),
            (model.undo.len(), model.redo.len(), model.held()),
            "the stacks are not those the rule leaves: {}",
            context()
        );
        assert_eq!(
            (retained.history_entries, retained.history_bytes),
            (model.history.len(), model.history_held()),
            "the rebase history is not what the rule leaves: {}",
            context()
        );
        assert_eq!(
            (retained.undo_redo_charged, retained.history_charged),
            (retained.undo_redo_held, retained.history_bytes),
            "running totals drifted from a recount: {}",
            context()
        );
        // The bounds, stated without the model.
        let steps = retained.undo_groups + retained.redo_groups;
        assert!(steps <= limits.undo_groups.max(1), "more steps than the count limit: {}", context());
        assert!(
            retained.undo_redo_held <= limits.undo_bytes || steps == 1,
            "over the undo budget with {steps} steps: {}",
            context()
        );
        if retained.undo_redo_held > limits.undo_bytes {
            self.coverage.over_budget_single_step += 1;
        }
        assert!(
            retained.history_entries <= limits.history_entries
                && (retained.history_bytes <= limits.history_bytes || retained.history_entries == 1),
            "over the history budget: {}",
            context()
        );
        // The base never trims: its stacks hold the same steps and more below.
        let base_retained = self.pair.base.retained(id).unwrap();
        assert!(
            retained.undo_groups <= base_retained.undo_groups && retained.redo_groups <= base_retained.redo_groups,
            "more steps than the base: {}",
            context()
        );
    }

    fn run(&mut self, ops: &[Op]) {
        let id = self.pair.open(SMALL_TEXT);
        let mut model = Model::new(self.limits, SMALL_TEXT);
        let limits = self.limits;
        for (index, &op) in ops.iter().enumerate() {
            let context = || format!("{limits:?}, operations {ops:?}, at #{index} ({op:?})");
            self.coverage.operations += 1;
            let mut compare_base = true;
            match op {
                Op::Undo | Op::Redo => {
                    let undo = op == Op::Undo;
                    let kind = if undo { ChangeKind::Undo } else { ChangeKind::Redo };
                    let (stack, base_groups) = {
                        let base_retained = self.pair.base.retained(&id).unwrap();
                        if undo {
                            (&model.undo, base_retained.undo_groups)
                        } else {
                            (&model.redo, base_retained.redo_groups)
                        }
                    };
                    if stack.is_empty() {
                        // Nothing is left here; the base may still hold steps
                        // that were dropped, so it is not asked.
                        let result = if undo {
                            self.pair.new.undo(&id, EditOrigin::Source)
                        } else {
                            self.pair.new.redo(&id, EditOrigin::Source)
                        };
                        let action = if undo { "undo" } else { "redo" };
                        assert_eq!(
                            result,
                            Err(TransactionError::NothingToUndo { document: id.clone(), action: action.into() }),
                            "{}",
                            context()
                        );
                        self.coverage.nothing_left_but_base_has += usize::from(base_groups > 0);
                        compare_base = false;
                    } else {
                        let exact = stack.back().unwrap().exact;
                        let (a, b) = if undo {
                            (self.pair.new.undo(&id, EditOrigin::Source), self.pair.base.undo(&id, EditOrigin::Source))
                        } else {
                            (self.pair.new.redo(&id, EditOrigin::Source), self.pair.base.redo(&id, EditOrigin::Source))
                        };
                        match (a, b) {
                            (Ok(event), Ok(mut base_event)) => {
                                model.step(undo, &event.patches, &context);
                                assert_eq!(event.change, kind);
                                (base_event.can_undo, base_event.can_redo) = (event.can_undo, event.can_redo);
                                assert_eq!(event, base_event, "{kind:?} differs from the base: {}", context());
                                self.coverage.exact_steps += usize::from(exact);
                                // The step just taken can be taken back.
                                let other = if undo { &model.redo } else { &model.undo };
                                assert!(!other.is_empty(), "the step made last was dropped: {}", context());
                            }
                            (Err(a), Err(b)) => {
                                assert_eq!(a, b, "{}", context());
                                assert!(matches!(a, TransactionError::Conflict { .. }), "{a:?}: {}", context());
                                assert!(!exact, "a step no unrecorded change touched was refused: {}", context());
                                self.coverage.blocked += 1;
                                compare_base = true;
                            }
                            (a, b) => panic!("{kind:?}: {a:?} here, {b:?} at the base: {}", context()),
                        }
                    }
                }
                Op::Save => {
                    let revision = model.revision();
                    let a = self.pair.new.mark_saved(&id, revision).unwrap();
                    let b = self.pair.base.mark_saved(&id, revision).unwrap();
                    assert_eq!((a.revision, a.saved_revision), (b.revision, b.saved_revision), "{}", context());
                    model.saved = Some(revision);
                }
                _ => {
                    let edit = Self::build(op, &model);
                    let (behavior, selection) = (edit.undo.clone(), edit.selection.is_some());
                    let origin = edit.origin.clone();
                    let base_revision = edit.base_revision;
                    if base_revision < model.oldest_base() {
                        // The revision was dropped from the rebase history;
                        // the base still has it, so it is not asked.
                        let request = self.pair.request(&id, edit);
                        assert_eq!(
                            self.pair.new.apply(request),
                            Err(TransactionError::HistoryUnavailable { document: id.clone(), base_revision }),
                            "{}",
                            context()
                        );
                        self.coverage.history_refused += 1;
                        compare_base = false;
                    } else {
                        match self.pair.apply_both(&id, edit, &context) {
                            Ok(outcome) => {
                                model.edit(&outcome.applied[0].patches, &behavior, selection, &origin);
                                if !matches!(behavior, UndoBehavior::Skip) {
                                    assert!(!model.undo.is_empty(), "the step made last was dropped: {}", context());
                                }
                            }
                            Err(TransactionError::Conflict { .. }) => self.coverage.conflicts += 1,
                            Err(other) => panic!("unexpected refusal {other:?}: {}", context()),
                        }
                    }
                }
            }
            self.check_events(&model, compare_base, &context);
            self.check_state(&id, &model, &context);
        }
        self.coverage.sequences += 1;
        self.coverage.dropped_undo += model.dropped_undo;
        self.coverage.dropped_redo += model.dropped_redo;
        self.coverage.dropped_history += model.dropped_history;
        self.pair.close(&id);
    }

    /// Every sequence of `depth` operations from `alphabet`.
    fn every_sequence(&mut self, alphabet: &[Op], depth: usize) {
        let mut digits = vec![0usize; depth];
        let mut ops = vec![alphabet[0]; depth];
        loop {
            for (op, digit) in ops.iter_mut().zip(&digits) {
                *op = alphabet[*digit];
            }
            self.run(&ops);
            let mut at = depth;
            loop {
                if at == 0 {
                    return;
                }
                at -= 1;
                digits[at] += 1;
                if digits[at] < alphabet.len() {
                    break;
                }
                digits[at] = 0;
            }
        }
    }
}

const ALPHABET: &[Op] = &[
    Op::Small,
    Op::Huge,
    Op::Type,
    Op::Undo,
    Op::Redo,
    Op::External,
    Op::SkipInsert,
    Op::SkipDelete,
    Op::Stale,
    Op::Multi,
];

const NO_LIMIT: usize = usize::MAX / 4;

/// Room for the records of `steps` small steps and `big` big texts.
fn undo_budget(steps: usize, big: usize) -> usize {
    steps * (held::UNDO_STEP + held::UNDO_PATCH + 2 * held::SELECTION_RANGE + 8) + big * (BIG_BYTES + 8)
}

fn configurations() -> Vec<(&'static str, HistoryLimits)> {
    let revision = held::REVISION + held::REVISION_PATCH;
    vec![
        (
            "one big text or three small steps; three revisions",
            HistoryLimits {
                undo_groups: 1000,
                undo_bytes: undo_budget(3, 0).max(undo_budget(1, 1)),
                history_entries: 2048,
                history_bytes: 3 * revision,
            },
        ),
        (
            "two big texts",
            HistoryLimits {
                undo_groups: 1000,
                undo_bytes: undo_budget(3, 2),
                history_entries: 2048,
                history_bytes: 4 * 1024 * 1024,
            },
        ),
        (
            "nothing but the step made last and the newest revision",
            HistoryLimits { undo_groups: 1000, undo_bytes: 0, history_entries: 2048, history_bytes: 0 },
        ),
        (
            "two steps and two revisions by count",
            HistoryLimits { undo_groups: 2, undo_bytes: NO_LIMIT, history_entries: 2, history_bytes: NO_LIMIT },
        ),
        (
            "two small steps",
            HistoryLimits {
                undo_groups: 3,
                undo_bytes: undo_budget(2, 0),
                history_entries: 4,
                history_bytes: 2 * revision,
            },
        ),
    ]
}

fn enumerate(depth: usize, alphabet: &[Op], only: Option<usize>) {
    for (index, (name, limits)) in configurations().into_iter().enumerate() {
        if only.is_some_and(|only| only != index) {
            continue;
        }
        let mut enumeration = Enumeration::new(limits);
        enumeration.every_sequence(alphabet, depth);
        let coverage = &enumeration.coverage;
        println!("depth {depth}, {name}: {coverage:?}");
        assert_eq!(coverage.sequences, (alphabet.len() as u64).pow(depth as u32));
        assert!(coverage.dropped_undo > 0 && coverage.exact_steps > 0 && coverage.blocked > 0, "{name}: {coverage:?}");
        // Redo steps go only once no undo step is left to drop: reached
        // where a few small steps fill the budget.
        if matches!(index, 0 | 4) {
            assert!(coverage.dropped_redo > 0, "{name}: {coverage:?}");
        }
        if index == 2 {
            assert!(coverage.history_refused > 0 && coverage.over_budget_single_step > 0, "{name}: {coverage:?}");
        }
        assert!(coverage.dropped_history > 0 || limits.history_bytes > 1024, "{name}: {coverage:?}");
    }
}

#[test]
fn every_sequence_of_five_operations_keeps_the_remaining_steps_exact() {
    enumerate(5, ALPHABET, None);
}

#[test]
fn every_sequence_of_five_operations_with_saves_keeps_the_remaining_steps_exact() {
    let mut alphabet = ALPHABET.to_vec();
    alphabet.push(Op::Save);
    enumerate(5, &alphabet, Some(0));
    enumerate(5, &alphabet, Some(2));
}

#[test]
fn every_sequence_of_six_operations_keeps_the_remaining_steps_exact() {
    enumerate(6, ALPHABET, Some(0));
    enumerate(6, ALPHABET, Some(2));
}

/// Twelve million sequences: run when `REVIEW_UNDO_DEPTH=7` asks for it
/// (`REVIEW_UNDO_CONFIG` selects one configuration); otherwise it does nothing.
#[test]
fn every_sequence_of_seven_operations_keeps_the_remaining_steps_exact_when_asked() {
    if std::env::var("REVIEW_UNDO_DEPTH").as_deref() != Ok("7") {
        println!("not asked (REVIEW_UNDO_DEPTH=7): nothing was enumerated");
        return;
    }
    let only = std::env::var("REVIEW_UNDO_CONFIG").ok().and_then(|value| value.parse().ok());
    enumerate(7, ALPHABET, Some(only.unwrap_or(0)));
}

// ----- accounting on refusals and over several documents ---------------------------

fn open_new(service: &new::DocumentService, text: &str, read_only: Option<&str>) -> DocumentId {
    service
        .open(new::OpenDocument {
            path: None,
            text: text.into(),
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: read_only.map(str::to_owned),
        })
        .id
}

fn transaction(edits: Vec<DocumentEdit>, undo: UndoBehavior) -> TransactionRequest {
    TransactionRequest {
        id: TransactionId("review-multi".into()),
        origin: EditOrigin::Source,
        edits,
        undo,
        conflict: ConflictPolicy::RebaseDisjoint,
        label: None,
        selection: None,
    }
}

fn counted(service: &new::DocumentService, id: &DocumentId) -> new::RetainedHistory {
    let retained = service.retained(id).unwrap();
    assert_eq!(retained.undo_redo_charged, retained.undo_redo_held);
    assert_eq!(retained.history_charged, retained.history_bytes);
    retained
}

#[test]
fn a_refused_transaction_over_several_documents_leaves_every_history_as_it_was() {
    let limits = HistoryLimits {
        undo_groups: 1000,
        undo_bytes: undo_budget(3, 1),
        history_entries: 2048,
        history_bytes: 3 * (held::REVISION + held::REVISION_PATCH),
    };
    let service = new::DocumentService::with_limits(limits);
    let a = open_new(&service, SMALL_TEXT, None);
    let b = open_new(&service, "bbbb", None);
    let locked = open_new(&service, "cccc", Some("review"));
    let edit = |document: &DocumentId, base_revision: u64, patches: Vec<TextPatch>| DocumentEdit {
        document: document.clone(),
        base_revision,
        patches,
    };
    // Fill `a` to the budget: three small steps, one undone.
    for revision in 1..=3 {
        service
            .apply(transaction(vec![edit(&a, revision, vec![TextPatch::insert(0, "s")])], UndoBehavior::NewGroup))
            .unwrap();
    }
    service.undo(&a, EditOrigin::Source).unwrap();
    let observe = || {
        let (sa, sb) = (service.state(&a).unwrap(), service.state(&b).unwrap());
        (counted(&service, &a), counted(&service, &b), sa.text, sa.info, sb.text, sb.info)
    };
    let before = observe();
    let huge = |document: &DocumentId, base_revision: u64, len: u32| {
        edit(document, base_revision, vec![TextPatch::replace(0, len, big_text())])
    };
    let a_len = utf16_len(&before.2);
    let refusals: [(Vec<DocumentEdit>, &str); 7] = [
        (vec![huge(&a, 5, a_len), edit(&b, 1, vec![TextPatch::insert(9, "x")])], "invalidRange"),
        (vec![edit(&b, 1, vec![TextPatch::insert(9, "x")]), huge(&a, 5, a_len)], "invalidRange"),
        (vec![huge(&a, 5, a_len), huge(&b, 1, 4), edit(&locked, 1, vec![TextPatch::insert(0, "x")])], "readOnly"),
        (vec![huge(&a, 5, a_len), huge(&b, 7, 4)], "futureRevision"),
        (vec![huge(&a, 5, a_len), huge(&a, 5, a_len)], "duplicateDocument"),
        (
            vec![huge(&a, 5, a_len), edit(&b, 1, vec![TextPatch::replace(2, 4, "x"), TextPatch::insert(3, "y")])],
            "overlappingPatches",
        ),
        // Revision 1 of `a` left its rebase history (three revisions are kept).
        (vec![huge(&b, 1, 4), edit(&a, 1, vec![TextPatch::insert(0, "x")])], "historyUnavailable"),
    ];
    for (edits, kind) in refusals {
        for behavior in [UndoBehavior::NewGroup, UndoBehavior::Skip, UndoBehavior::Merge { key: "k".into() }] {
            let error = service.apply(transaction(edits.clone(), behavior)).unwrap_err();
            let json = serde_json::to_value(&error).unwrap();
            assert_eq!(json["kind"], kind, "{error:?}");
            assert_eq!(before, observe(), "a refusal ({kind}) changed a document or its histories");
        }
    }
    // The same transaction without the failing part: both documents trim on their own.
    service.apply(transaction(vec![huge(&a, 5, a_len), huge(&b, 1, 4)], UndoBehavior::NewGroup)).unwrap();
    let (ra, rb) = (counted(&service, &a), counted(&service, &b));
    assert_eq!((ra.redo_groups, rb.redo_groups), (0, 0));
    assert!(ra.undo_redo_held <= limits.undo_bytes && rb.undo_redo_held <= limits.undo_bytes);
    assert!(ra.undo_groups >= 1 && rb.undo_groups == 1);
    assert_eq!(service.undo(&a, EditOrigin::Source).unwrap().revision, 7);
    assert_eq!(service.state(&a).unwrap().text, before.2);
    assert_eq!(service.undo(&b, EditOrigin::Source).unwrap().revision, 3);
    assert_eq!(service.state(&b).unwrap().text, "bbbb");
    counted(&service, &a);
    counted(&service, &b);
}

// =================================================================================
// G. A step larger than the whole budget, stepped back and forth
// =================================================================================

#[test]
fn a_step_over_the_budget_undone_and_redone_a_hundred_times_holds_one_step() {
    let limits = HistoryLimits { undo_bytes: 4096, ..HistoryLimits::default() };
    let service = new::DocumentService::with_limits(limits);
    let id = open_new(&service, "start 😀", None);
    let mut events = service.subscribe();
    // Two small steps first, so there is something for the big one to push out.
    for revision in 1..=2 {
        let edit =
            DocumentEdit { document: id.clone(), base_revision: revision, patches: vec![TextPatch::insert(0, "s")] };
        service.apply(transaction(vec![edit], UndoBehavior::NewGroup)).unwrap();
    }
    let small = service.state(&id).unwrap().text;
    let big = format!("😀{}é", "B".repeat(64 * 1024));
    let rewrite = DocumentEdit {
        document: id.clone(),
        base_revision: 3,
        patches: vec![TextPatch::replace(0, utf16_len(&small), big.clone())],
    };
    service.apply(transaction(vec![rewrite], UndoBehavior::NewGroup)).unwrap();
    let ceiling = big.len() + held::UNDO_STEP + held::UNDO_PATCH;
    let mut largest = 0;
    for cycle in 0..100 {
        let undone = service.undo(&id, EditOrigin::Source).unwrap();
        assert!(undone.can_redo, "cycle {cycle}: the undo just made can be redone");
        assert_eq!(service.state(&id).unwrap().text, small, "cycle {cycle}");
        let retained = counted(&service, &id);
        assert_eq!((retained.undo_groups, retained.redo_groups), (0, 1), "cycle {cycle}: {retained:?}");
        assert_eq!(undone.can_undo, retained.undo_groups > 0);
        largest = largest.max(retained.undo_redo_held);
        let redone = service.redo(&id, EditOrigin::Source).unwrap();
        assert!(redone.can_undo && !redone.can_redo, "cycle {cycle}");
        assert_eq!(service.state(&id).unwrap().text, big, "cycle {cycle}");
        let retained = counted(&service, &id);
        assert_eq!((retained.undo_groups, retained.redo_groups), (1, 0), "cycle {cycle}: {retained:?}");
        largest = largest.max(retained.undo_redo_held);
        assert_eq!(retained.history_bytes, retained.history_entries * (held::REVISION + held::REVISION_PATCH));
    }
    assert!(largest > limits.undo_bytes && largest <= ceiling, "held {largest}, one step is {ceiling}");
    assert_eq!(service.info(&id).unwrap().revision, 204);
    // Every event told the availability the info tells.
    let mut last = None;
    while let Ok(event) = events.try_recv() {
        if let DocumentEvent::Changed(changed) = event {
            last = Some((changed.can_undo, changed.can_redo));
        }
    }
    let info = service.info(&id).unwrap();
    assert_eq!(last, Some((info.can_undo, info.can_redo)));
}

// =================================================================================
// H. The numbers the documents state
// =================================================================================

const MIB: usize = 1024 * 1024;

#[test]
fn the_documented_numbers_are_the_constants() {
    // "41 MiB for the undo and redo steps together"; "at most 4 MiB".
    assert_eq!(new::UNDO_BYTE_BUDGET, 41 * MIB);
    assert_eq!(new::HISTORY_BYTE_BUDGET, 4 * MIB);
    assert_eq!(exposition_workspace::encoding::MAX_TEXT_BYTES, 10 * MIB as u64);
    let limits = HistoryLimits::default();
    assert_eq!((limits.undo_groups, limits.history_entries), (1000, 2048));
    assert_eq!((limits.undo_bytes, limits.history_bytes), (41 * MIB, 4 * MIB));
    // "four full replacements of a 10 MiB document fit" (with their records).
    let full = held::UNDO_STEP + held::UNDO_PATCH + 10 * MIB;
    assert!(4 * full <= new::UNDO_BYTE_BUDGET && 5 * full > new::UNDO_BYTE_BUDGET);
    // "a thousand steps that each put back less than 41 KiB reach the count limit first".
    let step = held::UNDO_STEP + held::UNDO_PATCH + 2 * held::SELECTION_RANGE + (41 * 1024 - 1);
    assert!(1000 * step <= new::UNDO_BYTE_BUDGET, "1000 steps of 41 KiB hold {}", 1000 * step);
    // "about 17 revisions of 20,000 patches each"; "holds 0.23 MiB".
    let revision = held::REVISION + 20_000 * held::REVISION_PATCH;
    assert_eq!(new::HISTORY_BYTE_BUDGET / revision, 17);
    assert_eq!((revision as f64 / MIB as f64 * 100.0).round(), 23.0);
    // "The full window of 2,048 revisions of typing holds about 0.2 MiB."
    let window = 2048 * (held::REVISION + held::REVISION_PATCH);
    println!(
        "records: step {}, patch {}, range {}, revision {}, revision patch {}; typing window {} bytes ({:.3} MiB)",
        held::UNDO_STEP,
        held::UNDO_PATCH,
        held::SELECTION_RANGE,
        held::REVISION,
        held::REVISION_PATCH,
        window,
        window as f64 / MIB as f64
    );
    assert!(window > MIB / 10 && window < MIB / 4, "{window}");
}

#[test]
fn seventeen_revisions_of_twenty_thousand_patches_stay_rebasable() {
    let service = new::DocumentService::new();
    let id = open_new(&service, &"ab".repeat(20_000), None);
    for revision in 1..=20u64 {
        let letter = if revision % 2 == 1 { "x" } else { "a" };
        let patches: Vec<TextPatch> = (0..20_000u32).map(|i| TextPatch::replace(2 * i, 2 * i + 1, letter)).collect();
        let edit = DocumentEdit { document: id.clone(), base_revision: revision, patches };
        service.apply(transaction(vec![edit], UndoBehavior::NewGroup)).unwrap();
    }
    let retained = counted(&service, &id);
    assert_eq!(retained.history_entries, 17, "{retained:?}");
    assert!(retained.history_bytes <= new::HISTORY_BYTE_BUDGET);
    // Revision 21; bases 4..=21 are available.
    let stale = |base_revision: u64| {
        let edit = DocumentEdit { document: id.clone(), base_revision, patches: vec![TextPatch::insert(40_000, "!")] };
        service.apply(transaction(vec![edit], UndoBehavior::NewGroup))
    };
    assert_eq!(stale(3), Err(TransactionError::HistoryUnavailable { document: id.clone(), base_revision: 3 }));
    assert_eq!(service.origins_since(&id, 3).unwrap(), None);
    assert_eq!(service.origins_since(&id, 4).unwrap().map(|origins| origins.len()), Some(17));
    let outcome = stale(4).unwrap();
    assert!(outcome.applied[0].rebased);
    assert_eq!(outcome.applied[0].patches, vec![TextPatch::insert(40_000, "!")]);
    assert!(service.state(&id).unwrap().text.ends_with("ab!"));
}
