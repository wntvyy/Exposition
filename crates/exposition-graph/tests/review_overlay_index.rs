//! Independent review of "overlays resolve against the index a pass uses":
//! the overlay of an unsaved buffer must resolve its links against the
//! labels and LaTeX main files a fresh index of the project has, in a new
//! session, after a project switch, and while a pass started elsewhere holds
//! the session's index; and it must cost what an overlay costs otherwise.
//!
//! A pass "started elsewhere" is simulated deterministically: this thread
//! holds the reconciliation state through `reconcile_after` (as a rename
//! from an IPC thread does) while another thread asks for the overlay. An
//! overlay asked for meanwhile is deferred (`overlay::DEFERRED`) and asked
//! for again until the pass is over, as the driver does (`requeued`); the
//! tests compare what it then resolves to.
//!
//! Every finding is fixed and every test but the benchmark is enabled.

#![allow(unsafe_code)]

mod support;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, ReconcileReport, Scope, overlay};
use exposition_graph::*;
use support::{EDITED_FILE, Scale, SqlWork, count_sql, generate, take};

fn wp(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn folder(name: &str) -> Project {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(name)).unwrap();
    let root = std::fs::canonicalize(dir.path().join(name)).unwrap();
    Project { _dir: dir, root }
}

impl Project {
    fn write(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
}

fn service() -> (tempfile::TempDir, Arc<GraphService>) {
    let data = tempfile::tempdir().unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    (data, svc)
}

#[track_caller]
fn attach(svc: &GraphService, root: &Path) {
    let status = svc.attach(root);
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
}

fn full(svc: &GraphService) {
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
}

fn reattach(svc: &GraphService, root: &Path) {
    svc.detach();
    attach(svc, root);
}

thread_local! {
    /// Told when an overlay asked for on this thread is first deferred (see
    /// `during_a_pass_with`).
    static ON_DEFERRAL: RefCell<Option<std::sync::mpsc::Sender<()>>> = const { RefCell::new(None) };
    /// Overlays deferred on this thread.
    static DEFERRALS: Cell<u32> = const { Cell::new(0) };
    /// Time spent in `overlay::compute` on this thread (not waiting).
    static COMPUTING: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

/// The overlay as the driver gets it: computed now or, while a pass holds
/// the session's index, deferred and asked for again until the pass is over.
fn requeued(svc: &GraphService, path: &str, revision: u64, text: &str) -> Option<overlay::OverlayEntry> {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let started = Instant::now();
        let result = overlay::compute(svc, &wp(path), revision, text);
        COMPUTING.with(|c| c.set(c.get() + started.elapsed()));
        match result {
            Err(e) if overlay::is_deferred(&e) => {
                DEFERRALS.with(|d| d.set(d.get() + 1));
                if let Some(tx) = ON_DEFERRAL.with(|slot| slot.borrow_mut().take()) {
                    let _ = tx.send(());
                }
                assert!(Instant::now() < deadline, "the overlay of {path} stays deferred");
                std::thread::sleep(Duration::from_millis(2));
            }
            other => return other.unwrap(),
        }
    }
}

/// What an overlay says about the buffer's links: the coverage (unresolved
/// count and the plain-words detail) and the relations it adds, by kind and
/// target id (the ids of one database are stable across its sessions).
fn summary(svc: &GraphService, path: &str, text: &str) -> String {
    let entry = requeued(svc, path, 7, text).expect("a known resource");
    summarize(&entry)
}

fn summarize(entry: &overlay::OverlayEntry) -> String {
    let c = entry.coverage.clone().expect("the overlay has coverage");
    let mut added: Vec<String> = entry.added.iter().map(|r| format!("{} -> {}", r.kind.as_str(), r.to)).collect();
    added.sort();
    format!("unresolved {} ({}); adds {:?}", c.unresolved, c.detail.unwrap_or_default(), added)
}

/// Run `f` on another thread while this thread holds the reconciliation
/// state, as a pass started elsewhere (a rename, an applied proposal) does:
/// `change` runs first under the hold, then `f` starts; the hold ends (with
/// the pass `options`, if any) once an overlay `f` asked for was deferred.
/// `f` gets its overlays as the driver does (`requeued`): computed once the
/// pass is over. Returns what `f` returned and the report of the pass.
fn during_a_pass_with<T: Send>(
    svc: &GraphService,
    change: impl FnOnce(),
    options: Option<ReconcileOptions>,
    f: impl FnOnce() -> T + Send,
) -> (T, Option<GraphResult<ReconcileReport>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|s| {
        let (handle, report) = svc.reconcile_after(|| {
            change();
            let handle = s.spawn(move || {
                ON_DEFERRAL.with(|slot| *slot.borrow_mut() = Some(tx));
                let before = DEFERRALS.with(Cell::get);
                let out = f();
                ON_DEFERRAL.with(|slot| slot.borrow_mut().take());
                (out, DEFERRALS.with(Cell::get) - before)
            });
            // Held until the thread's first overlay was deferred (or the
            // thread ended without one).
            let _ = rx.recv();
            (handle, options)
        });
        let (out, deferred) = handle.join().unwrap();
        assert!(deferred > 0, "an overlay asked for while a pass holds the session's index is deferred");
        (out, report)
    })
}

