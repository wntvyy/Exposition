//! A reviewed `formalizes` link is reviewed against the statement as it read
//! then. When the statement changes, or can no longer be located, the link
//! stops reading as reviewed until a person confirms it again (which records
//! the new statement) or rejects it. Nothing re-reviews by itself, agents
//! cannot confirm, and the declaration's Lean evidence is never altered.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::*;

struct Project {
    _data: tempfile::TempDir,
    _dir: tempfile::TempDir,
    root: PathBuf,
    svc: Arc<GraphService>,
}

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

const LEAN: &str = "theorem oddSum_eq_sq (n : Nat) : n = n := by\n  rfl\n";
const DECLARATION: &str = "oddSum_eq_sq";

fn project(files: &[(&str, &str)]) -> Project {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (rel, body) in files {
        write(&root, rel, body);
    }
    write(&root, "lean/OddSums.lean", LEAN);
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    assert!(status.is_usable(), "{status:?}");
    let p = Project { _data: data, _dir: dir, root, svc };
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    p
}

impl Project {
    /// Save a new text of `path` and reconcile it, as a save does.
    fn save(&self, path: &str, body: &str) {
        write(&self.root, path, body);
        self.svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![wp(path)]), force: false }).unwrap();
    }

    fn by_key(&self, kind: EntityKind, key: &str) -> EntityId {
        self.svc
            .snapshot(&GraphQuery { kinds: Some(vec![kind]), limit: Some(2000), ..Default::default() })
            .unwrap()
            .entities
            .into_iter()
            .find(|e| e.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("no {kind:?} {key}"))
            .id
    }

    fn declaration(&self) -> EntityId {
        self.by_key(EntityKind::FormalDeclaration, DECLARATION)
    }

    fn change(&self, origin: Origin, ops: Vec<GraphOperation>) -> (ChangeProposal, ApplyOutcome) {
        let base = self.svc.status().revision;
        self.svc
            .propose_and_apply(origin, &ChangeRequest { base_revision: base, operations: ops, summary: None })
            .unwrap()
    }

    fn user(&self, ops: Vec<GraphOperation>) -> ApplyOutcome {
        let (p, out) = self.change(Origin::User, ops);
        assert!(p.valid, "{:?}", p.diagnostics);
        assert!(matches!(out, ApplyOutcome::Applied { .. }), "{out:?}");
        out
    }

    /// The `formalizes` link into `statement`, as every reader gets it.
    fn link(&self, statement: &EntityId) -> Relation {
        self.svc
            .entity_detail(statement)
            .unwrap()
            .incoming
            .into_iter()
            .find(|r| r.kind == RelationKind::Formalizes)
            .expect("a formalizes link")
    }

    fn state(&self, statement: &EntityId) -> LinkReviewState {
        self.link(statement).review.expect("an accepted link has a review standing").state
    }

    fn row(&self, statement: &EntityId) -> MathRow {
        self.svc.math_overview().unwrap().rows.into_iter().find(|r| &r.entity.id == statement).unwrap()
    }

    /// An agent suggests the link; returns the relation.
    fn suggest(&self, statement: &EntityId) -> Relation {
        let (p, out) = self.change(agent(), vec![formalizes(&self.declaration(), statement)]);
        assert!(p.valid && matches!(out, ApplyOutcome::Applied { .. }), "{:?}", p.diagnostics);
        let link = self.link(statement);
        assert_eq!(link.status, AssertionStatus::Suggested);
        assert_eq!(link.review, None, "a suggestion has no review to qualify");
        assert_eq!(link.reviewed_statement, None);
        link
    }

    fn set_status(&self, relation: &RelationId, status: AssertionStatus) {
        self.user(vec![GraphOperation::SetRelationStatus { relation: relation.clone(), status }]);
    }

    /// Formal evidence the Lean service would record for the declaration.
    fn record_evidence(&self) -> EvidenceId {
        let path = "lean/OddSums.lean";
        let file = self.svc.resource(&wp(path)).unwrap().unwrap();
        let digest = self.svc.entity_detail(&file.id).unwrap().coverage.unwrap().content_hash.unwrap();
        let origin = Origin::Service { service: "lean".into() };
        let evidence = Evidence {
            id: EvidenceId::mint(),
            kind: EvidenceKind::FormalCheck,
            subject: Some(self.declaration()),
            operation: OperationRef { kind: "leanAudit".into(), id: format!("{path}@1") },
            inputs: vec![VersionedInput {
                path: wp(path),
                content_hash: Some(digest.clone()),
                revision: Some(1),
                unsaved: false,
            }],
            outcome: EvidenceOutcome::Verdict { verdict: FormalVerdict::Checked },
            environment: vec![],
            outputs: vec![],
            formal: Some(FormalEvidence {
                declaration: DECLARATION.into(),
                declaration_kind: "theorem".into(),
                statement: format!("{DECLARATION} : True"),
                axioms: vec![],
                file_verdict: FormalVerdict::Checked,
                declaration_verdict: Some(FormalVerdict::Checked),
                toolchain: "leanprover/lean4:v4.35.0".into(),
                source_path: wp(path),
                source_digest: digest,
                revision: Some(1),
                dependency_fingerprints: None,
                dependencies: FormalDependencies::Unavailable { reason: "not reported".into() },
            }),
            recorded_by: origin.clone(),
            recorded_at_ms: 1,
        };
        self.svc.register_evidence(&origin, evidence).unwrap()
    }
}

