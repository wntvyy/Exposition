//! Project operations on files: behavior against real folders, the real OS
//! Recycle Bin (disposable temp files, purged afterwards), real Windows
//! junctions (`mklink /J`), real read-only attributes and real file locks.
//! `FakeTrash` (a test double) is used only where an OS failure cannot be
//! produced on demand (an unavailable Recycle Bin) — those tests say so.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::ops::{ImpactKind, OpActor, OpDestination, OpKind, OpRecovery, OpStatus, OpTarget};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;

use super::*;
use crate::documents::DocumentService;
use crate::service::WorkspaceService;

struct Fixture {
    dir: tempfile::TempDir,
    service: Arc<WorkspaceService>,
}

impl Fixture {
    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }
    fn outside(&self) -> PathBuf {
        self.dir.path().join("outside")
    }
    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }
    fn file(&self, path: &str) -> PathBuf {
        let mut out = self.project();
        out.extend(path.split('/'));
        out
    }
    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.file(path)).unwrap()
    }
    fn exists(&self, path: &str) -> bool {
        fs::symlink_metadata(self.file(path)).is_ok()
    }
    fn restart(&self) -> Arc<WorkspaceService> {
        let service = WorkspaceService::new(Arc::new(DocumentService::new()), self.data());
        service.open_workspace(&self.project()).unwrap();
        service
    }
}

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

fn path(s: &str) -> OpTarget {
    OpTarget::Path { path: wp(s) }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    for d in ["chapters", "figures", "data/raw", ".exposition/runs", ".exposition/artifacts/r1", "src/__pycache__"] {
        fs::create_dir_all(project.join(d)).unwrap();
    }
    fs::create_dir_all(project.join(".exposition/research/rs-1/data")).unwrap();
    fs::create_dir_all(dir.path().join("outside")).unwrap();
    for (p, c) in [
        ("main.typ", "= Title\n"),
        ("chapters/intro.typ", "intro\n"),
        ("chapters/end.typ", "end\n"),
        ("figures/plot.png", "PNG"),
        ("data/raw/a.csv", "x,y\n1,2\n"),
        ("data/notes.md", "notes\n"),
        (".exposition/graph.sqlite3", "db"),
        (".exposition/project.json", "{}"),
        (".exposition/runs/r1.json", "{}"),
        (".exposition/artifacts/r1/output-1.png", "artifact"),
        (".exposition/research/rs-1/data/d.csv", "1\n"),
        ("src/__pycache__/m.cpython-314.pyc", "pyc"),
    ] {
        fs::write(project.join(p), c).unwrap();
    }
    fs::write(dir.path().join("outside/secret.txt"), "secret").unwrap();
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    service.open_workspace(&project).unwrap();
    Fixture { dir, service }
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
        .expect("run mklink");
    assert!(status.success(), "mklink /J failed");
}

