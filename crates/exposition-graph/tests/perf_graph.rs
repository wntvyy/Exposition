//! Graph cost at scale (run explicitly):
//! `node scripts/perf/rust-bench.mjs exposition-graph --out <file.jsonl>`
//! Scale from `EXPOSITION_PERF_SCALE` (small | large | stress).
//!
//! One generated project on disk (indexed by a real full pass) plus curated
//! lemmas and `dependsOn` relations written in bulk, with two hub lemmas.
//! Every measurement prints timings and the SQL work of one run (statements,
//! rows, virtual-machine steps, full-scan steps): the counters are what
//! `tests/cost_bounds.rs` asserts.

mod support;

use std::collections::BTreeMap;
use std::time::Instant;

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

#[test]
#[ignore = "perf"]
fn graph_costs() {
    let scale = Scale::from_env();
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    generate(&root, scale.files, scale.big_file_declarations);
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    svc.attach(&root);
    count_sql(&svc);

    // Cold index (one sample: the fixture is built once).
    let t = Instant::now();
    let (cold, work) = sql_of(|| svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap());
    report("graph.cold_index", &scale, &[t.elapsed().as_secs_f64() * 1000.0], &work.json());
    assert!(cold.resources_added as usize >= scale.files * 9 / 10, "{cold:?}");
    let t = Instant::now();
    let bulk = bulk(&svc, scale.lemmas, scale.relations, scale.hub_degree);
    report("graph.bulk_write", &scale, &[t.elapsed().as_secs_f64() * 1000.0], &take().json());
    let status = svc.status();
    eprintln!(
        "fixture: {} entities, {} relations, coverage {:?}",
        status.entities, status.relations, status.coverage.resources
    );

    // status(): the accessor everything calls.
    let (samples, work) = measure(7, |_| {
        svc.status();
    });
    report("graph.status", &scale, &samples, &work.json());

    // An unchanged full pass (what attach and a burst of changes run).
    let (samples, work) = measure(3, |_| {
        svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    });
    report("graph.reconcile_full_unchanged", &scale, &samples, &work.json());

    // A one-file pass over an unchanged file: the fixed cost of a pass.
    let (samples, work) = measure(7, |_| {
        svc.reconcile(&one(EDITED_FILE)).unwrap();
    });
    report("graph.reconcile_one_file_unchanged", &scale, &samples, &work.json());

    // A one-file pass after a local edit (a new labelled corollary each time).
    let edited = root.join(EDITED_FILE);
    let original = std::fs::read_to_string(&edited).unwrap();
    let (samples, work) = measure(7, |i| {
        let text = format!(
            "{original}\\begin{{corollary}}\\label{{cor:new{i}}}\nFrom Theorem~\\ref{{thm:5}} ({i}).\n\\end{{corollary}}\n"
        );
        std::fs::write(&edited, text).unwrap();
        let r = svc.reconcile(&one(EDITED_FILE)).unwrap();
        assert_eq!(r.read, 1, "{r:?}");
    });
    report("graph.reconcile_one_file_edit", &scale, &samples, &work.json());

    // A one-file pass after an edit to the large manuscript file.
    let big = root.join(BIG_FILE);
    let (samples, work) = measure(5, |i| {
        std::fs::write(&big, big_file_text(scale.big_file_declarations, &format!("edit {i}"))).unwrap();
        let r = svc.reconcile(&one(BIG_FILE)).unwrap();
        assert_eq!(r.read, 1, "{r:?}");
    });
    report("graph.reconcile_big_file_edit", &scale, &samples, &work.json());

    // One new file (containment is re-derived), then removed again.
    let (samples, work) = measure(5, |i| {
        let rel = format!("chapters/c5/extra{i}.md");
        std::fs::write(root.join(&rel), format!("# Extra {i}\n")).unwrap();
        let r = svc.reconcile(&one(&rel)).unwrap();
        assert_eq!(r.resources_added, 1, "{r:?}");
    });
    report("graph.reconcile_one_new_file", &scale, &samples, &work.json());

    // A typing pause in an unsaved buffer: the overlay of a small file and of
    // the large one.
    for (bench, file, text) in [
        ("graph.overlay_small_file", EDITED_FILE, original.clone()),
        ("graph.overlay_big_file", BIG_FILE, big_file_text(scale.big_file_declarations, "typed")),
    ] {
        let p = path(file);
        let (samples, work) = measure(7, |i| {
            let text = format!("{text}\n% typing {i}\n");
            let entry = overlay::compute(&svc, &p, 100 + i as u64, &text).unwrap().expect("a known resource");
            svc.set_overlay(svc.generation(), entry);
        });
        report(bench, &scale, &samples, &work.json());
        svc.clear_overlay(&p);
    }

    // Commit of a small curated write.
    let (samples, work) = measure(7, |i| {
        let base = svc.status().revision;
        take();
        let (_, out) = svc
            .propose_and_apply(
                Origin::User,
                &ChangeRequest {
                    base_revision: base,
                    operations: vec![GraphOperation::CreateEntity {
                        temp_id: "t".into(),
                        kind: EntityKind::Claim,
                        roles: vec![],
                        label: format!("Small write {i}"),
                        key: None,
                        attributes: BTreeMap::new(),
                        binding: None,
                    }],
                    summary: None,
                },
            )
            .unwrap();
        assert!(matches!(out, ApplyOutcome::Applied { .. }), "{out:?}");
    });
    report("graph.commit_small_write", &scale, &samples, &work.json());

    // Pending marks: one saved file, and a watcher batch (a folder switch).
    let (samples, work) = measure(7, |_| {
        svc.mark_pending(&[path(EDITED_FILE)]).unwrap();
    });
    report("graph.mark_pending_one", &scale, &samples, &work.json());
    let batch: Vec<WorkspacePath> = (0..scale.files * 12 / 100).map(|i| path(&format!("chapters/c{}/ch{i}.tex", i % 20))).collect();
    let (samples, work) = measure(5, |_| {
        svc.mark_pending(&batch).unwrap();
    });
    report("graph.mark_pending_batch", &scale, &samples, &format!("{{\"paths\":{},\"sql\":{}}}", batch.len(), work.json()));
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();

    // Reads.
    let (samples, work) = measure(7, |_| {
        let s = svc.snapshot(&GraphQuery::default()).unwrap();
        assert!(!s.entities.is_empty());
    });
    report("graph.snapshot_default", &scale, &samples, &work.json());
    let lemmas = GraphQuery { kinds: Some(vec![EntityKind::Lemma]), ..Default::default() };
    let mut returned = 0usize;
    let (samples, work) = measure(7, |_| {
        let s = svc.snapshot(&lemmas).unwrap();
        returned = s.relations.len();
    });
    report(
        "graph.snapshot_with_hub",
        &scale,
        &samples,
        &format!("{{\"relationsReturned\":{returned},\"sql\":{}}}", work.json()),
    );
    let (samples, work) = measure(7, |_| {
        svc.snapshot(&GraphQuery { text: Some("lemma 0000".into()), ..Default::default() }).unwrap();
    });
    report("graph.snapshot_search", &scale, &samples, &work.json());
    let around = GraphQuery {
        around: Some(Neighborhood {
            entity: bulk.hub_in.clone(),
            depth: 1,
            relation_kinds: None,
            direction: Direction::Both,
        }),
        ..Default::default()
    };
    let (samples, work) = measure(5, |_| {
        svc.snapshot(&around).unwrap();
    });
    report("graph.snapshot_hub_neighborhood", &scale, &samples, &work.json());
    let (samples, work) = measure(5, |_| {
        svc.entity_detail(&bulk.hub_in).unwrap();
    });
    report("graph.entity_detail_hub", &scale, &samples, &work.json());
    let (samples, work) = measure(5, |_| {
        svc.impact(&ImpactRequest { entities: vec![bulk.hub_in.clone()], max_depth: None, max_items: None }).unwrap();
    });
    report("graph.impact_hub", &scale, &samples, &work.json());
    let leaf = bulk.lemmas[bulk.lemmas.len() / 2].clone();
    let (samples, work) = measure(7, |_| {
        svc.impact(&ImpactRequest { entities: vec![leaf.clone()], max_depth: None, max_items: None }).unwrap();
    });
    report("graph.impact_leaf", &scale, &samples, &work.json());
    let (samples, work) = measure(5, |_| {
        svc.math_overview().unwrap();
    });
    report("graph.math_overview", &scale, &samples, &work.json());
    for (bench, file, offset) in [
        ("graph.inspect_at_offset_small_file", EDITED_FILE, Some(60)),
        ("graph.inspect_at_offset_big_file", BIG_FILE, Some(60)),
        ("graph.inspect_whole_big_file", BIG_FILE, None),
    ] {
        let req = InspectAtSourceRequest { path: path(file), offset, revision: None };
        let (samples, work) = measure(7, |_| {
            svc.inspect_at_source(&req).unwrap();
        });
        report(bench, &scale, &samples, &work.json());
    }

    // Reopen (validated open of the whole database).
    let t = Instant::now();
    svc.detach();
    svc.attach(&root);
    report("graph.reattach", &scale, &[t.elapsed().as_secs_f64() * 1000.0], "{}");
    let db = root.join(".exposition").join("graph.sqlite3");
    eprintln!("database size {} KiB", std::fs::metadata(db).unwrap().len() / 1024);
}
