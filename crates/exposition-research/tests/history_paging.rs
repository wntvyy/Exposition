//! The paged run history: every stored run is reachable, pages are stable
//! while runs are created and deleted, a page reads only the manifests it
//! returns, and damaged records neither end the listing nor falsify the count.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use exposition_core::artifact::{Producer, RunId, RunRecord, RunStatus, ToolVersion};
use exposition_core::job::JobSpec;
use exposition_research::launcher::{KernelProcessLauncher, LaunchError, LaunchFuture, ProbeOutput};
use exposition_research::store::write_manifest;
use exposition_research::types::{
    CodeOrigin, CodeProvenance, InterpreterProvenance, KernelDescriptor, OutputSummary, RunHistoryPage,
    RunHistoryRequest, RunManifest, SessionOwner, SessionProvenance,
};
use exposition_research::{HISTORY_CHANGED, HISTORY_PAGE, ResearchConfig, ResearchService};
use exposition_workspace::DocumentService;

struct NoLauncher;

impl KernelProcessLauncher for NoLauncher {
    fn launch_interactive(&self, _spec: JobSpec) -> LaunchFuture<'_> {
        Box::pin(async { Err(LaunchError::Refused("no kernels here".into())) })
    }
    fn run_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        Err(LaunchError::Refused("no probes here".into()))
    }
    fn run_project_probe(&self, _spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        Err(LaunchError::Refused("no probes here".into()))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    service: ResearchService,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let service = ResearchService::new(ResearchConfig::new(
        dir.path().join("appdata"),
        Arc::new(NoLauncher),
        Arc::new(DocumentService::new()),
    ));
    service.set_workspace(&root, true).unwrap();
    Fixture { _dir: dir, root, service }
}

fn run_id(i: u64) -> String {
    format!("run-{i:016x}")
}

fn manifest(id: &str, started_at_ms: u64) -> RunManifest {
    let code = format!("print({started_at_ms})\n");
    RunManifest {
        format: "exposition.run/1".into(),
        record: RunRecord {
            id: RunId(id.into()),
            producer: Producer::KernelSession { session_id: "s".into(), runtime: "python".into() },
            inputs: Vec::new(),
            environment: vec![ToolVersion { name: "python".into(), version: "3.14.0".into() }],
            outputs: Vec::new(),
            status: RunStatus::Succeeded,
            started_at_ms,
            finished_at_ms: Some(started_at_ms + 5),
        },
        session: SessionProvenance {
            session_id: "s".into(),
            owner: SessionOwner::User,
            environment_id: "python:x".into(),
            generation: 1,
            interpreter: InterpreterProvenance {
                implementation: "CPython".into(),
                version: "3.14.0".into(),
                location: "~".into(),
            },
            kernel: KernelDescriptor::default(),
            helper_version: "1".into(),
            protocol: 1,
        },
        code: CodeProvenance {
            origin: CodeOrigin::Input,
            sha256: exposition_research::helper::sha256_hex(code.as_bytes()),
            bytes: code.len() as u64,
            text: Some(code),
        },
        execution_count: Some(1),
        error: None,
        output: OutputSummary::default(),
        unsaved_inputs: Vec::new(),
        execution: None,
        authorization: None,
    }
}

fn record_file(root: &Path, id: &str) -> PathBuf {
    root.join(".exposition").join("runs").join(format!("{id}.json"))
}

/// A fixed point in time plus `tick` seconds: the record's modification time.
fn time(tick: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + tick)
}

fn set_modified(root: &Path, id: &str, tick: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(record_file(root, id)).unwrap();
    file.set_modified(time(tick)).unwrap();
}

/// Store a run whose record was last written at `tick`.
fn store_run(root: &Path, id: &str, tick: u64) {
    write_manifest(root, &manifest(id, 1_000 + tick)).unwrap();
    set_modified(root, id, tick);
}

fn page(service: &ResearchService, after: Option<String>, limit: u32) -> RunHistoryPage {
    service.run_history(&RunHistoryRequest { after, limit: Some(limit) }).unwrap()
}

fn ids(page: &RunHistoryPage) -> Vec<String> {
    page.runs.iter().map(|r| r.id.0.clone()).collect()
}

/// Every page from the start, in order.
fn all_pages(service: &ResearchService, limit: u32) -> Vec<RunHistoryPage> {
    let mut pages = Vec::new();
    let mut after = None;
    loop {
        let page = page(service, after, limit);
        after = page.next.clone();
        pages.push(page);
        if after.is_none() {
            return pages;
        }
    }
}

