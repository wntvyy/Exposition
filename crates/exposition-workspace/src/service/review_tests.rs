//! Independent review of the workspace performance lane (WS-1/WS-9c journal
//! staging, the `journaled` map, WS-5 watcher intake, WS-6 disk checks, WS-3
//! root accessor). Tests only; nothing here is product code.
//!
//! The journal tests compare the optimized path (journal staged outside the
//! I/O lock, installed under it) with the write under the lock from the
//! current state, which is what the base revision did: two services over two
//! folders receive the same operations; one stages before the operations and
//! installs after them, the other journals under the lock at the same point.

use std::collections::BTreeMap;

use exposition_core::document::TextEncoding;

use super::*;

const INITIAL: &str = "= Title\n\nBody.\n";

struct World {
    _dir: tempfile::TempDir,
    project: PathBuf,
    data: PathBuf,
    service: Arc<WorkspaceService>,
    id: DocumentId,
    ids: HashSet<DocumentId>,
    counter: u32,
    /// Text of every revision seen between operations.
    texts: HashMap<u64, String>,
}

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let data = dir.path().join("data");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("main.typ"), INITIAL).unwrap();
        let service = WorkspaceService::new(Arc::new(DocumentService::new()), data.clone());
        service.open_workspace(&project).unwrap();
        let id = service.open_document(&wp("main.typ")).unwrap().state.info.id;
        let mut world = Self {
            _dir: dir,
            project,
            data,
            service,
            ids: HashSet::from([id.clone()]),
            id,
            counter: 0,
            texts: HashMap::new(),
        };
        world.remember();
        world
    }

    fn remember(&mut self) {
        if let Ok(state) = self.service.documents().state(&self.id) {
            self.texts.insert(state.info.revision, state.text);
        }
    }

    fn path(&self) -> Option<WorkspacePath> {
        self.service.documents().info(&self.id).ok().and_then(|i| i.path)
    }

    fn op(&mut self, op: Op) {
        self.counter += 1;
        let n = self.counter;
        let service = Arc::clone(&self.service);
        let id = self.id.clone();
        match op {
            Op::Edit => {
                if let Ok(info) = service.documents().info(&id) {
                    let _ = service.documents().apply(TransactionRequest {
                        id: TransactionId(format!("review-{n}")),
                        origin: EditOrigin::Source,
                        edits: vec![DocumentEdit {
                            document: id.clone(),
                            base_revision: info.revision,
                            patches: vec![TextPatch { range: TextRange::new(0, 0), text: format!("e{n} ") }],
                        }],
                        undo: UndoBehavior::NewGroup,
                        conflict: ConflictPolicy::Reject,
                        label: None,
                        selection: None,
                    });
                }
            }
            Op::Undo => {
                let _ = service.documents().undo(&id, EditOrigin::Source);
            }
            Op::Save => {
                let _ = service.save_document(&id, false);
            }
            Op::SaveOverwrite => {
                let _ = service.save_document(&id, true);
            }
            Op::ExternalChange => {
                if let Some(path) = self.path() {
                    fs::write(self.project.join(path.as_str()), format!("disk {n}\n")).unwrap();
                    service.check_document(&id);
                }
            }
            Op::ExternalDelete => {
                if let Some(path) = self.path() {
                    let _ = fs::remove_file(self.project.join(path.as_str()));
                    service.check_document(&id);
                }
            }
            Op::KeepBuffer => {
                let _ = service.keep_buffer(&id);
            }
            Op::Reload => {
                let _ = service.reload_document(&id);
            }
            Op::Rename => {
                if let Some(path) = self.path() {
                    let to = if path.as_str() == "main.typ" { "renamed.typ" } else { "main.typ" };
                    let _ = service.rename_entry(&path, to);
                }
            }
            Op::Suspend => {
                let _ = service.suspend_document(&id);
            }
            Op::Resume => {
                if let Some(path) = self.path() {
                    service.resume_documents_under(&path);
                }
            }
            Op::Journal => service.journal_documents(&self.ids),
            Op::Encoding => {
                if let Ok(info) = service.documents().info(&id) {
                    let next =
                        if info.encoding == TextEncoding::Utf8 { TextEncoding::Utf8Bom } else { TextEncoding::Utf8 };
                    let _ = service.documents().set_encoding(&id, next);
                }
            }
            Op::Close => {
                let _ = service.close_document(&id);
            }
            Op::Reopen => {
                service.close_workspace();
                service.open_workspace(&self.project).unwrap();
            }
        }
        self.remember();
    }

    fn install(&self, staged: HashMap<DocumentId, StagedJournal>) {
        let _io = lock(&self.service.io);
        self.service.install_journals(&self.ids, staged);
    }

    /// Every file of the recovery folder: name (the time stamp of a `rec-`
    /// name left out) and, for journals, their content without `savedAt`.
    #[allow(clippy::type_complexity)]
    fn recovery(&self) -> BTreeMap<String, Option<(String, Option<String>, u64, TextEncoding, String)>> {
        let mut out = BTreeMap::new();
        let Ok(store) = self.service.recovery_store() else { return out };
        let Ok(read) = fs::read_dir(store.dir()) else { return out };
        let mut paths: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let key = match name.strip_prefix("rec-") {
                Some(rest) => {
                    let stem = format!("rec-{}", rest.split('-').next().unwrap_or(rest));
                    let ordinal = out.keys().filter(|k: &&String| k.starts_with(&stem)).count();
                    format!("{stem}#{ordinal}")
                }
                None => name.clone(),
            };
            let journal = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<crate::recovery::Journal>(&bytes).ok())
                .map(|j| (j.path.as_str().to_owned(), j.base_hash, j.revision, j.encoding, j.text));
            out.insert(key, journal);
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Edit,
    Undo,
    Save,
    SaveOverwrite,
    ExternalChange,
    ExternalDelete,
    KeepBuffer,
    Reload,
    Rename,
    Suspend,
    Resume,
    Journal,
    Encoding,
    Close,
    Reopen,
}

