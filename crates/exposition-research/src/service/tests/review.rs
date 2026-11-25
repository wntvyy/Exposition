//! Independent review of the research performance lane: released run
//! state, the run record written outside the run table's lock, and
//! one-at-a-time interpreter discovery.

use std::sync::Condvar;
use std::sync::atomic::AtomicUsize;

use super::*;
use crate::types::OutputEntry;

fn manifest_file(root: &Path, id: &str) -> PathBuf {
    root.join(".exposition").join("runs").join(format!("{id}.json"))
}

fn transcript_file(root: &Path, id: &str) -> PathBuf {
    root.join(".exposition").join("artifacts").join(id).join("transcript.json")
}

/// Leave the project and come back: every finished, stored run is released.
fn release_all(service: &ResearchService) -> PathBuf {
    let root = service.workspace_root().unwrap();
    service.clear_workspace();
    service.set_workspace(&root, true).unwrap();
    root
}

fn released(service: &ResearchService, id: &str) -> bool {
    lock(&service.shared.runs)[id].released.is_some()
}

/// Every page of a run's output, read the way the webview reads it.
fn all_pages(service: &ResearchService, id: &str) -> Result<(Vec<OutputEntry>, RunOutputPage), ResearchError> {
    let mut after = None;
    let mut entries = Vec::new();
    loop {
        let page = service.run_output(id, after, 500)?;
        entries.extend(page.entries.iter().cloned());
        if page.entries.len() < 500 {
            return Ok((entries, page));
        }
        after = page.entries.last().map(|e| e.seq);
    }
}

// ---- item 4: released runs ----------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn review_output_larger_than_a_page_reads_the_same_after_release() {
    let f = Fixture::new();
    let service = &f._service;
    let id = queued_run(&f, 0, 10);
    for n in 0..1200 {
        let text = format!("line {n}\n");
        f.session.push_output(&service.shared, &id, OutputItem::Stream { name: StreamName::Stdout, text });
    }
    f.session.finish_run(&id, RunStatus::Succeeded, Some(1), None, false);
    let (before, last_before) = all_pages(service, &id).unwrap();
    assert_eq!(before.len(), 1203);
    let manifest = service.run_manifest(&id).unwrap();
    release_all(service);
    assert!(released(service, &id));
    let (after, last_after) = all_pages(service, &id).unwrap();
    assert_eq!(after, before);
    assert_eq!(
        (last_after.first_retained_seq, last_after.last_seq, last_after.truncated),
        (last_before.first_retained_seq, last_before.last_seq, last_before.truncated)
    );
    assert_eq!(service.run_manifest(&id).unwrap(), manifest);
}

#[tokio::test(flavor = "multi_thread")]
async fn review_output_that_outgrew_retention_reads_the_same_after_release() {
    let f = Fixture::new();
    let service = &f._service;
    let id = queued_run(&f, 0, 10);
    // 40 x 100 KiB: more than is retained, so the oldest entries are dropped.
    for n in 0..40 {
        let text = format!("{n} {}\n", "z".repeat(100 * 1024));
        f.session.push_output(&service.shared, &id, OutputItem::Stream { name: StreamName::Stdout, text });
    }
    f.session.finish_run(&id, RunStatus::Succeeded, Some(1), None, false);
    let before = service.run_output(&id, None, 500).unwrap();
    assert!(before.truncated && before.first_retained_seq > 1, "the fixture must exceed retention");
    let tail = service.run_output(&id, Some(before.last_seq - 2), 500).unwrap();
    release_all(service);
    assert!(released(service, &id));
    assert_eq!(service.run_output(&id, None, 500).unwrap(), before);
    assert_eq!(service.run_output(&id, Some(before.last_seq - 2), 500).unwrap(), tail);
}

#[tokio::test(flavor = "multi_thread")]
async fn review_a_released_run_whose_manifest_is_gone_or_damaged_is_an_error() {
    let f = Fixture::new();
    let service = &f._service;
    let ids: Vec<String> = (0..4).map(|n| finished_run(&f, n, 50)).collect();
    let root = release_all(service);
    // Missing.
    std::fs::remove_file(manifest_file(&root, &ids[0])).unwrap();
    // Cut short.
    let bytes = std::fs::read(manifest_file(&root, &ids[1])).unwrap();
    std::fs::write(manifest_file(&root, &ids[1]), &bytes[..bytes.len() / 2]).unwrap();
    // Another run's record under this run's name.
    std::fs::copy(manifest_file(&root, &ids[3]), manifest_file(&root, &ids[2])).unwrap();
    for id in &ids[..3] {
        assert!(service.run_manifest(id).is_err(), "{id}");
        assert!(service.run_output(id, None, 500).is_err(), "{id}");
    }
    // The history still names them (their summaries are kept), so the error is what the person sees on selecting one.
    let listed = service.runs().unwrap();
    for id in &ids {
        assert!(listed.iter().any(|s| s.id.0 == *id), "{id}");
    }
    assert_eq!(service.run_output(&ids[3], None, 500).unwrap().entries.len(), 3);
}