#[test]
fn pages_reach_every_stored_run_once_newest_first() {
    let f = fixture();
    for i in 0..23 {
        store_run(&f.root, &run_id(i), i);
    }
    let pages = all_pages(&f.service, 5);
    assert_eq!(pages.len(), 5);
    let listed: Vec<String> = pages.iter().flat_map(ids).collect();
    let expected: Vec<String> = (0..23).rev().map(run_id).collect();
    assert_eq!(listed, expected, "newest first, each once");
    for (i, page) in pages.iter().enumerate() {
        assert_eq!(page.total, 23);
        assert_eq!(page.unreadable, 0);
        assert_eq!(page.has_more, i < 4, "page {i}");
        assert_eq!(page.next.is_some(), page.has_more);
    }
    // The first page is what `runs()` lists; the default page size is the
    // one the history always had.
    assert_eq!(HISTORY_PAGE, 500);
    let first = f.service.run_history(&RunHistoryRequest::default()).unwrap();
    assert_eq!(first.runs, f.service.runs().unwrap());
    assert_eq!(first.runs.len(), 23);
    assert!(!first.has_more && first.next.is_none());
    // A request for more than one page's worth gets one page.
    assert_eq!(page(&f.service, None, 100_000).runs.len(), 23);
    assert_eq!(page(&f.service, None, 0).runs.len(), 1, "at least one run per page");
}

#[test]
fn more_runs_than_one_page_are_all_listed() {
    let f = fixture();
    let total = u64::from(HISTORY_PAGE) + 20;
    for i in 0..total {
        store_run(&f.root, &run_id(i), i);
    }
    let first = f.service.run_history(&RunHistoryRequest::default()).unwrap();
    assert_eq!(first.runs.len(), HISTORY_PAGE as usize);
    assert_eq!(first.total, total as u32);
    assert!(first.has_more);
    assert_eq!(first.runs[0].id.0, run_id(total - 1));
    let second = f.service.run_history(&RunHistoryRequest { after: first.next.clone(), limit: None }).unwrap();
    assert_eq!(ids(&second), (0..20).rev().map(run_id).collect::<Vec<_>>());
    assert!(!second.has_more && second.next.is_none());
    // The oldest run is a run like any other.
    assert_eq!(f.service.run_summary(&run_id(0)).unwrap(), second.runs[19]);
    assert!(f.service.run_manifest(&run_id(0)).is_ok());
    // Storage & History lists every record, not the first page.
    assert_eq!(f.service.run_records().unwrap().len(), total as usize);
}

#[test]
fn a_page_reads_only_the_manifests_it_returns() {
    let mut per_size = Vec::new();
    for stored in [40u64, 400] {
        let f = fixture();
        for i in 0..stored {
            store_run(&f.root, &run_id(i), i);
        }
        let mut reads = Vec::new();
        let mut after = None;
        for _ in 0..3 {
            let before = f.service.history_counters();
            let page = page(&f.service, after, 10);
            let now = f.service.history_counters();
            assert_eq!(page.runs.len(), 10);
            assert_eq!(now.scans - before.scans, 1);
            reads.push(now.manifests_read - before.manifests_read);
            after = page.next;
        }
        assert_eq!(reads, vec![10, 10, 10], "{stored} runs stored");
        per_size.push(reads);
    }
    assert_eq!(per_size[0], per_size[1], "independent of the number of runs stored");
}

