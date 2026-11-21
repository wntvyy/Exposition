//! Independent review of the paged run history: attacks on the completeness
//! of a listing (every run that exists throughout is returned exactly once,
//! or the continuation is refused), on the truth of `total` / `unreadable`,
//! on continuations as untrusted input, and on the work one page does.
//!
//! Tests marked `#[ignore = "review: defect ..."]` fail against the code under
//! review and document a defect; every other test passes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use exposition_core::artifact::{Producer, RunId, RunRecord, RunStatus, ToolVersion};
use exposition_core::job::JobSpec;
use exposition_research::launcher::{KernelProcessLauncher, LaunchError, LaunchFuture, ProbeOutput};
use exposition_research::store::write_manifest;
use exposition_research::types::{
    CodeOrigin, CodeProvenance, InterpreterProvenance, KernelDescriptor, OutputSummary, RunHistoryPage,
    RunHistoryRequest, RunManifest, SessionOwner, SessionProvenance,
};
use exposition_research::{HISTORY_CHANGED, ResearchConfig, ResearchError, ResearchService};
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
    dir: tempfile::TempDir,
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
    Fixture { dir, root, service }
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

fn runs_dir(root: &Path) -> PathBuf {
    root.join(".exposition").join("runs")
}

fn record_file(root: &Path, id: &str) -> PathBuf {
    runs_dir(root).join(format!("{id}.json"))
}

fn time(tick: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + tick)
}

fn set_time(root: &Path, id: &str, at: SystemTime) {
    let file = std::fs::OpenOptions::new().write(true).open(record_file(root, id)).unwrap();
    file.set_modified(at).unwrap();
}

fn set_modified(root: &Path, id: &str, tick: u64) {
    set_time(root, id, time(tick));
}

fn store_run(root: &Path, id: &str, tick: u64) {
    write_manifest(root, &manifest(id, 1_000 + tick)).unwrap();
    set_modified(root, id, tick);
}

fn request(after: Option<String>, limit: u32) -> RunHistoryRequest {
    RunHistoryRequest { after, limit: Some(limit) }
}

fn page(service: &ResearchService, after: Option<String>, limit: u32) -> RunHistoryPage {
    service.run_history(&request(after, limit)).unwrap()
}

fn ids(page: &RunHistoryPage) -> Vec<String> {
    page.runs.iter().map(|r| r.id.0.clone()).collect()
}

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
        assert!(pages.len() < 10_000, "the listing never ends");
    }
}

fn all_ids(service: &ResearchService, limit: u32) -> Vec<String> {
    all_pages(service, limit).iter().flat_map(ids).collect()
}

fn assert_each_once(listed: &[String], expected: impl IntoIterator<Item = String>, what: &str) {
    let unique: BTreeSet<&String> = listed.iter().collect();
    assert_eq!(unique.len(), listed.len(), "{what}: a run was returned twice: {listed:?}");
    let expected: BTreeSet<String> = expected.into_iter().collect();
    assert_eq!(unique.into_iter().cloned().collect::<BTreeSet<_>>(), expected, "{what}");
}

/// xorshift64*.
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

// ---- completeness ---------------------------------------------------------

#[test]
fn identical_modification_times_straddling_every_page_boundary() {
    // A coarse file system clock: every record of a group has the same time.
    let f = fixture();
    for i in 0..41 {
        store_run(&f.root, &run_id(i), i / 14);
    }
    let expected: Vec<String> = (0..41).map(run_id).collect();
    for limit in [1, 2, 3, 5, 7, 13, 14, 15, 40, 41, 42] {
        let listed = all_ids(&f.service, limit);
        assert_each_once(&listed, expected.clone(), &format!("limit {limit}"));
    }
    // All at one instant: the order is by id alone.
    for i in 0..41 {
        set_modified(&f.root, &run_id(i), 9);
    }
    let listed = all_ids(&f.service, 4);
    assert_eq!(listed, (0..41).rev().map(run_id).collect::<Vec<_>>());
}

#[test]
fn epoch_zero_before_the_epoch_and_future_times_are_listed_once() {
    let f = fixture();
    for i in 0..12 {
        store_run(&f.root, &run_id(i), i);
    }
    set_time(&f.root, &run_id(3), SystemTime::UNIX_EPOCH);
    set_time(&f.root, &run_id(4), SystemTime::UNIX_EPOCH);
    // Before 1970 (Windows file times reach back to 1601); skipped when the
    // file system refuses it.
    let file = std::fs::OpenOptions::new().write(true).open(record_file(&f.root, &run_id(5))).unwrap();
    let _ = file.set_modified(SystemTime::UNIX_EPOCH - Duration::from_secs(86_400 * 365));
    drop(file);
    let file = std::fs::OpenOptions::new().write(true).open(record_file(&f.root, &run_id(6))).unwrap();
    let _ = file.set_modified(SystemTime::UNIX_EPOCH - Duration::from_secs(86_400 * 400));
    drop(file);
    // Far in the future (a clock that was wrong, a copied folder).
    set_time(&f.root, &run_id(7), SystemTime::UNIX_EPOCH + Duration::from_secs(40_000_000_000));
    set_time(&f.root, &run_id(8), SystemTime::now() + Duration::from_secs(86_400 * 3_650));
    for limit in [1, 2, 5] {
        let pages = all_pages(&f.service, limit);
        let listed: Vec<String> = pages.iter().flat_map(ids).collect();
        assert_each_once(&listed, (0..12).map(run_id), &format!("limit {limit}"));
        assert!(pages.iter().all(|p| p.total == 12 && p.unreadable == 0));
        // Pages follow the modification times: the first holds the newest.
        let first: Vec<String> = ids(&pages[0]);
        assert!(first.contains(&run_id(7)), "limit {limit}: {first:?}");
        assert_eq!(first.contains(&run_id(8)), limit > 1, "limit {limit}: {first:?}");
    }
    // A record written now (older than the "future" ones) between two pages
    // is new: it may appear or not, but nothing is returned twice or lost.
    let first = page(&f.service, None, 3);
    write_manifest(&f.root, &manifest(&run_id(50), 5)).unwrap();
    let mut listed = ids(&first);
    let mut after = first.next;
    while let Some(next) = after {
        let page = page(&f.service, Some(next), 3);
        listed.extend(ids(&page));
        after = page.next;
    }
    let unique: BTreeSet<&String> = listed.iter().collect();
    assert_eq!(unique.len(), listed.len(), "{listed:?}");
    for i in 0..12 {
        assert!(unique.contains(&run_id(i)), "{i}");
    }
}

