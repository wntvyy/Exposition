use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::materialize::{CreationAction, CreationFileState, Creator, Provenance, precheck};
use super::presets::{
    COMPUTATIONAL, EMPTY, LEAN, ManuscriptFormat, PAPER, PAPER_COMPUTATION, PAPER_LEAN, ProjectPresetRequest, RESEARCH,
    ResearchRuntime, blueprint,
};
use super::*;
use crate::journal::OperationState;
use crate::model::{AssertionStatus, Lifecycle};
use crate::query::GraphQuery;
use crate::service::{GraphConfig, GraphService};
use crate::workflow::DraftState;

fn request(preset: &str) -> ProjectPresetRequest {
    ProjectPresetRequest {
        preset: preset.into(),
        title: Some("Spectral gaps".into()),
        format: Some(ManuscriptFormat::Typst),
        runtime: Some(ResearchRuntime::Python),
        lean_toolchain: None,
        include_workflow: true,
        purpose: None,
    }
}

fn errors(v: &BlueprintValidation) -> Vec<String> {
    v.diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

fn paper() -> ProjectBlueprint {
    blueprint(&request(PAPER)).unwrap()
}

#[test]
fn every_preset_and_option_validates() {
    for preset in [PAPER, COMPUTATIONAL, LEAN, RESEARCH, PAPER_COMPUTATION, PAPER_LEAN, EMPTY] {
        for format in [
            ManuscriptFormat::Typst,
            ManuscriptFormat::LatexPdf,
            ManuscriptFormat::LatexLua,
            ManuscriptFormat::Markdown,
        ] {
            for runtime in [ResearchRuntime::Python, ResearchRuntime::R, ResearchRuntime::Julia] {
                let mut r = request(preset);
                r.format = Some(format);
                r.runtime = Some(runtime);
                let bp = blueprint(&r).unwrap();
                let v = validate_blueprint(&bp);
                assert!(v.valid, "{preset} {format:?} {runtime:?}: {:?}", errors(&v));
                let warnings: Vec<_> =
                    v.diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Warning).collect();
                assert!(warnings.is_empty(), "{preset}: preset drafts are complete: {warnings:?}");
                // Round-trips through the strict parser.
                let text = serde_json::to_string(&bp).unwrap();
                assert_eq!(parse_blueprint(&text).unwrap(), bp);
            }
        }
    }
}

#[test]
fn presets_are_honest_about_their_shape() {
    let research = blueprint(&request(RESEARCH)).unwrap();
    let v = validate_blueprint(&research);
    let s = v.summary.unwrap();
    assert!(!s.has_manuscript, "research-only: no manuscript");
    assert_eq!(s.entry, BlueprintEntry::Research);
    let paper = validate_blueprint(&paper()).summary.unwrap();
    assert!(paper.has_manuscript);
    assert_eq!(paper.entry, BlueprintEntry::Editor { path: "main.typ".into() });
    assert!(paper.relations.contains(&"Main result depends on Key definition".to_owned()), "{:?}", paper.relations);
    // Fresh research-space ids per blueprint.
    let a = blueprint(&request(COMPUTATIONAL)).unwrap().research.unwrap().id;
    let b = blueprint(&request(COMPUTATIONAL)).unwrap().research.unwrap().id;
    assert_ne!(a, b);
    // Lean: declarative lakefile, toolchain declared, never a lakefile.lean.
    let lean = blueprint(&request(LEAN)).unwrap();
    assert!(lean.files.iter().any(|f| f.path == "lakefile.toml"));
    assert!(lean.files.iter().all(|f| f.path != "lakefile.lean"));
    assert!(lean.capabilities.iter().any(|c| c.capability == CapabilityId::Lean));
    // R/Julia studies have no workflow draft (initial adapters are Python and Lean).
    let mut r = request(COMPUTATIONAL);
    r.runtime = Some(ResearchRuntime::R);
    assert!(blueprint(&r).unwrap().workflows.is_empty());
    // A missing choice is refused, not guessed.
    let mut r = request(PAPER);
    r.format = None;
    assert!(blueprint(&r).is_err());
}

