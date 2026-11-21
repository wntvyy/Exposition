//! An incremental pass leaves what a full reindex of the same files leaves:
//! one targeted case per dependency a file's derived records have on other
//! files (the declarations of a label, the files on a lookup path, the LaTeX
//! main files, the Lake package roots) and on how a path is spelled. Each
//! case states the expected records and then compares every stored record
//! with a forced full reindex. The randomized form of the same comparison is
//! `review_perf_differential.rs`; the cost of these passes is bounded in
//! `cost_bounds.rs`.

mod support;

use std::path::Path;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, ReconcileReport, Scope};
use exposition_graph::*;
use serde_json::Value;
use support::attached;

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn full(svc: &GraphService) -> ReconcileReport {
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap()
}

fn pass(svc: &GraphService, paths: &[&str]) -> ReconcileReport {
    let paths: Vec<WorkspacePath> = paths.iter().map(|p| path(p)).collect();
    svc.mark_pending(&paths).unwrap();
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(paths), force: false }).unwrap()
}

fn coverage(svc: &GraphService, p: &str) -> ResourceCoverage {
    svc.with_store(|s| Ok(s.coverage(&path(p))?)).unwrap().unwrap_or_else(|| panic!("{p} has no coverage"))
}

fn problems(svc: &GraphService, p: &str) -> String {
    coverage(svc, p).detail.unwrap_or_default()
}

fn waits_for(svc: &GraphService, p: &str) -> Vec<String> {
    let mut wanted: Vec<String> = svc
        .with_store(|s| Ok(s.index_wanted()?))
        .unwrap()
        .into_iter()
        .filter(|(at, _)| at.as_str() == p)
        .flat_map(|(_, wanted)| wanted)
        .collect();
    wanted.sort();
    wanted
}

fn live_resources(svc: &GraphService) -> Vec<String> {
    let mut out: Vec<String> = svc
        .with_store(|s| {
            Ok(s.resource_keys(None)?.into_iter().filter(|k| !k.tombstoned).map(|k| k.path.to_string()).collect())
        })
        .unwrap();
    out.sort();
    out
}

fn resource(svc: &GraphService, p: &str) -> EntityId {
    svc.resource(&path(p)).unwrap().unwrap_or_else(|| panic!("{p} is not a resource")).id
}

/// The paths of the resources `from`'s file is observed to link to.
fn links_to(svc: &GraphService, from: &str, kind: RelationKind) -> Vec<String> {
    let detail = svc.entity_detail(&resource(svc, from)).unwrap();
    let mut out: Vec<String> = detail
        .outgoing
        .iter()
        .filter(|r| r.kind == kind)
        .filter_map(|r| svc.with_store(|s| Ok(s.entity(&r.to)?)).unwrap())
        .filter_map(|e| e.key)
        .collect();
    out.sort();
    out
}

fn ids(svc: &GraphService, sql: &str) -> Vec<String> {
    svc.with_store(|store| {
        let mut stmt = store.connection().prepare(sql).map_err(store::StoreError::from)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(store::StoreError::from)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(store::StoreError::from)?);
        }
        Ok(out)
    })
    .unwrap()
}

/// Every stored record, without the times and revision counters a pass
/// advances.
fn records(svc: &GraphService) -> Vec<String> {
    let entities = ids(svc, "SELECT id FROM entities ORDER BY id");
    let relations = ids(svc, "SELECT id FROM relations ORDER BY id");
    let clean = |mut v: Value| {
        if let Value::Object(map) = &mut v {
            for key in ["updatedAtMs", "createdAtMs", "revision"] {
                map.remove(key);
            }
        }
        v
    };
    let mut lines = Vec::new();
    svc.with_store(|store| {
        for id in &entities {
            let id = EntityId::parse(id).unwrap();
            let e = store.entity(&id)?.expect("listed entity");
            lines.push(format!("E {}", clean(serde_json::to_value(&e).unwrap())));
            for b in store.bindings(&id)? {
                lines.push(format!("B {}", serde_json::to_value(&b).unwrap()));
            }
        }
        for id in &relations {
            let r = store.relation(&RelationId::parse(id).unwrap())?.expect("listed relation");
            lines.push(format!("R {}", clean(serde_json::to_value(&r).unwrap())));
        }
        for c in store.all_coverage()? {
            lines.push(format!("C {}", serde_json::to_value(&c).unwrap()));
        }
        for (path, mut wanted) in store.index_wanted()? {
            wanted.sort();
            lines.push(format!("W {path} {wanted:?}"));
        }
        Ok(())
    })
    .unwrap();
    lines.sort();
    lines
}

