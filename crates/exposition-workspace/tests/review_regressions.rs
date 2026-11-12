//! Regression tests for the independent M1 review (R-m1, review/m1 be63c73).
//!
//! Each `regression_*` test is a reviewer `review_defect_*` test converted to
//! assert the fixed behavior; each failed before its fix. `holds_*` tests are
//! reviewer attacks that held up and must keep holding.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_workspace::encoding::MAX_TEXT_BYTES;
use exposition_workspace::types::RecoveryDiskState;
use exposition_workspace::{DocumentService, WorkspaceService};

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

struct Fixture {
    dir: tempfile::TempDir,
    service: Arc<WorkspaceService>,
}

impl Fixture {
    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }
    fn file(&self, p: &str) -> PathBuf {
        self.project().join(p)
    }
    fn restart(&self) -> Arc<WorkspaceService> {
        let service = WorkspaceService::new(Arc::new(DocumentService::new()), self.dir.path().join("data"));
        service.open_workspace(&self.project()).unwrap();
        service
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("chapters")).unwrap();
    fs::write(project.join("main.typ"), b"= Title\n\nBody.\n").unwrap();
    fs::write(project.join("chapters").join("intro.typ"), b"intro\n").unwrap();
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    service.open_workspace(&project).unwrap();
    Fixture { dir, service }
}

fn edit(service: &WorkspaceService, id: &DocumentId, patches: Vec<TextPatch>) -> u64 {
    let docs = service.documents();
    let base = docs.info(id).unwrap().revision;
    docs.apply(TransactionRequest {
        id: TransactionId(format!("regression-{base}")),
        origin: EditOrigin::Source,
        edits: vec![DocumentEdit { document: id.clone(), base_revision: base, patches }],
        undo: UndoBehavior::NewGroup,
        conflict: ConflictPolicy::Reject,
        label: None,
        selection: None,
    })
    .unwrap()
    .applied[0]
        .revision
}

fn text(service: &WorkspaceService, id: &DocumentId) -> String {
    service.documents().state(id).unwrap().text
}

#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
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

// ---------------------------------------------------------------------------
// Finding 1: one document per physical file
// ---------------------------------------------------------------------------

/// Was review_defect_case_variant_opens_second_document_for_same_file.
#[cfg(windows)]
#[test]
fn regression_case_variant_returns_the_open_document() {
    let f = fixture();
    let a = f.service.open_document(&wp("main.typ")).unwrap().state;
    let b = f.service.open_document(&wp("MAIN.TYP")).unwrap().state;
    assert_eq!(a.info.id, b.info.id);
    assert_eq!(b.info.path.unwrap().as_str(), "main.typ", "recorded under the canonical path");
    // Opened through the alias first, the document still gets the canonical path.
    let c = f.service.open_document(&wp("CHAPTERS/Intro.TYP")).unwrap().state;
    assert_eq!(c.info.path.unwrap().as_str(), "chapters/intro.typ");
    assert_eq!(f.service.documents().list().len(), 2);
}

/// Was review_defect_inner_junction_opens_second_document_for_same_file.
#[test]
fn regression_inner_junction_returns_the_open_document() {
    let f = fixture();
    junction(&f.file("chapters-link"), &f.file("chapters"));
    let a = f.service.open_document(&wp("chapters/intro.typ")).unwrap().state.info.id;
    let b = f.service.open_document(&wp("chapters-link/intro.typ")).unwrap().state.info.id;
    assert_eq!(a, b);
    // Also in the other order: the alias resolves to the canonical file.
    let f = fixture();
    junction(&f.file("chapters-link"), &f.file("chapters"));
    let first = f.service.open_document(&wp("chapters-link/intro.typ")).unwrap().state;
    assert_eq!(first.info.path.unwrap().as_str(), "chapters/intro.typ");
}

/// Was review_defect_short_name_opens_second_document_for_same_file.
#[cfg(windows)]
#[test]
fn regression_short_name_returns_the_open_document() {
    let f = fixture();
    fs::write(f.file("long-chapter-name.typ"), b"text\n").unwrap();
    let out = std::process::Command::new("cmd")
        .args(["/C", "for %I in (long-chapter-name.typ) do @echo %~snxI"])
        .current_dir(f.project())
        .output()
        .unwrap();
    let short = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if short.is_empty() || short.eq_ignore_ascii_case("long-chapter-name.typ") {
        eprintln!("8.3 short names are disabled on this volume; skipping");
        return;
    }
    let a = f.service.open_document(&wp("long-chapter-name.typ")).unwrap().state.info.id;
    let b = f.service.open_document(&wp(&short)).unwrap().state.info.id;
    assert_eq!(a, b, "{short} opened a second document for long-chapter-name.typ");
}

/// Hard links inside the folder are the same physical file (volume + file index).
#[test]
fn regression_hard_link_returns_the_open_document() {
    let f = fixture();
    fs::hard_link(f.file("main.typ"), f.file("main-link.typ")).unwrap();
    let a = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    let b = f.service.open_document(&wp("main-link.typ")).unwrap().state.info.id;
    assert_eq!(a, b);
}

