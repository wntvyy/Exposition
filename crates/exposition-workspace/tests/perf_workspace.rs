//! Scale benchmarks for the document and workspace services (ignored tests;
//! run with `node scripts/perf/rust-bench.mjs exposition-workspace`).
//!
//! Each measurement prints one `PERF {...}` line. Timings are repeated samples
//! after a warm-up; the counters next to them do not depend on the machine or
//! the build profile:
//! - `copied` / `scanned`: bytes of document text the document service copied
//!   and scanned (`exposition_workspace::documents::work`); `passes` is their
//!   sum divided by the text length.
//! - `large_allocs`: heap allocations of at least half the text length made
//!   during the operation by any code (counting allocator of this binary).
//! - byte sizes of what an operation returns, writes, or emits.
//!
//! Scale (`EXPOSITION_PERF_SCALE`): `small` 100 KiB and 1 MiB documents, a
//! 1,000-entry folder; `large` adds 10 MiB documents (the `MAX_TEXT_BYTES`
//! limit) and a 10,000-entry folder; `stress` adds a 100,000-entry folder and
//! a 100 MiB file that must be refused without being read.
#![allow(unsafe_code)] // the counting allocator

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentEvent, DocumentId, EditOrigin, TextEncoding, TextPatch, TransactionId,
    TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::utf16_len;
use exposition_workspace::documents::work;
use exposition_workspace::encoding::MAX_TEXT_BYTES;
use exposition_workspace::types::{FileChange, FileChangeKind, WorkspaceEvent};
use exposition_workspace::{DocumentService, OpenDocument, WorkspaceService};
use notify::event::CreateKind;
use notify::{Event, EventKind};
use serde_json::json;

// ----- counting allocator -----------------------------------------------------

struct Counting;

static LARGE_MIN: AtomicUsize = AtomicUsize::new(usize::MAX);
static LARGE_ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

