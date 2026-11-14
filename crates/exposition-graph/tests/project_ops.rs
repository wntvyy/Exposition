//! P1 project operations on graph-held records (lane L2): which deletions
//! have concrete consequences, honest missing states without cascades,
//! recoverable deletion and restore with the same identity, curated
//! relationship removal that survives reindexing, workflow retirement with
//! pinned revisions, copies with fresh identities and no inherited evidence,
//! and cleanup of unreferenced tombstones.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::ops::{ImpactKind, OpImpact, OpTarget};
use exposition_core::path::WorkspacePath;
use exposition_graph::project_ops::{COPY_FROM, COPY_FROM_ENTITY};
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::workflow::presets::{NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use exposition_graph::workflow::registry::{AGENT_REVIEW, PortType, RESEARCH_PYTHON_BATCH};
use exposition_graph::workflow::{
    AuthorityCeiling, COMPOSITE_OPERATION, CodeLanguage, CompositeRef, ExecutionCeiling, InputSource, InputVersion,
    OutcomeKind, PortSource, StepOperation, StepPayload, StepPort, WORKFLOW_SCHEMA, WORKFLOW_SCHEMA_V2,
    WorkflowConnection, WorkflowInput, WorkflowOutcome, WorkflowStep,
};
use exposition_graph::*;
use sha2::Digest;

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

fn sha(body: &str) -> String {
    hex(&sha2::Sha256::digest(body.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const MAIN: &str = "\\documentclass{article}\n\\begin{document}\n\\begin{theorem}\\label{thm:main}\nAll is well.\n\\end{theorem}\n\\includegraphics{fig.png}\n\\bibliography{refs}\n\\end{document}\n";
const BIB: &str = "@article{key1, title={A paper}, year={2020}}\n";

fn project(files: &[(&str, &str)]) -> Project {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (rel, body) in files {
        write(&root, rel, body);
    }
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    assert!(svc.attach(&root).is_usable());
    let p = Project { _data: data, _dir: dir, root, svc };
    p.full();
    p
}

impl Project {
    fn full(&self) {
        self.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    }

    fn reindex(&self) {
        self.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    }

    fn paths(&self, paths: &[&str]) {
        self.svc
            .reconcile(&ReconcileOptions { scope: Scope::Paths(paths.iter().map(|p| wp(p)).collect()), force: false })
            .unwrap();
    }

    fn resource(&self, path: &str) -> Entity {
        self.svc.resource(&wp(path)).unwrap().unwrap_or_else(|| panic!("{path} is not a live resource"))
    }

    fn of_kind(&self, kind: EntityKind) -> Vec<Entity> {
        self.svc
            .snapshot(&GraphQuery {
                kinds: Some(vec![kind]),
                include_inactive: true,
                limit: Some(1000),
                ..Default::default()
            })
            .unwrap()
            .entities
    }

    fn apply(&self, ops: Vec<GraphOperation>) -> Vec<EntityId> {
        let base = self.svc.status().revision;
        let (p, out) = self
            .svc
            .propose_and_apply(Origin::User, &ChangeRequest { base_revision: base, operations: ops, summary: None })
            .unwrap();
        assert!(p.valid, "{:?}", p.diagnostics);
        match out {
            ApplyOutcome::Applied { created, .. } => created.into_iter().map(|c| c.id).collect(),
            other => panic!("{other:?}"),
        }
    }

    fn create(&self, kind: EntityKind, label: &str) -> EntityId {
        self.apply(vec![GraphOperation::CreateEntity {
            temp_id: "t".into(),
            kind,
            roles: vec![],
            label: label.into(),
            key: None,
            attributes: BTreeMap::new(),
            binding: None,
        }])
        .remove(0)
    }

    fn relate(&self, kind: RelationKind, from: &EntityId, to: &EntityId) -> RelationId {
        self.apply(vec![GraphOperation::AddRelation {
            kind,
            from: EntityRef::Id { id: from.clone() },
            to: EntityRef::Id { id: to.clone() },
            evidence_kind: None,
            evidence: vec![],
            note: None,
        }]);
        self.relation(kind, from, to).expect("relation added")
    }

    fn relation(&self, kind: RelationKind, from: &EntityId, to: &EntityId) -> Option<RelationId> {
        self.svc.entity_detail(from).unwrap().outgoing.into_iter().find(|r| r.kind == kind && &r.to == to).map(|r| r.id)
    }

    fn impacts(&self, paths: &[&str]) -> Vec<OpImpact> {
        self.svc.path_impacts(&paths.iter().map(|p| wp(p)).collect::<Vec<_>>()).unwrap()
    }

    fn remove(&self, rel: &str) {
        let p = self.root.join(rel);
        if p.is_dir() { std::fs::remove_dir_all(p).unwrap() } else { std::fs::remove_file(p).unwrap() }
    }

    fn save(&self, definition: WorkflowDefinition) -> SavedWorkflow {
        self.svc
            .save_workflow(
                &Origin::User,
                &SaveWorkflowRequest { id: None, expected_revision: None, definition },
                &ValidationContext::default(),
            )
            .unwrap()
    }

    fn evidence(
        &self,
        kind: EvidenceKind,
        subject: &EntityId,
        inputs: &[(&str, Option<String>)],
        outputs: &[&str],
    ) -> EvidenceId {
        let service = if kind == EvidenceKind::FormalCheck { "lean" } else { "research" };
        let origin = Origin::Service { service: service.into() };
        self.svc
            .register_evidence(
                &origin,
                Evidence {
                    id: EvidenceId::mint(),
                    kind,
                    subject: Some(subject.clone()),
                    operation: OperationRef { kind: "researchRun".into(), id: "run-0123456789abcdef".into() },
                    inputs: inputs
                        .iter()
                        .map(|(p, h)| VersionedInput {
                            path: wp(p),
                            content_hash: h.clone(),
                            revision: None,
                            unsaved: false,
                        })
                        .collect(),
                    outcome: EvidenceOutcome::Succeeded,
                    environment: vec![],
                    outputs: outputs
                        .iter()
                        .map(|p| EvidenceOutput {
                            path: wp(p),
                            content_hash: "00".repeat(32),
                            media_type: "image/svg+xml".into(),
                        })
                        .collect(),
                    formal: None,
                    recorded_by: origin.clone(),
                    recorded_at_ms: 1,
                },
            )
            .unwrap()
    }
}

fn kinds(impacts: &[OpImpact]) -> Vec<ImpactKind> {
    impacts.iter().map(|i| i.kind).collect()
}

fn numerical(script: &str, data: &str) -> WorkflowDefinition {
    draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        script: Some(wp(script)),
        data: Some(wp(data)),
        manuscript: None,
        lean_file: None,
    })
    .unwrap()
}

// ------------------------------------------------- workflow builders (v2) ----

fn step(id: &str, op: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Step {id}"),
        operation: StepOperation { id: op.into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
        composite: None,
        condition: None,
        repeat: None,
    }
}

fn feed(input: &str, to_step: &str, to_port: &str) -> WorkflowConnection {
    WorkflowConnection::Data {
        from: PortSource::Input { input: input.into() },
        to: StepPort { step: to_step.into(), port: to_port.into() },
    }
}

fn wire(from: &str, port: &str, to: &str, to_port: &str) -> WorkflowConnection {
    WorkflowConnection::Data {
        from: PortSource::Step { step: from.into(), port: port.into() },
        to: StepPort { step: to.into(), port: to_port.into() },
    }
}

fn outcome(id: &str, kind: OutcomeKind, step: &str, port: &str) -> WorkflowOutcome {
    WorkflowOutcome {
        id: id.into(),
        kind,
        label: format!("Outcome {id}"),
        from: StepPort { step: step.into(), port: port.into() },
        optional: false,
    }
}

fn input(id: &str, source: InputSource) -> WorkflowInput {
    WorkflowInput { id: id.into(), label: format!("Input {id}"), port_type: PortType::Dataset, source }
}

fn ceiling(model: bool) -> AuthorityCeiling {
    AuthorityCeiling {
        execution: ExecutionCeiling::Restricted,
        model_requests: model,
        retain_results: false,
        propose_changes: false,
    }
}

/// Child: input `data` (unset) -> restricted Python `fit` -> dataset outcome.
fn child_fit() -> WorkflowDefinition {
    let mut fit = step("fit", RESEARCH_PYTHON_BATCH);
    fit.payload = Some(StepPayload::Code { language: CodeLanguage::Python, source: "print(1)".into() });
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA.into(),
        title: "Fit".into(),
        description: String::new(),
        preset: None,
        inputs: vec![input("data", InputSource::Unset)],
        steps: vec![fit],
        connections: vec![feed("data", "fit", "data")],
        outcomes: vec![outcome("fitted", OutcomeKind::TransformedDataset, "fit", "dataset")],
        ceiling: ceiling(false),
        execution: None,
    }
}

/// Parent: data/obs.csv -> composite `sub` (child at `revision`) -> review.
fn parent(child: &WorkflowId, revision: u64) -> WorkflowDefinition {
    let mut sub = step("sub", COMPOSITE_OPERATION);
    sub.composite = Some(CompositeRef { workflow: child.clone(), revision });
    let mut review = step("review", AGENT_REVIEW);
    review.payload = Some(StepPayload::Prompt { text: "Summarize.".into() });
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA_V2.into(),
        title: "Parent".into(),
        description: String::new(),
        preset: None,
        inputs: vec![input(
            "data",
            InputSource::Resource { path: wp("data/obs.csv"), version: InputVersion::AtRunStart },
        )],
        steps: vec![sub, review],
        connections: vec![feed("data", "sub", "data"), wire("sub", "fitted", "review", "material")],
        outcomes: vec![outcome("report", OutcomeKind::InvestigationReport, "review", "report")],
        ceiling: ceiling(true),
        execution: None,
    }
}

