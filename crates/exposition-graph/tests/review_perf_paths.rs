//! Independent review of the graph performance change: folder prefixes as
//! bytewise key ranges instead of `LIKE` patterns (pending marks,
//! checkpoints, resource keys). Uses only what the base revision offers, so
//! the same file runs against both; `CASE ...` lines print what each build
//! does for a folder named in another case.

mod support;

use std::path::Path;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::*;
use support::attached;

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn full(svc: &GraphService) {
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
}

fn pending(svc: &GraphService) -> Vec<String> {
    svc.with_store(|s| {
        Ok(s.all_coverage()?
            .into_iter()
            .filter(|c| c.state == CoverageState::Pending)
            .map(|c| c.path.to_string())
            .collect())
    })
    .unwrap()
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

/// Every name around the key range of a folder: what sorts just before
/// `dir/`, just after `dir0`, pattern characters, and non-ASCII names.
#[test]
fn folder_ranges_match_exactly_the_folder_and_what_is_beneath_it() {
    let (_data, _project, root, svc) = attached();
    let all = [
        "a/b/x.md",
        "a/b/deep/er/y.md",
        "a/b.md",
        "a/b.d/z.md",
        "a/b-c/z.md",
        "a/b c/z.md",
        "a/b0.md",
        "a/b0/z.md",
        "a/bc/z.md",
        "a/b\u{e9}/z.md",
        "a/B/upper.md",
        "a_b/z.md",
        "aXb/z.md",
        "a%b/z.md",
        "a%/z.md",
        "aZ/z.md",
        "\u{e9}t\u{e9}/z.md",
        "\u{e9}t\u{e9}0.md",
    ];
    for rel in all {
        // `a/B` and `a/b` are one folder on a case-insensitive file system.
        if rel == "a/B/upper.md" && cfg!(windows) {
            continue;
        }
        write(&root, rel, &format!("# {rel}\n"));
    }
    full(&svc);
    let beneath = |dir: &str| -> Vec<String> {
        let mut want: Vec<String> =
            live_resources(&svc).into_iter().filter(|p| p == dir || p.starts_with(&format!("{dir}/"))).collect();
        want.sort();
        want
    };
    for dir in ["a/b", "a", "a_b", "a%", "a%b", "aXb", "a/b0", "a/b.d", "a/b c", "\u{e9}t\u{e9}", "a/b.md", "nothing"] {
        let want = beneath(dir);
        let marked = svc.mark_pending(&[path(dir)]).unwrap();
        assert_eq!(marked, want.len(), "marks under {dir}");
        assert_eq!(pending(&svc), want, "marks under {dir}");
        // The stored keys and checkpoints under it.
        let (keys, checkpoints) = svc
            .with_store(|s| {
                let mut keys: Vec<String> =
                    s.resource_keys(Some(&[path(dir)]))?.into_iter().map(|k| k.path.to_string()).collect();
                keys.sort();
                let checkpoints: Vec<String> =
                    s.checkpoints_under(&[path(dir)])?.into_keys().map(|p| p.to_string()).collect();
                Ok((keys, checkpoints))
            })
            .unwrap();
        assert_eq!(keys, want, "resource keys under {dir}");
        assert_eq!(checkpoints, want, "checkpoints under {dir}");
        // A pass over another folder leaves the marks; one over it clears them.
        svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("aZ")]), force: false }).unwrap();
        if dir != "aZ" && dir != "a" {
            assert_eq!(pending(&svc), want, "a pass elsewhere keeps the marks under {dir}");
        }
        svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path(dir)]), force: false }).unwrap();
        assert!(pending(&svc).is_empty(), "the pass over {dir} clears its marks: {:?}", pending(&svc));
    }
    assert_eq!(live_resources(&svc).len(), if cfg!(windows) { all.len() - 1 } else { all.len() });
}

