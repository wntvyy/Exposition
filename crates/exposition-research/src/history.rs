//! Paged listing of the run records in a project.
//!
//! Records are listed by their file's modification time, newest first (then
//! by id, descending): the order is known from the folder listing alone, so a
//! page reads only the manifests it returns, however many records the folder
//! holds.
//!
//! A continuation names the position of the last record a page examined.
//! Records created later are newer than every position already handed out and
//! deleted records simply no longer appear, so neither moves a record across
//! a position: paging on returns every remaining record once. Rewriting a
//! record changes its modification time and can move it across a position
//! (it would be returned twice, or never). The index remembers each record's
//! modification time from one listing to the next and logs such changes; a
//! continuation that a logged change crossed is refused with
//! `research.historyChanged` instead of answered with a page that looks
//! complete.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use crate::error::ResearchError;
use crate::store::valid_run_id;

/// Code of the refusal of a continuation that no longer describes the listing.
pub const HISTORY_CHANGED: &str = "research.historyChanged";
/// Record changes remembered for outstanding continuations.
const MOVES_KEPT: usize = 4096;

/// Where a record stands in the listing. Positions order by modification
/// time, then id; the listing is in descending order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Position {
    /// Nanoseconds since the Unix epoch.
    pub modified: u128,
    pub id: String,
}

/// The record files of `root` (valid run ids only), newest first. Reads the
/// folder listing and each entry's metadata, never a manifest.
pub(crate) fn scan(root: &Path) -> Vec<Position> {
    let dir = root.join(".exposition").join("runs");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut files: Vec<Position> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?.to_owned();
            if !valid_run_id(&id) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            Some(Position { modified, id })
        })
        .collect();
    files.sort_by(|a, b| b.cmp(a));
    files
}

/// A parsed continuation: the listing it belongs to, the changes it has
/// seen, and the last position examined.
struct Cursor {
    epoch: u64,
    serial: u64,
    at: Position,
}

impl Cursor {
    fn parse(text: &str) -> Result<Self, ResearchError> {
        let invalid = || ResearchError::InvalidRequest("the run history continuation is not valid".into());
        let mut parts = text.splitn(5, '.');
        if parts.next() != Some("h1") {
            return Err(invalid());
        }
        let mut number = || parts.next().and_then(|p| u128::from_str_radix(p, 16).ok()).ok_or_else(invalid);
        let epoch = u64::try_from(number()?).map_err(|_| invalid())?;
        let serial = u64::try_from(number()?).map_err(|_| invalid())?;
        let modified = number()?;
        let id = parts.next().filter(|id| valid_run_id(id)).ok_or_else(invalid)?.to_owned();
        Ok(Self { epoch, serial, at: Position { modified, id } })
    }
}

fn encode(epoch: u64, serial: u64, at: &Position) -> String {
    format!("h1.{epoch:x}.{serial:x}.{:x}.{}", at.modified, at.id)
}

/// A record whose modification time changed between two listings.
struct Moved {
    serial: u64,
    id: String,
    /// Its modification time before the change.
    from: u128,
}

struct State {
    root: PathBuf,
    /// Identifies this index: a continuation of another one (another
    /// project, an earlier start of the app) is refused.
    epoch: u64,
    /// Number of the latest logged change.
    serial: u64,
    /// Continuations older than this have lost changes they need to see.
    floor: u64,
    known: HashMap<String, u128>,
    moved: VecDeque<Moved>,
    /// Records that could not be read, by the modification time they had.
    unreadable: HashMap<String, u128>,
}

impl State {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            epoch: uuid::Uuid::new_v4().as_u128() as u64,
            serial: 0,
            floor: 0,
            known: HashMap::new(),
            moved: VecDeque::new(),
            unreadable: HashMap::new(),
        }
    }

    /// Record `listing` as the folder's content, logging every record whose
    /// modification time changed since the previous listing.
    fn observe(&mut self, listing: &[Position]) {
        let mut known = HashMap::with_capacity(listing.len());
        for file in listing {
            if let Some(&before) = self.known.get(&file.id)
                && before != file.modified
            {
                self.serial += 1;
                self.moved.push_back(Moved { serial: self.serial, id: file.id.clone(), from: before });
            }
            known.insert(file.id.clone(), file.modified);
        }
        self.known = known;
        while self.moved.len() > MOVES_KEPT {
            if let Some(dropped) = self.moved.pop_front() {
                self.floor = dropped.serial;
            }
        }
        let known = &self.known;
        self.unreadable.retain(|id, modified| known.get(id) == Some(modified));
    }

    /// Whether `cursor` still splits the listing into the records already
    /// returned and the records to come.
    fn holds(&self, cursor: &Cursor, live: &dyn Fn(&str) -> bool) -> bool {
        if cursor.epoch != self.epoch || cursor.serial < self.floor || cursor.serial > self.serial {
            return false;
        }
        let returned = |modified: u128, id: &str| (modified, id) >= (cursor.at.modified, cursor.at.id.as_str());
        let mut seen = HashSet::new();
        for change in self.moved.iter().filter(|m| m.serial > cursor.serial) {
            // The first change after the continuation holds the time the
            // record had when the continuation was made.
            if !seen.insert(change.id.as_str()) || live(&change.id) {
                continue;
            }
            let Some(&now) = self.known.get(&change.id) else { continue };
            if returned(change.from, &change.id) != returned(now, &change.id) {
                return false;
            }
        }
        true
    }
}

