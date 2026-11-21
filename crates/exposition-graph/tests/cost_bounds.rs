//! The cost of an edit follows the edit, not the project: each test runs the
//! same operation against a smaller and a larger graph and compares the SQL
//! work (statements, rows returned, virtual-machine steps, full-scan steps)
//! SQLite reports. No wall-clock thresholds.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope, overlay};
use exposition_graph::*;
use support::*;

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn one(p: &str) -> ReconcileOptions {
    ReconcileOptions { scope: Scope::Paths(vec![path(p)]), force: false }
}

struct Project {
    _data: tempfile::TempDir,
    _dir: tempfile::TempDir,
    root: PathBuf,
    svc: Arc<GraphService>,
}

/// A generated project of about `files` files, indexed, with `lemmas`
/// curated lemmas and three times as many relations; SQL work is counted.
fn project(files: usize, big_file_declarations: usize, lemmas: usize) -> Project {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    generate(&root, files, big_file_declarations);
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    svc.attach(&root);
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    bulk(&svc, lemmas, lemmas * 3, lemmas / 4);
    count_sql(&svc);
    Project { _data: data, _dir: dir, root, svc }
}

/// `work` of the larger graph is the smaller graph's, within `slack` steps
/// and rows (identifiers differ in the two graphs; the amount of data read
/// must not).
fn assert_same_cost(what: &str, small: SqlWork, large: SqlWork, slack: u64) {
    assert!(
        large.statements <= small.statements + 4
            && large.rows <= small.rows + slack / 10
            && large.vm_steps <= small.vm_steps + slack
            && large.fullscan_steps <= small.fullscan_steps + slack / 10,
        "{what} costs more in the larger project:\n small {small:?}\n large {large:?}"
    );
}

fn create_claim(svc: &GraphService, label: &str) {
    let base = svc.revision();
    let (_, out) = svc
        .propose_and_apply(
            Origin::User,
            &ChangeRequest {
                base_revision: base,
                operations: vec![GraphOperation::CreateEntity {
                    temp_id: "t".into(),
                    kind: EntityKind::Claim,
                    roles: vec![],
                    label: label.into(),
                    key: None,
                    attributes: BTreeMap::new(),
                    binding: None,
                }],
                summary: None,
            },
        )
        .unwrap();
    assert!(matches!(out, ApplyOutcome::Applied { .. }), "{out:?}");
}

/// The measurements of one project (each after a warm-up run).
struct Costs {
    status: SqlWork,
    revision: SqlWork,
    commit: SqlWork,
    mark_one: SqlWork,
    unchanged_pass: SqlWork,
    edit_pass: SqlWork,
    overlay: SqlWork,
    inspect: SqlWork,
}

fn costs(p: &Project) -> Costs {
    let svc = &p.svc;
    svc.status();
    let (_, status) = sql_of(|| svc.status());
    let (_, revision) = sql_of(|| (svc.revision(), svc.is_usable()));
    create_claim(svc, "warm-up");
    let (_, commit) = sql_of(|| create_claim(svc, "measured"));
    svc.mark_pending(&[path(EDITED_FILE)]).unwrap();
    let (n, mark_one) = sql_of(|| svc.mark_pending(&[path(EDITED_FILE)]).unwrap());
    assert_eq!(n, 1, "the file's coverage row is marked");
    svc.reconcile(&one(EDITED_FILE)).unwrap();
    // The first status after a write recomputes the counts (one read of the
    // entities and the coverage, whoever asks first); it is taken here so
    // each measurement below is the operation alone.
    svc.status();
    let (_, unchanged_pass) = sql_of(|| svc.reconcile(&one(EDITED_FILE)).unwrap());
    let edited = p.root.join(EDITED_FILE);
    let original = std::fs::read_to_string(&edited).unwrap();
    let edit = |i: usize| {
        let text = format!(
            "{original}\\begin{{corollary}}\\label{{cor:new{i}}}\nFrom Theorem~\\ref{{thm:5}} ({i}).\n\\end{{corollary}}\n"
        );
        std::fs::write(&edited, text).unwrap();
        let report = svc.reconcile(&one(EDITED_FILE)).unwrap();
        assert_eq!(report.read, 1, "{report:?}");
    };
    edit(0);
    svc.status();
    let (_, edit_pass) = sql_of(|| edit(1));
    let typed = |i: u64| {
        let text = format!("{original}\n% typing {i}\n");
        let entry = overlay::compute(svc, &path(EDITED_FILE), 100 + i, &text).unwrap().expect("a known resource");
        svc.set_overlay(svc.generation(), entry);
    };
    typed(0);
    svc.status();
    let (_, overlay) = sql_of(|| typed(1));
    svc.clear_overlay(&path(EDITED_FILE));
    let req = InspectAtSourceRequest { path: path(BIG_FILE), offset: Some(40), revision: None };
    svc.inspect_at_source(&req).unwrap();
    let (found, inspect) = sql_of(|| svc.inspect_at_source(&req).unwrap());
    assert_eq!(found.entities.len(), 1);
    Costs { status, revision, commit, mark_one, unchanged_pass, edit_pass, overlay, inspect }
}