// ------------------------------------------------------------------ impacts ----

#[test]
fn only_concrete_consequences_are_impacts_never_membership_or_containment() {
    let p = project(&[
        ("main.tex", MAIN),
        ("fig.png", "png"),
        ("refs.bib", BIB),
        ("notes/todo.txt", "todo"),
        ("scripts/a.py", "import b\nfrom . import b\n"),
        ("scripts/b.py", "x = 1\n"),
    ]);
    // Membership in a collection and containment in folders are not
    // consequences; neither is a script importing another script.
    let collection = p.create(EntityKind::Collection, "Reading list");
    p.relate(RelationKind::MemberOf, &p.resource("notes/todo.txt").id, &collection);
    let revision = p.svc.status().revision;
    for path in ["notes/todo.txt", "notes", "scripts/b.py", "scripts", "main.tex"] {
        assert_eq!(p.impacts(&[path]), vec![], "{path}");
    }

    // A manuscript's image and bibliography are its dependencies.
    let fig = p.impacts(&["fig.png"]);
    assert_eq!(kinds(&fig), vec![ImpactKind::ManuscriptDependency], "{fig:?}");
    assert_eq!(fig[0].target, OpTarget::Path { path: wp("fig.png") });
    assert_eq!(fig[0].consumers.len(), 1);
    assert_eq!(fig[0].consumers[0].label, "main.tex");
    assert_eq!(fig[0].consumers[0].kind, "document");
    assert!(fig[0].message.contains("main.tex") && fig[0].message.contains("fig.png"), "{}", fig[0].message);
    assert_eq!(kinds(&p.impacts(&["refs.bib"])), vec![ImpactKind::ManuscriptDependency]);

    // Deleting the manuscript together with what it uses leaves no consumer.
    assert_eq!(p.impacts(&["fig.png", "refs.bib", "main.tex"]), vec![]);
    // Planning changes nothing.
    assert_eq!(p.svc.status().revision, revision);
}

