//! R-m7 independent review: formal-declaration identity between the Lean
//! audit path (evidence recording) and source indexing, against the real Lean
//! toolchain and the real graph service. Skips (with a message) without the
//! pinned toolchain unless `EXPOSITION_REQUIRE_LEAN=1`.
//!
//! The spec (§5) requires one identity per referent: a declaration indexed
//! from the source and the same declaration admitted from an audit must be
//! one entity, never silently duplicated.

mod common;

use std::time::Duration;

use common::*;
use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::{Entity, EntityKind, GraphConfig, GraphQuery, GraphService, Lifecycle};
use exposition_lean::evidence;
use exposition_lean::{LeanCheckRequest, LeanCheckTarget};

const FIRST: Duration = Duration::from_secs(300);
const FILE: &str = "LeanBasic/Review.lean";

const SOURCE: &str = "namespace Review\n\
\n\
structure Box where\n\
  val : Nat\n\
\n\
instance : Inhabited Box := ⟨⟨0⟩⟩\n\
\n\
theorem «weird name» : 1 + 1 = 2 := rfl\n\
\n\
theorem plain : 2 + 2 = 4 := rfl\n\
\n\
end Review\n";

fn declarations(graph: &GraphService, path: &WorkspacePath) -> Vec<(Entity, String)> {
    let snap = graph
        .snapshot(&GraphQuery {
            kinds: Some(vec![EntityKind::FormalDeclaration]),
            include_inactive: false,
            limit: Some(2000),
            ..Default::default()
        })
        .unwrap();
    snap.entities
        .into_iter()
        .filter(|e| e.lifecycle != Lifecycle::Tombstoned)
        .filter_map(|e| {
            let detail = graph.entity_detail(&e.id).ok()?;
            let b = detail.bindings.iter().find(|b| &b.path == path)?;
            Some((e, format!("{:?} {:?} range={:?} detail={:?}", b.state, b.selector, b.range, b.detail)))
        })
        .collect()
}

fn dump(label: &str, decls: &[(Entity, String)]) {
    println!("--- {label}");
    for (e, state) in decls {
        println!("  {:<28} key={:<26?} origin={:?} binding={state}", e.label, e.key, e.origin);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn review_defect_audited_and_indexed_declarations_share_one_identity() {
    if !lean_available() {
        return;
    }
    let mut h = harness("lean-basic", Options::default()).await;
    let full = h.root.join("LeanBasic").join("Review.lean");
    std::fs::write(&full, SOURCE).unwrap();
    let path = WorkspacePath::parse(FILE).unwrap();

    let data = h.tmp.path().join("graph-data");
    let graph = GraphService::new(GraphConfig { data_dir: data });
    let root = std::fs::canonicalize(&h.root).unwrap();
    assert!(graph.attach(&root).is_usable());
    graph.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    let indexed = declarations(&graph, &path);
    dump("indexed from the saved file", &indexed);

    // Real audit of the saved file.
    let doc = h.open(FILE);
    let report = h
        .lean
        .check(LeanCheckRequest {
            target: LeanCheckTarget::Path { path: path.clone() },
            goals_at: vec![],
            timeout_ms: Some(300_000),
        })
        .await
        .unwrap();
    println!("audit status: {}", status_name(&report.status));
    let record = h.lean.audit_record(doc.clone()).await.unwrap().expect("an audit record");
    let names: Vec<String> = record.declarations.iter().map(|d| d.name.clone()).collect();
    println!("audited declarations: {names:?}");
    let out = evidence::record_evidence(&graph, &record, &names, true, 1).unwrap();
    println!("recorded {} refused {}", out.recorded.len(), out.refused.len());
    graph.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path.clone()]), force: true }).unwrap();
    let after = declarations(&graph, &path);
    dump("after recording audit evidence and reindexing", &after);

    // Every source declaration the indexer extracted must be the entity the
    // audit evidence attaches to: no second entity for the same declaration.
    // The audit reports each declaration's source range; two live entities
    // bound inside one audited declaration's range are two identities for it.
    let mut problems = Vec::new();
    // Text each live entity is bound to (UTF-16 ranges over the saved file).
    let utf16: Vec<u16> = SOURCE.encode_utf16().collect();
    let bound_text = |e: &Entity| -> Option<String> {
        let det = graph.entity_detail(&e.id).ok()?;
        let r = det.bindings.iter().find(|b| b.path == path)?.range?;
        Some(String::from_utf16_lossy(utf16.get(r.start as usize..(r.end as usize).min(utf16.len()))?))
    };
    for d in &record.declarations {
        let short = d.name.strip_prefix("Review.").unwrap_or(&d.name);
        let entities: Vec<String> = after
            .iter()
            .filter(|(e, _)| {
                e.key.as_deref() == Some(d.name.as_str())
                    || bound_text(e).is_some_and(|t| t.starts_with(&format!("theorem {short} ")))
            })
            .map(|(e, _)| format!("{} [key {:?}, {:?}]", e.label, e.key, e.origin))
            .collect();
        if entities.len() > 1 {
            problems.push(format!("audited declaration {} has {} entities: {entities:?}", d.name, entities.len()));
        }
    }
    // An audit-admitted declaration that is literally in the saved file must
    // not be reported as no longer declared.
    for (e, state) in &after {
        let in_text = e.key.as_deref().and_then(|k| k.strip_prefix("Review.")).is_some_and(|short| {
            SOURCE.contains(&format!("theorem {short} ")) || SOURCE.contains(&format!("theorem {short}:"))
        });
        if in_text && state.starts_with("Missing") {
            problems.push(format!("{} is in the saved file but its binding says: {state}", e.label));
        }
    }

    // Unsaved-buffer audit, then save: still one entity.
    let rev = h.replace(&doc, "end Review", "theorem fresh : 3 = 3 := rfl\n\nend Review");
    let _ = h.finished(&doc, rev, FIRST).await;
    let unsaved_record = h.lean.audit_record(doc.clone()).await.unwrap().expect("an audit of the buffer");
    assert_eq!(unsaved_record.revision, rev, "the buffer revision was audited");
    let out = evidence::record_evidence(&graph, &unsaved_record, &["Review.fresh".to_owned()], false, 2).unwrap();
    println!("unsaved audit: recorded {:?} refused {:?}", out.recorded, out.refused);
    h.workspace.save_document(&doc, false).unwrap();
    graph.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path.clone()]), force: true }).unwrap();
    let saved = declarations(&graph, &path);
    dump("after the unsaved audit and save", &saved);
    let fresh = saved.iter().filter(|(e, _)| e.key.as_deref() == Some("Review.fresh")).count();
    if fresh != 1 {
        problems.push(format!("`theorem fresh` (audited unsaved, then saved) has {fresh} entities"));
    } else {
        println!("ok: unsaved-buffer audit then save keeps one entity for Review.fresh");
    }
    let plain = after.iter().filter(|(e, _)| e.key.as_deref() == Some("Review.plain")).count();
    if plain == 1 {
        println!("ok: Review.plain has one entity across indexing and audit evidence");
    } else {
        problems.push(format!("`theorem plain` has {plain} entities"));
    }
    for p in &problems {
        println!("DEFECT: {p}");
    }
    assert!(problems.is_empty(), "{problems:?}");
}