const OPS: &[Op] = &[
    Op::Edit,
    Op::Undo,
    Op::Save,
    Op::SaveOverwrite,
    Op::ExternalChange,
    Op::ExternalDelete,
    Op::KeepBuffer,
    Op::Reload,
    Op::Rename,
    Op::Suspend,
    Op::Resume,
    Op::Journal,
    Op::Encoding,
    Op::Close,
    Op::Reopen,
];

/// One scenario: `prefix`, a journal staged, `middle`, the staged journal
/// installed (optimized) or the journal written under the lock (reference).
/// With `second`, another journal is staged after `middle`, `second` runs,
/// the later one is installed first and the earlier one after it.
fn check_scenario(prefix: &[Op], middle: &[Op], second: Option<&[Op]>) {
    let label = format!("prefix {prefix:?}, staged, {middle:?}, second {second:?}");
    let mut staged = World::new();
    let mut reference = World::new();
    for op in prefix {
        staged.op(*op);
        reference.op(*op);
    }
    let first = staged.service.stage_journals(&staged.ids);
    for op in middle {
        staged.op(*op);
        reference.op(*op);
    }
    if let Some(second) = second {
        let later = staged.service.stage_journals(&staged.ids);
        for op in second {
            staged.op(*op);
            reference.op(*op);
        }
        staged.install(later);
    }
    staged.install(first);
    reference.install(HashMap::new());

    // Staging has no effect on the documents.
    let a = staged.service.documents().state(&staged.id).ok().map(|s| (s.info.revision, s.info.saved_revision, s.text));
    let b = reference
        .service
        .documents()
        .state(&reference.id)
        .ok()
        .map(|s| (s.info.revision, s.info.saved_revision, s.text));
    assert_eq!(a, b, "{label}: documents differ");

    let optimized = staged.recovery();
    let plain = reference.recovery();
    assert!(optimized.keys().all(|name| !name.ends_with(".tmp")), "{label}: temporary file left: {optimized:?}");
    assert_eq!(
        optimized.keys().collect::<Vec<_>>(),
        plain.keys().collect::<Vec<_>>(),
        "{label}: a journal exists in one case and not in the other\noptimized {optimized:?}\nreference {plain:?}"
    );
    let mut behind = false;
    for (name, journal) in &optimized {
        let expected = &plain[name];
        let (Some(journal), Some(expected)) = (journal, expected) else {
            assert_eq!(journal, expected, "{label}: {name}");
            continue;
        };
        // Same file, base, and encoding as a write under the lock.
        assert_eq!((&journal.0, &journal.1, journal.3), (&expected.0, &expected.1, expected.3), "{label}: {name}");
        // The text of one revision of the document: the current one, or the
        // staged one while edits made since still wait for the next tick.
        assert!(journal.2 <= expected.2, "{label}: {name} holds revision {} > {}", journal.2, expected.2);
        assert_eq!(
            staged.texts.get(&journal.2),
            Some(&journal.4),
            "{label}: {name} does not hold the text of revision {}",
            journal.2
        );
        if journal.2 < expected.2 {
            behind = true;
            let current = a.as_ref().map(|s| s.0);
            assert_eq!(current, Some(expected.2), "{label}: the reference journal is the current revision");
        } else {
            assert_eq!(journal.4, expected.4, "{label}: {name}");
        }
    }
    // The next tick (the edits made after staging announced themselves):
    // exactly the journal of a write under the lock.
    staged.service.journal_documents(&staged.ids);
    reference.service.journal_documents(&reference.ids);
    assert_eq!(staged.recovery(), reference.recovery(), "{label}: after the next tick (was behind: {behind})");

    // What a kill now leaves for the next session is the same in both.
    {
        let restart = |world: &World| {
            let service = WorkspaceService::new(Arc::new(DocumentService::new()), world.data.clone());
            service.open_workspace(&world.project).unwrap();
            let mut entries: Vec<(Option<String>, u32, bool)> = service
                .recovery_entries()
                .unwrap()
                .into_iter()
                .map(|e| (e.path.map(|p| p.as_str().to_owned()), e.length, e.problem.is_some()))
                .collect();
            entries.sort();
            let names: Vec<String> = fs::read_dir(service.recovery_store().unwrap().dir())
                .map(|read| read.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
            entries
        };
        assert_eq!(restart(&staged), restart(&reference), "{label}: recovery offered after a kill");
    }
}

/// Every operation sequence of length 0, 1 and 2 between staging and
/// installing, from four starting states.
#[test]
fn review_staged_journal_equals_the_locked_write_for_every_short_interleaving() {
    let prefixes: [&[Op]; 4] = [
        &[Op::Edit],
        &[Op::Edit, Op::Journal, Op::Edit],
        &[Op::Edit, Op::Save, Op::Edit],
        &[Op::ExternalDelete, Op::Edit],
    ];
    for prefix in prefixes {
        check_scenario(prefix, &[], None);
        for a in OPS {
            check_scenario(prefix, &[*a], None);
            for b in OPS {
                check_scenario(prefix, &[*a, *b], None);
            }
        }
    }
}

/// Longer random sequences, and two staged journals installed newest first.
#[test]
fn review_staged_journal_equals_the_locked_write_for_random_interleavings() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut below = |n: usize| {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as usize % n
    };
    for round in 0..260 {
        let mut sequence = |max: usize| -> Vec<Op> {
            let n = below(max + 1);
            (0..n).map(|_| if below(3) == 0 { Op::Edit } else { OPS[below(OPS.len())] }).collect()
        };
        let mut prefix = vec![Op::Edit];
        prefix.extend(sequence(3));
        let middle = sequence(4);
        let second = sequence(3);
        check_scenario(&prefix, &middle, (round % 2 == 0).then_some(second.as_slice()));
    }
}