#[test]
fn a_required_workflow_input_is_an_impact_until_the_workflow_is_deleted() {
    let p = project(&[("data/obs.csv", "x\n1\n"), ("analysis/run.py", "print(1)\n"), ("data/other.csv", "y\n")]);
    let saved = p.save(numerical("analysis/run.py", "data/obs.csv"));
    for target in ["data/obs.csv", "data"] {
        let impacts = p.impacts(&[target]);
        let input = impacts.iter().find(|i| i.kind == ImpactKind::WorkflowInput).expect(target);
        assert_eq!(input.consumers[0].kind, "workflow");
        assert_eq!(input.consumers[0].id, saved.id.to_string());
        assert!(input.message.contains("cannot run"), "{}", input.message);
    }
    assert_eq!(p.impacts(&["data/other.csv"]), vec![], "an unused file has no consequence");
    p.svc.delete_workflow(&Origin::User, &saved.id, false, false).unwrap();
    assert_eq!(p.impacts(&["data/obs.csv"]), vec![], "a deleted workflow is not a consumer");
}

#[test]
fn retained_evidence_is_an_impact_only_while_a_surviving_item_relies_on_it() {
    let p = project(&[("data.csv", "a\n1\n"), ("results/fig.svg", "<svg/>"), ("scratch.csv", "z\n")]);
    let fig = p.resource("results/fig.svg");
    p.evidence(EvidenceKind::Computational, &fig.id, &[("data.csv", None)], &["results/fig.svg"]);
    let input = p.impacts(&["data.csv"]);
    assert_eq!(kinds(&input), vec![ImpactKind::RetainedEvidence], "{input:?}");
    assert_eq!(input[0].consumers[0].kind, "evidence");
    assert!(input[0].message.contains("verdicts stay as history"), "{}", input[0].message);
    // Deleting the retained result itself: nothing surviving relies on its
    // evidence (no claim cites it).
    assert_eq!(p.impacts(&["results/fig.svg"]), vec![]);
    assert_eq!(p.impacts(&["results", "data.csv"]), vec![]);
    assert_eq!(p.impacts(&["scratch.csv"]), vec![]);
}

#[test]
fn proceeding_leaves_honest_missing_states_without_cascading() {
    let p = project(&[
        ("main.tex", MAIN),
        ("fig.png", "png"),
        ("refs.bib", BIB),
        ("data/obs.csv", "x\n1\n"),
        ("analysis/run.py", "print(1)\n"),
        ("results/out.svg", "<svg/>"),
    ]);
    let fig = p.resource("fig.png");
    let main = p.resource("main.tex");
    let theorem = p.of_kind(EntityKind::Theorem).remove(0);
    let saved = p.save(numerical("analysis/run.py", "data/obs.csv"));
    let out = p.resource("results/out.svg");
    let evidence = p.evidence(
        EvidenceKind::Computational,
        &out.id,
        &[("data/obs.csv", Some(sha("x\n1\n")))],
        &["results/out.svg"],
    );
    let before = p.svc.entity_detail(&out.id).unwrap();
    assert!(before.evidence_freshness.iter().any(|(id, f)| id == &evidence && *f == Freshness::Current), "{before:?}");

    p.remove("fig.png");
    p.remove("data/obs.csv");
    p.paths(&["fig.png", "data/obs.csv"]);

    // The deleted file is tombstoned (no live identity), its consumers stay.
    assert!(p.svc.resource(&wp("fig.png")).unwrap().is_none());
    let fig_after = p.svc.entity_detail(&fig.id).unwrap().entity;
    assert_eq!(fig_after.lifecycle, Lifecycle::Tombstoned);
    assert_eq!(p.svc.entity_detail(&main.id).unwrap().entity.lifecycle, Lifecycle::Active);
    assert_eq!(p.svc.entity_detail(&theorem.id).unwrap().entity.lifecycle, Lifecycle::Active);
    // The workflow stays saved and reports its missing input.
    let wf = p.svc.workflow(&saved.id, None).unwrap();
    let exists = |path: &WorkspacePath| p.root.join(path.as_str()).is_file();
    let v = validate_workflow(&wf.definition, &ValidationContext { file_exists: Some(&exists), ..Default::default() });
    assert!(v.diagnostics.iter().any(|d| d.code == "workflow.inputMissing"), "{:?}", v.diagnostics);
    // The evidence stays (unchanged record), but no longer current.
    let after = p.svc.entity_detail(&out.id).unwrap();
    assert_eq!(after.evidence, before.evidence, "the evidence record is never rewritten");
    let freshness = after.evidence_freshness.iter().find(|(id, _)| id == &evidence).unwrap();
    assert!(matches!(freshness.1, Freshness::Stale { .. }), "{freshness:?}");
}