/// The former `LIKE` prefix ignored ASCII case in the folder part; the
/// ranges compare bytes. What each call returns for a folder named in
/// another case is printed (`CASE ...` lines) so two builds can be compared.
#[test]
fn a_folder_named_in_another_case() {
    let (_data, _project, root, svc) = attached();
    write(&root, "Notes/a.md", "# A\n\nSee [b](sub/b.md).\n");
    write(&root, "Notes/sub/b.md", "# B\n");
    write(&root, "other.md", "# Other\n");
    full(&svc);
    assert_eq!(live_resources(&svc), ["Notes/a.md", "Notes/sub/b.md", "other.md"]);
    // The folder named as stored.
    assert_eq!(svc.mark_pending(&[path("Notes")]).unwrap(), 2);
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("Notes")]), force: false }).unwrap();
    assert!(pending(&svc).is_empty());
    // The folder named in another case.
    let marked = svc.mark_pending(&[path("notes")]).unwrap();
    println!("CASE mark_pending(notes) marked {marked} rows; pending {:?}", pending(&svc));
    let (keys, checkpoints) = svc
        .with_store(|s| {
            Ok((s.resource_keys(Some(&[path("notes")]))?.len(), s.checkpoints_under(&[path("notes")])?.len()))
        })
        .unwrap();
    println!("CASE resource_keys(notes) {keys}; checkpoints_under(notes) {checkpoints}");
    let report = svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("notes")]), force: false }).unwrap();
    println!(
        "CASE pass over notes: read {}, added {}, removed {}; pending after {:?}; live resources {:?}",
        report.read,
        report.resources_added,
        report.resources_removed,
        pending(&svc),
        live_resources(&svc)
    );
    // One resource per file, whatever the case the folder was named in.
    assert_eq!(live_resources(&svc), ["Notes/a.md", "Notes/sub/b.md", "other.md"]);
    assert_eq!((report.resources_added, report.resources_removed), (0, 0), "{report:?}");
    // A file named in another case (exact comparison before and after the change).
    let marked = svc.mark_pending(&[path("notes/A.md")]).unwrap();
    println!("CASE mark_pending(notes/A.md) marked {marked} rows");
    // Source-removal planning for the folder named in another case.
    let impacts = svc.path_impacts(&[path("notes")]).map(|i| i.len());
    println!("CASE path_impacts(notes) {impacts:?}");
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    println!("CASE after a full pass: live resources {:?}; pending {:?}", live_resources(&svc), pending(&svc));
    assert_eq!(live_resources(&svc), ["Notes/a.md", "Notes/sub/b.md", "other.md"]);
    // What the reviewed code does (the base marked the two rows under
    // `Notes`, read both tables' rows under it, and cleared the marks in the
    // pass over `notes`).
    assert_eq!(marked, 0);
}

/// A case-only rename of a folder, reported by the watcher as the old and
/// the new name: the events as the driver delivers them.
#[test]
fn a_case_only_folder_rename_reported_by_the_watcher() {
    let (_data, _project, root, svc) = attached();
    write(&root, "Notes/a.md", "# A\n");
    write(&root, "Notes/b.md", "# B\n");
    full(&svc);
    let before = svc.resource(&path("Notes/a.md")).unwrap().unwrap().id;
    std::fs::rename(root.join("Notes"), root.join("notes")).unwrap();
    let events = [path("Notes"), path("notes")];
    let marked = svc.mark_pending(&events).unwrap();
    let report = svc.reconcile(&ReconcileOptions { scope: Scope::Paths(events.to_vec()), force: false }).unwrap();
    let after = svc.resource(&path("notes/a.md")).unwrap().map(|e| e.id);
    println!(
        "CASE-RENAME marked {marked}; pass renamed {}, added {}, removed {}; live {:?}; pending {:?}; identity kept {}",
        report.resources_renamed,
        report.resources_added,
        report.resources_removed,
        live_resources(&svc),
        pending(&svc),
        after.as_ref() == Some(&before)
    );
    // A rename: the identities are kept, nothing is left under the old name.
    assert_eq!(after.as_ref(), Some(&before));
    assert_eq!(live_resources(&svc), ["notes/a.md", "notes/b.md"]);
    assert_eq!((report.resources_renamed, report.resources_added, report.resources_removed), (2, 0, 0), "{report:?}");
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    println!("CASE-RENAME after a full pass: live {:?}; pending {:?}", live_resources(&svc), pending(&svc));
    assert_eq!(live_resources(&svc), ["notes/a.md", "notes/b.md"]);
    assert!(pending(&svc).is_empty());
}

/// FINDING (regression against 92bf299): a pass over a few paths that
/// overlap (a folder and a file beneath it, as a watcher reports a new or
/// moved folder together with its files) reads the file's resource once per
/// path, takes the second read for a second identity of the file, and
/// tombstones the file's only resource.
#[test]
fn a_pass_over_a_folder_and_a_file_beneath_it_keeps_the_file() {
    let (_data, _project, root, svc) = attached();
    write(&root, "notes/a.md", "# A\n");
    write(&root, "notes/deep/b.md", "# B\n");
    full(&svc);
    let before = svc.resource(&path("notes/a.md")).unwrap().unwrap().id;
    for events in [
        vec![path("notes"), path("notes/a.md")],
        vec![path("notes/deep/b.md"), path("notes/deep"), path("notes")],
        vec![path("notes/a.md"), path("notes/a.md")],
    ] {
        svc.mark_pending(&events).unwrap();
        let report = svc.reconcile(&ReconcileOptions { scope: Scope::Paths(events.clone()), force: false }).unwrap();
        println!("OVERLAP {events:?}: {report:?}; live {:?}", live_resources(&svc));
        assert_eq!(live_resources(&svc), ["notes/a.md", "notes/deep/b.md"], "after a pass over {events:?}");
        assert_eq!(svc.resource(&path("notes/a.md")).unwrap().map(|e| e.id), Some(before.clone()));
    }
}
