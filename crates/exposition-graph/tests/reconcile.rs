//! Reconciliation with real files: admission, identity across renames and
//! deletions, curated records preserved, incremental checkpoints, forced
//! reindex, folders, temporary graphs, and backups/restore.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, ReconcileReport, Scope};
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

fn project(files: &[(&str, &str)]) -> Project {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (rel, body) in files {
        write(&root, rel, body);
    }
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    assert!(status.is_usable(), "{status:?}");
    Project { _data: data, _dir: dir, root, svc }
}

impl Project {
    fn full(&self) -> ReconcileReport {
        self.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap()
    }

    fn paths(&self, paths: &[&str]) -> ReconcileReport {
        self.svc
            .reconcile(&ReconcileOptions { scope: Scope::Paths(paths.iter().map(|p| wp(p)).collect()), force: false })
            .unwrap()
    }

    fn resource(&self, path: &str) -> Option<Entity> {
        self.svc.resource(&wp(path)).unwrap()
    }

    fn entity(&self, id: &EntityId) -> Entity {
        self.svc.entity_detail(id).unwrap().entity
    }

    fn all(&self, kind: EntityKind) -> Vec<Entity> {
        self.svc
            .snapshot(&GraphQuery {
                kinds: Some(vec![kind]),
                include_inactive: true,
                limit: Some(2000),
                ..Default::default()
            })
            .unwrap()
            .entities
    }

    fn user(&self, ops: Vec<GraphOperation>) -> ApplyOutcome {
        let base = self.svc.status().revision;
        let (p, out) = self
            .svc
            .propose_and_apply(Origin::User, &ChangeRequest { base_revision: base, operations: ops, summary: None })
            .unwrap();
        assert!(p.valid, "{:?}", p.diagnostics);
        out
    }
}

