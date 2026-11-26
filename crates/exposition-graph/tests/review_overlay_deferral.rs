//! Independent re-review of the overlay deferral: `overlay::compute` defers
//! while the reconciliation state is held, the driver asks again on the next
//! graph event or after `OVERLAY_RETRY`, and recomputes the overlays of open
//! unsaved buffers after passes it did not run. Driver tests run the real
//! workspace and document services; the cost of waiting is counted (SQL
//! statements on the graph's connection from every thread, graph events),
//! never timed. Also the two limits the change states: labels of an oversize
//! index record within one pass, and `latex_mains()` while the state is held.
//!
//! Tests that fail on the reviewed change are `#[ignore = "review finding:
//! ..."]`.

#![allow(unsafe_code)]

mod support;

use std::ffi::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::*;
use exposition_workspace::{DocumentService, WorkspaceService};
use rusqlite::ffi;
use support::{EDITED_FILE, SqlWork, count_sql, generate, take};

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

// ----- counters on every thread, one per connection -----

unsafe extern "C" fn count_statement(event: c_uint, ctx: *mut c_void, _p: *mut c_void, _x: *mut c_void) -> c_int {
    if event == ffi::SQLITE_TRACE_STMT {
        // SAFETY: `ctx` is the leaked counter `count_all_statements`
        // registered with this connection; it is never freed.
        let counter = unsafe { &*ctx.cast::<AtomicU64>() };
        counter.fetch_add(1, Ordering::SeqCst);
    }
    0
}

/// Count the statements this graph's connection starts, on any thread (the
/// driver's included), from now on. Each test gets its own counter: the
/// tests in this file run in parallel unless RUST_TEST_THREADS=1 (CI does
/// not set it), and one process-wide counter added the passes of the
/// back-to-back test to the deferral test's count. The product never traces.
fn count_all_statements(g: &GraphService) -> &'static AtomicU64 {
    let counter: &'static AtomicU64 = Box::leak(Box::new(AtomicU64::new(0)));
    g.with_store(|store| {
        // SAFETY: the connection is alive for the call; the callback only
        // increments `counter`, which is leaked and so outlives the connection.
        unsafe {
            ffi::sqlite3_trace_v2(
                store.connection().handle(),
                ffi::SQLITE_TRACE_STMT,
                Some(count_statement),
                std::ptr::from_ref(counter).cast_mut().cast::<c_void>(),
            );
        }
        Ok(())
    })
    .unwrap();
    counter
}

fn events(rx: &mut tokio::sync::broadcast::Receiver<GraphEvent>) -> u64 {
    let mut n = 0;
    loop {
        match rx.try_recv() {
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => n += 1,
            Err(_) => return n,
        }
    }
}

// ----- the driver with real services (as in tests/driver.rs) -----