fn note(size: usize) {
    ALLOC_BYTES.fetch_add(size as u64, Ordering::Relaxed);
    if size >= LARGE_MIN.load(Ordering::Relaxed) {
        LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            note(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// ----- measurement ------------------------------------------------------------

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;
const SAMPLES: usize = 7;
/// Pause between writing a file and timing an operation that opens it.
const SETTLE: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, Default)]
struct Work {
    copied: u64,
    scanned: u64,
    large_allocs: u64,
    alloc_bytes: u64,
}

impl Work {
    fn json(&self, text_len: usize) -> serde_json::Value {
        let passes = (self.copied + self.scanned) as f64 / text_len.max(1) as f64;
        json!({
            "copied": self.copied,
            "scanned": self.scanned,
            "passes": (passes * 100.0).round() / 100.0,
            "large_allocs": self.large_allocs,
            "alloc_bytes": self.alloc_bytes,
        })
    }
}

/// Run `f` once: elapsed milliseconds, the work it did, and its result.
/// "Large" allocations are those of at least half of `text_len` bytes.
fn measure<T>(text_len: usize, f: impl FnOnce() -> T) -> (f64, Work, T) {
    work::reset();
    LARGE_MIN.store((text_len / 2).max(1), Ordering::Relaxed);
    LARGE_ALLOCS.store(0, Ordering::Relaxed);
    ALLOC_BYTES.store(0, Ordering::Relaxed);
    let started = Instant::now();
    let out = f();
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let (copied, scanned) = work::totals();
    let result = Work {
        copied,
        scanned,
        large_allocs: LARGE_ALLOCS.load(Ordering::Relaxed),
        alloc_bytes: ALLOC_BYTES.load(Ordering::Relaxed),
    };
    LARGE_MIN.store(usize::MAX, Ordering::Relaxed);
    (ms, result, out)
}

fn perf(bench: &str, scale: serde_json::Value, samples_ms: &[f64], counters: serde_json::Value) {
    let rounded: Vec<f64> = samples_ms.iter().map(|ms| (ms * 1000.0).round() / 1000.0).collect();
    println!("PERF {}", json!({ "bench": bench, "scale": scale, "samples_ms": rounded, "counters": counters }));
}

fn merge(mut counters: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    if let (Some(a), Some(b)) = (counters.as_object_mut(), extra.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
    counters
}

fn scale_name() -> String {
    std::env::var("EXPOSITION_PERF_SCALE").unwrap_or_else(|_| "small".into())
}

fn doc_sizes() -> Vec<usize> {
    match scale_name().as_str() {
        "large" | "stress" => vec![100 * KIB, MIB, 10 * MIB],
        _ => vec![100 * KIB, MIB],
    }
}

fn dir_sizes() -> Vec<usize> {
    match scale_name().as_str() {
        "stress" => vec![1_000, 10_000, 100_000],
        "large" => vec![1_000, 10_000],
        _ => vec![1_000],
    }
}

fn size_label(bytes: usize) -> String {
    if bytes >= MIB { format!("{}MiB", bytes / MIB) } else { format!("{}KiB", bytes / KIB) }
}

/// Deterministic text of exactly `bytes` bytes in LF-terminated lines of about
/// 80 bytes. `unicode` mixes in two-, three- and four-byte characters.
fn text_of(bytes: usize, unicode: bool, seed: u64) -> String {
    const WORDS: &[&str] = &["lemma", "proof", "theorem", "let", "then", "hence", "x", "y", "sum", "integral", "since"];
    const SYMBOLS: &[&str] = &["é", "∀", "ℝ", "→", "𝔸", "λ"];
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let mut out = String::with_capacity(bytes + 16);
    let mut line = 0usize;
    while out.len() < bytes {
        let pick = next();
        if unicode && pick.is_multiple_of(5) {
            out.push_str(SYMBOLS[pick / 7 % SYMBOLS.len()]);
        } else {
            out.push_str(WORDS[pick % WORDS.len()]);
        }
        out.push(' ');
        line += 1;
        if line.is_multiple_of(12) {
            out.push('\n');
        }
    }
    while out.len() > bytes {
        out.pop();
    }
    while out.len() < bytes {
        out.push('.');
    }
    out
}

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

struct Project {
    dir: tempfile::TempDir,
    service: Arc<WorkspaceService>,
}

impl Project {
    fn root(&self) -> PathBuf {
        self.dir.path().join("project")
    }
    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }
    /// A fresh service over the same folders, as after the process was killed.
    fn restart(&self) -> Arc<WorkspaceService> {
        let service = WorkspaceService::new(Arc::new(DocumentService::new()), self.data());
        service.open_workspace(&self.root()).unwrap();
        service
    }
}

fn project(files: &[(&str, &str)]) -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    fs::create_dir_all(&root).unwrap();
    for (name, text) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    service.open_workspace(&root).unwrap();
    Project { dir, service }
}

fn request(id: &DocumentId, base: u64, patches: Vec<TextPatch>, undo: UndoBehavior) -> TransactionRequest {
    TransactionRequest {
        id: TransactionId(format!("perf-{base}")),
        origin: EditOrigin::Source,
        edits: vec![DocumentEdit { document: id.clone(), base_revision: base, patches }],
        undo,
        conflict: ConflictPolicy::Reject,
        label: None,
        selection: None,
    }
}

fn typing() -> UndoBehavior {
    UndoBehavior::Merge { key: "typing".into() }
}

fn type_char(docs: &DocumentService, id: &DocumentId, at: u32) {
    let base = docs.info(id).unwrap().revision;
    docs.apply(request(id, base, vec![TextPatch::insert(at, "x")], typing())).unwrap();
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(read) = fs::read_dir(dir) else { return 0 };
    read.flatten()
        .map(|e| {
            let meta = e.metadata().unwrap();
            if meta.is_dir() { dir_bytes(&e.path()) } else { meta.len() }
        })
        .sum()
}

// ----- documents ----------------------------------------------------------------