#[test]
fn hostile_paths_are_refused_before_anything_is_written() {
    for bad in [
        "../escape.txt",
        "a/../../escape.txt",
        "/abs.txt",
        "C:/Windows/evil.txt",
        "\\\\server\\share\\x.txt",
        "notes.txt:secret",
        "CON.typ",
        "aux.tex",
        "COM1",
        ".git/hooks/pre-commit",
        ".exposition/graph.sqlite3",
        "CLAUDE.md",
        ".latexmkrc",
        "run.bat",
    ] {
        let mut bp = paper();
        bp.files.push(BlueprintFile { path: bad.into(), content: "x".into() });
        let v = validate_blueprint(&bp);
        assert!(!v.valid, "{bad} accepted");
        assert!(v.diagnostics.iter().any(|d| d.code == "blueprint.path"), "{bad}: {:?}", errors(&v));
    }
    // Case-insensitive duplicates and a file used as a folder.
    let mut bp = paper();
    bp.files.push(BlueprintFile { path: "MAIN.typ".into(), content: String::new() });
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.collision"));
    let mut bp = paper();
    bp.files.push(BlueprintFile { path: "main.typ/inner.txt".into(), content: String::new() });
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.collision"));
}

#[test]
fn trust_execution_and_hook_fields_are_schema_errors() {
    let base = serde_json::to_value(paper()).unwrap();
    for (pointer, field) in [
        ("", "trust"),
        ("", "run"),
        ("", "onOpen"),
        ("", "grants"),
        ("", "permissions"),
        ("/files/0", "execute"),
        ("/files/0", "mode"),
        ("/entities/0", "trusted"),
        ("/research", "kernel"),
    ] {
        let mut json = base.clone();
        json.pointer_mut(if pointer.is_empty() { "" } else { pointer }).unwrap()[field] = serde_json::json!(true);
        let err = parse_blueprint(&json.to_string()).unwrap_err();
        assert_eq!(err.code, "blueprint.schema", "{pointer}/{field}");
        assert!(err.message.contains(&format!("unknown field `{field}`")), "{}", err.message);
    }
    let mut json = base.clone();
    json["workflows"] =
        serde_json::json!([{ "tempId": "w", "definition": { "schema": "exposition.workflow/1", "autoRun": true } }]);
    assert!(parse_blueprint(&json.to_string()).is_err());
    let mut json = base;
    json["schema"] = serde_json::json!("exposition.blueprint/2");
    let bp: ProjectBlueprint = serde_json::from_value(json).unwrap();
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.schema"));
}

#[test]
fn sizes_are_bounded() {
    let mut bp = paper();
    bp.files.push(BlueprintFile { path: "big.txt".into(), content: "x".repeat(MAX_FILE_BYTES + 1) });
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.tooLarge"));
    let mut bp = paper();
    for i in 0..=MAX_FILES {
        bp.files.push(BlueprintFile { path: format!("f{i}.txt"), content: String::new() });
    }
    assert!(!validate_blueprint(&bp).valid);
    let mut bp = paper();
    for i in 0..9 {
        bp.files.push(BlueprintFile { path: format!("chunk{i}.txt"), content: "y".repeat(MAX_FILE_BYTES) });
    }
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.message.contains("total")));
    assert_eq!(parse_blueprint(&" ".repeat(MAX_BLUEPRINT_BYTES + 1)).unwrap_err().code, "blueprint.tooLarge");
    let mut bp = paper();
    bp.files[0].content.push('\0');
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.content"));
}

