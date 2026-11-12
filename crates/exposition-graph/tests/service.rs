//! Behavior of the graph service through its public API: attach, typed
//! changes and their refusals, views, impact, and hostile databases.

use std::collections::BTreeMap;

use exposition_graph::*;

fn service() -> (tempfile::TempDir, tempfile::TempDir, std::sync::Arc<GraphService>) {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&std::fs::canonicalize(project.path()).unwrap());
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
    (data, project, svc)
}

fn create(temp: &str, kind: EntityKind, label: &str) -> GraphOperation {
    GraphOperation::CreateEntity {
        temp_id: temp.into(),
        kind,
        roles: vec![],
        label: label.into(),
        key: None,
        attributes: BTreeMap::new(),
        binding: None,
    }
}

fn temp(t: &str) -> EntityRef {
    EntityRef::Temp { temp_id: t.into() }
}

fn rel(kind: RelationKind, a: &str, b: &str) -> GraphOperation {
    GraphOperation::AddRelation { kind, from: temp(a), to: temp(b), evidence_kind: None, evidence: vec![], note: None }
}

fn apply(svc: &GraphService, origin: Origin, ops: Vec<GraphOperation>) -> (ChangeProposal, ApplyOutcome) {
    let base = svc.status().revision;
    svc.propose_and_apply(origin, &ChangeRequest { base_revision: base, operations: ops, summary: None }).unwrap()
}

#[test]
fn attach_creates_one_project_entity_and_reopen_preserves_curated_relations() {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let first = svc.attach(&root);
    assert_eq!(first.entities, 1, "the project entity");
    let project_id = first.project.clone().unwrap();
    let (_, out) = apply(
        &svc,
        Origin::User,
        vec![
            create("t", EntityKind::Theorem, "Main theorem"),
            create("l", EntityKind::Lemma, "Key lemma"),
            create("c", EntityKind::Collection, "Core results"),
            rel(RelationKind::DependsOn, "t", "l"),
            rel(RelationKind::MemberOf, "t", "c"),
        ],
    );
    assert!(matches!(out, ApplyOutcome::Applied { .. }), "{out:?}");
    svc.detach();
    let again = svc.attach(&root);
    assert_eq!(again.project, Some(project_id), "stable project identity");
    assert_eq!(again.entities, 4);
    assert_eq!(again.relations, 2);
    let snap = svc.snapshot(&GraphQuery { kinds: Some(vec![EntityKind::Theorem]), ..Default::default() }).unwrap();
    let theorem = &snap.entities[0];
    let detail = svc.entity_detail(&theorem.id).unwrap();
    assert_eq!(detail.outgoing.len(), 2);
    assert!(detail.outgoing.iter().all(|r| r.status == AssertionStatus::Accepted));
}

#[test]
fn invalid_connections_explain_why_and_change_nothing() {
    let (_d, _p, svc) = service();
    let (_, out) = apply(
        &svc,
        Origin::User,
        vec![create("p", EntityKind::Proof, "Proof"), create("d", EntityKind::Definition, "Def")],
    );
    let ApplyOutcome::Applied { created, .. } = out else { panic!() };
    let (p, d) = (created[0].id.clone(), created[1].id.clone());
    let before = svc.status().revision;
    let proposal = svc
        .propose_change(
            Origin::User,
            &ChangeRequest {
                base_revision: before,
                operations: vec![GraphOperation::AddRelation {
                    kind: RelationKind::ProofOf,
                    from: EntityRef::Id { id: p.clone() },
                    to: EntityRef::Id { id: d.clone() },
                    evidence_kind: None,
                    evidence: vec![],
                    note: None,
                }],
                summary: None,
            },
        )
        .unwrap();
    assert!(!proposal.valid);
    assert_eq!(proposal.diagnostics[0].code, "graph.endpointRule");
    assert!(
        proposal.diagnostics[0].message.contains("A proof targets a statement"),
        "{}",
        proposal.diagnostics[0].message
    );
    // An invalid proposal cannot be applied (it is not even stored).
    assert!(
        svc.apply_change(&Origin::User, &ApplyChangeRequest { proposal: proposal.id, expected_revision: before })
            .is_err()
    );
    assert_eq!(svc.status().revision, before);
    assert_eq!(svc.entity_detail(&p).unwrap().outgoing.len(), 0);
}