/// xorshift64*: a fixed sequence per seed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Page through the history while runs are created, deleted (the record
/// moved aside or removed) and rewritten between the pages. Against a model
/// of what exists: a listing that completes returns every run that existed
/// from its start to its end exactly once and never a run deleted before it
/// started; a record rewritten before it was returned makes the next request
/// fail with `research.historyChanged` (never a page), and the listing then
/// starts again.
#[test]
fn pages_are_stable_while_runs_are_created_deleted_and_rewritten() {
    let mut refusals = 0;
    let mut completed = 0;
    for seed in 1..=60u64 {
        let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let f = fixture();
        let mut next_id = 0u64;
        // Modification times of the first runs repeat (ties are ordered by id).
        let mut clock = 50u64;
        let mut existing: BTreeSet<String> = BTreeSet::new();
        let mut gone: BTreeSet<String> = BTreeSet::new();
        for _ in 0..random.below(30) + 3 {
            let id = run_id(next_id);
            next_id += 1;
            store_run(&f.root, &id, random.below(12));
            existing.insert(id);
        }
        // Some runs are deleted before the listing starts.
        for id in existing.clone() {
            if random.below(5) == 0 {
                f.service.delete_run_record(&id, random.below(2) == 0).unwrap();
                existing.remove(&id);
                gone.insert(id);
            }
        }
        let limit = random.below(6) as u32 + 1;
        let mut passes = 0;
        'listing: loop {
            passes += 1;
            assert!(passes < 50, "seed {seed}: the listing never completed");
            // What this pass must return, and what it has returned.
            let mut throughout = existing.clone();
            let gone_before = gone.clone();
            let mut returned: Vec<String> = Vec::new();
            let mut after: Option<String> = None;
            // Records rewritten in this pass before they were returned.
            let mut crossed: BTreeSet<String> = BTreeSet::new();
            loop {
                let request = RunHistoryRequest { after: after.clone(), limit: Some(limit) };
                let page = match f.service.run_history(&request) {
                    Ok(page) => page,
                    Err(e) => {
                        assert_eq!(e.code(), HISTORY_CHANGED, "seed {seed}: {e}");
                        assert!(!crossed.is_empty(), "seed {seed}: refused although no pending record was rewritten");
                        refusals += 1;
                        continue 'listing;
                    }
                };
                assert!(crossed.is_empty(), "seed {seed}: a page was returned after a pending record was rewritten");
                assert_eq!(page.total as usize, existing.len(), "seed {seed}");
                returned.extend(ids(&page));
                after = page.next.clone();
                if after.is_none() {
                    break;
                }
                for _ in 0..random.below(4) {
                    match random.below(10) {
                        0..=3 => {
                            let id = run_id(next_id);
                            next_id += 1;
                            clock += 1;
                            store_run(&f.root, &id, clock);
                            existing.insert(id);
                        }
                        4..=7 => {
                            let Some(id) = existing.iter().nth(random.below(existing.len() as u64 + 1) as usize) else {
                                continue;
                            };
                            let id = id.clone();
                            f.service.delete_run_record(&id, random.below(2) == 0).unwrap();
                            existing.remove(&id);
                            throughout.remove(&id);
                            crossed.remove(&id);
                            gone.insert(id);
                        }
                        _ => {
                            let Some(id) = existing.iter().nth(random.below(existing.len() as u64 + 1) as usize) else {
                                continue;
                            };
                            clock += 1;
                            set_modified(&f.root, id, clock);
                            // Pending: it existed when the pass started (a
                            // run created later is newer than every page).
                            if throughout.contains(id) && !returned.contains(id) {
                                crossed.insert(id.clone());
                            }
                        }
                    }
                }
            }
            let unique: BTreeSet<&String> = returned.iter().collect();
            assert_eq!(unique.len(), returned.len(), "seed {seed}: a run was returned twice: {returned:?}");
            for id in &throughout {
                assert!(unique.contains(id), "seed {seed}: {id} existed throughout and was not returned");
            }
            for id in &returned {
                assert!(!gone_before.contains(id), "seed {seed}: {id} was deleted before the listing started");
            }
            completed += 1;
            break;
        }
    }
    assert_eq!(completed, 60);
    assert!(refusals > 5, "the sequences exercise refused continuations ({refusals})");
}

#[test]
fn a_rewritten_pending_record_refuses_the_continuation() {
    let f = fixture();
    for i in 0..5 {
        store_run(&f.root, &run_id(i), i);
    }
    let first = page(&f.service, None, 2);
    assert_eq!(ids(&first), vec![run_id(4), run_id(3)]);
    // A record already returned is rewritten: the rest is unchanged.
    set_modified(&f.root, &run_id(4), 10);
    assert_eq!(ids(&page(&f.service, first.next.clone(), 2)), vec![run_id(2), run_id(1)]);
    // The oldest record, not yet returned, is rewritten: continuing would
    // never return it.
    set_modified(&f.root, &run_id(0), 11);
    let refused = f.service.run_history(&RunHistoryRequest { after: first.next.clone(), limit: Some(2) }).unwrap_err();
    assert_eq!(refused.code(), HISTORY_CHANGED);
    let again: Vec<String> = all_pages(&f.service, 2).iter().flat_map(ids).collect();
    assert_eq!(again.len(), 5);
    assert_eq!(again.iter().collect::<BTreeSet<_>>().len(), 5);
}