/// Opening a project file as a document, and the size of what the command returns.
#[test]
#[ignore = "perf"]
fn document_open() {
    for size in doc_sizes() {
        let p = project(&[("doc.txt", &text_of(size, false, 1))]);
        let path = wp("doc.txt");
        let mut samples = Vec::new();
        let mut last = (Work::default(), 0usize);
        for i in 0..=SAMPLES {
            let (ms, work, opened) = measure(size, || p.service.open_document(&path).unwrap());
            let returned = serde_json::to_vec(&opened).unwrap().len();
            p.service.close_document(&opened.state.info.id).unwrap();
            if i > 0 {
                samples.push(ms);
            }
            last = (work, returned);
        }
        perf(
            "workspace.document_open",
            json!({ "bytes": size, "size": size_label(size) }),
            &samples,
            merge(last.0.json(size), json!({ "returned_bytes": last.1 })),
        );
    }
}

/// A file above `MAX_TEXT_BYTES` is refused from its metadata: nothing of its
/// size is allocated or read.
#[test]
#[ignore = "perf"]
fn oversize_file_is_refused_without_loading() {
    let mut sizes = vec![MAX_TEXT_BYTES as usize + 1];
    if scale_name() == "stress" {
        sizes.push(100 * MIB);
    }
    for size in sizes {
        let p = project(&[("small.txt", "x\n")]);
        let file = fs::File::create(p.root().join("big.txt")).unwrap();
        file.set_len(size as u64).unwrap();
        drop(file);
        let path = wp("big.txt");
        let mut samples = Vec::new();
        let mut last = Work::default();
        for i in 0..=SAMPLES {
            let (ms, work, result) = measure(2 * MIB, || p.service.open_document(&path));
            assert_eq!(result.unwrap_err().code, "file.tooLarge");
            assert_eq!(work.large_allocs, 0, "an oversize file must not be loaded");
            if i > 0 {
                samples.push(ms);
            }
            last = work;
        }
        assert!(p.service.documents().list().is_empty());
        perf(
            "workspace.oversize_refusal",
            json!({ "bytes": size, "limit": MAX_TEXT_BYTES }),
            &samples,
            json!({ "large_allocs": last.large_allocs, "alloc_bytes": last.alloc_bytes }),
        );
    }
}

/// One typed character (merged into the typing undo group) at the start,
/// middle, and end of ASCII and non-ASCII text: passes and copies of the text,
/// and the bytes the command and its event carry.
#[test]
#[ignore = "perf"]
fn apply_one_small_edit() {
    for size in doc_sizes() {
        for unicode in [false, true] {
            let text = text_of(size, unicode, 2);
            for position in ["start", "middle", "end"] {
                let byte = match position {
                    "start" => 0,
                    "middle" => (0..=size / 2).rev().find(|b| text.is_char_boundary(*b)).unwrap(),
                    _ => text.len(),
                };
                let at = utf16_len(&text[..byte]) as u32;
                let docs = DocumentService::new();
                let info = docs.open(OpenDocument {
                    path: None,
                    text: text.clone(),
                    language: "plaintext".into(),
                    encoding: TextEncoding::Utf8,
                    read_only: None,
                });
                let mut events = docs.subscribe();
                // Warm-up: the first character starts the undo group, the next merges.
                type_char(&docs, &info.id, at);
                type_char(&docs, &info.id, at + 1);
                while events.try_recv().is_ok() {}
                let mut samples = Vec::new();
                let mut last = (Work::default(), 0usize, 0usize);
                for i in 0..SAMPLES as u32 {
                    let base = docs.info(&info.id).unwrap().revision;
                    let req = request(&info.id, base, vec![TextPatch::insert(at + 2 + i, "x")], typing());
                    let (ms, work, outcome) = measure(size, || docs.apply(req).unwrap());
                    let mut event_bytes = 0;
                    while let Ok(event) = events.try_recv() {
                        if matches!(event, DocumentEvent::Changed(_)) {
                            event_bytes += serde_json::to_vec(&event).unwrap().len();
                        }
                    }
                    samples.push(ms);
                    last = (work, serde_json::to_vec(&outcome).unwrap().len(), event_bytes);
                }
                let retained = docs.retained(&info.id).unwrap();
                assert_eq!(retained.undo_groups, 1, "typing merges into one undo group");
                perf(
                    "documents.apply_typed_char",
                    json!({ "bytes": size, "size": size_label(size), "text": if unicode { "unicode" } else { "ascii" }, "at": position }),
                    &samples,
                    merge(last.0.json(size), json!({ "result_bytes": last.1, "event_bytes": last.2 })),
                );

                // Undo of the typing group, then redo (restoring the state).
                let mut undo_samples = Vec::new();
                let mut undo_work = Work::default();
                for _ in 0..SAMPLES {
                    let (ms, work, _) = measure(size, || docs.undo(&info.id, EditOrigin::Source).unwrap());
                    docs.redo(&info.id, EditOrigin::Source).unwrap();
                    undo_samples.push(ms);
                    undo_work = work;
                }
                if position == "middle" {
                    perf(
                        "documents.undo_typing_group",
                        json!({ "bytes": size, "size": size_label(size), "text": if unicode { "unicode" } else { "ascii" } }),
                        &undo_samples,
                        undo_work.json(size),
                    );
                }
            }
        }
    }
}