#[test]
fn the_cost_of_an_edit_does_not_grow_with_the_project() {
    let small = project(320, 20, 200);
    let large = project(960, 200, 2_000);
    let large_files = large.svc.status().coverage.resources;
    let small_files = small.svc.status().coverage.resources;
    assert!(large_files > small_files * 2, "{small_files} / {large_files} files");
    let (s, l) = (costs(&small), costs(&large));

    // status(): nothing was written since the previous status, so no table
    // is read; the cheap accessors never compute a status.
    assert!(l.status.statements <= 3 && l.status.fullscan_steps == 0 && l.status.vm_steps < 100, "{:?}", l.status);
    assert!(l.revision.statements <= 1 && l.revision.vm_steps < 50, "{:?}", l.revision);
    assert_same_cost("status()", s.status, l.status, 20);

    // A small committed write does not count the tables.
    assert_same_cost("a small commit", s.commit, l.commit, 200);
    assert_eq!(l.commit.fullscan_steps, s.commit.fullscan_steps, "{:?} / {:?}", s.commit, l.commit);

    // Marking one saved file pending touches that file's row.
    assert!(l.mark_one.fullscan_steps == 0 && l.mark_one.vm_steps < 400, "{:?}", l.mark_one);
    assert_same_cost("mark_pending of one file", s.mark_one, l.mark_one, 50);

    // A one-file pass, unchanged and after a local edit that adds a label.
    assert_same_cost("an unchanged one-file pass", s.unchanged_pass, l.unchanged_pass, 300);
    assert_same_cost("a one-file pass after an edit", s.edit_pass, l.edit_pass, 2_000);
    assert_eq!(l.unchanged_pass.fullscan_steps, 0, "{:?}", l.unchanged_pass);

    // A typing pause in an unsaved buffer.
    assert_same_cost("an overlay", s.overlay, l.overlay, 2_000);

    // The dependency panel at a caret position: the large project's file
    // holds ten times the declarations; only the one at the caret is read.
    assert!(l.inspect.statements <= s.inspect.statements + 2, "{:?} / {:?}", s.inspect, l.inspect);
}

#[test]
fn status_follows_every_write_although_it_is_not_recomputed_between_them() {
    let (_data, _project, root, svc) = attached();
    count_sql(&svc);
    let before = svc.status();
    create_claim(&svc, "One");
    let after = svc.status();
    assert_eq!(after.entities, before.entities + 1);
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(svc.revision(), after.revision);
    // Pending marks change the coverage summary without a revision.
    std::fs::write(root.join("a.md"), "# A\n").unwrap();
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    let indexed = svc.status();
    assert_eq!((indexed.coverage.resources, indexed.coverage.pending), (1, 0));
    svc.mark_pending(&[path("a.md")]).unwrap();
    let marked = svc.status();
    assert_eq!(marked.revision, indexed.revision);
    assert_eq!(marked.coverage.pending, 1, "a mark is visible at once");
    svc.reconcile(&one("a.md")).unwrap();
    assert_eq!(svc.status().coverage.pending, 0);
    // A row written by anything else on the connection is seen too.
    svc.with_store(|s| {
        s.connection()
            .execute("INSERT INTO coverage (path, state, indexed_at) VALUES ('raw.md', 'failed', 0)", [])
            .map_err(exposition_graph::store::StoreError::from)?;
        Ok(())
    })
    .unwrap();
    let raw = svc.status();
    assert_eq!((raw.coverage.resources, raw.coverage.failed), (2, 1));
    // Detached: usable is false and the revision reads 0.
    svc.detach();
    assert!(!svc.is_usable());
    assert_eq!(svc.revision(), 0);
    assert!(!svc.status().is_usable());
}

