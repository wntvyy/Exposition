//! Reads that decide something are complete, and reads that stay bounded say
//! what they left out: each test puts more records behind a read than its
//! former silent cap (20,000 relations per entity or per snapshot, 20,000
//! coverage rows, 20,000 bindings per file, 500 neighbours, 200 relations and
//! 200 entities per source inspection, 200 awaited link targets per file).

mod support;

use std::collections::BTreeSet;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::store::MAX_ROWS;
use exposition_graph::*;
use support::*;

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn binding(entity: &EntityId, at: &WorkspacePath) -> SourceBinding {
    SourceBinding {
        entity: entity.clone(),
        path: at.clone(),
        selector: SourceSelector::WholeFile,
        range: None,
        version: None,
        fingerprint: None,
        state: BindingState::Resolved,
        detail: None,
    }
}

const OVER: usize = MAX_ROWS + 50;

/// A conjecture with `OVER` rejected `assumes` suggestions (they sort before
/// every other kind and are followed by nothing) and one accepted dependent;
/// every one of the `OVER` lemmas is bound in one file.
struct Hub {
    conjecture: EntityId,
    dependent: EntityId,
    file: WorkspacePath,
}

fn hub(svc: &GraphService) -> Hub {
    let file = path("notes/big.md");
    svc.with_store(|store| {
        let w = store.writer()?;
        let conjecture = entity(EntityKind::Conjecture, "The conjecture");
        w.put_entity(&conjecture)?;
        for i in 0..OVER {
            let lemma = entity(EntityKind::Lemma, &format!("Lemma {i:06}"));
            w.put_entity(&lemma)?;
            w.set_bindings(&lemma.id, &[binding(&lemma.id, &file)])?;
            let mut rejected = relation(RelationKind::Assumes, &lemma.id, &conjecture.id);
            rejected.status = AssertionStatus::Rejected;
            w.put_relation(&rejected)?;
        }
        let dependent = entity(EntityKind::Theorem, "The dependent theorem");
        w.put_entity(&dependent)?;
        w.put_relation(&relation(RelationKind::DependsOn, &dependent.id, &conjecture.id))?;
        let mut delta = GraphDelta::default();
        w.record_change(&Origin::User, "hub", &mut delta)?;
        w.commit()?;
        Ok(Hub { conjecture: conjecture.id, dependent: dependent.id, file: file.clone() })
    })
    .unwrap()
}

#[test]
fn an_entity_with_more_than_twenty_thousand_relations_keeps_all_of_them_where_they_decide() {
    let (_data, _project, _root, svc) = attached();
    let hub = hub(&svc);

    // The store hands out every relation; the bounded read says it is bounded.
    svc.with_store(|store| {
        let all = store.relations_to(&hub.conjecture)?;
        assert_eq!(all.len(), OVER + 1, "every relation, not the first {MAX_ROWS}");
        assert!(all.iter().any(|r| r.kind == RelationKind::DependsOn && r.from == hub.dependent));
        let (page, more) = store.relations_to_bounded(&hub.conjecture, MAX_ROWS)?;
        assert_eq!(page.len(), MAX_ROWS);
        assert!(more, "the bounded read reports what it left out");
        let (page, more) = store.relations_from_bounded(&hub.dependent, MAX_ROWS)?;
        assert_eq!((page.len(), more), (1, false));
        // Every binding in the file (it holds more than the former cap).
        assert_eq!(store.bindings_at_path(&hub.file)?.len(), OVER);
        Ok(())
    })
    .unwrap();

    // Impact: the one real dependent sorts after 20,050 rejected suggestions
    // and is still found; nothing is reported as cut.
    let report = svc
        .impact(&ImpactRequest { entities: vec![hub.conjecture.clone()], max_depth: None, max_items: None })
        .unwrap();
    assert!(report.items.iter().any(|i| i.entity.id == hub.dependent), "the dependent is not lost behind the hub");
    assert_eq!(report.items.len(), 1, "{:?}", report.items.iter().map(|i| &i.entity.label).collect::<Vec<_>>());
    assert!(!report.truncated && !report.work_limit_reached);

    // The neighborhood reaches it too.
    let around = svc
        .snapshot(&GraphQuery {
            around: Some(Neighborhood {
                entity: hub.conjecture.clone(),
                depth: 1,
                relation_kinds: None,
                direction: Direction::Incoming,
            }),
            ..Default::default()
        })
        .unwrap();
    assert!(around.entities.iter().any(|e| e.id == hub.dependent));

    // The inspector's detail is a bounded presentation and says so.
    let detail = svc.entity_detail(&hub.conjecture).unwrap();
    assert_eq!(detail.incoming.len(), MAX_ROWS);
    assert!(detail.relations_truncated, "more relations exist than are listed");
    assert!(detail.neighbors_truncated, "more neighbours exist than are listed");
    assert_eq!(detail.neighbors.len(), 500);
    let small = svc.entity_detail(&hub.dependent).unwrap();
    assert!(!small.relations_truncated && !small.neighbors_truncated && !small.evidence_truncated);
    assert_eq!(small.outgoing.len(), 1);
}