#[test]
fn people_cannot_create_indexed_kinds_or_structural_relations_and_agents_only_suggest() {
    let (_d, _p, svc) = service();
    let (prop, _) = apply(&svc, Origin::User, vec![create("r", EntityKind::Resource, "fake.tex")]);
    assert_eq!(prop.diagnostics[0].code, "graph.notCreatable");
    let (prop, _) = apply(&svc, Origin::User, vec![create("f", EntityKind::FormalDeclaration, "Nat.add_comm")]);
    assert_eq!(prop.diagnostics[0].code, "graph.notCreatable");

    let agent = Origin::Agent { task_id: "task-1".into() };
    let (_, out) = apply(
        &svc,
        agent.clone(),
        vec![
            create("a", EntityKind::Theorem, "A"),
            create("b", EntityKind::Lemma, "B"),
            rel(RelationKind::DependsOn, "a", "b"),
        ],
    );
    let ApplyOutcome::Applied { created, .. } = out else { panic!("{out:?}") };
    let detail = svc.entity_detail(&created[0].id).unwrap();
    let relation = detail.outgoing[0].clone();
    assert_eq!(relation.status, AssertionStatus::Suggested, "agent assertions stay suggestions");
    // The agent cannot accept its own suggestion; a person can.
    let (prop, _) = apply(
        &svc,
        agent,
        vec![GraphOperation::SetRelationStatus { relation: relation.id.clone(), status: AssertionStatus::Accepted }],
    );
    assert!(!prop.valid);
    let (_, out) = apply(
        &svc,
        Origin::User,
        vec![GraphOperation::SetRelationStatus { relation: relation.id.clone(), status: AssertionStatus::Accepted }],
    );
    assert!(matches!(out, ApplyOutcome::Applied { .. }));
}

#[test]
fn formal_check_evidence_cannot_be_minted_by_people_or_agents() {
    let (_d, _p, svc) = service();
    let (_, out) =
        apply(&svc, Origin::User, vec![create("t", EntityKind::Theorem, "T"), create("p", EntityKind::Proof, "P")]);
    let ApplyOutcome::Applied { created, .. } = out else { panic!() };
    let evidence_relation = |kind: EvidenceKind| GraphOperation::AddRelation {
        kind: RelationKind::EvidenceFor,
        from: EntityRef::Id { id: created[1].id.clone() },
        to: EntityRef::Id { id: created[0].id.clone() },
        evidence_kind: Some(kind),
        evidence: vec![],
        note: None,
    };
    let (prop, _) = apply(&svc, Origin::User, vec![evidence_relation(EvidenceKind::FormalCheck)]);
    assert_eq!(prop.diagnostics[0].code, "graph.serviceOwnedEvidence");
    let (prop, _) =
        apply(&svc, Origin::Agent { task_id: "task-2".into() }, vec![evidence_relation(EvidenceKind::FormalCheck)]);
    assert_eq!(prop.diagnostics[0].code, "graph.serviceOwnedEvidence");
    // A review-kind relation is fine.
    let (prop, out) = apply(&svc, Origin::User, vec![evidence_relation(EvidenceKind::Review)]);
    assert!(prop.valid && matches!(out, ApplyOutcome::Applied { .. }), "{prop:?}");
}