/// One edit that starts its own undo group (a paste, an agent edit).
#[test]
#[ignore = "perf"]
fn apply_one_edit_in_a_new_group() {
    for size in doc_sizes() {
        let text = text_of(size, false, 3);
        let docs = DocumentService::new();
        let info = docs.open(OpenDocument {
            path: None,
            text,
            language: "plaintext".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        });
        let at = (size / 2) as u32;
        let mut samples = Vec::new();
        let mut last = Work::default();
        for i in 0..=SAMPLES as u32 {
            let base = docs.info(&info.id).unwrap().revision;
            let req = request(&info.id, base, vec![TextPatch::insert(at + i, "y")], UndoBehavior::NewGroup);
            let (ms, work, _) = measure(size, || docs.apply(req).unwrap());
            if i > 0 {
                samples.push(ms);
            }
            last = work;
        }
        perf(
            "documents.apply_new_group",
            json!({ "bytes": size, "size": size_label(size) }),
            &samples,
            last.json(size),
        );
    }
}

// ----- journal, save, external changes --------------------------------------------

/// The recovery-journal write that follows an edit burst, a save, and the
/// check of an open document after its file was reported changed.
#[test]
#[ignore = "perf"]
fn journal_save_and_disk_check() {
    for size in doc_sizes() {
        // Room for the characters typed below: a 10 MiB document is at the limit.
        let size = size - KIB;
        let p = project(&[("doc.txt", &text_of(size, false, 4))]);
        let id = p.service.open_document(&wp("doc.txt")).unwrap().state.info.id;
        let docs = Arc::clone(p.service.documents());
        let ids = HashSet::from([id.clone()]);
        let scale = json!({ "bytes": size, "size": size_label(size + KIB) });
        let at = (size / 2) as u32;

        let mut samples = Vec::new();
        let mut last = Work::default();
        for i in 0..=SAMPLES as u32 {
            type_char(&docs, &id, at + i);
            let (ms, work, ()) = measure(size, || p.service.journal_documents(&ids));
            if i > 0 {
                samples.push(ms);
            }
            last = work;
        }
        let journal_bytes = dir_bytes(&p.data().join("recovery"));
        assert!(journal_bytes as usize > size, "the journal holds the unsaved text");
        perf(
            "workspace.journal_write",
            scale.clone(),
            &samples,
            merge(last.json(size), json!({ "journal_bytes": journal_bytes })),
        );

        let mut samples = Vec::new();
        let mut last = (Work::default(), 0u64);
        for i in 0..=SAMPLES as u32 {
            type_char(&docs, &id, at + 100 + i);
            // Saves are spaced as a person spaces them. Back to back, each
            // one waits for the virus scan of the file the last one wrote.
            std::thread::sleep(SETTLE);
            let (ms, work, saved) = measure(size, || p.service.save_document(&id, false).unwrap());
            assert_eq!(saved.saved_revision, Some(saved.revision));
            if i > 0 {
                samples.push(ms);
            }
            last = (work, fs::metadata(p.root().join("doc.txt")).unwrap().len());
        }
        assert_eq!(dir_bytes(&p.data().join("recovery")), 0, "saving removes the journal");
        perf("workspace.save", scale.clone(), &samples, merge(last.0.json(size), json!({ "file_bytes": last.1 })));

        // The watcher reports the file the app just saved: nothing changed.
        let mut samples = Vec::new();
        let mut last = Work::default();
        for i in 0..=SAMPLES {
            std::thread::sleep(SETTLE);
            let (ms, work, ()) = measure(size, || p.service.check_document(&id));
            if i > 0 {
                samples.push(ms);
            }
            last = work;
        }
        perf("workspace.disk_check_unchanged", scale.clone(), &samples, last.json(size));

        // Another program rewrote the first line: the clean buffer is reloaded.
        let mut samples = Vec::new();
        let mut last = Work::default();
        for i in 0..=SAMPLES {
            let mut text = fs::read_to_string(p.root().join("doc.txt")).unwrap();
            text.replace_range(0..12, &format!("ext{i:08}\n"));
            fs::write(p.root().join("doc.txt"), &text).unwrap();
            std::thread::sleep(SETTLE);
            let (ms, work, ()) = measure(size, || p.service.check_document(&id));
            assert_eq!(*docs.snapshot(&id).unwrap().text, *text, "the buffer follows the file");
            if i > 0 {
                samples.push(ms);
            }
            last = work;
        }
        perf("workspace.disk_check_reload", scale, &samples, last.json(size));
    }
}