fn during_a_pass<T: Send>(svc: &GraphService, f: impl FnOnce() -> T + Send) -> T {
    let (out, report) = during_a_pass_with(svc, || {}, None, f);
    assert!(report.is_none());
    out
}

fn exec(svc: &GraphService, statements: &str) {
    svc.with_store(|s| {
        s.connection().execute_batch(statements).map_err(store::StoreError::from)?;
        Ok(())
    })
    .unwrap();
}

/// A project whose buffers need the label index (a section label is no
/// entity) and the LaTeX main files (an `\input` looked up from the main
/// file's folder).
fn labelled_project(name: &str) -> Project {
    let p = folder(name);
    p.write("notes/a.tex", "\\section{Intro}\\label{sec:intro}\nText.\n");
    p.write("notes/r.tex", "See \\ref{sec:intro}.\n");
    p.write("paper/main.tex", "\\documentclass{article}\n\\begin{document}\n\\input{ch/one}\n\\end{document}\n");
    p.write("paper/ch/one.tex", "One.\n");
    p.write("paper/ch/two.tex", "Two.\n");
    p
}

const R_BUFFER: &str = "See \\ref{sec:intro} and \\ref{sec:nowhere}.\n";
const ONE_BUFFER: &str = "One.\n\\input{ch/two}\n";

/// The overlays of both buffers.
fn both(svc: &GraphService) -> (String, String) {
    (summary(svc, "notes/r.tex", R_BUFFER), summary(svc, "paper/ch/one.tex", ONE_BUFFER))
}

// ----- correctness: the paths the change adds -----

/// While a pass started elsewhere holds the session's index, an overlay is
/// deferred, then computed once the pass is over: the same result as in the
/// session that indexed the project, in that session and in a new one. (On
/// the base the overlay resolved against an empty index.)
#[test]
fn an_overlay_during_a_pass_started_elsewhere_resolves_as_a_fresh_index_does() {
    let p = labelled_project("project");
    let (_data, svc) = service();
    attach(&svc, &p.root);
    full(&svc);
    let indexed = both(&svc);
    assert!(indexed.0.starts_with("unresolved 1 "), "only sec:nowhere is missing: {}", indexed.0);
    assert!(indexed.1.starts_with("unresolved 0 "), "ch/two is found from the main file's folder: {}", indexed.1);

    let held = during_a_pass(&svc, || both(&svc));
    assert_eq!(held, indexed, "in the session that indexed the project, while a pass holds its index");

    reattach(&svc, &p.root);
    let held_new = during_a_pass(&svc, || both(&svc));
    assert_eq!(held_new, indexed, "in a new session, while a pass holds its index");
    assert_eq!(both(&svc), indexed, "in a new session, before its first pass");
}