/// A released run whose transcript is missing or unreadable: its output was
/// on screen from memory before the release (and would have stayed there in
/// the base version for the rest of the session). After the release the
/// read succeeds with no entries and no truncation mark, exactly like a run
/// that printed nothing, although the manifest records three entries.
#[tokio::test(flavor = "multi_thread")]
async fn review_a_released_run_whose_transcript_is_gone_or_damaged_is_not_shown_as_no_output() {
    let f = Fixture::new();
    let service = &f._service;
    let ids: Vec<String> = (0..3).map(|n| finished_run(&f, n, 50)).collect();
    for id in &ids {
        assert_eq!(service.run_output(id, None, 500).unwrap().entries.len(), 3);
    }
    let root = release_all(service);
    std::fs::remove_file(transcript_file(&root, &ids[0])).unwrap();
    let bytes = std::fs::read(transcript_file(&root, &ids[1])).unwrap();
    std::fs::write(transcript_file(&root, &ids[1]), &bytes[..bytes.len() / 2]).unwrap();
    std::fs::write(transcript_file(&root, &ids[2]), b"{\"not\": \"a transcript\"}").unwrap();
    for id in &ids {
        assert_eq!(service.run_manifest(id).unwrap().output.entries, 3, "the record says there was output");
        match service.run_output(id, None, 500) {
            Err(_) => {}
            Ok(page) => assert!(
                !page.entries.is_empty() || page.truncated,
                "{id}: lost output reads as an empty, complete page: {page:?}"
            ),
        }
    }
}

/// Output that arrives for a run after it finished (a notice pushed by a
/// cancel or interrupt request that raced with the end of the run, or late
/// kernel output) is kept in memory and shown, but it is not in the
/// transcript: it disappears when the run is released. In the base version
/// it stayed visible for the whole session.
#[tokio::test(flavor = "multi_thread")]
async fn review_output_recorded_after_the_finish_survives_the_release() {
    let f = Fixture::new();
    let service = &f._service;
    let id = finished_run(&f, 0, 50);
    f.session.push_output(&service.shared, &id, OutputItem::Notice { message: "Cancel requested.".into() });
    let before = service.run_output(&id, None, 500).unwrap();
    assert_eq!(before.entries.len(), 4, "the late entry is shown while the run is held in memory");
    release_all(service);
    assert!(released(service, &id));
    assert_eq!(service.run_output(&id, None, 500).unwrap(), before, "the same output after the release");
}

#[tokio::test(flavor = "multi_thread")]
async fn review_readers_racing_with_releases_and_project_switches_never_see_wrong_output() {
    let f = Fixture::new();
    let service = &f._service;
    let root = service.workspace_root().unwrap();
    let mut expected = Vec::new();
    for n in 0..12 {
        let id = finished_run(&f, n, 200);
        expected.push((id.clone(), service.run_manifest(&id).unwrap(), service.run_output(&id, None, 500).unwrap()));
    }
    let more: Vec<String> = (12..60).map(|n| queued_run(&f, n, 200)).collect();
    let stop = AtomicBool::new(false);
    let switching = AtomicBool::new(false);
    let reads = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    for (id, manifest, page) in &expected {
                        // While the project is being closed and reopened a read may find it
                        // closed (asked after the read: the flag only ever turns on).
                        let may_be_closed = || switching.load(Ordering::SeqCst);
                        match service.run_output(id, None, 500) {
                            Ok(read) => assert_eq!(&read, page, "{id}"),
                            Err(ResearchError::NoWorkspace | ResearchError::UnknownRun(_)) if may_be_closed() => {}
                            Err(e) => panic!("{id}: {e}"),
                        }
                        match service.run_manifest(id) {
                            Ok(read) => assert_eq!(&read, manifest, "{id}"),
                            Err(ResearchError::NoWorkspace | ResearchError::UnknownRun(_)) if may_be_closed() => {}
                            Err(e) => panic!("{id}: {e}"),
                        }
                        reads.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
        // Finishing more runs releases the older ones under the readers.
        for id in &more {
            f.session.finish_run(id, RunStatus::Succeeded, None, None, false);
        }
        // Then the project is closed and opened repeatedly.
        switching.store(true, Ordering::SeqCst);
        for _ in 0..40 {
            service.clear_workspace();
            service.set_workspace(&root, true).unwrap();
        }
        let seen = reads.load(Ordering::SeqCst);
        while reads.load(Ordering::SeqCst) < seen + 24 {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::SeqCst);
    });
    for (id, manifest, page) in &expected {
        assert!(released(service, id));
        assert_eq!(&service.run_output(id, None, 500).unwrap(), page);
        assert_eq!(&service.run_manifest(id).unwrap(), manifest);
    }
    assert_eq!(service.runs().unwrap().len(), 60);
}

