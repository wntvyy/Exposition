//! R-m7 independent review: graph admission, hostile/imported databases,
//! recovery, and impact bounds, exercised through the public service API
//! against real files and real SQLite databases.
//!
//! Tests named `review_defect_*` reproduce a defect: they FAIL while the
//! defect exists (the assertion states the expected behavior). Tests named
//! `review_held_*` record attacks that held.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::*;
use sha2::{Digest, Sha256};

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

struct Project {
    _data: tempfile::TempDir,
    #[allow(dead_code)]
    root: PathBuf,
    svc: Arc<GraphService>,
}

fn attach(root: &Path) -> Project {
    let data = tempfile::tempdir().unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(root);
    assert!(status.is_usable(), "{status:?}");
    Project { _data: data, root: root.to_path_buf(), svc }
}

impl Project {
    fn full(&self) {
        self.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    }

    fn resource(&self, path: &str) -> Option<Entity> {
        self.svc.resource(&wp(path)).unwrap()
    }

    fn all(&self, kind: EntityKind) -> Vec<Entity> {
        self.svc
            .snapshot(&GraphQuery {
                kinds: Some(vec![kind]),
                include_inactive: false,
                limit: Some(2000),
                ..Default::default()
            })
            .unwrap()
            .entities
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(unix)]
fn junction(link: &Path, target: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// R-M7-G1. §7 "Imported evidence records are historical claims of their
/// recorded origin, not fresh local verification" / §9 "Treat imported graph
/// databases ... as untrusted data": a project cloned with its
/// `.exposition/graph.sqlite3` (written elsewhere, e.g. by a collaborator or a
/// crafted file) makes this installation's Mathematics view report a Lean
/// verdict and even `Current` freshness for a declaration that was never
/// checked here. Real local Lean evidence never carries dependency
/// fingerprints (it is always Unknown), so `Current` is reachable only through
/// such imported records; nothing marks them imported.
#[test]
fn review_defect_imported_graph_evidence_is_presented_as_current_local_verification() {
    let tmp = tempfile::tempdir().unwrap();
    // --- Elsewhere: the author's copy (any tool that writes SQLite would do).
    let origin = std::fs::canonicalize(tmp.path()).unwrap().join("origin");
    let lean = b"theorem riemann : False := sorry\n";
    write(&origin, "Proofs/Main.lean", lean);
    {
        let p = attach(&origin);
        p.full();
        let decl =
            p.all(EntityKind::FormalDeclaration).into_iter().find(|e| e.key.as_deref() == Some("riemann")).unwrap();
        let formal = FormalEvidence {
            declaration: "riemann".into(),
            declaration_kind: "theorem".into(),
            statement: "riemann : False".into(),
            axioms: vec![],
            file_verdict: FormalVerdict::Checked,
            declaration_verdict: Some(FormalVerdict::Checked),
            toolchain: "leanprover/lean4:v4.35.0".into(),
            source_path: wp("Proofs/Main.lean"),
            source_digest: sha(lean),
            revision: Some(1),
            dependency_fingerprints: Some(BTreeMap::new()),
            dependencies: FormalDependencies::Reported { names: vec![] },
        };
        let evidence = Evidence {
            id: EvidenceId::mint(),
            kind: EvidenceKind::FormalCheck,
            subject: Some(decl.id.clone()),
            operation: OperationRef { kind: "leanAudit".into(), id: "Proofs/Main.lean@1:forged".into() },
            inputs: vec![VersionedInput {
                path: wp("Proofs/Main.lean"),
                content_hash: Some(sha(lean)),
                revision: Some(1),
                unsaved: false,
            }],
            outcome: EvidenceOutcome::Verdict { verdict: FormalVerdict::Checked },
            environment: vec![("lean toolchain".into(), "leanprover/lean4:v4.35.0".into())],
            outputs: vec![],
            formal: Some(formal),
            recorded_by: Origin::Service { service: "lean".into() },
            recorded_at_ms: 1,
        };
        p.svc.register_evidence(&Origin::Service { service: "lean".into() }, evidence).unwrap();
        p.svc.detach();
    }
    // --- Here: clone/copy the project (new machine: a fresh app data dir).
    let here = std::fs::canonicalize(tmp.path()).unwrap().join("clone");
    copy_dir(&origin, &here);
    let p = attach(&here);
    p.full();
    let math = p.svc.math_overview().unwrap();
    let row = math.rows.iter().find(|r| r.entity.key.as_deref() == Some("riemann")).expect("declaration row");
    println!("imported row: verdict={:?} freshness={:?}", row.verdict, row.freshness);
    let detail = p.svc.entity_detail(&row.entity.id).unwrap();
    println!("entity detail evidence freshness: {:?}", detail.evidence_freshness);
    let shown_current = matches!(row.freshness, Some(Freshness::Current))
        || detail.evidence_freshness.iter().any(|(_, f)| matches!(f, Freshness::Current));
    if shown_current {
        println!(
            "DEFECT: a FormalCheck record that this installation never produced (copied in with the project's graph \
             database) is shown as verdict {:?} with freshness Current for `theorem riemann : False := sorry`",
            row.verdict
        );
    }
    assert!(
        !shown_current,
        "imported formal evidence must not be presented as current local verification (verdict {:?}, freshness {:?})",
        row.verdict, row.freshness
    );
}

/// R-M7-G2. §2: normal scanning excludes `.exposition/runs/`, generated
/// artifact directories, graph backups. The exclusion is applied only to the
/// top-level `.exposition`; a nested project folder (an example project, a
/// vendored copy, a sub-project) has its run records, artifacts and project
/// metadata indexed as datasets/assets of the outer project.
#[test]
fn review_defect_nested_project_metadata_is_admitted_as_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    write(&root, "main.tex", b"\\documentclass{article}\\begin{document}x\\end{document}\n");
    write(&root, "examples/demo/main.typ", b"= Demo\n");
    write(&root, "examples/demo/.exposition/project.json", b"{}");
    write(&root, "examples/demo/.exposition/runs/run-1.json", b"{\"record\":{}}");
    write(&root, "examples/demo/.exposition/artifacts/run-1/output-0.png", b"\x89PNG");
    write(&root, "examples/demo/.exposition/graph-backups/graph-1-r1-auto.json", b"{}");
    let p = attach(&root);
    p.full();
    assert!(p.resource("main.tex").is_some());
    assert!(p.resource("examples/demo/main.typ").is_some(), "the nested project's sources are project files");
    let leaked: Vec<&str> = [
        "examples/demo/.exposition/runs/run-1.json",
        "examples/demo/.exposition/artifacts/run-1/output-0.png",
        "examples/demo/.exposition/graph-backups/graph-1-r1-auto.json",
    ]
    .into_iter()
    .filter(|path| p.resource(path).is_some())
    .collect();
    for path in &leaked {
        println!("DEFECT: {path} (run record / generated artifact / backup) was admitted as a resource");
    }
    assert!(leaked.is_empty(), "run records and generated artifacts were indexed: {leaked:?}");
}

/// R-M7-G3 (Windows file names ignore case). A project whose metadata folder
/// is spelled `.Exposition` (same folder for Windows) has its run records and
/// generated artifacts admitted, because the exclusions compare `.exposition`
/// case-sensitively while the graph itself happily lives in that folder.
#[cfg(windows)]
#[test]
fn review_defect_metadata_folder_spelled_with_another_case_is_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(root.join(".Exposition")).unwrap();
    write(&root, "notes.md", b"# Notes\n");
    let p = attach(&root);
    assert!(root.join(".exposition").join("graph.sqlite3").exists(), "the graph lives in the same folder");
    write(&root, ".exposition/runs/run-1.json", b"{}");
    write(&root, ".exposition/artifacts/run-1/output-0.png", b"\x89PNG");
    p.full();
    let leaked: Vec<String> = p
        .all(EntityKind::Resource)
        .into_iter()
        .filter_map(|e| e.key)
        .filter(|k| k.to_ascii_lowercase().starts_with(".exposition/") && !k.ends_with("project.json"))
        .collect();
    for k in &leaked {
        println!("DEFECT: {k} was admitted as a project resource");
    }
    assert!(leaked.is_empty(), "{leaked:?}");
}