// ----------------------------------------------------------------- entities ----

#[test]
fn curated_entities_are_deleted_with_their_relationships_and_restored_with_the_same_identity() {
    let p = project(&[("main.tex", MAIN), ("fig.png", "png")]);
    let a = p.create(EntityKind::Claim, "Claim A");
    let b = p.create(EntityKind::Claim, "Claim B");
    let c = p.create(EntityKind::Claim, "Claim C");
    p.relate(RelationKind::DependsOn, &a, &b);
    p.relate(RelationKind::DependsOn, &c, &a);
    let check = p.svc.check_entity_delete(&a).unwrap();
    assert_eq!(check.relations, 2);
    assert_eq!(check.impacts, vec![], "plain dependencies need no decision");

    let rid = p.svc.delete_entity(&Origin::User, &a, false).unwrap().expect("recoverable");
    assert_eq!(rid, format!("entity:{a}"));
    assert!(p.svc.entity_detail(&a).is_err(), "really removed from the graph");
    assert!(p.svc.entity_detail(&b).unwrap().incoming.is_empty());
    assert!(p.svc.entity_detail(&c).unwrap().outgoing.is_empty());
    assert!(p.svc.recoverables().unwrap().iter().any(|r| r.id == rid && r.label == "Claim A" && r.bytes > 0));
    // A full reindex does not bring it back.
    p.reindex();
    assert!(p.svc.entity_detail(&a).is_err());

    let restored = p.svc.restore_recoverable(&Origin::User, &rid).unwrap();
    assert_eq!(restored.target, OpTarget::GraphEntity { entity: a.to_string() });
    assert_eq!(restored.skipped_relations, 0);
    let detail = p.svc.entity_detail(&a).unwrap();
    assert_eq!(detail.entity.label, "Claim A");
    assert_eq!(detail.outgoing.len() + detail.incoming.len(), 2);
    assert!(p.svc.recoverables().unwrap().is_empty());

    // Permanent deletion keeps nothing to restore.
    assert_eq!(p.svc.delete_entity(&Origin::User, &a, true).unwrap(), None);
    assert!(p.svc.recoverables().unwrap().is_empty());
    assert_eq!(p.svc.restore_recoverable(&Origin::User, &rid).unwrap_err().code, "ops.notFound");
}

#[test]
fn files_declarations_and_structure_are_not_deleted_as_metadata() {
    let p = project(&[("main.tex", MAIN), ("fig.png", "png"), ("refs.bib", BIB)]);
    let fig = p.resource("fig.png");
    let err = p.svc.check_entity_delete(&fig.id).unwrap_err();
    assert_eq!(err.code, "ops.useSourceFile");
    assert!(err.message.contains("fig.png"), "{}", err.message);
    let theorem = p.of_kind(EntityKind::Theorem).remove(0);
    assert_eq!(p.svc.check_entity_delete(&theorem.id).unwrap_err().code, "ops.sourceDerived");
    assert_eq!(p.svc.delete_entity(&Origin::User, &theorem.id, true).unwrap_err().code, "ops.sourceDerived");
    let reference = p.of_kind(EntityKind::Reference).remove(0);
    assert_eq!(p.svc.check_entity_delete(&reference.id).unwrap_err().code, "ops.sourceDerived");
    let project_entity = p.of_kind(EntityKind::Project).remove(0);
    assert_eq!(p.svc.check_entity_delete(&project_entity.id).unwrap_err().code, "ops.protected");
    // Nothing changed.
    assert_eq!(p.svc.entity_detail(&theorem.id).unwrap().entity.lifecycle, Lifecycle::Active);
    assert!(p.svc.resource(&wp("fig.png")).unwrap().is_some());
}