fn add_rel(kind: RelationKind, from: &EntityId, to: &EntityId) -> GraphOperation {
    GraphOperation::AddRelation {
        kind,
        from: EntityRef::Id { id: from.clone() },
        to: EntityRef::Id { id: to.clone() },
        evidence_kind: None,
        evidence: vec![],
        note: None,
    }
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

const MANUSCRIPT_FILES: &[(&str, &str)] = &[
    ("main.tex", "\\documentclass{article}\n\\begin{document}\n\\input{chapters/intro}\n\\end{document}\n"),
    ("chapters/intro.tex", "Intro text.\n"),
    ("refs.bib", "@book{knuth1984, title = {The TeXbook}, year = 1984}\n"),
    ("figures/box.png", "png-bytes"),
    ("analysis/fit.py", "import numpy\n"),
    ("main.aux", "aux"),
    ("main.log", "log"),
    ("main.synctex.gz", "gz"),
    (".git/HEAD", "ref: refs/heads/main"),
    ("node_modules/pkg/index.js", "x"),
    (".lake/build/A.olean", "o"),
    ("analysis/__pycache__/fit.cpython-314.pyc", "pyc"),
    (".exposition/runs/run-1.json", "{}"),
    (".exposition/artifacts/run-1/output-0.png", "artifact"),
    (".exposition/research/rs-1/sources/sim.py", "print(1)\n"),
    (".exposition/project.json", "{}"),
    ("CLAUDE.md", "planning"),
];

#[test]
fn a_project_is_indexed_with_stable_identities_and_nothing_excluded_becomes_an_entity() {
    let p = project(MANUSCRIPT_FILES);
    let report = p.full();
    assert_eq!(report.resources_added, 7, "{report:?}");
    let resources: Vec<String> = p.all(EntityKind::Resource).into_iter().filter_map(|e| e.key).collect();
    for expected in [
        "main.tex",
        "chapters/intro.tex",
        "refs.bib",
        "figures/box.png",
        "analysis/fit.py",
        ".exposition/research/rs-1/sources/sim.py",
        ".exposition/project.json",
    ] {
        assert!(resources.iter().any(|r| r == expected), "{expected} in {resources:?}");
    }
    for excluded in ["main.aux", "main.log", ".git/HEAD", ".exposition/runs/run-1.json", "CLAUDE.md"] {
        assert!(!resources.iter().any(|r| r == excluded), "{excluded} must not be an entity");
    }
    assert!(!resources.iter().any(|r| r.contains("artifacts/")), "generated artifacts are not scanned");
    // Roles from the policy; folders and containment.
    assert_eq!(p.resource("refs.bib").unwrap().roles, vec![ResourceRole::Bibliography]);
    let mut folders: Vec<String> = p.all(EntityKind::Folder).into_iter().filter_map(|e| e.key).collect();
    folders.sort();
    assert_eq!(
        folders,
        [".exposition/research/rs-1", ".exposition/research/rs-1/sources", "analysis", "chapters", "figures"],
        "relevant folders only (no .exposition/, caches, or dependency folders)"
    );
    let intro = p.resource("chapters/intro.tex").unwrap();
    let detail = p.svc.entity_detail(&intro.id).unwrap();
    let container = detail.incoming.iter().find(|r| r.kind == RelationKind::Contains).expect("contained");
    let chapters = p.all(EntityKind::Folder).into_iter().find(|f| f.key.as_deref() == Some("chapters")).unwrap();
    assert_eq!(container.from, chapters.id);
    // Coverage is recorded per file.
    let status = p.svc.status();
    assert!(status.coverage.resources >= 7, "{:?}", status.coverage);
    assert_eq!(status.freshness.state, SyncState::Synchronized);
    // A second pass reads nothing (checkpoints) and changes nothing.
    let revision = status.revision;
    let again = p.full();
    assert_eq!(again.read, 0, "{again:?}");
    assert_eq!(p.svc.status().revision, revision, "an unchanged project does not move the graph revision");
    // Ids are stable across passes and reattachment.
    let id = p.resource("main.tex").unwrap().id;
    p.svc.detach();
    p.svc.attach(&p.root);
    p.full();
    assert_eq!(p.resource("main.tex").unwrap().id, id);
}

#[test]
fn renames_keep_identity_deletions_tombstone_and_curated_records_survive() {
    let p = project(&[("notes/a.md", "# A\n"), ("notes/b.md", "# B\n"), ("data/obs.csv", "x,y\n1,2\n")]);
    p.full();
    let a = p.resource("notes/a.md").unwrap();
    let b = p.resource("notes/b.md").unwrap();
    let data = p.resource("data/obs.csv").unwrap();
    // Curated: a collection holding both notes, and a claim using the data.
    let out =
        p.user(vec![create("c", EntityKind::Collection, "Reading list"), create("k", EntityKind::Claim, "Claim")]);
    let ApplyOutcome::Applied { created, .. } = out else { panic!("{out:?}") };
    let (collection, claim) = (created[0].id.clone(), created[1].id.clone());
    p.user(vec![
        add_rel(RelationKind::MemberOf, &a.id, &collection),
        add_rel(RelationKind::MemberOf, &b.id, &collection),
        add_rel(RelationKind::References, &claim, &data.id),
    ]);

    // External rename (same physical file): same identity.
    std::fs::rename(p.root.join("notes/a.md"), p.root.join("notes/renamed.md")).unwrap();
    let r = p.paths(&["notes/a.md", "notes/renamed.md"]);
    assert_eq!(r.resources_renamed, 1, "{r:?}");
    let renamed = p.resource("notes/renamed.md").unwrap();
    assert_eq!(renamed.id, a.id, "rename keeps the id");
    assert_eq!(renamed.label, "renamed.md");
    assert!(p.resource("notes/a.md").is_none());

    // Moved into another folder and edited while the app was not watching:
    // the physical identity confirms it.
    std::fs::create_dir_all(p.root.join("archive")).unwrap();
    std::fs::rename(p.root.join("notes/b.md"), p.root.join("archive/b.md")).unwrap();
    write(&p.root, "archive/b.md", "# B, edited\n");
    let moved_folder_before = p.all(EntityKind::Folder).len();
    let r = p.full();
    assert_eq!(r.resources_renamed, 1, "{r:?}");
    assert_eq!(p.resource("archive/b.md").unwrap().id, b.id);
    assert!(p.all(EntityKind::Folder).iter().any(|f| f.key.as_deref() == Some("archive")));
    let _ = moved_folder_before;

    // Deleted: tombstoned (identity kept), curated relations kept.
    std::fs::remove_file(p.root.join("data/obs.csv")).unwrap();
    let r = p.full();
    assert_eq!(r.resources_removed, 1, "{r:?}");
    let gone = p.entity(&data.id);
    assert_eq!(gone.lifecycle, Lifecycle::Tombstoned);
    assert!(gone.tombstone_reason.unwrap().contains("data/obs.csv"));
    let claim_detail = p.svc.entity_detail(&claim).unwrap();
    assert!(
        claim_detail.outgoing.iter().any(|r| r.kind == RelationKind::References && r.to == data.id),
        "a curated relation is never deleted by reconciliation"
    );
    let members = p.svc.entity_detail(&collection).unwrap().incoming;
    assert_eq!(members.iter().filter(|r| r.kind == RelationKind::MemberOf).count(), 2);
    // The empty folder is retired.
    let data_folder = p.all(EntityKind::Folder).into_iter().find(|f| f.key.as_deref() == Some("data")).unwrap();
    assert_eq!(data_folder.lifecycle, Lifecycle::Tombstoned);

    // Restored with the same content at the same path: the same resource again.
    write(&p.root, "data/obs.csv", "x,y\n1,2\n");
    p.full();
    let back = p.resource("data/obs.csv").unwrap();
    assert_eq!(back.id, data.id, "restored at the same path with the same content");
    assert_eq!(back.lifecycle, Lifecycle::Active);
}

#[test]
fn ambiguous_moves_are_not_guessed() {
    let p = project(&[("one.txt", "same"), ("two.txt", "same")]);
    p.full();
    let one = p.resource("one.txt").unwrap();
    let two = p.resource("two.txt").unwrap();
    // Replace both by copies elsewhere (new physical files, identical content).
    for (old, new) in [("one.txt", "x/one-copy.txt"), ("two.txt", "x/two-copy.txt")] {
        write(&p.root, new, "same");
        std::fs::remove_file(p.root.join(old)).unwrap();
    }
    let r = p.full();
    assert_eq!(r.resources_renamed, 0, "two equal candidates: nothing is guessed");
    assert_eq!(p.entity(&one.id).lifecycle, Lifecycle::Tombstoned);
    assert_eq!(p.entity(&two.id).lifecycle, Lifecycle::Tombstoned);
    let new = p.resource("x/one-copy.txt").unwrap();
    assert_ne!(new.id, one.id);
    assert_ne!(new.id, two.id);
}

#[test]
fn a_unique_same_content_move_keeps_identity_even_across_physical_files() {
    const DRAFT: &str = "= Draft\nThis draft has enough distinctive content that identical bytes elsewhere are the same \
                         document moved, not a coincidence: unique content 42.\n";
    let p = project(&[("draft.typ", DRAFT), ("tiny.typ", "= T\n")]);
    p.full();
    let id = p.resource("draft.typ").unwrap().id;
    let tiny = p.resource("tiny.typ").unwrap().id;
    // Copy + delete (a new physical file, e.g. a branch switch rewriting files).
    write(&p.root, "paper/draft.typ", DRAFT);
    std::fs::remove_file(p.root.join("draft.typ")).unwrap();
    // A trivially small file is never matched by content alone (R-M7-G7).
    write(&p.root, "paper/tiny.typ", "= T\n");
    std::fs::remove_file(p.root.join("tiny.typ")).unwrap();
    let r = p.full();
    assert_eq!(r.resources_renamed, 1, "{r:?}");
    assert_eq!(p.resource("paper/draft.typ").unwrap().id, id);
    assert_ne!(p.resource("paper/tiny.typ").unwrap().id, tiny, "no identity transfer on trivial content");
    assert_eq!(p.entity(&tiny).lifecycle, Lifecycle::Tombstoned);
}

#[test]
fn forced_reindex_rebuilds_projections_only() {
    let p = project(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
    p.full();
    let a = p.resource("a.md").unwrap();
    let out = p.user(vec![create("c", EntityKind::Collection, "Set")]);
    let ApplyOutcome::Applied { created, .. } = out else { panic!() };
    p.user(vec![add_rel(RelationKind::MemberOf, &a.id, &created[0].id)]);
    let before = p.svc.entity_detail(&a.id).unwrap();
    let report = p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    assert_eq!(report.read, 2, "every file is read again");
    assert_eq!(report.resources_added + report.resources_removed + report.resources_renamed, 0);
    let after = p.svc.entity_detail(&a.id).unwrap();
    assert_eq!(after.entity.id, before.entity.id);
    assert_eq!(after.outgoing, before.outgoing, "curated relations untouched");
}

#[test]
fn a_burst_of_changes_like_a_branch_switch_is_reconciled() {
    let mut files: Vec<(String, String)> = (0..300).map(|i| (format!("src/m{i}.py"), format!("x = {i}\n"))).collect();
    files.push(("main.typ".into(), "= Main\n".into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let p = project(&refs);
    p.full();
    let main = p.resource("main.typ").unwrap().id;
    let kept = p.resource("src/m7.py").unwrap().id;
    // The "other branch": half the files gone, many edited, new ones added.
    for i in 0..150 {
        std::fs::remove_file(p.root.join(format!("src/m{i}.py"))).unwrap();
    }
    for i in 150..300 {
        write(&p.root, &format!("src/m{i}.py"), &format!("x = {i} # other branch\n"));
    }
    for i in 0..50 {
        write(&p.root, &format!("lib/n{i}.py"), &format!("y = {i}\n"));
    }
    let r = p.full();
    assert_eq!(r.resources_removed, 150, "{r:?}");
    assert_eq!(r.resources_added, 50, "{r:?}");
    assert_eq!(p.entity(&kept).lifecycle, Lifecycle::Tombstoned);
    assert_eq!(p.resource("main.typ").unwrap().id, main);
    assert_eq!(p.svc.status().freshness.state, SyncState::Synchronized);
}

#[test]
fn a_renamed_folder_keeps_its_identity() {
    let p = project(&[("chapters/one.tex", "One\n"), ("chapters/two.tex", "Two\n")]);
    p.full();
    let folder = p.all(EntityKind::Folder).into_iter().find(|f| f.key.as_deref() == Some("chapters")).unwrap();
    std::fs::rename(p.root.join("chapters"), p.root.join("parts")).unwrap();
    let r = p.paths(&["chapters", "parts"]);
    assert_eq!(r.resources_renamed, 2, "{r:?}");
    let parts = p.all(EntityKind::Folder).into_iter().find(|f| f.key.as_deref() == Some("parts")).unwrap();
    assert_eq!(parts.id, folder.id, "the folder kept its identity");
    let one = p.resource("parts/one.tex").unwrap();
    let container =
        p.svc.entity_detail(&one.id).unwrap().incoming.into_iter().find(|r| r.kind == RelationKind::Contains);
    assert_eq!(container.unwrap().from, folder.id);
}

#[test]
fn a_folder_that_cannot_hold_the_graph_gets_a_temporary_graph_that_still_reconciles() {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    write(&root, "main.typ", "= Read only\n");
    // `.exposition` is a file: project metadata cannot be created here.
    std::fs::write(root.join(".exposition"), "not a folder").unwrap();
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = svc.attach(&root);
    assert!(matches!(status.state, GraphState::TemporaryReadOnly { .. }), "{status:?}");
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    assert!(svc.resource(&wp("main.typ")).unwrap().is_some());
    assert_eq!(svc.backup("manual").unwrap_err().code, "graph.temporary");
    assert_eq!(std::fs::read(root.join(".exposition")).unwrap(), b"not a folder");
}

#[test]
fn a_damaged_graph_can_be_restored_from_a_consistent_backup() {
    let p = project(&[("a.md", "# A\n")]);
    p.full();
    let a = p.resource("a.md").unwrap();
    let out = p.user(vec![create("c", EntityKind::Collection, "Kept")]);
    let ApplyOutcome::Applied { created, .. } = out else { panic!() };
    p.user(vec![add_rel(RelationKind::MemberOf, &a.id, &created[0].id)]);
    let backup = p.svc.backup("manual").unwrap();
    assert!(p.svc.backups().unwrap().iter().any(|b| b.name == backup.name));
    p.svc.detach();
    let db = p.root.join(".exposition").join("graph.sqlite3");
    std::fs::write(&db, b"damaged beyond repair").unwrap();
    let status = p.svc.attach(&p.root);
    let GraphState::Unavailable { recovery, .. } = &status.state else { panic!("{status:?}") };
    let restore = recovery
        .iter()
        .find(|o| matches!(o, RecoveryOption::RestoreBackup { backup: b, .. } if b == &backup.name))
        .expect("the backup is offered")
        .clone();
    assert!(recovery.contains(&RecoveryOption::StartNew));
    let status = p.svc.recover(&restore).unwrap();
    assert!(matches!(status.state, GraphState::Attached), "{status:?}");
    let detail = p.svc.entity_detail(&a.id).unwrap();
    assert!(detail.outgoing.iter().any(|r| r.kind == RelationKind::MemberOf), "curated relations restored");
    let kept: Vec<_> = std::fs::read_dir(p.root.join(".exposition"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("graph.damaged-"))
        .collect();
    assert_eq!(kept.len(), 1, "the damaged file is kept aside");
    assert_eq!(std::fs::read(kept[0].path()).unwrap(), b"damaged beyond repair");
}

#[test]
fn region_bindings_follow_their_text_or_become_unresolved() {
    let p = project(&[("notes.md", "Intro.\nThe central claim of this note.\nEnd.\n")]);
    p.full();
    let start = "Intro.\n".len() as u32;
    let range = exposition_core::text::TextRange::new(start, start + "The central claim of this note.".len() as u32);
    let out = p.user(vec![
        create("k", EntityKind::Claim, "Central claim"),
        GraphOperation::BindSource {
            entity: EntityRef::Temp { temp_id: "k".into() },
            binding: NewBinding { path: wp("notes.md"), selector: SourceSelector::Region, range: Some(range) },
        },
    ]);
    let ApplyOutcome::Applied { created, .. } = out else { panic!() };
    let claim = created[0].id.clone();
    // Not yet indexed since the binding was made: reconcile resolves it.
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    let b = &p.svc.entity_detail(&claim).unwrap().bindings[0];
    assert_eq!(b.state, BindingState::Resolved, "{b:?}");
    // Text inserted before it: still resolved, at the shifted range.
    write(&p.root, "notes.md", "A new first line.\nIntro.\nThe central claim of this note.\nEnd.\n");
    p.paths(&["notes.md"]);
    let b = &p.svc.entity_detail(&claim).unwrap().bindings[0];
    assert_eq!(b.state, BindingState::Resolved);
    assert_eq!(b.range.unwrap().start, start + "A new first line.\n".len() as u32);
    // Edited inside: explicitly missing, never re-anchored by position.
    write(&p.root, "notes.md", "A new first line.\nIntro.\nThe central claim, rewritten.\nEnd.\n");
    p.paths(&["notes.md"]);
    let detail = p.svc.entity_detail(&claim).unwrap();
    assert_eq!(detail.bindings[0].state, BindingState::Missing);
    assert_eq!(detail.entity.lifecycle, Lifecycle::Active, "a curated entity keeps its identity");
    let coverage = p
        .svc
        .inspect_at_source(&InspectAtSourceRequest { path: wp("notes.md"), offset: None, revision: None })
        .unwrap();
    assert!(coverage.coverage.unwrap().unresolved >= 1);
}

#[test]
fn evidence_is_current_only_for_the_indexed_version_and_unknown_is_never_current() {
    let p = project(&[("analysis/fit.py", "print(1)\n")]);
    p.full();
    let fit = p.resource("analysis/fit.py").unwrap();
    let hash = p.svc.entity_detail(&fit.id).unwrap().bindings[0].version.clone().unwrap().content_hash;
    let evidence = |hash: Option<String>, path: &str| Evidence {
        id: EvidenceId::mint(),
        kind: EvidenceKind::Computational,
        subject: Some(fit.id.clone()),
        operation: OperationRef { kind: "researchRun".into(), id: "run-1".into() },
        inputs: vec![VersionedInput { path: wp(path), content_hash: hash, revision: None, unsaved: false }],
        outcome: EvidenceOutcome::Succeeded,
        environment: vec![],
        outputs: vec![],
        formal: None,
        recorded_by: Origin::Service { service: "research".into() },
        recorded_at_ms: 1,
    };
    let svc_origin = Origin::Service { service: "research".into() };
    let current = p.svc.register_evidence(&svc_origin, evidence(Some(hash.clone()), "analysis/fit.py")).unwrap();
    let unknown = p.svc.register_evidence(&svc_origin, evidence(None, "analysis/fit.py")).unwrap();
    let freshness = |id: &EvidenceId| {
        p.svc.entity_detail(&fit.id).unwrap().evidence_freshness.into_iter().find(|(e, _)| e == id).unwrap().1
    };
    assert_eq!(freshness(&current), Freshness::Current);
    assert!(matches!(freshness(&unknown), Freshness::Unknown { .. }));
    write(&p.root, "analysis/fit.py", "print(2)\n");
    p.paths(&["analysis/fit.py"]);
    assert!(matches!(freshness(&current), Freshness::Stale { .. }), "{:?}", freshness(&current));
    std::fs::remove_file(p.root.join("analysis/fit.py")).unwrap();
    p.full();
    assert!(matches!(freshness(&current), Freshness::Stale { .. }), "a deleted input makes evidence stale");
}

#[test]
fn evidence_from_another_installation_is_imported_never_current() {
    let p = project(&[("analysis/fit.py", "print(1)\n")]);
    p.full();
    let fit = p.resource("analysis/fit.py").unwrap();
    let hash = p.svc.entity_detail(&fit.id).unwrap().bindings[0].version.clone().unwrap().content_hash;
    let origin = Origin::Service { service: "research".into() };
    let id = p
        .svc
        .register_evidence(
            &origin,
            Evidence {
                id: EvidenceId::mint(),
                kind: EvidenceKind::Computational,
                subject: Some(fit.id.clone()),
                operation: OperationRef { kind: "researchRun".into(), id: "run-1".into() },
                inputs: vec![VersionedInput {
                    path: wp("analysis/fit.py"),
                    content_hash: Some(hash),
                    revision: None,
                    unsaved: false,
                }],
                outcome: EvidenceOutcome::Succeeded,
                environment: vec![],
                outputs: vec![],
                formal: None,
                recorded_by: origin.clone(),
                recorded_at_ms: 1,
            },
        )
        .unwrap();
    let here = p.svc.entity_detail(&fit.id).unwrap();
    assert!(here.imported_evidence.is_empty(), "recorded by this installation");
    assert_eq!(here.evidence_freshness[0].1, Freshness::Current);
    // The same project opened by another installation (another key).
    p.svc.detach();
    let other_data = tempfile::tempdir().unwrap();
    let other = GraphService::new(GraphConfig { data_dir: other_data.path().to_path_buf() });
    assert!(other.attach(&p.root).is_usable());
    let there = other.entity_detail(&fit.id).unwrap();
    assert_eq!(there.imported_evidence, vec![id.clone()], "a historical claim of its recorded origin");
    let Freshness::Unknown { reason } = &there.evidence_freshness[0].1 else {
        panic!("{:?}", there.evidence_freshness)
    };
    assert!(reason.contains("recorded outside this installation"), "{reason}");
    // Back in the recording installation it is local again.
    other.detach();
    p.svc.attach(&p.root);
    assert!(p.svc.entity_detail(&fit.id).unwrap().imported_evidence.is_empty());
}

/// A folder the user cannot write to gets a temporary graph (nothing is
/// written into the project) that still indexes the files.
#[cfg(windows)]
#[test]
fn a_read_only_project_folder_gets_a_temporary_graph() {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap().join("readonly");
    write(&root, "paper.typ", "= Paper\n");
    let icacls = |args: &[&str]| {
        let status = std::process::Command::new("icacls")
            .arg(&root)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "icacls {args:?}");
    };
    // Deny creating files and folders to everyone (S-1-1-0).
    icacls(&["/deny", "*S-1-1-0:(OI)(CI)(WD,AD)"]);
    let result = std::panic::catch_unwind(|| {
        let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
        let status = svc.attach(&root);
        assert!(matches!(status.state, GraphState::TemporaryReadOnly { .. }), "{status:?}");
        svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
        assert!(svc.resource(&wp("paper.typ")).unwrap().is_some());
        assert!(!root.join(".exposition").exists(), "nothing was written into the folder");
    });
    icacls(&["/remove:d", "*S-1-1-0"]);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

#[test]
fn attaching_labels_operations_an_earlier_session_left_pending() {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let journal = OperationJournal::new(data.path());
    let op = journal.begin(OperationKind::Promotion, &root, None, "Promote a figure", vec!["attach".into()]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    // A new application session.
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    svc.attach(&root);
    let record = svc.journal().get(&root, &op.id).unwrap();
    assert!(matches!(record.state, OperationState::NeedsReconciliation { .. }), "{record:?}");
    // An operation of this session stays pending across a re-attach.
    let current = svc.journal().begin(OperationKind::ProjectCreation, &root, None, "Create", vec![]).unwrap();
    svc.detach();
    svc.attach(&root);
    assert_eq!(svc.journal().get(&root, &current.id).unwrap().state, OperationState::Pending);
}

const PAPER: &str = "\\documentclass{article}\n\\begin{document}\n\\section{Main}\n\\begin{lemma}\\label{lem:a}\nA.\n\\end{lemma}\n\\begin{proof}\nTrivial.\n\\end{proof}\n\\begin{theorem}\\label{thm:b}\nBy Lemma~\\ref{lem:a} and \\cite{knuth}.\n\\end{theorem}\n\\end{document}\n";

fn by_key(p: &Project, kind: EntityKind, key: &str) -> Option<Entity> {
    p.all(kind).into_iter().find(|e| e.key.as_deref() == Some(key) && e.lifecycle != Lifecycle::Tombstoned)
}

#[test]
fn extracted_mathematics_keeps_references_proofs_and_dependencies_apart() {
    let p = project(&[("paper.tex", PAPER), ("refs.bib", "@book{knuth, title = {The TeXbook}}\n")]);
    p.full();
    let lemma = by_key(&p, EntityKind::Lemma, "lem:a").unwrap();
    let theorem = by_key(&p, EntityKind::Theorem, "thm:b").unwrap();
    let knuth = by_key(&p, EntityKind::Reference, "knuth").unwrap();
    let t = p.svc.entity_detail(&theorem.id).unwrap();
    assert!(t.outgoing.iter().any(|r| r.kind == RelationKind::References && r.to == lemma.id));
    assert!(t.outgoing.iter().any(|r| r.kind == RelationKind::References && r.to == knuth.id));
    assert!(!t.outgoing.iter().any(|r| r.kind == RelationKind::DependsOn), "a reference is not a dependency");
    assert!(t.outgoing.iter().any(|r| r.kind == RelationKind::AnchoredIn));
    let math = p.svc.math_overview().unwrap();
    let row = |id: &EntityId| math.rows.iter().find(|r| &r.entity.id == id).unwrap().clone();
    assert_eq!(row(&lemma.id).informal, InformalState::ProofLinked, "the proof following the lemma targets it");
    assert_eq!(row(&theorem.id).informal, InformalState::NoProof);
    assert_eq!(row(&theorem.id).verdict, None, "no formal evidence: no verdict");
    // Impact of changing the lemma: the citing theorem needs a citation check.
    let report =
        p.svc.impact(&ImpactRequest { entities: vec![lemma.id.clone()], max_depth: None, max_items: None }).unwrap();
    let item = report.items.iter().find(|i| i.entity.id == theorem.id).unwrap();
    assert_eq!(item.action, ImpactAction::CitationReview);
    // Impact of changing the file: its declarations are listed (informational).
    let file = p.resource("paper.tex").unwrap();
    let report =
        p.svc.impact(&ImpactRequest { entities: vec![file.id.clone()], max_depth: None, max_items: None }).unwrap();
    assert!(report.items.iter().any(|i| i.entity.id == lemma.id && i.action == ImpactAction::Informational));
}

#[test]
fn duplicate_labels_are_ambiguous_and_counted_not_merged() {
    let p = project(&[("paper.tex", PAPER)]);
    p.full();
    let lemma = by_key(&p, EntityKind::Lemma, "lem:a").unwrap();
    // A second lemma with the same label.
    write(
        &p.root,
        "paper.tex",
        &PAPER.replace("\\begin{theorem}", "\\begin{lemma}\\label{lem:a}\nB.\n\\end{lemma}\n\\begin{theorem}"),
    );
    p.paths(&["paper.tex"]);
    let detail = p.svc.entity_detail(&lemma.id).unwrap();
    assert_eq!(detail.entity.lifecycle, Lifecycle::Active, "the known entity is kept");
    assert_eq!(detail.bindings[0].state, BindingState::Ambiguous);
    assert_eq!(p.all(EntityKind::Lemma).len(), 1, "no second entity is created for a duplicate name");
    let coverage =
        detail.coverage.or_else(|| p.svc.entity_detail(&p.resource("paper.tex").unwrap().id).unwrap().coverage);
    assert!(coverage.unwrap().unresolved >= 1);
    // The duplicate removed again: resolved, same identity.
    write(&p.root, "paper.tex", PAPER);
    p.paths(&["paper.tex"]);
    let detail = p.svc.entity_detail(&lemma.id).unwrap();
    assert_eq!(detail.bindings[0].state, BindingState::Resolved);
}

#[test]
fn removed_declarations_are_tombstoned_and_curated_declaration_bindings_go_missing() {
    let p = project(&[("paper.tex", PAPER)]);
    p.full();
    let theorem = by_key(&p, EntityKind::Theorem, "thm:b").unwrap();
    // A person's claim bound to the theorem's declaration.
    let out = p.user(vec![
        create("k", EntityKind::Claim, "My reading of theorem B"),
        GraphOperation::BindSource {
            entity: EntityRef::Temp { temp_id: "k".into() },
            binding: NewBinding {
                path: wp("paper.tex"),
                selector: SourceSelector::Declaration {
                    syntax: "latex.env:theorem".into(),
                    name: Some("thm:b".into()),
                    ordinal: None,
                },
                range: None,
            },
        },
    ]);
    let ApplyOutcome::Applied { created, .. } = out else { panic!() };
    let claim = created[0].id.clone();
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![wp("paper.tex")]), force: true }).unwrap();
    assert_eq!(p.svc.entity_detail(&claim).unwrap().bindings[0].state, BindingState::Resolved);
    // The theorem is deleted from the source.
    let start = PAPER.find("\\begin{theorem}").unwrap();
    let end = PAPER.find("\\end{theorem}").unwrap() + "\\end{theorem}\n".len();
    write(&p.root, "paper.tex", &format!("{}{}", &PAPER[..start], &PAPER[end..]));
    p.paths(&["paper.tex"]);
    let gone = p.entity(&theorem.id);
    assert_eq!(gone.lifecycle, Lifecycle::Tombstoned, "an extracted declaration whose referent vanished");
    let c = p.svc.entity_detail(&claim).unwrap();
    assert_eq!(c.entity.lifecycle, Lifecycle::Active, "a curated entity keeps its identity");
    assert_eq!(c.bindings[0].state, BindingState::Missing);
    // Restored (e.g. undo + save): the same identity comes back.
    write(&p.root, "paper.tex", PAPER);
    p.paths(&["paper.tex"]);
    assert_eq!(by_key(&p, EntityKind::Theorem, "thm:b").unwrap().id, theorem.id);
    assert_eq!(p.svc.entity_detail(&claim).unwrap().bindings[0].state, BindingState::Resolved);
}

#[test]
fn attachment_blocks_admit_their_artifact_and_undo_removes_the_observed_use() {
    let base = "= Results\n\nIntro.\n";
    let block = "// exposition:begin fig-run1\n#figure(\n  image(\".exposition/artifacts/run-1/output-0.png\", width: 80%),\n  caption: [Fit],\n) <fig-run1>\n// exposition:end fig-run1\n";
    let p = project(&[("main.typ", base), (".exposition/artifacts/run-1/output-0.png", "png-bytes")]);
    p.full();
    assert!(p.resource(".exposition/artifacts/run-1/output-0.png").is_none(), "generated outputs are not scanned");
    write(&p.root, "main.typ", &format!("{base}{block}"));
    p.paths(&["main.typ"]);
    let artifact = p.resource(".exposition/artifacts/run-1/output-0.png").expect("admitted by the attachment");
    assert!(artifact.roles.contains(&ResourceRole::Artifact), "{:?}", artifact.roles);
    let main = p.resource("main.typ").unwrap();
    let uses = |p: &Project| {
        p.svc
            .entity_detail(&main.id)
            .unwrap()
            .outgoing
            .into_iter()
            .filter(|r| r.kind == RelationKind::UsesAsset && r.to == artifact.id)
            .count()
    };
    assert_eq!(uses(&p), 1);
    // Undo/revert of the insertion: the block is gone, so is the observed use.
    write(&p.root, "main.typ", base);
    p.paths(&["main.typ"]);
    assert_eq!(uses(&p), 0);
    assert_eq!(p.entity(&artifact.id).lifecycle, Lifecycle::Active, "the retained artifact stays a resource");
}

fn coverage_state(p: &Project, path: &str) -> CoverageState {
    p.svc
        .inspect_at_source(&InspectAtSourceRequest { path: wp(path), offset: None, revision: None })
        .unwrap()
        .coverage
        .expect("indexed")
        .state
}

#[test]
fn changed_paths_read_as_pending_until_a_pass_reconciles_them() {
    let p = project(&[("a.md", "# A\n"), ("b.md", "# B\n"), ("dir/c.md", "# C\n")]);
    p.full();
    assert_eq!(coverage_state(&p, "a.md"), CoverageState::Complete);
    p.svc.mark_pending(&[wp("a.md")]).unwrap();
    assert_eq!(coverage_state(&p, "a.md"), CoverageState::Pending, "visible to agents through coverage");
    assert_eq!(coverage_state(&p, "b.md"), CoverageState::Complete);
    assert_eq!(p.svc.status().coverage.pending, 1);
    // An unchanged file: the pass clears the mark and keeps the stored state.
    p.paths(&["a.md"]);
    assert_eq!(coverage_state(&p, "a.md"), CoverageState::Complete);
    // A changed file: reconciled from its new content.
    write(&p.root, "b.md", "# B, edited\n");
    p.svc.mark_pending(&[wp("b.md")]).unwrap();
    assert_eq!(coverage_state(&p, "b.md"), CoverageState::Pending);
    p.paths(&["b.md"]);
    assert_eq!(coverage_state(&p, "b.md"), CoverageState::Complete);
    // A folder mark covers everything beneath it.
    p.svc.mark_pending(&[wp("dir")]).unwrap();
    assert_eq!(coverage_state(&p, "dir/c.md"), CoverageState::Pending);
    p.full();
    assert_eq!(coverage_state(&p, "dir/c.md"), CoverageState::Complete);
    assert_eq!(p.svc.status().coverage.pending, 0);
}

#[test]
fn two_live_identities_for_one_file_are_never_left_in_place() {
    let p = project(&[("notes.md", "# Notes\n")]);
    p.full();
    let kept = p.resource("notes.md").unwrap();
    // A second identity written around the service's checks (an old graph, a hand-made batch).
    let mut dup = kept.clone();
    dup.id = EntityId::mint();
    dup.created_at_ms = kept.created_at_ms + 1;
    p.svc
        .with_store(|s| {
            let w = exposition_graph::store::Writer::new(s.transaction()?);
            w.put_entity(&dup)?;
            w.commit()?;
            Ok(())
        })
        .unwrap();
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    let live: Vec<Entity> =
        p.all(EntityKind::Resource).into_iter().filter(|e| e.key.as_deref() == Some("notes.md")).collect();
    let alive: Vec<&Entity> = live.iter().filter(|e| e.lifecycle != Lifecycle::Tombstoned).collect();
    assert_eq!(alive.len(), 1);
    assert_eq!(alive[0].id, kept.id, "the oldest identity stays the file's resource");
    let gone = p.entity(&dup.id);
    assert_eq!(gone.lifecycle, Lifecycle::Tombstoned);
    assert!(gone.tombstone_reason.unwrap().contains(kept.id.as_str()));
}

/// A formal declaration the Lean service admitted from an audit (e.g. of an
/// unsaved buffer) is adopted by the extractor by qualified name: one
/// identity per declaration. A service-admitted declaration the extractor
/// does not see is never tombstoned by reconciliation.
#[test]
fn audit_admitted_lean_declarations_are_adopted_not_duplicated() {
    let p = project(&[("Main.lean", "-- empty for now\n")]);
    p.full();
    let lean = Origin::Service { service: "lean".into() };
    let decl = |name: &str| EntityUpsert {
        id: None,
        kind: EntityKind::FormalDeclaration,
        roles: vec![],
        label: name.into(),
        key: Some(name.into()),
        attributes: BTreeMap::new(),
        lifecycle: None,
        tombstone_reason: None,
        bindings: Some(vec![SourceBinding {
            entity: EntityId::mint(),
            path: wp("Main.lean"),
            selector: SourceSelector::Declaration {
                syntax: "lean.declaration".into(),
                name: Some(name.into()),
                ordinal: None,
            },
            range: None,
            version: None,
            fingerprint: None,
            state: BindingState::Unresolved,
            detail: None,
        }]),
    };
    let out = p
        .svc
        .apply_service_batch(
            &lean,
            &ServiceBatch {
                entities: vec![decl("Foo.bar"), decl("Foo.generated")],
                summary: "audit".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let (audited, generated) = (out.entity_ids[0].clone(), out.entity_ids[1].clone());
    // The declaration is saved later; the extractor sees `Foo.bar` but not the macro-generated one.
    write(&p.root, "Main.lean", "namespace Foo\ntheorem bar : True := trivial\nend Foo\n");
    p.paths(&["Main.lean"]);
    let named: Vec<Entity> = p
        .all(EntityKind::FormalDeclaration)
        .into_iter()
        .filter(|e| e.key.as_deref() == Some("Foo.bar") && e.lifecycle == Lifecycle::Active)
        .collect();
    assert_eq!(named.len(), 1, "one identity: {named:?}");
    assert_eq!(named[0].id, audited, "the audit-admitted entity is adopted");
    let binding = p.svc.entity_detail(&audited).unwrap().bindings.remove(0);
    assert_eq!(binding.state, BindingState::Resolved, "bound by the extractor now: {binding:?}");
    assert_eq!(p.entity(&generated).lifecycle, Lifecycle::Active, "not seen by the extractor, not tombstoned");
    // Re-running keeps one identity.
    p.full();
    let again = p
        .all(EntityKind::FormalDeclaration)
        .into_iter()
        .filter(|e| e.key.as_deref() == Some("Foo.bar") && e.lifecycle == Lifecycle::Active)
        .count();
    assert_eq!(again, 1);
}

/// A region a person binds in an unchanged, already indexed file is resolved
/// at once (ordinary passes skip unchanged files), not left "waiting for the
/// source to be indexed".
#[test]
fn a_manual_region_binding_resolves_without_a_file_change() {
    let text = "Intro.\nAssume hypothesis H. Then the image is compact.\nEnd.\n";
    let p = project(&[("notes.md", text)]);
    p.full();
    let start = text.find("Assume").unwrap() as u32;
    let end = start + "Assume hypothesis H. Then the image is compact.".len() as u32;
    let (proposal, outcome) = p
        .svc
        .propose_and_apply(
            Origin::User,
            &ChangeRequest {
                base_revision: p.svc.status().revision,
                operations: vec![GraphOperation::CreateEntity {
                    temp_id: "c".into(),
                    kind: EntityKind::Claim,
                    roles: vec![],
                    label: "Image claim".into(),
                    key: None,
                    attributes: BTreeMap::new(),
                    binding: Some(NewBinding {
                        path: wp("notes.md"),
                        selector: SourceSelector::Region,
                        range: Some(exposition_core::text::TextRange::new(start, end)),
                    }),
                }],
                summary: None,
            },
        )
        .unwrap();
    assert!(proposal.valid, "{:?}", proposal.diagnostics);
    let ApplyOutcome::Applied { created, .. } = outcome else { panic!("{outcome:?}") };
    let binding = p.svc.entity_detail(&created[0].id).unwrap().bindings.remove(0);
    assert_eq!(binding.state, BindingState::Resolved, "{binding:?}");
    assert!(binding.fingerprint.is_some(), "anchored by its text");
}

// ---- a changed file: what is located in it, and what a save invalidates ----

const ODD_TYP: &str =
    "= Odd sums\n\nTheorem. The sum of the first n odd numbers is n squared.\n\nProof. By induction on n.\n";
const ODD_STATEMENT: &str = "Theorem. The sum of the first n odd numbers is n squared.";
const ODD_LEAN: &str = "def oddSum : Nat -> Nat\n  | 0 => 0\n  | n + 1 => oddSum n + (2 * n + 1)\n\ntheorem oddSum_eq_sq (n : Nat) : oddSum n = n * n := by\n  sorry\n";

fn bound(temp: &str, kind: EntityKind, label: &str, path: &str, range: Option<(u32, u32)>) -> GraphOperation {
    GraphOperation::CreateEntity {
        temp_id: temp.into(),
        kind,
        roles: vec![],
        label: label.into(),
        key: None,
        attributes: BTreeMap::new(),
        binding: Some(NewBinding {
            path: wp(path),
            selector: if range.is_some() { SourceSelector::Region } else { SourceSelector::WholeFile },
            range: range.map(|(s, e)| exposition_core::text::TextRange::new(s, e)),
        }),
    }
}

/// The walkthrough's graph: a theorem and its proof curated by an agent and
/// bound to `main.typ`, a finding that references the theorem, and a Lean
/// file with an indexed declaration. Returns (theorem, proof, finding).
fn odd_sums(p: &Project) -> (EntityId, EntityId, EntityId) {
    p.full();
    let start = ODD_TYP.find(ODD_STATEMENT).unwrap() as u32;
    let base = p.svc.status().revision;
    let (proposal, outcome) = p
        .svc
        .propose_and_apply(
            Origin::Agent { task_id: "task-1".into() },
            &ChangeRequest {
                base_revision: base,
                operations: vec![
                    bound(
                        "thm",
                        EntityKind::Theorem,
                        "Sum of odd numbers",
                        "main.typ",
                        Some((start, start + ODD_STATEMENT.len() as u32)),
                    ),
                    bound("prf", EntityKind::Proof, "Proof by induction", "main.typ", None),
                    create("fnd", EntityKind::Finding, "Numerical check up to 1000"),
                    GraphOperation::AddRelation {
                        kind: RelationKind::ProofOf,
                        from: EntityRef::Temp { temp_id: "prf".into() },
                        to: EntityRef::Temp { temp_id: "thm".into() },
                        evidence_kind: None,
                        evidence: vec![],
                        note: None,
                    },
                    GraphOperation::AddRelation {
                        kind: RelationKind::References,
                        from: EntityRef::Temp { temp_id: "fnd".into() },
                        to: EntityRef::Temp { temp_id: "thm".into() },
                        evidence_kind: None,
                        evidence: vec![],
                        note: None,
                    },
                ],
                summary: None,
            },
        )
        .unwrap();
    assert!(proposal.valid, "{:?}", proposal.diagnostics);
    let ApplyOutcome::Applied { created, .. } = outcome else { panic!("{outcome:?}") };
    (created[0].id.clone(), created[1].id.clone(), created[2].id.clone())
}

fn impact_of(p: &Project, id: &EntityId) -> ImpactReport {
    p.svc.impact(&ImpactRequest { entities: vec![id.clone()], max_depth: None, max_items: None }).unwrap()
}

/// Formal evidence the Lean service would record for `name` in `path` at the
/// file's indexed content.
fn formal_evidence(p: &Project, path: &str, name: &str, subject: &EntityId) -> EvidenceId {
    let file = p.resource(path).unwrap();
    let digest = p.svc.entity_detail(&file.id).unwrap().coverage.unwrap().content_hash.unwrap();
    let origin = Origin::Service { service: "lean".into() };
    let evidence = Evidence {
        id: EvidenceId::mint(),
        kind: EvidenceKind::FormalCheck,
        subject: Some(subject.clone()),
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
            declaration: name.into(),
            declaration_kind: "theorem".into(),
            statement: format!("{name} : True"),
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
    p.svc.register_evidence(&origin, evidence).unwrap()
}

/// Walkthrough lead B: "what is affected if main.typ changes?" answered
/// nothing, although a theorem and its proof are bound to that file (a
/// binding is not a relation). The file now lists what is located in it,
/// labelled as such; like declarations anchored in a file, those items are
/// informational and their own dependents are not followed.
#[test]
fn a_changed_file_lists_the_entities_bound_to_it() {
    let p = project(&[("main.typ", ODD_TYP), ("lean/OddSums.lean", ODD_LEAN)]);
    let (theorem, proof, finding) = odd_sums(&p);
    let file = p.resource("main.typ").unwrap();

    let report = impact_of(&p, &file.id);
    let item = |id: &EntityId| report.items.iter().find(|i| &i.entity.id == id);
    for id in [&theorem, &proof] {
        let item = item(id).unwrap_or_else(|| panic!("{id} is bound to main.typ: {:?}", report.items));
        assert_eq!(item.action, ImpactAction::Informational);
        assert_eq!(item.located_in.as_ref(), Some(&file.id));
        assert!(item.path.is_empty(), "a binding is not a relation: {:?}", item.path);
        assert_eq!(item.reason, "is located in “main.typ”, which changed; check whether it changed");
        assert_eq!(item.depth, 1);
    }
    assert!(item(&finding).is_none(), "dependents of an item in the file are not followed from the file");
    assert_eq!(report.items.len(), 2, "{:?}", report.items);

    // Naming the item follows its relations (unchanged).
    let report = impact_of(&p, &theorem);
    let actions: Vec<(EntityId, ImpactAction)> = report.items.iter().map(|i| (i.entity.id.clone(), i.action)).collect();
    assert!(actions.contains(&(proof.clone(), ImpactAction::NeedsReview)), "{actions:?}");
    assert!(actions.contains(&(finding.clone(), ImpactAction::CitationReview)), "{actions:?}");
    assert!(report.items.iter().all(|i| i.located_in.is_none()));
    assert!(report.items.iter().all(|i| i.reason.ends_with("(suggested relationship)")), "{:?}", report.items);

    // An indexed declaration is anchored in its file and bound to it: listed once.
    let lean = p.resource("lean/OddSums.lean").unwrap();
    let decl = by_key(&p, EntityKind::FormalDeclaration, "oddSum_eq_sq").expect("indexed Lean declaration");
    let report = impact_of(&p, &lean.id);
    let listed: Vec<&ImpactItem> = report.items.iter().filter(|i| i.entity.id == decl.id).collect();
    assert_eq!(listed.len(), 1, "{:?}", report.items);
    assert_eq!(listed[0].located_in.as_ref(), Some(&lean.id));
    assert!(listed[0].reason.starts_with("is declared in “OddSums.lean”"), "{}", listed[0].reason);

    // A rejected relation drops out.
    let relation = p.svc.entity_detail(&finding).unwrap().outgoing[0].id.clone();
    p.user(vec![GraphOperation::SetRelationStatus { relation, status: AssertionStatus::Rejected }]);
    let report = impact_of(&p, &theorem);
    assert!(report.items.iter().all(|i| i.entity.id != finding), "a rejected relation is not followed");
}

/// The integrity side of lead B: evidence that records a file is reported
/// when that file is asked about, and an actual save of the file leaves no
/// verification falsely current.
#[test]
fn a_saved_change_makes_the_evidence_of_a_file_historical_and_flags_changed_bindings() {
    let p = project(&[("main.typ", ODD_TYP), ("lean/OddSums.lean", ODD_LEAN)]);
    let (theorem, proof, _) = odd_sums(&p);
    let lean = p.resource("lean/OddSums.lean").unwrap();
    let decl = by_key(&p, EntityKind::FormalDeclaration, "oddSum_eq_sq").unwrap();
    let evidence = formal_evidence(&p, "lean/OddSums.lean", "oddSum_eq_sq", &decl.id);
    p.user(vec![add_rel(RelationKind::Formalizes, &decl.id, &theorem)]);

    // Before any change: the file's impact names the evidence that pins it.
    let report = impact_of(&p, &lean.id);
    let hit = report.evidence.iter().find(|e| e.evidence == evidence).expect("evidence of a declaration in the file");
    assert_eq!(hit.subject, decl.id);
    assert!(!matches!(hit.freshness, Freshness::Stale { .. }), "{:?}", hit.freshness);
    assert_eq!(
        hit.reason,
        "formalCheck evidence for “oddSum_eq_sq” records lean/OddSums.lean as an input; a change to the file makes this evidence historical"
    );
    let row = |p: &Project, id: &EntityId| {
        p.svc.math_overview().unwrap().rows.into_iter().find(|r| &r.entity.id == id).unwrap()
    };
    assert_eq!(row(&p, &theorem).verdict, Some(FormalVerdict::Checked), "the declaration's verdict, by association");
    assert!(matches!(row(&p, &theorem).freshness, Some(Freshness::Unknown { .. })), "never current");

    // The Lean file changes on disk and is reconciled (a save).
    write(&p.root, "lean/OddSums.lean", &ODD_LEAN.replace("sorry", "induction n <;> simp_all [oddSum]"));
    p.paths(&["lean/OddSums.lean"]);
    let detail = p.svc.entity_detail(&decl.id).unwrap();
    let fresh = &detail.evidence_freshness.iter().find(|(id, _)| id == &evidence).unwrap().1;
    assert!(matches!(fresh, Freshness::Stale { .. }), "{fresh:?}");
    for id in [&decl.id, &theorem] {
        assert!(matches!(row(&p, id).freshness, Some(Freshness::Stale { .. })), "{:?}", row(&p, id).freshness);
    }
    let report = impact_of(&p, &lean.id);
    let hit = report.evidence.iter().find(|e| e.evidence == evidence).unwrap();
    assert!(matches!(hit.freshness, Freshness::Stale { .. }));
    assert_eq!(hit.reason, "formalCheck evidence for “oddSum_eq_sq” was recorded for older inputs");

    // The manuscript statement changes: the theorem bound to that text no
    // longer resolves (it is not silently moved), the whole-file proof follows
    // the file, and the formal evidence is untouched (it never was about the
    // manuscript).
    write(&p.root, "main.typ", &ODD_TYP.replace("n squared", "n cubed"));
    p.paths(&["main.typ"]);
    let t = p.svc.entity_detail(&theorem).unwrap();
    assert_eq!(t.bindings[0].state, BindingState::Missing, "{:?}", t.bindings[0]);
    assert_eq!(t.bindings[0].detail.as_deref(), Some("The bound text was changed or removed."));
    assert_eq!(p.svc.entity_detail(&proof).unwrap().bindings[0].state, BindingState::Resolved);
    let report = impact_of(&p, &p.resource("main.typ").unwrap().id);
    assert!(report.unknown_coverage.contains(&wp("main.typ")), "an unresolved binding makes the file's impact unknown");
    assert!(report.items.iter().any(|i| i.entity.id == theorem), "still listed with the file it was bound to");
}

// ----- QC audit: relation semantics (RL-4, RL-5) and identity (GI-2) -----

const PAPER_UNCITED: &str = "\\documentclass{article}\n\\begin{document}\n\\section{Main}\n\\begin{lemma}\\label{lem:a}\nA.\n\\end{lemma}\n\\begin{proof}\nTrivial.\n\\end{proof}\n\\begin{theorem}\\label{thm:b}\nB.\n\\end{theorem}\n\\end{document}\n";
const PAPER_CITED: &str = "\\documentclass{article}\n\\begin{document}\n\\section{Main}\n\\begin{lemma}\\label{lem:a}\nA, see \\cite{knuth}.\n\\end{lemma}\n\\begin{proof}\nTrivial.\n\\end{proof}\n\\begin{theorem}\\label{thm:b}\nB, by \\cite{knuth}.\n\\end{theorem}\n\\end{document}\n";

/// RL-4: what the source states is observed, whatever was suggested or
/// rejected about the same relationship before.
#[test]
fn a_rejected_or_suggested_relation_does_not_hide_what_the_source_states() {
    let p = project(&[("paper.tex", PAPER_UNCITED), ("refs.bib", "@book{knuth, title = {The TeXbook}}\n")]);
    p.full();
    let lemma = by_key(&p, EntityKind::Lemma, "lem:a").unwrap();
    let theorem = by_key(&p, EntityKind::Theorem, "thm:b").unwrap();
    let knuth = by_key(&p, EntityKind::Reference, "knuth").unwrap();
    let agent = Origin::Agent { task_id: "task-4".into() };
    let (proposal, out) = p
        .svc
        .propose_and_apply(
            agent,
            &ChangeRequest {
                base_revision: p.svc.status().revision,
                operations: vec![
                    add_rel(RelationKind::References, &theorem.id, &knuth.id),
                    add_rel(RelationKind::References, &lemma.id, &knuth.id),
                ],
                summary: None,
            },
        )
        .unwrap();
    assert!(matches!(out, ApplyOutcome::Applied { .. }), "{:?}", proposal.diagnostics);
    let cites = |from: &EntityId| -> Vec<Relation> {
        p.svc
            .entity_detail(from)
            .unwrap()
            .outgoing
            .into_iter()
            .filter(|r| r.kind == RelationKind::References && r.to == knuth.id)
            .collect()
    };
    let rejected = cites(&theorem.id).remove(0);
    let suggested = cites(&lemma.id).remove(0);
    p.user(vec![GraphOperation::SetRelationStatus {
        relation: rejected.id.clone(),
        status: AssertionStatus::Rejected,
    }]);
    let rejected = cites(&theorem.id).remove(0);
    let flagged = |p: &Project| -> Vec<EntityId> {
        impact_of(p, &knuth.id)
            .items
            .into_iter()
            .filter(|i| i.action == ImpactAction::CitationReview)
            .map(|i| i.entity.id)
            .collect()
    };
    assert!(!flagged(&p).contains(&theorem.id), "a rejected suggestion is not followed");

    // The author now cites the book in both statements and saves.
    write(&p.root, "paper.tex", PAPER_CITED);
    p.paths(&["paper.tex"]);
    for (from, before) in [(&theorem.id, &rejected), (&lemma.id, &suggested)] {
        let now = cites(from);
        assert_eq!(now.len(), 1, "one relationship per pair: {now:?}");
        assert_eq!(now[0].status, AssertionStatus::Observed, "{:?}", now[0]);
        assert!(matches!(now[0].origin, Origin::Extracted { .. }), "{:?}", now[0].origin);
        assert_eq!(now[0].id, before.id, "the same record");
        assert!(now[0].revision > before.revision, "{} → {}", before.revision, now[0].revision);
    }
    let flagged_now = flagged(&p);
    assert!(flagged_now.contains(&theorem.id) && flagged_now.contains(&lemma.id), "{flagged_now:?}");
    // Through persistence, and stable under a forced reindex.
    p.svc.detach();
    assert!(p.svc.attach(&p.root).is_usable());
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    assert_eq!(cites(&theorem.id)[0].status, AssertionStatus::Observed);
    // An observed relation follows the source: removed from it, it is gone.
    write(&p.root, "paper.tex", PAPER_UNCITED);
    p.paths(&["paper.tex"]);
    assert!(cites(&theorem.id).is_empty() && cites(&lemma.id).is_empty());
}

/// RL-4: a relationship a person accepted is their record; the source
/// stating the same thing does not take it over.
#[test]
fn an_accepted_relation_is_kept_when_the_source_states_the_same() {
    let p = project(&[("paper.tex", PAPER_UNCITED), ("refs.bib", "@book{knuth, title = {The TeXbook}}\n")]);
    p.full();
    let theorem = by_key(&p, EntityKind::Theorem, "thm:b").unwrap();
    let knuth = by_key(&p, EntityKind::Reference, "knuth").unwrap();
    p.user(vec![add_rel(RelationKind::References, &theorem.id, &knuth.id)]);
    let before = p.svc.entity_detail(&theorem.id).unwrap().outgoing.into_iter().find(|r| r.to == knuth.id).unwrap();
    write(&p.root, "paper.tex", PAPER_CITED);
    p.paths(&["paper.tex"]);
    let after: Vec<Relation> =
        p.svc.entity_detail(&theorem.id).unwrap().outgoing.into_iter().filter(|r| r.to == knuth.id).collect();
    assert_eq!(after, vec![before]);
}

/// RL-5: an extracted declaration's label and attributes come from its
/// source and are not edited as metadata; archiving it is the person's
/// choice and survives indexing of its file.
#[test]
fn an_extracted_declaration_keeps_a_persons_archive_and_refuses_metadata_edits() {
    let p = project(&[("paper.tex", PAPER)]);
    p.full();
    let theorem = by_key(&p, EntityKind::Theorem, "thm:b").unwrap();
    let edit = |label: Option<&str>, attributes: Option<BTreeMap<String, String>>| {
        let (proposal, _) = p
            .svc
            .propose_and_apply(
                Origin::Agent { task_id: "task-5".into() },
                &ChangeRequest {
                    base_revision: p.svc.status().revision,
                    operations: vec![GraphOperation::UpdateEntity {
                        entity: theorem.id.clone(),
                        expected_revision: theorem.revision,
                        label: label.map(str::to_owned),
                        attributes,
                    }],
                    summary: None,
                },
            )
            .unwrap();
        proposal.diagnostics.first().map(|d| d.code.clone())
    };
    assert_eq!(edit(Some("Main theorem (verified)"), None).as_deref(), Some("graph.sourceDerived"));
    assert_eq!(
        edit(None, Some(BTreeMap::from([("source.title".to_owned(), "Verified".to_owned())]))).as_deref(),
        Some("graph.sourceDerived")
    );
    assert_eq!(p.entity(&theorem.id), theorem, "nothing changed");
    // A file's attributes are not rewritten by indexing: they stay editable and survive it.
    let file = p.resource("paper.tex").unwrap();
    p.user(vec![GraphOperation::UpdateEntity {
        entity: file.id.clone(),
        expected_revision: file.revision,
        label: None,
        attributes: Some(BTreeMap::from([("note".to_owned(), "camera-ready".to_owned())])),
    }]);

    p.user(vec![GraphOperation::SetLifecycle {
        entity: theorem.id.clone(),
        expected_revision: theorem.revision,
        lifecycle: Lifecycle::Archived,
    }]);
    // The file is edited (inside the theorem, and elsewhere) and saved.
    write(&p.root, "paper.tex", &PAPER.replace("By Lemma", "Now by Lemma").replace("Trivial.", "Obvious."));
    p.paths(&["paper.tex"]);
    let after = p.entity(&theorem.id);
    assert_eq!(after.lifecycle, Lifecycle::Archived, "indexing does not undo a person's archive");
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    assert_eq!(p.entity(&theorem.id).lifecycle, Lifecycle::Archived);
    assert_eq!(p.entity(&file.id).attributes.get("note").map(String::as_str), Some("camera-ready"));
    // Removed from the source it reads "Removed from source".
    write(
        &p.root,
        "paper.tex",
        PAPER_UNCITED.replace("\\begin{theorem}\\label{thm:b}\nB.\n\\end{theorem}\n", "").as_str(),
    );
    p.paths(&["paper.tex"]);
    assert_eq!(p.entity(&theorem.id).lifecycle, Lifecycle::Tombstoned);
}

/// GI-2: a file that returns unchanged is the same resource, whoever asks
/// for its identity first (a service admitting it, or the reconciler).
#[test]
fn a_service_admitting_a_returned_file_revives_its_identity() {
    for with_hash in [true, false] {
        let p = project(&[("notes/fit.md", "# Fit\n"), ("other.md", "# Other\n")]);
        p.full();
        let first = p.resource("notes/fit.md").unwrap();
        let claim = match p.user(vec![create("c", EntityKind::Claim, "Claim")]) {
            ApplyOutcome::Applied { created, .. } => created[0].id.clone(),
            other => panic!("{other:?}"),
        };
        p.user(vec![add_rel(RelationKind::References, &claim, &first.id)]);
        let hash = p.svc.entity_detail(&first.id).unwrap().bindings[0].version.clone().unwrap().content_hash;

        std::fs::remove_file(p.root.join("notes/fit.md")).unwrap();
        p.paths(&["notes/fit.md"]);
        assert_eq!(p.entity(&first.id).lifecycle, Lifecycle::Tombstoned);
        assert!(p.resource("notes/fit.md").is_none());

        // The identical file is put back; a service asks before the reconciler runs.
        write(&p.root, "notes/fit.md", "# Fit\n");
        let admitted =
            p.svc.ensure_resource(&wp("notes/fit.md"), with_hash.then_some(hash.as_str()), false, "research").unwrap();
        assert_eq!(admitted.id, first.id, "with_hash {with_hash}: the same identity");
        assert_eq!(admitted.lifecycle, Lifecycle::Active);
        assert_eq!(admitted.tombstone_reason, None);
        p.paths(&["notes/fit.md"]);
        p.full();
        let live: Vec<Entity> = p
            .all(EntityKind::Resource)
            .into_iter()
            .filter(|e| e.key.as_deref() == Some("notes/fit.md") && e.lifecycle != Lifecycle::Tombstoned)
            .collect();
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(live[0].id, first.id);
        let detail = p.svc.entity_detail(&first.id).unwrap();
        assert!(detail.incoming.iter().any(|r| r.kind == RelationKind::References && r.from == claim));
        assert_eq!(detail.bindings[0].state, BindingState::Resolved);
    }
}

/// GI-2: different content at the path is a different file (the reconciler's rule).
#[test]
fn a_service_admitting_a_different_file_at_a_removed_path_gets_a_new_identity() {
    let p = project(&[("notes/fit.md", "# Fit\n")]);
    p.full();
    let first = p.resource("notes/fit.md").unwrap();
    std::fs::remove_file(p.root.join("notes/fit.md")).unwrap();
    p.paths(&["notes/fit.md"]);
    write(&p.root, "notes/fit.md", "# Something else\n");
    let admitted = p.svc.ensure_resource(&wp("notes/fit.md"), None, false, "research").unwrap();
    assert_ne!(admitted.id, first.id);
    assert_eq!(p.entity(&first.id).lifecycle, Lifecycle::Tombstoned);
}