#[test]
fn entities_and_relations_follow_the_graph_vocabulary() {
    // Kinds that come from files/indexing/services.
    for kind in [EntityKind::Resource, EntityKind::FormalDeclaration, EntityKind::Project, EntityKind::Folder] {
        let mut bp = paper();
        bp.entities[0].kind = kind;
        assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.entityKind"), "{kind:?}");
    }
    // Endpoint rules (a claim is not a proof).
    let mut bp = paper();
    bp.relations.push(BlueprintRelation {
        kind: RelationKind::ProofOf,
        from: BlueprintRef::Temp { temp_id: "mainResult".into() },
        to: BlueprintRef::Temp { temp_id: "definition".into() },
        note: None,
    });
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.endpointRule"));
    // Structure, evidence, and formal association are not blueprint assertions.
    for kind in [RelationKind::Contains, RelationKind::EvidenceFor, RelationKind::Formalizes, RelationKind::AnchoredIn]
    {
        let mut bp = paper();
        bp.relations.push(BlueprintRelation {
            kind,
            from: BlueprintRef::Project,
            to: BlueprintRef::Temp { temp_id: "mainResult".into() },
            note: None,
        });
        assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.relationKind"), "{kind:?}");
    }
    // Unknown references and non-resource files.
    let mut bp = paper();
    bp.relations.push(BlueprintRelation {
        kind: RelationKind::DependsOn,
        from: BlueprintRef::Temp { temp_id: "mainResult".into() },
        to: BlueprintRef::Temp { temp_id: "ghost".into() },
        note: None,
    });
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.reference"));
    let mut bp = paper();
    bp.files.push(BlueprintFile { path: ".gitignore".into(), content: "*.pdf\n".into() });
    bp.relations.push(BlueprintRelation {
        kind: RelationKind::UsesAsset,
        from: BlueprintRef::File { path: "main.typ".into() },
        to: BlueprintRef::File { path: ".gitignore".into() },
        note: None,
    });
    assert!(validate_blueprint(&bp).diagnostics.iter().any(|d| d.code == "blueprint.reference"));
    // Invalid labels/attributes use the graph validator's rules.
    let mut bp = paper();
    bp.entities[0].label = "two\nlines".into();
    bp.entities[1].attributes.insert("bad key!".into(), "x".into());
    let v = validate_blueprint(&bp);
    assert!(v.diagnostics.iter().any(|d| d.code == "blueprint.label"));
    assert!(v.diagnostics.iter().any(|d| d.code == "blueprint.attributes"));
}

#[test]
fn incomplete_workflow_drafts_are_warnings_not_errors() {
    let mut bp = blueprint(&request(LEAN)).unwrap();
    bp.workflows[0].definition.inputs[0].source = crate::workflow::InputSource::Unset;
    let v = validate_blueprint(&bp);
    assert!(v.valid, "{:?}", errors(&v));
    assert!(v.diagnostics.iter().any(|d| d.code == "blueprint.workflowIncomplete"));
    assert!(!v.summary.unwrap().workflows[0].well_formed);
}

// ----- materialization -----

struct Fixture {
    _data: tempfile::TempDir,
    _parent: tempfile::TempDir,
    data_dir: PathBuf,
    root: PathBuf,
    graph: Arc<GraphService>,
}

fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let data_dir = data.path().to_path_buf();
    let graph = GraphService::new(GraphConfig { data_dir: data_dir.clone() });
    Fixture { _data: data, _parent: parent, data_dir, root, graph }
}

fn create(f: &Fixture, bp: &ProjectBlueprint) -> (crate::ids::OperationId, super::materialize::GraphApplied) {
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    precheck(&f.root, bp).unwrap();
    let op = creator.begin(&f.root, bp, &Provenance::Preset).unwrap();
    creator.write_files(&f.root, &op, bp, &|_| false).unwrap();
    assert!(f.graph.attach(&f.root).is_usable());
    creator.record_open(&f.root, &op).unwrap();
    let applied = creator.apply_graph(&f.root, &op, bp, &Provenance::Preset).unwrap();
    creator.complete(&f.root, &op).unwrap();
    (op, applied)
}