#[test]
fn stale_entity_revisions_conflict_and_apply_nothing() {
    let (_d, _p, svc) = service();
    let (_, out) = apply(&svc, Origin::User, vec![create("c", EntityKind::Claim, "Claim one")]);
    let ApplyOutcome::Applied { created, .. } = out else { panic!() };
    let id = created[0].id.clone();
    let rev = svc.entity_detail(&id).unwrap().entity.revision;
    let req = |label: &str| ChangeRequest {
        base_revision: 0,
        operations: vec![GraphOperation::UpdateEntity {
            entity: id.clone(),
            expected_revision: rev,
            label: Some(label.into()),
            attributes: None,
        }],
        summary: None,
    };
    let first = svc.propose_change(Origin::User, &req("Claim A")).unwrap();
    let second = svc.propose_change(Origin::User, &req("Claim B")).unwrap();
    assert!(matches!(
        svc.apply_change(&Origin::User, &ApplyChangeRequest { proposal: first.id, expected_revision: 0 }).unwrap(),
        ApplyOutcome::Applied { .. }
    ));
    let out =
        svc.apply_change(&Origin::User, &ApplyChangeRequest { proposal: second.id, expected_revision: 0 }).unwrap();
    assert!(matches!(out, ApplyOutcome::Conflict { .. }), "{out:?}");
    assert_eq!(svc.entity_detail(&id).unwrap().entity.label, "Claim A");
}

#[test]
fn a_proposal_can_only_be_applied_by_its_origin() {
    let (_d, _p, svc) = service();
    let agent = Origin::Agent { task_id: "task-9".into() };
    let proposal = svc
        .propose_change(
            agent,
            &ChangeRequest {
                base_revision: 0,
                operations: vec![create("q", EntityKind::Question, "Q")],
                summary: None,
            },
        )
        .unwrap();
    assert!(proposal.valid);
    let err = svc.apply_change(&Origin::User, &ApplyChangeRequest { proposal: proposal.id, expected_revision: 0 });
    assert_eq!(err.unwrap_err().code, "graph.unknownProposal");
}

#[test]
fn views_are_presentation_only_and_revision_checked() {
    let (_d, _p, svc) = service();
    let before = svc.status();
    let input = GraphViewInput {
        id: None,
        name: "Main".into(),
        scope: ViewScope::Project,
        nodes: vec![],
        viewport: Viewport { x: 10.0, y: -5.0, zoom: 1.5 },
        filters: ViewFilters::default(),
        hidden: vec![],
        collapsed: vec![],
        expected_revision: None,
    };
    let saved = svc.save_view(&input).unwrap();
    assert_eq!(saved.revision, 1);
    let again = svc
        .save_view(&GraphViewInput { id: Some(saved.id.clone()), expected_revision: Some(1), ..input.clone() })
        .unwrap();
    assert_eq!(again.revision, 2);
    let stale =
        svc.save_view(&GraphViewInput { id: Some(saved.id.clone()), expected_revision: Some(1), ..input.clone() });
    assert_eq!(stale.unwrap_err().code, "graph.staleView");
    assert!(
        svc.save_view(&GraphViewInput { viewport: Viewport { x: f64::NAN, y: 0.0, zoom: 1.0 }, ..input.clone() })
            .is_err()
    );
    assert!(svc.save_view(&GraphViewInput { viewport: Viewport { x: 0.0, y: 0.0, zoom: 100.0 }, ..input }).is_err());
    let after = svc.status();
    assert_eq!(before.revision, after.revision, "saving a layout does not change the graph");
}

#[test]
fn impact_follows_meaning_and_terminates_on_cycles() {
    let (_d, _p, svc) = service();
    let (_, out) = apply(
        &svc,
        Origin::User,
        vec![
            create("asm", EntityKind::Assumption, "Assumption A"),
            create("lem", EntityKind::Lemma, "Lemma"),
            create("thm", EntityKind::Theorem, "Theorem"),
            create("prf", EntityKind::Proof, "Proof of theorem"),
            create("cit", EntityKind::Claim, "Claim citing the theorem"),
            rel(RelationKind::Assumes, "lem", "asm"),
            rel(RelationKind::DependsOn, "thm", "lem"),
            rel(RelationKind::ProofOf, "prf", "thm"),
            rel(RelationKind::References, "cit", "thm"),
            // A cycle: the lemma depends on the theorem too.
            rel(RelationKind::DependsOn, "lem", "thm"),
        ],
    );
    let ApplyOutcome::Applied { created, .. } = out else { panic!("{out:?}") };
    let report =
        svc.impact(&ImpactRequest { entities: vec![created[0].id.clone()], max_depth: None, max_items: None }).unwrap();
    let labels: Vec<(String, ImpactAction)> = report.items.iter().map(|i| (i.entity.label.clone(), i.action)).collect();
    assert!(labels.contains(&("Lemma".into(), ImpactAction::NeedsReview)), "{labels:?}");
    assert!(labels.contains(&("Theorem".into(), ImpactAction::NeedsReview)), "{labels:?}");
    assert!(labels.contains(&("Claim citing the theorem".into(), ImpactAction::CitationReview)), "{labels:?}");
    assert!(labels.contains(&("Proof of theorem".into(), ImpactAction::NeedsReview)), "{labels:?}");
    assert!(!report.cycles.is_empty(), "the lemma/theorem cycle is reported");
    assert!(report.items.iter().all(|i| !i.reason.is_empty()));
}