/// A project switch keeps nothing of the previous project's index: a buffer
/// of project B that refers to a label only project A declares has it
/// unresolved, in the new session, while a pass holds the state across the
/// switch, and after a pass that started in A's session runs on B. (Passes
/// on the change; fails on the base, where the new session's overlay used
/// an empty index.)
#[test]
fn an_overlay_after_a_project_switch_sees_none_of_the_previous_projects_labels() {
    let a = folder("alpha");
    a.write("notes/a.tex", "\\section{A}\\label{sec:only-a}\n");
    a.write("paper/main.tex", "\\documentclass{article}\n\\begin{document}\n\\end{document}\n");
    a.write("paper/ch/two.tex", "Two in A.\n");
    let b = folder("beta");
    b.write("notes/b.tex", "\\section{B}\\label{sec:b}\n");
    b.write("notes/r.tex", "See \\ref{sec:b}.\n");
    b.write("paper/ch/one.tex", "One.\n");
    b.write("paper/ch/two.tex", "Two.\n");
    let buffer = "See \\ref{sec:only-a} and \\ref{sec:b}.\n";
    // `ch/two` from paper/ch/one.tex resolves only from the folder of a
    // LaTeX main file, and project B has none (A's is paper/main.tex).
    let one = "One.\n\\input{ch/two}\n";

    let (_data, svc) = service();
    attach(&svc, &b.root);
    full(&svc);
    let expected = (summary(&svc, "notes/r.tex", buffer), summary(&svc, "paper/ch/one.tex", one));
    assert!(expected.0.starts_with("unresolved 1 "), "{}", expected.0);
    assert!(expected.1.starts_with("unresolved 1 "), "{}", expected.1);
    let in_b = |svc: &GraphService| (summary(svc, "notes/r.tex", buffer), summary(svc, "paper/ch/one.tex", one));

    attach(&svc, &a.root);
    full(&svc);
    attach(&svc, &b.root);
    assert_eq!(in_b(&svc), expected, "after switching from A, before B's first pass");

    attach(&svc, &a.root);
    full(&svc);
    // The switch happens while this thread holds A's reconciliation state.
    let (held, report) = during_a_pass_with(
        &svc,
        || attach(&svc, &b.root),
        Some(ReconcileOptions { scope: Scope::Paths(vec![wp("notes/b.tex")]), force: true }),
        || in_b(&svc),
    );
    assert_eq!(held, expected, "switched while a pass of A's session held the state");
    report.expect("a pass ran").expect("the pass started in A's session runs on B");
    assert_eq!(in_b(&svc), expected, "after that pass");
}

// ----- correctness: the session's index as a cache of the stored one -----

/// A read of the stored index that fails must not leave the session's index
/// marked as read and empty (every later overlay and pass of the session
/// would resolve against no labels, although a fresh index, after the store
/// reads again, has them; the pass right after the failed overlay would
/// store `sec:intro` as unresolved). Rule 5 of docs/performance.md: "not
/// loaded" must never read as "none". The read failure is injected by
/// damaging one stored index row and repairing it (a transient store error
/// has the same effect). The index is marked read only after both reads
/// succeeded.
#[test]
fn a_failed_read_of_the_index_is_not_kept_as_the_sessions_index() {
    let p = labelled_project("project");
    let (_data, svc) = service();
    attach(&svc, &p.root);
    full(&svc);
    let indexed = summary(&svc, "notes/r.tex", R_BUFFER);
    assert!(indexed.starts_with("unresolved 1 "), "{indexed}");

    reattach(&svc, &p.root);
    exec(&svc, "UPDATE source_index SET path = '../outside.tex' WHERE path = 'notes/a.tex'");
    let failed = overlay::compute(&svc, &wp("notes/r.tex"), 7, R_BUFFER);
    let pass = svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![wp("notes/r.tex")]), force: true });
    exec(&svc, "UPDATE source_index SET path = 'notes/a.tex' WHERE path = '../outside.tex'");
    let after = summary(&svc, "notes/r.tex", R_BUFFER);
    assert_eq!(
        after,
        indexed,
        "the store reads again; the overlay must see sec:intro (the failed overlay returned {:?}, the failed pass {:?})",
        failed.as_ref().map(|o| o.as_ref().map(summarize)),
        pass.as_ref().map(|r| r.unresolved),
    );
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![wp("notes/r.tex")]), force: true }).unwrap();
    let stored = svc.with_store(|s| Ok(s.coverage(&wp("notes/r.tex"))?)).unwrap().unwrap();
    assert_eq!(stored.unresolved, 0, "a pass after the repair resolves sec:intro: {stored:?}");
}

