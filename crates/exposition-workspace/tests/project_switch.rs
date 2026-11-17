//! Project switch (QOL B): opening or creating another project while one is
//! open. The destination is checked before anything of the open project is
//! closed, and the documents the user chose to discard lose their unsaved
//! changes (and recovery journals) only once the switch is committed.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_workspace::{DocumentService, WorkspaceService};

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

struct Fixture {
    dir: tempfile::TempDir,
    service: Arc<WorkspaceService>,
}

impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn data(&self) -> PathBuf {
        self.path("data")
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    for project in ["a", "b"] {
        let root = dir.path().join(project);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("main.typ"), format!("= {project}\n")).unwrap();
        fs::write(root.join("notes.md"), b"notes\n").unwrap();
    }
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    service.open_workspace(&dir.path().join("a")).unwrap();
    Fixture { dir, service }
}

fn edit(service: &WorkspaceService, id: &DocumentId, insert: &str) {
    let docs = service.documents();
    let base = docs.info(id).unwrap().revision;
    docs.apply(TransactionRequest {
        id: TransactionId(format!("switch-{base}")),
        origin: EditOrigin::Source,
        edits: vec![DocumentEdit {
            document: id.clone(),
            base_revision: base,
            patches: vec![TextPatch::insert(0, insert)],
        }],
        undo: UndoBehavior::NewGroup,
        conflict: ConflictPolicy::Reject,
        label: None,
        selection: None,
    })
    .unwrap();
}

fn dirty_open(f: &Fixture, path: &str, insert: &str) -> DocumentId {
    let id = f.service.open_document(&wp(path)).unwrap().state.info.id;
    edit(&f.service, &id, insert);
    f.service.journal_documents(&[id.clone()].into_iter().collect());
    id
}

fn is_dirty(service: &WorkspaceService, id: &DocumentId) -> bool {
    let info = service.documents().info(id).unwrap();
    info.saved_revision != Some(info.revision)
}

#[test]
fn check_workspace_opens_nothing() {
    let f = fixture();
    let display = f.service.check_workspace(&f.path("b")).unwrap();
    assert!(display.ends_with("/b") || display.ends_with("\\b"), "{display}");
    assert!(f.service.current().unwrap().root.ends_with('a'));
    assert_eq!(f.service.check_workspace(&f.path("missing")).unwrap_err().code, "workspace.notFound");
    assert_eq!(f.service.check_workspace(&f.data()).unwrap_err().code, "workspace.tooBroad");
}

/// A destination that cannot be opened changes nothing: the open project,
/// the buffers the user chose to discard, and their journals stay.
#[test]
fn refused_destination_keeps_the_project_buffers_and_journals() {
    let f = fixture();
    let main = dirty_open(&f, "main.typ", "UNSAVED ");
    let before = f.service.current().unwrap().root;
    for target in [f.path("missing"), f.data(), f.path("a").join("main.typ")] {
        assert!(f.service.switch_workspace(&target, std::slice::from_ref(&main)).is_err(), "{target:?}");
        assert_eq!(f.service.current().unwrap().root, before);
        assert!(is_dirty(&f.service, &main), "the buffer is still open and unsaved");
        assert!(f.service.documents().state(&main).unwrap().text.starts_with("UNSAVED "));
    }
    // Its journal is intact: a restart (crash) still recovers it.
    let restarted = WorkspaceService::new(Arc::new(DocumentService::new()), f.data());
    restarted.open_workspace(&f.path("a")).unwrap();
    let entries = restarted.recovery_entries().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
}

/// A committed switch discards exactly the chosen documents; another unsaved
/// buffer keeps its journal, and reopening the original project offers it.
#[test]
fn switch_discards_only_the_chosen_documents() {
    let f = fixture();
    let main = dirty_open(&f, "main.typ", "DISCARD ");
    let notes = dirty_open(&f, "notes.md", "KEEP ");
    let info = f.service.switch_workspace(&f.path("b"), std::slice::from_ref(&main)).unwrap();
    assert!(info.root.ends_with('b'));
    assert!(f.service.documents().info(&main).is_err(), "closed");
    assert!(f.service.documents().info(&notes).is_err(), "closed with the folder");
    assert!(info.recovery.is_empty(), "nothing of a/ is offered in b/: {:?}", info.recovery);
    // Files on disk are untouched.
    assert_eq!(fs::read_to_string(f.path("a").join("main.typ")).unwrap(), "= a\n");

    let back = f.service.open_workspace(&f.path("a")).unwrap();
    let paths: Vec<_> = back.recovery.iter().filter_map(|e| e.path.as_ref().map(|p| p.as_str().to_owned())).collect();
    assert_eq!(paths, vec!["notes.md".to_owned()], "{:?}", back.recovery);
    let restored = f.service.restore_recovery(&back.recovery[0].id).unwrap();
    assert!(restored.state.text.starts_with("KEEP "));
}

/// Opening the folder that is already open discards nothing.
#[test]
fn reopening_the_open_folder_discards_nothing() {
    let f = fixture();
    let main = dirty_open(&f, "main.typ", "UNSAVED ");
    f.service.switch_workspace(&f.path("a"), std::slice::from_ref(&main)).unwrap();
    assert!(is_dirty(&f.service, &main));
}

/// Saved work is what the reopened project shows (no recovery entry).
#[test]
fn saved_work_reopens_without_recovery() {
    let f = fixture();
    let main = dirty_open(&f, "main.typ", "SAVED ");
    f.service.save_document(&main, false).unwrap();
    f.service.switch_workspace(&f.path("b"), &[]).unwrap();
    let back = f.service.open_workspace(&f.path("a")).unwrap();
    assert!(back.recovery.is_empty(), "{:?}", back.recovery);
    assert_eq!(fs::read_to_string(f.path("a").join("main.typ")).unwrap(), "SAVED = a\n");
}