#[test]
fn only_a_surviving_item_supported_by_evidence_makes_an_entity_deletion_a_decision() {
    let p = project(&[("results/fig.svg", "<svg/>")]);
    let claim = p.create(EntityKind::Claim, "Claim");
    let finding = p.create(EntityKind::Finding, "Finding");
    let lone = p.create(EntityKind::Finding, "Lone finding");
    let fig = p.resource("results/fig.svg");
    let evidence = p.evidence(EvidenceKind::Computational, &fig.id, &[], &["results/fig.svg"]);
    // Only the subject of evidence, nothing else uses it: no decision.
    p.evidence(EvidenceKind::Computational, &lone, &[], &[]);
    assert_eq!(p.svc.check_entity_delete(&lone).unwrap().impacts, vec![]);
    // The finding provides evidence for the claim.
    p.apply(vec![GraphOperation::AddRelation {
        kind: RelationKind::EvidenceFor,
        from: EntityRef::Id { id: finding.clone() },
        to: EntityRef::Id { id: claim.clone() },
        evidence_kind: Some(EvidenceKind::Computational),
        evidence: vec![evidence.clone()],
        note: None,
    }]);
    let support = p.relation(RelationKind::EvidenceFor, &finding, &claim).unwrap();
    // Deleting the supported claim affects nothing that remains.
    assert_eq!(p.svc.check_entity_delete(&claim).unwrap().impacts, vec![]);
    // Deleting the link itself is the action, not a consequence.
    assert_eq!(p.svc.check_relation_delete(&support).unwrap().impacts, vec![]);
    // Deleting what provides the evidence leaves the claim without it.
    let check = p.svc.check_entity_delete(&finding).unwrap();
    assert_eq!(kinds(&check.impacts), vec![ImpactKind::RetainedEvidence]);
    assert_eq!(check.impacts[0].consumers[0].label, "Claim");
    // After deletion the evidence record itself is unchanged history.
    p.svc.delete_entity(&Origin::User, &finding, false).unwrap();
    let detail = p.svc.entity_detail(&fig.id).unwrap();
    assert!(detail.evidence.iter().any(|e| e.id == evidence));
}

#[test]
fn a_composite_whose_pinned_revision_was_purged_reads_as_incomplete() {
    let p = project(&[("data/obs.csv", "x\n")]);
    let child = p.save(child_fit());
    let parent = p.save(parent(&child.id, 1));
    assert_eq!(parent.validation.state, DraftState::WellFormed, "{:?}", parent.validation.diagnostics);
    p.svc.delete_workflow(&Origin::User, &child.id, true, true).unwrap();
    let read = p.svc.workflow(&parent.id, None).unwrap();
    assert_eq!(read.validation.state, DraftState::Incomplete);
    assert!(read.validation.diagnostics.iter().any(|d| d.code == "workflow.compositeMissing"));
    assert!(!read.validation.runnable);
    let summary = p.svc.workflows().unwrap().into_iter().find(|w| w.id == parent.id).unwrap();
    assert_eq!(summary.state, DraftState::Incomplete);
    assert!(summary.errors >= 1);
    // A workflow whose pins exist reads as saved.
    let other = p.save(child_fit());
    assert_eq!(p.svc.workflow(&other.id, None).unwrap().validation.state, other.validation.state);
}

// ------------------------------------------------------------ relationships ----

#[test]
fn removing_a_curated_relationship_survives_reindexing_while_source_facts_follow_the_source() {
    let p = project(&[("main.tex", MAIN), ("fig.png", "png")]);
    let theorem = p.of_kind(EntityKind::Theorem).remove(0);
    let claim = p.create(EntityKind::Claim, "Claim X");
    let curated = p.relate(RelationKind::DependsOn, &claim, &theorem.id);
    let uses = p.relation(RelationKind::UsesAsset, &p.resource("main.tex").id, &p.resource("fig.png").id).unwrap();

    let observed = p.svc.check_relation_delete(&uses).unwrap_err();
    assert_eq!(observed.code, "ops.sourceDerived");
    assert!(p.svc.check_relation_delete(&curated).unwrap().description.contains("depends on"));

    let rid = p.svc.delete_relation(&Origin::User, &curated, false).unwrap().unwrap();
    assert!(p.relation(RelationKind::DependsOn, &claim, &theorem.id).is_none());
    p.reindex();
    assert!(p.relation(RelationKind::DependsOn, &claim, &theorem.id).is_none(), "curated removal persists");
    assert!(
        p.relation(RelationKind::UsesAsset, &p.resource("main.tex").id, &p.resource("fig.png").id).is_some(),
        "the source-derived relationship still follows the source"
    );
    // Restorable while both ends exist.
    assert_eq!(
        p.svc.restore_recoverable(&Origin::User, &rid).unwrap().target,
        OpTarget::GraphRelation { relation: curated.to_string() }
    );
    assert_eq!(p.relation(RelationKind::DependsOn, &claim, &theorem.id), Some(curated));
}

// ----------------------------------------------------------------- workflows ----

