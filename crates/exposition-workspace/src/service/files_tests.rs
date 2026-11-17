use std::sync::Arc;

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::text::TextRange;

use super::*;
use crate::documents::DocumentService;
use crate::recovery::{RecoveryStore, workspace_key};

struct Fixture {
    dir: tempfile::TempDir,
    service: Arc<WorkspaceService>,
}

impl Fixture {
    fn file(&self, path: &str) -> PathBuf {
        self.dir.path().join("project").join(path)
    }

    fn names(&self, folder: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.file(folder))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("chapters")).unwrap();
    fs::create_dir_all(project.join(".exposition/runs")).unwrap();
    fs::write(project.join("main.typ"), b"= Title\n").unwrap();
    fs::write(project.join("notes.txt"), b"plain\n").unwrap();
    fs::write(project.join("chapters/one.tex"), b"\\section{One}\n").unwrap();
    fs::write(project.join("chapters/two.tex"), b"\\section{Two}\n").unwrap();
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    service.open_workspace(&project).unwrap();
    Fixture { dir, service }
}

fn type_text(service: &WorkspaceService, id: &DocumentId, at: u32, text: &str) {
    let base = service.documents().info(id).unwrap().revision;
    service
        .documents()
        .apply(TransactionRequest {
            id: TransactionId(format!("t-{base}")),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: id.clone(),
                base_revision: base,
                patches: vec![TextPatch { range: TextRange::new(at, at), text: text.into() }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
}

#[test]
fn new_files_take_exactly_the_typed_name_and_never_replace_anything() {
    let f = fixture();
    for name in [
        "Untitled.txt",
        "paper.tex",
        "doc.typ",
        "README.md",
        "fit.py",
        "Basic.lean",
        ".gitignore",
        "Makefile",
        "with space ü.md",
    ] {
        let path = f.service.create_file(None, name).unwrap();
        assert_eq!(path.as_str(), name);
        assert_eq!(fs::read(f.file(name)).unwrap(), b"", "{name} is created empty");
    }
    // In a folder, with the folder's real name.
    assert_eq!(f.service.create_file(Some(&wp("chapters")), "three.tex").unwrap().as_str(), "chapters/three.tex");
    // Collisions (also by case on Windows) keep the existing file.
    let err = f.service.create_file(None, "notes.txt").unwrap_err();
    assert_eq!(err.code, "file.exists");
    assert_eq!(fs::read(f.file("notes.txt")).unwrap(), b"plain\n");
    if cfg!(windows) {
        assert_eq!(f.service.create_file(None, "NOTES.TXT").unwrap_err().code, "file.exists");
        assert_eq!(fs::read(f.file("notes.txt")).unwrap(), b"plain\n");
    }
    // A folder of that name, invalid names, protected folders, missing folders.
    assert_eq!(f.service.create_file(None, "chapters").unwrap_err().code, "file.exists");
    assert_eq!(f.service.create_file(None, "con.txt").unwrap_err().code, "name.reserved");
    assert_eq!(f.service.create_file(None, "a/b.txt").unwrap_err().code, "name.separator");
    assert_eq!(f.service.create_file(Some(&wp(".exposition/runs")), "x.json").unwrap_err().code, "file.protected");
    assert_eq!(f.service.create_file(Some(&wp("missing")), "x.txt").unwrap_err().code, "path.notFound");
    assert_eq!(f.service.create_file(Some(&wp("notes.txt")), "x.txt").unwrap_err().code, "path.notDirectory");
}

#[test]
fn renaming_an_open_dirty_file_keeps_the_document_its_text_history_and_recovery() {
    let f = fixture();
    let opened = f.service.open_document(&wp("notes.txt")).unwrap();
    let id = opened.state.info.id.clone();
    assert_eq!(opened.state.info.language, "plaintext");
    type_text(&f.service, &id, 0, "unsaved ");
    let before = f.service.documents().info(&id).unwrap();
    f.service.journal_documents(&HashSet::from([id.clone()]));

    let renamed = f.service.rename_entry(&wp("notes.txt"), "notes.md").unwrap();
    assert_eq!(renamed.to.as_str(), "notes.md");
    assert!(!renamed.is_dir);
    assert_eq!(renamed.documents.len(), 1);
    let after = f.service.documents().info(&id).unwrap();
    assert_eq!(after.id, before.id, "same document");
    assert_eq!(after.path.as_ref().map(|p| p.as_str()), Some("notes.md"));
    assert_eq!(after.language, "markdown", "language follows the new extension");
    assert_eq!((after.revision, after.saved_revision), (before.revision, before.saved_revision), "still unsaved");
    assert_eq!(f.service.documents().state(&id).unwrap().text, "unsaved plain\n");
    // The saved bytes moved; nothing was written.
    assert!(!f.file("notes.txt").exists());
    assert_eq!(fs::read(f.file("notes.md")).unwrap(), b"plain\n");
    // Undo still works on the same history.
    f.service.documents().undo(&id, EditOrigin::Source).unwrap();
    assert_eq!(f.service.documents().state(&id).unwrap().text, "plain\n");
    type_text(&f.service, &id, 0, "again ");
    // The recovery journal is under the new path only.
    let store = RecoveryStore::new(&f.dir.path().join("data"), &workspace_key(&f.service.current().unwrap().root));
    assert!(store.dir().exists());
    f.service.journal_documents(&HashSet::from([id.clone()]));
    let restarted = WorkspaceService::new(Arc::new(DocumentService::new()), f.dir.path().join("data"));
    restarted.open_workspace(&f.dir.path().join("project")).unwrap();
    let entries = restarted.recovery_entries().unwrap();
    let paths: Vec<_> = entries.iter().filter_map(|e| e.path.as_ref().map(|p| p.to_string())).collect();
    assert_eq!(paths, ["notes.md"], "{entries:?}");
    // Saving writes to the new path.
    f.service.save_document(&id, false).unwrap();
    assert_eq!(fs::read(f.file("notes.md")).unwrap(), b"again plain\n");
    assert!(!f.file("notes.txt").exists(), "never resurrected");
}

#[test]
fn renaming_a_folder_moves_the_documents_beneath_it() {
    let f = fixture();
    let one = f.service.open_document(&wp("chapters/one.tex")).unwrap().state.info.id;
    type_text(&f.service, &one, 0, "% edited\n");
    let renamed = f.service.rename_entry(&wp("chapters"), "parts").unwrap();
    assert!(renamed.is_dir);
    assert_eq!(f.service.documents().info(&one).unwrap().path.unwrap().as_str(), "parts/one.tex");
    assert_eq!(f.names("parts"), ["one.tex", "two.tex"]);
    f.service.save_document(&one, false).unwrap();
    assert_eq!(fs::read(f.file("parts/one.tex")).unwrap(), b"% edited\n\\section{One}\n");
}

#[test]
fn collisions_and_refusals_leave_the_file_and_its_buffer_unchanged() {
    let f = fixture();
    let id = f.service.open_document(&wp("notes.txt")).unwrap().state.info.id;
    type_text(&f.service, &id, 0, "mine ");
    for (name, code) in [
        ("main.typ", "file.exists"),
        ("chapters", "file.exists"),
        ("notes.txt", "name.unchanged"),
        ("bad?.txt", "name.invalid"),
        ("aux.md", "name.reserved"),
        ("x/y.txt", "name.separator"),
        ("end.", "name.trailing"),
    ] {
        assert_eq!(f.service.rename_entry(&wp("notes.txt"), name).unwrap_err().code, code, "{name}");
    }
    if cfg!(windows) {
        assert_eq!(f.service.rename_entry(&wp("notes.txt"), "MAIN.TYP").unwrap_err().code, "file.exists");
    }
    assert_eq!(fs::read(f.file("notes.txt")).unwrap(), b"plain\n");
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"= Title\n");
    let info = f.service.documents().info(&id).unwrap();
    assert_eq!(info.path.unwrap().as_str(), "notes.txt");
    assert_eq!(f.service.documents().state(&id).unwrap().text, "mine plain\n");
    // Application data is never renamed; missing entries are reported.
    assert_eq!(f.service.rename_entry(&wp(".exposition/runs"), "r").unwrap_err().code, "file.protected");
    assert_eq!(f.service.rename_entry(&wp("gone.txt"), "x.txt").unwrap_err().code, "path.notFound");
    // A target that appears between planning and applying is never replaced.
    let plan = f.service.plan_rename(&wp("notes.txt"), "late.txt").unwrap();
    fs::write(f.file("late.txt"), b"someone else's").unwrap();
    assert_eq!(f.service.apply_rename(&plan).unwrap_err().code, "file.exists");
    assert_eq!(fs::read(f.file("late.txt")).unwrap(), b"someone else's");
    assert_eq!(fs::read(f.file("notes.txt")).unwrap(), b"plain\n");
}

#[test]
fn a_case_only_rename_changes_the_name_on_disk() {
    let f = fixture();
    let id = f.service.open_document(&wp("notes.txt")).unwrap().state.info.id;
    let renamed = f.service.rename_entry(&wp("notes.txt"), "Notes.txt").unwrap();
    assert_eq!(renamed.to.as_str(), "Notes.txt");
    assert!(f.names("").contains(&"Notes.txt".to_owned()), "{:?}", f.names(""));
    assert!(!f.names("").contains(&"notes.txt".to_owned()));
    assert_eq!(f.service.documents().info(&id).unwrap().path.unwrap().as_str(), "Notes.txt");
    assert_eq!(fs::read(f.file("Notes.txt")).unwrap(), b"plain\n");
}
