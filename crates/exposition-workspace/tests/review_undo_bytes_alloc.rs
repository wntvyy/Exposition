//! Independent review of the byte budgets of the undo and rebase histories:
//! what the service allocates and keeps allocated, counted by the allocator
//! (bytes on the calling thread; independent of the build profile and of the
//! service's own accounting).
#![allow(unsafe_code)] // the counting allocator

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextEncoding, TextPatch, TransactionId, TransactionRequest,
    UndoBehavior,
};
use exposition_workspace::documents as new;
use exposition_workspace::documents::{HistoryLimits, held};

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "review_support/documents_b122b77.rs"]
mod base;

// ----- counting allocator -----------------------------------------------------------

thread_local! {
    /// Bytes requested on this thread, ever.
    static REQUESTED: Cell<u64> = const { Cell::new(0) };
    /// Bytes allocated and not yet freed on this thread.
    static LIVE: Cell<i64> = const { Cell::new(0) };
}

fn note(requested: usize, live: i64) {
    let _ = REQUESTED.try_with(|c| c.set(c.get() + requested as u64));
    let _ = LIVE.try_with(|c| c.set(c.get() + live));
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size(), layout.size() as i64);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size(), layout.size() as i64);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note(0, -(layout.size() as i64));
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size, new_size as i64 - layout.size() as i64);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn requested() -> u64 {
    REQUESTED.with(Cell::get)
}

fn live() -> i64 {
    LIVE.with(Cell::get)
}

/// Bytes requested from the allocator while `work` runs.
fn requested_by<T>(work: impl FnOnce() -> T) -> (u64, T) {
    let before = requested();
    let out = work();
    (requested() - before, out)
}

// ----- helpers ------------------------------------------------------------------------

const KIB: usize = 1024;

fn request(id: &DocumentId, base_revision: u64, patches: Vec<TextPatch>, undo: UndoBehavior) -> TransactionRequest {
    TransactionRequest {
        id: TransactionId("review".into()),
        origin: EditOrigin::Source,
        edits: vec![DocumentEdit { document: id.clone(), base_revision, patches }],
        undo,
        conflict: ConflictPolicy::Reject,
        label: None,
        selection: None,
    }
}

fn open_new(service: &new::DocumentService, text: &str) -> DocumentId {
    service
        .open(new::OpenDocument {
            path: None,
            text: text.into(),
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        })
        .id
}

fn open_base(service: &base::DocumentService, text: &str) -> DocumentId {
    service
        .open(base::OpenDocument {
            path: None,
            text: text.into(),
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        })
        .id
}

/// `groups` undo steps that each put back `bytes` of text in the middle of a
/// small document (after its first 8 characters), so an insertion at offset 0
/// moves every one of them.
fn middle_rewrites(groups: usize, bytes: usize) -> Vec<Vec<TextPatch>> {
    (0..groups)
        .map(|i| {
            let letter = if i % 2 == 0 { "x" } else { "y" };
            vec![TextPatch::replace(8, 8 + bytes as u32, letter.repeat(bytes))]
        })
        .collect()
}

// ----- claim 4: an unrecorded change moves ranges, it copies no step's text ---------------

