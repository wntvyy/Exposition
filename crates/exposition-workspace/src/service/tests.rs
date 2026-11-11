use std::path::Path;

use exposition_core::document::{ChangeKind, TextEncoding};

use super::*;

struct Fixture {
    dir: tempfile::TempDir,
    service: Arc<WorkspaceService>,
}

impl Fixture {
    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    fn file(&self, path: &str) -> PathBuf {
        self.project().join(path)
    }

    /// A fresh service over the same data directory (simulates an app restart).
    fn restart(&self) -> Arc<WorkspaceService> {
        let service = WorkspaceService::new(Arc::new(DocumentService::new()), self.data());
        service.open_workspace(&self.project()).unwrap();
        service
    }
}

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("chapters")).unwrap();
    fs::write(project.join("main.typ"), b"= Title\n\nBody.\n").unwrap();
    fs::write(project.join("crlf.tex"), b"\\section{A}\r\nText.\r\n").unwrap();
    fs::write(project.join("bom.md"), b"\xEF\xBB\xBF# Heading\n").unwrap();
    let utf16: Vec<u8> =
        [0xFF, 0xFE].into_iter().chain("x = 1\r\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    fs::write(project.join("utf16.py"), utf16).unwrap();
    fs::write(project.join("mixed.txt"), b"a\r\nb\nc\r\n").unwrap();
    fs::write(project.join("image.png"), b"\x89PNG\r\n\x1a\n\0\0\0").unwrap();
    fs::write(project.join("latin1.tex"), b"caf\xe9\n").unwrap();
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    service.open_workspace(&project).unwrap();
    Fixture { dir, service }
}

