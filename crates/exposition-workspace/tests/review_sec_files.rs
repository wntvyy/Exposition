//! R-sec (M6 security review): file integrity and workspace-root checks of
//! the workspace service that are reachable without Tauri.
//!
//! Failing checks are `#[ignore = "R-sec Fn: ..."]`; run all with
//! `cargo test -p exposition-workspace --test review_sec_files -- --include-ignored`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
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
    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("main.typ"), b"= Title\n\nBody.\n").unwrap();
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    service.open_workspace(&project).unwrap();
    Fixture { dir, service }
}

fn edit(service: &WorkspaceService, path: &str, text: &str) -> exposition_core::document::DocumentId {
    let opened = service.open_document(&wp(path)).unwrap();
    let info = opened.state.info;
    service
        .documents()
        .apply(TransactionRequest {
            id: TransactionId("review-sec".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: info.id.clone(),
                base_revision: info.revision,
                patches: vec![TextPatch { range: TextRange { start: 0, end: 0 }, text: text.into() }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    info.id
}

/// The current token's user SID as an icacls principal (`*S-1-5-...`); the
/// account name can resolve to a different SID (e.g. Microsoft accounts).
#[cfg(windows)]
fn user() -> String {
    let out = std::process::Command::new("whoami").args(["/user", "/fo", "csv", "/nh"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let sid = text.trim().rsplit(',').next().unwrap().trim_matches(|c| c == '"' || c == '\r' || c == '\n').to_owned();
    assert!(sid.starts_with("S-1-"), "no SID from whoami: {text}");
    format!("*{sid}")
}

/// Deny creating files in `dir` (not inherited by existing children).
#[cfg(windows)]
fn deny_add_file(dir: &Path) {
    let status = std::process::Command::new("icacls")
        .arg(dir)
        .args(["/deny", &format!("{}:(WD)", user())])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "icacls /deny failed");
}

#[cfg(windows)]
fn allow_again(dir: &Path) {
    let _ = std::process::Command::new("icacls")
        .arg(dir)
        .args(["/remove:d", &user()])
        .stdout(std::process::Stdio::null())
        .status();
}

/// R-sec F5: reading project settings moves an invalid/newer
/// `.exposition/project.json` aside by `fs::copy(file, backup)` (result
/// ignored) followed by `fs::remove_file(file)`. When the backup cannot be
/// written, the original is deleted anyway and its bytes are gone.
/// (`exposition-workspace/src/settings.rs::load`; app `settings.json` uses the
/// same code.)
#[cfg(windows)]
#[test]
fn invalid_project_settings_are_never_deleted_without_a_backup() {
    let fx = fixture();
    let dot = fx.project().join(".exposition");
    fs::create_dir_all(&dot).unwrap();
    // A project.json written by a newer Exposition (version 2): valid JSON the
    // user wants to keep.
    let original = b"{ \"version\": 2, \"entry\": \"main.typ\", \"futureField\": [1, 2, 3] }\n";
    fs::write(dot.join("project.json"), original).unwrap();
    deny_add_file(&dot);
    let state = fx.service.project_settings();
    allow_again(&dot);
    let left: Vec<String> =
        fs::read_dir(&dot).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    let kept = fs::read(dot.join("project.json")).ok().as_deref() == Some(&original[..])
        || left.iter().any(|n| {
            n.starts_with("project.json.invalid-") && fs::read(dot.join(n)).ok().as_deref() == Some(&original[..])
        });
    assert!(
        kept,
        "DEFECT: the newer-version project.json was deleted and no backup exists (project_settings → {state:?}); .exposition now holds {left:?}"
    );
}

/// HELD when the backup can be written: the original bytes survive under
/// `project.json.invalid-<ms>` (documented behavior). Records that merely
/// reading project settings renames a file inside an untrusted project.
#[test]
fn invalid_project_settings_are_preserved_when_backup_succeeds() {
    let fx = fixture();
    let dot = fx.project().join(".exposition");
    fs::create_dir_all(&dot).unwrap();
    let original = b"{ \"version\": 2 }\n";
    fs::write(dot.join("project.json"), original).unwrap();
    let _ = fx.service.project_settings();
    let names: Vec<String> =
        fs::read_dir(&dot).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    let backup = names.iter().find(|n| n.starts_with("project.json.invalid-")).expect("backup written");
    assert_eq!(fs::read(dot.join(backup)).unwrap(), original);
}

/// R-sec F3: the application's own data directory (recovery journals,
/// `execution/trust.json`, `execution/settings.json` with tool locations) can
/// be opened as a project. Its files then become editable and saveable through
/// the document service (and by agents with project scope), e.g. adding a
/// trusted folder or a tool location without the trust dialog or the
/// tool-location checks.
#[test]
#[ignore = "R-sec F3: the app's own data directory can be opened as a workspace and its files edited"]
fn the_application_data_directory_is_not_a_workspace() {
    let fx = fixture();
    // Something the app keeps there (the execution service writes trust.json
    // into <data>/execution/; the workspace service writes recent-projects.json).
    let exec = fx.data().join("execution");
    fs::create_dir_all(&exec).unwrap();
    fs::write(exec.join("trust.json"), b"{\"version\":1,\"roots\":[]}\n").unwrap();
    let opened = fx.service.open_workspace(&fx.data());
    let opened_sub = opened.is_ok() || fx.service.open_workspace(&exec).is_ok();
    let mut detail = format!("open_workspace(data dir) → {:?}", opened.as_ref().map(|i| i.root.clone()));
    if opened.is_ok() {
        // Show the consequence: trust.json edited and saved like any document.
        let id = edit(&fx.service, "execution/trust.json", " ");
        let saved = fx.service.save_document(&id, false);
        detail.push_str(&format!("; editing+saving execution/trust.json → {:?}", saved.map(|i| i.revision)));
    }
    assert!(!opened_sub, "DEFECT: {detail}");
}

/// HELD: an atomic save that fails (target locked without delete sharing)
/// leaves the original bytes and no temporary file behind.
#[cfg(windows)]
#[test]
fn failed_save_on_a_locked_file_keeps_the_original() {
    use std::os::windows::fs::OpenOptionsExt;
    let fx = fixture();
    let id = edit(&fx.service, "main.typ", "EDIT ");
    let target = fx.project().join("main.typ");
    // Share read/write but NOT delete: a rename over it must fail.
    let lock = fs::OpenOptions::new().read(true).share_mode(0x1 | 0x2).open(&target).unwrap();
    let outcome = fx.service.save_document(&id, false);
    drop(lock);
    assert!(outcome.is_err(), "save over a locked file reported success: {outcome:?}");
    assert_eq!(fs::read(&target).unwrap(), b"= Title\n\nBody.\n");
    let temps: Vec<String> = fs::read_dir(fx.project())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".exposition-save-"))
        .collect();
    assert!(temps.is_empty(), "temporary files left: {temps:?}");
    // The buffer is still dirty and saves once the lock is gone.
    fx.service.save_document(&id, false).unwrap();
    assert!(fs::read(&target).unwrap().starts_with(b"EDIT "));
}

/// HELD: an open file replaced by a hard link to an outside file between open
/// and save: a normal save reports the external change; an explicit overwrite
/// replaces the link entry and never writes the outside file.
#[test]
fn save_after_swap_to_a_hard_link_never_writes_the_outside_file() {
    let fx = fixture();
    let id = edit(&fx.service, "main.typ", "EDIT ");
    let outside = fx.dir.path().join("outside.txt");
    fs::write(&outside, b"OUTSIDE ORIGINAL\n").unwrap();
    let target = fx.project().join("main.typ");
    fs::remove_file(&target).unwrap();
    fs::hard_link(&outside, &target).unwrap();
    let normal = fx.service.save_document(&id, false);
    assert!(normal.is_err(), "save over a swapped file did not report the change: {normal:?}");
    let forced = fx.service.save_document(&id, true);
    assert_eq!(fs::read(&outside).unwrap(), b"OUTSIDE ORIGINAL\n", "outside file written ({forced:?})");
}

/// HELD: `.exposition` replaced by a junction to an outside folder: project
/// settings are neither read from nor written to the outside folder.
#[cfg(windows)]
#[test]
fn project_settings_never_follow_a_junction_outside() {
    let fx = fixture();
    let outside = fx.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("project.json"), b"{\"version\":1,\"entry\":\"secret.typ\"}\n").unwrap();
    let status = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(fx.project().join(".exposition"))
        .arg(&outside)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let read = fx.service.project_settings();
    assert!(read.is_err(), "project settings read through the junction: {read:?}");
    let settings = exposition_workspace::settings::ProjectSettings::default();
    let write = fx.service.update_project_settings(&settings);
    assert!(write.is_err(), "project settings written through the junction: {write:?}");
    assert_eq!(fs::read(outside.join("project.json")).unwrap(), b"{\"version\":1,\"entry\":\"secret.typ\"}\n");
}

/// HELD: recovery journals live in the application data directory, never in
/// the project, and a journal's recorded path cannot escape the workspace
/// (a planted journal with a traversing path is quarantined, not restored).
#[test]
fn planted_recovery_journal_cannot_target_outside_the_workspace() {
    let fx = fixture();
    let root = fx.service.current().unwrap().root;
    let _id = edit(&fx.service, "main.typ", "UNSAVED ");
    fx.service.close_workspace();
    let project_files: Vec<PathBuf> = walk(&fx.project());
    assert!(
        project_files.iter().all(|p| !p.to_string_lossy().contains("recovery")),
        "recovery data inside the project: {project_files:?}"
    );
    // Plant journals whose paths traverse out of / are absolute outside the workspace.
    let key_dir = fx.data().join("recovery").join(exposition_workspace::recovery::workspace_key(&root));
    fs::create_dir_all(&key_dir).unwrap();
    let outside = fx.dir.path().join("outside").join("evil.txt").display().to_string();
    for (n, path) in ["../outside/evil.txt", outside.as_str(), "..\\outside\\evil.txt"].iter().enumerate() {
        let journal = serde_json::json!({
            "version": 1,
            "path": path,
            "baseHash": null,
            "revision": 1,
            "encoding": "utf8",
            "savedAt": 1,
            "text": "planted"
        });
        fs::write(key_dir.join(format!("rec-0123456789abcdef-{}.json", n + 1)), serde_json::to_vec(&journal).unwrap())
            .unwrap();
    }
    fx.service.open_workspace(&fx.project()).unwrap();
    let entries = fx.service.recovery_entries().unwrap();
    for entry in &entries {
        let _ = fx.service.restore_recovery(&entry.id);
    }
    assert!(!fx.dir.path().join("outside").join("evil.txt").exists());
    let _ = fx.service.save_all();
    assert!(!fx.dir.path().join("outside").join("evil.txt").exists(), "a planted journal wrote outside the workspace");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(read) = fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            }
            out.push(path);
        }
    }
    out
}