#[cfg(unix)]
fn junction(link: &Path, target: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

fn edit(service: &WorkspaceService, id: &DocumentId, text: &str) {
    let base = service.documents().info(id).unwrap().revision;
    service
        .documents()
        .apply(TransactionRequest {
            id: TransactionId(format!("test-{base}")),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: id.clone(),
                base_revision: base,
                patches: vec![TextPatch { range: TextRange::new(0, 0), text: text.into() }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
}

fn delete(permanent: bool) -> OpKind {
    OpKind::Delete { permanent }
}

fn paste_into(folder: Option<&str>) -> OpKind {
    OpKind::Paste { destination: OpDestination::Folder { folder: folder.map(wp) } }
}

fn code_of(plan: &PathPlan, target: &OpTarget) -> Option<String> {
    plan.refusals.iter().find(|r| &r.target == target).map(|r| r.code.clone())
}

/// A trash double: records what it was asked to delete and fails on demand.
/// (Used where a real OS failure cannot be produced on demand.)
#[derive(Default)]
struct FakeTrash {
    fail: bool,
    deleted: Mutex<Vec<PathBuf>>,
}

impl TrashBin for FakeTrash {
    fn name(&self) -> &'static str {
        "the test trash"
    }
    fn delete(&self, paths: &[PathBuf]) -> Vec<Result<Option<TrashedItem>, String>> {
        paths
            .iter()
            .map(|p| {
                if self.fail {
                    Err("the Recycle Bin is not available on this drive".into())
                } else {
                    let to = p.with_extension("faketrashed");
                    fs::rename(p, &to).map_err(|e| e.to_string())?;
                    self.deleted.lock().unwrap().push(p.clone());
                    Ok(None)
                }
            })
            .collect()
    }
    fn contains(&self, _item: &TrashedItem) -> Result<bool, String> {
        Ok(false)
    }
    fn restore_to(&self, _item: &TrashedItem, _parent: &Path, _name: &str) -> Result<(), String> {
        Err("not supported".into())
    }
    fn purge(&self, _items: &[TrashedItem]) -> Result<(), String> {
        Ok(())
    }
}

fn apply(fx: &Fixture, plan: &PathPlan, trash: &dyn TrashBin, choices: &[(&str, &str)]) -> Vec<PathOutcome> {
    let choices: BTreeMap<String, String> = choices.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    fx.service.apply_paths(plan, &ApplyOptions { choices: &choices, trash, skip: &[], tag: "t1" }, &mut |_| {})
}

fn status_of(outcomes: &[PathOutcome], target: &OpTarget) -> OpStatus {
    outcomes.iter().find(|o| &o.outcome.target == target).map(|o| o.outcome.status).expect("an outcome per target")
}

/// The real Recycle Bin works on this host (else the real-trash tests skip).
fn real_trash(dir: &Path) -> Option<OsTrash> {
    let probe = dir.join("trash-probe.txt");
    fs::write(&probe, "probe").unwrap();
    let canonical = fs::canonicalize(dir).unwrap().join("trash-probe.txt");
    match OsTrash.delete(&[canonical]).pop().unwrap() {
        Ok(found) => {
            if let Some(item) = found {
                let _ = OsTrash.purge(&[item]);
            }
            Some(OsTrash)
        }
        Err(e) => {
            eprintln!("SKIPPED: the OS trash is not available here: {e}");
            None
        }
    }
}

// ------------------------------------------------------------ delete ------

#[test]
fn delete_moves_files_and_folders_to_the_real_recycle_bin_and_restores_them() {
    let fx = fixture();
    let Some(bin) = real_trash(fx.dir.path()) else { return };
    let plan = fx.service.plan_paths(&delete(false), &[path("main.typ"), path("data")], &OpActor::User);
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert!(plan.impacts.is_empty(), "unused files need no decision: {:?}", plan.impacts);
    assert!(plan.effects.iter().all(|e| e.recovery == OpRecovery::OsTrash));
    let outcomes = apply(&fx, &plan, &bin, &[]);
    assert!(!fx.exists("main.typ") && !fx.exists("data"));
    let mut items = Vec::new();
    for o in &outcomes {
        assert_eq!(o.outcome.status, OpStatus::Succeeded, "{:?}", o.outcome);
        assert_eq!(o.outcome.recovery, OpRecovery::OsTrash);
        if cfg!(windows) {
            assert!(o.trashed.is_some(), "the trashed item is identified reliably on Windows: {:?}", o.outcome);
        }
        items.extend(o.trashed.clone());
    }
    // Restore both to their original places.
    for o in &outcomes {
        let Some(item) = &o.trashed else { continue };
        let restored = fx.service.restore_trashed(o.path.as_ref().unwrap(), item, None, &bin, "r1");
        assert_eq!(restored.status, OpStatus::Succeeded, "{restored:?}");
        assert_eq!(restored.created, Some(o.outcome.target.clone()));
    }
    if cfg!(windows) {
        assert_eq!(fx.read("main.typ"), "= Title\n");
        assert_eq!(fx.read("data/raw/a.csv"), "x,y\n1,2\n");
        assert_eq!(fx.read("data/notes.md"), "notes\n");
        // Restoring again: it is no longer in the Recycle Bin.
        let again = fx.service.restore_trashed(&wp("main.typ"), &items[0], None, &bin, "r2");
        assert_eq!(again.code.as_deref(), Some("ops.restoreUnavailable"));
    }
    let _ = bin.purge(&items);
}

#[test]
fn restore_never_overwrites_an_occupied_original_path() {
    let fx = fixture();
    let Some(bin) = real_trash(fx.dir.path()) else { return };
    if !cfg!(windows) {
        return;
    }
    let plan = fx.service.plan_paths(&delete(false), &[path("chapters/intro.typ")], &OpActor::User);
    let outcomes = apply(&fx, &plan, &bin, &[]);
    let item = outcomes[0].trashed.clone().expect("identified");
    // Something new now lives at the original path.
    fs::write(fx.file("chapters/intro.typ"), "a NEW intro").unwrap();

    let cancelled = fx.service.restore_trashed(&wp("chapters/intro.typ"), &item, Some("cancel"), &bin, "r1");
    assert_eq!(cancelled.status, OpStatus::Skipped);
    assert_eq!(cancelled.code.as_deref(), Some("ops.restoreCollision"));
    assert_eq!(fx.read("chapters/intro.typ"), "a NEW intro");
    assert!(bin.contains(&item).unwrap(), "cancel leaves it in the Recycle Bin");

    let renamed = fx.service.restore_trashed(&wp("chapters/intro.typ"), &item, None, &bin, "r2");
    assert_eq!(renamed.status, OpStatus::Succeeded, "{renamed:?}");
    assert_eq!(renamed.created, Some(path("chapters/intro (2).typ")));
    assert_eq!(fx.read("chapters/intro.typ"), "a NEW intro", "the new file is untouched");
    assert_eq!(fx.read("chapters/intro (2).typ"), "intro\n", "both versions kept");
    let leftovers: Vec<_> = fs::read_dir(fx.file("chapters"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(crate::fsutil::TEMP_MARKER))
        .collect();
    assert!(leftovers.is_empty(), "no staging leftovers");
}

#[cfg(windows)]
#[test]
fn a_locked_file_fails_to_trash_and_is_never_deleted_permanently_instead() {
    use std::os::windows::fs::OpenOptionsExt;
    let fx = fixture();
    let Some(bin) = real_trash(fx.dir.path()) else { return };
    // Real OS failure: the file is open without sharing delete access.
    let lock = fs::OpenOptions::new().read(true).share_mode(0).open(fx.file("chapters/end.typ")).unwrap();
    let plan =
        fx.service.plan_paths(&delete(false), &[path("chapters/end.typ"), path("chapters/intro.typ")], &OpActor::User);
    let outcomes = apply(&fx, &plan, &bin, &[]);
    drop(lock);
    // Partial success is reported per target.
    assert_eq!(status_of(&outcomes, &path("chapters/end.typ")), OpStatus::Failed);
    let failed = outcomes.iter().find(|o| o.outcome.target == path("chapters/end.typ")).unwrap();
    assert_eq!(failed.outcome.code.as_deref(), Some("ops.trashUnavailable"));
    assert!(failed.outcome.message.as_deref().unwrap().contains("Nothing was deleted permanently"));
    assert_eq!(fx.read("chapters/end.typ"), "end\n", "the locked file is intact");
    assert_eq!(status_of(&outcomes, &path("chapters/intro.typ")), OpStatus::Succeeded);
    assert!(!fx.exists("chapters/intro.typ"));
    let items: Vec<_> = outcomes.iter().filter_map(|o| o.trashed.clone()).collect();
    let _ = bin.purge(&items);
}

#[test]
fn an_unavailable_trash_never_falls_back_to_permanent_deletion() {
    // MOCKED trash (an unavailable Recycle Bin cannot be produced on demand).
    let fx = fixture();
    let bin = FakeTrash { fail: true, ..FakeTrash::default() };
    let plan = fx.service.plan_paths(&delete(false), &[path("main.typ"), path("data")], &OpActor::User);
    let outcomes = apply(&fx, &plan, &bin, &[]);
    for o in &outcomes {
        assert_eq!(o.outcome.status, OpStatus::Failed);
        assert_eq!(o.outcome.code.as_deref(), Some("ops.trashUnavailable"));
    }
    assert!(fx.exists("main.typ") && fx.exists("data/raw/a.csv"), "nothing removed");
}

#[test]
fn permanent_deletion_states_the_lost_recovery_except_for_caches() {
    let fx = fixture();
    let plan = fx.service.plan_paths(
        &delete(true),
        &[path("chapters"), path("src/__pycache__"), path("figures/plot.png")],
        &OpActor::User,
    );
    let losses: Vec<&OpTarget> =
        plan.impacts.iter().filter(|i| i.kind == ImpactKind::RecoveryLoss).map(|i| &i.target).collect();
    assert_eq!(losses, [&path("chapters"), &path("figures/plot.png")], "{:?}", plan.impacts);
    assert!(plan.impacts.iter().all(|i| i.message.contains("permanently")));
    let outcomes = apply(&fx, &plan, &FakeTrash::default(), &[]);
    assert!(outcomes.iter().all(|o| o.outcome.status == OpStatus::Succeeded && o.outcome.recovery == OpRecovery::None));
    assert!(!fx.exists("chapters") && !fx.exists("src/__pycache__") && !fx.exists("figures/plot.png"));
}

#[test]
fn overlapping_selections_are_normalized() {
    let fx = fixture();
    let bin = FakeTrash::default();
    let targets = [path("data/raw/a.csv"), path("data"), path("DATA/notes.md"), path("data")];
    let plan = fx.service.plan_paths(&delete(false), &targets, &OpActor::User);
    assert_eq!(plan.deleted, [wp("data")]);
    assert_eq!(plan.effects.len(), 1);
    let outcomes = apply(&fx, &plan, &bin, &[]);
    assert_eq!(bin.deleted.lock().unwrap().len(), 1, "one trash operation for the folder");
    let covered = outcomes.iter().find(|o| o.outcome.target == path("data/raw/a.csv")).unwrap();
    assert_eq!(covered.outcome.status, OpStatus::Succeeded);
    assert_eq!(covered.outcome.message.as_deref(), Some("Included in data."));
}

#[test]
fn protected_application_data_is_refused() {
    let fx = fixture();
    for target in
        [".exposition", ".exposition/graph.sqlite3", ".exposition/runs/r1.json", ".exposition/research/rs-1/data"]
    {
        let plan = fx.service.plan_paths(&delete(true), &[path(target)], &OpActor::User);
        assert_eq!(code_of(&plan, &path(target)).as_deref(), Some("ops.protected"), "{target}");
        let plan = fx.service.plan_paths(&OpKind::Duplicate, &[path(target)], &OpActor::User);
        assert_eq!(code_of(&plan, &path(target)).as_deref(), Some("ops.protected"), "duplicate {target}");
    }
    // project.json lives in the metadata folder: it cannot be duplicated there.
    let plan = fx.service.plan_paths(&OpKind::Duplicate, &[path(".exposition/project.json")], &OpActor::User);
    assert_eq!(code_of(&plan, &path(".exposition/project.json")).as_deref(), Some("ops.protected"));
    for dest in [".exposition", ".exposition/artifacts/r1", ".exposition/runs"] {
        let plan = fx.service.plan_paths(&paste_into(Some(dest)), &[path("main.typ")], &OpActor::User);
        assert_eq!(code_of(&plan, &path("main.typ")).as_deref(), Some("ops.protected"), "into {dest}");
    }
    assert!(fx.exists(".exposition/graph.sqlite3") && fx.exists(".exposition/runs/r1.json"));
    // Research files and generated artifacts can be copied out; research files deleted.
    let plan = fx.service.plan_paths(
        &paste_into(Some("figures")),
        &[path(".exposition/artifacts/r1/output-1.png")],
        &OpActor::User,
    );
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    let plan = fx.service.plan_paths(&delete(false), &[path(".exposition/research/rs-1/data/d.csv")], &OpActor::User);
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
}

#[test]
fn read_only_entries_are_refused() {
    let fx = fixture();
    let set_ro = |p: &Path, ro: bool| {
        let mut perms = fs::metadata(p).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(ro);
        fs::set_permissions(p, perms).unwrap();
    };
    set_ro(&fx.file("main.typ"), true);
    set_ro(&fx.file("data/raw/a.csv"), true);
    for permanent in [false, true] {
        let plan = fx.service.plan_paths(&delete(permanent), &[path("main.typ"), path("data")], &OpActor::User);
        assert_eq!(code_of(&plan, &path("main.typ")).as_deref(), Some("ops.readOnly"));
        let folder = plan.refusals.iter().find(|r| r.target == path("data")).expect("folder refused");
        assert_eq!(folder.code, "ops.readOnly");
        assert!(folder.message.contains("data/raw/a.csv"), "{}", folder.message);
        assert!(plan.effects.is_empty());
        apply(&fx, &plan, &FakeTrash::default(), &[]);
    }
    assert!(fx.exists("main.typ") && fx.exists("data/notes.md"));
    set_ro(&fx.file("main.typ"), false);
    set_ro(&fx.file("data/raw/a.csv"), false);
}

#[test]
fn deleting_a_junction_never_touches_its_target() {
    let fx = fixture();
    junction(&fx.file("escape"), &fx.outside());
    fs::create_dir_all(fx.file("box")).unwrap();
    junction(&fx.file("box/inner-escape"), &fx.outside());
    // Through the junction is outside the project.
    let plan = fx.service.plan_paths(&delete(false), &[path("escape/secret.txt")], &OpActor::User);
    assert_eq!(code_of(&plan, &path("escape/secret.txt")).as_deref(), Some("ops.outsideProject"));
    // The junction itself is a project entry: removing it removes only the link.
    let plan = fx.service.plan_paths(&delete(true), &[path("escape"), path("box")], &OpActor::User);
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    let outcomes = apply(&fx, &plan, &FakeTrash::default(), &[]);
    assert!(outcomes.iter().all(|o| o.outcome.status == OpStatus::Succeeded), "{outcomes:?}");
    assert!(!fx.exists("escape") && !fx.exists("box"));
    assert_eq!(fs::read_to_string(fx.outside().join("secret.txt")).unwrap(), "secret", "target intact");
}

#[cfg(windows)]
#[test]
fn trashing_a_junction_in_the_real_recycle_bin_keeps_its_target() {
    let fx = fixture();
    let Some(bin) = real_trash(fx.dir.path()) else { return };
    junction(&fx.file("escape"), &fx.outside());
    fs::create_dir_all(fx.file("box")).unwrap();
    junction(&fx.file("box/inner"), &fx.outside());
    let plan = fx.service.plan_paths(&delete(false), &[path("escape"), path("box")], &OpActor::User);
    let outcomes = apply(&fx, &plan, &bin, &[]);
    assert!(outcomes.iter().all(|o| o.outcome.status == OpStatus::Succeeded), "{outcomes:?}");
    assert!(!fx.exists("escape") && !fx.exists("box"));
    assert_eq!(fs::read_to_string(fx.outside().join("secret.txt")).unwrap(), "secret", "target intact");
    let items: Vec<_> = outcomes.iter().filter_map(|o| o.trashed.clone()).collect();
    let _ = bin.purge(&items);
    assert_eq!(fs::read_to_string(fx.outside().join("secret.txt")).unwrap(), "secret", "purging the link too");
}

// ------------------------------------------------------- dirty buffers ----

#[test]
fn unsaved_buffers_need_a_decision_and_preserving_suspends_saving_and_journaling() {
    let fx = fixture();
    let opened = fx.service.open_document(&wp("chapters/intro.typ")).unwrap();
    let id = opened.state.info.id.clone();
    edit(&fx.service, &id, "UNSAVED ");
    fx.service.journal_documents(&[id.clone()].into());
    let clean = fx.service.open_document(&wp("chapters/end.typ")).unwrap().state.info.id;

    let plan = fx.service.plan_paths(&delete(false), &[path("chapters")], &OpActor::User);
    let impact = plan.impacts.iter().find(|i| i.kind == ImpactKind::UnsavedBuffer).expect("unsaved impact");
    assert_eq!(impact.consumers[0].path, Some(wp("chapters/intro.typ")));
    let choice = plan.choices.iter().find(|c| c.key == CHOICE_UNSAVED).expect("choice");
    assert_eq!(choice.default, UNSAVED_PRESERVE, "the default is never destructive");

    let outcomes = apply(&fx, &plan, &FakeTrash::default(), &[(CHOICE_UNSAVED, UNSAVED_PRESERVE)]);
    assert_eq!(outcomes[0].outcome.status, OpStatus::Succeeded);
    assert!(!fx.exists("chapters"));
    // The text is still in the editor.
    assert!(fx.service.documents().state(&id).unwrap().text.starts_with("UNSAVED intro"));
    assert!(fx.service.is_suspended(&id));
    // The clean document of the deleted file closed.
    assert!(fx.service.documents().info(&clean).is_err());
    // Neither saving nor journaling brings the file back.
    let err = fx.service.save_document(&id, true).unwrap_err();
    assert_eq!(err.code, "document.fileDeleted");
    edit(&fx.service, &id, "MORE ");
    fx.service.journal_documents(&[id.clone()].into());
    assert!(!fx.exists("chapters/intro.typ"), "no save resurrection");
    // A restart offers no recovery that would recreate it.
    let restarted = fx.restart();
    assert!(restarted.recovery_entries().unwrap().is_empty(), "no journal resurrection");
}

#[test]
fn discarding_closes_the_buffer_and_removes_its_journal_and_cancel_keeps_everything() {
    let fx = fixture();
    let id = fx.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&fx.service, &id, "UNSAVED ");
    fx.service.journal_documents(&[id.clone()].into());

    // Cancel: nothing deleted, buffer still dirty and journaled.
    let plan = fx.service.plan_paths(&delete(false), &[path("main.typ"), path("figures")], &OpActor::User);
    let outcomes = apply(&fx, &plan, &FakeTrash::default(), &[(CHOICE_UNSAVED, UNSAVED_CANCEL)]);
    assert_eq!(status_of(&outcomes, &path("main.typ")), OpStatus::Skipped);
    assert_eq!(status_of(&outcomes, &path("figures")), OpStatus::Succeeded, "the rest proceeds");
    assert!(fx.exists("main.typ") && !fx.service.is_suspended(&id));
    assert_eq!(fx.restart().recovery_entries().unwrap().len(), 1, "journal kept");

    // Discard: closed, journal removed, file deleted.
    let fx = fixture();
    let id = fx.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&fx.service, &id, "UNSAVED ");
    fx.service.journal_documents(&[id.clone()].into());
    let plan = fx.service.plan_paths(&delete(false), &[path("main.typ")], &OpActor::User);
    let outcomes = apply(&fx, &plan, &FakeTrash::default(), &[(CHOICE_UNSAVED, UNSAVED_DISCARD)]);
    assert_eq!(outcomes[0].outcome.status, OpStatus::Succeeded);
    assert!(fx.service.documents().info(&id).is_err(), "closed");
    assert!(fx.restart().recovery_entries().unwrap().is_empty(), "journal removed");
}

#[test]
fn a_failed_delete_resumes_journaling_of_a_preserved_buffer() {
    // MOCKED trash failure.
    let fx = fixture();
    let id = fx.service.open_document(&wp("main.typ")).unwrap().state.info.id;
    edit(&fx.service, &id, "UNSAVED ");
    let plan = fx.service.plan_paths(&delete(false), &[path("main.typ")], &OpActor::User);
    let outcomes = apply(&fx, &plan, &FakeTrash { fail: true, ..FakeTrash::default() }, &[]);
    assert_eq!(outcomes[0].outcome.status, OpStatus::Failed);
    assert!(!fx.service.is_suspended(&id));
    fx.service.journal_documents(&[id.clone()].into());
    assert_eq!(fx.restart().recovery_entries().unwrap().len(), 1, "still crash-protected");
}

#[test]
fn copying_a_dirty_source_copies_its_saved_version_and_says_so() {
    let fx = fixture();
    let id = fx.service.open_document(&wp("chapters/intro.typ")).unwrap().state.info.id;
    edit(&fx.service, &id, "UNSAVED ");
    let plan = fx.service.plan_paths(&OpKind::Duplicate, &[path("chapters/intro.typ")], &OpActor::User);
    let notice = plan.impacts.iter().find(|i| i.kind == ImpactKind::SavedVersionCopied).expect("notice");
    assert!(!notice.kind.needs_decision());
    assert!(plan.impacts.iter().all(|i| !i.kind.needs_decision()));
    let outcomes = apply(&fx, &plan, &FakeTrash::default(), &[]);
    assert_eq!(outcomes[0].outcome.created, Some(path("chapters/intro (2).typ")));
    assert_eq!(fx.read("chapters/intro (2).typ"), "intro\n", "saved bytes, not the buffer");
    assert!(fx.service.documents().state(&id).unwrap().text.starts_with("UNSAVED"), "buffer untouched, not saved");
    assert_eq!(fx.read("chapters/intro.typ"), "intro\n", "the source was not saved silently");
}

// ------------------------------------------------------ copy / paste ------

#[test]
fn duplicates_get_readable_collision_free_names_and_never_overwrite() {
    let fx = fixture();
    let bin = FakeTrash::default();
    for expected in ["figures/plot (2).png", "figures/plot (3).png"] {
        let plan = fx.service.plan_paths(&OpKind::Duplicate, &[path("figures/plot.png")], &OpActor::User);
        let outcomes = apply(&fx, &plan, &bin, &[]);
        assert_eq!(outcomes[0].outcome.created, Some(path(expected)));
    }
    // Duplicating a copy continues its counter.
    let plan = fx.service.plan_paths(&OpKind::Duplicate, &[path("figures/plot (2).png")], &OpActor::User);
    assert_eq!(apply(&fx, &plan, &bin, &[])[0].outcome.created, Some(path("figures/plot (4).png")));
    // A FILE named like the folder copy is never replaced (std::fs::rename would).
    fs::write(fx.file("data (2)"), "a file in the way").unwrap();
    let plan = fx.service.plan_paths(&OpKind::Duplicate, &[path("data")], &OpActor::User);
    let outcomes = apply(&fx, &plan, &bin, &[]);
    assert_eq!(outcomes[0].outcome.created, Some(path("data (3)")));
    assert_eq!(fx.read("data (2)"), "a file in the way");
    assert_eq!(fx.read("data (3)/raw/a.csv"), "x,y\n1,2\n");
    // An existing empty folder is never merged into.
    fs::create_dir(fx.file("data (4)")).unwrap();
    let outcomes = apply(&fx, &fx.service.plan_paths(&OpKind::Duplicate, &[path("data")], &OpActor::User), &bin, &[]);
    assert_eq!(outcomes[0].outcome.created, Some(path("data (5)")));
    assert!(fs::read_dir(fx.file("data (4)")).unwrap().next().is_none());
    // Originals unchanged.
    assert_eq!(fx.read("figures/plot.png"), "PNG");
    assert_eq!(fx.read("data/raw/a.csv"), "x,y\n1,2\n");
}

#[test]
fn paste_keeps_the_name_in_another_folder_and_counts_up_in_the_same_one() {
    let fx = fixture();
    let bin = FakeTrash::default();
    let plan = fx.service.plan_paths(
        &paste_into(Some("chapters")),
        &[path("main.typ"), path("data/notes.md"), path("chapters/intro.typ")],
        &OpActor::User,
    );
    let outcomes = apply(&fx, &plan, &bin, &[]);
    let created: Vec<_> = outcomes.iter().map(|o| o.outcome.created.clone().unwrap()).collect();
    assert_eq!(created, [path("chapters/main.typ"), path("chapters/notes.md"), path("chapters/intro (2).typ")]);
    // Two sources with the same name into one folder.
    fs::write(fx.file("figures/notes.md"), "other notes").unwrap();
    let plan =
        fx.service.plan_paths(&paste_into(None), &[path("data/notes.md"), path("figures/notes.md")], &OpActor::User);
    let planned: Vec<_> = plan.effects.iter().map(|e| e.destination.clone().unwrap()).collect();
    assert_eq!(planned, [path("notes.md"), path("notes (2).md")]);
    let outcomes = apply(&fx, &plan, &bin, &[]);
    assert_eq!(fx.read("notes.md"), "notes\n");
    assert_eq!(fx.read("notes (2).md"), "other notes");
    assert_eq!(outcomes.len(), 2);
}

#[test]
fn a_folder_is_never_copied_into_itself() {
    let fx = fixture();
    for dest in ["data", "data/raw", "DATA/Raw"] {
        let plan = fx.service.plan_paths(&paste_into(Some(dest)), &[path("data")], &OpActor::User);
        assert_eq!(code_of(&plan, &path("data")).as_deref(), Some("ops.intoItself"), "into {dest}");
        apply(&fx, &plan, &FakeTrash::default(), &[]);
    }
    // Through a junction inside the project that leads into the folder.
    junction(&fx.file("alias"), &fx.file("data/raw"));
    let plan = fx.service.plan_paths(&paste_into(Some("alias")), &[path("data")], &OpActor::User);
    assert_eq!(code_of(&plan, &path("data")).as_deref(), Some("ops.intoItself"));
    let names: Vec<_> = fs::read_dir(fx.file("data/raw")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names.len(), 1, "nothing created: {names:?}");
}

#[test]
fn links_are_never_followed_or_copied() {
    let fx = fixture();
    junction(&fx.file("escape"), &fx.outside());
    fs::create_dir_all(fx.file("box/sub")).unwrap();
    fs::write(fx.file("box/sub/own.txt"), "own").unwrap();
    junction(&fx.file("box/sub/out"), &fx.outside());
    // A selected link is refused.
    let plan = fx.service.plan_paths(&OpKind::Duplicate, &[path("escape")], &OpActor::User);
    assert_eq!(code_of(&plan, &path("escape")).as_deref(), Some("ops.linkNotCopied"));
    // Paths through a link that leaves the project are outside it.
    let plan = fx.service.plan_paths(&paste_into(None), &[path("escape/secret.txt")], &OpActor::User);
    assert_eq!(code_of(&plan, &path("escape/secret.txt")).as_deref(), Some("ops.outsideProject"));
    let plan = fx.service.plan_paths(&paste_into(Some("escape")), &[path("main.typ")], &OpActor::User);
    assert_eq!(code_of(&plan, &path("main.typ")).as_deref(), Some("ops.outsideProject"));
    assert!(!fx.outside().join("main.typ").exists());
    // A folder with a link inside is copied without it, and the outcome says so.
    let plan = fx.service.plan_paths(&OpKind::Duplicate, &[path("box")], &OpActor::User);
    assert!(plan.effects[0].description.contains("1 link inside is not copied"), "{:?}", plan.effects);
    let outcomes = apply(&fx, &plan, &FakeTrash::default(), &[]);
    let o = &outcomes[0].outcome;
    assert_eq!(o.status, OpStatus::Succeeded);
    assert_eq!(o.code.as_deref(), Some("ops.linkNotCopied"));
    assert!(o.message.as_deref().unwrap().contains("sub/out"), "{o:?}");
    assert_eq!(fx.read("box (2)/sub/own.txt"), "own");
    assert!(!fx.exists("box (2)/sub/out"), "the link was not copied");
    assert!(!fx.file("box (2)/sub/secret.txt").exists());
}

#[test]
fn concurrent_pastes_never_take_the_same_name() {
    let fx = fixture();
    let service = Arc::clone(&fx.service);
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let service = Arc::clone(&service);
            std::thread::spawn(move || {
                let plan =
                    service.plan_paths(&paste_into(Some("figures")), &[path("figures/plot.png")], &OpActor::User);
                let choices = BTreeMap::new();
                let tag = format!("c{n}");
                service.apply_paths(
                    &plan,
                    &ApplyOptions { choices: &choices, trash: &FakeTrash::default(), skip: &[], tag: &tag },
                    &mut |_| {},
                )
            })
        })
        .collect();
    let mut created: Vec<String> = threads
        .into_iter()
        .map(|t| {
            let outcomes = t.join().unwrap();
            assert_eq!(outcomes[0].outcome.status, OpStatus::Succeeded, "{:?}", outcomes[0].outcome);
            match outcomes[0].outcome.created.clone().unwrap() {
                OpTarget::Path { path } => path.to_string(),
                other => panic!("{other:?}"),
            }
        })
        .collect();
    created.sort();
    created.dedup();
    assert_eq!(created.len(), 8, "eight distinct copies: {created:?}");
    for c in &created {
        assert_eq!(fx.read(c), "PNG");
    }
    let entries = fs::read_dir(fx.file("figures")).unwrap().count();
    assert_eq!(entries, 9, "original + 8 copies, no staging leftovers");
}