#[test]
fn pending_marks_cover_a_folder_and_names_with_pattern_characters_exactly() {
    let (_data, _project, root, svc) = attached();
    for rel in ["a_b/x.md", "a_b/deep/y.md", "aXb/z.md", "a_b.md", "a_b0.md", "a%/p.md", "aZ/q.md"] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("# {rel}\n")).unwrap();
    }
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    let pending = |svc: &GraphService| -> Vec<String> {
        svc.with_store(|s| {
            Ok(s.all_coverage()?
                .into_iter()
                .filter(|c| c.state == CoverageState::Pending)
                .map(|c| c.path.to_string())
                .collect())
        })
        .unwrap()
    };
    // A folder: everything beneath it, nothing that merely shares the prefix
    // or matches `_` as a wildcard.
    assert_eq!(svc.mark_pending(&[path("a_b")]).unwrap(), 2);
    assert_eq!(pending(&svc), vec!["a_b/deep/y.md", "a_b/x.md"]);
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("a_b")]), force: false }).unwrap();
    assert!(pending(&svc).is_empty(), "the pass over the folder clears its marks");
    // `%` is a name, not a pattern.
    assert_eq!(svc.mark_pending(&[path("a%")]).unwrap(), 1);
    assert_eq!(pending(&svc), vec!["a%/p.md"]);
    // A pass over another path leaves the mark.
    svc.reconcile(&one("a_b.md")).unwrap();
    assert_eq!(pending(&svc), vec!["a%/p.md"]);
    svc.reconcile(&one("a%/p.md")).unwrap();
    assert!(pending(&svc).is_empty());
    // A file path marks that file only.
    assert_eq!(svc.mark_pending(&[path("a_b.md")]).unwrap(), 1);
    assert_eq!(pending(&svc), vec!["a_b.md"]);
}

#[test]
fn a_scoped_pass_resolves_links_to_files_outside_its_scope_and_wakes_files_waiting_elsewhere() {
    let (_data, _project, root, svc) = attached();
    let w = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    w("figures/plot.png", "png");
    w("other/defs.tex", "\\begin{lemma}\\label{lem:far}\nFar.\n\\end{lemma}\n");
    w("other/waits.tex", "See Lemma~\\ref{lem:later}.\n");
    w("ch/one.tex", "\\section{One}\nNothing yet.\n");
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    let unresolved = |p: &str| svc.with_store(|s| Ok(s.coverage(&path(p))?)).unwrap().unwrap().unresolved;
    assert_eq!(unresolved("other/waits.tex"), 1);
    // The edit (a one-file pass) links to a figure and a label in other
    // folders and defines the label another folder's file waits for.
    w(
        "ch/one.tex",
        "\\section{One}\n\\includegraphics{figures/plot}\n\\begin{lemma}\\label{lem:later}\nBy Lemma~\\ref{lem:far}.\n\\end{lemma}\n",
    );
    let report = svc.reconcile(&one("ch/one.tex")).unwrap();
    assert_eq!(report.read, 1);
    assert_eq!(unresolved("ch/one.tex"), 0, "links leaving the scope resolve");
    assert_eq!(unresolved("other/waits.tex"), 0, "the waiting file was resolved again");
    let one_tex = svc.resource(&path("ch/one.tex")).unwrap().unwrap();
    let plot = svc.resource(&path("figures/plot.png")).unwrap().unwrap();
    let detail = svc.entity_detail(&one_tex.id).unwrap();
    assert!(detail.outgoing.iter().any(|r| r.kind == RelationKind::UsesAsset && r.to == plot.id), "{detail:?}");
    // The same graph as a full pass over the same files produces.
    let scoped = svc.status();
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    let full = svc.status();
    assert_eq!((scoped.entities, scoped.relations), (full.entities, full.relations));
    // Removing the file in a scoped pass retires it and its folder.
    std::fs::remove_file(root.join("ch/one.tex")).unwrap();
    let report = svc.reconcile(&one("ch/one.tex")).unwrap();
    assert_eq!(report.resources_removed, 1);
    assert!(svc.resource(&path("ch/one.tex")).unwrap().is_none());
    assert_eq!(unresolved("other/waits.tex"), 1, "the label is gone again");
}