/// Was review_defect_case_variant_buffers_lose_recovery_data: edits through
/// either alias land in one buffer, and its one journal recovers all of them.
#[cfg(windows)]
#[test]
fn regression_aliases_share_one_buffer_and_journal() {
    let f = fixture();
    let a = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    let b = f.service.open_document(&wp("MAIN.TYP")).unwrap().state.info.id;
    assert_eq!(a, b);
    edit(&f.service, &a, vec![TextPatch::insert(0, "UNSAVED-A ")]);
    edit(&f.service, &b, vec![TextPatch::insert(0, "UNSAVED-B ")]);
    f.service.journal_documents(&[a].into_iter().collect());
    let service = f.restart();
    let entries = service.recovery_entries().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let restored = service.restore_recovery(&entries[0].id).unwrap();
    assert!(restored.state.text.contains("UNSAVED-A") && restored.state.text.contains("UNSAVED-B"));
}

/// Saving through one alias leaves no stale second buffer; build snapshots read
/// the buffer for any alias.
#[cfg(windows)]
#[test]
fn regression_aliases_see_saves_and_snapshots_read_the_buffer() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "edited ")]);
    let snapshot = f.service.snapshot().unwrap();
    let input = snapshot.read(&wp("MAIN.TYP")).unwrap();
    assert_eq!(input.document.as_ref(), Some(&id));
    assert_eq!(input.text.as_deref(), Some("edited = Title\n\nBody.\n"));
    f.service.save_document(&id, false).unwrap();
    let alias = f.service.open_document(&wp("MAIN.TYP")).unwrap().state;
    assert_eq!(alias.info.id, id);
    assert_eq!(alias.text, "edited = Title\n\nBody.\n");
    assert_eq!(alias.info.saved_revision, Some(alias.info.revision));
}

// ---------------------------------------------------------------------------
// Finding 3: what Exposition saves, it can reopen
// ---------------------------------------------------------------------------

/// Was review_defect_saved_document_larger_than_limit_cannot_be_reopened: the
/// save is refused with the same limit (buffer kept, file intact), so the file
/// on disk always stays reopenable.
#[test]
fn regression_document_over_the_size_limit_is_not_saved() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    let big = "x".repeat(MAX_TEXT_BYTES as usize);
    edit(&f.service, &id, vec![TextPatch::insert(0, big)]);
    let err = f.service.save_document(&id, false).unwrap_err();
    assert_eq!(err.code, "file.tooLarge");
    assert!(err.message.contains("10 MB"), "{}", err.message);
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"= Title\n\nBody.\n");
    let info = f.service.documents().info(&id).unwrap();
    assert_ne!(info.saved_revision, Some(info.revision), "buffer stays dirty");
    f.service.close_document(&id).unwrap();
    assert!(f.service.open_document(&wp("main.typ")).is_ok());

    // Exactly at the limit saves and reopens.
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    let len = text(&f.service, &id).len();
    edit(&f.service, &id, vec![TextPatch::insert(0, "y".repeat(MAX_TEXT_BYTES as usize - len))]);
    f.service.save_document(&id, false).unwrap();
    f.service.close_document(&id).unwrap();
    assert!(f.service.open_document(&wp("main.typ")).is_ok());
}

/// Was review_defect_nul_character_saved_then_unopenable: text with an isolated
/// NUL saves and reopens as text with the NUL intact.
#[test]
fn regression_nul_character_saves_and_reopens() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "a\u{0}b ")]);
    f.service.save_document(&id, false).unwrap();
    f.service.close_document(&id).unwrap();
    let reopened = f.service.open_document(&wp("main.typ")).unwrap().state;
    assert_eq!(reopened.text, "a\u{0}b = Title\n\nBody.\n");

    // Text dense with control characters would reopen as binary, so it is not saved.
    let id = reopened.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "\u{0}".repeat(64))]);
    let err = f.service.save_document(&id, false).unwrap_err();
    assert_eq!(err.code, "file.binary");
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"a\0b = Title\n\nBody.\n");
}

// ---------------------------------------------------------------------------
// Finding 4: unreadable recovery data is surfaced and quarantined
// ---------------------------------------------------------------------------

