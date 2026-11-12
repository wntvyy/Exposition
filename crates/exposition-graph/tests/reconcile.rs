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
    let p = project(&[("draft.typ", "= Draft\nunique content 42\n")]);
    p.full();
    let id = p.resource("draft.typ").unwrap().id;
    // Copy + delete (a new physical file, e.g. a branch switch rewriting files).
    write(&p.root, "paper/draft.typ", "= Draft\nunique content 42\n");
    std::fs::remove_file(p.root.join("draft.typ")).unwrap();
    let r = p.full();
    assert_eq!(r.resources_renamed, 1, "{r:?}");
    assert_eq!(p.resource("paper/draft.typ").unwrap().id, id);
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