fn agent() -> Origin {
    Origin::Agent { task_id: "task-1".into() }
}

fn formalizes(declaration: &EntityId, statement: &EntityId) -> GraphOperation {
    GraphOperation::AddRelation {
        kind: RelationKind::Formalizes,
        from: EntityRef::Id { id: declaration.clone() },
        to: EntityRef::Id { id: statement.clone() },
        evidence_kind: None,
        evidence: vec![],
        note: None,
    }
}

fn paper(lemma: &str) -> String {
    format!(
        "\\documentclass{{article}}\n\\begin{{document}}\n\\section{{Main}}\nSome prose.\n\\begin{{lemma}}\\label{{lem:odd}}\n{lemma}\n\\end{{lemma}}\n\\end{{document}}\n"
    )
}

const LEMMA: &str = "The sum of the first $n$ odd numbers is $n^2$.";

/// An extracted statement (a LaTeX lemma) keeps its identity when its text
/// changes, so the change itself is observed.
#[test]
fn a_changed_statement_flags_the_reviewed_link_until_a_person_confirms_it() {
    let p = project(&[("paper.tex", &paper(LEMMA))]);
    let lemma = p.by_key(EntityKind::Lemma, "lem:odd");
    let evidence = p.record_evidence();

    // Suggested: the verdict reaches the statement through an unreviewed link.
    let link = p.suggest(&lemma);
    let row = p.row(&lemma);
    assert_eq!(row.formal_association, FormalAssociationState::Suggested);
    assert_eq!(row.verdict, Some(FormalVerdict::Checked), "the verdict stays visible");
    assert_eq!(row.verdict_association, Some(FormalAssociationState::Suggested));
    let own = p.row(&p.declaration());
    assert_eq!(own.verdict_association, None, "a declaration's own verdict comes through no link");

    // The user accepts: the statement reviewed is recorded.
    p.set_status(&link.id, AssertionStatus::Accepted);
    let reviewed = p.link(&lemma);
    assert_eq!(reviewed.review.as_ref().unwrap().state, LinkReviewState::Current);
    assert_eq!(reviewed.review.as_ref().unwrap().reason, None);
    let first_digest = reviewed.reviewed_statement.clone().expect("the reviewed statement is recorded");
    let row = p.row(&lemma);
    assert_eq!(row.formal_association, FormalAssociationState::Reviewed);
    assert_eq!(row.association_reason, None);
    assert_eq!(row.verdict_association, Some(FormalAssociationState::Reviewed));

    // Changes that leave the statement's text alone do not flag: text around
    // it, and re-wrapping (the normalization declaration bindings use).
    p.save("paper.tex", &paper(LEMMA).replace("Some prose.", "Entirely different prose,\nover two lines."));
    assert_eq!(p.state(&lemma), LinkReviewState::Current);
    p.save("paper.tex", &paper("The sum of the first $n$ odd\n    numbers   is $n^2$."));
    assert_eq!(p.state(&lemma), LinkReviewState::Current, "identical after whitespace normalization");
    p.save("paper.tex", &paper(LEMMA));
    assert_eq!(p.state(&lemma), LinkReviewState::Current, "byte-identical again");

    // The statement changes: the link no longer reads as reviewed.
    p.save("paper.tex", &paper("The sum of the first $n$ odd numbers is $n^3$."));
    let flagged = p.link(&lemma);
    assert_eq!(flagged.status, AssertionStatus::Accepted, "the assertion is kept; its review is qualified");
    let review = flagged.review.clone().unwrap();
    assert_eq!(review.state, LinkReviewState::StatementChanged);
    assert!(review.needs_re_review());
    assert!(review.reason.as_deref().unwrap().starts_with("The statement changed since this link was reviewed."));
    assert_eq!(flagged.reviewed_statement.as_deref(), Some(first_digest.as_str()), "nothing re-reviews by itself");
    let row = p.row(&lemma);
    assert_eq!(row.formal_association, FormalAssociationState::NeedsReReview);
    assert_eq!(row.association_reason, review.reason);
    assert_eq!(row.verdict_association, Some(FormalAssociationState::NeedsReReview));
    // The evidence is what it was: only the link's review is qualified.
    assert_eq!(row.verdict, Some(FormalVerdict::Checked));
    assert_eq!(row.evidence.as_ref().map(|e| &e.id), Some(&evidence));
    let declaration = p.svc.entity_detail(&p.declaration()).unwrap();
    assert_eq!(declaration.evidence.len(), 1);
    assert_eq!(
        declaration.outgoing.iter().find(|r| r.kind == RelationKind::Formalizes).unwrap().review,
        flagged.review
    );
    // Impact through the link says so.
    let impact =
        p.svc.impact(&ImpactRequest { entities: vec![lemma.clone()], max_depth: None, max_items: None }).unwrap();
    let item = impact.items.iter().find(|i| i.entity.id == p.declaration()).unwrap();
    assert!(item.reason.ends_with("(reviewed link needs re-review)"), "{}", item.reason);

    // An agent can neither confirm the link nor replace it with a fresh one.
    let (proposal, out) = p.change(
        agent(),
        vec![GraphOperation::SetRelationStatus { relation: flagged.id.clone(), status: AssertionStatus::Accepted }],
    );
    assert!(!proposal.valid && !matches!(out, ApplyOutcome::Applied { .. }));
    assert_eq!(proposal.diagnostics[0].code, "graph.reviewOnly");
    let (proposal, _) = p.change(agent(), vec![GraphOperation::RemoveRelation { relation: flagged.id.clone() }]);
    assert_eq!(proposal.diagnostics[0].code, "graph.reviewOnly");
    let (proposal, _) = p.change(agent(), vec![formalizes(&p.declaration(), &lemma)]);
    assert_eq!(proposal.diagnostics[0].code, "graph.duplicateRelation");
    assert_eq!(p.link(&lemma), flagged, "the link is exactly as it was");

    // It survives a restart.
    p.svc.detach();
    assert!(p.svc.attach(&p.root).is_usable());
    assert_eq!(p.link(&lemma).review, flagged.review);
    assert_eq!(p.row(&lemma).formal_association, FormalAssociationState::NeedsReReview);

    // The user confirms: the new statement becomes the reviewed one.
    p.set_status(&flagged.id, AssertionStatus::Accepted);
    let confirmed = p.link(&lemma);
    assert_eq!(confirmed.review.as_ref().unwrap().state, LinkReviewState::Current);
    let second_digest = confirmed.reviewed_statement.clone().unwrap();
    assert_ne!(second_digest, first_digest);
    assert_eq!(p.row(&lemma).formal_association, FormalAssociationState::Reviewed);
    // The earlier text is now a change again.
    p.save("paper.tex", &paper(LEMMA));
    assert_eq!(p.state(&lemma), LinkReviewState::StatementChanged);

    // Rejecting ends it: no review is claimed, and the row shows no link.
    p.set_status(&flagged.id, AssertionStatus::Rejected);
    let rejected = p.link(&lemma);
    assert_eq!(
        (rejected.status, &rejected.review, &rejected.reviewed_statement),
        (AssertionStatus::Rejected, &None, &None)
    );
    let row = p.row(&lemma);
    assert_eq!(row.formal_association, FormalAssociationState::None);
    assert_eq!((row.verdict, row.verdict_association), (None, None));
}