/// Deleting (recoverably), restoring, and deleting for good a released run.
#[tokio::test(flavor = "multi_thread")]
async fn review_a_released_run_can_be_deleted_and_restored() {
    let f = Fixture::new();
    let service = &f._service;
    let ids: Vec<String> = (0..3).map(|n| finished_run(&f, n, 50)).collect();
    let page = service.run_output(&ids[0], None, 500).unwrap();
    release_all(service);
    service.delete_run_record(&ids[0], false).unwrap();
    assert!(!service.runs().unwrap().iter().any(|s| s.id.0 == ids[0]), "a deleted run leaves the history");
    assert!(service.run_manifest(&ids[0]).is_err());
    service.restore_run_record(&ids[0]).unwrap();
    assert!(service.runs().unwrap().iter().any(|s| s.id.0 == ids[0]));
    assert_eq!(service.run_output(&ids[0], None, 500).unwrap(), page);
    service.delete_run_record(&ids[1], true).unwrap();
    assert!(service.run_manifest(&ids[1]).is_err());
    assert!(service.run_output(&ids[1], None, 500).is_err());
}

// ---- item 5: the record is written outside the run table's lock ----------

/// While runs finish concurrently (some of them finished twice at once, as a
/// cancel racing the kernel does), a run that no longer reads as executing
/// always has its final record on disk, every run finishes exactly once,
/// and the announced summary is the stored one.
#[tokio::test(flavor = "multi_thread")]
async fn review_a_run_reads_as_finished_only_with_its_final_record_on_disk() {
    for round in 0..6 {
        let mut f = Fixture::new();
        let service = &f._service;
        let root = service.workspace_root().unwrap();
        let ids: Vec<String> = (0..24).map(|n| queued_run(&f, n, 2000)).collect();
        let stop = AtomicBool::new(false);
        let checks = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    while !stop.load(Ordering::SeqCst) {
                        for id in &ids {
                            if service.run_active(id) {
                                continue;
                            }
                            // No longer executing: the stored record is final and complete.
                            let stored = read_manifest(&root, id)
                                .unwrap_or_else(|e| panic!("round {round}: {id} finished without a record: {e}"));
                            assert_ne!(stored.record.status, RunStatus::Running, "round {round}: {id}");
                            assert!(stored.record.finished_at_ms.is_some(), "round {round}: {id}");
                            assert!(store::read_transcript(&root, id).len() >= 3, "round {round}: {id}");
                            let live = service.run_manifest(id).unwrap();
                            assert_eq!(live.record.status, stored.record.status, "round {round}: {id}");
                            checks.fetch_add(1, Ordering::SeqCst);
                        }
                        // The history can always be listed, and never shows a run twice.
                        let listed = service.runs().unwrap();
                        assert_eq!(listed.len(), ids.len());
                    }
                });
            }
            let finishers: Vec<_> = (0..4)
                .map(|t| {
                    let session = f.session.clone();
                    let mine: Vec<String> = ids.iter().skip(t).step_by(4).cloned().collect();
                    scope.spawn(move || {
                        for id in &mine {
                            session.finish_run(id, RunStatus::Succeeded, Some(1), None, false);
                        }
                    })
                })
                .collect();
            // A second finish of the same runs at the same time (in reverse).
            let session = f.session.clone();
            let again: Vec<String> = ids.iter().rev().cloned().collect();
            let racer = scope.spawn(move || {
                for id in &again {
                    session.finish_run(id, RunStatus::Canceled, None, Some("Canceled."), false);
                }
            });
            for finisher in finishers {
                finisher.join().unwrap();
            }
            racer.join().unwrap();
            // Every finish call has returned. A finish that found the record
            // being written returned at once, so wait for the writers.
            let deadline = Instant::now() + Duration::from_secs(20);
            while ids.iter().any(|id| service.run_active(id)) {
                assert!(Instant::now() < deadline, "round {round}: a run stayed executing");
                std::thread::yield_now();
            }
            let seen = checks.load(Ordering::SeqCst);
            while checks.load(Ordering::SeqCst) < seen + 2 * ids.len() {
                std::thread::yield_now();
            }
            stop.store(true, Ordering::SeqCst);
        });
        let mut finished: HashMap<String, Vec<RunSummary>> = HashMap::new();
        loop {
            match f.events.try_recv() {
                Ok(ResearchEvent::RunFinished { run }) => finished.entry(run.id.0.clone()).or_default().push(run),
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(e) => panic!("events: {e}"),
            }
        }
        for id in &ids {
            let announced = finished.get(id).unwrap_or_else(|| panic!("round {round}: {id} was never announced"));
            assert_eq!(announced.len(), 1, "round {round}: {id} announced {} times", announced.len());
            let stored = read_manifest(&root, id).unwrap();
            assert_eq!(announced[0].status, stored.record.status, "round {round}: {id}");
            assert_eq!(announced[0].finished_at_ms, stored.record.finished_at_ms, "round {round}: {id}");
            // The transcript holds exactly what the run holds in memory (or held, if released).
            let run_state_entries = lock(&service.shared.runs)[id.as_str()].entries.len();
            let transcript = store::read_transcript(&root, id).len();
            if !released(service, id) {
                assert_eq!(transcript, run_state_entries, "round {round}: {id}");
            }
            assert_eq!(stored.output.entries as usize, transcript, "round {round}: {id}");
        }
    }
}