/// The graph as the incremental passes left it is the graph a forced full
/// reindex of the same files leaves.
#[track_caller]
fn assert_reindex_changes_nothing(svc: &GraphService, when: &str) {
    let incremental = records(svc);
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    let reindexed = records(svc);
    let only = |a: &[String], b: &[String]| -> Vec<String> { a.iter().filter(|l| !b.contains(l)).cloned().collect() };
    assert!(
        incremental == reindexed,
        "{when}: a forced full reindex changed the graph\n incremental only: {:#?}\n reindexed only: {:#?}",
        only(&incremental, &reindexed),
        only(&reindexed, &incremental)
    );
}

fn lemma(label: &str) -> String {
    format!("\\begin{{lemma}}\\label{{{label}}}\nA statement.\n\\end{{lemma}}\n")
}

// ----- 1. the declarations of a label -----

#[test]
fn a_second_declaration_of_a_label_makes_every_reference_to_it_ambiguous_and_back() {
    let (_data, _project, root, svc) = attached();
    write(&root, "defs/a.tex", &lemma("lem:x"));
    write(&root, "uses/r.tex", "\\begin{theorem}\\label{thm:r}\nBy Lemma~\\ref{lem:x}.\n\\end{theorem}\n");
    write(&root, "uses/plain.tex", "See Lemma~\\ref{lem:x}.\n");
    full(&svc);
    let referrers = ["uses/r.tex", "uses/plain.tex"];
    let resolved = |when: &str| {
        for r in referrers {
            assert_eq!(coverage(&svc, r).unresolved, 0, "{when}: {r}: {}", problems(&svc, r));
            assert!(waits_for(&svc, r).is_empty(), "{when}: {r} waits for {:?}", waits_for(&svc, r));
        }
    };
    let unresolved = |when: &str, reason: &str| {
        for r in referrers {
            assert_eq!(coverage(&svc, r).unresolved, 1, "{when}: {r}: {}", problems(&svc, r));
            assert!(problems(&svc, r).contains(reason), "{when}: {r}: {}", problems(&svc, r));
            assert_eq!(waits_for(&svc, r), ["name:lem:x"], "{when}: {r}");
        }
    };
    resolved("one declaration");

    // A second file declares the label: one pass over that file.
    write(&root, "other/b.tex", &lemma("lem:x"));
    let report = pass(&svc, &["other/b.tex"]);
    assert_eq!(report.read, 1, "{report:?}");
    unresolved("a second declaration", "matches 2 declarations");
    assert_reindex_changes_nothing(&svc, "a second declaration");

    // The second declaration is edited away: unique again.
    write(&root, "other/b.tex", "No statement.\n");
    pass(&svc, &["other/b.tex"]);
    resolved("the second declaration removed by an edit");
    assert_reindex_changes_nothing(&svc, "the second declaration removed by an edit");

    // Declared twice again, then one declaring file is deleted.
    write(&root, "other/b.tex", &lemma("lem:x"));
    pass(&svc, &["other/b.tex"]);
    unresolved("declared twice again", "matches 2 declarations");
    std::fs::remove_file(root.join("other/b.tex")).unwrap();
    pass(&svc, &["other/b.tex"]);
    resolved("the second declaring file deleted");
    assert_reindex_changes_nothing(&svc, "the second declaring file deleted");

    // The label is renamed where it is declared, and renamed back.
    write(&root, "defs/a.tex", &lemma("lem:y"));
    pass(&svc, &["defs/a.tex"]);
    unresolved("the label renamed", "not found");
    assert_reindex_changes_nothing(&svc, "the label renamed");
    write(&root, "defs/a.tex", &lemma("lem:x"));
    pass(&svc, &["defs/a.tex"]);
    resolved("the label renamed back");
    assert_reindex_changes_nothing(&svc, "the label renamed back");

    // The declaring file moves: the declaration stays the one of its name.
    std::fs::create_dir_all(root.join("moved")).unwrap();
    std::fs::rename(root.join("defs/a.tex"), root.join("moved/a.tex")).unwrap();
    pass(&svc, &["defs/a.tex", "moved/a.tex"]);
    resolved("the declaring file moved");
    assert_reindex_changes_nothing(&svc, "the declaring file moved");

    // The only declaring file is deleted.
    std::fs::remove_file(root.join("moved/a.tex")).unwrap();
    pass(&svc, &["moved/a.tex"]);
    unresolved("the declaring file deleted", "not found");
    assert_reindex_changes_nothing(&svc, "the declaring file deleted");
}