fn read(root: &Path, p: &str) -> String {
    std::fs::read_to_string(root.join(p)).unwrap()
}

#[test]
fn paper_preset_materializes_files_and_meaningful_graph() {
    let f = fixture();
    let bp = paper();
    let (op, applied) = create(&f, &bp);
    assert!(read(&f.root, "main.typ").contains("= Spectral gaps"));
    assert!(f.root.join("figures").is_dir());
    assert_eq!(applied.resources.len(), 1);
    assert_eq!(applied.entities.len(), 4, "definition, assumptions, main result, collection");
    let claim = applied.entities.iter().find(|e| e.temp_id == "mainResult").unwrap();
    let detail = f.graph.entity_detail(&claim.entity).unwrap();
    assert_eq!(detail.entity.kind, EntityKind::Claim);
    assert_eq!(detail.bindings[0].path.as_str(), "main.typ", "the claim is bound to the paper");
    assert_eq!(detail.bindings[0].state, crate::model::BindingState::Resolved);
    let depends = detail.outgoing.iter().find(|r| r.kind == RelationKind::DependsOn).unwrap();
    assert_eq!(depends.status, AssertionStatus::Accepted, "preset assertions are accepted");
    assert!(matches!(&depends.origin, crate::model::Origin::Preset { blueprint } if blueprint == "preset:paper"));
    let record = f.graph.journal().get(&f.root, &op).unwrap();
    assert_eq!(record.state, OperationState::Completed);
    assert!(record.steps.iter().any(|s| s.name == "graph" && s.completed));
    // Nothing executed or retained: no run records or artifacts.
    assert!(!f.root.join(".exposition/runs").exists());
    assert!(!f.root.join(".exposition/artifacts").exists());
}

#[test]
fn fresh_identities_for_every_creation() {
    let f1 = fixture();
    let f2 = fixture();
    let bp = paper();
    let (_, a) = create(&f1, &bp);
    let p1 = f1.graph.status().project.unwrap();
    let (_, b) = create(&f2, &bp);
    let p2 = f2.graph.status().project.unwrap();
    assert_ne!(p1, p2);
    let ids_a: BTreeSet<_> = a.entities.iter().map(|e| e.entity.clone()).collect();
    assert!(b.entities.iter().all(|e| !ids_a.contains(&e.entity)));
}

#[test]
fn research_only_preset_has_a_research_space_and_saved_draft_that_never_runs() {
    let f = fixture();
    let bp = blueprint(&request(COMPUTATIONAL)).unwrap();
    let rs = bp.research.clone().unwrap();
    let (_, applied) = create(&f, &bp);
    let space = applied.research_space.clone().expect("research space entity");
    let detail = f.graph.entity_detail(&space).unwrap();
    assert_eq!(detail.entity.kind, EntityKind::ResearchSpace);
    assert_eq!(detail.entity.key.as_deref(), Some(rs.id.as_str()));
    assert_eq!(detail.entity.attributes.get("research.path"), Some(&rs.folder()));
    assert!(f.root.join(format!("{}/sources/analysis.py", rs.folder())).is_file());
    assert!(f.root.join(format!("{}/data", rs.folder())).is_dir());
    // The script is a resource and a member of the research space.
    let script = applied.resources.iter().find(|r| r.path.as_str().ends_with("analysis.py")).unwrap();
    let members = f.graph.entity_detail(&script.entity).unwrap().outgoing;
    assert!(members.iter().any(|r| r.kind == RelationKind::MemberOf && r.to == space));
    // No manuscript resource.
    let snapshot =
        f.graph.snapshot(&GraphQuery { kinds: Some(vec![EntityKind::Resource]), ..Default::default() }).unwrap();
    assert!(snapshot.entities.iter().all(|e| !e.roles.contains(&crate::vocabulary::ResourceRole::Manuscript)
        || e.key.as_deref().is_some_and(|k| k.starts_with(".exposition/"))));
    // The workflow draft is saved, admitted, well formed, and not runnable.
    let wf = &applied.workflows[0];
    assert_eq!(wf.state, DraftState::WellFormed);
    let saved = f.graph.workflow(&wf.workflow, None).unwrap();
    assert!(!saved.validation.runnable);
    assert!(wf.entity.is_some());
}