fn edit(service: &WorkspaceService, id: &DocumentId, patches: Vec<TextPatch>) -> u64 {
    let base = service.documents().info(id).unwrap().revision;
    let outcome = service
        .documents()
        .apply(TransactionRequest {
            id: TransactionId(format!("test-{base}")),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit { document: id.clone(), base_revision: base, patches }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    outcome.applied[0].revision
}

fn text(service: &WorkspaceService, id: &DocumentId) -> String {
    service.documents().state(id).unwrap().text
}

fn dirty(service: &WorkspaceService, id: &DocumentId) -> bool {
    let info = service.documents().info(id).unwrap();
    info.saved_revision != Some(info.revision)
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

fn set_read_only(path: &Path, read_only: bool) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(read_only);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn opens_files_with_detected_encoding_language_and_line_endings() {
    let f = fixture();
    let crlf = f.service.open_document(&wp("crlf.tex")).unwrap().state;
    assert_eq!(crlf.text, "\\section{A}\r\nText.\r\n");
    assert_eq!(crlf.info.language, "latex");
    assert_eq!(crlf.info.line_ending, LineEnding::Crlf);
    let bom = f.service.open_document(&wp("bom.md")).unwrap().state;
    assert_eq!((bom.text.as_str(), bom.info.encoding), ("# Heading\n", TextEncoding::Utf8Bom));
    let utf16 = f.service.open_document(&wp("utf16.py")).unwrap().state;
    assert_eq!((utf16.text.as_str(), utf16.info.encoding), ("x = 1\r\n", TextEncoding::Utf16Le));
    assert_eq!(f.service.open_document(&wp("mixed.txt")).unwrap().state.info.line_ending, LineEnding::Mixed);
    // Opening again returns the same document.
    let again = f.service.open_document(&wp("crlf.tex")).unwrap().state;
    assert_eq!(again.info.id, crlf.info.id);
}

#[test]
fn refuses_binary_undecodable_large_and_folder_paths() {
    let f = fixture();
    assert_eq!(f.service.open_document(&wp("image.png")).unwrap_err().code, "file.binary");
    assert_eq!(f.service.open_document(&wp("latin1.tex")).unwrap_err().code, "file.undecodable");
    assert_eq!(fs::read(f.file("latin1.tex")).unwrap(), b"caf\xe9\n", "bytes untouched");
    fs::write(f.file("big.txt"), vec![b'a'; MAX_TEXT_BYTES as usize + 1]).unwrap();
    assert_eq!(f.service.open_document(&wp("big.txt")).unwrap_err().code, "file.tooLarge");
    assert_eq!(f.service.open_document(&wp("chapters")).unwrap_err().code, "file.isDirectory");
    assert_eq!(f.service.open_document(&wp("missing.typ")).unwrap_err().code, "path.notFound");
    assert!(f.service.documents().list().is_empty());
}

#[test]
fn rejects_paths_escaping_through_junctions() {
    let f = fixture();
    let outside = f.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "secret").unwrap();
    junction(&f.file("escape"), &outside);
    assert_eq!(f.service.open_document(&wp("escape/secret.txt")).unwrap_err().code, "path.outsideWorkspace");
    assert_eq!(f.service.list_dir(Some(&wp("escape"))).unwrap_err().code, "path.outsideWorkspace");
    assert!(f.service.documents().list().is_empty());
}

#[test]
fn saves_exact_bytes_preserving_crlf_bom_and_utf16() {
    let f = fixture();
    let crlf = f.service.open_document(&wp("crlf.tex")).unwrap().state.info.id;
    edit(&f.service, &crlf, vec![TextPatch::insert(20, "More.\r\n")]);
    let bom = f.service.open_document(&wp("bom.md")).unwrap().state.info.id;
    edit(&f.service, &bom, vec![TextPatch::insert(10, "é\n")]);
    let utf16 = f.service.open_document(&wp("utf16.py")).unwrap().state.info.id;
    edit(&f.service, &utf16, vec![TextPatch::replace(4, 5, "😀")]);

    let outcome = f.service.save_all();
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(outcome.saved.len(), 3);
    assert_eq!(fs::read(f.file("crlf.tex")).unwrap(), b"\\section{A}\r\nText.\r\nMore.\r\n");
    assert_eq!(fs::read(f.file("bom.md")).unwrap(), "\u{feff}# Heading\n\u{e9}\n".as_bytes());
    let expected: Vec<u8> =
        [0xFF, 0xFE].into_iter().chain("x = 😀\r\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    assert_eq!(fs::read(f.file("utf16.py")).unwrap(), expected);
    assert!(!dirty(&f.service, &crlf));

    // Reopen in a fresh session: the saved text comes back.
    let service = f.restart();
    let reopened = service.open_document(&wp("crlf.tex")).unwrap().state;
    assert_eq!(reopened.text, "\\section{A}\r\nText.\r\nMore.\r\n");
    let leftovers: Vec<String> = fs::read_dir(f.project())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn save_refuses_external_modifications_unless_overwriting() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "mine ")]);
    fs::write(f.file("main.typ"), b"theirs\n").unwrap();
    let err = f.service.save_document(&id, false).unwrap_err();
    assert_eq!(err.code, "file.modifiedOnDisk");
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"theirs\n");
    assert!(dirty(&f.service, &id));
    f.service.save_document(&id, true).unwrap();
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"mine = Title\n\nBody.\n");
    assert!(!dirty(&f.service, &id));
}

#[test]
fn failed_save_keeps_original_bytes_and_dirty_buffer() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "unsaved ")]);
    set_read_only(&f.file("main.typ"), true);
    let err = f.service.save_document(&id, false).unwrap_err();
    let forced = f.service.save_document(&id, true).unwrap_err();
    set_read_only(&f.file("main.typ"), false);
    assert_eq!(err.code, "file.readOnly");
    assert_eq!(forced.code, "file.readOnly");
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"= Title\n\nBody.\n");
    assert!(dirty(&f.service, &id));
    assert_eq!(text(&f.service, &id), "unsaved = Title\n\nBody.\n");
    // After the problem is fixed, the same buffer saves.
    f.service.save_document(&id, false).unwrap();
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"unsaved = Title\n\nBody.\n");
}