async fn wait_for<T>(what: &str, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

struct Setup {
    _data: tempfile::TempDir,
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    workspace: Arc<WorkspaceService>,
    graph: Arc<GraphService>,
    reconciler: reconcile::ReconcilerHandle,
}

async fn setup(files: &[(&str, &str)]) -> Setup {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (rel, body) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let documents = Arc::new(DocumentService::new());
    let workspace = WorkspaceService::new(documents, data.path().join("app"));
    tokio::spawn(Arc::clone(&workspace).run());
    let graph = GraphService::new(GraphConfig { data_dir: data.path().join("app") });
    let reconciler = reconcile::spawn(Arc::clone(&graph), Arc::clone(&workspace));
    workspace.open_workspace(&root).unwrap();
    graph.attach(&root);
    Setup { _data: data, _dir: dir, root, workspace, graph, reconciler }
}

fn synchronized(g: &GraphService) -> bool {
    g.freshness().state == SyncState::Synchronized
}

fn type_into(s: &Setup, doc: &DocumentId, base_revision: u64, at: u32, text: &str) -> u64 {
    let outcome = s
        .workspace
        .documents()
        .apply(TransactionRequest {
            id: TransactionId(format!("t-{base_revision}-{at}")),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: doc.clone(),
                base_revision,
                patches: vec![TextPatch::insert(at, text)],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    outcome.applied[0].revision
}

const LABELLED: &[(&str, &str)] = &[
    ("notes/a.tex", "\\section{Intro}\\label{sec:intro}\nText.\n"),
    ("notes/r.tex", "See.\n"),
    ("notes/s.tex", "So.\n"),
];
const REFERENCES: &str = " \\ref{sec:intro} and \\ref{sec:nowhere}";

async fn open(s: &Setup, path: &str) -> (DocumentId, u64) {
    let g = &s.graph;
    wait_for("initial reconciliation", Duration::from_secs(20), || {
        (synchronized(g) && g.resource(&wp(path)).ok().flatten().is_some()).then_some(())
    })
    .await;
    let opened = s.workspace.open_document(&wp(path)).unwrap();
    (opened.state.info.id.clone(), opened.state.info.revision)
}

/// Until the graph is synchronized and no overlay changes for a while (every
/// computation the setup caused is over).
async fn quiet(g: &GraphService) {
    let stamp = |g: &GraphService| {
        g.freshness().overlay.iter().map(|o| format!("{}@{}", o.path, o.revision)).collect::<Vec<_>>().join(",")
            + &["r", "s", "a"]
                .iter()
                .filter_map(|n| g.overlay(&wp(&format!("notes/{n}.tex"))).map(|o| o.computed_at_ms.to_string()))
                .collect::<Vec<_>>()
                .join(",")
    };
    let mut last = stamp(g);
    loop {
        tokio::time::sleep(Duration::from_millis(600)).await;
        let now = stamp(g);
        if now == last && synchronized(g) {
            return;
        }
        last = now;
    }
}

/// A holder of the reconciliation state on its own thread (a pass started
/// elsewhere that has not finished): returns once it holds the state.
struct Holder {
    release: mpsc::Sender<bool>,
    thread: std::thread::JoinHandle<()>,
}

fn hold(g: &Arc<GraphService>) -> Holder {
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release, release_rx) = mpsc::channel::<bool>();
    let g = Arc::clone(g);
    let thread = std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            g.reconcile_after(|| {
                held_tx.send(()).unwrap();
                if release_rx.recv().unwrap() {
                    panic!("a pass panics while it holds the reconciliation state");
                }
                ((), None)
            })
        }));
    });
    held_rx.recv().unwrap();
    Holder { release, thread }
}

impl Holder {
    fn release(self, panic: bool) {
        self.release.send(panic).unwrap();
        self.thread.join().unwrap();
    }
}

fn overlay_at(g: &GraphService, path: &str, revision: u64) -> Option<Arc<reconcile::OverlayEntry>> {
    g.overlay(&wp(path)).filter(|o| o.revision == revision)
}

/// While a pass started elsewhere holds the state, a typed buffer's overlay
/// is asked for after its quiet period and again every `OVERLAY_RETRY`:
/// over 1.5 s (about 13 retries) nothing is read from the graph and no event
/// is sent; once the holder lets go, the overlay is computed against the
/// session's index. (Passes on the change: 0 statements and 0 events while
/// held; 14 statements and 1 event for the computation once released.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deferred_overlay_reads_nothing_and_sends_nothing_while_the_state_is_held() {
    let s = setup(LABELLED).await;
    let g = &s.graph;
    let (doc, revision) = open(&s, "notes/r.tex").await;
    quiet(g).await;
    let statements = count_all_statements(g);
    let mut rx = g.subscribe();
    let holder = hold(g);
    let typed = type_into(&s, &doc, revision, 3, REFERENCES);
    statements.store(0, Ordering::SeqCst);
    events(&mut rx);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let held = (statements.load(Ordering::SeqCst), events(&mut rx), overlay_at(g, "notes/r.tex", typed).is_some());
    holder.release(false);
    let overlay =
        wait_for("the overlay once released", Duration::from_secs(10), || overlay_at(g, "notes/r.tex", typed)).await;
    let after = (statements.load(Ordering::SeqCst) - held.0, events(&mut rx));
    eprintln!(
        "held 1.5 s: statements {}, events {}, overlay {}; after release: statements {}, events {}",
        held.0, held.1, held.2, after.0, after.1
    );
    assert_eq!(held, (0, 0, false), "statements, events and an overlay while the state is held");
    assert_eq!(overlay.coverage.as_ref().map(|c| c.unresolved), Some(1), "only sec:nowhere is missing");
    s.reconciler.shutdown();
}