/// A file whose stored index record is too large keeps its declarations and
/// drops its labels from the store (`bounded_index_json`). The session's
/// index takes a file's labels from the record written, so it stays a cache
/// of the stored one: an overlay resolves a reference to such a label the
/// same way in the session that indexed the file, after the project is
/// attached again, and once a pass that held the index is over.
#[test]
fn the_session_keeps_no_label_the_stored_index_dropped() {
    let p = folder("project");
    // 19,500 labels of 230 characters: about 4.5 MB of labels, over the
    // 4 MiB bound of one stored index record; the file is under the 8 MiB
    // extraction bound.
    let mut big = String::from("\\section{Big}\\label{sec:big}\n");
    for i in 0..19_500 {
        big.push_str(&format!("\\label{{x{i:0>229}}}\n"));
    }
    assert!(big.len() < 8 * 1024 * 1024);
    p.write("big/big.tex", &big);
    p.write("notes/r.tex", "See \\ref{sec:big}.\n");
    let (_data, svc) = service();
    attach(&svc, &p.root);
    full(&svc);
    let stored_has_label = svc
        .with_store(|s| Ok(s.source_index(&wp("big/big.tex"))?))
        .unwrap()
        .map(|r| r.body.contains("sec:big"))
        .unwrap_or(false);
    let buffer = "See \\ref{sec:big}. Typed.\n";
    let in_session = summary(&svc, "notes/r.tex", buffer);
    let held = during_a_pass(&svc, || summary(&svc, "notes/r.tex", buffer));
    reattach(&svc, &p.root);
    let fresh = summary(&svc, "notes/r.tex", buffer);
    assert_eq!(
        (in_session.as_str(), held.as_str()),
        (fresh.as_str(), fresh.as_str()),
        "the same buffer, in the session that indexed big.tex / while a pass holds its index / in a new session \
         (the stored record keeps the label: {stored_has_label})"
    );
}

// ----- cost -----

/// The SQL work of one overlay of `EDITED_FILE` (after a warm-up), computed
/// directly and asked for while a pass started elsewhere holds the session's
/// index (deferred, then computed once the pass is over; the work of both
/// counted on the asking thread).
fn overlay_work(svc: &GraphService, root: &Path) -> (SqlWork, SqlWork) {
    let original = std::fs::read_to_string(root.join(EDITED_FILE)).unwrap();
    let typed = |i: u64| {
        let text = format!("{original}\n% typing {i}\n");
        requeued(svc, EDITED_FILE, 100 + i, &text).expect("a known resource");
    };
    typed(0);
    take();
    typed(1);
    let direct = take();
    during_a_pass(svc, || typed(2));
    let held = during_a_pass(svc, || {
        take();
        typed(3);
        take()
    });
    (direct, held)
}

fn generated(files: usize, big_file_declarations: usize) -> (Project, tempfile::TempDir, Arc<GraphService>) {
    let p = folder("project");
    generate(&p.root, files, big_file_declarations);
    let (data, svc) = service();
    attach(&svc, &p.root);
    full(&svc);
    count_sql(&svc);
    (p, data, svc)
}