/// Was review_holds_corrupt_journals_do_not_break_opening plus the
/// review-m1-integrity finding that corrupt journals were silently dropped.
#[test]
fn regression_corrupt_journals_are_surfaced_quarantined_and_discardable() {
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "precious ")]);
    f.service.journal_documents(&[id].into_iter().collect());
    let recovery_root = f.dir.path().join("data").join("recovery");
    let ws_dir = fs::read_dir(&recovery_root).unwrap().next().unwrap().unwrap().path();
    fs::write(ws_dir.join("live-0123456789abcdef.json"), b"{not json").unwrap();
    fs::write(ws_dir.join("live-1123456789abcdef.json"), b"").unwrap();
    fs::write(ws_dir.join("rec-2123456789abcdef-1.json"), br#"{"version":1,"path":"../../escape.txt","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}"#).unwrap();
    fs::write(
        ws_dir.join("rec-3123456789abcdef-1.json"),
        br#"{"version":99,"path":"a.txt","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}"#,
    )
    .unwrap();

    let service = f.restart();
    let entries = service.recovery_entries().unwrap();
    let (valid, unreadable): (Vec<_>, Vec<_>) = entries.iter().partition(|e| e.problem.is_none());
    assert_eq!(valid.len(), 1, "{entries:?}");
    assert_eq!(unreadable.len(), 4, "every corrupt journal is reported: {entries:?}");
    for entry in &unreadable {
        assert_eq!(entry.disk, RecoveryDiskState::Unknown);
        // Only a valid workspace path is salvaged from damaged data.
        assert!(entry.path.as_ref().is_none_or(|p| p.as_str() == "a.txt"), "{entry:?}");
        let location = PathBuf::from(entry.location.as_ref().unwrap());
        assert!(location.exists(), "quarantined data kept for inspection");
        assert_eq!(service.restore_recovery(&entry.id).unwrap_err().code, "recovery.unreadable");
    }
    assert!(unreadable.iter().any(|e| e.problem.as_ref().unwrap().contains("newer version")));
    // The valid entry still restores.
    let restored = service.restore_recovery(&valid[0].id).unwrap();
    assert!(restored.state.text.starts_with("precious "));
    assert!(!f.dir.path().join("escape.txt").exists());
    // Unreadable entries stay listed until explicitly discarded.
    // (The restored buffer was never saved, so the next session offers it again.)
    let again = f.restart();
    let listed = again.recovery_entries().unwrap();
    assert_eq!(listed.iter().filter(|e| e.problem.is_some()).count(), 4, "{listed:?}");
    for entry in &unreadable {
        again.discard_recovery(&entry.id).unwrap();
        assert!(!PathBuf::from(entry.location.as_ref().unwrap()).exists());
    }
    let remaining = again.recovery_entries().unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].problem.is_none());
}

// ---------------------------------------------------------------------------
// Finding 5: too-broad workspace roots
// ---------------------------------------------------------------------------

/// workspace_open(SystemRoot) then document_open("win.ini") read a system file.
#[cfg(windows)]
#[test]
fn regression_system_and_profile_folders_cannot_be_opened() {
    let f = fixture();
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let err = f.service.open_workspace(&system).unwrap_err();
    assert_eq!(err.code, "workspace.tooBroad");
    // The previous project stays open; nothing from the system folder is reachable.
    assert_eq!(f.service.current().unwrap().name, "project");
    assert!(f.service.open_document(&wp("win.ini")).is_err());
    let profile = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
    assert_eq!(f.service.open_workspace(&profile).unwrap_err().code, "workspace.tooBroad");
}

// ---------------------------------------------------------------------------
// Attacks that held and must keep holding
// ---------------------------------------------------------------------------

#[test]
fn holds_parent_swapped_for_junction_after_open() {
    let f = fixture();
    let outside = f.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("intro.typ"), b"OUTSIDE ORIGINAL\n").unwrap();
    let id = f.service.open_document(&wp("chapters/intro.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "edited ")]);
    fs::rename(f.file("chapters"), f.dir.path().join("chapters-moved")).unwrap();
    junction(&f.file("chapters"), &outside);
    for overwrite in [false, true] {
        let err = f.service.save_document(&id, overwrite).unwrap_err();
        assert_eq!(err.code, "path.outsideWorkspace", "overwrite={overwrite}");
    }
    assert_eq!(fs::read(outside.join("intro.typ")).unwrap(), b"OUTSIDE ORIGINAL\n");
    assert!(f.service.reload_document(&id).is_err());
}

#[test]
fn holds_hardlink_save_does_not_write_outside_file() {
    let f = fixture();
    let outside = f.dir.path().join("outside.txt");
    fs::write(&outside, b"outside\n").unwrap();
    fs::hard_link(&outside, f.file("linked.txt")).unwrap();
    let id = f.service.open_document(&wp("linked.txt")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "edited ")]);
    f.service.save_document(&id, false).unwrap();
    assert_eq!(fs::read(&outside).unwrap(), b"outside\n");
    assert_eq!(fs::read(f.file("linked.txt")).unwrap(), b"edited outside\n");
}

#[cfg(windows)]
#[test]
fn holds_save_to_exclusively_locked_file() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = fixture();
    let id = f.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&f.service, &id, vec![TextPatch::insert(0, "unsaved ")]);
    let lock = fs::OpenOptions::new().read(true).share_mode(0).open(f.file("main.typ")).unwrap();
    let err = f.service.save_document(&id, false).unwrap_err();
    drop(lock);
    assert!(matches!(err.code, "file.permissionDenied" | "file.writeFailed" | "file.io"), "{err:?}");
    assert_eq!(fs::read(f.file("main.typ")).unwrap(), b"= Title\n\nBody.\n");
    let info = f.service.documents().info(&id).unwrap();
    assert_ne!(info.saved_revision, Some(info.revision));
    f.service.save_document(&id, false).unwrap();
}