#[test]
fn clean_buffers_reload_external_changes_as_undoable_external_transactions() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    let mut docs = f.service.documents().subscribe();
    let mut events = f.service.subscribe();
    fs::write(f.file("main.typ"), b"= Title\n\nChanged body.\n").unwrap();
    f.service.check_document(&id);
    assert_eq!(text(&f.service, &id), "= Title\n\nChanged body.\n");
    assert!(!dirty(&f.service, &id));
    match docs.try_recv().unwrap() {
        DocumentEvent::Changed(changed) => {
            assert_eq!(changed.origin, EditOrigin::External);
            assert_eq!(changed.patches, vec![TextPatch::replace(9, 10, "Changed b")], "minimal patch");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(events.try_recv().unwrap(), WorkspaceEvent::DocumentReloaded { .. }));
    // The previous text is one undo away.
    f.service.documents().undo(&id, EditOrigin::Source).unwrap();
    assert_eq!(text(&f.service, &id), "= Title\n\nBody.\n");
    // Our own save is not mistaken for an external change.
    f.service.save_document(&id, false).unwrap();
    let revision = f.service.documents().info(&id).unwrap().revision;
    f.service.check_document(&id);
    assert_eq!(f.service.documents().info(&id).unwrap().revision, revision);
}

#[test]
fn dirty_buffers_enter_a_conflict_and_never_lose_either_version() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "mine ")]);
    let mut events = f.service.subscribe();
    fs::write(f.file("main.typ"), b"theirs\n").unwrap();
    f.service.check_document(&id);
    assert_eq!(text(&f.service, &id), "mine = Title\n\nBody.\n");
    match events.try_recv().unwrap() {
        WorkspaceEvent::Conflict { conflict } => assert_eq!(conflict.kind, ConflictKind::Modified),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(f.service.open_document(&wp("main.typ")).unwrap().conflict.unwrap().kind, ConflictKind::Modified);
    assert_eq!(f.service.save_document(&id, false).unwrap_err().code, "file.modifiedOnDisk");

    // Reload from disk: the buffer takes the disk text; undo brings mine back.
    f.service.reload_document(&id).unwrap();
    assert_eq!(text(&f.service, &id), "theirs\n");
    assert!(!dirty(&f.service, &id));
    assert!(f.service.open_document(&wp("main.typ")).unwrap().conflict.is_none());
    f.service.documents().undo(&id, EditOrigin::Source).unwrap();
    assert_eq!(text(&f.service, &id), "mine = Title\n\nBody.\n");

    // Keep mine: a later save overwrites the observed disk version.
    fs::write(f.file("main.typ"), b"theirs again\n").unwrap();
    f.service.check_document(&id);
    f.service.keep_buffer(&id).unwrap();
    f.service.save_document(&id, false).unwrap();
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"mine = Title\n\nBody.\n");
}

#[test]
fn deleted_files_mark_the_buffer_unsaved_and_save_recreates_them() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    fs::remove_file(f.file("main.typ")).unwrap();
    f.service.check_document(&id);
    assert!(dirty(&f.service, &id));
    assert_eq!(f.service.open_document(&wp("main.typ")).unwrap().conflict.unwrap().kind, ConflictKind::Deleted);
    f.service.save_document(&id, false).unwrap();
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"= Title\n\nBody.\n");
    assert!(f.service.open_document(&wp("main.typ")).unwrap().conflict.is_none());
}