/// A status part that cannot be read is empty on its own: the other part is
/// still reported (unreadable coverage never reads as an empty graph).
#[test]
fn a_failed_coverage_read_does_not_zero_the_counts() {
    let (_data, _project, _root, svc) = attached();
    create_claim(&svc, "One");
    let before = svc.status();
    assert!(before.entities >= 2, "{before:?}");
    svc.with_store(|s| {
        s.connection().execute("DROP TABLE coverage", []).map_err(exposition_graph::store::StoreError::from)?;
        Ok(())
    })
    .unwrap();
    // A write makes the next status read the tables again.
    create_claim(&svc, "Two");
    let after = svc.status();
    assert_eq!((after.entities, after.relations), (before.entities + 1, before.relations));
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.coverage.resources, 0);
}

/// A second declaration of a label is a change to the references to that
/// label, wherever they are: the pass resolves the files that refer to it
/// again (here one file per label) and nothing else, so its cost is the
/// same in a project three times the size.
#[test]
fn a_second_declaration_of_a_label_costs_the_references_to_it() {
    let small = project(320, 20, 200);
    let large = project(960, 200, 2_000);
    let cost = |p: &Project| -> SqlWork {
        let svc = &p.svc;
        let edited = p.root.join(EDITED_FILE);
        let original = std::fs::read_to_string(&edited).unwrap();
        // `lem:<n>` is declared in chapter n and referred to by chapter n + 1.
        let declare_again = |n: usize| {
            let text = format!("{original}\\begin{{lemma}}\\label{{lem:{n}}}\nAgain.\n\\end{{lemma}}\n");
            std::fs::write(&edited, text).unwrap();
            let report = svc.reconcile(&one(EDITED_FILE)).unwrap();
            assert_eq!(report.read, 1, "{report:?}");
        };
        let detail = |chapter: usize| -> String {
            let at = path(&format!("chapters/c{}/ch{chapter}.tex", chapter % 20));
            svc.with_store(|s| Ok(s.coverage(&at)?)).unwrap().unwrap().detail.unwrap_or_default()
        };
        declare_again(3);
        assert!(detail(4).contains("\u{201c}lem:3\u{201d} matches 2 declarations"), "{}", detail(4));
        // A warm-up of the measured shape (one label unique again, another
        // declared twice): the counters of a cached statement start at its
        // first use.
        declare_again(6);
        svc.status();
        // Measured: `lem:6` is unique again, `lem:9` is declared twice.
        let (_, work) = sql_of(|| declare_again(9));
        assert!(!detail(4).contains("lem:3") && !detail(7).contains("lem:6"), "{} / {}", detail(4), detail(7));
        assert!(detail(10).contains("\u{201c}lem:9\u{201d} matches 2 declarations"), "{}", detail(10));
        work
    };
    let (s, l) = (cost(&small), cost(&large));
    assert_same_cost("a pass that declares a label a second time", s, l, 2_000);
}

/// A lakefile that appears changes the module name of the Lean files
/// beneath it and how every Lean import resolves: the pass derives those
/// again from the stored index (no Lean file is read) and leaves the files
/// of other kinds alone. Its statements grow with the Lean files only.
#[test]
fn a_lakefile_costs_the_lean_files_not_the_project() {
    let small = project(320, 20, 200);
    let large = project(960, 200, 2_000);
    let lean = |p: &Project| std::fs::read_dir(p.root.join("P")).unwrap().count() as u64;
    let cost = |p: &Project| -> SqlWork {
        let svc = &p.svc;
        let module = |svc: &GraphService| -> Option<String> {
            let e = svc.formal_declaration(&path("P/L1.lean"), "P.t1").unwrap().expect("an indexed declaration");
            e.attributes.get("lean.module").cloned()
        };
        assert_eq!(module(svc).as_deref(), Some("P.L1"));
        // Warm-up: the same kind of pass (a new file in the folder).
        std::fs::write(p.root.join("P/notes.md"), "# Notes\n").unwrap();
        svc.reconcile(&one("P/notes.md")).unwrap();
        svc.status();
        std::fs::write(p.root.join("P/lakefile.toml"), "name = \"L\"\n").unwrap();
        let (report, work) = sql_of(|| svc.reconcile(&one("P/lakefile.toml")).unwrap());
        assert_eq!(report.read, 1, "no Lean file is read again: {report:?}");
        assert_eq!(module(svc).as_deref(), Some("L1"));
        work
    };
    let (s, l) = (cost(&small), cost(&large));
    let more = lean(&large) - lean(&small);
    assert!(more >= 10, "{more} more Lean files in the larger project");
    // Per Lean file: its index record, its two declarations, its links.
    assert!(
        l.statements <= s.statements + 40 * more + 20,
        "a lakefile costs more than the Lean files it concerns ({more} more in the larger project):\n small {s:?}\n large {l:?}"
    );
}