#[test]
fn continuations_of_another_listing_or_none_are_refused() {
    let a = fixture();
    let b = fixture();
    for i in 0..4 {
        store_run(&a.root, &run_id(i), i);
        store_run(&b.root, &run_id(i), i);
    }
    let next = page(&a.service, None, 2).next;
    assert!(next.is_some());
    // Another service (an earlier start of the app, another project).
    let other = b.service.run_history(&RunHistoryRequest { after: next.clone(), limit: None }).unwrap_err();
    assert_eq!(other.code(), HISTORY_CHANGED);
    // The same service after the project changed and came back.
    a.service.set_workspace(&b.root, true).unwrap();
    assert_eq!(a.service.run_history(&RunHistoryRequest::default()).unwrap().total, 4);
    a.service.set_workspace(&a.root, true).unwrap();
    let stale = a.service.run_history(&RunHistoryRequest { after: next, limit: None }).unwrap_err();
    assert_eq!(stale.code(), HISTORY_CHANGED);
    for text in ["", "next", "h1.0.0.0.run-../../x", "h1.zz.0.0.run-a"] {
        let bad = a.service.run_history(&RunHistoryRequest { after: Some(text.into()), limit: None }).unwrap_err();
        assert_eq!(bad.code(), "research.invalidRequest", "{text}");
    }
}