/// A label that names no declaration (an equation) and later a declaration,
/// and a reference a bibliography entry answers until a label of that name
/// is declared.
#[test]
fn a_reference_follows_what_its_name_comes_to_mean() {
    let (_data, _project, root, svc) = attached();
    write(&root, "eq.tex", "\\begin{equation}\\label{key:k} x = 1 \\end{equation}\n");
    write(&root, "r.tex", "\\begin{theorem}\\label{thm:r}\nBy \\ref{key:k}.\n\\end{theorem}\n");
    full(&svc);
    assert_eq!(coverage(&svc, "r.tex").unresolved, 0, "{}", problems(&svc, "r.tex"));
    // The equation goes: the reference dangles.
    write(&root, "eq.tex", "Nothing.\n");
    pass(&svc, &["eq.tex"]);
    assert!(problems(&svc, "r.tex").contains("not found"), "{}", problems(&svc, "r.tex"));
    assert_reindex_changes_nothing(&svc, "the label removed");
    // A lemma of that name appears: the reference is a relation to it.
    write(&root, "eq.tex", &lemma("key:k"));
    pass(&svc, &["eq.tex"]);
    assert_eq!(coverage(&svc, "r.tex").unresolved, 0, "{}", problems(&svc, "r.tex"));
    assert_reindex_changes_nothing(&svc, "a declaration of the name");
    // Back to a label that names no declaration.
    write(&root, "eq.tex", "\\begin{equation}\\label{key:k} x = 1 \\end{equation}\n");
    pass(&svc, &["eq.tex"]);
    assert_eq!(coverage(&svc, "r.tex").unresolved, 0, "{}", problems(&svc, "r.tex"));
    assert_reindex_changes_nothing(&svc, "the declaration became a plain label");
}

// ----- the files on a lookup path, the LaTeX main files -----