#[test]
fn deleting_a_workflow_retires_it_keeps_pinned_revisions_and_restores_it() {
    let p = project(&[("data/obs.csv", "x\n")]);
    let child = p.save(child_fit());
    let parent = p.save(parent(&child.id, 1));
    let child_entity = child.entity.clone().unwrap();
    let pins = p.svc.workflow_pins(&child.id).unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!((pins[0].revision, &pins[0].by, pins[0].by_title.as_str()), (1, &parent.id, "Parent"));

    // Ordinary deletion: out of active use, recoverable.
    let deleted = p.svc.delete_workflow(&Origin::User, &child.id, false, false).unwrap();
    assert_eq!(deleted.recoverable.as_deref(), Some(format!("workflow:{}", child.id).as_str()));
    assert!(p.svc.workflows().unwrap().iter().all(|w| w.id != child.id));
    assert_eq!(p.svc.workflow(&child.id, None).unwrap_err().code, "workflow.unknown");
    let revise = p.svc.save_workflow(
        &Origin::User,
        &SaveWorkflowRequest { id: Some(child.id.clone()), expected_revision: None, definition: child_fit() },
        &ValidationContext::default(),
    );
    assert_eq!(revise.unwrap_err().code, "workflow.unknown", "a deleted workflow cannot be revised");
    assert!(p.svc.entity_detail(&child_entity).is_err());
    assert!(p.svc.workflow_definition(&child.id, 1).is_some(), "the composite still resolves its pin");
    assert_eq!(p.svc.delete_workflow(&Origin::User, &child.id, false, false).unwrap_err().code, "ops.alreadyDeleted");

    // Restore: same id, same entity, listed again.
    let restored = p.svc.restore_recoverable(&Origin::User, &format!("workflow:{}", child.id)).unwrap();
    assert_eq!(restored.target, OpTarget::Workflow { workflow: child.id.to_string() });
    assert!(p.svc.workflows().unwrap().iter().any(|w| w.id == child.id));
    assert_eq!(p.svc.workflow(&child.id, None).unwrap().entity, Some(child_entity));

    // Permanent deletion keeps the pinned revision by default...
    let purged = p.svc.delete_workflow(&Origin::User, &child.id, true, false).unwrap();
    assert_eq!((purged.purged.clone(), purged.kept.clone()), (vec![], vec![1]));
    assert!(p.svc.workflow_definition(&child.id, 1).is_some());
    assert!(p.svc.workflows().unwrap().iter().all(|w| w.id != child.id));
    assert!(p.svc.recoverables().unwrap().is_empty(), "nothing left to restore");
    // ...and removes it after the explicit pinned-revision decision.
    assert_eq!(p.svc.check_revision_purge(&child.id, 1).unwrap().len(), 1);
    let gone = p.svc.delete_workflow(&Origin::User, &child.id, true, true).unwrap();
    assert_eq!(gone.purged, vec![1]);
    assert!(p.svc.workflow_definition(&child.id, 1).is_none(), "the composite now shows its child missing");
    assert!(!p.svc.workflow_retired(&child.id), "a workflow with no revisions left is forgotten");
    // The parent is untouched (it cannot run until rebound, but it is kept).
    assert_eq!(p.svc.workflow(&parent.id, None).unwrap().revision, 1);
}

#[test]
fn the_current_revision_is_never_purged_alone() {
    let p = project(&[]);
    let saved = p.save(child_fit());
    let mut second = child_fit();
    second.description = "v2".into();
    let v2 = p
        .svc
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: Some(saved.id.clone()), expected_revision: Some(1), definition: second },
            &ValidationContext::default(),
        )
        .unwrap();
    assert_eq!(v2.revision, 2);
    assert_eq!(p.svc.check_revision_purge(&saved.id, 2).unwrap_err().code, "ops.latestRevision");
    assert_eq!(p.svc.check_revision_purge(&saved.id, 1).unwrap(), vec![]);
    p.svc.purge_workflow_revision(&Origin::User, &saved.id, 1).unwrap();
    assert!(p.svc.workflow_definition(&saved.id, 1).is_none());
    assert_eq!(p.svc.workflow(&saved.id, None).unwrap().revision, 2);
}

#[test]
fn duplicating_a_workflow_makes_a_new_identity_with_remapped_steps_and_explicit_pins() {
    let p = project(&[("data/obs.csv", "x\n")]);
    let child = p.save(child_fit());
    let parent = p.save(parent(&child.id, 1));
    let resolve = |id: &WorkflowId, revision: u64| p.svc.workflow_definition(id, revision);
    let ctx = ValidationContext { resolve_workflow: Some(&resolve), ..Default::default() };
    let copy = p.svc.duplicate_workflow(&Origin::User, &parent.id, None, &ctx).unwrap();
    assert_ne!(copy.id, parent.id);
    assert_eq!(copy.revision, 1);
    assert_eq!(copy.definition.title, "Parent (copy)");
    assert!(!copy.imported);
    let old: Vec<&str> = parent.definition.steps.iter().map(|s| s.id.as_str()).collect();
    let new: Vec<&str> = copy.definition.steps.iter().map(|s| s.id.as_str()).collect();
    assert!(new.iter().all(|id| !old.contains(id)), "fresh step ids: {new:?}");
    // Every internal reference points at the copy's own steps.
    for c in &copy.definition.connections {
        if let WorkflowConnection::Data { from, to } = c {
            assert!(new.contains(&to.step.as_str()));
            if let PortSource::Step { step, .. } = from {
                assert!(new.contains(&step.as_str()));
            }
        }
    }
    assert!(copy.definition.outcomes.iter().all(|o| new.contains(&o.from.step.as_str())));
    // External references stay explicit: the same pinned child and inputs.
    let pin = copy.definition.steps.iter().find_map(|s| s.composite.clone()).unwrap();
    assert_eq!(pin, CompositeRef { workflow: child.id.clone(), revision: 1 });
    assert_eq!(copy.definition.inputs, parent.definition.inputs);
    assert_eq!(copy.validation.state, parent.validation.state);
    assert!(!copy.validation.runnable, "a copy is never runnable by itself");
    assert_ne!(copy.entity, parent.entity);
}

// ------------------------------------------------------------------- copies ----