/// A journal staged before a save must not be installed after that save.
/// The check is "newer than the saved revision", and a file deleted on disk
/// sets the saved revision back to 0: stage (revision 2), type (3), save (3),
/// the file is deleted (saved 0, base none) - the staged journal of revision
/// 2 passes every check and is installed although revision 3 was saved since.
#[test]
#[ignore = "review: defect (WS-1): a journal staged before a save is installed after it when the saved file was then deleted; until the next tick the journal holds text older than the last save"]
fn review_a_journal_staged_before_a_save_is_not_installed_after_the_saved_file_was_deleted() {
    let mut world = World::new();
    world.op(Op::ExternalDelete);
    world.op(Op::Edit);
    let staged = world.service.stage_journals(&world.ids);
    assert_eq!(staged.len(), 1);
    world.op(Op::Edit);
    world.service.save_document(&world.id, false).unwrap();
    let saved = world.service.documents().state(&world.id).unwrap();
    assert_eq!(fs::read_to_string(world.project.join("main.typ")).unwrap(), saved.text);
    assert!(world.recovery().keys().all(|name| name.ends_with(".tmp")), "saved: no journal, only the staged file");
    world.op(Op::ExternalDelete);
    world.install(staged);
    let files = world.recovery();
    assert_eq!(files.len(), 1, "the buffer is the only copy again: {files:?}");
    let journal = files.values().next().unwrap().as_ref().unwrap();
    assert_eq!(
        (journal.2, journal.4.as_str()),
        (saved.info.revision, saved.text.as_str()),
        "the journal holds the text of revision {} from before the save of revision {}",
        journal.2,
        saved.info.revision
    );
}