#[test]
fn hostile_project_databases_leave_the_graph_unavailable_not_replaced() {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(root.join(".exposition")).unwrap();
    let db = root.join(".exposition").join("graph.sqlite3");
    std::fs::write(&db, b"not a database, but the user's bytes").unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    let GraphState::Unavailable { recovery, .. } = &status.state else { panic!("{status:?}") };
    assert!(recovery.contains(&RecoveryOption::StartNew));
    assert_eq!(std::fs::read(&db).unwrap(), b"not a database, but the user's bytes", "left unchanged");
    assert_eq!(svc.snapshot(&GraphQuery::default()).unwrap_err().code, "graph.unavailable");
    // Explicit recovery keeps the damaged file aside and starts a new graph.
    let status = svc.recover(&RecoveryOption::StartNew).unwrap();
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
    let aside: Vec<_> = std::fs::read_dir(root.join(".exposition"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("graph.damaged-"))
        .collect();
    assert_eq!(aside.len(), 1);
    assert_eq!(std::fs::read(aside[0].path()).unwrap(), b"not a database, but the user's bytes");
}

#[test]
fn service_batches_are_not_available_to_people_or_agents() {
    let (_d, _p, svc) = service();
    let err = svc.apply_service_batch(&Origin::User, &ServiceBatch::default()).unwrap_err();
    assert_eq!(err.code, "graph.notService");
}

#[test]
fn graph_storage_paths_are_recognised() {
    let p = |s: &str| exposition_core::path::WorkspacePath::parse(s).unwrap();
    assert!(is_graph_storage(&p(".exposition/graph.sqlite3")));
    assert!(is_graph_storage(&p(".exposition/graph.sqlite3-journal")));
    assert!(is_graph_storage(&p(".exposition/graph-backups/x.sqlite3")));
    assert!(!is_graph_storage(&p(".exposition/project.json")));
    assert!(!is_graph_storage(&p(".exposition/research/r1/fit.py")));
}

#[test]
fn ensure_resource_admits_by_policy_once_and_refuses_excluded_paths() {
    let (_d, _p, svc) = service();
    let p = |s: &str| exposition_core::path::WorkspacePath::parse(s).unwrap();
    let a = svc.ensure_resource(&p("paper/main.tex"), Some("abc"), false, "test").unwrap();
    assert_eq!(a.kind, EntityKind::Resource);
    assert_eq!(a.roles, vec![ResourceRole::Manuscript]);
    let again = svc.ensure_resource(&p("paper/main.tex"), Some("abc"), false, "test").unwrap();
    assert_eq!(a.id, again.id, "one identity per file");
    assert_eq!(
        svc.ensure_resource(&p(".exposition/runs/run-1.json"), None, true, "test").unwrap_err().code,
        "graph.excluded"
    );
    let out = p(".exposition/artifacts/run-1/output-0.png");
    assert_eq!(svc.ensure_resource(&out, None, false, "test").unwrap_err().code, "graph.excluded");
    let kept = svc.ensure_resource(&out, Some("h"), true, "research").unwrap();
    assert_eq!(kept.roles, vec![ResourceRole::Artifact]);
}