/// What one page reads: the records after the continuation, newest first.
#[derive(Debug)]
pub(crate) struct Plan {
    epoch: u64,
    serial: u64,
    pub candidates: Vec<Position>,
}

impl Plan {
    /// The continuation after `last` (the last candidate examined).
    pub fn continuation(&self, last: &Position) -> String {
        encode(self.epoch, self.serial, last)
    }
}

/// Counts of the work the history listing did (tests and benchmarks).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryCounters {
    /// Folder listings made.
    pub scans: u64,
    /// Manifests read from disk.
    pub manifests_read: u64,
}

#[derive(Default)]
pub(crate) struct HistoryIndex {
    state: Mutex<Option<State>>,
    scans: AtomicU64,
    reads: AtomicU64,
}

impl HistoryIndex {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<State>> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn counters(&self) -> HistoryCounters {
        HistoryCounters {
            scans: self.scans.load(Ordering::Relaxed),
            manifests_read: self.reads.load(Ordering::Relaxed),
        }
    }

    pub fn count_scan(&self) {
        self.scans.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count_read(&self) {
        self.reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Bring the index up to date with `listing` (the folder now, newest
    /// first) and choose what a page after `after` reads: the records past
    /// the continuation that are not `live` (those are listed from memory, on
    /// the first page) and not known to be unreadable.
    pub fn plan(
        &self,
        root: &Path,
        listing: &[Position],
        after: Option<&str>,
        live: &dyn Fn(&str) -> bool,
    ) -> Result<Plan, ResearchError> {
        let cursor = after.map(Cursor::parse).transpose()?;
        let mut guard = self.lock();
        if guard.as_ref().is_none_or(|state| state.root != root) {
            *guard = Some(State::new(root));
        }
        let state = guard.as_mut().expect("set above");
        state.observe(listing);
        if let Some(cursor) = &cursor
            && !state.holds(cursor, live)
        {
            return Err(ResearchError::coded(
                HISTORY_CHANGED,
                "The run history changed since this part of it was listed; list it again from the start.",
            ));
        }
        let candidates = listing
            .iter()
            .filter(|file| cursor.as_ref().is_none_or(|c| **file < c.at))
            .filter(|file| !live(&file.id) && state.unreadable.get(&file.id) != Some(&file.modified))
            .cloned()
            .collect();
        Ok(Plan { epoch: state.epoch, serial: state.serial, candidates })
    }

    /// A record that exists but could not be read as a manifest.
    pub fn note_unreadable(&self, root: &Path, plan: &Plan, file: &Position) {
        if let Some(state) = self.lock().as_mut()
            && state.root == root
            && state.epoch == plan.epoch
        {
            state.unreadable.insert(file.id.clone(), file.modified);
        }
    }

    /// Records of `root` known to be unreadable.
    pub fn unreadable(&self, root: &Path) -> usize {
        self.lock().as_ref().filter(|state| state.root == root).map_or(0, |state| state.unreadable.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(modified: u128, id: &str) -> Position {
        Position { modified, id: id.to_owned() }
    }

    fn listing(files: &[(u128, &str)]) -> Vec<Position> {
        let mut out: Vec<Position> = files.iter().map(|(m, id)| at(*m, id)).collect();
        out.sort_by(|a, b| b.cmp(a));
        out
    }

    const NOT_LIVE: &dyn Fn(&str) -> bool = &|_| false;

    #[test]
    fn continuations_round_trip_and_reject_other_text() {
        let text = encode(7, 3, &at(1_700_000_000_123_456_789, "run-abc"));
        let cursor = Cursor::parse(&text).unwrap();
        assert_eq!((cursor.epoch, cursor.serial), (7, 3));
        assert_eq!(cursor.at, at(1_700_000_000_123_456_789, "run-abc"));
        for bad in ["", "h1", "h2.1.1.1.run-a", "h1.x.1.1.run-a", "h1.1.1.1.../x", "h1.1.1.1.run-A", "h1.1.1.1"] {
            assert!(matches!(Cursor::parse(bad), Err(ResearchError::InvalidRequest(_))), "{bad}");
        }
    }

    #[test]
    fn created_and_deleted_records_keep_a_continuation_valid() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let first = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let plan = index.plan(root, &first, None, NOT_LIVE).unwrap();
        let next = plan.continuation(&plan.candidates[0]);
        // run-b is deleted and run-d created before the next page.
        let second = listing(&[(40, "run-d"), (30, "run-c"), (10, "run-a")]);
        let plan = index.plan(root, &second, Some(&next), NOT_LIVE).unwrap();
        assert_eq!(plan.candidates, vec![at(10, "run-a")]);
    }

    #[test]
    fn a_record_rewritten_across_the_continuation_refuses_it() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let first = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let plan = index.plan(root, &first, None, NOT_LIVE).unwrap();
        let next = plan.continuation(&plan.candidates[0]);
        // A rewrite of a record already returned leaves the rest as it was.
        let touched = listing(&[(50, "run-c"), (20, "run-b"), (10, "run-a")]);
        assert_eq!(index.plan(root, &touched, Some(&next), NOT_LIVE).unwrap().candidates.len(), 2);
        // run-a was still to come and is now newer than the continuation.
        let crossed = listing(&[(60, "run-a"), (50, "run-c"), (20, "run-b")]);
        let refused = index.plan(root, &crossed, Some(&next), NOT_LIVE).unwrap_err();
        assert_eq!(refused.code(), HISTORY_CHANGED);
        // Still refused on a later request (the change stays logged), also
        // when the record changes again.
        assert_eq!(index.plan(root, &crossed, Some(&next), NOT_LIVE).unwrap_err().code(), HISTORY_CHANGED);
        // Unless the record is listed from memory anyway.
        assert!(index.plan(root, &crossed, Some(&next), &|id| id == "run-a").is_ok());
        // A fresh first page is always answered.
        assert_eq!(index.plan(root, &crossed, None, NOT_LIVE).unwrap().candidates.len(), 3);
    }

    #[test]
    fn a_continuation_of_another_listing_is_refused() {
        let index = HistoryIndex::default();
        let files = listing(&[(30, "run-c"), (20, "run-b")]);
        let plan = index.plan(Path::new("/one"), &files, None, NOT_LIVE).unwrap();
        let next = plan.continuation(&plan.candidates[0]);
        let other = index.plan(Path::new("/two"), &files, Some(&next), NOT_LIVE).unwrap_err();
        assert_eq!(other.code(), HISTORY_CHANGED);
        // Back in the first project the index started again.
        assert_eq!(index.plan(Path::new("/one"), &files, Some(&next), NOT_LIVE).unwrap_err().code(), HISTORY_CHANGED);
    }

    #[test]
    fn a_continuation_older_than_the_change_log_is_refused() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let mut files = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let plan = index.plan(root, &files, None, NOT_LIVE).unwrap();
        let next = plan.continuation(&plan.candidates[0]);
        // run-c (already returned) is rewritten more often than the log holds.
        for i in 0..=MOVES_KEPT as u128 + 1 {
            files[0].modified = 100 + i;
            index.plan(root, &files, None, NOT_LIVE).unwrap();
        }
        assert_eq!(index.plan(root, &files, Some(&next), NOT_LIVE).unwrap_err().code(), HISTORY_CHANGED);
    }

    #[test]
    fn unreadable_records_are_skipped_until_they_change() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let files = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let plan = index.plan(root, &files, None, NOT_LIVE).unwrap();
        index.note_unreadable(root, &plan, &at(20, "run-b"));
        assert_eq!(index.unreadable(root), 1);
        let plan = index.plan(root, &files, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates, vec![at(30, "run-c"), at(10, "run-a")]);
        let repaired = listing(&[(40, "run-b"), (30, "run-c"), (10, "run-a")]);
        assert_eq!(index.plan(root, &repaired, None, NOT_LIVE).unwrap().candidates.len(), 3);
        assert_eq!(index.unreadable(root), 0);
    }
}