#[test]
fn a_record_rewritten_to_the_identical_time_keeps_its_place() {
    let f = fixture();
    for i in 0..8 {
        store_run(&f.root, &run_id(i), i);
    }
    let first = page(&f.service, None, 3);
    // A pending record gets new content and the same time; so does one
    // already returned.
    for i in [1, 6] {
        write_manifest(&f.root, &manifest(&run_id(i), 777_000 + i)).unwrap();
        set_modified(&f.root, &run_id(i), i);
    }
    let mut listed = ids(&first);
    let mut after = first.next;
    let mut rewritten = None;
    while let Some(next) = after {
        let page = page(&f.service, Some(next), 3);
        rewritten = rewritten.or(page.runs.iter().find(|r| r.id.0 == run_id(1)).cloned());
        listed.extend(ids(&page));
        after = page.next;
    }
    assert_each_once(&listed, (0..8).map(run_id), "identical time");
    assert_eq!(rewritten.unwrap().started_at_ms, 777_001, "the page holds the record as it is on disk");
}

#[test]
fn a_record_rewritten_twice_back_to_its_place_does_not_repeat_or_hide_a_run() {
    for observed_between in [false, true] {
        let f = fixture();
        for i in 0..8 {
            store_run(&f.root, &run_id(i), i * 10);
        }
        let first = page(&f.service, None, 3);
        assert_eq!(ids(&first), vec![run_id(7), run_id(6), run_id(5)]);
        // Pending run 1 moves to the front (seen by another listing or not),
        // then back; returned run 6 moves to the back, then to the front.
        set_modified(&f.root, &run_id(1), 500);
        set_modified(&f.root, &run_id(6), 1);
        if observed_between {
            assert_eq!(page(&f.service, None, 2).runs.len(), 2);
        }
        set_modified(&f.root, &run_id(1), 10);
        set_modified(&f.root, &run_id(6), 600);
        let mut listed = ids(&first);
        let mut after = first.next.clone();
        while let Some(next) = after {
            let page = page(&f.service, Some(next), 3);
            listed.extend(ids(&page));
            after = page.next;
        }
        assert_each_once(&listed, (0..8).map(run_id), &format!("observed between: {observed_between}"));
    }
}

#[test]
fn rewrites_on_either_side_of_the_continuation() {
    let f = fixture();
    for i in 0..10 {
        store_run(&f.root, &run_id(i), 100 + i * 10);
    }
    let refused = |after: &Option<String>| {
        let error = f.service.run_history(&request(after.clone(), 3)).unwrap_err();
        assert_eq!(error.code(), HISTORY_CHANGED);
    };
    let first = page(&f.service, None, 3);
    assert_eq!(ids(&first), vec![run_id(9), run_id(8), run_id(7)]);
    // Returned, and stays on the returned side (also exactly at the boundary
    // time, ordered by id above the last run returned): the page follows.
    set_modified(&f.root, &run_id(9), 171);
    set_modified(&f.root, &run_id(8), 170);
    let set = |page: &RunHistoryPage| ids(page).into_iter().collect::<BTreeSet<_>>();
    let of = |runs: [u64; 3]| runs.into_iter().map(run_id).collect::<BTreeSet<_>>();
    assert_eq!(set(&page(&f.service, first.next.clone(), 3)), of([6, 5, 4]));
    // Pending, and stays pending (reordered among the pending ones).
    set_modified(&f.root, &run_id(2), 165);
    assert_eq!(set(&page(&f.service, first.next.clone(), 3)), of([2, 6, 5]));
    // Returned, written back in time across the continuation (a clock set
    // back, a tool that restores times): it would be returned again.
    set_modified(&f.root, &run_id(9), 5);
    refused(&first.next);
    refused(&first.next);
    // Put back on its side: the continuation holds again.
    set_modified(&f.root, &run_id(9), 400);
    assert_eq!(page(&f.service, first.next.clone(), 3).runs.len(), 3);
    // Pending, moved to exactly the boundary time with a smaller id than the
    // last run returned: still pending.
    set_modified(&f.root, &run_id(0), 170);
    assert!(ids(&page(&f.service, first.next.clone(), 1)).contains(&run_id(0)));
    // Pending, moved across.
    set_modified(&f.root, &run_id(0), 171);
    refused(&first.next);
    // A fresh listing is complete.
    assert_each_once(&all_ids(&f.service, 3), (0..10).map(run_id), "fresh listing");
}