/// R-M7-G4. §4/§9: `graph_recover` applies any recovery option the webview
/// sends, even one the service did not offer. For a project whose
/// `.exposition` is a link that leaves the project (offered: Retry only),
/// `StartNew` renames the database *outside the project* through the link.
#[test]
fn review_defect_recover_applies_an_option_not_offered_and_writes_through_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let outside = base.join("someone-elses-folder");
    std::fs::create_dir_all(&outside).unwrap();
    // A real graph database belonging to something else.
    drop(exposition_graph::store::GraphStore::open(&outside.join("graph.sqlite3"), true).unwrap());
    let before = std::fs::read(outside.join("graph.sqlite3")).unwrap();
    let root = base.join("project");
    std::fs::create_dir_all(&root).unwrap();
    write(&root, "main.typ", b"= Paper\n");
    junction(&root.join(".exposition"), &outside);
    let data = tempfile::tempdir().unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    let GraphState::Unavailable { recovery, reason } = &status.state else { panic!("{status:?}") };
    println!("refused: {reason}; offered: {recovery:?}");
    assert!(!recovery.contains(&RecoveryOption::StartNew), "StartNew is not offered for a link");
    let result = svc.recover(&RecoveryOption::StartNew);
    println!("recover(StartNew) -> {result:?}");
    let still_there = std::fs::read(outside.join("graph.sqlite3")).ok();
    let renamed: Vec<String> = std::fs::read_dir(&outside)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("graph.damaged-"))
        .collect();
    if still_there.as_deref() != Some(before.as_slice()) || !renamed.is_empty() {
        println!(
            "DEFECT: recover(StartNew) (not offered) moved {}\\graph.sqlite3 outside the project to {renamed:?}",
            outside.display()
        );
    }
    assert!(result.is_err(), "an option the service did not offer must be refused");
    assert_eq!(still_there.as_deref(), Some(before.as_slice()), "a file outside the project was changed");
}