/// Matching a batch of file changes against the open documents.
#[test]
#[ignore = "perf"]
fn changed_documents_scan() {
    let (documents, changes) = match scale_name().as_str() {
        "large" | "stress" => (50, 100_000),
        _ => (10, 5_000),
    };
    let names: Vec<String> = (0..documents).map(|i| format!("chapters/part{}/doc{i}.typ", i % 5)).collect();
    let files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "= Title\n")).collect();
    let p = project(&files);
    for name in &names {
        p.service.open_document(&wp(name)).unwrap();
    }
    let batch: Vec<FileChange> = (0..changes)
        .map(|i| FileChange { path: wp(&format!("data/run{}/out{i}.csv", i % 100)), kind: FileChangeKind::Modified })
        .collect();
    let mut samples = Vec::new();
    for i in 0..=SAMPLES {
        let (ms, _, ()) = measure(MIB, || p.service.check_changed_documents(&batch, false));
        if i > 0 {
            samples.push(ms);
        }
    }
    perf(
        "workspace.changed_documents_scan",
        json!({ "documents": documents, "changes": changes }),
        &samples,
        json!({ "pairs": documents * changes }),
    );
}

// ----- directory listing ------------------------------------------------------------

#[test]
#[ignore = "perf"]
fn directory_listing() {
    for entries in dir_sizes() {
        let p = project(&[("main.typ", "= Main\n")]);
        let folder = p.root().join("many");
        fs::create_dir_all(&folder).unwrap();
        for i in 0..entries {
            if i.is_multiple_of(100) {
                fs::create_dir(folder.join(format!("Group{i:06}"))).unwrap();
            } else {
                fs::write(folder.join(format!("{}{i:06}.typ", if i.is_multiple_of(2) { "File" } else { "file" })), b"")
                    .unwrap();
            }
        }
        let dir = wp("many");
        let mut samples = Vec::new();
        let mut last = (0usize, 0usize, Work::default());
        for i in 0..=SAMPLES {
            let (ms, work, listed) = measure(64 * KIB, || p.service.list_dir(Some(&dir)).unwrap());
            assert_eq!(listed.len(), entries);
            if i > 0 {
                samples.push(ms);
            }
            last = (listed.len(), serde_json::to_vec(&listed).unwrap().len(), work);
        }
        perf(
            "workspace.list_dir",
            json!({ "entries": entries }),
            &samples,
            json!({ "entries": last.0, "returned_bytes": last.1, "alloc_bytes": last.2.alloc_bytes }),
        );
    }
}