/// Passes started elsewhere one after another (no gap between them) keep the
/// state held nearly all the time. The overlay of a buffer typed meanwhile
/// must be computed once they stop, and the work they cause the driver is
/// counted (statements and events per pass). (Passes on the change. Seen:
/// 117 passes in 2 s, 11.0 statements and 2.0 events per pass, which is the
/// passes' own work; the overlay was not computed during the burst, only
/// after it: a burst with no gap between passes defers it for its whole
/// length.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overlay_deferred_by_back_to_back_passes_elsewhere_is_computed_when_they_stop() {
    let s = setup(LABELLED).await;
    let g = &s.graph;
    let (doc, revision) = open(&s, "notes/r.tex").await;
    quiet(g).await;
    let statements = count_all_statements(g);
    let mut rx = g.subscribe();
    let stop = Arc::new(AtomicBool::new(false));
    let passes = Arc::new(AtomicU64::new(0));
    let looping = {
        let (g, stop, passes) = (Arc::clone(g), Arc::clone(&stop), Arc::clone(&passes));
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let (_, report) = g.reconcile_after(|| {
                    std::thread::sleep(Duration::from_millis(20));
                    ((), Some(ReconcileOptions { scope: Scope::Paths(vec![wp("notes/a.tex")]), force: false }))
                });
                report.unwrap().unwrap();
                passes.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let typed = type_into(&s, &doc, revision, 3, REFERENCES);
    statements.store(0, Ordering::SeqCst);
    events(&mut rx);
    let p0 = passes.load(Ordering::SeqCst);
    let mut computed_during = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        computed_during |= overlay_at(g, "notes/r.tex", typed).is_some();
    }
    let burst = (passes.load(Ordering::SeqCst) - p0, statements.load(Ordering::SeqCst), events(&mut rx));
    stop.store(true, Ordering::SeqCst);
    looping.join().unwrap();
    let overlay = wait_for("the overlay once the passes stopped", Duration::from_secs(10), || {
        overlay_at(g, "notes/r.tex", typed)
    })
    .await;
    eprintln!(
        "burst of 2 s: {} passes, {} statements ({:.1} per pass), {} events ({:.1} per pass), overlay computed during the burst: {computed_during}",
        burst.0,
        burst.1,
        burst.1 as f64 / burst.0.max(1) as f64,
        burst.2,
        burst.2 as f64 / burst.0.max(1) as f64
    );
    assert_eq!(overlay.coverage.as_ref().map(|c| c.unresolved), Some(1));
    s.reconciler.shutdown();
}

/// A pass that panics while it holds the state leaves the mutex poisoned,
/// not held: the deferred overlay is computed. A pass that held the state
/// across a detach and a new attach leaves no overlay deferred for the
/// earlier session, and the buffer is overlaid in the new one. (Passes on
/// the change.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_holder_that_panics_or_outlives_a_detach_leaves_no_overlay_deferred() {
    let s = setup(LABELLED).await;
    let g = &s.graph;
    let (doc, revision) = open(&s, "notes/r.tex").await;
    quiet(g).await;

    let holder = hold(g);
    let typed = type_into(&s, &doc, revision, 3, REFERENCES);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(overlay_at(g, "notes/r.tex", typed).is_none());
    holder.release(true);
    let overlay = wait_for("the overlay after a panicking holder", Duration::from_secs(10), || {
        overlay_at(g, "notes/r.tex", typed)
    })
    .await;
    assert_eq!(overlay.coverage.as_ref().map(|c| c.unresolved), Some(1));

    let holder = hold(g);
    let typed2 = type_into(&s, &doc, typed, 3, " \\ref{sec:intro}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    g.detach();
    tokio::time::sleep(Duration::from_millis(300)).await;
    holder.release(false);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(g.overlay(&wp("notes/r.tex")).is_none(), "no overlay while detached");
    assert!(matches!(g.attach(&s.root).state, GraphState::Attached));
    let overlay =
        wait_for("the overlay in the new session", Duration::from_secs(20), || overlay_at(g, "notes/r.tex", typed2))
            .await;
    assert_eq!(overlay.coverage.as_ref().map(|c| c.unresolved), Some(1));
    s.reconciler.shutdown();
}

/// After a pass started elsewhere, only open buffers with unsaved changes
/// are overlaid again: a buffer saved before the pass, and one deferred
/// while the state was held and closed before it was let go, get none.
/// (Passes on the change.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_and_closed_buffers_get_no_overlay_after_a_pass_elsewhere() {
    let s = setup(LABELLED).await;
    let g = &s.graph;
    let (r, r_rev) = open(&s, "notes/r.tex").await;
    let (sdoc, s_rev) = open(&s, "notes/s.tex").await;
    let typed = type_into(&s, &r, r_rev, 3, REFERENCES);
    wait_for("the overlay of r", Duration::from_secs(10), || overlay_at(g, "notes/r.tex", typed)).await;
    s.workspace.save_document(&r, false).unwrap();
    wait_for("r's overlay dropped", Duration::from_secs(10), || g.overlay(&wp("notes/r.tex")).is_none().then_some(()))
        .await;
    quiet(g).await;

    let holder = hold(g);
    let s_typed = type_into(&s, &sdoc, s_rev, 2, " \\ref{sec:intro}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    s.workspace.close_document(&sdoc).unwrap();
    holder.release(false);
    let other = Arc::clone(g);
    tokio::task::spawn_blocking(move || {
        other.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![wp("notes/a.tex")]), force: true }).unwrap()
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(g.overlay(&wp("notes/r.tex")).is_none(), "the saved buffer is not overlaid again");
    assert!(g.overlay(&wp("notes/s.tex")).is_none(), "the closed buffer (revision {s_typed}) is not overlaid");
    s.reconciler.shutdown();
}