/// R-M7-G5. A graph written by a newer version is refused and left
/// unchanged (offered: Retry only); the IPC still accepts `StartNew`, which
/// sets the newer graph aside and starts an empty one.
#[test]
fn review_defect_newer_schema_graph_can_be_replaced_by_an_option_not_offered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    write(&root, "main.typ", b"= Paper\n");
    std::fs::create_dir_all(root.join(".exposition")).unwrap();
    let db = root.join(".exposition").join("graph.sqlite3");
    {
        let store = exposition_graph::store::GraphStore::open(&db, true).unwrap();
        store.connection().pragma_update(None, "user_version", 99).unwrap();
    }
    let data = tempfile::tempdir().unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    let GraphState::Unavailable { recovery, .. } = &status.state else { panic!("{status:?}") };
    assert_eq!(recovery, &vec![RecoveryOption::Retry], "only Retry is offered for a newer graph");
    let result = svc.recover(&RecoveryOption::StartNew);
    println!("recover(StartNew) on a newer-schema graph -> {:?}", result.as_ref().map(|s| &s.state));
    if result.as_ref().is_ok_and(|s| s.is_usable()) {
        println!(
            "DEFECT: a newer-schema graph was set aside and replaced by an empty graph through an option not offered"
        );
    }
    assert!(result.is_err(), "options the service did not offer must be refused");
}