#[test]
fn a_change_made_while_no_listing_was_in_progress_is_noticed() {
    // The record changes after the last page was read and long before the
    // next request; nothing lists the folder in between. Also across other
    // listings that started and ended in between.
    let f = fixture();
    for i in 0..9 {
        store_run(&f.root, &run_id(i), i * 10);
    }
    let first = page(&f.service, None, 4);
    set_modified(&f.root, &run_id(2), 999);
    assert_eq!(f.service.run_history(&request(first.next.clone(), 4)).unwrap_err().code(), HISTORY_CHANGED);
    for _ in 0..5 {
        assert_eq!(all_ids(&f.service, 2).len(), 9);
    }
    assert_eq!(f.service.run_history(&request(first.next.clone(), 4)).unwrap_err().code(), HISTORY_CHANGED);
    // The record changes once more, staying across.
    set_modified(&f.root, &run_id(2), 1_500);
    assert_eq!(f.service.run_history(&request(first.next, 4)).unwrap_err().code(), HISTORY_CHANGED);
}

/// One listing in progress, in the model.
struct Listing {
    limit: u32,
    after: Option<String>,
    returned: Vec<String>,
    /// Runs that exist since the listing started (removed when deleted).
    throughout: BTreeSet<String>,
    started: bool,
}

impl Listing {
    fn new(limit: u32) -> Self {
        Self { limit, after: None, returned: Vec::new(), throughout: BTreeSet::new(), started: false }
    }
}

/// Two listings are paged in turns (different page sizes, their own
/// continuations) while records are created (also with old times: a clock set
/// back), deleted, restored and rewritten to any time, earlier or later or
/// equal to another record's. A listing may be refused at any request; one
/// that completes returned every run that existed throughout exactly once.
#[test]
fn interleaved_listings_under_arbitrary_time_changes_never_repeat_or_lose_a_run() {
    let (mut completed, mut refusals) = (0, 0);
    for seed in 1..=80u64 {
        let mut random = Random(seed.wrapping_mul(0xD6E8_FEB8_6659_FD93) | 1);
        let f = fixture();
        let mut next_id = 0u64;
        let mut existing: BTreeMap<String, u64> = BTreeMap::new();
        let mut deleted: Vec<String> = Vec::new();
        for _ in 0..random.below(25) + 4 {
            let id = run_id(next_id);
            next_id += 1;
            let tick = random.below(8);
            store_run(&f.root, &id, tick);
            existing.insert(id, tick);
        }
        let mut listings = [Listing::new(random.below(4) as u32 + 1), Listing::new(random.below(7) as u32 + 1)];
        for _step in 0..400 {
            let which = random.below(2) as usize;
            let listing = &mut listings[which];
            if !listing.started {
                listing.started = true;
                listing.after = None;
                listing.returned.clear();
                listing.throughout = existing.keys().cloned().collect();
            }
            match f.service.run_history(&request(listing.after.clone(), listing.limit)) {
                Ok(page) => {
                    assert_eq!(page.total as usize, existing.len(), "seed {seed}: total");
                    assert_eq!(page.unreadable, 0, "seed {seed}");
                    assert!(page.runs.len() <= listing.limit as usize);
                    assert_eq!(page.next.is_some(), page.has_more, "seed {seed}");
                    listing.returned.extend(ids(&page));
                    let unique: BTreeSet<&String> = listing.returned.iter().collect();
                    assert_eq!(
                        unique.len(),
                        listing.returned.len(),
                        "seed {seed}: listing {which} returned a run twice: {:?}",
                        listing.returned
                    );
                    listing.after = page.next;
                    if listing.after.is_none() {
                        for id in &listing.throughout {
                            assert!(
                                unique.contains(id),
                                "seed {seed}: listing {which}: {id} existed throughout and was not returned ({:?})",
                                listing.returned
                            );
                        }
                        completed += 1;
                        listing.started = false;
                    }
                }
                Err(error) => {
                    assert_eq!(error.code(), HISTORY_CHANGED, "seed {seed}: {error}");
                    assert!(listing.after.is_some(), "seed {seed}: a first page was refused");
                    refusals += 1;
                    listing.started = false;
                }
            }
            for _ in 0..random.below(3) {
                match random.below(12) {
                    0..=2 => {
                        let id = run_id(next_id);
                        next_id += 1;
                        let tick = random.below(40);
                        store_run(&f.root, &id, tick);
                        existing.insert(id, tick);
                    }
                    3..=4 => {
                        let Some(id) = existing.keys().nth(random.below(existing.len() as u64 + 1) as usize).cloned()
                        else {
                            continue;
                        };
                        let permanent = random.below(3) == 0;
                        f.service.delete_run_record(&id, permanent).unwrap();
                        existing.remove(&id);
                        for listing in &mut listings {
                            listing.throughout.remove(&id);
                        }
                        if !permanent {
                            deleted.push(id);
                        }
                    }
                    5 => {
                        if deleted.is_empty() {
                            continue;
                        }
                        let id = deleted.swap_remove(random.below(deleted.len() as u64) as usize);
                        f.service.restore_run_record(&id).unwrap();
                        let tick = std::fs::metadata(record_file(&f.root, &id)).unwrap().modified().unwrap();
                        let tick = tick.duration_since(time(0)).unwrap().as_secs();
                        existing.insert(id, tick);
                    }
                    _ => {
                        let Some(id) = existing.keys().nth(random.below(existing.len() as u64 + 1) as usize).cloned()
                        else {
                            continue;
                        };
                        // Earlier, later, or the time of another record.
                        let tick = match random.below(3) {
                            0 => random.below(40),
                            1 => existing.values().nth(random.below(existing.len() as u64) as usize).copied().unwrap(),
                            _ => existing[&id],
                        };
                        if random.below(2) == 0 {
                            write_manifest(&f.root, &manifest(&id, 5_000 + tick)).unwrap();
                        }
                        set_modified(&f.root, &id, tick);
                        existing.insert(id, tick);
                    }
                }
            }
        }
    }
    assert!(completed > 200, "listings completed: {completed}");
    assert!(refusals > 50, "refusals exercised: {refusals}");
}