/// A kill between the complete write of the staged file and its rename
/// (the file stays behind, as nothing runs the destructor): the next session
/// offers exactly the journal that was in place, and the leftover is removed.
#[test]
fn review_a_kill_before_the_rename_leaves_the_previous_journal_and_no_leftover() {
    for with_previous in [false, true] {
        let mut world = World::new();
        world.op(Op::Edit);
        let journaled = world.service.documents().state(&world.id).unwrap().text;
        if with_previous {
            world.op(Op::Journal);
        }
        world.op(Op::Edit);
        let staged = world.service.stage_journals(&world.ids);
        assert_eq!(staged.len(), 1);
        let dir = world.service.recovery_store().unwrap().dir().to_path_buf();
        let temps = || -> Vec<PathBuf> {
            fs::read_dir(&dir)
                .map(|r| r.flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(".tmp")).collect())
                .unwrap_or_default()
        };
        assert_eq!(temps().len(), 1, "the staged journal is a temporary file next to its place");
        // The staged file is complete and flushed: it parses as the journal of the current revision.
        let parsed: crate::recovery::Journal = serde_json::from_slice(&fs::read(&temps()[0]).unwrap()).unwrap();
        assert_eq!(parsed.text, world.service.documents().state(&world.id).unwrap().text);
        std::mem::forget(staged);
        // Kill: a new session over the same data.
        let service = WorkspaceService::new(Arc::new(DocumentService::new()), world.data.clone());
        service.open_workspace(&world.project).unwrap();
        assert!(temps().is_empty(), "the leftover is removed when the folder opens: {:?}", temps());
        let entries = service.recovery_entries().unwrap();
        if with_previous {
            assert_eq!(entries.len(), 1, "{entries:?}");
            assert_eq!(service.restore_recovery(&entries[0].id).unwrap().state.text, journaled);
        } else {
            assert!(entries.is_empty(), "a temporary file is never taken for a journal: {entries:?}");
        }
    }
}