#[test]
fn recovery_restores_unsaved_text_with_undo_back_to_disk() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(15, "Unsaved paragraph.\n")]);
    f.service.journal_documents(&HashSet::from([id.clone()]));
    // Crash: no save, no close. A new session opens the same folder.
    let service = f.restart();
    let entries = service.recovery_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path.as_str(), "main.typ");
    assert_eq!(entries[0].disk, RecoveryDiskState::Unchanged);
    assert_eq!(service.current().unwrap().recovery, entries);

    let mut docs = service.documents().subscribe();
    let restored = service.restore_recovery(&entries[0].id).unwrap();
    assert!(!restored.disk_changed);
    assert_eq!(restored.state.text, "= Title\n\nBody.\nUnsaved paragraph.\n");
    let restored_id = restored.state.info.id.clone();
    assert!(dirty(&service, &restored_id));
    let recovery_event = std::iter::from_fn(|| docs.try_recv().ok())
        .find_map(|e| match e {
            DocumentEvent::Changed(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(recovery_event.origin, EditOrigin::Recovery);
    assert!(service.recovery_entries().unwrap().is_empty(), "entry consumed");
    // The restored buffer is journaled again before the entry was removed.
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"= Title\n\nBody.\n", "disk untouched");
    let undone = service.documents().undo(&restored_id, EditOrigin::Source).unwrap();
    assert_eq!(undone.kind, ChangeKind::Undo);
    assert_eq!(text(&service, &restored_id), "= Title\n\nBody.\n");

    // A second crash now still has the restored text (redo re-applies it first).
    service.documents().redo(&restored_id, EditOrigin::Source).unwrap();
    service.journal_documents(&HashSet::from([restored_id]));
    let again = f.restart();
    assert_eq!(again.recovery_entries().unwrap().len(), 1);
}

#[test]
fn recovery_keeps_both_versions_when_disk_changed() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "mine ")]);
    f.service.journal_documents(&HashSet::from([id]));
    fs::write(f.file("main.typ"), b"changed by another tool\n").unwrap();

    let service = f.restart();
    let entries = service.recovery_entries().unwrap();
    assert_eq!(entries[0].disk, RecoveryDiskState::Changed);
    let restored = service.restore_recovery(&entries[0].id).unwrap();
    assert!(restored.disk_changed);
    assert_eq!(restored.state.text, "mine = Title\n\nBody.\n");
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"changed by another tool\n");
    service.documents().undo(&restored.state.info.id, EditOrigin::Source).unwrap();
    assert_eq!(text(&service, &restored.state.info.id), "changed by another tool\n");
}

#[test]
fn saving_closing_or_matching_disk_clears_journals() {
    let f = fixture();
    let a = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    let b = f.service.open_document(&wp("crlf.tex")).unwrap().state.info.id;
    edit(&f.service, &a, vec![TextPatch::insert(0, "x")]);
    edit(&f.service, &b, vec![TextPatch::insert(0, "y")]);
    f.service.journal_documents(&HashSet::from([a.clone(), b.clone()]));
    f.service.save_document(&a, false).unwrap();
    f.service.close_document(&b).unwrap();
    assert!(f.restart().recovery_entries().unwrap().is_empty());

    // A journal whose text equals the disk is dropped instead of offered.
    let service = f.restart();
    let c = service.open_document(&wp("bom.md")).unwrap().state.info.id;
    edit(&service, &c, vec![TextPatch::insert(0, "z")]);
    service.journal_documents(&HashSet::from([c.clone()]));
    fs::write(f.file("bom.md"), b"\xEF\xBB\xBFz# Heading\n").unwrap();
    assert!(f.restart().recovery_entries().unwrap().is_empty());
}

#[test]
fn discard_removes_recovery_entries() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "x")]);
    f.service.journal_documents(&HashSet::from([id]));
    let service = f.restart();
    let entries = service.recovery_entries().unwrap();
    assert!(service.discard_recovery(&entries[0].id).unwrap().is_empty());
    assert_eq!(service.restore_recovery(&entries[0].id).unwrap_err().code, "recovery.notFound");
    assert_eq!(service.discard_recovery("../../settings").unwrap_err().code, "recovery.io");
}

#[test]
fn normalizing_line_endings_is_one_undoable_transaction() {
    let f = fixture();
    let id = f.service.open_document(&wp("mixed.txt")).unwrap().state.info.id;
    f.service.normalize_line_endings(&id, LineEnding::Lf).unwrap().unwrap();
    assert_eq!(text(&f.service, &id), "a\nb\nc\n");
    assert!(f.service.normalize_line_endings(&id, LineEnding::Lf).unwrap().is_none());
    f.service.documents().undo(&id, EditOrigin::Source).unwrap();
    assert_eq!(text(&f.service, &id), "a\r\nb\nc\r\n");
    f.service.normalize_line_endings(&id, LineEnding::Crlf).unwrap();
    assert_eq!(text(&f.service, &id), "a\r\nb\r\nc\r\n");
    assert_eq!(line_ending_patches("x\ry", "\n"), vec![TextPatch::replace(1, 2, "\n")]);
}