#[test]
fn an_impact_walk_that_stops_at_a_hub_says_so() {
    let (_data, _project, _root, svc) = attached();
    // More accepted dependents than the walk follows.
    let target = svc
        .with_store(|store| {
            let w = store.writer()?;
            let target = entity(EntityKind::Lemma, "Depended on");
            w.put_entity(&target)?;
            for i in 0..OVER {
                let lemma = entity(EntityKind::Lemma, &format!("Dependent {i:06}"));
                w.put_entity(&lemma)?;
                w.put_relation(&relation(RelationKind::DependsOn, &lemma.id, &target.id))?;
            }
            let mut delta = GraphDelta::default();
            w.record_change(&Origin::User, "dependents", &mut delta)?;
            w.commit()?;
            Ok(target.id)
        })
        .unwrap();
    let report = svc.impact(&ImpactRequest { entities: vec![target], max_depth: None, max_items: Some(1000) }).unwrap();
    assert_eq!(report.items.len(), 1000);
    assert!(report.truncated, "more dependents exist than are listed");
}

#[test]
fn a_snapshot_and_the_mathematics_projection_say_when_their_relations_are_bounded() {
    let (_data, _project, _root, svc) = attached();
    // 150 lemmas, each depending on every other: 22,350 relations among
    // them, more than one read returns. One statement has a proof.
    let (statement, proof) = svc
        .with_store(|store| {
            let w = store.writer()?;
            let mut ids = Vec::new();
            for i in 0..150 {
                let lemma = entity(EntityKind::Lemma, &format!("Dense lemma {i:03}"));
                w.put_entity(&lemma)?;
                ids.push(lemma.id);
            }
            for a in &ids {
                for b in &ids {
                    if a != b {
                        w.put_relation(&relation(RelationKind::DependsOn, a, b))?;
                    }
                }
            }
            let proof = entity(EntityKind::Proof, "The proof");
            w.put_entity(&proof)?;
            w.put_relation(&relation(RelationKind::ProofOf, &proof.id, &ids[149]))?;
            let mut delta = GraphDelta::default();
            w.record_change(&Origin::User, "dense", &mut delta)?;
            w.commit()?;
            Ok((ids[149].clone(), proof.id))
        })
        .unwrap();
    let snapshot = svc.snapshot(&GraphQuery { kinds: Some(vec![EntityKind::Lemma]), ..Default::default() }).unwrap();
    assert_eq!(snapshot.entities.len(), 150);
    assert!(!snapshot.truncated, "every entity is listed");
    assert_eq!(snapshot.relations.len(), MAX_ROWS);
    assert!(snapshot.relations_truncated, "the relation list is bounded and says so");
    // Ordered by source, kind, id, as before.
    let order: Vec<(&EntityId, &str, &RelationId)> =
        snapshot.relations.iter().map(|r| (&r.from, r.kind.as_str(), &r.id)).collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);

    let math = svc.math_overview().unwrap();
    assert!(math.relations_truncated);
    assert_eq!(math.relations.len(), MAX_ROWS);
    // Each row's state comes from every relation, whatever the list holds.
    let row = math.rows.iter().find(|r| r.entity.id == statement).unwrap();
    assert_eq!(row.informal, InformalState::ProofLinked);
    assert!(math.rows.iter().any(|r| r.entity.id == proof));
    let unproved = math.rows.iter().filter(|r| r.informal == InformalState::NoProof).count();
    assert_eq!(unproved, 149);

    // A small graph reports nothing cut.
    let few = svc.snapshot(&GraphQuery { kinds: Some(vec![EntityKind::Proof]), ..Default::default() }).unwrap();
    assert!(!few.relations_truncated);
}

#[test]
fn relations_among_filters_in_the_database_and_keeps_the_order() {
    let (_data, _project, _root, svc) = attached();
    let bulk = bulk(&svc, 400, 1200, 300);
    count_sql(&svc);
    let mut ids: BTreeSet<EntityId> = bulk.lemmas.iter().take(10).cloned().collect();
    ids.insert(bulk.hub_out.clone());
    svc.with_store(|store| {
        // Reference: every outgoing relation of each entity, filtered here.
        let mut expected = Vec::new();
        for id in &ids {
            for r in store.relations_from(id)? {
                if ids.contains(&r.to) {
                    expected.push(r);
                }
            }
        }
        let ((found, more), work) = sql_of(|| store.relations_among_bounded(&ids, MAX_ROWS).unwrap());
        assert!(!more);
        assert_eq!(found, expected, "same relations in the same order");
        assert!(found.len() >= 10, "the hub's links into the set");
        // The hub's 290 other links are not returned to be discarded here.
        assert_eq!(work.rows as usize, found.len(), "{work:?}");
        assert_eq!(work.statements, 1, "{work:?}");
        let (page, more) = store.relations_among_bounded(&ids, 3)?;
        assert_eq!(page, expected[..3].to_vec());
        assert!(more);
        Ok(())
    })
    .unwrap();
}