#[test]
fn an_unrecorded_edit_under_a_deep_stack_requests_no_copy_of_the_steps() {
    const GROUPS: usize = 200;
    const BYTES: usize = 16 * KIB;
    let text = format!("headhead{}tail", "o".repeat(BYTES));
    let skip =
        |id: &DocumentId, revision: u64| request(id, revision, vec![TextPatch::insert(0, "😀")], UndoBehavior::Skip);

    let service = new::DocumentService::new();
    let id = open_new(&service, &text);
    for (i, patches) in middle_rewrites(GROUPS, BYTES).into_iter().enumerate() {
        service.apply(request(&id, 1 + i as u64, patches, UndoBehavior::NewGroup)).unwrap();
    }
    let before = service.retained(&id).unwrap();
    assert_eq!((before.undo_groups, before.undo_bytes), (GROUPS, GROUPS * BYTES));
    let (bytes, result) = requested_by(|| service.apply(skip(&id, 1 + GROUPS as u64)));
    result.unwrap();
    let after = service.retained(&id).unwrap();
    assert_eq!(after.undo_redo_held, before.undo_redo_held, "an unrecorded edit changes no step's bytes");
    assert_eq!(after.undo_redo_charged, after.undo_redo_held);

    // The base clones every step's text (the measurement can tell).
    let base_service = base::DocumentService::new();
    let base_id = open_base(&base_service, &text);
    for (i, patches) in middle_rewrites(GROUPS, BYTES).into_iter().enumerate() {
        base_service.apply(request(&base_id, 1 + i as u64, patches, UndoBehavior::NewGroup)).unwrap();
    }
    let (base_bytes, result) = requested_by(|| base_service.apply(skip(&base_id, 1 + GROUPS as u64)));
    result.unwrap();

    println!("unrecorded edit under {GROUPS} steps of {BYTES} bytes: {bytes} bytes requested (base: {base_bytes})");
    assert!(base_bytes as usize >= GROUPS * BYTES, "the base copies every step: {base_bytes}");
    // A few copies of the 16 KiB document and one record per step.
    assert!((bytes as usize) < 8 * BYTES + GROUPS * 64, "{bytes} bytes for one unrecorded edit");

    // The moved steps still undo exactly: every undo restores the 16 KiB it holds.
    for _ in 0..GROUPS {
        service.undo(&id, EditOrigin::Source).unwrap();
    }
    assert_eq!(service.state(&id).unwrap().text, format!("😀{text}"));
}

// ----- claim 3 and the standard: no work per edit in proportion to the histories ----------

#[test]
fn a_typed_character_requests_the_same_bytes_under_full_histories() {
    const BYTES: usize = 4 * KIB;
    let text = format!("headhead{}tail", "o".repeat(BYTES));
    let typed = |service: &new::DocumentService, id: &DocumentId| {
        let mut least = u64::MAX;
        for _ in 0..12 {
            let info = service.info(id).unwrap();
            let at = 4;
            let patch = vec![TextPatch::insert(at, "t")];
            let merge = UndoBehavior::Merge { key: "typing".into() };
            let (bytes, result) = requested_by(|| service.apply(request(id, info.revision, patch, merge)));
            result.unwrap();
            least = least.min(bytes);
        }
        least
    };

    let fresh = new::DocumentService::new();
    let fresh_id = open_new(&fresh, &text);
    let shallow = typed(&fresh, &fresh_id);

    let service = new::DocumentService::new();
    let id = open_new(&service, &text);
    // 999 steps of 4 KiB and a full revision window.
    for (i, patches) in middle_rewrites(2100, BYTES).into_iter().enumerate() {
        service.apply(request(&id, 1 + i as u64, patches, UndoBehavior::NewGroup)).unwrap();
    }
    let retained = service.retained(&id).unwrap();
    assert_eq!((retained.undo_groups, retained.history_entries), (1000, 2048));
    let deep = typed(&service, &id);
    let retained = service.retained(&id).unwrap();
    assert_eq!((retained.undo_groups, retained.history_entries), (1000, 2048));
    assert_eq!(retained.undo_redo_charged, retained.undo_redo_held);

    println!("one typed character: {shallow} bytes requested with empty histories, {deep} with full ones");
    assert!(deep <= shallow + 1024, "typing under full histories requests {deep} bytes, {shallow} without");
}

#[test]
fn trimming_requests_nothing_and_frees_the_dropped_steps() {
    const BYTES: usize = 8 * KIB;
    // Room for four steps.
    let limits =
        HistoryLimits { undo_bytes: 4 * (BYTES + held::UNDO_STEP + held::UNDO_PATCH), ..HistoryLimits::default() };
    let text = format!("headhead{}tail", "o".repeat(BYTES));
    let unlimited = new::DocumentService::new();
    let limited = new::DocumentService::with_limits(limits);
    let (a, b) = (open_new(&unlimited, &text), open_new(&limited, &text));
    let (mut most_untrimmed, mut most_trimmed) = (0, 0);
    let live_before = live();
    for (i, patches) in middle_rewrites(64, BYTES).into_iter().enumerate() {
        let (untrimmed, result) =
            requested_by(|| unlimited.apply(request(&a, 1 + i as u64, patches.clone(), UndoBehavior::NewGroup)));
        result.unwrap();
        let live_limited = live();
        let (trimmed, result) =
            requested_by(|| limited.apply(request(&b, 1 + i as u64, patches, UndoBehavior::NewGroup)));
        result.unwrap();
        if i >= 8 {
            most_untrimmed = most_untrimmed.max(untrimmed);
            most_trimmed = most_trimmed.max(trimmed);
            // The dropped step is freed: the edit leaves no more allocated than before.
            assert!(live() - live_limited < 4096, "edit {i} grew live memory by {}", live() - live_limited);
        }
    }
    let retained = limited.retained(&b).unwrap();
    assert_eq!(retained.undo_groups, 4, "{retained:?}");
    println!(
        "an edit that drops one step requests at most {most_trimmed} bytes; without trimming {most_untrimmed}; \
         live after both services: {}",
        live() - live_before
    );
    // Growth of the untrimmed stack's ring buffer aside, the two request the same.
    assert!(most_trimmed <= most_untrimmed + 1024, "{most_trimmed} against {most_untrimmed}");
}