/// Builds a structurally valid graph database at the row limits' scale.
fn big_valid_database(db: &Path, entities: usize, relations: usize) {
    let store = exposition_graph::store::GraphStore::open(db, true).unwrap();
    let conn = store.connection();
    conn.execute_batch("BEGIN").unwrap();
    {
        let mut e = conn
            .prepare(
                "INSERT INTO entities (id, kind, roles, label, key, lifecycle, tombstone_reason, revision, origin, attributes, created_at, updated_at) \
                 VALUES (?1, 'lemma', '[]', ?2, NULL, 'active', NULL, 1, '{\"kind\":\"user\"}', '{}', 1, 1)",
            )
            .unwrap();
        for i in 0..entities {
            e.execute(rusqlite::params![format!("ent-{i:032x}"), format!("Lemma {i}")]).unwrap();
        }
        let mut r = conn
            .prepare(
                "INSERT INTO relations (id, kind, from_id, to_id, origin, status, evidence_kind, evidence, from_version, to_version, note, revision, created_at, updated_at) \
                 VALUES (?1, 'dependsOn', ?2, ?3, '{\"kind\":\"user\"}', 'accepted', NULL, '[]', NULL, NULL, NULL, 1, 1, 1)",
            )
            .unwrap();
        for i in 0..relations {
            let a = i % entities;
            let b = (i / entities + a + 1) % entities;
            r.execute(rusqlite::params![format!("rel-{i:032x}"), format!("ent-{a:032x}"), format!("ent-{b:032x}")])
                .unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();
}

/// R-M7-G6 (measurement). A database within every limit the store accepts is
/// fully decoded while attaching, with the graph lock held, and
/// `workspace_open` awaits the attach. Prints the time; fails when opening
/// such a project would block for more than 30 s.
#[test]
#[ignore = "builds a ~0.5 GB database; run with --ignored --nocapture"]
fn review_probe_attach_time_of_a_large_valid_database() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(root.join(".exposition")).unwrap();
    let db = root.join(".exposition").join("graph.sqlite3");
    let t = Instant::now();
    big_valid_database(&db, 500_000, 2_000_000);
    println!("built in {:?}, {} MB", t.elapsed(), std::fs::metadata(&db).unwrap().len() / 1_048_576);
    let data = tempfile::tempdir().unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let t = Instant::now();
    let status = svc.attach(&root);
    let took = t.elapsed();
    println!("attach -> {:?} in {took:?}", std::mem::discriminant(&status.state));
    if took.as_secs() > 30 {
        println!("DEFECT: opening a project whose graph is within the accepted limits blocked for {took:?}");
    }
    assert!(took.as_secs() <= 30, "attach took {took:?}");
}

/// Hostile databases held: every variant below is refused, left byte-for-byte
/// unchanged, never replaced by an empty graph, and ordinary scanning does not
/// touch it.
#[test]
fn review_held_hostile_databases_are_refused_and_left_unchanged() {
    let cases: &[(&str, &str)] = &[
        ("view", "CREATE VIEW v AS SELECT * FROM entities;"),
        ("trigger", "CREATE TRIGGER t AFTER INSERT ON entities BEGIN DELETE FROM meta; END;"),
        ("extra table", "CREATE TABLE extra (x);"),
        ("extra index", "CREATE INDEX extra_ix ON entities(lower(label));"),
        (
            "bad endpoint kinds",
            "INSERT INTO entities VALUES ('ent-00000000000000000000000000000001','resource','[\"manuscript\"]','a.tex','a.tex','active',NULL,1,'{\"kind\":\"user\"}','{}',1,1);\
             INSERT INTO entities VALUES ('ent-00000000000000000000000000000002','lemma','[]','L','k','active',NULL,1,'{\"kind\":\"user\"}','{}',1,1);\
             INSERT INTO relations VALUES ('rel-00000000000000000000000000000001','proofOf','ent-00000000000000000000000000000001','ent-00000000000000000000000000000002','{\"kind\":\"user\"}','accepted',NULL,'[]',NULL,NULL,NULL,1,1,1);",
        ),
        (
            "unknown kind",
            "INSERT INTO entities VALUES ('ent-00000000000000000000000000000003','spaceship','[]','x',NULL,'active',NULL,1,'{\"kind\":\"user\"}','{}',1,1);",
        ),
        (
            "oversized label",
            "INSERT INTO entities VALUES ('ent-00000000000000000000000000000004','lemma','[]',hex(zeroblob(200000)),NULL,'active',NULL,1,'{\"kind\":\"user\"}','{}',1,1);",
        ),
        (
            "evidence id mismatch",
            "INSERT INTO evidence VALUES ('evd-00000000000000000000000000000001','formalCheck',NULL,'{\"id\":\"evd-00000000000000000000000000000002\"}',1);",
        ),
        ("bad revision", "UPDATE meta SET value = 'NaN' WHERE key = 'revision';"),
    ];
    for (name, sql) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        write(&root, "main.typ", b"= Paper\n");
        std::fs::create_dir_all(root.join(".exposition")).unwrap();
        let db = root.join(".exposition").join("graph.sqlite3");
        {
            let store = exposition_graph::store::GraphStore::open(&db, true).unwrap();
            store.connection().execute_batch(sql).unwrap();
        }
        let before = std::fs::read(&db).unwrap();
        let data = tempfile::tempdir().unwrap();
        let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
        let status = svc.attach(&root);
        let refused = matches!(status.state, GraphState::Unavailable { .. });
        let _ = svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true });
        let snapshot = svc.snapshot(&GraphQuery::default());
        let after = std::fs::read(&db).unwrap();
        println!(
            "{} {name}: refused={refused} unchanged={} snapshot_err={}",
            if refused && after == before && snapshot.is_err() { "ok:" } else { "DEFECT:" },
            after == before,
            snapshot.is_err()
        );
        assert!(refused, "{name}: {status:?}");
        assert_eq!(after, before, "{name}: the refused database changed");
        assert!(snapshot.is_err(), "{name}: graph reads must report unavailable");
    }
}