#[test]
fn a_written_path_follows_the_files_on_its_lookup_path() {
    let (_data, _project, root, svc) = attached();
    write(&root, "ch/c.tex", "\\section{C}\n\\includegraphics{fig/p}\n\\includegraphics{fig/q}\n");
    write(&root, "fig/p.png", "root png");
    full(&svc);
    assert_eq!(links_to(&svc, "ch/c.tex", RelationKind::UsesAsset), ["fig/p.png"]);
    assert!(problems(&svc, "ch/c.tex").contains("fig/q"), "{}", problems(&svc, "ch/c.tex"));
    // An extension tried earlier in the same folder.
    write(&root, "fig/p.pdf", "root pdf");
    pass(&svc, &["fig/p.pdf"]);
    assert_eq!(links_to(&svc, "ch/c.tex", RelationKind::UsesAsset), ["fig/p.pdf"]);
    assert_reindex_changes_nothing(&svc, "an earlier extension");
    // A folder tried earlier (the file's own).
    write(&root, "ch/fig/p.svg", "local svg");
    pass(&svc, &["ch/fig/p.svg"]);
    assert_eq!(links_to(&svc, "ch/c.tex", RelationKind::UsesAsset), ["ch/fig/p.svg"]);
    assert_reindex_changes_nothing(&svc, "an earlier folder");
    // The missing figure appears as a late candidate (the twelfth tried), and goes.
    write(&root, "fig/q.svg", "root svg");
    pass(&svc, &["fig/q.svg"]);
    assert_eq!(links_to(&svc, "ch/c.tex", RelationKind::UsesAsset), ["ch/fig/p.svg", "fig/q.svg"]);
    assert_reindex_changes_nothing(&svc, "a late candidate");
    std::fs::remove_file(root.join("fig/q.svg")).unwrap();
    pass(&svc, &["fig/q.svg"]);
    assert_eq!(links_to(&svc, "ch/c.tex", RelationKind::UsesAsset), ["ch/fig/p.svg"]);
    // The folder of a LaTeX main file that appears holds the missing figure.
    write(&root, "paper/fig/q.png", "paper png");
    pass(&svc, &["paper/fig/q.png"]);
    assert!(problems(&svc, "ch/c.tex").contains("fig/q"), "not on the lookup path yet");
    write(&root, "paper/main.tex", "\\documentclass{article}\n\\begin{document}\n\\input{../ch/c}\n\\end{document}\n");
    pass(&svc, &["paper/main.tex"]);
    assert_eq!(links_to(&svc, "ch/c.tex", RelationKind::UsesAsset), ["ch/fig/p.svg", "paper/fig/q.png"]);
    assert_eq!(coverage(&svc, "ch/c.tex").unresolved, 0, "{}", problems(&svc, "ch/c.tex"));
    assert_reindex_changes_nothing(&svc, "a main file appeared");
    // It stops being a main file: its folder leaves the lookup path.
    write(&root, "paper/main.tex", "\\section{Not a main file}\n");
    pass(&svc, &["paper/main.tex"]);
    assert_eq!(links_to(&svc, "ch/c.tex", RelationKind::UsesAsset), ["ch/fig/p.svg"]);
    assert!(problems(&svc, "ch/c.tex").contains("fig/q"), "{}", problems(&svc, "ch/c.tex"));
    assert_reindex_changes_nothing(&svc, "the main file is one no longer");
}

// ----- 2. the Lake package roots -----

fn lean_modules(svc: &GraphService) -> Vec<(String, Option<String>)> {
    let all = ids(svc, "SELECT id FROM entities ORDER BY id");
    let mut out: Vec<(String, Option<String>)> = svc
        .with_store(|store| {
            let mut out = Vec::new();
            for id in &all {
                let e = store.entity(&EntityId::parse(id).unwrap())?.expect("listed entity");
                if e.kind == EntityKind::FormalDeclaration && e.lifecycle != Lifecycle::Tombstoned {
                    out.push((e.key.clone().unwrap_or_default(), e.attributes.get("lean.module").cloned()));
                }
            }
            Ok(out)
        })
        .unwrap();
    out.sort();
    out
}