#[test]
fn external_sources_are_explicit_read_only_paste_inputs() {
    let fx = fixture();
    let secret = fx.outside().join("secret.txt").display().to_string();
    let ext = OpTarget::External { path: secret.clone() };
    // Pasted by the user: copied in, the source untouched.
    let plan = fx.service.plan_paths(&paste_into(Some("data")), std::slice::from_ref(&ext), &OpActor::User);
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    let outcomes = apply(&fx, &plan, &FakeTrash::default(), &[]);
    assert_eq!(outcomes[0].outcome.created, Some(path("data/secret.txt")));
    assert_eq!(fs::read_to_string(&secret).unwrap(), "secret");
    // Never a delete/duplicate target; never for agents.
    let plan = fx.service.plan_paths(&delete(true), std::slice::from_ref(&ext), &OpActor::User);
    assert_eq!(code_of(&plan, &ext).as_deref(), Some("ops.notSupported"));
    assert!(fx.outside().join("secret.txt").exists());
    let plan = fx.service.plan_paths(&OpKind::Duplicate, std::slice::from_ref(&ext), &OpActor::User);
    assert_eq!(code_of(&plan, &ext).as_deref(), Some("ops.notSupported"));
    let plan =
        fx.service.plan_paths(&paste_into(None), std::slice::from_ref(&ext), &OpActor::Agent { task: "t".into() });
    assert_eq!(code_of(&plan, &ext).as_deref(), Some("ops.agentScope"));
    // Exposition's own data folder is never a source.
    fs::create_dir_all(fx.data()).unwrap();
    fs::write(fx.data().join("settings.json"), "{}").unwrap();
    for p in [fx.data(), fx.data().join("settings.json")] {
        let t = OpTarget::External { path: p.display().to_string() };
        let plan = fx.service.plan_paths(&paste_into(None), std::slice::from_ref(&t), &OpActor::User);
        assert_eq!(code_of(&plan, &t).as_deref(), Some("ops.protected"), "{}", p.display());
    }
    // A folder that contains the project is never copied into it.
    let t = OpTarget::External { path: fx.dir.path().display().to_string() };
    let plan = fx.service.plan_paths(&paste_into(None), std::slice::from_ref(&t), &OpActor::User);
    assert!(code_of(&plan, &t).is_some_and(|c| c == "ops.intoItself" || c == "ops.protected"), "{:?}", plan.refusals);
    // Relative or missing paths are not sources.
    for raw in ["relative/x.txt", ""] {
        let t = OpTarget::External { path: raw.into() };
        let plan = fx.service.plan_paths(&paste_into(None), std::slice::from_ref(&t), &OpActor::User);
        assert_eq!(code_of(&plan, &t).as_deref(), Some("ops.notFound"));
    }
    // A link given as an external source is not followed.
    junction(&fx.dir.path().join("outlink"), &fx.outside());
    let t = OpTarget::External { path: fx.dir.path().join("outlink").display().to_string() };
    let plan = fx.service.plan_paths(&paste_into(None), std::slice::from_ref(&t), &OpActor::User);
    assert_eq!(code_of(&plan, &t).as_deref(), Some("ops.linkNotCopied"));
    // An external path inside the project is an ordinary project source.
    let t = OpTarget::External { path: fx.file("main.typ").display().to_string() };
    let plan = fx.service.plan_paths(&paste_into(Some("chapters")), &[t], &OpActor::User);
    assert_eq!(plan.copied, [wp("main.typ")]);
}