// ----- watcher ------------------------------------------------------------------------

struct Burst {
    events: u64,
    changes: u64,
    rescans: u64,
    payload_bytes: u64,
    last_event_ms: f64,
}

/// Collect `FilesChanged` events until none arrived for `quiet`.
async fn collect_files_changed(
    rx: &mut tokio::sync::broadcast::Receiver<WorkspaceEvent>,
    started: Instant,
    quiet: Duration,
) -> Burst {
    let mut burst = Burst { events: 0, changes: 0, rescans: 0, payload_bytes: 0, last_event_ms: 0.0 };
    loop {
        match tokio::time::timeout(quiet, rx.recv()).await {
            Ok(Ok(event)) => {
                if let WorkspaceEvent::FilesChanged { changes, rescan } = &event {
                    burst.events += 1;
                    burst.changes += changes.len() as u64;
                    burst.rescans += u64::from(*rescan);
                    burst.payload_bytes += serde_json::to_vec(&event).unwrap().len() as u64;
                    burst.last_event_ms = started.elapsed().as_secs_f64() * 1000.0;
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => burst.rescans += 1,
            Ok(Err(_)) | Err(_) => return burst,
        }
    }
}

const HIDDEN_TREES: &[&str] = &[".git/objects/ab", "target/debug/deps", "node_modules/pkg/lib", ".lake/build/lib"];

/// A burst of watcher notifications delivered as the OS watcher delivers them:
/// 5,000 events of which 4,000 are under `.git`, `target`, `node_modules`,
/// and `.lake`; then 5,000 that are all under those trees.
#[test]
#[ignore = "perf"]
fn watcher_burst() {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let p = project(&[("main.typ", "= Main\n")]);
    let root = p.service.open_root().unwrap();
    let service = Arc::clone(&p.service);
    runtime.spawn(Arc::clone(&service).run());
    let event_at = |rel: &str| Ok(Event::new(EventKind::Create(CreateKind::File)).add_path(root.join(rel)));
    for (name, visible) in [("mixed", 1_000u32), ("hidden_only", 0)] {
        let mut samples = Vec::new();
        let mut last = (0u64, None::<Burst>);
        for round in 0..5 {
            let mut rx = service.subscribe();
            let queued_before = service.fs_signals_queued();
            let started = Instant::now();
            let mut injected = 0u64;
            for i in 0..1_000u32 {
                for tree in HIDDEN_TREES {
                    service.inject_fs_event(event_at(&format!("{tree}/r{round}/f{i}.o")));
                    injected += 1;
                }
                if i < visible {
                    service.inject_fs_event(event_at(&format!("src/r{round}/f{i}.typ")));
                    injected += 1;
                } else if visible == 0 && i.is_multiple_of(4) {
                    // Keep the batch at 5,000 events.
                    for tree in HIDDEN_TREES {
                        service.inject_fs_event(event_at(&format!("{tree}/r{round}/g{i}.o")));
                        injected += 1;
                    }
                }
            }
            let inject_ms = started.elapsed().as_secs_f64() * 1000.0;
            let burst = runtime.block_on(collect_files_changed(&mut rx, started, Duration::from_millis(700)));
            assert_eq!(burst.changes, u64::from(visible), "only visible paths are reported");
            samples.push(if visible > 0 { burst.last_event_ms } else { inject_ms });
            last = (service.fs_signals_queued() - queued_before, Some(burst));
            assert_eq!(injected, 5_000);
        }
        let burst = last.1.unwrap();
        perf(
            &format!("workspace.watcher_burst.{name}"),
            json!({ "events": 5_000, "visible": visible }),
            &samples,
            json!({
                "queued_signals": last.0,
                "files_changed_events": burst.events,
                "changes": burst.changes,
                "rescans": burst.rescans,
                "payload_bytes": burst.payload_bytes,
            }),
        );
    }
}

/// The real OS watcher: files created under `.git` and in a visible folder.
#[test]
#[ignore = "perf"]
fn watcher_real_files() {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let p = project(&[("main.typ", "= Main\n")]);
    let service = Arc::clone(&p.service);
    runtime.spawn(Arc::clone(&service).run());
    let hidden = p.root().join(".git").join("objects");
    let visible = p.root().join("notes");
    fs::create_dir_all(&hidden).unwrap();
    fs::create_dir_all(&visible).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let mut rx = service.subscribe();
    let queued_before = service.fs_signals_queued();
    let started = Instant::now();
    for i in 0..2_000 {
        fs::write(hidden.join(format!("o{i}")), b"x").unwrap();
    }
    for i in 0..200 {
        fs::write(visible.join(format!("n{i}.md")), b"x").unwrap();
    }
    let burst = runtime.block_on(collect_files_changed(&mut rx, started, Duration::from_millis(1_500)));
    perf(
        "workspace.watcher_real_files",
        json!({ "hidden_files": 2_000, "visible_files": 200 }),
        &[burst.last_event_ms],
        json!({
            "queued_signals": service.fs_signals_queued() - queued_before,
            "files_changed_events": burst.events,
            "changes": burst.changes,
            "rescans": burst.rescans,
            "payload_bytes": burst.payload_bytes,
        }),
    );
}

/// Continuous typing with the background loop running: how many journal
/// writes used the file staged outside the I/O lock, and how many had to be
/// written under the lock because the document changed while its journal was
/// being written.
#[test]
#[ignore = "perf"]
fn journal_writes_while_typing() {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    for size in doc_sizes() {
        let size = size - KIB;
        for keys_per_second in [5u64, 10] {
            let p = project(&[("doc.txt", &text_of(size, false, 6))]);
            let service = Arc::clone(&p.service);
            let id = service.open_document(&wp("doc.txt")).unwrap().state.info.id;
            let loop_task = runtime.spawn(Arc::clone(&service).run());
            let docs = Arc::clone(service.documents());
            let before = service.journal_write_counts();
            let started = Instant::now();
            let interval = Duration::from_millis(1000 / keys_per_second);
            let mut keys = 0u32;
            while started.elapsed() < Duration::from_secs(8) {
                let next = started + interval * (keys + 1);
                type_char(&docs, &id, (size / 2) as u32 + keys);
                keys += 1;
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
            // The pause after the last key: its journal.
            std::thread::sleep(Duration::from_millis(1500));
            let after = service.journal_write_counts();
            let (installed, locked) = (after.0 - before.0, after.1 - before.1);
            let journal: serde_json::Value = fs::read_dir(p.data().join("recovery"))
                .unwrap()
                .flatten()
                .flat_map(|d| fs::read_dir(d.path()).unwrap().flatten())
                .find(|e| e.file_name().to_string_lossy().starts_with("live-"))
                .map(|e| serde_json::from_slice(&fs::read(e.path()).unwrap()).unwrap())
                .expect("a live journal");
            assert_eq!(journal["revision"], docs.info(&id).unwrap().revision, "the last journal holds the last edit");
            loop_task.abort();
            perf(
                "workspace.journal_writes_while_typing",
                json!({ "bytes": size, "size": size_label(size + KIB), "keys_per_second": keys_per_second, "seconds": 8 }),
                &[],
                json!({
                    "keys": keys,
                    "journal_writes": installed + locked,
                    "installed_staged": installed,
                    "written_under_lock": locked,
                }),
            );
        }
    }
}

// ----- current() versus open_root() ---------------------------------------------------

/// With recovery entries waiting for the user's decision: the cost of asking
/// for the workspace info (which lists them) against asking only for the
/// root, and of an exact (agent) save of another file.
#[test]
#[ignore = "perf"]
fn current_versus_open_root() {
    let (entries, size) = match scale_name().as_str() {
        "large" | "stress" => (5, 5 * MIB),
        _ => (2, 100 * KIB),
    };
    let names: Vec<String> = (0..entries).map(|i| format!("unsaved{i}.txt")).collect();
    let body = text_of(size, false, 5);
    let mut files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), body.as_str())).collect();
    files.push(("other.txt", "other\n"));
    let p = project(&files);
    let mut ids = HashSet::new();
    for name in &names {
        let id = p.service.open_document(&wp(name)).unwrap().state.info.id;
        type_char(p.service.documents(), &id, 0);
        ids.insert(id);
    }
    p.service.journal_documents(&ids);
    let journal_bytes = dir_bytes(&p.data().join("recovery"));
    // The process is killed; the next session finds the journals.
    let service = p.restart();
    let scale = json!({ "pending_entries": entries, "entry_bytes": size });

    let mut samples = Vec::new();
    let mut last = Work::default();
    for i in 0..=SAMPLES {
        let (ms, work, info) = measure(size, || service.current().unwrap());
        assert_eq!(info.recovery.len(), entries);
        if i > 0 {
            samples.push(ms);
        }
        last = work;
    }
    perf(
        "workspace.current",
        scale.clone(),
        &samples,
        json!({ "journal_bytes": journal_bytes, "large_allocs": last.large_allocs, "alloc_bytes": last.alloc_bytes }),
    );

    let mut samples = Vec::new();
    let mut last = Work::default();
    for i in 0..=SAMPLES {
        let (ms, work, root) = measure(size, || service.open_root());
        assert!(root.is_some());
        if i > 0 {
            samples.push(ms);
        }
        last = work;
    }
    perf(
        "workspace.open_root",
        scale.clone(),
        &samples,
        json!({ "large_allocs": last.large_allocs, "alloc_bytes": last.alloc_bytes }),
    );

    let other = service.open_document(&wp("other.txt")).unwrap().state.info.id;
    let mut samples = Vec::new();
    let mut last = Work::default();
    for i in 0..=SAMPLES as u32 {
        type_char(service.documents(), &other, i);
        let revision = service.documents().info(&other).unwrap().revision;
        let (ms, work, saved) = measure(size, || service.save_document_exact(&other, revision).unwrap());
        assert_eq!(saved.revision, revision);
        if i > 0 {
            samples.push(ms);
        }
        last = work;
    }
    perf(
        "workspace.exact_save_with_pending_recovery",
        scale,
        &samples,
        json!({ "large_allocs": last.large_allocs, "alloc_bytes": last.alloc_bytes }),
    );
}