/// Impact held: cycles terminate and are reported, references ask for a
/// citation review (never a dependency), and a large fan-out is bounded.
#[test]
fn review_held_impact_is_bounded_and_keeps_references_apart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let p = attach(&root);
    let mut ops = Vec::new();
    let create = |t: &str, kind: EntityKind, label: &str| GraphOperation::CreateEntity {
        temp_id: t.into(),
        kind,
        roles: vec![],
        label: label.into(),
        key: None,
        attributes: BTreeMap::new(),
        binding: None,
    };
    let rel = |kind: RelationKind, a: &str, b: &str| GraphOperation::AddRelation {
        kind,
        from: EntityRef::Temp { temp_id: a.into() },
        to: EntityRef::Temp { temp_id: b.into() },
        evidence_kind: None,
        evidence: vec![],
        note: None,
    };
    ops.push(create("a", EntityKind::Lemma, "A"));
    ops.push(create("b", EntityKind::Lemma, "B"));
    ops.push(create("c", EntityKind::Theorem, "C"));
    ops.push(create("r", EntityKind::Claim, "R"));
    ops.push(rel(RelationKind::DependsOn, "b", "a"));
    ops.push(rel(RelationKind::DependsOn, "c", "b"));
    ops.push(rel(RelationKind::DependsOn, "a", "c"));
    ops.push(rel(RelationKind::References, "r", "a"));
    let (proposal, outcome) = p
        .svc
        .propose_and_apply(Origin::User, &ChangeRequest { base_revision: 0, operations: ops, summary: None })
        .unwrap();
    assert!(proposal.valid, "{:?}", proposal.diagnostics);
    let ApplyOutcome::Applied { created, .. } = outcome else { panic!("{outcome:?}") };
    let a = created.iter().find(|c| c.temp_id == "a").unwrap().id.clone();
    let r = created.iter().find(|c| c.temp_id == "r").unwrap().id.clone();
    let t = Instant::now();
    let report = p
        .svc
        .impact(&ImpactRequest { entities: vec![a.clone()], max_depth: Some(99), max_items: Some(99_999) })
        .unwrap();
    println!("impact in {:?}: items={} cycles={:?}", t.elapsed(), report.items.len(), report.cycles.len());
    assert!(!report.cycles.is_empty(), "the a→c→b→a cycle is reported");
    let ref_item = report.items.iter().find(|i| i.entity.id == r).expect("the referencing remark");
    assert_eq!(ref_item.action, ImpactAction::CitationReview, "a reference is not a dependency");
    println!("ok: cycle reported, reference → citation review ({})", ref_item.reason);
}

/// R-M7-G7. §5: "Ambiguous external moves, duplicate labels, deleted
/// declarations and uncertain matches produce unresolved bindings or
/// tombstones; do not silently merge or reassign identities." A deleted file
/// and an unrelated new file that merely share trivially common content (an
/// empty `__init__.py`) are treated as a rename: the new file inherits the old
/// identity together with its curated relations (collection membership,
/// research provenance), although nothing confirms a move.
#[test]
fn review_defect_identical_trivial_content_reassigns_identity_and_curated_relations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    write(&root, "solver/__init__.py", b"");
    write(&root, "solver/core.py", b"def solve():\n    return 1\n");
    let p = attach(&root);
    p.full();
    let old = p.resource("solver/__init__.py").unwrap();
    let (proposal, outcome) = p
        .svc
        .propose_and_apply(
            Origin::User,
            &ChangeRequest {
                base_revision: p.svc.status().revision,
                operations: vec![
                    GraphOperation::CreateEntity {
                        temp_id: "c".into(),
                        kind: EntityKind::Collection,
                        roles: vec![],
                        label: "Validated solver package".into(),
                        key: None,
                        attributes: BTreeMap::new(),
                        binding: None,
                    },
                    GraphOperation::AddRelation {
                        kind: RelationKind::MemberOf,
                        from: EntityRef::Id { id: old.id.clone() },
                        to: EntityRef::Temp { temp_id: "c".into() },
                        evidence_kind: None,
                        evidence: vec![],
                        note: Some("reviewed".into()),
                    },
                ],
                summary: None,
            },
        )
        .unwrap();
    assert!(proposal.valid, "{:?}", proposal.diagnostics);
    assert!(matches!(outcome, ApplyOutcome::Applied { .. }));
    // Unrelated change: the solver package is deleted; a plotting package with
    // its own (also empty) __init__.py is added.
    std::fs::remove_dir_all(root.join("solver")).unwrap();
    write(&root, "plotting/__init__.py", b"");
    write(&root, "plotting/figures.py", b"import matplotlib\n");
    p.full();
    let new = p.resource("plotting/__init__.py").unwrap();
    let detail = p.svc.entity_detail(&new.id).unwrap();
    let inherited = detail.outgoing.iter().any(|r| r.kind == RelationKind::MemberOf);
    println!(
        "old solver/__init__.py = {}, new plotting/__init__.py = {} (same identity: {}), member of 'Validated solver package': {inherited}",
        old.id,
        new.id,
        new.id == old.id
    );
    if new.id == old.id {
        println!(
            "DEFECT: plotting/__init__.py silently took the identity of the deleted solver/__init__.py (both empty) and \
             inherited its curated membership"
        );
    }
    assert_ne!(new.id, old.id, "an unconfirmed match on trivially common content must not transfer identity");
}