/// A record that cannot be written: the run still finishes (it does not stay
/// executing), is announced once, and its output stays readable from memory.
#[tokio::test(flavor = "multi_thread")]
async fn review_a_run_whose_record_cannot_be_written_still_finishes_and_is_announced() {
    let mut f = Fixture::new();
    let service = &f._service;
    let id = queued_run(&f, 0, 100);
    let blocked = f._dir.path().join("not-a-folder");
    std::fs::write(&blocked, "x").unwrap();
    let real = std::mem::replace(&mut lock(&service.shared.runs).get_mut(&id).unwrap().root, blocked);
    f.session.finish_run(&id, RunStatus::Succeeded, None, None, false);
    assert!(!service.run_active(&id), "the run must not stay executing");
    lock(&service.shared.runs).get_mut(&id).unwrap().root = real;
    let mut announced = 0;
    while let Ok(event) = f.events.try_recv() {
        if matches!(&event, ResearchEvent::RunFinished { run } if run.id.0 == id) {
            announced += 1;
        }
    }
    assert_eq!(announced, 1);
    release_all(service);
    assert!(!released(service, &id), "the only copy is never released");
    assert_eq!(service.run_output(&id, None, 500).unwrap().entries.len(), 3);
    assert_eq!(service.run_manifest(&id).unwrap().record.status, RunStatus::Succeeded);
}

// ---- item 9: one discovery at a time --------------------------------------

/// A launcher whose probes can be held, counted, and made to fail.
struct GateLauncher {
    probes: AtomicUsize,
    hold: Mutex<bool>,
    released: Condvar,
    panic_once: AtomicBool,
    dir: PathBuf,
}

impl GateLauncher {
    fn probe(&self) -> Result<ProbeOutput, LaunchError> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        let mut hold = lock(&self.hold);
        while *hold {
            hold = self.released.wait(hold).unwrap_or_else(|p| p.into_inner());
        }
        Err(LaunchError::Refused("no interpreter answers in this test".into()))
    }
    fn set_hold(&self, hold: bool) {
        *lock(&self.hold) = hold;
        self.released.notify_all();
    }
    fn count(&self) -> usize {
        self.probes.load(Ordering::SeqCst)
    }
}

impl KernelProcessLauncher for GateLauncher {
    fn launch_interactive(&self, _spec: JobSpec) -> LaunchFuture<'_> {
        Box::pin(async move { Err(LaunchError::Refused("no sessions in this test".into())) })
    }
    fn run_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        self.probe()
    }
    fn run_project_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        self.probe()
    }
    fn probe_dir(&self) -> Option<PathBuf> {
        if self.panic_once.swap(false, Ordering::SeqCst) {
            panic!("review: discovery fails here once");
        }
        Some(self.dir.clone())
    }
    fn workspace_trusted(&self, _root: &Path) -> Option<bool> {
        Some(true)
    }
}

struct Discovery {
    _dir: tempfile::TempDir,
    launcher: Arc<GateLauncher>,
    service: Arc<ResearchService>,
}

/// A trusted project with a project interpreter file, so that every
/// discovery probes at least once whatever is installed on the machine.
fn discovery() -> Discovery {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    let (bin, exe) = if cfg!(windows) { ("Scripts", "python.exe") } else { ("bin", "python") };
    std::fs::create_dir_all(root.join(".venv").join(bin)).unwrap();
    std::fs::write(root.join(".venv").join(bin).join(exe), b"not an interpreter").unwrap();
    let probe_dir = dir.path().join("probe");
    std::fs::create_dir_all(&probe_dir).unwrap();
    let launcher = Arc::new(GateLauncher {
        probes: AtomicUsize::new(0),
        hold: Mutex::new(false),
        released: Condvar::new(),
        panic_once: AtomicBool::new(false),
        dir: probe_dir,
    });
    let config = ResearchConfig::new(dir.path().join("data"), launcher.clone(), Arc::new(DocumentService::new()));
    let service = Arc::new(ResearchService::new(config));
    service.set_workspace(&root, true).unwrap();
    Discovery { _dir: dir, launcher, service }
}

impl Discovery {
    /// Probes one whole discovery makes on this machine.
    async fn probes_per_discovery(&self) -> usize {
        let before = self.launcher.count();
        self.service.environments(true).await.unwrap();
        let n = self.launcher.count() - before;
        assert!(n > 0, "the fixture must make discovery probe");
        n
    }

    fn request(&self, refresh: bool) -> tokio::task::JoinHandle<Result<EnvironmentReport, ResearchError>> {
        let service = self.service.clone();
        tokio::spawn(async move { service.environments(refresh).await })
    }

