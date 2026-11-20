//! Relation semantics and authority (QC audit, RL-1 … RL-5): what a person
//! or an agent can and cannot establish by adding, relabelling, or
//! re-proposing a relationship, and where impact reports a cycle.

use std::collections::BTreeMap;
use std::sync::Arc;

use exposition_graph::*;

fn service() -> (tempfile::TempDir, tempfile::TempDir, Arc<GraphService>) {
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

fn relation(
    kind: RelationKind,
    from: &EntityId,
    to: &EntityId,
    evidence_kind: Option<EvidenceKind>,
    evidence: &[&EvidenceId],
) -> GraphOperation {
    GraphOperation::AddRelation {
        kind,
        from: EntityRef::Id { id: from.clone() },
        to: EntityRef::Id { id: to.clone() },
        evidence_kind,
        evidence: evidence.iter().map(|e| (*e).clone()).collect(),
        note: None,
    }
}

fn plain(kind: RelationKind, from: &EntityId, to: &EntityId) -> GraphOperation {
    relation(kind, from, to, None, &[])
}

fn apply(svc: &GraphService, origin: Origin, ops: Vec<GraphOperation>) -> (ChangeProposal, ApplyOutcome) {
    let base = svc.status().revision;
    svc.propose_and_apply(origin, &ChangeRequest { base_revision: base, operations: ops, summary: None }).unwrap()
}

fn created(svc: &GraphService, ops: Vec<GraphOperation>) -> Vec<EntityId> {
    match apply(svc, Origin::User, ops) {
        (_, ApplyOutcome::Applied { created, .. }) => created.into_iter().map(|c| c.id).collect(),
        (p, out) => panic!("{out:?} {:?}", p.diagnostics),
    }
}

fn applied(svc: &GraphService, origin: Origin, ops: Vec<GraphOperation>) {
    let (p, out) = apply(svc, origin, ops);
    assert!(matches!(out, ApplyOutcome::Applied { .. }), "{out:?} {:?}", p.diagnostics);
}

fn refused(svc: &GraphService, origin: Origin, op: GraphOperation) -> String {
    let (p, out) = apply(svc, origin, vec![op]);
    assert!(!p.valid && !matches!(out, ApplyOutcome::Applied { .. }), "accepted: {out:?}");
    p.diagnostics[0].code.clone()
}

fn agent(task: &str) -> Origin {
    Origin::Agent { task_id: task.into() }
}

/// A formal declaration admitted by the Lean service, with one Checked
/// formal-check record about it.
fn declaration_with_evidence(svc: &GraphService, name: &str) -> (EntityId, EvidenceId) {
    let lean = Origin::Service { service: "lean".into() };
    let out = svc
        .apply_service_batch(
            &lean,
            &ServiceBatch {
                entities: vec![EntityUpsert {
                    id: None,
                    kind: EntityKind::FormalDeclaration,
                    roles: vec![],
                    label: name.into(),
                    key: Some(name.into()),
                    attributes: BTreeMap::new(),
                    lifecycle: None,
                    tombstone_reason: None,
                    bindings: None,
                }],
                summary: "Admit declaration".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let declaration = out.entity_ids[0].clone();
    let evidence = svc.register_evidence(&lean, record(EvidenceKind::FormalCheck, Some(&declaration), &lean)).unwrap();
    (declaration, evidence)
}

fn record(kind: EvidenceKind, subject: Option<&EntityId>, by: &Origin) -> Evidence {
    Evidence {
        id: EvidenceId::mint(),
        kind,
        subject: subject.cloned(),
        operation: OperationRef { kind: "test".into(), id: "x".into() },
        inputs: vec![],
        outcome: if kind == EvidenceKind::FormalCheck {
            EvidenceOutcome::Verdict { verdict: FormalVerdict::Checked }
        } else {
            EvidenceOutcome::Succeeded
        },
        environment: vec![],
        outputs: vec![],
        formal: None,
        recorded_by: by.clone(),
        recorded_at_ms: 1,
    }
}

fn evidence_ids(svc: &GraphService, entity: &EntityId) -> Vec<EvidenceId> {
    svc.entity_detail(entity).unwrap().evidence.into_iter().map(|e| e.id).collect()
}

/// RL-1: a record supports something only through an `evidenceFor`
/// relation of the record's own kind that starts at the item the record is
/// about; no relation makes it read as another item's evidence.
#[test]
fn a_relation_cannot_move_or_relabel_an_evidence_record() {
    let (_d, _p, svc) = service();
    let ids = created(
        &svc,
        vec![
            create("t", EntityKind::Theorem, "T"),
            create("x", EntityKind::Claim, "X"),
            create("p", EntityKind::Proof, "P"),
        ],
    );
    let (t, x, p) = (&ids[0], &ids[1], &ids[2]);
    let (d, e) = declaration_with_evidence(&svc, "Demo.main");

    for origin in [Origin::User, agent("task-1")] {
        // A record on a relation that is not an evidence relation.
        assert_eq!(
            refused(&svc, origin.clone(), relation(RelationKind::DependsOn, t, x, None, &[&e])),
            "graph.evidenceNotCarried",
            "{origin:?}"
        );
        // A formal check relabelled as a review.
        assert_eq!(
            refused(&svc, origin.clone(), relation(RelationKind::EvidenceFor, p, t, Some(EvidenceKind::Review), &[&e])),
            "graph.evidenceKind",
            "{origin:?}"
        );
        // The declaration's record cited from another item.
        assert_eq!(
            refused(
                &svc,
                origin.clone(),
                relation(RelationKind::EvidenceFor, p, t, Some(EvidenceKind::FormalCheck), &[&e])
            ),
            "graph.evidenceSubject",
            "{origin:?}"
        );
    }
    assert!(evidence_ids(&svc, t).is_empty());

    // The legitimate route: the declaration supports the theorem, citing the
    // record that is about the declaration. It stays the declaration's.
    applied(
        &svc,
        agent("task-1"),
        vec![relation(RelationKind::EvidenceFor, &d, t, Some(EvidenceKind::FormalCheck), &[&e])],
    );
    let link =
        svc.entity_detail(t).unwrap().incoming.into_iter().find(|r| r.kind == RelationKind::EvidenceFor).unwrap();
    assert_eq!((link.status, &link.evidence), (AssertionStatus::Suggested, &vec![e.clone()]));
    assert!(evidence_ids(&svc, t).is_empty(), "a suggestion gives the theorem no evidence of its own");
    assert_eq!(evidence_ids(&svc, &d), vec![e.clone()]);
    // Impact names the cited record while the relation stands, and not once
    // the user rejected it.
    let impact = |of: &EntityId| {
        svc.impact(&ImpactRequest { entities: vec![of.clone()], max_depth: None, max_items: None }).unwrap()
    };
    assert!(impact(t).evidence.iter().any(|i| i.evidence == e));
    applied(
        &svc,
        Origin::User,
        vec![GraphOperation::SetRelationStatus { relation: link.id.clone(), status: AssertionStatus::Rejected }],
    );
    assert!(evidence_ids(&svc, t).is_empty());
    assert!(impact(t).evidence.iter().all(|i| i.evidence != e), "a rejected relation cites nothing");
    // A review record without a subject can be cited by a review relation.
    let agents = Origin::Service { service: "agents".into() };
    let review = svc.register_evidence(&agents, record(EvidenceKind::Review, None, &agents)).unwrap();
    applied(
        &svc,
        Origin::User,
        vec![relation(RelationKind::EvidenceFor, p, t, Some(EvidenceKind::Review), &[&review])],
    );
    assert!(evidence_ids(&svc, t).is_empty());
}

/// RL-1, stored rows: a relation written before the rule (or restored, or
/// imported with a copied graph) is kept, and is not read as support.
#[test]
fn a_stored_relation_that_breaks_the_citation_rule_is_kept_and_not_read_as_evidence() {
    let (_d, project, svc) = service();
    let ids = created(
        &svc,
        vec![
            create("t", EntityKind::Theorem, "T"),
            create("x", EntityKind::Claim, "X"),
            create("p", EntityKind::Proof, "P"),
            GraphOperation::AddRelation {
                kind: RelationKind::DependsOn,
                from: EntityRef::Temp { temp_id: "t".into() },
                to: EntityRef::Temp { temp_id: "x".into() },
                evidence_kind: None,
                evidence: vec![],
                note: None,
            },
            GraphOperation::AddRelation {
                kind: RelationKind::EvidenceFor,
                from: EntityRef::Temp { temp_id: "p".into() },
                to: EntityRef::Temp { temp_id: "t".into() },
                evidence_kind: Some(EvidenceKind::Review),
                evidence: vec![],
                note: None,
            },
        ],
    );
    let (t, p) = (&ids[0], &ids[2]);
    let (_declaration, e) = declaration_with_evidence(&svc, "Demo.main");
    // Rows as the previous validator admitted them: the record on a
    // dependency, and a formal check cited as a review from another item.
    let detail = svc.entity_detail(t).unwrap();
    let mut rows: Vec<Relation> = detail.outgoing.into_iter().chain(detail.incoming).collect();
    assert_eq!(rows.len(), 2);
    for r in &mut rows {
        r.evidence = vec![e.clone()];
    }
    svc.with_store(|s| {
        let w = exposition_graph::store::Writer::new(s.transaction()?);
        for r in &rows {
            w.put_relation(r)?;
        }
        w.commit()?;
        Ok(())
    })
    .unwrap();
    // Through persistence: the graph still opens, the rows are intact.
    svc.detach();
    let status = svc.attach(&std::fs::canonicalize(project.path()).unwrap());
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
    let detail = svc.entity_detail(t).unwrap();
    assert!(detail.outgoing.iter().chain(detail.incoming.iter()).all(|r| r.evidence == vec![e.clone()]));
    assert!(detail.evidence.is_empty(), "{:?}", detail.evidence);
    assert!(evidence_ids(&svc, p).is_empty());
    let impact = svc.impact(&ImpactRequest { entities: vec![t.clone()], max_depth: None, max_items: None }).unwrap();
    assert!(impact.evidence.is_empty(), "{:?}", impact.evidence);
}

/// RL-2: a relation followed in both directions is not a cycle, nor is a
/// relation between two roots; a real dependency cycle still is.
#[test]
fn impact_reports_only_real_cycles() {
    let (_d, _p, svc) = service();
    let ids = created(
        &svc,
        vec![
            create("t", EntityKind::Theorem, "T"),
            create("p", EntityKind::Proof, "P"),
            create("l", EntityKind::Lemma, "L"),
            create("s", EntityKind::Claim, "S"),
        ],
    );
    let (t, p, l, s) = (&ids[0], &ids[1], &ids[2], &ids[3]);
    let (d, _) = declaration_with_evidence(&svc, "Demo.main");
    applied(
        &svc,
        Origin::User,
        vec![
            plain(RelationKind::ProofOf, p, t),
            plain(RelationKind::Formalizes, &d, t),
            relation(RelationKind::EvidenceFor, p, t, Some(EvidenceKind::Review), &[]),
            plain(RelationKind::DependsOn, t, l),
        ],
    );
    let impact = |roots: &[&EntityId]| {
        svc.impact(&ImpactRequest {
            entities: roots.iter().map(|r| (*r).clone()).collect(),
            max_depth: None,
            max_items: None,
        })
        .unwrap()
    };
    for root in [t, p, &d, l] {
        let report = impact(&[root]);
        assert_eq!(report.cycles, Vec::<Vec<EntityId>>::new(), "root {root}");
        assert!(!report.items.is_empty());
    }
    // Two roots joined by a relation.
    assert_eq!(impact(&[t, l]).cycles, Vec::<Vec<EntityId>>::new());
    assert_eq!(impact(&[t, p]).cycles, Vec::<Vec<EntityId>>::new());
    // A real cycle: T depends on L, L depends on S, S depends on T.
    applied(&svc, Origin::User, vec![plain(RelationKind::DependsOn, l, s), plain(RelationKind::DependsOn, s, t)]);
    let report = impact(&[t]);
    assert_eq!(report.cycles.len(), 1, "{:?}", report.cycles);
    let cycle = &report.cycles[0];
    assert_eq!(cycle.first(), cycle.last());
    let mut members: Vec<&EntityId> = cycle[1..].iter().collect();
    members.sort();
    let mut expected = vec![t, l, s];
    expected.sort();
    assert_eq!(members, expected);
    // Asked of an item outside the cycle, the cycle is still found once.
    let report = impact(&[p]);
    assert_eq!(report.cycles.len(), 1, "{:?}", report.cycles);
}

/// RL-3: a person's rejection is durable against agents, and a relation's
/// revision never goes backwards.
#[test]
fn a_rejected_relation_is_not_replaced_by_an_agent_and_its_revision_never_decreases() {
    let (_d, _p, svc) = service();
    let ids = created(&svc, vec![create("t", EntityKind::Theorem, "T"), create("l", EntityKind::Lemma, "L")]);
    let (t, l) = (&ids[0], &ids[1]);
    applied(&svc, agent("task-a"), vec![plain(RelationKind::DependsOn, t, l)]);
    let row =
        || svc.entity_detail(t).unwrap().outgoing.into_iter().find(|r| r.kind == RelationKind::DependsOn).unwrap();
    let suggested = row();
    applied(
        &svc,
        Origin::User,
        vec![GraphOperation::SetRelationStatus { relation: suggested.id.clone(), status: AssertionStatus::Rejected }],
    );
    let rejected = row();
    assert_eq!(rejected.status, AssertionStatus::Rejected);
    assert!(rejected.revision > suggested.revision);

    // The same task, or another one, proposing it again is refused.
    for task in ["task-a", "task-b"] {
        assert_eq!(refused(&svc, agent(task), plain(RelationKind::DependsOn, t, l)), "graph.rejected", "{task}");
    }
    assert_eq!(row(), rejected, "the rejected record is unchanged");
    // Nor can an agent withdraw the record of the rejection.
    assert_eq!(
        refused(&svc, agent("task-a"), GraphOperation::RemoveRelation { relation: rejected.id.clone() }),
        "graph.reviewOnly"
    );

    // The person can assert it after all: the same record, a later revision.
    applied(&svc, Origin::User, vec![plain(RelationKind::DependsOn, t, l)]);
    let accepted = row();
    assert_eq!(accepted.id, rejected.id);
    assert_eq!((accepted.status, &accepted.origin), (AssertionStatus::Accepted, &Origin::User));
    assert!(accepted.revision > rejected.revision, "{} → {}", rejected.revision, accepted.revision);
    assert_eq!(accepted.created_at_ms, rejected.created_at_ms);
}