const TYP: &str =
    "= Odd sums\n\nTheorem. The sum of the first n odd numbers is n squared.\n\nProof. By induction on n.\n";
const STATEMENT: &str = "Theorem. The sum of the first n odd numbers is n squared.";

fn region(text: &str, part: &str) -> NewBinding {
    let start = text.find(part).unwrap() as u32;
    NewBinding {
        path: wp("main.typ"),
        selector: SourceSelector::Region,
        range: Some(TextRange::new(start, start + part.len() as u32)),
    }
}

/// A theorem a person or agent bound to a region of a manuscript.
fn bound_theorem(p: &Project) -> EntityId {
    let out = p.user(vec![GraphOperation::CreateEntity {
        temp_id: "thm".into(),
        kind: EntityKind::Theorem,
        roles: vec![],
        label: "Sum of odd numbers".into(),
        key: None,
        attributes: Default::default(),
        binding: Some(region(TYP, STATEMENT)),
    }]);
    let ApplyOutcome::Applied { created, .. } = out else { unreachable!() };
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    created[0].id.clone()
}

/// A region is identified by its exact text: once the text is edited the
/// statement cannot be located, which is reported as such — never taken for
/// unchanged — and confirming cannot record a statement that is not there.
#[test]
fn a_statement_that_can_no_longer_be_located_is_reported_and_rebinding_then_confirming_clears_it() {
    let p = project(&[("main.typ", TYP)]);
    let theorem = bound_theorem(&p);
    let link = p.suggest(&theorem);
    p.set_status(&link.id, AssertionStatus::Accepted);
    assert_eq!(p.state(&theorem), LinkReviewState::Current);
    let first_digest = p.link(&theorem).reviewed_statement.unwrap();

    // The same bytes at another place of the file: unchanged.
    p.save("main.typ", &format!("A new first line.\n\n{TYP}"));
    assert_eq!(p.state(&theorem), LinkReviewState::Current);

    let edited = TYP.replace("n squared", "n cubed");
    p.save("main.typ", &edited);
    let review = p.link(&theorem).review.unwrap();
    assert_eq!(review.state, LinkReviewState::StatementMissing);
    assert!(review.needs_re_review());
    let reason = review.reason.clone().unwrap();
    assert!(reason.starts_with("The statement cannot be located"), "{reason}");
    assert!(reason.contains("main.typ: The bound text was changed or removed"), "{reason}");
    assert_eq!(p.row(&theorem).formal_association, FormalAssociationState::NeedsReReview);

    assert!(reason.contains("Rebind the statement to its current text, then confirm the link."), "{reason}");

    // Confirming now is refused, with what to do (there is no statement to
    // record): nothing changes, and the last statement actually reviewed
    // stays on record.
    let before = p.link(&theorem);
    let (proposal, out) = p.change(
        Origin::User,
        vec![GraphOperation::SetRelationStatus { relation: link.id.clone(), status: AssertionStatus::Accepted }],
    );
    assert!(!proposal.valid && !matches!(out, ApplyOutcome::Applied { .. }), "{out:?}");
    assert_eq!(proposal.diagnostics[0].code, "graph.statementNotLocated");
    let refusal = &proposal.diagnostics[0].message;
    assert!(refusal.starts_with("Nothing was confirmed. The statement cannot be located"), "{refusal}");
    assert!(refusal.contains("Rebind the statement to its current text, then confirm the link."), "{refusal}");
    let still = p.link(&theorem);
    assert_eq!(still, before, "a refused confirmation changes nothing");
    assert_eq!(still.review.as_ref().unwrap().state, LinkReviewState::StatementMissing);
    assert_eq!(still.reviewed_statement.as_deref(), Some(first_digest.as_str()));

    // After a restart it is still reported.
    p.svc.detach();
    assert!(p.svc.attach(&p.root).is_usable());
    assert_eq!(p.state(&theorem), LinkReviewState::StatementMissing);

    // An agent re-binding the theorem does not make the link reviewed.
    let (proposal, out) = p.change(
        agent(),
        vec![GraphOperation::BindSource {
            entity: EntityRef::Id { id: theorem.clone() },
            binding: region(&edited, &STATEMENT.replace("n squared", "n cubed")),
        }],
    );
    assert!(proposal.valid && matches!(out, ApplyOutcome::Applied { .. }), "{:?}", proposal.diagnostics);
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    assert_eq!(p.state(&theorem), LinkReviewState::StatementChanged, "located again, as a text nobody reviewed");

    // The user confirms against the located statement.
    p.set_status(&link.id, AssertionStatus::Accepted);
    let confirmed = p.link(&theorem);
    assert_eq!(confirmed.review.as_ref().unwrap().state, LinkReviewState::Current);
    assert_ne!(confirmed.reviewed_statement.as_deref(), Some(first_digest.as_str()));
}