#[test]
fn lean_modules_follow_a_lakefile_that_appears_moves_and_disappears() {
    let (_data, _project, root, svc) = attached();
    write(&root, "pkg/P/L.lean", "namespace P\ntheorem t : 1 = 1 := rfl\nend P\n");
    write(&root, "pkg/P/M.lean", "import P.L\nnamespace P\ntheorem u : 2 = 2 := rfl\nend P\n");
    write(&root, "notes.md", "# Notes\n");
    full(&svc);
    let modules =
        |l: &str, m: &str| vec![("P.t".to_owned(), Some(l.to_owned())), ("P.u".to_owned(), Some(m.to_owned()))];
    // No package file: the project folder is the root; `P.L` names no file.
    assert_eq!(lean_modules(&svc), modules("pkg.P.L", "pkg.P.M"));
    assert!(links_to(&svc, "pkg/P/M.lean", RelationKind::Imports).is_empty());

    // A lakefile appears in `pkg` (a pass over that file only).
    write(&root, "pkg/lakefile.toml", "name = \"P\"\n");
    let report = pass(&svc, &["pkg/lakefile.toml"]);
    assert_eq!(report.read, 1, "no Lean file is read again: {report:?}");
    assert_eq!(lean_modules(&svc), modules("P.L", "P.M"));
    assert_eq!(links_to(&svc, "pkg/P/M.lean", RelationKind::Imports), ["pkg/P/L.lean"]);
    assert_reindex_changes_nothing(&svc, "a lakefile appeared");

    // It moves one folder down.
    std::fs::rename(root.join("pkg/lakefile.toml"), root.join("pkg/P/lakefile.toml")).unwrap();
    pass(&svc, &["pkg/lakefile.toml", "pkg/P/lakefile.toml"]);
    assert_eq!(lean_modules(&svc), modules("L", "M"));
    assert!(links_to(&svc, "pkg/P/M.lean", RelationKind::Imports).is_empty());
    assert_reindex_changes_nothing(&svc, "the lakefile moved down");

    // It moves to the project folder.
    std::fs::rename(root.join("pkg/P/lakefile.toml"), root.join("lakefile.toml")).unwrap();
    pass(&svc, &["pkg/P/lakefile.toml", "lakefile.toml"]);
    assert_eq!(lean_modules(&svc), modules("pkg.P.L", "pkg.P.M"));
    assert_reindex_changes_nothing(&svc, "the lakefile moved to the project folder");

    // A second package root beside it, then both disappear.
    write(&root, "pkg/lakefile.lean", "import Lake\n");
    pass(&svc, &["pkg/lakefile.lean"]);
    assert_eq!(lean_modules(&svc), modules("P.L", "P.M"));
    assert_eq!(links_to(&svc, "pkg/P/M.lean", RelationKind::Imports), ["pkg/P/L.lean"]);
    assert_reindex_changes_nothing(&svc, "a second package root");
    std::fs::remove_file(root.join("pkg/lakefile.lean")).unwrap();
    std::fs::remove_file(root.join("lakefile.toml")).unwrap();
    pass(&svc, &["pkg/lakefile.lean", "lakefile.toml"]);
    assert_eq!(lean_modules(&svc), modules("pkg.P.L", "pkg.P.M"));
    assert!(links_to(&svc, "pkg/P/M.lean", RelationKind::Imports).is_empty());
    assert_reindex_changes_nothing(&svc, "the lakefiles disappeared");
}

// ----- 3. one key per file, however its path is spelled -----

/// The file system under `root` treats names that differ in case as one.
fn ignores_case(root: &Path) -> bool {
    std::fs::write(root.join("case-probe.tmp"), "").unwrap();
    let same = root.join("CASE-PROBE.TMP").exists();
    std::fs::remove_file(root.join("case-probe.tmp")).unwrap();
    same
}