#[test]
fn existing_files_are_never_overwritten() {
    let f = fixture();
    std::fs::write(f.root.join("main.typ"), "my own work").unwrap();
    let err = precheck(&f.root, &paper()).unwrap_err();
    assert_eq!(err.code, "project.collision");
    assert_eq!(read(&f.root, "main.typ"), "my own work");
    // Even if a file appears after the precheck, create-new refuses it.
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    let bp = paper();
    let op = creator.begin(&f.root, &bp, &Provenance::Preset).unwrap();
    let err = creator.write_files(&f.root, &op, &bp, &|_| false).unwrap_err();
    assert_eq!(err.code, "project.collision");
    assert_eq!(read(&f.root, "main.typ"), "my own work");
    let statuses = creator.statuses(&f.root, &BTreeSet::new());
    assert_eq!(statuses.len(), 1);
    assert!(matches!(statuses[0].state, OperationState::NeedsReconciliation { .. }));
    assert_eq!(statuses[0].files[0].state, CreationFileState::Changed);
}

#[test]
fn interrupted_creation_is_recoverable_and_never_complete() {
    let f = fixture();
    let bp = blueprint(&request(PAPER_LEAN)).unwrap();
    let total = bp.files.len();
    assert!(total >= 3);
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    let op = creator.begin(&f.root, &bp, &Provenance::Preset).unwrap();
    // Crash after the first file.
    let err = creator.write_files(&f.root, &op, &bp, &|n| n == 1).unwrap_err();
    assert_eq!(err.code, "project.interrupted");
    // While running it is in progress, not interrupted.
    let running: BTreeSet<_> = [op.clone()].into_iter().collect();
    let s = creator.statuses(&f.root, &running);
    assert!(s[0].in_progress && s[0].actions.is_empty());
    // After a restart: needs reconciliation, shows created and missing files.
    let s = creator.statuses(&f.root, &BTreeSet::new());
    assert_eq!(s.len(), 1);
    let s = &s[0];
    assert!(matches!(s.state, OperationState::NeedsReconciliation { .. }), "{:?}", s.state);
    assert_eq!((s.created, s.missing, s.changed), (1, total as u32 - 1, 0));
    assert!(!s.graph_applied);
    assert_eq!(s.actions, vec![CreationAction::Finish, CreationAction::Keep]);
    assert_ne!(f.graph.journal().get(&f.root, &op).unwrap().state, OperationState::Completed);
    // The user edits the created file before finishing: it is kept as is.
    let first = &bp.files[0].path;
    std::fs::write(f.root.join(first), "edited by the user").unwrap();
    // Finish (explicit): opens the project, creates the missing files only.
    assert!(f.graph.attach(&f.root).is_usable());
    let (applied, _) = creator.finish(&f.root, &op).unwrap();
    assert_eq!(read(&f.root, first), "edited by the user");
    for file in &bp.files {
        assert!(f.root.join(&file.path).is_file(), "{}", file.path);
    }
    assert!(!applied.entities.is_empty());
    assert_eq!(applied.workflows.len(), 1);
    assert!(creator.statuses(&f.root, &BTreeSet::new()).is_empty(), "resolved");
    assert!(matches!(f.graph.journal().get(&f.root, &op).unwrap().state, OperationState::Resolved { .. }));
    // Finishing again is refused (nothing left to do).
    assert_eq!(creator.finish(&f.root, &op).unwrap_err().code, "project.resolved");
}