/// Accepting a suggestion whose statement cannot be located reviews an
/// unknown text: it never reads as a current review.
#[test]
fn accepting_a_link_to_an_unlocatable_statement_records_no_reviewed_statement() {
    let p = project(&[("main.typ", TYP)]);
    let theorem = bound_theorem(&p);
    let link = p.suggest(&theorem);
    let edited = TYP.replace("n squared", "n cubed");
    p.save("main.typ", &edited);
    p.set_status(&link.id, AssertionStatus::Accepted);
    let accepted = p.link(&theorem);
    assert_eq!(accepted.reviewed_statement, None);
    assert_eq!(accepted.review.as_ref().unwrap().state, LinkReviewState::StatementMissing);
    p.user(vec![GraphOperation::BindSource {
        entity: EntityRef::Id { id: theorem.clone() },
        binding: region(&edited, &STATEMENT.replace("n squared", "n cubed")),
    }]);
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    assert_eq!(p.state(&theorem), LinkReviewState::Unrecorded, "located, but what was reviewed is unknown");
    p.set_status(&link.id, AssertionStatus::Accepted);
    assert_eq!(p.state(&theorem), LinkReviewState::Current);
}

/// A link reviewed before the reviewed statement was recorded: what was
/// reviewed is unknown. It is said so — neither claimed current nor flagged
/// as changed — until the user confirms it.
#[test]
fn a_link_reviewed_before_statements_were_recorded_reads_as_unknown_not_as_changed() {
    let p = project(&[("paper.tex", &paper(LEMMA))]);
    let lemma = p.by_key(EntityKind::Lemma, "lem:odd");
    p.record_evidence();
    // A link the user asserts directly is reviewed as asserted.
    p.user(vec![formalizes(&p.declaration(), &lemma)]);
    let link = p.link(&lemma);
    assert_eq!(link.review.as_ref().unwrap().state, LinkReviewState::Current);
    // As an older version stored it.
    p.svc
        .with_store(|store| {
            store.connection().execute("UPDATE relations SET reviewed_statement = NULL", []).unwrap();
            Ok(())
        })
        .unwrap();
    let legacy = p.link(&lemma);
    let review = legacy.review.clone().unwrap();
    assert_eq!(review.state, LinkReviewState::Unrecorded);
    assert!(!review.needs_re_review());
    assert!(review.reason.as_deref().unwrap().contains("was not recorded"), "{review:?}");
    let row = p.row(&lemma);
    assert_eq!(row.formal_association, FormalAssociationState::ReviewUnrecorded);
    assert_eq!(row.association_reason, review.reason);
    assert_eq!(row.verdict_association, Some(FormalAssociationState::ReviewUnrecorded));
    // A change cannot be detected, and none is claimed.
    p.save("paper.tex", &paper("Something else entirely."));
    assert_eq!(p.state(&lemma), LinkReviewState::Unrecorded);
    // Confirming records the statement as it is now.
    p.set_status(&legacy.id, AssertionStatus::Accepted);
    assert_eq!(p.state(&lemma), LinkReviewState::Current);
    p.save("paper.tex", &paper(LEMMA));
    assert_eq!(p.state(&lemma), LinkReviewState::StatementChanged);
}