#[test]
fn a_path_named_in_another_case_is_the_file_it_reaches() {
    let (_data, _project, root, svc) = attached();
    if !ignores_case(&root) {
        println!("SKIPPED: this file system distinguishes case (see `two_spellings_that_both_exist_are_two_files`)");
        return;
    }
    write(&root, "Notes/a.md", "# A\n\nSee [b](sub/b.md).\n");
    write(&root, "Notes/sub/b.md", "# B\n");
    write(&root, "other.md", "# Other\n");
    full(&svc);
    let stored = ["Notes/a.md", "Notes/sub/b.md", "other.md"];
    assert_eq!(live_resources(&svc), stored);
    let a = resource(&svc, "Notes/a.md");
    let before = records(&svc);
    // The folder, a file, and a file in a folder, each in another case, in
    // a pass over a few paths and in one over many.
    for scope in
        [vec!["notes"], vec!["NOTES/A.MD"], vec!["notes/SUB/b.md", "OTHER.md"], vec!["Notes", "notes", "NOTES"]]
    {
        let report = pass(&svc, &scope);
        assert_eq!((report.resources_added, report.resources_removed, report.read), (0, 0, 0), "{scope:?}: {report:?}");
        assert_eq!(live_resources(&svc), stored, "after a pass over {scope:?}");
    }
    let mut many: Vec<String> = (0..20).map(|i| format!("nothing/n{i}.md")).collect();
    many.push("notes".into());
    pass(&svc, &many.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(live_resources(&svc), stored, "after a pass over many paths");
    assert_eq!(resource(&svc, "Notes/a.md"), a);
    assert_eq!(records(&svc), before, "nothing was written");
    // A changed file reported in another case is reconciled under its key.
    write(&root, "Notes/a.md", "# A\n\nNo link.\n");
    let report = pass(&svc, &["notes/a.md"]);
    assert_eq!((report.read, report.resources_added), (1, 0), "{report:?}");
    assert!(links_to(&svc, "Notes/a.md", RelationKind::References).is_empty());
    // An admission that names the file in another case finds its resource.
    let admitted = svc.ensure_resource(&path("NOTES/a.md"), None, false, "test").unwrap();
    assert_eq!((admitted.id, admitted.key.as_deref()), (a, Some("Notes/a.md")));
    assert_eq!(live_resources(&svc), stored);
    assert_reindex_changes_nothing(&svc, "paths named in another case");
}

#[test]
fn a_case_only_rename_of_a_folder_keeps_every_identity() {
    let (_data, _project, root, svc) = attached();
    if !ignores_case(&root) {
        println!("SKIPPED: this file system distinguishes case");
        return;
    }
    write(&root, "Notes/a.md", &format!("# A\n\n{}\n", "Long enough to be told apart. ".repeat(8)));
    write(&root, "Notes/deep/b.md", "# B\n");
    full(&svc);
    let (a, b) = (resource(&svc, "Notes/a.md"), resource(&svc, "Notes/deep/b.md"));
    // Reported as the old and the new name.
    std::fs::rename(root.join("Notes"), root.join("notes")).unwrap();
    let report = pass(&svc, &["Notes", "notes"]);
    assert_eq!((report.resources_renamed, report.resources_added, report.resources_removed), (2, 0, 0), "{report:?}");
    assert_eq!(live_resources(&svc), ["notes/a.md", "notes/deep/b.md"]);
    assert_eq!((resource(&svc, "notes/a.md"), resource(&svc, "notes/deep/b.md")), (a.clone(), b.clone()));
    assert_reindex_changes_nothing(&svc, "a case-only rename reported with both names");
    // Reported as the new name only.
    std::fs::rename(root.join("notes"), root.join("NOTES")).unwrap();
    let report = pass(&svc, &["NOTES"]);
    assert_eq!((report.resources_renamed, report.resources_added, report.resources_removed), (2, 0, 0), "{report:?}");
    assert_eq!(live_resources(&svc), ["NOTES/a.md", "NOTES/deep/b.md"]);
    assert_eq!((resource(&svc, "NOTES/a.md"), resource(&svc, "NOTES/deep/b.md")), (a.clone(), b.clone()));
    // Reported as the old name only: the folder is found where it is.
    std::fs::rename(root.join("NOTES"), root.join("Notes")).unwrap();
    let report = pass(&svc, &["NOTES"]);
    assert_eq!((report.resources_renamed, report.resources_added, report.resources_removed), (2, 0, 0), "{report:?}");
    assert_eq!(live_resources(&svc), ["Notes/a.md", "Notes/deep/b.md"]);
    assert_eq!((resource(&svc, "Notes/a.md"), resource(&svc, "Notes/deep/b.md")), (a, b));
    // One folder entity per folder, no orphan under an old spelling.
    let folders =
        ids(&svc, "SELECT key FROM entities WHERE kind = 'folder' AND lifecycle != 'tombstoned' ORDER BY key");
    assert_eq!(folders, ["Notes", "Notes/deep"]);
    assert_reindex_changes_nothing(&svc, "case-only renames");
}

/// A graph written by an earlier build holds two live resources for one
/// file (its path stored in two cases). An unforced full pass keeps the one
/// spelled as the file is and retires the other.
#[test]
fn a_full_pass_retires_a_resource_stored_under_a_spelling_no_file_has() {
    let (_data, _project, root, svc) = attached();
    if !ignores_case(&root) {
        println!("SKIPPED: this file system distinguishes case");
        return;
    }
    write(&root, "Notes/a.md", "# A\n");
    write(&root, "Notes/b.md", "# B\n");
    full(&svc);
    let a = resource(&svc, "Notes/a.md");
    // The second identity, as the earlier build recorded it: a copy of the
    // file's rows under the other spelling (written past the service).
    svc.with_store(|s| {
        let c = s.connection();
        let run = |sql: &str| c.execute_batch(sql).map_err(store::StoreError::from);
        run("CREATE TEMP TABLE dup AS SELECT * FROM entities WHERE kind = 'resource' AND key = 'Notes/a.md';
             UPDATE dup SET id = 'ent-00000000000000000000000000000001', key = 'notes/a.md', created_at = created_at + 1;
             INSERT INTO entities SELECT * FROM dup;
             DROP TABLE dup;
             CREATE TEMP TABLE dupc AS SELECT * FROM coverage WHERE path = 'Notes/a.md';
             UPDATE dupc SET path = 'notes/a.md';
             INSERT INTO coverage SELECT * FROM dupc;
             DROP TABLE dupc;")?;
        Ok(())
    })
    .unwrap();
    assert_eq!(live_resources(&svc), ["Notes/a.md", "Notes/b.md", "notes/a.md"]);
    let report = full(&svc);
    assert_eq!((report.resources_removed, report.resources_added, report.read), (1, 0, 0), "{report:?}");
    assert_eq!(live_resources(&svc), ["Notes/a.md", "Notes/b.md"]);
    assert_eq!(resource(&svc, "Notes/a.md"), a, "the resource spelled as the file is stays");
    assert!(svc.with_store(|s| Ok(s.coverage(&path("notes/a.md"))?)).unwrap().is_none());
    assert_reindex_changes_nothing(&svc, "a duplicate retired");
}

/// Where the file system distinguishes case, two spellings that both exist
/// are two files with two resources, and a path names exactly its own.
/// Runs on a case-sensitive file system, and on Windows in a folder marked
/// case-sensitive when the host allows that (otherwise it says so).
#[test]
fn two_spellings_that_both_exist_are_two_files() {
    let (_data, _project, root, svc) = attached();
    if ignores_case(&root) {
        let enabled = cfg!(windows)
            && std::process::Command::new("fsutil")
                .args(["file", "setCaseSensitiveInfo"])
                .arg(&root)
                .arg("enable")
                .output()
                .is_ok_and(|o| o.status.success());
        if !enabled || ignores_case(&root) {
            println!("SKIPPED: this host has no case-sensitive folder to test in");
            return;
        }
    }
    write(&root, "Notes.md", "# Upper\n");
    write(&root, "notes.md", "# Lower\n");
    full(&svc);
    assert_eq!(live_resources(&svc), ["Notes.md", "notes.md"]);
    let (upper, lower) = (resource(&svc, "Notes.md"), resource(&svc, "notes.md"));
    assert_ne!(upper, lower);
    // A pass over one spelling concerns that file only.
    write(&root, "notes.md", "# Lower, edited\n");
    let report = pass(&svc, &["notes.md"]);
    assert_eq!((report.read, report.resources_added, report.resources_removed), (1, 0, 0), "{report:?}");
    std::fs::remove_file(root.join("Notes.md")).unwrap();
    let report = pass(&svc, &["Notes.md"]);
    assert_eq!(report.resources_removed, 1, "{report:?}");
    assert_eq!(live_resources(&svc), ["notes.md"]);
    assert_eq!(resource(&svc, "notes.md"), lower);
    // A file that appears beside one that differs only in case is new.
    write(&root, "NOTES.md", "# Shouting\n");
    let report = pass(&svc, &["NOTES.md"]);
    assert_eq!((report.resources_added, report.resources_renamed), (1, 0), "{report:?}");
    assert_eq!(live_resources(&svc), ["NOTES.md", "notes.md"]);
    assert_reindex_changes_nothing(&svc, "two spellings, two files");
}