// ----- residual: a step larger than the budget, stepped back and forth ---------------------

fn ping_pong(subscribe: bool) -> (i64, i64) {
    const BYTES: usize = 256 * KIB;
    let limits = HistoryLimits { undo_bytes: 16 * KIB, ..HistoryLimits::default() };
    let service = new::DocumentService::with_limits(limits);
    let _lagging = subscribe.then(|| service.subscribe());
    let id = open_new(&service, "small");
    service.apply(request(&id, 1, vec![TextPatch::replace(0, 5, "B".repeat(BYTES))], UndoBehavior::NewGroup)).unwrap();
    let cycle = || {
        service.undo(&id, EditOrigin::Source).unwrap();
        service.redo(&id, EditOrigin::Source).unwrap();
    };
    cycle();
    let after_first = live();
    let mut most = after_first;
    for _ in 0..100 {
        cycle();
        most = most.max(live());
    }
    let retained = service.retained(&id).unwrap();
    assert_eq!((retained.undo_groups, retained.redo_groups, retained.undo_bytes), (1, 0, 5));
    assert_eq!(service.state(&id).unwrap().text.len(), BYTES);
    (most - after_first, live() - after_first)
}

#[test]
fn a_step_over_the_budget_stepped_a_hundred_times_keeps_no_more_allocated() {
    let (most, last) = ping_pong(false);
    println!("100 undo/redo cycles of a 256 KiB step: live memory grew by at most {most} bytes, {last} at the end");
    // 200 more revisions of the rebase history (records only) and nothing else.
    assert!(most < 64 * KIB as i64, "live memory grew by {most} bytes over 100 cycles");
}

/// Not a bound of the histories: the events of the service carry the patch
/// texts, and the event channel keeps up to 4,096 events for a subscriber
/// that has not read them. Measured for the report; the bound asserted is the
/// channel's, not the undo budget's.
#[test]
fn events_not_yet_read_by_a_subscriber_hold_the_texts_of_the_steps() {
    let (most, last) = ping_pong(true);
    println!("the same with a subscriber that does not read: live memory grew by {most} bytes ({last} at the end)");
    // 100 undo events of 256 KiB each wait in the channel.
    assert!(most >= 100 * 256 * KIB as i64, "{most}");
    assert!(most < 110 * 256 * KIB as i64, "{most}");
}

// ----- what the budget does not count --------------------------------------------------

/// A step keeps the key it was made with (`UndoBehavior::Merge`), a string of
/// the caller's, and the key counts toward the budget like the texts: two
/// hundred steps with keys of 32 KiB do not stay allocated under a budget of
/// 64 KiB.
#[test]
fn the_merge_keys_of_the_steps_count_toward_the_undo_budget() {
    const KEY: usize = 32 * KIB;
    let limits = HistoryLimits { undo_bytes: 64 * KIB, ..HistoryLimits::default() };
    let service = new::DocumentService::with_limits(limits);
    let id = open_new(&service, "x");
    let before = live();
    for i in 0..200u64 {
        let key = format!("{i:0>KEY$}");
        service.apply(request(&id, 1 + i, vec![TextPatch::insert(0, "k")], UndoBehavior::Merge { key })).unwrap();
    }
    let retained = service.retained(&id).unwrap();
    let grown = live() - before;
    println!(
        "200 steps with 32 KiB keys: {} steps kept, {} bytes charged against a budget of {}, {grown} bytes allocated",
        retained.undo_groups, retained.undo_redo_charged, limits.undo_bytes
    );
    assert!(
        grown < 4 * limits.undo_bytes as i64,
        "{grown} bytes are kept by a history bounded to {}",
        limits.undo_bytes
    );
}