/// Several links into one statement: the one needing attention is not
/// hidden behind another, and other relation kinds carry no review.
#[test]
fn only_accepted_formal_links_carry_a_review_standing() {
    let p = project(&[("paper.tex", &paper(LEMMA))]);
    let lemma = p.by_key(EntityKind::Lemma, "lem:odd");
    let out = p.user(vec![GraphOperation::CreateEntity {
        temp_id: "c".into(),
        kind: EntityKind::Claim,
        roles: vec![],
        label: "A claim".into(),
        key: None,
        attributes: Default::default(),
        binding: None,
    }]);
    let ApplyOutcome::Applied { created, .. } = out else { unreachable!() };
    let claim = created[0].id.clone();
    p.user(vec![GraphOperation::AddRelation {
        kind: RelationKind::DependsOn,
        from: EntityRef::Id { id: lemma.clone() },
        to: EntityRef::Id { id: claim.clone() },
        evidence_kind: None,
        evidence: vec![],
        note: None,
    }]);
    let depends = p.svc.entity_detail(&claim).unwrap().incoming.remove(0);
    assert_eq!((depends.status, depends.review, depends.reviewed_statement), (AssertionStatus::Accepted, None, None));
    // An unbound item is its label: renaming it changes the statement.
    p.user(vec![formalizes(&p.declaration(), &claim)]);
    assert_eq!(p.state(&claim), LinkReviewState::Current);
    let entity = p.svc.entity_detail(&claim).unwrap().entity;
    p.user(vec![GraphOperation::UpdateEntity {
        entity: claim.clone(),
        expected_revision: entity.revision,
        label: Some("Another claim".into()),
        attributes: None,
    }]);
    assert_eq!(p.state(&claim), LinkReviewState::StatementChanged);
}
