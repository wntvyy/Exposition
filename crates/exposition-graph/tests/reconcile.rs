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