#[test]
fn the_fingerprint_changes_when_a_target_changes() {
    let fx = fixture();
    let before = fx.service.plan_paths(&delete(false), &[path("data")], &OpActor::User).fingerprint;
    assert_eq!(before, fx.service.plan_paths(&delete(false), &[path("data")], &OpActor::User).fingerprint);
    fs::write(fx.file("data/raw/new.csv"), "new").unwrap();
    let after = fx.service.plan_paths(&delete(false), &[path("data")], &OpActor::User).fingerprint;
    assert_ne!(before, after, "a file added to a folder being deleted is material");
    let id = fx.service.open_document(&wp("data/notes.md")).unwrap().state.info.id;
    edit(&fx.service, &id, "x");
    let dirty = fx.service.plan_paths(&delete(false), &[path("data")], &OpActor::User).fingerprint;
    assert_ne!(after, dirty, "a buffer becoming dirty is material");
}

#[test]
fn staging_leftovers_of_an_interrupted_copy_are_removed_by_tag() {
    let fx = fixture();
    let staged = fsops::staging_path(&fs::canonicalize(fx.file("figures")).unwrap(), "op9");
    fs::create_dir(&staged).unwrap();
    fs::write(staged.join("half.png"), "half").unwrap();
    let other = fsops::staging_path(&fs::canonicalize(fx.file("figures")).unwrap(), "op8");
    fs::write(&other, "x").unwrap();
    let removed = fx.service.remove_staging_leftovers(&[fs::canonicalize(fx.file("figures")).unwrap()], "op9");
    assert_eq!(removed, 1);
    assert!(!staged.exists() && other.exists(), "only that operation's leftovers");
}