/// Rows, bytes of bodies and bytes of awaited-target lists of the stored index.
fn index_size(svc: &GraphService) -> (i64, i64, i64) {
    svc.with_store(|s| {
        Ok(s.connection()
            .query_row(
                "SELECT count(*), coalesce(sum(length(body)), 0), coalesce(sum(length(wanted)), 0) FROM source_index",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(store::StoreError::from)?)
    })
    .unwrap()
}

/// docs/performance.md: an overlay is "scoped to the document", and
/// `cost_bounds` asserts that it costs the same in a larger project. That
/// holds for an overlay asked for while a pass started elsewhere holds the
/// session's index too: it reads nothing until the pass is over (the
/// reviewed change read every stored index record and awaited-target list
/// for it instead, under the store lock: at the `large` perf scale 35,099
/// rows and 248,460 VM steps instead of 64 and 2,111; see
/// `perf_overlay_during_a_pass`). Same sizes and slack as `cost_bounds`.
#[test]
fn an_overlay_during_a_pass_costs_the_document_not_the_project() {
    let (sp, _sd, small) = generated(320, 20);
    let (lp, _ld, large) = generated(960, 200);
    let (s_direct, s_held) = overlay_work(&small, &sp.root);
    let (l_direct, l_held) = overlay_work(&large, &lp.root);
    eprintln!(
        "overlay work (statements/rows/vm steps/full-scan steps)\n small {:?}: direct {s_direct:?}, during a pass {s_held:?}\n large {:?}: direct {l_direct:?}, during a pass {l_held:?}",
        index_size(&small),
        index_size(&large)
    );
    let slack = 2_000;
    let same = |s: SqlWork, l: SqlWork| {
        l.statements <= s.statements + 4
            && l.rows <= s.rows + slack / 10
            && l.vm_steps <= s.vm_steps + slack
            && l.fullscan_steps <= s.fullscan_steps + slack / 10
    };
    assert!(same(s_direct, l_direct), "direct: small {s_direct:?}, large {l_direct:?}");
    assert!(
        same(s_held, l_held),
        "during a pass the overlay costs more in the larger project:\n small {s_held:?}\n large {l_held:?}"
    );
}

/// Benchmark (counters and, as evidence only, times) of the overlay of a
/// small file at `EXPOSITION_PERF_SCALE`, directly and asked for while a
/// pass started elsewhere holds the session's index. The SQL work is that
/// of the whole request (the deferred attempts and the computation once the
/// pass is over); the times are those spent in `overlay::compute`, not the
/// wait for the pass.
#[test]
#[ignore = "perf"]
fn perf_overlay_during_a_pass() {
    let scale = Scale::from_env();
    let (p, _d, svc) = generated(scale.files, scale.big_file_declarations);
    let (rows, body_bytes, wanted_bytes) = index_size(&svc);
    let original = std::fs::read_to_string(p.root.join(EDITED_FILE)).unwrap();
    let typed = |i: u64| {
        let text = format!("{original}\n% typing {i}\n");
        take();
        COMPUTING.with(|c| c.set(Duration::ZERO));
        let deferred = DEFERRALS.with(Cell::get);
        requeued(&svc, EDITED_FILE, 100 + i, &text).expect("a known resource");
        (COMPUTING.with(Cell::get).as_secs_f64() * 1000.0, take(), DEFERRALS.with(Cell::get) - deferred)
    };
    for (bench, held) in [("review.overlay_direct", false), ("review.overlay_during_pass", true)] {
        let one = |i: u64| if held { during_a_pass(&svc, || typed(i)) } else { typed(i) };
        one(0);
        let mut samples = Vec::new();
        let mut work = SqlWork::default();
        let mut deferred = 0;
        for i in 1..=5u64 {
            let (ms, w, d) = one(i);
            samples.push(ms);
            work = w;
            deferred = d;
        }
        support::report(
            bench,
            &scale,
            &samples,
            &format!(
                "{{\"sql\":{},\"deferred\":{deferred},\"indexRows\":{rows},\"indexBodyBytes\":{body_bytes},\"wantedBytes\":{wanted_bytes}}}",
                work.json()
            ),
        );
    }
}