    /// Wait until a held discovery has started probing and its probe count is stable.
    async fn wait_probing(&self, more_than: usize) -> usize {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.launcher.count() <= more_than {
            assert!(Instant::now() < deadline, "no discovery started probing");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.launcher.count()
    }
}

const PATIENCE: Duration = Duration::from_secs(60);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_requests_during_a_discovery_wait_for_it_and_a_refresh_probes_again() {
    let d = discovery();
    let per = d.probes_per_discovery().await;
    // Cached: no probe.
    let cached = d.service.environments(false).await.unwrap();
    assert_eq!(d.launcher.count(), per);
    // Forget the report (as a trust change or project switch does), then
    // three requests arrive while the first discovery is held.
    *lock(&d.service.shared.environments) = None;
    d.launcher.set_hold(true);
    let first = d.request(false);
    let held = d.wait_probing(per).await;
    let second = d.request(false);
    let blocking = {
        let service = d.service.clone();
        tokio::task::spawn_blocking(move || service.environments_blocking())
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(d.launcher.count(), held, "requests that arrive during a discovery do not probe");
    d.launcher.set_hold(false);
    let first = tokio::time::timeout(PATIENCE, first).await.unwrap().unwrap().unwrap();
    let second = tokio::time::timeout(PATIENCE, second).await.unwrap().unwrap().unwrap();
    let blocking = tokio::time::timeout(PATIENCE, blocking).await.unwrap().unwrap().unwrap();
    assert_eq!(d.launcher.count(), 2 * per, "one discovery answered all three");
    // The very same report (one scan), listing what the earlier scan listed.
    assert_eq!(second, first);
    assert_eq!(blocking, first);
    assert_eq!(first.runtimes, cached.runtimes);
    // A refresh that arrives during a discovery waits, then probes itself.
    *lock(&d.service.shared.environments) = None;
    d.launcher.set_hold(true);
    let first = d.request(false);
    let held = d.wait_probing(2 * per).await;
    let refresh = d.request(true);
    let rediscover = {
        let service = d.service.clone();
        tokio::task::spawn_blocking(move || service.rediscover_blocking())
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(d.launcher.count(), held);
    d.launcher.set_hold(false);
    tokio::time::timeout(PATIENCE, first).await.unwrap().unwrap().unwrap();
    tokio::time::timeout(PATIENCE, refresh).await.unwrap().unwrap().unwrap();
    tokio::time::timeout(PATIENCE, rediscover).await.unwrap().unwrap().unwrap();
    assert_eq!(d.launcher.count(), 5 * per, "the first discovery, the refresh, and the rediscovery each probed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_a_cancelled_first_discovery_does_not_hang_or_answer_the_next_request() {
    let d = discovery();
    let per = d.probes_per_discovery().await;
    *lock(&d.service.shared.environments) = None;
    d.launcher.set_hold(true);
    let first = d.request(false);
    d.wait_probing(per).await;
    let second = d.request(false);
    let blocking = {
        let service = d.service.clone();
        tokio::task::spawn_blocking(move || service.environments_blocking())
    };
    // The first request is dropped (its caller went away) while its probes are still running.
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    assert!(d.service.cached_environments().is_none(), "a cancelled discovery leaves no report");
    d.launcher.set_hold(false);
    let second = tokio::time::timeout(PATIENCE, second).await.expect("the second request hung").unwrap().unwrap();
    let blocking = tokio::time::timeout(PATIENCE, blocking).await.expect("the blocking request hung").unwrap().unwrap();
    assert_eq!(second, blocking);
    assert_eq!(d.service.cached_environments(), Some(second));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_a_failed_first_discovery_does_not_poison_the_next_request() {
    let d = discovery();
    let per = d.probes_per_discovery().await;
    *lock(&d.service.shared.environments) = None;
    // The async discovery panics while it holds its turn.
    d.launcher.panic_once.store(true, Ordering::SeqCst);
    let first = d.request(false);
    assert!(first.await.unwrap_err().is_panic());
    assert!(d.service.cached_environments().is_none(), "a failed discovery leaves no report");
    let second =
        tokio::time::timeout(PATIENCE, d.request(false)).await.expect("hung after a failure").unwrap().unwrap();
    assert_eq!(d.launcher.count(), 2 * per);
    // The blocking discovery panics while it holds its turn.
    *lock(&d.service.shared.environments) = None;
    d.launcher.panic_once.store(true, Ordering::SeqCst);
    let service = d.service.clone();
    let blocking = tokio::task::spawn_blocking(move || service.environments_blocking());
    assert!(blocking.await.unwrap_err().is_panic());
    let third = tokio::time::timeout(PATIENCE, d.request(false)).await.expect("hung after a failure").unwrap().unwrap();
    assert_eq!(third.runtimes, second.runtimes);
    let service = d.service.clone();
    let fourth = tokio::time::timeout(PATIENCE, tokio::task::spawn_blocking(move || service.rediscover_blocking()))
        .await
        .expect("hung after a failure")
        .unwrap()
        .unwrap();
    assert_eq!(fourth.runtimes, second.runtimes);
    // A report for data that cannot be prepared is an error, and the next request works.
    assert_eq!(d.launcher.count(), 4 * per);
}

// ---- review: display artifacts of released runs ---------------------------
//
// Independent review of "a display artifact registered for an already
// released research run is written to that run's stored manifest". Tests
// marked `#[ignore = "review finding: ..."]` fail against the change and
// document a finding; every other test passes.

/// A finished, stored run of the fixture's session with its output folder
/// set (as a dispatched run has), not released yet.
fn stored_run_with_folder(f: &Fixture, n: usize, chars: usize) -> (String, PathBuf) {
    let service = &f._service;
    let root = service.workspace_root().unwrap();
    let id = queued_run(f, n, chars);
    let dir = ensure_dir_within(&root, &artifact_dir_path(&id)).unwrap();
    lock(&service.shared.runs).get_mut(&id).unwrap().output_dir = Some(dir.clone());
    f.session.finish_run(&id, RunStatus::Succeeded, Some(1), None, false);
    (id, dir)
}

/// The kernel displays `bytes` as `name` in the run's folder (a late
/// display, after the run finished).
fn late_display(session: &Arc<Session>, id: &str, dir: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(dir.join(name), bytes).unwrap();
    session.on_event(HelperEvent::Display {
        run_id: id.to_owned(),
        output_type: "display_data".into(),
        execution_count: Some(1),
        text: None,
        files: vec![crate::protocol::HelperFile {
            path: dir.join(name).display().to_string(),
            name: name.into(),
            media_type: "image/png".into(),
            role: "image".into(),
            size: bytes.len() as u64,
            sha256: sha256_hex(bytes),
            seq: 1,
            truncated: false,
        }],
    });
}

fn png(tag: &str) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

fn deleted_file(root: &Path, id: &str) -> PathBuf {
    root.join(".exposition").join("runs").join("deleted").join(format!("{id}.json"))
}

/// A service started afresh on `root` (nothing in memory).
fn fresh_service(f: &Fixture, root: &Path, name: &str) -> ResearchService {
    let fresh = ResearchService::new(ResearchConfig::new(
        f._dir.path().join(name),
        f.launcher.clone(),
        Arc::new(DocumentService::new()),
    ));
    fresh.set_workspace(root, true).unwrap();
    fresh
}

/// Every artifact a display entry of the run's stored output names is one
/// its stored manifest holds.
fn displays_name_only_recorded_artifacts(service: &ResearchService, root: &Path, id: &str) -> usize {
    let outputs = read_manifest(root, id).unwrap().record.outputs;
    let (entries, _) = all_pages(service, id).unwrap();
    let mut named = 0;
    for entry in &entries {
        if let OutputItem::Display { artifacts, .. } = &entry.item {
            for artifact in artifacts {
                assert!(outputs.iter().any(|o| &o.id == artifact), "{artifact:?} is not in the record");
                named += 1;
            }
        }
    }
    named
}

/// A run released by a project switch gets a late display while another
/// project is open: the artifact goes to the run's own project (its record
/// and folder), nothing is written into the open project, and the history
/// of each project is what a fresh service lists.
#[tokio::test(flavor = "multi_thread")]
async fn review_a_late_artifact_after_a_project_switch_goes_to_its_own_project() {
    let f = Fixture::new();
    let service = &f._service;
    let a = service.workspace_root().unwrap();
    let (id, dir) = stored_run_with_folder(&f, 0, 10);
    let other = f._dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    service.set_workspace(&other, true).unwrap();
    let b = service.workspace_root().unwrap();
    assert!(released(service, &id));

    late_display(&f.session, &id, &dir, "output-1.png", &png("after the switch"));
    let stored = read_manifest(&a, &id).unwrap();
    assert_eq!(stored.record.outputs.len(), 1, "added to the run's record in its own project");
    assert!(!manifest_file(&b, &id).exists(), "no record written into the open project");
    assert!(!b.join(".exposition").join("artifacts").join(&id).exists(), "no folder in the open project");
    assert!(service.runs().unwrap().iter().all(|r| r.id.0 != id), "not in the open project's history");
    assert!(fresh_service(&f, &b, "fresh-b").runs().unwrap().is_empty());

    service.set_workspace(&a, true).unwrap();
    assert_eq!(displays_name_only_recorded_artifacts(service, &a, &id), 1);
    let fresh = fresh_service(&f, &a, "fresh-a");
    let listed = |s: &ResearchService| s.runs().unwrap().into_iter().find(|r| r.id.0 == id).unwrap();
    assert_eq!(listed(service).outputs, listed(&fresh).outputs, "the history a fresh service lists");
    assert_eq!(listed(service).status, listed(&fresh).status);
    assert_eq!(service.run_summary(&id).unwrap().outputs, stored.record.outputs);
}

/// A late display for a released run whose record was deleted: a
/// recoverably deleted record stays as it was and no record is put back in
/// the history (also none for a permanently deleted one); restored, the
/// record names no artifact and no display names one.
#[tokio::test(flavor = "multi_thread")]
async fn review_a_late_artifact_never_brings_back_a_deleted_record() {
    let f = Fixture::new();
    let service = &f._service;
    let (kept, kept_dir) = stored_run_with_folder(&f, 0, 10);
    let (gone, gone_dir) = stored_run_with_folder(&f, 1, 10);
    let root = release_all(service);
    assert!(released(service, &kept) && released(service, &gone));

    service.delete_run_record(&kept, false).unwrap();
    let before = std::fs::read(deleted_file(&root, &kept)).unwrap();
    late_display(&f.session, &kept, &kept_dir, "output-1.png", &png("deleted"));
    assert!(!manifest_file(&root, &kept).exists(), "the deleted record is not put back");
    assert_eq!(std::fs::read(deleted_file(&root, &kept)).unwrap(), before, "and is not changed");

    service.delete_run_record(&gone, true).unwrap();
    // The deletion removed the emptied folder; the kernel writes its figure anew.
    std::fs::create_dir_all(&gone_dir).unwrap();
    late_display(&f.session, &gone, &gone_dir, "output-1.png", &png("gone"));
    assert!(!manifest_file(&root, &gone).exists(), "a permanently deleted record stays deleted");
    assert!(!deleted_file(&root, &gone).exists());
    assert!(service.runs().unwrap().iter().all(|r| r.id.0 != kept), "a recoverably deleted run is not listed");
    let fresh = fresh_service(&f, &root, "fresh");
    assert!(fresh.runs().unwrap().iter().all(|r| r.id.0 != kept && r.id.0 != gone), "nothing stored lists them");

    service.restore_run_record(&kept).unwrap();
    assert!(read_manifest(&root, &kept).unwrap().record.outputs.is_empty());
    assert_eq!(displays_name_only_recorded_artifacts(service, &root, &kept), 0);
}

/// The same file displayed again (an updated figure under the same name):
/// one artifact per id in the record, the summary and a fresh service's
/// listing, with the content the file has now.
#[tokio::test(flavor = "multi_thread")]
async fn review_a_late_artifact_displayed_twice_is_recorded_once_with_its_last_content() {
    let f = Fixture::new();
    let service = &f._service;
    let (id, dir) = stored_run_with_folder(&f, 0, 10);
    let root = release_all(service);
    late_display(&f.session, &id, &dir, "output-1.png", &png("first"));
    late_display(&f.session, &id, &dir, "output-1.png", &png("second version"));
    let stored = read_manifest(&root, &id).unwrap();
    assert_eq!(stored.record.outputs.len(), 1, "{:?}", stored.record.outputs);
    assert_eq!(stored.record.outputs[0].content_hash, format!("sha256:{}", sha256_hex(&png("second version"))));
    assert_eq!(service.run_summary(&id).unwrap().outputs, stored.record.outputs);
    let fresh = fresh_service(&f, &root, "fresh");
    assert_eq!(fresh.run_summary(&id).unwrap().outputs, stored.record.outputs);
    let (record, file) = service.artifact(&id, &format!("{id}/output-1.png")).unwrap();
    assert_eq!(record, stored.record.outputs[0]);
    assert_eq!(std::fs::read(file).unwrap(), png("second version"));
    assert_eq!(displays_name_only_recorded_artifacts(service, &root, &id), 2);
}

/// Late displays and late stream output for the same released run, from
/// two threads at once (`late_writes` serializes the record's
/// read-modify-write): no artifact and no entry is lost, the manifest's
/// entry count is the transcript's, and the output reads back whole.
#[tokio::test(flavor = "multi_thread")]
async fn review_late_artifacts_and_late_output_at_once_lose_nothing() {
    let f = Fixture::new();
    let service = &f._service;
    let (id, dir) = stored_run_with_folder(&f, 0, 10);
    let root = release_all(service);
    let (figures, lines) = (24usize, 48usize);
    std::thread::scope(|s| {
        let session = f.session.clone();
        let session_a = f.session.clone();
        let (id_a, dir_a) = (id.clone(), dir.clone());
        s.spawn(move || {
            for k in 0..figures {
                late_display(&session_a, &id_a, &dir_a, &format!("output-{k}.png"), &png(&format!("figure {k}")));
            }
        });
        let id_b = id.clone();
        s.spawn(move || {
            for k in 0..lines {
                session.on_event(HelperEvent::Stream {
                    run_id: id_b.clone(),
                    name: "stdout".into(),
                    text: format!("late {k}\n"),
                });
            }
        });
    });
    let stored = read_manifest(&root, &id).unwrap();
    let ids: HashSet<&str> = stored.record.outputs.iter().map(|o| o.id.0.as_str()).collect();
    assert_eq!(ids.len(), figures, "every late artifact is in the record");
    let (entries, page) = all_pages(service, &id).unwrap();
    assert!(!page.truncated);
    assert_eq!(entries.len(), 3 + figures + lines, "every late entry is in the transcript");
    assert_eq!(u64::from(stored.output.entries), entries.len() as u64, "the manifest counts the transcript");
    assert_eq!(displays_name_only_recorded_artifacts(service, &root, &id), figures);
    let fresh = fresh_service(&f, &root, "fresh");
    assert_eq!(fresh.run_summary(&id).unwrap().outputs.len(), figures);
    assert_eq!(service.run_summary(&id).unwrap().outputs.len(), figures);
}

/// Not from this change (the history's in-memory runs are unchanged): a run
/// of this app session whose record is deleted permanently is still listed
/// by the history, from memory, until the app restarts (a recoverably
/// deleted one is left out); a fresh service does not list it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "review finding (also in the base version): a permanently deleted run of this app session is still listed by the history from memory"]
async fn review_a_permanently_deleted_run_leaves_the_history() {
    let f = Fixture::new();
    let service = &f._service;
    let (gone, _) = stored_run_with_folder(&f, 0, 10);
    let root = release_all(service);
    service.delete_run_record(&gone, true).unwrap();
    assert!(!manifest_file(&root, &gone).exists());
    assert!(fresh_service(&f, &root, "fresh").runs().unwrap().iter().all(|r| r.id.0 != gone));
    assert!(service.runs().unwrap().iter().all(|r| r.id.0 != gone), "listed from memory after a permanent deletion");
}

/// Late output racing a deletion of the run's record (the user deletes the
/// run while its kernel still sends output): `kind` 0 is a late display
/// with a figure (the record's manifest is rewritten by
/// `add_outputs_to_record`, new in this change, then the entry is appended
/// by `append_to_record`), 1 a late stream line (`append_to_record` only, as
/// in the base version). Each write checks "deleted?" and then reads,
/// changes and writes the record without the deletion waiting for it. Returns
/// how many of `runs` were deleted and yet have a record in the history again,
/// and how many have both a record and a recoverably deleted copy.
fn race_late_output_with_deletion(kind: u8, runs: usize) -> (Vec<String>, usize) {
    let f = Fixture::new();
    let service = &f._service;
    let made: Vec<(String, PathBuf)> = (0..runs).map(|n| stored_run_with_folder(&f, n, 60_000)).collect();
    let root = release_all(service);
    let mut random = 0x9E37_79B9_7F4A_7C15u64;
    let (mut resurrected, mut both) = (Vec::new(), 0);
    for (id, dir) in &made {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let delay = Duration::from_micros(random % 4000);
        let deleted = std::thread::scope(|s| {
            let deleter = s.spawn(|| {
                std::thread::sleep(delay);
                service.delete_run_record(id, false)
            });
            match kind {
                0 => late_display(&f.session, id, dir, "output-1.png", &png("racing")),
                _ => f.session.on_event(HelperEvent::Stream {
                    run_id: id.clone(),
                    name: "stdout".into(),
                    text: "late".into(),
                }),
            }
            deleter.join().unwrap()
        });
        if deleted.is_ok() && manifest_file(&root, id).exists() {
            resurrected.push(id.clone());
            if deleted_file(&root, id).exists() {
                both += 1;
            }
        }
    }
    (resurrected, both)
}

/// New in this change: a late display with a figure.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "review finding: a late artifact written to a released run's record can recreate a record deleted at the same moment (check-then-write not ordered with the deletion)"]
async fn review_a_late_artifact_racing_a_deletion_never_resurrects_the_record() {
    let (resurrected, both) = race_late_output_with_deletion(0, 40);
    assert!(
        resurrected.is_empty(),
        "{} of 40 deleted, then written back ({both} with a deleted copy too): {resurrected:?}",
        resurrected.len()
    );
}

/// The same race in the base version's path (a late stream line).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "review finding (also in the base version): late output appended to a released run's record can recreate a record deleted at the same moment"]
async fn review_late_output_racing_a_deletion_never_resurrects_the_record() {
    let (resurrected, both) = race_late_output_with_deletion(1, 40);
    assert!(
        resurrected.is_empty(),
        "{} of 40 deleted, then written back ({both} with a deleted copy too): {resurrected:?}",
        resurrected.len()
    );
}

/// An active workflow batch whose stored record cannot be read is dropped
/// from `active_run_records` (the reader the project operations use for
/// active research work) without an error.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "review finding: active_run_records drops an active batch whose record cannot be read instead of failing"]
async fn review_an_active_batch_whose_record_cannot_be_read_is_not_dropped() {
    let f = Fixture::new();
    let service = &f._service;
    let root = service.workspace_root().unwrap();
    let id = "run-00000000000000ab";
    std::fs::create_dir_all(root.join(".exposition").join("runs")).unwrap();
    std::fs::write(manifest_file(&root, id), b"{ damaged").unwrap();
    service.set_batch_live(id, true);
    assert!(service.active_run_ids().iter().any(|a| a == id), "the batch is active");
    match service.active_run_records() {
        Err(_) => {}
        Ok(records) => assert!(records.iter().any(|r| r.id == id), "an active batch vanished: {records:?}"),
    }
}