/// The recovery folder cannot be written: the failure is reported, the
/// document is untouched, and journaling works again once it can.
#[test]
fn review_a_journal_that_cannot_be_written_is_reported_and_retried() {
    let mut world = World::new();
    let dir = world.service.recovery_store().unwrap().dir().to_path_buf();
    fs::create_dir_all(dir.parent().unwrap()).unwrap();
    // A file where the folder should be.
    fs::write(&dir, b"in the way").unwrap();
    let mut events = world.service.subscribe();
    world.op(Op::Edit);
    world.service.journal_documents(&world.ids);
    let mut failed = false;
    while let Ok(event) = events.try_recv() {
        failed |= matches!(event, WorkspaceEvent::JournalFailed { .. });
    }
    assert!(failed, "the failure is reported");
    assert_eq!(world.service.journaled_revision(&world.id), None, "no journal is believed to exist");
    fs::remove_file(&dir).unwrap();
    world.service.journal_documents(&world.ids);
    let files = world.recovery();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(
        files.values().next().unwrap().as_ref().unwrap().4,
        world.service.documents().state(&world.id).unwrap().text
    );

    // The rename fails (a folder sits at the journal's place): reported, the
    // staged file does not stay behind.
    let live = fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
    fs::remove_file(&live).unwrap();
    fs::create_dir(&live).unwrap();
    world.op(Op::Edit);
    world.service.journal_documents(&world.ids);
    let mut failed = false;
    while let Ok(event) = events.try_recv() {
        failed |= matches!(event, WorkspaceEvent::JournalFailed { .. });
    }
    assert!(failed, "a failed rename is reported");
    let names: Vec<String> =
        fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 1, "only the folder in the way remains: {names:?}");
}

/// Threads: typing, journal ticks, and saves at the same time. Afterwards the
/// journal is exactly what the final state calls for, whatever happened.
#[test]
fn review_concurrent_typing_journaling_and_saving_end_in_the_right_journal() {
    for round in 0..6 {
        let world = World::new();
        let stop = Arc::new(AtomicBool::new(false));
        let typing = {
            let (service, id, stop) = (Arc::clone(&world.service), world.id.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut n = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    let Ok(info) = service.documents().info(&id) else { break };
                    let _ = service.documents().apply(TransactionRequest {
                        id: TransactionId(format!("t-{n}")),
                        origin: EditOrigin::Source,
                        edits: vec![DocumentEdit {
                            document: id.clone(),
                            base_revision: info.revision,
                            patches: vec![TextPatch { range: TextRange::new(0, 0), text: format!("{n} ") }],
                        }],
                        undo: UndoBehavior::Merge { key: "typing".into() },
                        conflict: ConflictPolicy::Reject,
                        label: None,
                        selection: None,
                    });
                    n += 1;
                    if n.is_multiple_of(16) {
                        std::thread::yield_now();
                    }
                }
            })
        };
        let journaling: Vec<_> = (0..2)
            .map(|_| {
                let (service, ids, stop) = (Arc::clone(&world.service), world.ids.clone(), Arc::clone(&stop));
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        service.journal_documents(&ids);
                    }
                })
            })
            .collect();
        let saving = {
            let (service, id, stop) = (Arc::clone(&world.service), world.id.clone(), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = service.save_document(&id, true);
                    std::thread::sleep(Duration::from_millis(3));
                }
            })
        };
        std::thread::sleep(Duration::from_millis(250));
        stop.store(true, Ordering::Relaxed);
        typing.join().unwrap();
        for thread in journaling {
            thread.join().unwrap();
        }
        saving.join().unwrap();
        // The last save may have been the final word, or typing was.
        if round % 2 == 0 {
            world.service.save_document(&world.id, true).unwrap();
        }
        world.service.journal_documents(&world.ids);
        let state = world.service.documents().state(&world.id).unwrap();
        let files = world.recovery();
        if state.info.saved_revision == Some(state.info.revision) {
            assert!(files.is_empty(), "round {round}: a saved document has no journal: {:?}", files.keys());
        } else {
            assert_eq!(files.len(), 1, "round {round}: {:?}", files.keys());
            let journal = files.values().next().unwrap().as_ref().expect("a readable journal");
            assert_eq!((journal.2, &journal.4), (state.info.revision, &state.text), "round {round}");
            assert_eq!(journal.1, world.service.disk_base(&world.id).unwrap(), "round {round}");
        }
        assert!(files.keys().all(|n| !n.ends_with(".tmp")), "round {round}: {:?}", files.keys());
    }
}

// ----- WS-3 ---------------------------------------------------------------------