#[test]
fn the_project_changing_between_pages_never_mixes_two_projects() {
    let f = fixture();
    let other = f.dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = std::fs::canonicalize(&other).unwrap();
    for i in 0..9 {
        store_run(&f.root, &run_id(i), i);
        store_run(&other, &run_id(100 + i), i);
    }
    let first = page(&f.service, None, 3);
    f.service.set_workspace(&other, true).unwrap();
    let error = f.service.run_history(&request(first.next.clone(), 3)).unwrap_err();
    assert_eq!(error.code(), HISTORY_CHANGED);
    assert!(!error.to_string().contains("project") && !error.to_string().contains("other"), "{error}");
    let theirs = all_ids(&f.service, 4);
    assert_each_once(&theirs, (100..109).map(run_id), "the other project");
    // A continuation of the other project, used after coming back.
    let their_next = page(&f.service, None, 3).next;
    f.service.set_workspace(&f.root, true).unwrap();
    for stale in [first.next, their_next] {
        assert_eq!(f.service.run_history(&request(stale, 3)).unwrap_err().code(), HISTORY_CHANGED);
    }
    assert_each_once(&all_ids(&f.service, 4), (0..9).map(run_id), "back in the first project");
}

#[test]
fn a_continuation_older_than_the_change_log_is_refused_and_a_recent_one_holds() {
    let f = fixture();
    let records = 40u64;
    for i in 0..records {
        store_run(&f.root, &run_id(i), i);
    }
    let old = page(&f.service, None, 38);
    assert!(old.next.is_some());
    // Records already returned are rewritten (never crossing the
    // continuation) more often than the log of changes holds: 4,096.
    let mut tick = 1_000;
    let mut logged = 0;
    let mut recent = None;
    while logged <= 4_096 + 38 {
        for i in 2..records {
            tick += 1;
            set_modified(&f.root, &run_id(i), tick);
            logged += 1;
        }
        let page = page(&f.service, None, 38);
        assert_eq!(page.total, records as u32);
        recent = page.next;
    }
    assert_eq!(f.service.run_history(&request(old.next, 38)).unwrap_err().code(), HISTORY_CHANGED);
    let last = page(&f.service, recent, 38);
    assert_eq!(ids(&last), vec![run_id(1), run_id(0)]);
    assert_each_once(&all_ids(&f.service, 7), (0..records).map(run_id), "after the log overflowed");
}

// ---- the request as input -------------------------------------------------