#[test]
fn reapplying_after_a_crash_between_graph_commit_and_journal_creates_no_duplicates() {
    let f = fixture();
    let bp = paper();
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    let op = creator.begin(&f.root, &bp, &Provenance::Preset).unwrap();
    creator.write_files(&f.root, &op, &bp, &|_| false).unwrap();
    assert!(f.graph.attach(&f.root).is_usable());
    let first = creator.apply_graph(&f.root, &op, &bp, &Provenance::Preset).unwrap();
    let count = f.graph.status().entities;
    let again = creator.apply_graph(&f.root, &op, &bp, &Provenance::Preset).unwrap();
    assert_eq!(f.graph.status().entities, count, "no duplicate entities");
    let ids =
        |a: &super::materialize::GraphApplied| a.entities.iter().map(|e| e.entity.clone()).collect::<BTreeSet<_>>();
    assert_eq!(ids(&first), ids(&again));
}

#[test]
fn keep_admits_what_exists_and_resolves() {
    let f = fixture();
    let bp = paper();
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    let op = creator.begin(&f.root, &bp, &Provenance::Preset).unwrap();
    creator.write_files(&f.root, &op, &bp, &|_| true).unwrap_err();
    assert!(f.graph.attach(&f.root).is_usable());
    let admitted = creator.keep(&f.root, &op).unwrap();
    assert_eq!(admitted, 1);
    assert!(f.graph.resource(&WorkspacePath::parse("main.typ").unwrap()).unwrap().is_some());
    assert!(creator.statuses(&f.root, &BTreeSet::new()).is_empty());
    // Keep applied no blueprint entities.
    let claims = f.graph.snapshot(&GraphQuery { kinds: Some(vec![EntityKind::Claim]), ..Default::default() }).unwrap();
    assert!(claims.entities.is_empty());
}

#[test]
fn graph_unavailable_leaves_the_creation_unfinished() {
    let f = fixture();
    let bp = paper();
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    let op = creator.begin(&f.root, &bp, &Provenance::Preset).unwrap();
    creator.write_files(&f.root, &op, &bp, &|_| false).unwrap();
    // The project was never opened: the graph step refuses rather than pretending.
    let err = creator.apply_graph(&f.root, &op, &bp, &Provenance::Preset).unwrap_err();
    assert_eq!(err.code, "graph.unavailable");
    let s = creator.statuses(&f.root, &BTreeSet::new());
    assert!(matches!(s[0].state, OperationState::NeedsReconciliation { .. }));
    assert!(!s[0].graph_applied);
}

#[test]
fn agent_proposed_blueprints_create_suggestions() {
    let f = fixture();
    let mut bp = paper();
    bp.source = "agent".into();
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    let op = creator.begin(&f.root, &bp, &Provenance::Preset).unwrap();
    creator.write_files(&f.root, &op, &bp, &|_| false).unwrap();
    assert!(f.graph.attach(&f.root).is_usable());
    let applied = creator.apply_graph(&f.root, &op, &bp, &Provenance::Agent { task_id: "task-123".into() }).unwrap();
    let claim = applied.entities.iter().find(|e| e.temp_id == "mainResult").unwrap();
    let detail = f.graph.entity_detail(&claim.entity).unwrap();
    assert!(
        detail
            .outgoing
            .iter()
            .filter(|r| r.kind == RelationKind::DependsOn)
            .all(|r| r.status == AssertionStatus::Suggested)
    );
    assert!(detail.entity.lifecycle == Lifecycle::Active);
}

#[test]
fn research_space_ids_follow_the_preset_or_agent_convention() {
    let original = blueprint(&request(RESEARCH)).unwrap();
    let old_id = original.research.as_ref().unwrap().id.clone();
    let text = serde_json::to_string(&original).unwrap().replace(&old_id, "main");
    let bp: ProjectBlueprint = serde_json::from_str(&text).unwrap();
    assert_eq!(bp.research.as_ref().unwrap().id, "main");
    let v = validate_blueprint(&bp);
    assert!(v.valid, "{:?}", errors(&v));
    for bad in ["Main", "rs-", "rs-AB12", "../x", "research"] {
        let mut b = bp.clone();
        b.research.as_mut().unwrap().id = bad.into();
        assert!(!validate_blueprint(&b).valid, "{bad}");
    }
}

