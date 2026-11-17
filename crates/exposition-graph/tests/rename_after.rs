//! A rename recorded with `reconcile_after` (QOL E): the move and the pass
//! over both paths are one serialized step; the resource keeps its identity
//! and curated relations; later passes over the same paths (the watcher's
//! events of the move) change nothing; a case-only rename on a
//! case-insensitive file system does not create a second resource.

use std::path::PathBuf;
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
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

fn project(files: &[(&str, &str)]) -> Project {
    let data = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (rel, body) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let svc = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    assert!(svc.attach(&root).is_usable());
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    Project { _data: data, _dir: dir, root, svc }
}

impl Project {
    fn rename(&self, from: &str, to: &str) -> u32 {
        let paths = vec![wp(from), wp(to)];
        let (moved, report) = self.svc.reconcile_after(|| {
            let moved = std::fs::rename(self.root.join(from), self.root.join(to));
            (moved, Some(ReconcileOptions { scope: Scope::Paths(paths.clone()), force: false }))
        });
        moved.unwrap();
        report.unwrap().unwrap().resources_renamed
    }

    fn pass(&self, paths: &[&str]) {
        self.svc
            .reconcile(&ReconcileOptions { scope: Scope::Paths(paths.iter().map(|p| wp(p)).collect()), force: false })
            .unwrap();
    }

    fn resources(&self) -> Vec<Entity> {
        self.svc
            .snapshot(&GraphQuery {
                kinds: Some(vec![EntityKind::Resource]),
                include_inactive: true,
                limit: Some(1000),
                ..Default::default()
            })
            .unwrap()
            .entities
    }

    fn live(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .resources()
            .into_iter()
            .filter(|e| e.lifecycle != Lifecycle::Tombstoned)
            .filter_map(|e| e.key)
            .collect();
        keys.sort();
        keys
    }
}

#[test]
fn a_renamed_file_keeps_its_resource_and_curated_relations_and_watcher_passes_add_nothing() {
    let p = project(&[("notes.txt", "plain notes\n"), ("data.csv", "x\n1\n")]);
    let notes = p.svc.resource(&wp("notes.txt")).unwrap().unwrap();
    let data = p.svc.resource(&wp("data.csv")).unwrap().unwrap();
    let base = p.svc.status().revision;
    let (proposal, _) = p
        .svc
        .propose_and_apply(
            Origin::User,
            &ChangeRequest {
                base_revision: base,
                operations: vec![GraphOperation::AddRelation {
                    kind: RelationKind::References,
                    from: EntityRef::Id { id: notes.id.clone() },
                    to: EntityRef::Id { id: data.id.clone() },
                    evidence_kind: None,
                    evidence: vec![],
                    note: None,
                }],
                summary: None,
            },
        )
        .unwrap();
    assert!(proposal.valid, "{:?}", proposal.diagnostics);
    let before = p.resources().len();

    // Extension change included: the same physical file.
    assert_eq!(p.rename("notes.txt", "notes.md"), 1);
    let renamed = p.svc.resource(&wp("notes.md")).unwrap().unwrap();
    assert_eq!(renamed.id, notes.id, "same identity");
    assert_eq!(renamed.label, "notes.md");
    assert!(p.svc.resource(&wp("notes.txt")).unwrap().is_none());
    let detail = p.svc.entity_detail(&notes.id).unwrap();
    assert!(detail.outgoing.iter().any(|r| r.kind == RelationKind::References && r.to == data.id));

    // The watcher reports the old path removed and the new one created (in
    // one batch or two): nothing new is recorded.
    p.pass(&["notes.txt", "notes.md"]);
    p.pass(&["notes.txt"]);
    p.pass(&["notes.md"]);
    p.svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
    assert_eq!(p.resources().len(), before, "no second resource, no tombstone");
    assert_eq!(p.live(), ["data.csv", "notes.md"]);
}

#[test]
fn a_case_only_rename_keeps_one_resource() {
    let p = project(&[("notes.txt", "plain notes\n")]);
    let id = p.svc.resource(&wp("notes.txt")).unwrap().unwrap().id;
    // Through a temporary name, as the workspace service does when a direct
    // case-only move is refused.
    std::fs::rename(p.root.join("notes.txt"), p.root.join("tmp-name")).unwrap();
    std::fs::rename(p.root.join("tmp-name"), p.root.join("Notes.txt")).unwrap();
    let (_, report) = p.svc.reconcile_after(|| {
        ((), Some(ReconcileOptions { scope: Scope::Paths(vec![wp("notes.txt"), wp("Notes.txt")]), force: false }))
    });
    assert_eq!(report.unwrap().unwrap().resources_renamed, 1);
    assert_eq!(p.svc.resource(&wp("Notes.txt")).unwrap().unwrap().id, id);
    // A later pass over the old spelling never finds a second file there.
    p.pass(&["notes.txt", "Notes.txt"]);
    p.pass(&["notes.txt"]);
    assert_eq!(p.live(), ["Notes.txt"]);
    assert_eq!(p.resources().len(), 1, "{:?}", p.resources().iter().map(|e| (&e.key, e.lifecycle)).collect::<Vec<_>>());
}