#[test]
fn review_open_display_root_is_the_root_of_the_workspace_info_in_every_state() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("Project With Space");
    fs::create_dir_all(project.join("sub")).unwrap();
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    assert_eq!(service.open_display_root(), None);
    assert!(service.current().is_none());
    let spellings = [
        project.clone(),
        PathBuf::from(format!("{}{}", project.display(), std::path::MAIN_SEPARATOR)),
        project.join("sub").join(".."),
        PathBuf::from(project.to_string_lossy().to_uppercase()),
        fs::canonicalize(&project).unwrap(),
    ];
    for spelling in spellings {
        if !spelling.exists() {
            continue;
        }
        let info = service.open_workspace(&spelling).unwrap();
        assert_eq!(service.open_display_root().as_deref(), Some(info.root.as_str()), "{spelling:?}");
        assert_eq!(service.current().map(|c| c.root), service.open_display_root());
        assert_eq!(service.check_workspace(&spelling).unwrap(), info.root);
        service.close_workspace();
        assert_eq!(service.open_display_root(), None);
        assert!(service.current().is_none());
    }
}

// ----- WS-5 ---------------------------------------------------------------------

fn modify_event(path: PathBuf) -> notify::Result<Event> {
    Ok(Event::new(EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content))).add_path(path))
}

async fn wait_for_text(service: &WorkspaceService, id: &DocumentId, expected: &str, seconds: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if service.documents().state(id).is_ok_and(|s| s.text == expected) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Overflow, then again overflow while the first rescan is still waiting,
/// then a third burst: every change made to an open document during any of
/// them is picked up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_repeated_overflows_never_lose_a_change_to_an_open_document() {
    let world = World::new();
    let service = Arc::clone(&world.service);
    let root = service.open_root().unwrap();
    tokio::spawn(Arc::clone(&service).run());
    for burst in 0..3 {
        let expected = format!("burst {burst}\n");
        for i in 0..FS_BACKLOG_LIMIT + 2_000 {
            service.inject_fs_event(modify_event(root.join(format!("data/f{i}.csv"))));
            if i == FS_BACKLOG_LIMIT + 1_000 {
                // Changed while notifications are being dropped; its own notification is dropped too.
                fs::write(world.project.join("main.typ"), &expected).unwrap();
                service.inject_fs_event(modify_event(root.join("main.typ")));
            }
        }
        assert!(wait_for_text(&service, &world.id, &expected, 20).await, "burst {burst}: the change was lost");
    }
}

/// The overflow state belongs to the intake, not to a project: an overflow
/// of the project that was open before must not swallow the rescan that a
/// notification dropped for the project open now needs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "review: defect (WS-5): a notification of the newly opened project dropped while the backlog of the previous project is over the limit is reported by no rescan"]
async fn review_an_overflow_of_the_previous_project_does_not_hide_a_dropped_notification_of_the_next() {
    let world = World::new();
    let service = Arc::clone(&world.service);
    let first_root = service.open_root().unwrap();
    // The loop is busy (not started yet here): the backlog of the first project runs over.
    for i in 0..FS_BACKLOG_LIMIT + 10 {
        service.inject_fs_event(modify_event(first_root.join(format!("data/f{i}.csv"))));
    }
    // Another project is opened.
    let second = world._dir.path().join("second");
    fs::create_dir_all(&second).unwrap();
    fs::write(second.join("main.typ"), "second\n").unwrap();
    service.open_workspace(&second).unwrap();
    let id = service.open_document(&wp("main.typ")).unwrap().state.info.id;
    let root = service.open_root().unwrap();
    let mut events = service.subscribe();
    // Its file changes on disk; the notification arrives while the backlog is still full.
    fs::write(second.join("main.typ"), "second, changed on disk\n").unwrap();
    let before = service.fs_signals_queued();
    service.inject_fs_event(modify_event(root.join("main.typ")));
    let dropped = service.fs_signals_queued() == before;
    tokio::spawn(Arc::clone(&service).run());
    let reloaded = wait_for_text(&service, &id, "second, changed on disk\n", 5).await;
    let mut rescan = false;
    while let Ok(event) = events.try_recv() {
        rescan |= matches!(event, WorkspaceEvent::FilesChanged { rescan: true, .. });
    }
    assert!(
        reloaded || rescan,
        "the notification was dropped without a signal ({dropped}) and no rescan was reported: the open document still shows the old text"
    );
}