/// R-M7-G8. §4: "Read-only folders receive an explicitly temporary/read-only
/// graph context ... graph-dependent actions report unavailable or
/// unsynchronized rather than claiming successful updates." A project whose
/// existing graph file cannot be written (read-only media, a read-only
/// attribute, a read-only share) is reported `Attached` like a writable one.
#[test]
fn review_defect_existing_graph_on_read_only_storage_is_reported_attached() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    write(&root, "main.typ", b"= Paper\n");
    {
        let p = attach(&root);
        p.full();
        p.svc.detach();
    }
    let db = root.join(".exposition").join("graph.sqlite3");
    let mut perm = std::fs::metadata(&db).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&db, perm).unwrap();
    let data = tempfile::tempdir().unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    println!("status on a read-only graph file: {:?}", status.state);
    write(&root, "notes.md", b"# Notes\n");
    let reconcile = svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true });
    let change = svc.propose_and_apply(
        Origin::User,
        &ChangeRequest {
            base_revision: svc.status().revision,
            operations: vec![GraphOperation::CreateEntity {
                temp_id: "q".into(),
                kind: EntityKind::Question,
                roles: vec![],
                label: "Q".into(),
                key: None,
                attributes: BTreeMap::new(),
                binding: None,
            }],
            summary: None,
        },
    );
    println!("reconcile -> {:?}", reconcile.as_ref().map(|_| "ok").map_err(|e| e.message.clone()));
    println!("change -> {:?}", change.as_ref().map(|(_, o)| std::mem::discriminant(o)).map_err(|e| e.message.clone()));
    let after = svc.status();
    println!("status after: {:?} freshness {:?}", after.state, after.freshness.state);
    let mut perm = std::fs::metadata(&db).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&db, perm).unwrap();
    let honest = !matches!(status.state, GraphState::Attached) || (reconcile.is_ok() && change.is_ok());
    if !honest {
        println!(
            "DEFECT: the graph reports Attached (writable) while every write fails; it is not presented as a temporary/read-only context"
        );
    }
    assert!(honest, "a graph that cannot be written must not be reported as an ordinary attached graph");
}

/// R-M7-G9. §4 ("A damaged or unavailable graph must not prevent ordinary
/// source editing", curated state preserved) / §9 (bounded hostile input):
/// `graph_view_save` accepts any number of saved views, but a database with
/// more than 10 000 views is refused as untrusted on the next open — the
/// project's graph (with its curated records) becomes unavailable until it is
/// replaced or restored.
#[test]
fn review_defect_saving_many_views_makes_the_graph_unopenable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    write(&root, "main.typ", b"= Paper\n");
    let p = attach(&root);
    p.full();
    let input = |i: usize| GraphViewInput {
        id: None,
        name: format!("View {i}"),
        scope: ViewScope::Project,
        nodes: vec![],
        viewport: Viewport { x: 0.0, y: 0.0, zoom: 1.0 },
        filters: ViewFilters { kinds: vec![], relation_kinds: vec![], text: None, show_inactive: false, depth: None },
        hidden: vec![],
        collapsed: vec![],
        expected_revision: None,
    };
    let t = Instant::now();
    let mut saved = 0;
    for i in 0..10_001 {
        if p.svc.save_view(&input(i)).is_ok() {
            saved += 1;
        } else {
            break;
        }
    }
    println!("saved {saved} views in {:?}", t.elapsed());
    p.svc.detach();
    let status = p.svc.attach(&root);
    println!("reopen -> {:?}", status.state);
    let unopenable = matches!(status.state, GraphState::Unavailable { .. });
    if saved > 10_000 && unopenable {
        println!("DEFECT: {saved} views were accepted one by one; the project's graph is then refused on open");
    }
    assert!(!(saved > 10_000 && unopenable), "every save was accepted, then the graph became unavailable");
}
