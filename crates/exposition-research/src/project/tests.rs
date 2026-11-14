//! Research ↔ graph integration against a real graph database, document
//! service, and run records on disk (no kernel: runs are recorded manifests).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::artifact::{
    ArtifactId, ArtifactKind, ArtifactRecord, AttachmentPlacement, Producer, RunId, RunInput, RunRecord, RunStatus,
    ToolVersion,
};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextEncoding, TextPatch, TransactionId, TransactionRequest,
    UndoBehavior,
};
use exposition_core::job::JobSpec;
use exposition_core::path::WorkspacePath;
use exposition_graph::{
    AssertionStatus, EntityKind, EvidenceKind, GraphConfig, GraphService, Lifecycle, OperationState, RelationKind,
    ResourceRole,
};
use exposition_workspace::DocumentService;
use exposition_workspace::documents::OpenDocument;

use super::ResearchProject;
use super::types::*;
use crate::helper::sha256_hex;
use crate::launcher::{KernelProcessLauncher, LaunchError, LaunchFuture, ProbeOutput};
use crate::store::write_manifest;
use crate::types::{
    CodeOrigin, CodeProvenance, InterpreterProvenance, KernelDescriptor, OutputSummary, RunManifest, SessionOwner,
    SessionProvenance,
};
use crate::{ResearchConfig, ResearchService};

struct NoLauncher;

impl KernelProcessLauncher for NoLauncher {
    fn launch_interactive(&self, _spec: JobSpec) -> LaunchFuture<'_> {
        Box::pin(async { Err(LaunchError::Refused("no kernels in tests".into())) })
    }
    fn run_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        Err(LaunchError::Refused("no probes in tests".into()))
    }
    fn run_project_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        Err(LaunchError::Refused("no probes in tests".into()))
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake figure";
const CSV: &str = "x,y\n1,2\n3,4\n";
const SCRIPT: &str = "import matplotlib\nprint(1)\n";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    graph: Arc<GraphService>,
    documents: Arc<DocumentService>,
    project: ResearchProject,
}

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir_all(root.join("analysis")).unwrap();
    std::fs::write(root.join("analysis/fit.py"), SCRIPT).unwrap();
    std::fs::write(root.join("paper.typ"), "= Paper\n\nIntro.\n").unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: dir.path().join("data") });
    assert!(graph.attach(&root).is_usable());
    let documents = Arc::new(DocumentService::new());
    let research = Arc::new(ResearchService::new(ResearchConfig::new(
        dir.path().join("data"),
        Arc::new(NoLauncher),
        documents.clone(),
    )));
    research.set_workspace(&root, true).unwrap();
    let project = ResearchProject::new(graph.clone(), research, documents.clone());
    Fixture { _dir: dir, root, graph, documents, project }
}

/// Record a finished run with a PNG figure and a CSV table on disk.
fn record_run(root: &Path, run: &str, inputs: Vec<RunInput>, unsaved: Vec<WorkspacePath>) -> RunManifest {
    let dir = root.join(".exposition/artifacts").join(run);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("output-1.png"), PNG).unwrap();
    std::fs::write(dir.join("output-2.csv"), CSV).unwrap();
    let artifact = |name: &str, kind, media: &str, bytes: &[u8]| ArtifactRecord {
        id: ArtifactId(format!("{run}/{name}")),
        kind,
        path: Some(path(&format!(".exposition/artifacts/{run}/{name}"))),
        scratch_path: None,
        media_type: media.into(),
        size_bytes: bytes.len() as u64,
        content_hash: format!("sha256:{}", sha256_hex(bytes)),
    };
    let manifest = RunManifest {
        format: "exposition.run/1".into(),
        record: RunRecord {
            id: RunId(run.into()),
            producer: Producer::KernelSession { session_id: "s".into(), runtime: "python".into() },
            inputs,
            environment: vec![ToolVersion { name: "python".into(), version: "3.14.0".into() }],
            outputs: vec![
                artifact("output-1.png", ArtifactKind::Figure, "image/png", PNG),
                artifact("output-2.csv", ArtifactKind::Table, "text/csv", CSV.as_bytes()),
            ],
            status: RunStatus::Succeeded,
            started_at_ms: 1,
            finished_at_ms: Some(2),
        },
        session: SessionProvenance {
            session_id: "s".into(),
            owner: SessionOwner::User,
            environment_id: "python:x".into(),
            generation: 1,
            interpreter: InterpreterProvenance {
                implementation: "CPython".into(),
                version: "3.14.0".into(),
                location: "~".into(),
            },
            kernel: KernelDescriptor::default(),
            helper_version: "1".into(),
            protocol: 1,
        },
        code: CodeProvenance {
            origin: CodeOrigin::File { path: path("analysis/fit.py"), revision: None },
            sha256: sha256_hex(SCRIPT.as_bytes()),
            bytes: SCRIPT.len() as u64,
            text: Some(SCRIPT.into()),
        },
        execution_count: Some(1),
        error: None,
        output: OutputSummary::default(),
        unsaved_inputs: unsaved,
        execution: None,
    };
    write_manifest(root, &manifest).unwrap();
    manifest
}