/// The filter of the watcher callback never drops a notification that the
/// batch processing would have reported: every event shape over ignored and
/// visible paths, compared with what `process_fs` reports for it alone.
#[test]
fn review_the_callback_filter_drops_only_what_processing_would_not_report() {
    let world = World::new();
    let service = &world.service;
    let root = service.open_root().unwrap();
    let display = PathBuf::from(service.open_display_root().unwrap());
    fs::create_dir_all(world.project.join("target/debug")).unwrap();
    fs::create_dir_all(world.project.join("chapters")).unwrap();
    fs::create_dir_all(world.project.join(".exposition/graph-backups")).unwrap();
    let relative = [
        "",
        "main.typ",
        "chapters",
        "chapters/one.typ",
        "target",
        "target/debug",
        "target/debug/x.rlib",
        "targets/a.txt",
        "chapters/target.typ",
        ".git",
        ".git/index",
        ".gitignore",
        "node_modules/p/i.js",
        ".lake/build/A.olean",
        ".exposition",
        ".exposition/project.json",
        ".exposition/graph.sqlite3-wal",
        ".exposition/graph-backups",
        ".exposition/graph-backups/b.sqlite3",
        ".Exposition/Graph.sqlite3",
        "chapters/.one.typ.exposition-save-1.tmp",
        "sub/.exposition/graph.sqlite3",
        "TARGET/x",
        ".GIT/index",
    ];
    let kinds = [
        EventKind::Any,
        EventKind::Other,
        EventKind::Create(notify::event::CreateKind::Any),
        EventKind::Create(notify::event::CreateKind::Folder),
        EventKind::Remove(notify::event::RemoveKind::Any),
        EventKind::Modify(ModifyKind::Any),
        EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
        EventKind::Modify(ModifyKind::Name(RenameMode::From)),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
        EventKind::Access(notify::event::AccessKind::Any),
    ];
    let mut checked = 0;
    let mut dropped = 0;
    for base in [&root, &display] {
        let filter = WatchFilter::new(&lock(&service.state).open.as_ref().unwrap().root);
        let generation = lock(&service.state).open.as_ref().unwrap().generation;
        for kind in kinds {
            for first in relative {
                for second in [None, Some("main.typ"), Some("target/out.pdf"), Some(".git/x"), Some("chapters/new.typ")]
                {
                    for flag in [false, true] {
                        let mut event =
                            Event::new(kind).add_path(if first.is_empty() { base.clone() } else { base.join(first) });
                        if let Some(second) = second {
                            event = event.add_path(base.join(second));
                        }
                        if flag {
                            event = event.set_flag(notify::event::Flag::Rescan);
                        }
                        let mut events = service.subscribe();
                        service.process_fs(vec![FsSignal { generation, event: Ok(event.clone()) }]);
                        let reported = events.try_recv().is_ok();
                        checked += 1;
                        if filter.ignores(&event) {
                            dropped += 1;
                            assert!(!reported, "dropped by the callback, but processing reports it: {event:?}");
                        }
                    }
                }
            }
        }
    }
    assert!(checked > 5_000 && dropped > 500, "{checked} checked, {dropped} dropped");
}

// ----- WS-6 ---------------------------------------------------------------------