#[test]
fn reapplying_never_overwrites_what_people_changed_since() {
    let f = fixture();
    let bp = paper();
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    let op = creator.begin(&f.root, &bp, &Provenance::Preset).unwrap();
    creator.write_files(&f.root, &op, &bp, &|_| false).unwrap();
    assert!(f.graph.attach(&f.root).is_usable());
    let first = creator.apply_graph(&f.root, &op, &bp, &Provenance::Preset).unwrap();
    let claim = first.entities.iter().find(|e| e.temp_id == "mainResult").unwrap().entity.clone();
    let rev = f.graph.entity_detail(&claim).unwrap().entity.revision;
    let request = crate::ops::ChangeRequest {
        base_revision: f.graph.status().revision,
        operations: vec![crate::ops::GraphOperation::UpdateEntity {
            entity: claim.clone(),
            expected_revision: rev,
            label: Some("Spectral gap theorem".into()),
            attributes: None,
        }],
        summary: None,
    };
    let (_, outcome) = f.graph.propose_and_apply(crate::model::Origin::User, &request).unwrap();
    assert!(matches!(outcome, crate::ops::ApplyOutcome::Applied { .. }), "{outcome:?}");
    creator.apply_graph(&f.root, &op, &bp, &Provenance::Preset).unwrap();
    assert_eq!(f.graph.entity_detail(&claim).unwrap().entity.label, "Spectral gap theorem");
}

#[test]
fn imported_blueprints_only_suggest_whatever_source_they_claim() {
    let f = fixture();
    let mut bp = blueprint(&request(LEAN)).unwrap();
    bp.source = "preset:lean".into(); // self-declared; the caller decides provenance
    let creator = Creator { graph: &f.graph, data_dir: &f.data_dir };
    let op = creator.begin(&f.root, &bp, &Provenance::Imported).unwrap();
    creator.write_files(&f.root, &op, &bp, &|_| false).unwrap();
    assert!(f.graph.attach(&f.root).is_usable());
    let applied = creator.apply_graph(&f.root, &op, &bp, &Provenance::Imported).unwrap();
    let collection = applied.entities.iter().find(|e| e.kind == EntityKind::Collection).unwrap();
    let incoming = f.graph.entity_detail(&collection.entity).unwrap().incoming;
    assert!(!incoming.is_empty());
    assert!(incoming.iter().all(|r| r.status == AssertionStatus::Suggested));
    let saved = f.graph.workflow(&applied.workflows[0].workflow, None).unwrap();
    assert!(saved.imported, "an imported blueprint's drafts are imported drafts");
    assert!(!saved.validation.runnable);
    // Finish uses the recorded provenance too.
    assert_eq!(creator.record(&op).unwrap().provenance, Provenance::Imported);
}

#[test]
fn a_bare_git_repository_layout_is_refused() {
    let mut bp = paper();
    for (path, content) in
        [("payload/HEAD", "ref: refs/heads/main\n"), ("payload/config", "[core]\n\tfsmonitor = calc\n")]
    {
        bp.files.push(BlueprintFile { path: path.into(), content: content.into() });
    }
    let v = validate_blueprint(&bp);
    assert!(!v.valid);
    assert!(v.diagnostics.iter().any(|d| d.message.contains("Git repository")), "{:?}", errors(&v));
    // Either file alone is ordinary.
    let mut bp = paper();
    bp.files.push(BlueprintFile { path: "notes/config".into(), content: String::new() });
    assert!(validate_blueprint(&bp).valid);
}