// ----- the limits the change states -----

/// A file whose stored index record dropped its labels for size: a pass that
/// reads it still resolves references to those labels from what it planned
/// (`planned_labels`), a later pass does not. A scoped pass of the referring
/// file in the same session then disagrees with a forced full reindex. On
/// efbbb54 the session's index kept the labels, so the same sequence agreed
/// in the session (and disagreed only after the project was attached again):
/// this test passes on efbbb54 and fails on 851c7bc ((0, 1) against (0, 0)).
#[test]
#[ignore = "review finding: within one session a scoped pass disagrees with a full reindex for labels a stored record dropped"]
fn a_scoped_pass_agrees_with_a_full_reindex_for_labels_a_stored_record_dropped() {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let write = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    let mut big = String::from("\\section{Big}\\label{sec:big}\n");
    for i in 0..19_500 {
        big.push_str(&format!("\\label{{x{i:0>229}}}\n"));
    }
    write("big/big.tex", &big);
    write("notes/r.tex", "See \\ref{sec:big}.\n");
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    assert!(matches!(svc.attach(&root).state, GraphState::Attached));
    let unresolved = || svc.with_store(|s| Ok(s.coverage(&wp("notes/r.tex"))?)).unwrap().unwrap().unresolved;
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    let full_pass = unresolved();
    write("notes/r.tex", "See \\ref{sec:big} again.\n");
    svc.mark_pending(&[wp("notes/r.tex")]).unwrap();
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![wp("notes/r.tex")]), force: false }).unwrap();
    let scoped = unresolved();
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    let reindexed = unresolved();
    assert_eq!(
        (full_pass, scoped),
        (reindexed, reindexed),
        "unresolved in notes/r.tex: after the full pass / after a scoped pass in the same session / after a forced full reindex"
    );
}

/// `latex_mains()` (image references of a LaTeX buffer, asked for 300 ms
/// after each edit by the editor's previews) reads every stored index record
/// while the reconciliation state is held. Same sizes and slack as
/// `cost_bounds`. The code is the same on efbbb54, which fails the same way
/// (2 statements; 251 rows for 320 files, 1,296 for 960).
#[test]
#[ignore = "review finding: image references during a pass read every stored index record (pre-existing)"]
fn image_references_during_a_pass_cost_the_document_not_the_project() {
    let measure = |files: usize| {
        let data = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        generate(&root, files, 20);
        let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
        assert!(matches!(svc.attach(&root).state, GraphState::Attached));
        svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
        count_sql(&svc);
        let text = std::fs::read_to_string(root.join(EDITED_FILE)).unwrap();
        let refs = |svc: &GraphService| {
            let found = reconcile::assets::asset_references(svc, &wp(EDITED_FILE), &text).unwrap();
            assert!(!found.is_empty(), "the chapter includes a figure");
        };
        refs(&svc);
        take();
        refs(&svc);
        let direct = take();
        let (held, _) = svc.reconcile_after(|| {
            let work = std::thread::scope(|s| {
                s.spawn(|| {
                    take();
                    refs(&svc);
                    take()
                })
                .join()
                .unwrap()
            });
            (work, None)
        });
        (direct, held)
    };
    let (s_direct, s_held) = measure(320);
    let (l_direct, l_held) = measure(960);
    eprintln!("image references: small direct {s_direct:?} held {s_held:?}; large direct {l_direct:?} held {l_held:?}");
    let slack = 2_000;
    let same = |s: SqlWork, l: SqlWork| {
        l.statements <= s.statements + 4
            && l.rows <= s.rows + slack / 10
            && l.vm_steps <= s.vm_steps + slack
            && l.fullscan_steps <= s.fullscan_steps + slack / 10
    };
    assert!(same(s_direct, l_direct), "direct: small {s_direct:?}, large {l_direct:?}");
    assert!(same(s_held, l_held), "while the state is held: small {s_held:?}, large {l_held:?}");
}