#[test]
fn coverage_of_more_than_twenty_thousand_files_is_all_listed() {
    let (_data, _project, _root, svc) = attached();
    svc.with_store(|store| {
        let w = store.writer()?;
        for i in 0..OVER {
            w.set_coverage(&ResourceCoverage {
                path: path(&format!("d{}/f{i}.md", i % 50)),
                extractor: Some("markdown/1".into()),
                state: CoverageState::Complete,
                unresolved: 0,
                detail: None,
                content_hash: Some("h".into()),
            })?;
        }
        w.commit()?;
        let all = store.all_coverage()?;
        assert_eq!(all.len(), OVER);
        assert!(all.windows(2).all(|w| w[0].path < w[1].path), "ordered by path");
        Ok(())
    })
    .unwrap();
    assert_eq!(svc.status().coverage.resources as usize, OVER);
}

#[test]
fn a_source_inspection_says_when_it_lists_fewer_entities_or_relations_than_exist() {
    let (_data, _project, root, svc) = attached();
    std::fs::create_dir_all(root.join("big")).unwrap();
    std::fs::write(root.join(BIG_FILE), big_file_text(250, "")).unwrap();
    std::fs::write(root.join("small.tex"), "\\begin{lemma}\\label{s:1}\nSmall.\n\\end{lemma}\n").unwrap();
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    let whole = svc.inspect_at_source(&InspectAtSourceRequest { path: path(BIG_FILE), offset: None, revision: None });
    let whole = whole.unwrap();
    assert_eq!(whole.entities.len(), 200);
    assert!(whole.entities_truncated, "250 lemmas are bound in the file");
    let small =
        svc.inspect_at_source(&InspectAtSourceRequest { path: path("small.tex"), offset: None, revision: None });
    let small = small.unwrap();
    assert_eq!(small.entities.len(), 1);
    assert!(!small.entities_truncated);
    assert!(!small.entities[0].relations_truncated);
    // More relations on one entity than an inspection lists.
    let lemma = small.entities[0].entity.id.clone();
    svc.with_store(|store| {
        let w = store.writer()?;
        for i in 0..250 {
            let other = entity(EntityKind::Lemma, &format!("Other {i}"));
            w.put_entity(&other)?;
            w.put_relation(&relation(RelationKind::DependsOn, &other.id, &lemma))?;
        }
        let mut delta = GraphDelta::default();
        w.record_change(&Origin::User, "many", &mut delta)?;
        w.commit()?;
        Ok(())
    })
    .unwrap();
    let small =
        svc.inspect_at_source(&InspectAtSourceRequest { path: path("small.tex"), offset: None, revision: None });
    let small = small.unwrap();
    assert_eq!(small.entities[0].relations.len(), 200);
    assert!(small.entities[0].relations_truncated);
    // At a position: the innermost entity only, found without reading the others.
    let at = svc.inspect_at_source(&InspectAtSourceRequest { path: path(BIG_FILE), offset: Some(40), revision: None });
    let at = at.unwrap();
    assert_eq!(at.entities.len(), 1, "{:?}", at.entities.iter().map(|e| &e.entity.label).collect::<Vec<_>>());
    assert!(!at.entities_truncated);
}

#[test]
fn a_file_waiting_for_more_than_two_hundred_targets_is_resolved_when_any_of_them_appears() {
    let (_data, _project, root, svc) = attached();
    let mut text = String::from("\\section{Waiting}\n");
    for i in 0..250 {
        text.push_str(&format!("See Lemma~\\ref{{missing:{i}}}.\n"));
    }
    std::fs::write(root.join("waiting.tex"), text).unwrap();
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    let waiting = path("waiting.tex");
    let before = svc.with_store(|s| Ok(s.coverage(&waiting)?)).unwrap().unwrap();
    assert!(before.unresolved > 200, "{before:?}");
    // `missing:99` sorts last of the 250 awaited names: beyond a 200-name list.
    std::fs::write(root.join("defines.tex"), "\\begin{lemma}\\label{missing:99}\nNow stated.\n\\end{lemma}\n").unwrap();
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("defines.tex")]), force: false }).unwrap();
    let after = svc.with_store(|s| Ok(s.coverage(&waiting)?)).unwrap().unwrap();
    assert_eq!(after.unresolved, before.unresolved - 1, "the reference resolved without re-reading the file");
    // The same after a restart of the session (the stored list is what is read).
    svc.detach();
    svc.attach(&root);
    std::fs::write(root.join("defines2.tex"), "\\begin{lemma}\\label{missing:98}\nAlso stated.\n\\end{lemma}\n")
        .unwrap();
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("defines2.tex")]), force: false }).unwrap();
    let again = svc.with_store(|s| Ok(s.coverage(&waiting)?)).unwrap().unwrap();
    assert_eq!(again.unresolved, before.unresolved - 2);
}