#[test]
fn build_inputs_prefer_open_buffers() {
    let f = fixture();
    let id = f.service.open_document(&wp("bom.md")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "new ")]);
    let inputs = f.service.build_inputs(&[wp("bom.md"), wp("main.typ")]).unwrap();
    assert_eq!(inputs[0].document.as_ref(), Some(&id));
    assert_eq!(&*inputs[0].bytes, b"\xEF\xBB\xBFnew # Heading\n");
    assert_eq!(inputs[0].text.as_deref(), Some("new # Heading\n"));
    assert_eq!(inputs[1].document, None);
    assert_eq!(&*inputs[1].bytes, b"= Title\n\nBody.\n");
    assert_eq!(inputs[1].content_hash, sha256_hex(b"= Title\n\nBody.\n"));

    // A snapshot is immutable: later edits do not change what it reads.
    let snapshot = f.service.snapshot().unwrap();
    edit(&f.service, &id, vec![TextPatch::insert(0, "later ")]);
    assert_eq!(snapshot.read(&wp("bom.md")).unwrap().text.as_deref(), Some("new # Heading\n"));
    assert_eq!(snapshot.buffers().count(), 1);
    // Disk reads go through the same path checks.
    let outside = f.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    junction(&f.file("escape"), &outside);
    assert_eq!(snapshot.read(&wp("escape/x.typ")).unwrap_err().code, "path.outsideWorkspace");
    assert_eq!(snapshot.read(&wp("chapters")).unwrap_err().code, "file.isDirectory");
    assert_eq!(snapshot.read(&wp("image.png")).unwrap().text, None, "binary inputs carry bytes only");
}

#[test]
fn project_and_app_settings_round_trip() {
    let f = fixture();
    let state = f.service.project_settings().unwrap();
    assert!(!state.exists);
    let mut settings = state.settings;
    settings.entry = Some(wp("main.typ"));
    f.service.update_project_settings(&settings).unwrap();
    assert!(f.file(".exposition/project.json").exists());
    assert_eq!(f.service.project_settings().unwrap().settings, settings);

    let mut app = f.service.app_settings().settings;
    app.editor.tab_size = 4;
    f.service.update_app_settings(&app).unwrap();
    assert_eq!(f.restart().app_settings().settings.editor.tab_size, 4);
    assert_eq!(f.service.recent_projects()[0].name, "project");
}

#[test]
fn switching_workspaces_closes_documents_and_keeps_journals() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "x")]);
    f.service.journal_documents(&HashSet::from([id]));
    let other = f.dir.path().join("other");
    fs::create_dir_all(&other).unwrap();
    // Re-opening the same folder is a no-op.
    f.service.open_workspace(&f.project()).unwrap();
    assert_eq!(f.service.documents().list().len(), 1);
    f.service.open_workspace(&other).unwrap();
    assert!(f.service.documents().list().is_empty());
    let back = f.service.open_workspace(&f.project()).unwrap();
    assert_eq!(back.recovery.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_reloads_clean_buffers_and_reports_file_changes() {
    let f = fixture();
    let service = f.service.clone();
    let mut events = service.subscribe();
    tokio::spawn(service.clone().run());
    let id = service.open_document(&wp("main.typ")).unwrap().state.info.id;
    // Give the watcher a moment to arm, then change files externally.
    tokio::time::sleep(Duration::from_millis(200)).await;
    fs::write(f.file("main.typ"), b"= Title\n\nEdited elsewhere.\n").unwrap();
    fs::write(f.file("chapters/new.typ"), b"new\n").unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut reloaded, mut created) = (false, false);
    while !(reloaded && created) && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), events.recv()).await {
            Ok(Ok(WorkspaceEvent::DocumentReloaded { document, .. })) if document == id => reloaded = true,
            Ok(Ok(WorkspaceEvent::FilesChanged { changes, .. })) => {
                created |= changes.iter().any(|c| c.path.as_str() == "chapters/new.typ");
            }
            _ => {}
        }
    }
    assert!(reloaded && created, "reloaded={reloaded} created={created}");
    assert_eq!(text(&service, &id), "= Title\n\nEdited elsewhere.\n");

    // Typing is journaled shortly after, without any explicit call.
    edit(&service, &id, vec![TextPatch::insert(0, "typed ")]);
    tokio::time::sleep(JOURNAL_DELAY * 4).await;
    let live = fs::read_dir(service.recovery_store().unwrap().dir())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("live-"))
        .count();
    assert_eq!(live, 1);
}