/// The disk check compares the hash of the file's bytes: a file replaced by
/// other bytes that decode to the same text (a byte-order mark added), by
/// different text of the same length, or by the same bytes again.
#[test]
fn review_the_disk_check_sees_every_change_of_bytes_and_nothing_else() {
    let mut world = World::new();
    let file = world.project.join("main.typ");
    let service = Arc::clone(&world.service);
    let revision = |s: &WorkspaceService| s.documents().info(&world.id).unwrap().revision;
    // The same bytes written again (new time stamp): nothing happens.
    fs::write(&file, INITIAL).unwrap();
    service.check_document(&world.id);
    assert_eq!(revision(&service), 1);
    assert!(lock(&service.state).open.as_ref().unwrap().conflicts.is_empty());
    // Other text of the same length.
    let same_length = INITIAL.replace("Body", "Soul");
    assert_eq!(same_length.len(), INITIAL.len());
    fs::write(&file, &same_length).unwrap();
    service.check_document(&world.id);
    assert_eq!(service.documents().state(&world.id).unwrap().text, same_length);
    // The same text with a byte-order mark: other bytes, another encoding.
    let mut with_bom = vec![0xEF, 0xBB, 0xBF];
    with_bom.extend_from_slice(same_length.as_bytes());
    fs::write(&file, &with_bom).unwrap();
    service.check_document(&world.id);
    let info = service.documents().info(&world.id).unwrap();
    assert_eq!(info.encoding, TextEncoding::Utf8Bom, "the encoding follows the file");
    assert_eq!(info.saved_revision, Some(info.revision));
    // Saving writes the mark back: the file is unchanged by the save.
    world.op(Op::Edit);
    service.save_document(&world.id, false).unwrap();
    assert!(fs::read(&file).unwrap().starts_with(&[0xEF, 0xBB, 0xBF]));
    // The app's own save, then an external change before the watcher batch is
    // processed: the check sees the external bytes, not its own save.
    world.op(Op::Edit);
    service.save_document(&world.id, false).unwrap();
    fs::write(&file, "external after the save\n").unwrap();
    service.check_document(&world.id);
    assert_eq!(service.documents().state(&world.id).unwrap().text, "external after the save\n");
    // Dirty buffer and an external change: a conflict, the buffer is kept.
    world.op(Op::Edit);
    let mine = service.documents().state(&world.id).unwrap().text;
    fs::write(&file, "external while dirty\n").unwrap();
    service.check_document(&world.id);
    assert_eq!(service.documents().state(&world.id).unwrap().text, mine);
    assert_eq!(lock(&service.state).open.as_ref().unwrap().conflicts.len(), 1);
}

/// Changed-documents scan: the same documents as the pairwise comparison of
/// the base revision, for random change lists (files, folders above,
/// siblings that share a prefix, other spellings).
#[test]
fn review_the_changed_documents_scan_selects_what_the_pairwise_comparison_selected() {
    let world = World::new();
    let service = &world.service;
    let mut ids = Vec::new();
    for path in ["a/b/c.typ", "a/b2/c.typ", "a.typ", "x/y/z/deep.typ", "ab/c.typ"] {
        let absolute = world.project.join(path);
        fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        fs::write(&absolute, "one\n").unwrap();
        ids.push((path, service.open_document(&wp(path)).unwrap().state.info.id));
    }
    let candidates = [
        "a",
        "a/b",
        "a/b2",
        "a/",
        "ab",
        "x",
        "x/y",
        "x/y/z",
        "x/y/z/deep.typ",
        "A",
        "a/B",
        "a.typ",
        "a/b/c.typ/x",
        "x/y/zz",
        "main.typ",
    ];
    let mut seed = 99u64;
    let mut below = |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize % n
    };
    for round in 0..200 {
        let changes: Vec<FileChange> = (0..below(4))
            .filter_map(|_| WorkspacePath::parse(candidates[below(candidates.len())]).ok())
            .map(|path| FileChange { path, kind: FileChangeKind::Modified })
            .collect();
        // Every file gets new content, so a checked document reloads (its revision moves).
        for (path, _) in &ids {
            fs::write(world.project.join(path), format!("round {round}\n")).unwrap();
        }
        let before: Vec<u64> = ids.iter().map(|(_, id)| service.documents().info(id).unwrap().revision).collect();
        service.check_changed_documents(&changes, false);
        for (index, (path, id)) in ids.iter().enumerate() {
            let expected =
                changes.iter().any(|c| c.path.as_str() == *path || path.starts_with(&format!("{}/", c.path.as_str())));
            let moved = service.documents().info(id).unwrap().revision != before[index];
            assert_eq!(moved, expected, "round {round}: {path} with changes {changes:?}");
        }
        // Bring every document level with its file again.
        service.check_changed_documents(&[], true);
    }
}