#[test]
fn duplicating_curated_items_remaps_internal_relations_and_never_attaches_copies_to_consumers() {
    let p = project(&[("main.tex", MAIN), ("fig.png", "png")]);
    let a = p.create(EntityKind::Claim, "Claim A");
    let b = p.create(EntityKind::Claim, "Claim B");
    let c = p.create(EntityKind::Claim, "External input C");
    let d = p.create(EntityKind::Claim, "Consumer D");
    p.relate(RelationKind::DependsOn, &a, &b);
    p.relate(RelationKind::DependsOn, &a, &c);
    p.relate(RelationKind::DependsOn, &d, &a);
    let map: BTreeMap<EntityId, EntityId> =
        p.svc.duplicate_entities(&Origin::User, &[a.clone(), b.clone()]).unwrap().into_iter().collect();
    let (a2, b2) = (map[&a].clone(), map[&b].clone());
    assert!(a2 != a && b2 != b);
    let detail = p.svc.entity_detail(&a2).unwrap();
    assert_eq!(detail.entity.label, "Claim A (copy)");
    assert_eq!(detail.entity.key, None);
    assert_eq!(detail.entity.attributes.get(COPY_FROM_ENTITY), Some(&a.to_string()));
    assert!(detail.evidence.is_empty());
    let targets: Vec<&EntityId> = detail.outgoing.iter().map(|r| &r.to).collect();
    assert!(targets.contains(&&b2), "internal relation remapped onto the copy");
    assert!(targets.contains(&&c), "external input kept explicitly");
    assert!(!targets.contains(&&b), "never to the original of a copied item");
    assert!(detail.incoming.is_empty(), "nothing consumes the copy");
    assert!(p.relation(RelationKind::DependsOn, &d, &a).is_some(), "the original is untouched");

    let theorem = p.of_kind(EntityKind::Theorem).remove(0);
    let err = p.svc.check_entity_duplicate(&theorem.id).unwrap_err();
    assert_eq!(err.code, "ops.sourceAnchored");
    assert!(err.message.contains("Copy Reference") && err.message.contains("Copy Text"), "{}", err.message);
    assert_eq!(p.svc.check_entity_duplicate(&p.resource("fig.png").id).unwrap_err().code, "ops.useSourceFile");
    assert_eq!(
        p.svc.duplicate_entities(&Origin::User, std::slice::from_ref(&theorem.id)).unwrap_err().code,
        "ops.sourceAnchored"
    );
}

#[test]
fn a_copied_file_gets_a_fresh_identity_with_provenance_and_no_inherited_evidence_or_verdict() {
    let lean = "theorem foo : True := trivial\n";
    let p = project(&[("Proofs/A.lean", lean), ("results/fig.svg", "<svg/>")]);
    let original = p.resource("Proofs/A.lean");
    let decl = p.svc.formal_declaration(&wp("Proofs/A.lean"), "foo").unwrap().expect("declaration indexed");
    let formal = Evidence {
        id: EvidenceId::mint(),
        kind: EvidenceKind::FormalCheck,
        subject: Some(decl.id.clone()),
        operation: OperationRef { kind: "leanAudit".into(), id: "audit-1".into() },
        inputs: vec![VersionedInput {
            path: wp("Proofs/A.lean"),
            content_hash: Some(sha(lean)),
            revision: None,
            unsaved: false,
        }],
        outcome: EvidenceOutcome::Verdict { verdict: FormalVerdict::Checked },
        environment: vec![],
        outputs: vec![],
        formal: Some(FormalEvidence {
            declaration: "foo".into(),
            declaration_kind: "theorem".into(),
            statement: "True".into(),
            axioms: vec![],
            file_verdict: FormalVerdict::Checked,
            declaration_verdict: Some(FormalVerdict::Checked),
            toolchain: "lean4".into(),
            source_path: wp("Proofs/A.lean"),
            source_digest: sha(lean),
            revision: None,
            dependency_fingerprints: None,
            dependencies: FormalDependencies::Unavailable { reason: "test".into() },
        }),
        recorded_by: Origin::Service { service: "lean".into() },
        recorded_at_ms: 1,
    };
    p.svc.register_evidence(&Origin::Service { service: "lean".into() }, formal).unwrap();
    let fig = p.resource("results/fig.svg");
    p.evidence(EvidenceKind::Computational, &fig.id, &[], &["results/fig.svg"]);

    std::fs::copy(p.root.join("Proofs/A.lean"), p.root.join("Proofs/B.lean")).unwrap();
    std::fs::create_dir_all(p.root.join("copies")).unwrap();
    std::fs::copy(p.root.join("results/fig.svg"), p.root.join("copies/fig.svg")).unwrap();
    let n = p.svc.record_copies(&[(wp("Proofs/A.lean"), wp("Proofs/B.lean")), (wp("results"), wp("copies"))]).unwrap();
    assert_eq!(n, 2);
    let copy = p.resource("Proofs/B.lean");
    assert_ne!(copy.id, original.id);
    assert_eq!(copy.attributes.get(COPY_FROM).map(String::as_str), Some("Proofs/A.lean"));
    assert_eq!(copy.attributes.get(COPY_FROM_ENTITY), Some(&original.id.to_string()));
    assert!(p.svc.entity_detail(&copy.id).unwrap().evidence.is_empty());
    let fig_copy = p.resource("copies/fig.svg");
    assert_eq!(fig_copy.attributes.get(COPY_FROM).map(String::as_str), Some("results/fig.svg"));
    assert!(p.svc.entity_detail(&fig_copy.id).unwrap().evidence.is_empty(), "no inherited computational evidence");
    // The copied declaration is its own identity with no verdict.
    let copied_decl = p.svc.formal_declaration(&wp("Proofs/B.lean"), "foo").unwrap().expect("copy indexed");
    assert_ne!(copied_decl.id, decl.id);
    assert!(p.svc.entity_detail(&copied_decl.id).unwrap().evidence.is_empty());
    let overview = p.svc.math_overview().unwrap();
    let row = overview.rows.iter().find(|r| r.entity.id == copied_decl.id).expect("row for the copy");
    assert_eq!(row.verdict, None, "no unearned verification");
    let original_row = overview.rows.iter().find(|r| r.entity.id == decl.id).unwrap();
    assert_eq!(original_row.verdict, Some(FormalVerdict::Checked), "the original keeps its verdict");
}