#[test]
fn limits_outside_the_range_and_malformed_requests() {
    let f = fixture();
    for i in 0..3 {
        store_run(&f.root, &run_id(i), i);
    }
    let parse = |json: &str| serde_json::from_str::<RunHistoryRequest>(json);
    assert_eq!(f.service.run_history(&parse("{}").unwrap()).unwrap().runs.len(), 3);
    assert_eq!(f.service.run_history(&parse(r#"{"after":null,"limit":null}"#).unwrap()).unwrap().runs.len(), 3);
    assert_eq!(f.service.run_history(&parse(r#"{"limit":0}"#).unwrap()).unwrap().runs.len(), 1);
    assert_eq!(f.service.run_history(&parse(r#"{"limit":501}"#).unwrap()).unwrap().runs.len(), 3);
    assert_eq!(f.service.run_history(&parse(r#"{"limit":4294967295}"#).unwrap()).unwrap().runs.len(), 3);
    for bad in [
        r#"{"limit":-1}"#,
        r#"{"limit":4294967296}"#,
        r#"{"limit":1.5}"#,
        r#"{"limit":"10"}"#,
        r#"{"after":7}"#,
        r#"{"after":["h1"]}"#,
        r#"{"limit":1,"root":"C:/elsewhere"}"#,
        r#"{"offset":3}"#,
        r#"["h1.0.0.0.run-0",1,2]"#,
        "null",
        "7",
    ] {
        assert!(parse(bad).is_err(), "{bad}");
    }
}

fn check_refusal(f: &Fixture, error: &ResearchError, text: &str) {
    let code = error.code();
    assert!(code == "research.invalidRequest" || code == HISTORY_CHANGED, "{code} for {text:?}");
    let message = error.to_string();
    let root = f.root.display().to_string();
    let temp = f.dir.path().display().to_string();
    assert!(!message.contains(&root) && !message.contains(&temp), "the refusal names a path: {message}");
    assert!(!message.contains(".exposition") && !message.contains('\\'), "{message}");
    assert!(message.len() < 300, "the refusal repeats its input ({} bytes)", message.len());
}

#[test]
fn forged_continuations_are_refused_or_answered_from_the_open_project_only() {
    let f = fixture();
    let other = f.dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = std::fs::canonicalize(&other).unwrap();
    for i in 0..30 {
        store_run(&f.root, &run_id(i), i);
        store_run(&other, &run_id(1_000 + i), i);
    }
    let mine: BTreeSet<String> = (0..30).map(run_id).collect();
    let valid = page(&f.service, None, 10).next.unwrap();
    let parts: Vec<&str> = valid.split('.').collect();
    assert_eq!(parts.len(), 5, "{valid}");
    let before = f.service.history_counters();
    let mut attempts = 0u64;
    let mut answered = 0;
    let mut check = |text: String, limit: u32| {
        attempts += 1;
        match f.service.run_history(&request(Some(text.clone()), limit)) {
            Ok(page) => {
                answered += 1;
                assert!(page.runs.len() <= limit.max(1) as usize);
                assert!(page.runs.iter().all(|r| mine.contains(&r.id.0)), "a run of another project: {text}");
                assert_eq!(page.total, 30);
            }
            Err(error) => check_refusal(&f, &error, &text[..text.len().min(80)]),
        }
    };
    // Structured forgeries: every field replaced.
    let replacements = [
        "",
        "0",
        "1",
        "ffffffffffffffff",
        "10000000000000000",
        "ffffffffffffffffffffffffffffffff",
        "100000000000000000000000000000000",
        "+1",
        "-1",
        " 1",
        "0x1",
        "1e3",
        "h1",
        "run-0000000000000005",
        "run-zzzzzzzzzzzzzzzz",
        "run-",
        "run-../../other/.exposition/runs/run-00000000000003e8",
        "..\\..\\other",
        "run-a\u{0}b",
        "RUN-0000000000000005",
        "run-00000000000003e8",
        "\u{202e}",
        "%2e%2e",
    ];
    for field in 0..5 {
        for replacement in replacements {
            let mut forged: Vec<String> = parts.iter().map(|p| (*p).to_owned()).collect();
            forged[field] = replacement.to_owned();
            check(forged.join("."), 10);
        }
    }
    // Another project's absolute paths and separators inside the text.
    for text in [
        format!("h1.{}.{}.{}.{}", parts[1], parts[2], parts[3], other.display()),
        format!("{valid}.{}", other.display()),
        format!("{valid}/../../../other"),
        format!("{valid}\n{valid}"),
        format!("{valid}."),
        format!(".{valid}"),
        valid.to_uppercase(),
        valid.repeat(3),
        "h1....".to_owned(),
        "h1.0.0.0.".to_owned(),
        ".".repeat(100_000),
        format!("h1.{}.0.0.run-0", "f".repeat(1_000_000)),
        format!("h1.0.0.0.run-{}", "a".repeat(1_000_000)),
        "h1.".repeat(300_000),
    ] {
        check(text, 500);
    }
    // Random text and random mutations of a valid continuation.
    let mut random = Random(0x5EED_1234_ABCD_0001);
    let alphabet: Vec<char> = "h1.0123456789abcdefrun-_/\\: +x\u{0}\u{e9}".chars().collect();
    for _ in 0..3_000 {
        let text: String = if random.below(2) == 0 {
            (0..random.below(60)).map(|_| alphabet[random.below(alphabet.len() as u64) as usize]).collect()
        } else {
            let mut chars: Vec<char> = valid.chars().collect();
            for _ in 0..random.below(4) + 1 {
                let at = random.below(chars.len() as u64) as usize;
                match random.below(3) {
                    0 => chars[at] = alphabet[random.below(alphabet.len() as u64) as usize],
                    1 => {
                        chars.remove(at);
                    }
                    _ => chars.insert(at, alphabet[random.below(alphabet.len() as u64) as usize]),
                }
            }
            chars.into_iter().collect()
        };
        check(text, random.below(12) as u32);
    }
    // No forgery made a request read more than its page, and each request
    // listed the folder at most once.
    let after = f.service.history_counters();
    assert!(after.scans - before.scans <= attempts);
    assert!(after.manifests_read - before.manifests_read <= answered * 30, "{after:?}");
    assert!(answered > 0, "some forged positions are answered (from this project)");
    // The listing itself is intact afterwards.
    assert_each_once(&all_ids(&f.service, 7), mine.iter().cloned(), "after the forgeries");

    // A continuation made in the other project with this project's position
    // written into it lists the open project only.
    f.service.set_workspace(&other, true).unwrap();
    let theirs = page(&f.service, None, 10).next.unwrap();
    let their_parts: Vec<&str> = theirs.split('.').collect();
    let forged = format!("h1.{}.{}.{}.{}", their_parts[1], their_parts[2], parts[3], parts[4]);
    if let Ok(page) = f.service.run_history(&request(Some(forged), 500)) {
        assert!(page.runs.iter().all(|r| !mine.contains(&r.id.0)));
    }
}

// ---- total and unreadable -------------------------------------------------

#[test]
fn total_and_unreadable_are_true_on_every_page() {
    let f = fixture();
    for i in 0..20 {
        store_run(&f.root, &run_id(i), i);
    }
    let runs = runs_dir(&f.root);
    // Not records: other files, folders, the partial file of a record being
    // written (as the store names it, and other spellings), a record of the
    // recoverable area.
    std::fs::write(runs.join("index.json"), "{}").unwrap();
    std::fs::write(runs.join("RUN-00000000000000ff.json"), "{}").unwrap();
    std::fs::write(runs.join("run_00000000000000fe.json"), "{}").unwrap();
    std::fs::write(runs.join("run-0000000000000001.JSON.bak"), "{}").unwrap();
    std::fs::write(runs.join(format!("{}.{}.partial", run_id(77), std::process::id())), "{\"format\":").unwrap();
    std::fs::write(runs.join(format!("{}.json.tmp", run_id(78))), "{").unwrap();
    std::fs::write(runs.join(format!("{}.json~", run_id(79))), "{").unwrap();
    std::fs::create_dir(runs.join(format!("{}.json", run_id(80)))).unwrap();
    std::fs::create_dir(runs.join("run-folder")).unwrap();
    std::fs::write(runs.join("run-folder").join(format!("{}.json", run_id(81))), "{}").unwrap();
    for limit in [1, 3, 20, 500] {
        for page in all_pages(&f.service, limit) {
            assert_eq!((page.total, page.unreadable), (20, 0), "limit {limit}");
        }
    }
    assert_each_once(&all_ids(&f.service, 6), (0..20).map(run_id), "only records are runs");
    assert_eq!(f.service.run_records().unwrap().len(), 20);

    // Two damaged records on the second and the last page.
    std::fs::write(record_file(&f.root, &run_id(12)), "[]").unwrap();
    set_modified(&f.root, &run_id(12), 12);
    std::fs::write(record_file(&f.root, &run_id(0)), "").unwrap();
    set_modified(&f.root, &run_id(0), 0);
    let pages = all_pages(&f.service, 5);
    let listed: Vec<String> = pages.iter().flat_map(ids).collect();
    assert_each_once(&listed, (0..20).filter(|i| *i != 12 && *i != 0).map(run_id), "damaged records left out");
    for (i, page) in pages.iter().enumerate() {
        assert_eq!(page.total, 20, "page {i}");
        // Never more than exist, never fewer than found so far.
        assert!(page.unreadable <= 2);
        assert!(page.runs.len() as u32 + page.unreadable <= page.total);
    }
    assert_eq!(pages.last().unwrap().unreadable, 2);
    assert_eq!(listed.len() as u32 + 2, pages.last().unwrap().total, "listed and unreadable add up");

    // A readable record is deleted to the recoverable area and restored; a
    // damaged one is removed.
    f.service.delete_run_record(&run_id(15), false).unwrap();
    let pages = all_pages(&f.service, 5);
    assert!(pages.iter().all(|p| p.total == 19 && p.unreadable == 2));
    assert_eq!(pages.iter().map(|p| p.runs.len()).sum::<usize>(), 17);
    f.service.restore_run_record(&run_id(15)).unwrap();
    std::fs::remove_file(record_file(&f.root, &run_id(12))).unwrap();
    let pages = all_pages(&f.service, 5);
    assert!(pages.iter().all(|p| p.total == 19 && p.unreadable == 1), "{:?}", pages.last().map(|p| p.unreadable));
    let listed: Vec<String> = pages.iter().flat_map(ids).collect();
    assert_each_once(&listed, (1..20).filter(|i| *i != 12).map(run_id), "after restore");
}

#[test]
fn a_damaged_record_repaired_with_a_new_time_is_listed_again_at_once() {
    let f = fixture();
    for i in 0..6 {
        store_run(&f.root, &run_id(i), i);
    }
    std::fs::write(record_file(&f.root, &run_id(2)), "{").unwrap();
    set_modified(&f.root, &run_id(2), 2);
    assert_eq!(all_pages(&f.service, 2).last().unwrap().unreadable, 1);
    store_run(&f.root, &run_id(2), 3);
    let pages = all_pages(&f.service, 2);
    assert!(pages.iter().all(|p| p.unreadable == 0));
    assert_each_once(&pages.iter().flat_map(ids).collect::<Vec<_>>(), (0..6).map(run_id), "repaired");
}

/// The record of damaged files is keyed by modification time alone. On a
/// file system with coarse times (or a repair that keeps the file's time: a
/// copy back from a backup with times preserved) the repaired record has the
/// time the damaged one had, and stays out of the history, counted as
/// unreadable, until the project is reopened.
#[test]
#[ignore = "review: defect — a record repaired without a change of modification time stays 'unreadable' and unlisted"]
fn a_damaged_record_repaired_at_the_same_time_is_listed_again() {
    let f = fixture();
    for i in 0..6 {
        store_run(&f.root, &run_id(i), i);
    }
    std::fs::write(record_file(&f.root, &run_id(2)), "{").unwrap();
    set_modified(&f.root, &run_id(2), 2);
    assert_eq!(all_pages(&f.service, 2).last().unwrap().unreadable, 1);
    // Repaired within the same tick of the file system's clock.
    store_run(&f.root, &run_id(2), 2);
    assert!(f.service.run_manifest(&run_id(2)).is_ok(), "the record is a readable manifest");
    assert_eq!(f.service.run_records().unwrap().len(), 6, "Storage & History lists it");
    let pages = all_pages(&f.service, 2);
    assert_eq!(pages.last().unwrap().unreadable, 0, "nothing is unreadable any more");
    assert_each_once(&pages.iter().flat_map(ids).collect::<Vec<_>>(), (0..6).map(run_id), "repaired");
}

/// A read that fails for a passing reason (another program holds the file
/// without sharing it: a scanner, a sync client, an editor) marks an intact
/// record as damaged; it then stays out of every later listing.
#[cfg(windows)]
#[test]
#[ignore = "review: defect — a record that could not be opened once is hidden as 'unreadable' in later listings although it is intact"]
fn a_record_locked_during_one_listing_is_listed_by_the_next() {
    use std::os::windows::fs::OpenOptionsExt;
    let f = fixture();
    for i in 0..6 {
        store_run(&f.root, &run_id(i), i);
    }
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(record_file(&f.root, &run_id(3))).unwrap();
    let during = page(&f.service, None, 500);
    // While it cannot be read it is reported, not silently dropped.
    assert_eq!(during.runs.len() as u32 + during.unreadable, 6);
    drop(held);
    let after = page(&f.service, None, 500);
    assert_eq!(after.unreadable, 0, "the record is intact and readable");
    assert_eq!(after.runs.len(), 6, "an intact run is missing from the history");
}

/// Records are rewritten (as the service does when a finished run records
/// late output) while the history is listed: every listing of the first page
/// holds every run, none is dropped or counted unreadable in passing.
#[test]
fn records_rewritten_while_the_history_is_read_are_never_dropped() {
    let f = fixture();
    let records = 24u64;
    for i in 0..records {
        store_run(&f.root, &run_id(i), i);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (root, stop) = (f.root.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let id = run_id(n % records);
                write_manifest(&root, &manifest(&id, 9_000 + n)).unwrap();
                n += 1;
            }
            n
        })
    };
    let mut short = Vec::new();
    for round in 0..400 {
        let page = page(&f.service, None, 500);
        if page.runs.len() as u64 != records || page.unreadable != 0 || page.total as u64 != records {
            short.push((round, page.runs.len(), page.unreadable, page.total));
        }
    }
    stop.store(true, Ordering::Relaxed);
    let written = writer.join().unwrap();
    assert!(written > 100, "the writer ran ({written})");
    // Once the writer has stopped the listing is whole whatever happened.
    let settled = page(&f.service, None, 500);
    assert_eq!((settled.runs.len() as u64, settled.unreadable), (records, 0), "after the writes: {short:?}");
    assert!(short.is_empty(), "listings (round, runs, unreadable, total) that were short: {short:?}");
}

// ---- cost -----------------------------------------------------------------

#[test]
fn a_page_deep_in_a_large_history_reads_only_its_own_manifests() {
    let f = fixture();
    let stored = 1_300u64;
    for i in 0..stored {
        store_run(&f.root, &run_id(i), i / 3);
    }
    let mut after = None;
    let mut listed = Vec::new();
    let mut requests = 0;
    loop {
        let before = f.service.history_counters();
        let page = page(&f.service, after, 100);
        let now = f.service.history_counters();
        requests += 1;
        assert_eq!(now.scans - before.scans, 1, "request {requests}");
        assert_eq!(now.manifests_read - before.manifests_read, page.runs.len() as u64, "request {requests}");
        assert_eq!(page.total as u64, stored);
        listed.extend(ids(&page));
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(requests, 13);
    assert_each_once(&listed, (0..stored).map(run_id), "1,300 records");
    // The default page and the largest: 500 manifests of 1,300.
    let before = f.service.history_counters();
    assert_eq!(f.service.runs().unwrap().len(), 500);
    assert_eq!(page(&f.service, None, u32::MAX).runs.len(), 500);
    let now = f.service.history_counters();
    assert_eq!((now.scans - before.scans, now.manifests_read - before.manifests_read), (2, 1_000));
    // Looking one run up reads no page.
    let before = f.service.history_counters();
    assert_eq!(f.service.run_summary(&run_id(0)).unwrap().id.0, run_id(0));
    assert_eq!(f.service.history_counters(), before);
    // Storage & History lists every record.
    assert_eq!(f.service.run_records().unwrap().len(), stored as usize);
    assert_eq!(f.service.history_counters(), before, "the record list is not a history page");
}

#[test]
fn damaged_records_are_read_once_however_often_the_history_is_listed() {
    let f = fixture();
    for i in 0..60 {
        store_run(&f.root, &run_id(i), i);
    }
    for i in (0..60).filter(|i| i % 3 == 0) {
        std::fs::write(record_file(&f.root, &run_id(i)), "not a manifest").unwrap();
        set_modified(&f.root, &run_id(i), i);
    }
    let first = all_pages(&f.service, 10);
    assert_eq!(first.last().unwrap().unreadable, 20);
    for _ in 0..3 {
        let before = f.service.history_counters();
        let pages = all_pages(&f.service, 10);
        assert_eq!(pages.len(), 4);
        assert!(pages.iter().all(|p| p.unreadable == 20 && p.total == 60));
        assert_eq!(f.service.history_counters().manifests_read - before.manifests_read, 40);
    }
}

// ---- the index under two requests at once ---------------------------------

/// `error` and `store` as `src/history.rs` names them, so that the index can
/// be driven directly (it is private to the crate).
mod error {
    pub use exposition_research::ResearchError;
}
mod store {
    pub use exposition_research::store::valid_run_id;
}
#[allow(dead_code, unused_imports)]
#[path = "../src/history.rs"]
mod history;

/// The service lists the folder before it takes the index (`run_history`:
/// `scan`, then `plan`), so two requests can reach the index in the other
/// order than they listed the folder. The index then takes the older listing
/// as the folder's content and forgets a record that the newer listing (and a
/// continuation made from it) already knows. A later rewrite of that record
/// is not logged, and the continuation it crosses is answered.
mod index_order {
    use std::path::Path;

    use super::history::{HISTORY_CHANGED, HistoryIndex, Position};

    fn listing(files: &[(u128, &str)]) -> Vec<Position> {
        let mut out: Vec<Position> =
            files.iter().map(|(m, id)| Position { modified: *m, id: (*id).to_owned() }).collect();
        out.sort_by(|a, b| b.cmp(a));
        out
    }

    const NOT_LIVE: &dyn Fn(&str) -> bool = &|_| false;

    #[test]
    #[ignore = "review: defect — a request that listed the folder earlier but reaches the index later makes it forget a record; a run is then returned twice"]
    fn an_older_listing_arriving_late_does_not_let_a_run_be_returned_twice() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        // Request A lists the folder (run-x is not written yet).
        let listed_by_a = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        // run-x is written; request B lists the folder and gets its first
        // page: run-x and run-c.
        let listed_by_b = listing(&[(40, "run-x"), (30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let page = index.plan(root, &listed_by_b, None, NOT_LIVE).unwrap();
        let next = page.continuation(&page.candidates[1]);
        // Request A reaches the index only now.
        index.plan(root, &listed_by_a, None, NOT_LIVE).unwrap();
        // run-x is rewritten with an earlier time (a restored copy, a clock
        // set back): it now stands after the continuation.
        let now = listing(&[(30, "run-c"), (20, "run-b"), (15, "run-x"), (10, "run-a")]);
        match index.plan(root, &now, Some(&next), NOT_LIVE) {
            Err(refused) => assert_eq!(refused.code(), HISTORY_CHANGED),
            Ok(plan) => assert!(
                plan.candidates.iter().all(|file| file.id != "run-x"),
                "run-x was returned on the first page and is returned again: {:?}",
                plan.candidates
            ),
        }
    }

    #[test]
    #[ignore = "review: defect — a request that listed the folder earlier but reaches the index later makes it forget a record; a run is then never returned"]
    fn an_older_listing_arriving_late_does_not_let_a_run_be_skipped() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        // Request A lists the folder while run-r is in the recoverable area.
        let listed_by_a = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        // run-r is restored (its record keeps its old time); request B lists
        // the folder and gets a first page of two runs. run-r is to come.
        let listed_by_b = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a"), (5, "run-r")]);
        let page = index.plan(root, &listed_by_b, None, NOT_LIVE).unwrap();
        let next = page.continuation(&page.candidates[1]);
        // Request A reaches the index only now.
        index.plan(root, &listed_by_a, None, NOT_LIVE).unwrap();
        // run-r is rewritten (late output of the run, for one): newest now.
        let now = listing(&[(50, "run-r"), (30, "run-c"), (20, "run-b"), (10, "run-a")]);
        match index.plan(root, &now, Some(&next), NOT_LIVE) {
            Err(refused) => assert_eq!(refused.code(), HISTORY_CHANGED),
            Ok(plan) => assert!(
                plan.candidates.iter().any(|file| file.id == "run-r"),
                "run-r existed for the whole listing and is on no page: {:?}",
                plan.candidates
            ),
        }
    }

    /// The same sequences with the requests reaching the index in the order
    /// they listed the folder: refused, as the contract says.
    #[test]
    fn in_order_the_same_rewrites_refuse_the_continuation() {
        let root = Path::new("/project");
        let index = HistoryIndex::default();
        index.plan(root, &listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]), None, NOT_LIVE).unwrap();
        let listed = listing(&[(40, "run-x"), (30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let page = index.plan(root, &listed, None, NOT_LIVE).unwrap();
        let next = page.continuation(&page.candidates[1]);
        let now = listing(&[(30, "run-c"), (20, "run-b"), (15, "run-x"), (10, "run-a")]);
        assert_eq!(index.plan(root, &now, Some(&next), NOT_LIVE).unwrap_err().code(), HISTORY_CHANGED);

        let index = HistoryIndex::default();
        index.plan(root, &listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]), None, NOT_LIVE).unwrap();
        let listed = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a"), (5, "run-r")]);
        let page = index.plan(root, &listed, None, NOT_LIVE).unwrap();
        let next = page.continuation(&page.candidates[1]);
        let now = listing(&[(50, "run-r"), (30, "run-c"), (20, "run-b"), (10, "run-a")]);
        assert_eq!(index.plan(root, &now, Some(&next), NOT_LIVE).unwrap_err().code(), HISTORY_CHANGED);
    }
}