#[test]
fn damaged_records_inside_a_page_are_skipped_and_counted() {
    let f = fixture();
    for i in 0..12 {
        store_run(&f.root, &run_id(i), i);
    }
    // Not JSON, a manifest naming another run, a manifest cut short, and
    // JSON that is not a manifest; plus entries that are not records at all.
    let other = serde_json::to_vec(&manifest(&run_id(99), 1)).unwrap();
    let whole = serde_json::to_vec(&manifest(&run_id(4), 1)).unwrap();
    let damaged: HashMap<u64, Vec<u8>> = HashMap::from([
        (9, b"\x00\xff not a manifest".to_vec()),
        (8, other),
        (4, whole[..whole.len() / 2].to_vec()),
        (3, br#"{"format":"exposition.run/1","record":{"id":"run-0000000000000003","outputs":"x"}}"#.to_vec()),
    ]);
    for (i, bytes) in &damaged {
        std::fs::write(record_file(&f.root, &run_id(*i)), bytes).unwrap();
        set_modified(&f.root, &run_id(*i), *i);
    }
    let runs = f.root.join(".exposition").join("runs");
    std::fs::write(runs.join("notes.json"), "{}").unwrap();
    std::fs::write(runs.join("run-UPPER.json"), "{}").unwrap();
    std::fs::create_dir(runs.join("run-00000000000000aa.json")).unwrap();

    let readable: Vec<String> = (0..12).rev().filter(|i| !damaged.contains_key(i)).map(run_id).collect();
    let before = f.service.history_counters();
    let pages = all_pages(&f.service, 3);
    // Damaged records never shorten a page or end the listing.
    assert_eq!(pages.iter().map(|p| p.runs.len()).collect::<Vec<_>>(), vec![3, 3, 2]);
    assert_eq!(pages.iter().flat_map(ids).collect::<Vec<_>>(), readable);
    assert_eq!(ids(&pages[0]), vec![run_id(11), run_id(10), run_id(7)], "the page continues past the damaged records");
    let last = pages.last().unwrap();
    assert_eq!(last.total, 12, "every record file is counted");
    assert_eq!(last.unreadable, 4);
    assert_eq!(last.total - last.unreadable, readable.len() as u32, "listed and unreadable add up to the total");
    assert!(!last.has_more);
    // Each record was read once; a damaged one is not read again.
    assert_eq!(f.service.history_counters().manifests_read - before.manifests_read, 12);
    let before = f.service.history_counters();
    let again = all_pages(&f.service, 3);
    assert_eq!(again.iter().flat_map(ids).collect::<Vec<_>>(), readable);
    assert_eq!(again[0].unreadable, 4, "known from the first request on");
    assert_eq!(f.service.history_counters().manifests_read - before.manifests_read, 8);
    // Storage & History leaves the same records out.
    assert_eq!(f.service.run_records().unwrap().len(), readable.len());
    // A repaired record is a run again.
    store_run(&f.root, &run_id(9), 20);
    let repaired = all_pages(&f.service, 3);
    assert_eq!(repaired[0].runs[0].id.0, run_id(9));
    assert_eq!(repaired.last().unwrap().unreadable, 3);
    assert_eq!(repaired.iter().map(|p| p.runs.len()).sum::<usize>(), 9);
}

#[test]
fn a_page_of_only_damaged_records_is_not_the_end() {
    let f = fixture();
    for i in 0..10 {
        store_run(&f.root, &run_id(i), i);
    }
    for i in 3..9 {
        std::fs::write(record_file(&f.root, &run_id(i)), "{").unwrap();
        set_modified(&f.root, &run_id(i), i);
    }
    let pages = all_pages(&f.service, 2);
    assert_eq!(pages.iter().flat_map(ids).collect::<Vec<_>>(), vec![run_id(9), run_id(2), run_id(1), run_id(0)]);
    assert_eq!(pages.last().unwrap().unreadable, 6);
    assert_eq!(pages.last().unwrap().total, 10);
}

#[test]
fn deleted_and_restored_runs_follow_the_listing() {
    let f = fixture();
    for i in 0..9 {
        store_run(&f.root, &run_id(i), i);
    }
    let first = page(&f.service, None, 3);
    // A run of a later page is deleted (recoverably), then restored.
    f.service.delete_run_record(&run_id(1), false).unwrap();
    let second = page(&f.service, first.next.clone(), 3);
    assert_eq!(second.total, 8);
    f.service.restore_run_record(&run_id(1)).unwrap();
    let third = page(&f.service, second.next.clone(), 3);
    assert_eq!(third.total, 9);
    let listed: Vec<String> = [&first, &second, &third].into_iter().flat_map(ids).collect();
    assert_eq!(listed, (0..9).rev().map(run_id).collect::<Vec<_>>());
    assert!(!third.has_more);
}

/// Several listings at once (real threads) while records are created,
/// deleted and rewritten: a listing that completes holds every record that
/// was only ever rewritten (never deleted) exactly once and no record twice;
/// one that a rewrite crossed is refused and read again from its start.
#[test]
fn listings_on_several_threads_never_skip_or_repeat_a_run() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let f = Arc::new(fixture());
    // Kept for the whole test; some of them are rewritten now and then.
    let kept: Vec<String> = (0..40).map(run_id).collect();
    for (i, id) in kept.iter().enumerate() {
        store_run(&f.root, id, i as u64 % 7);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicU64::new(0));
    let refused = Arc::new(AtomicU64::new(0));
    let writer = {
        let (f, stop, kept) = (f.clone(), stop.clone(), kept.clone());
        std::thread::spawn(move || {
            let mut random = Random(0x5EED_1234_5678_9ABC);
            let mut clock = 100u64;
            let mut next = 1_000u64;
            let mut made: Vec<String> = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                clock += 1;
                match random.below(8) {
                    0..=2 => {
                        let id = run_id(next);
                        next += 1;
                        store_run(&f.root, &id, clock);
                        made.push(id);
                    }
                    3..=5 if !made.is_empty() => {
                        let id = made.swap_remove(random.below(made.len() as u64) as usize);
                        std::fs::remove_file(record_file(&f.root, &id)).unwrap();
                    }
                    6 if !made.is_empty() => {
                        set_modified(&f.root, &made[random.below(made.len() as u64) as usize], clock);
                    }
                    _ => set_modified(&f.root, &kept[random.below(kept.len() as u64) as usize], clock),
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };
    let readers: Vec<_> = (0..4u32)
        .map(|n| {
            let (f, stop, kept) = (f.clone(), stop.clone(), kept.clone());
            let (completed, refused) = (completed.clone(), refused.clone());
            std::thread::spawn(move || {
                let limit = 5 + n * 4;
                'listing: while !stop.load(Ordering::Relaxed) {
                    let mut returned: Vec<String> = Vec::new();
                    let mut after = None;
                    loop {
                        match f.service.run_history(&RunHistoryRequest { after, limit: Some(limit) }) {
                            Ok(page) => {
                                returned.extend(ids(&page));
                                after = page.next;
                                if after.is_none() {
                                    break;
                                }
                            }
                            Err(e) => {
                                assert_eq!(e.code(), HISTORY_CHANGED, "{e}");
                                refused.fetch_add(1, Ordering::Relaxed);
                                continue 'listing;
                            }
                        }
                    }
                    let unique: BTreeSet<&String> = returned.iter().collect();
                    assert_eq!(unique.len(), returned.len(), "a run was returned twice: {returned:?}");
                    for id in &kept {
                        assert!(unique.contains(id), "{id} existed throughout and is on no page");
                    }
                    completed.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    let started = std::time::Instant::now();
    while completed.load(Ordering::Relaxed) < 200 && started.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    for reader in readers {
        reader.join().expect("a listing was wrong");
    }
    assert!(completed.load(Ordering::Relaxed) > 0, "no listing completed");
    println!("listings completed: {}, refused: {}", completed.load(Ordering::Relaxed), refused.load(Ordering::Relaxed));
    // With the writer stopped, one more listing is whole.
    let listed: Vec<String> = all_pages(&f.service, 9).iter().flat_map(ids).collect();
    assert_eq!(listed.iter().collect::<BTreeSet<_>>().len(), listed.len());
    assert!(kept.iter().all(|id| listed.contains(id)));
}