#[test]
fn an_unrelated_new_file_at_a_deleted_path_never_inherits_identity_or_current_evidence() {
    let p = project(&[("data.csv", "a\n1\n"), ("results/fig.svg", "<svg/>")]);
    let old = p.resource("data.csv");
    let fig = p.resource("results/fig.svg");
    let evidence = p.evidence(EvidenceKind::Computational, &fig.id, &[("data.csv", Some(sha("a\n1\n")))], &[]);
    p.remove("data.csv");
    p.paths(&["data.csv"]);
    write(&p.root, "data.csv", "something else\n");
    p.paths(&["data.csv"]);
    let new = p.resource("data.csv");
    assert_ne!(new.id, old.id, "an unrelated file is a new identity");
    let detail = p.svc.entity_detail(&fig.id).unwrap();
    let freshness = detail.evidence_freshness.iter().find(|(id, _)| id == &evidence).unwrap();
    assert!(!matches!(freshness.1, Freshness::Current), "{freshness:?}");
}

// --------------------------------------------------------------- tombstones ----

#[test]
fn unreferenced_tombstones_are_eligible_for_cleanup_and_referenced_ones_stay() {
    let p = project(&[("a.txt", "a"), ("b.txt", "b"), ("c.txt", "c")]);
    let (a, b) = (p.resource("a.txt").id, p.resource("b.txt").id);
    let collection = p.create(EntityKind::Collection, "Kept");
    p.relate(RelationKind::MemberOf, &b, &collection);
    p.remove("a.txt");
    p.remove("b.txt");
    p.paths(&["a.txt", "b.txt"]);
    let eligible = p.svc.unreferenced_tombstones(None).unwrap();
    assert!(eligible.contains(&a), "{eligible:?}");
    assert!(!eligible.contains(&b), "a tombstone a surviving record refers to is kept");
    assert_eq!(p.svc.cleanup_tombstones(Some(&[wp("a.txt")])).unwrap(), 1);
    assert!(p.svc.entity_detail(&a).is_err(), "the metadata is gone");
    assert_eq!(p.svc.entity_detail(&b).unwrap().entity.lifecycle, Lifecycle::Tombstoned);
    assert!(p.svc.resource(&wp("c.txt")).unwrap().is_some());
}

// -------------------------------------------------------------------- views ----

#[test]
fn saved_views_are_deleted_restored_and_duplicated_as_presentation_only() {
    let p = project(&[("a.txt", "a")]);
    let view = p
        .svc
        .save_view(&GraphViewInput {
            id: None,
            name: "Layout".into(),
            scope: ViewScope::Project,
            nodes: vec![],
            viewport: Viewport { x: 0.0, y: 0.0, zoom: 1.0 },
            filters: ViewFilters::default(),
            hidden: vec![],
            collapsed: vec![],
            expected_revision: None,
        })
        .unwrap();
    let revision = p.svc.status().revision;
    let copy = p.svc.duplicate_view(&view.id).unwrap();
    assert_ne!(copy.id, view.id);
    assert_eq!(copy.name, "Layout (copy)");
    let rid = p.svc.delete_view_recoverable(&view.id, false).unwrap().unwrap();
    assert!(p.svc.view(&view.id).is_err());
    assert_eq!(p.svc.status().revision, revision, "presentation never changes the graph revision");
    p.svc.restore_recoverable(&Origin::User, &rid).unwrap();
    assert_eq!(p.svc.view(&view.id).unwrap().name, "Layout");
    assert_eq!(p.svc.delete_view_recoverable(&copy.id, true).unwrap(), None);
    assert!(p.svc.view(&copy.id).is_err());
}

#[test]
fn evidence_recorded_from_a_run_is_found_by_its_operation() {
    let p = project(&[("results/fig.svg", "<svg/>"), ("other.svg", "<svg/>")]);
    let fig = p.resource("results/fig.svg");
    let evidence = p.evidence(EvidenceKind::Computational, &fig.id, &[], &["results/fig.svg"]);
    let found = p.svc.evidence_for_operation("researchRun", "run-0123456789abcdef").unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, evidence);
    assert_eq!(found[0].1.as_ref().map(|e| e.id.clone()), Some(fig.id.clone()));
    assert!(p.svc.evidence_for_operation("researchRun", "run-ffffffffffffffff").unwrap().is_empty());
    assert!(p.svc.evidence_for_operation("leanAudit", "run-0123456789abcdef").unwrap().is_empty());
}