fn script_input() -> RunInput {
    RunInput {
        path: path("analysis/fit.py"),
        revision: Some(3),
        content_hash: Some(format!("sha256:{}", sha256_hex(SCRIPT.as_bytes()))),
    }
}

fn open(f: &Fixture, p: &str) -> DocumentId {
    let text = std::fs::read_to_string(f.root.join(p)).unwrap();
    f.documents
        .open(OpenDocument {
            path: Some(path(p)),
            text,
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        })
        .id
}

fn edit(f: &Fixture, doc: &DocumentId, text: &str) {
    let snapshot = f.documents.snapshot(doc).unwrap();
    f.documents
        .apply(TransactionRequest {
            id: TransactionId(format!("user-{}", snapshot.revision)),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: doc.clone(),
                base_revision: snapshot.revision,
                patches: vec![TextPatch::insert(0, text.to_owned())],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
}

fn relation_exists(
    f: &Fixture,
    kind: RelationKind,
    from: &exposition_graph::EntityId,
    to: &exposition_graph::EntityId,
) -> bool {
    f.graph.with_store(|s| Ok(s.find_relation(kind, from, to)?)).unwrap().is_some()
}

#[test]
fn enabling_research_creates_exactly_one_space_without_a_manuscript_or_code() {
    let f = fixture();
    std::fs::remove_file(f.root.join("paper.typ")).unwrap();
    assert!(f.project.space().unwrap().is_none());
    let space = f.project.enable().unwrap();
    assert!(space.root.as_str().starts_with(".exposition/research/rs-"));
    for group in ["sources", "data", "notes"] {
        assert!(f.root.join(space.root.as_str()).join(group).is_dir(), "{group} folder");
    }
    let again = f.project.enable().unwrap();
    assert_eq!(again, space, "enabling again reuses the one space");
    assert_eq!(super::space::spaces(&f.graph).len(), 1);
    let detail = f.graph.entity_detail(&space.entity).unwrap();
    assert_eq!(detail.entity.kind, EntityKind::ResearchSpace);
    assert!(detail.incoming.iter().any(|r| r.kind == RelationKind::Contains), "the project contains the space");
    // Nothing ran: no run records exist.
    assert!(!f.root.join(".exposition/runs").exists());
}

#[test]
fn research_files_get_resource_identities_and_imports_record_their_origin() {
    let f = fixture();
    let space = f.project.enable().unwrap();
    std::fs::write(f.root.join(space.root.as_str()).join("sources/sim.py"), "print(2)\n").unwrap();
    let listing = f.project.listing().unwrap();
    let sim = listing.sources.iter().find(|s| s.name == "sim.py").expect("listed");
    assert!(sim.entity.is_some(), "admitted as a resource");
    assert_eq!(sim.roles, vec![ResourceRole::Script]);
    // Copy import: a project file with its recorded origin.
    let outside = f._dir.path().join("obs.csv");
    std::fs::write(&outside, CSV).unwrap();
    let imported = f
        .project
        .import(&ImportRequest {
            group: SpaceGroup::Data,
            source: outside.display().to_string(),
            mode: ImportMode::Copy,
        })
        .unwrap();
    let ImportOutcome::Copied { file } = imported else { panic!("copied") };
    assert_eq!(std::fs::read_to_string(f.root.join(file.path.as_str())).unwrap(), CSV);
    let origin = file.origin.unwrap();
    assert_eq!(origin.content_hash, sha256_hex(CSV.as_bytes()));
    assert_eq!(origin.mode, ImportMode::Copy);
    // Importing the same name again never overwrites.
    let ImportOutcome::Copied { file: second } = f
        .project
        .import(&ImportRequest {
            group: SpaceGroup::Data,
            source: outside.display().to_string(),
            mode: ImportMode::Copy,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_ne!(second.path, file.path);
    // Reference: an external resource, not copied.
    let ImportOutcome::Referenced { reference } = f
        .project
        .import(&ImportRequest {
            group: SpaceGroup::Data,
            source: outside.display().to_string(),
            mode: ImportMode::Reference,
        })
        .unwrap()
    else {
        panic!()
    };
    let listing = f.project.listing().unwrap();
    assert_eq!(listing.data.len(), 2);
    assert_eq!(listing.references.len(), 1);
    assert_eq!(listing.references[0].entity, reference.entity);
    // Project metadata cannot be imported.
    let graph_db = f.root.join(".exposition/graph.sqlite3");
    assert!(
        f.project
            .import(&ImportRequest {
                group: SpaceGroup::Data,
                source: graph_db.display().to_string(),
                mode: ImportMode::Copy
            })
            .is_err()
    );
}

#[test]
fn retaining_admits_one_identity_with_service_owned_evidence_and_provenance() {
    let f = fixture();
    f.project.enable().unwrap();
    record_run(&f.root, "run-a1", vec![script_input()], vec![path("analysis/fit.py")]);
    let out = f
        .project
        .retain(&RetainRequest {
            run_id: "run-a1".into(),
            artifact_id: "run-a1/output-2.csv".into(),
            state: RetainState::Active,
            experiment: None,
        })
        .unwrap();
    assert_eq!(out.result.lifecycle, Lifecycle::Active);
    assert_eq!(out.result.roles, vec![ResourceRole::Dataset, ResourceRole::Artifact], "one identity, two roles");
    assert!(!out.evidence_existing);
    let detail = f.graph.entity_detail(&out.result.entity).unwrap();
    let evidence = &detail.evidence[0];
    assert_eq!(evidence.kind, EvidenceKind::Computational);
    assert_eq!(evidence.operation.id, "run-a1");
    assert_eq!(evidence.inputs[0].content_hash.as_deref(), Some(sha256_hex(SCRIPT.as_bytes()).as_str()));
    assert!(evidence.inputs[0].unsaved, "an unsaved input is labelled (only its digest is evidence)");
    assert_eq!(evidence.outputs[0].content_hash, sha256_hex(CSV.as_bytes()));
    assert!(evidence.recorded_by == super::origin());
    let script = f.graph.resource(&path("analysis/fit.py")).unwrap().unwrap();
    assert!(detail.outgoing.iter().any(|r| r.kind == RelationKind::DerivedFrom && r.to == script.id));
    // Idempotent.
    let again = f
        .project
        .retain(&RetainRequest {
            run_id: "run-a1".into(),
            artifact_id: "run-a1/output-2.csv".into(),
            state: RetainState::Active,
            experiment: None,
        })
        .unwrap();
    assert!(again.evidence_existing);
    assert_eq!(again.result.entity, out.result.entity);
    // Archive / discard change the lifecycle only.
    let discarded = f
        .project
        .set_result_state(&SetResultStateRequest { entity: out.result.entity.clone(), state: RetainState::Discarded })
        .unwrap();
    assert_eq!(discarded.result.lifecycle, Lifecycle::Discarded);
    assert!(f.root.join(".exposition/artifacts/run-a1/output-2.csv").exists(), "discard never deletes the file");
    assert_eq!(f.graph.entity_detail(&out.result.entity).unwrap().evidence.len(), 1, "evidence is kept");
    // Output never retained cannot be "discarded" into the graph.
    let err = f
        .project
        .retain(&RetainRequest {
            run_id: "run-a1".into(),
            artifact_id: "run-a1/output-1.png".into(),
            state: RetainState::Discarded,
            experiment: None,
        })
        .unwrap_err();
    assert_eq!(err.code(), "research.notRetained");
    assert!(f.graph.resource(&path(".exposition/artifacts/run-a1/output-1.png")).unwrap().is_none());
}

#[test]
fn an_altered_artifact_is_refused() {
    let f = fixture();
    record_run(&f.root, "run-b1", vec![script_input()], vec![]);
    std::fs::write(f.root.join(".exposition/artifacts/run-b1/output-1.png"), b"tampered").unwrap();
    let err = f
        .project
        .retain(&RetainRequest {
            run_id: "run-b1".into(),
            artifact_id: "run-b1/output-1.png".into(),
            state: RetainState::Active,
            experiment: None,
        })
        .unwrap_err();
    assert_eq!(err.code(), "research.artifactChanged");
    assert!(f.graph.resource(&path(".exposition/artifacts/run-b1/output-1.png")).unwrap().is_none());
}

#[test]
fn experiments_link_procedure_inputs_outcomes_and_list_attempts() {
    let f = fixture();
    let space = f.project.enable().unwrap();
    std::fs::write(f.root.join(space.root.as_str()).join("data/obs.csv"), CSV).unwrap();
    let data = space.root.join("data").unwrap().join("obs.csv").unwrap();
    record_run(&f.root, "run-c1", vec![script_input()], vec![]);
    let retained = f
        .project
        .retain(&RetainRequest {
            run_id: "run-c1".into(),
            artifact_id: "run-c1/output-1.png".into(),
            state: RetainState::Active,
            experiment: None,
        })
        .unwrap();
    let detail = f
        .project
        .save_experiment(&ExperimentInput {
            entity: None,
            expected_revision: None,
            title: "Fit the observations".into(),
            objective: "Estimate the slope".into(),
            procedure: Some(path("analysis/fit.py")),
            inputs: vec![data.clone()],
            outcomes: vec![retained.result.entity.clone()],
        })
        .unwrap();
    assert_eq!(detail.summary.objective, "Estimate the slope");
    assert_eq!(detail.procedure.as_ref().unwrap().path, path("analysis/fit.py"));
    assert_eq!(detail.inputs[0].path, data);
    assert_eq!(detail.outcomes[0].entity, retained.result.entity);
    assert_eq!(detail.attempts.len(), 1, "the run of the procedure is an attempt");
    assert_eq!(detail.attempts[0].id.0, "run-c1");
    let exp = detail.summary.entity.clone();
    let rel = f
        .graph
        .with_store(|s| Ok(s.find_relation(RelationKind::DerivedFrom, &retained.result.entity, &exp)?))
        .unwrap()
        .unwrap();
    assert_eq!(rel.status, AssertionStatus::Accepted, "the person's assertion");
    // A historical run is not an experiment by itself.
    assert_eq!(f.project.listing().unwrap().experiments.len(), 1);
    // Update: drop the declared input; a stale revision conflicts.
    let updated = f
        .project
        .save_experiment(&ExperimentInput {
            entity: Some(exp.clone()),
            expected_revision: Some(detail.summary.revision),
            title: "Fit".into(),
            objective: "Estimate the slope".into(),
            procedure: Some(path("analysis/fit.py")),
            inputs: vec![],
            outcomes: vec![retained.result.entity.clone()],
        })
        .unwrap();
    assert!(updated.inputs.is_empty());
    let stale = f.project.save_experiment(&ExperimentInput {
        entity: Some(exp.clone()),
        expected_revision: Some(detail.summary.revision),
        title: "Fit again".into(),
        objective: String::new(),
        procedure: None,
        inputs: vec![],
        outcomes: vec![],
    });
    assert_eq!(stale.unwrap_err().code(), "research.experimentChanged");
    // Impact: changing the script reaches the experiment and its outcome.
    let script = f.graph.resource(&path("analysis/fit.py")).unwrap().unwrap();
    let impact = f
        .graph
        .impact(&exposition_graph::ImpactRequest { entities: vec![script.id], max_depth: None, max_items: None })
        .unwrap();
    assert!(impact.items.iter().any(|i| i.entity.id == exp));
    assert!(impact.items.iter().any(|i| i.entity.id == retained.result.entity));
}

fn promote(f: &Fixture, doc: &DocumentId, run: &str, artifact: &str) -> PromotionPreview {
    let snapshot = f.documents.snapshot(doc).unwrap();
    f.project
        .prepare_promotion(&PromotionRequest {
            run_id: run.into(),
            artifact_id: artifact.into(),
            document: doc.clone(),
            base_revision: Some(snapshot.revision),
            placement: AttachmentPlacement::Insert { offset: snapshot.text.encode_utf16().count() as u32 },
            caption: Some("Slope fit".into()),
            attachment_key: None,
        })
        .unwrap()
}

#[test]
fn reviewed_promotion_inserts_exactly_the_reviewed_markup_and_records_use() {
    let f = fixture();
    record_run(&f.root, "run-d1", vec![script_input()], vec![]);
    let doc = open(&f, "paper.typ");
    let preview = promote(&f, &doc, "run-d1", "run-d1/output-1.png");
    assert_eq!(preview.format, "typst");
    assert_eq!(preview.revision, 1);
    assert!(preview.edits[0].text.contains("// exposition:begin fig-"), "{:?}", preview.edits);
    assert!(preview.edits[0].text.contains("Slope fit"));
    assert_eq!(f.documents.snapshot(&doc).unwrap().revision, 1, "preparing changes nothing");
    let outcome = f.project.approve_promotion(&preview.promotion).unwrap();
    let text = f.documents.snapshot(&doc).unwrap().text.to_string();
    assert!(text.ends_with(&preview.edits[0].text) || text.contains(&preview.edits[0].text));
    assert_eq!(outcome.revision, 2);
    let PromotionGraphState::Recorded { manuscript, artifact, .. } = &outcome.graph else {
        panic!("{:?}", outcome.graph)
    };
    assert!(relation_exists(&f, RelationKind::UsesAsset, manuscript, artifact));
    let record = f.graph.journal().get(&f.root, &outcome.operation).unwrap();
    assert_eq!(record.state, OperationState::Completed);
    assert!(record.steps.iter().any(|s| s.name == "attach" && s.refs["transaction"] == outcome.transaction));
    // The review cannot be approved twice.
    assert_eq!(f.project.approve_promotion(&preview.promotion).unwrap_err().code(), "promotion.unknown");

    // Undo removes the use; redo restores it (the graph follows the document).
    f.documents.undo(&doc, EditOrigin::Source).unwrap();
    let text = f.documents.snapshot(&doc).unwrap().text.to_string();
    let changed = f.project.observe_text(&path("paper.typ"), &text, false);
    assert_eq!(changed.len(), 1);
    assert!(!relation_exists(&f, RelationKind::UsesAsset, manuscript, artifact));
    assert_eq!(
        f.graph.entity_detail(artifact).unwrap().entity.lifecycle,
        Lifecycle::Active,
        "the retained result stays retained"
    );
    f.documents.redo(&doc, EditOrigin::Source).unwrap();
    let text = f.documents.snapshot(&doc).unwrap().text.to_string();
    f.project.observe_text(&path("paper.typ"), &text, false);
    assert!(relation_exists(&f, RelationKind::UsesAsset, manuscript, artifact));
}

#[test]
fn stale_approval_and_altered_artifact_are_refused_without_touching_the_source() {
    let f = fixture();
    record_run(&f.root, "run-e1", vec![script_input()], vec![]);
    let doc = open(&f, "paper.typ");
    let preview = promote(&f, &doc, "run-e1", "run-e1/output-1.png");
    edit(&f, &doc, "x");
    let before = f.documents.snapshot(&doc).unwrap();
    let err = f.project.approve_promotion(&preview.promotion).unwrap_err();
    assert_eq!(err.code(), "promotion.stale");
    let after = f.documents.snapshot(&doc).unwrap();
    assert_eq!((after.revision, after.text.as_ref()), (before.revision, before.text.as_ref()), "source untouched");

    let preview = promote(&f, &doc, "run-e1", "run-e1/output-1.png");
    std::fs::write(f.root.join(".exposition/artifacts/run-e1/output-1.png"), b"altered").unwrap();
    let err = f.project.approve_promotion(&preview.promotion).unwrap_err();
    assert_eq!(err.code(), "research.artifactChanged");
    assert_eq!(f.documents.snapshot(&doc).unwrap().revision, after.revision, "source untouched");
    assert!(f.graph.journal().unresolved(&f.root, false).is_empty(), "no journal record for refused approvals");
}

#[test]
fn a_failed_graph_write_leaves_a_pending_record_that_reconciliation_resolves() {
    let f = fixture();
    record_run(&f.root, "run-f1", vec![script_input()], vec![]);
    let doc = open(&f, "paper.typ");
    let preview = promote(&f, &doc, "run-f1", "run-f1/output-2.csv");
    f.project.fail_next_graph_write();
    let outcome = f.project.approve_promotion(&preview.promotion).unwrap();
    assert!(matches!(outcome.graph, PromotionGraphState::PendingReconciliation { .. }), "{:?}", outcome.graph);
    let text = f.documents.snapshot(&doc).unwrap().text.to_string();
    assert!(text.contains(&format!("// exposition:begin {}", outcome.attachment_key)), "the source edit happened once");
    assert!(
        f.graph.resource(&path(".exposition/artifacts/run-f1/output-2.csv")).unwrap().is_none(),
        "no fictional graph state"
    );
    let state = f.project.promotions().unwrap();
    assert_eq!(state.unresolved.len(), 1);
    assert!(matches!(state.unresolved[0].state, OperationState::NeedsReconciliation { .. }));

    let reconciled = f.project.reconcile_promotion(&outcome.operation).unwrap();
    assert!(reconciled.attachment_present);
    assert!(matches!(reconciled.record.state, OperationState::Resolved { .. }));
    let artifact = f.graph.resource(&path(".exposition/artifacts/run-f1/output-2.csv")).unwrap().unwrap();
    let manuscript = f.graph.resource(&path("paper.typ")).unwrap().unwrap();
    assert!(relation_exists(&f, RelationKind::UsesAsset, &manuscript.id, &artifact.id));
    assert_eq!(f.documents.snapshot(&doc).unwrap().text.as_ref(), text, "reconciliation never re-applies the edit");
    assert!(f.project.promotions().unwrap().unresolved.is_empty());

    // An interrupted promotion whose insertion was undone closes without graph writes.
    let preview = promote(&f, &doc, "run-f1", "run-f1/output-1.png");
    f.project.fail_next_graph_write();
    let second = f.project.approve_promotion(&preview.promotion).unwrap();
    f.documents.undo(&doc, EditOrigin::Source).unwrap();
    let r = f.project.reconcile_promotion(&second.operation).unwrap();
    assert!(!r.attachment_present);
    assert!(matches!(r.record.state, OperationState::Resolved { .. }));
    assert!(f.graph.resource(&path(".exposition/artifacts/run-f1/output-1.png")).unwrap().is_none());
}

#[test]
fn interrupted_promotions_are_marked_for_reconciliation_on_open_and_attachments_followed() {
    let f = fixture();
    record_run(&f.root, "run-g1", vec![script_input()], vec![]);
    let doc = open(&f, "paper.typ");
    let preview = promote(&f, &doc, "run-g1", "run-g1/output-1.png");
    let outcome = f.project.approve_promotion(&preview.promotion).unwrap();
    // Simulate a crash mid-way through another operation.
    let crashed = f
        .graph
        .journal()
        .begin(exposition_graph::OperationKind::Promotion, &f.root, None, "interrupted", vec!["attach".into()])
        .unwrap();
    // The document was reverted outside the session (e.g. reloaded from disk).
    f.documents.undo(&doc, EditOrigin::Source).unwrap();
    let state = f.project.load_promotions().unwrap();
    assert!(
        state
            .unresolved
            .iter()
            .any(|r| r.id == crashed.id && matches!(r.state, OperationState::NeedsReconciliation { .. }))
    );
    let tracked = state.tracked.iter().find(|t| t.operation == outcome.operation).unwrap();
    assert!(!tracked.present);
    let PromotionGraphState::Recorded { manuscript, artifact, .. } = &outcome.graph else { panic!() };
    assert!(!relation_exists(&f, RelationKind::UsesAsset, manuscript, artifact));
}

#[test]
fn marker_detection_matches_the_format_adapters() {
    use exposition_attach::Format;
    assert!(super::attachment_present(
        Format::Typst,
        "a\n  // exposition:begin fig-1\nx\n// exposition:end fig-1\n",
        "fig-1"
    ));
    assert!(!super::attachment_present(Format::Typst, "// exposition:begin fig-1\n", "fig-1"));
    assert!(super::attachment_present(
        Format::Latex,
        "% exposition:begin tab-2\r\n\\begin{table}\r\n% exposition:end tab-2\r\n",
        "tab-2"
    ));
    assert!(super::attachment_present(
        Format::Markdown,
        "<!-- exposition:begin fig-3 -->\n![x](y)\n<!-- exposition:end fig-3 -->",
        "fig-3"
    ));
    assert!(!super::attachment_present(
        Format::Markdown,
        "<!-- exposition:begin fig-3 -->\n<!-- exposition:begin fig-3 -->\n<!-- exposition:end fig-3 -->",
        "fig-3"
    ));
}

#[test]
fn an_attachment_inserted_elsewhere_is_recorded_and_followed() {
    // The agent `insert_artifact` tool inserts through the format-owned
    // attachment service; research then records the use in the graph.
    let f = fixture();
    let manifest = record_run(&f.root, "run-h1", vec![script_input()], vec![]);
    let doc = open(&f, "paper.typ");
    let snapshot = f.documents.snapshot(&doc).unwrap();
    let artifact = manifest.record.outputs[0].clone();
    let request = exposition_core::artifact::AttachArtifactRequest {
        document: doc.clone(),
        base_revision: snapshot.revision,
        artifact,
        run: Some(RunId("run-h1".into())),
        placement: AttachmentPlacement::Insert { offset: snapshot.text.encode_utf16().count() as u32 },
        caption: Some("Agent figure".into()),
        attachment_key: None,
        origin: EditOrigin::Agent { task_id: "task-1".into() },
    };
    let (outcome, _) = exposition_attach::attach_artifact(&f.documents, &f.root, &request).unwrap();
    let notice = AttachmentNotice {
        run_id: "run-h1".into(),
        artifact_id: "run-h1/output-1.png".into(),
        document: doc.clone(),
        attachment_key: outcome.attachment_key.clone(),
        transaction: None,
        revision: outcome.revision,
        actor: "agent task task-1".into(),
    };
    let recorded = f.project.record_external_attachment(&notice).unwrap();
    let PromotionGraphState::Recorded { manuscript, artifact, .. } = &recorded.graph else {
        panic!("{:?}", recorded.graph)
    };
    assert!(relation_exists(&f, RelationKind::UsesAsset, manuscript, artifact));
    let entity = f.graph.resource(&path(".exposition/artifacts/run-h1/output-1.png")).unwrap().unwrap();
    assert_eq!(&entity.id, artifact, "one identity for the attached artifact");
    assert_eq!(f.graph.journal().get(&f.root, &recorded.operation).unwrap().state, OperationState::Completed);
    // Followed through undo like a reviewed promotion.
    f.documents.undo(&doc, EditOrigin::Source).unwrap();
    let text = f.documents.snapshot(&doc).unwrap().text.to_string();
    f.project.observe_text(&path("paper.typ"), &text, false);
    assert!(!relation_exists(&f, RelationKind::UsesAsset, manuscript, artifact));
    // A key that is not in the document records nothing.
    let missing = AttachmentNotice { attachment_key: "fig-absent".into(), ..notice };
    assert_eq!(f.project.record_external_attachment(&missing).unwrap_err().code(), "promotion.notAttached");
}

/// A workflow batch run (restricted container, batch-named artifacts) is
/// retained through the same path, and its evidence names the mode it
/// actually ran in; manifests without a recorded mode stay trusted native.
#[test]
fn retained_batch_output_evidence_names_the_actual_execution_mode() {
    let f = fixture();
    f.project.enable().unwrap();
    let mut manifest = record_run(&f.root, "run-b1", vec![script_input()], Vec::new());
    let dir = f.root.join(".exposition/artifacts/run-b1");
    std::fs::write(dir.join("table.csv"), CSV).unwrap();
    manifest.record.outputs = vec![ArtifactRecord {
        id: ArtifactId("run-b1/table.csv".into()),
        kind: ArtifactKind::Table,
        path: Some(path(".exposition/artifacts/run-b1/table.csv")),
        scratch_path: None,
        media_type: "text/csv".into(),
        size_bytes: CSV.len() as u64,
        content_hash: format!("sha256:{}", sha256_hex(CSV.as_bytes())),
    }];
    manifest.session.owner = SessionOwner::Workflow { run: "wf".into(), step: "s".into() };
    manifest.execution = Some(crate::types::RunExecution::RestrictedContainer {
        backend: "docker".into(),
        image: "python:3.13-slim".into(),
        image_id: Some("sha256:abc".into()),
        network: "none".into(),
        user: "65534:65534".into(),
        memory: "2g".into(),
        cpus: "2".into(),
        pids_limit: 256,
        time_limit_ms: 60_000,
    });
    write_manifest(&f.root, &manifest).unwrap();
    let out = f
        .project
        .retain(&RetainRequest {
            run_id: "run-b1".into(),
            artifact_id: "run-b1/table.csv".into(),
            state: RetainState::Active,
            experiment: None,
        })
        .unwrap();
    let detail = f.graph.entity_detail(&out.result.entity).unwrap();
    let execution = detail.evidence[0].environment.iter().find(|(k, _)| k == "execution").unwrap().1.clone();
    assert!(execution.starts_with("restricted container (docker, image python:3.13-slim, network none"), "{execution}");
    assert!(!execution.contains("trusted native"), "{execution}");

    record_run(&f.root, "run-b2", vec![script_input()], Vec::new());
    let old = f
        .project
        .retain(&RetainRequest {
            run_id: "run-b2".into(),
            artifact_id: "run-b2/output-2.csv".into(),
            state: RetainState::Active,
            experiment: None,
        })
        .unwrap();
    let detail = f.graph.entity_detail(&old.result.entity).unwrap();
    let label = detail
        .evidence
        .iter()
        .find(|e| e.operation.id == "run-b2")
        .and_then(|e| e.environment.iter().find(|(k, _)| k == "execution"))
        .map(|(_, v)| v.clone());
    assert_eq!(label.as_deref(), Some("trusted native (user account privileges)"));
}

/// P1: a research run record is deleted recoverably (hidden, restorable),
/// its generated outputs are removed only as their own scope (kept outputs
/// stay), a permanent deletion removes the record (manifest + transcript),
/// and an active run is never removed.
#[test]
fn run_records_delete_restore_purge_and_outputs_are_a_separate_scope() {
    let f = fixture();
    let (one, two) = ("run-aaaa000000000001", "run-aaaa000000000002");
    record_run(&f.root, one, vec![script_input()], vec![]);
    record_run(&f.root, two, vec![], vec![]);
    std::fs::write(f.root.join(".exposition/artifacts").join(one).join("transcript.json"), b"[]").unwrap();
    let research = &f.project.research;
    let records = research.run_records().unwrap();
    assert_eq!(records.len(), 2);
    let first = records.iter().find(|r| r.id == one).unwrap();
    assert_eq!(first.outputs.len(), 2, "transcript is part of the record, not an output");
    assert!(first.record_bytes > 0);
    assert_eq!(first.inputs, vec![path("analysis/fit.py")]);

    research.delete_run_record(one, false).unwrap();
    assert!(research.runs().unwrap().iter().all(|r| r.id.0 != one), "hidden from the history");
    assert!(research.run_records().unwrap().iter().all(|r| r.id != one));
    assert_eq!(research.deleted_run_records().unwrap().len(), 1);
    assert!(f.root.join(".exposition/artifacts").join(one).join("output-1.png").is_file(), "outputs untouched");
    research.restore_run_record(one).unwrap();
    assert!(research.runs().unwrap().iter().any(|r| r.id.0 == one));

    // Outputs: only what nothing keeps.
    let kept = path(&format!(".exposition/artifacts/{one}/output-1.png"));
    let removed = research.remove_run_outputs(one, &|p| p == &kept).unwrap();
    assert_eq!(removed.removed, vec![path(&format!(".exposition/artifacts/{one}/output-2.csv"))]);
    assert_eq!(removed.kept, vec![kept.clone()]);
    assert_eq!(removed.bytes, CSV.len() as u64);
    assert!(research.runs().unwrap().iter().any(|r| r.id.0 == one), "the record stays");

    // Permanent: the record goes, the kept output stays.
    research.delete_run_record(one, true).unwrap();
    assert!(!f.root.join(".exposition/runs").join(format!("{one}.json")).exists());
    assert!(!f.root.join(".exposition/artifacts").join(one).join("transcript.json").exists());
    assert!(f.root.join(".exposition/artifacts").join(one).join("output-1.png").is_file());
    assert_eq!(research.delete_run_record(one, true).unwrap_err().code(), "research.unknownRun");

    // An active run (a workflow batch executing now) is never removed.
    research.set_batch_live(two, true);
    assert_eq!(research.delete_run_record(two, false).unwrap_err().code(), "research.runActive");
    assert_eq!(research.remove_run_outputs(two, &|_| false).unwrap_err().code(), "research.runActive");
    assert!(f.root.join(".exposition/runs").join(format!("{two}.json")).is_file());
    research.set_batch_live(two, false);
    research.delete_run_record(two, false).unwrap();
    // Restore never replaces a record that exists again.
    std::fs::copy(
        f.root.join(".exposition/runs/deleted").join(format!("{two}.json")),
        f.root.join(".exposition/runs").join(format!("{two}.json")),
    )
    .unwrap();
    assert_eq!(research.restore_run_record(two).unwrap_err().code(), "research.invalidRequest");
}