// ----- retained history (WS-4) ------------------------------------------------------------

/// A clean open document whose file is rewritten in full again and again by
/// another program: the text retained by its undo and rebase histories.
#[test]
#[ignore = "perf"]
fn retained_history_after_external_rewrites() {
    let (size, rewrites) = match scale_name().as_str() {
        "large" | "stress" => (2 * MIB, 50),
        _ => (MIB, 10),
    };
    let p = project(&[("generated.csv", &text_of(size, false, 100))]);
    let id = p.service.open_document(&wp("generated.csv")).unwrap().state.info.id;
    let mut samples = Vec::new();
    for i in 0..rewrites {
        fs::write(p.root().join("generated.csv"), text_of(size, false, 200 + i as u64)).unwrap();
        let (ms, _, ()) = measure(size, || p.service.check_document(&id));
        samples.push(ms);
    }
    let docs = p.service.documents();
    assert_eq!(docs.info(&id).unwrap().revision, 1 + rewrites as u64, "every rewrite was loaded");
    let retained = docs.retained(&id).unwrap();
    perf(
        "documents.retained_after_rewrites",
        json!({ "bytes": size, "rewrites": rewrites }),
        &samples,
        json!({
            "text_bytes": size,
            "undo_groups": retained.undo_groups,
            "undo_bytes": retained.undo_bytes,
            "history_entries": retained.history_entries,
            "history_bytes": retained.history_bytes,
            "retained_bytes": retained.undo_bytes + retained.redo_bytes + retained.history_bytes,
        }),
    );
}
