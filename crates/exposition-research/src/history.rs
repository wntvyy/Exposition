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
//! a position: paging on returns every remaining record once. A record whose
//! modification time changes (it is rewritten, or replaced by another copy)
//! can move across a position (it would be returned twice, or never). The
//! index remembers the modification time of every record it has seen, also
//! of records that are gone for now, and logs each change of it; a
//! continuation that a logged change crossed is refused with
//! `research.historyChanged` instead of answered with a page that looks
//! complete.
//!
//! The service lists the folder while it holds the index, so listings reach
//! the index in the order they were made. The index does not depend on it: a
//! listing older than the one before it only adds changes to the log, and
//! since no record's time is ever forgotten, a change that a continuation
//! needs to see is logged whichever listing arrives first.
//!
//! A record that is not a manifest of its run is remembered by modification
//! time, size and [`Stamp`] (the file's identity and its change time) and
//! not read again until one of them changes; each page (each folder
//! listing) checks the stamps of the damaged records whose time and size are
//! unchanged (a metadata query each, no read). A repair that keeps time and
//! size is so seen by the next page, whether it replaces the file (as this
//! app's store writes) or writes into it (as copy tools do on Windows:
//! CopyFile, `copy`, `Copy-Item`, robocopy, Explorer): the change time moves
//! with every write and with setting the modification time back. Two
//! rewrites within one tick of the file system's clock (NTFS: the system
//! timer, 1 to 15.6 ms; Linux: a scheduler tick) that also keep size and
//! modification time cannot be told apart. A record that could not be
//! opened (another program holds it) is counted, never remembered as
//! damaged, and read again by the next page that reaches it.

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
/// Records that are gone whose modification time is still remembered.
const GONE_KEPT: usize = 16_384;

/// Where a record stands in the listing. Positions order by modification
/// time, then id; the listing is in descending order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Position {
    /// Nanoseconds since the Unix epoch.
    pub modified: u128,
    pub id: String,
}

/// Waits between the further tries for a record that is not there while it
/// is being replaced (the folder listing of
/// [`HistoryIndex::plan_listing`], and the read of one record).
pub(crate) const REPLACED_WAITS_MS: [u64; 5] = [5, 10, 20, 40, 80];

/// A record file as the folder lists it.
#[derive(Clone, Debug)]
pub(crate) struct Listed {
    pub at: Position,
    /// Size of the file in bytes.
    pub bytes: u64,
}

/// The record files of `root` (valid run ids only), newest first. Reads the
/// folder listing and each entry's metadata, never a manifest.
pub(crate) fn scan(root: &Path) -> Vec<Position> {
    scan_sized(root).into_iter().map(|file| file.at).collect()
}

fn listed(id: String, metadata: &std::fs::Metadata) -> Option<Listed> {
    if !metadata.is_file() {
        return None;
    }
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    Some(Listed { at: Position { modified, id }, bytes: metadata.len() })
}

/// [`scan`] with each file's size.
pub(crate) fn scan_sized(root: &Path) -> Vec<Listed> {
    let dir = root.join(".exposition").join("runs");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut files: Vec<Listed> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?.to_owned();
            if !valid_run_id(&id) {
                return None;
            }
            listed(id, &entry.metadata().ok()?)
        })
        .collect();
    files.sort_by(|a, b| b.at.cmp(&a.at));
    files
}

/// The record file of `id` asked for by name: `None` when there is none.
/// A folder listing made while a record is being replaced can leave that
/// record out; a record the listing no longer has is looked for this way
/// before it is taken as gone.
pub(crate) fn stat(root: &Path, id: &str) -> Option<Listed> {
    if !valid_run_id(id) {
        return None;
    }
    let path = root.join(".exposition").join("runs").join(format!("{id}.json"));
    listed(id.to_owned(), &std::fs::symlink_metadata(path).ok()?)
}

/// What tells one version of a record file from another without reading
/// it, beyond its modification time and size: the file's identity and its
/// change time ([`exposition_core::identity::FileVersion`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp([u64; 4]);

impl From<exposition_core::identity::FileVersion> for Stamp {
    fn from(version: exposition_core::identity::FileVersion) -> Self {
        Self(version.words())
    }
}

/// Gives a record's [`Stamp`] now, by run id.
type StampOf<'a> = &'a dyn Fn(&str) -> Option<Stamp>;

/// The stamp of an open file. Taken before its content is read, so that a
/// change made after the stamp is a change of the stamp.
pub(crate) fn file_stamp(file: &std::fs::File) -> Option<Stamp> {
    exposition_core::identity::file_version(file).map(Stamp::from)
}

/// The stamp of `id`'s record file now: `None` when there is none or it
/// cannot be queried (on Windows it is opened for its attributes only, which
/// another program holding it does not prevent).
pub(crate) fn stamp(root: &Path, id: &str) -> Option<Stamp> {
    if !valid_run_id(id) {
        return None;
    }
    let path = root.join(".exposition").join("runs").join(format!("{id}.json"));
    exposition_core::identity::path_version(&path).map(Stamp::from)
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

/// The last modification time seen for a record, kept while it is gone.
struct Seen {
    modified: u128,
    /// In the latest listing applied.
    present: bool,
}

/// A record that was read and is not a manifest of its run, as it was then.
struct Damaged {
    /// Its modification time in the listing it was read from.
    modified: u128,
    /// Its size as read.
    bytes: u64,
    /// Its stamp before it was read. Without one nothing proves it
    /// unchanged: it is dropped by the next listing and read again.
    stamp: Option<Stamp>,
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
    known: HashMap<String, Seen>,
    moved: VecDeque<Moved>,
    /// Records that are not a manifest of their run, by the version they
    /// had when they were read.
    damaged: HashMap<String, Damaged>,
    /// Records that could not be opened when a page last tried.
    unopened: HashSet<String>,
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
            damaged: HashMap::new(),
            unopened: HashSet::new(),
        }
    }

    /// Apply `listing` as the folder's content, logging every record whose
    /// modification time differs from the one last seen for it (also when
    /// the record was gone in between: its time is not forgotten). `sizes`
    /// are the files' sizes where the listing has them; `stamps` gives a
    /// record's [`Stamp`] now, asked for each damaged record whose time and
    /// size are unchanged (without it the stamps are not compared).
    fn observe(&mut self, listing: &[Position], sizes: Option<&HashMap<&str, u64>>, stamps: Option<StampOf<'_>>) {
        let listed: HashSet<&str> = listing.iter().map(|file| file.id.as_str()).collect();
        let mut gone = 0;
        for (id, seen) in &mut self.known {
            seen.present = listed.contains(id.as_str());
            gone += usize::from(!seen.present);
        }
        for file in listing {
            match self.known.get_mut(&file.id) {
                Some(seen) => {
                    if seen.modified != file.modified {
                        self.serial += 1;
                        self.moved.push_back(Moved { serial: self.serial, id: file.id.clone(), from: seen.modified });
                        seen.modified = file.modified;
                    }
                    seen.present = true;
                }
                None => {
                    self.known.insert(file.id.clone(), Seen { modified: file.modified, present: true });
                }
            }
        }
        while self.moved.len() > MOVES_KEPT {
            if let Some(dropped) = self.moved.pop_front() {
                self.floor = dropped.serial;
            }
        }
        if gone > GONE_KEPT {
            // The times of the records that are gone are dropped: every
            // continuation made so far may need one of them.
            self.known.retain(|_, seen| seen.present);
            self.serial += 1;
            self.floor = self.serial;
        }
        let known = &self.known;
        let present = |id: &str| known.get(id).filter(|seen| seen.present);
        self.damaged.retain(|id, damaged| {
            present(id).is_some_and(|seen| seen.modified == damaged.modified)
                && sizes.is_none_or(|sizes| sizes.get(id.as_str()) == Some(&damaged.bytes))
                && stamps.is_none_or(|stamp| damaged.stamp.is_some() && stamp(id.as_str()) == damaged.stamp)
        });
        self.unopened.retain(|id| present(id).is_some());
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
            // A record that is gone is in no later page.
            let Some(now) = self.known.get(&change.id).filter(|seen| seen.present) else { continue };
            if returned(change.from, &change.id) != returned(now.modified, &change.id) {
                return false;
            }
        }
        true
    }

    fn plan(
        &mut self,
        listing: Vec<Position>,
        sizes: Option<&HashMap<&str, u64>>,
        stamps: Option<StampOf<'_>>,
        cursor: Option<&Cursor>,
        live: &dyn Fn(&str) -> bool,
    ) -> Result<Plan, ResearchError> {
        self.observe(&listing, sizes, stamps);
        if let Some(cursor) = cursor
            && !self.holds(cursor, live)
        {
            return Err(ResearchError::coded(
                HISTORY_CHANGED,
                "The run history changed since this part of it was listed; list it again from the start.",
            ));
        }
        let candidates = listing
            .iter()
            .filter(|file| cursor.is_none_or(|c| **file < c.at))
            .filter(|file| !live(&file.id) && self.damaged.get(&file.id).is_none_or(|d| d.modified != file.modified))
            .cloned()
            .collect();
        Ok(Plan { epoch: self.epoch, serial: self.serial, candidates, listing })
    }
}

/// What one page reads: the records after the continuation, newest first.
#[derive(Debug)]
pub(crate) struct Plan {
    epoch: u64,
    serial: u64,
    pub candidates: Vec<Position>,
    /// The folder as it was listed for this page.
    pub listing: Vec<Position>,
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
    /// Stamps of damaged records asked for (a metadata query each, no read):
    /// one per damaged record whose time and size are unchanged, per page.
    pub stamps_checked: u64,
}

#[derive(Default)]
pub(crate) struct HistoryIndex {
    state: Mutex<Option<State>>,
    scans: AtomicU64,
    reads: AtomicU64,
    stamps: AtomicU64,
}

impl HistoryIndex {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<State>> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn counters(&self) -> HistoryCounters {
        HistoryCounters {
            scans: self.scans.load(Ordering::Relaxed),
            manifests_read: self.reads.load(Ordering::Relaxed),
            stamps_checked: self.stamps.load(Ordering::Relaxed),
        }
    }

    pub fn count_read(&self) {
        self.reads.fetch_add(1, Ordering::Relaxed);
    }

    fn state<'a>(guard: &'a mut Option<State>, root: &Path) -> &'a mut State {
        if guard.as_ref().is_none_or(|state| state.root != root) {
            *guard = Some(State::new(root));
        }
        guard.as_mut().expect("set above")
    }

    /// Apply `listing` (the folder, newest first) to the index and choose
    /// what a page after `after` reads: the records past the continuation
    /// that are not `live` (those are listed from memory, on the first page)
    /// and not known to be damaged. The service lists the folder itself
    /// ([`Self::plan_listing`]); tests drive the index with listings in any
    /// order through this.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn plan(
        &self,
        root: &Path,
        listing: &[Position],
        after: Option<&str>,
        live: &dyn Fn(&str) -> bool,
    ) -> Result<Plan, ResearchError> {
        let cursor = after.map(Cursor::parse).transpose()?;
        let mut guard = self.lock();
        Self::state(&mut guard, root).plan(listing.to_vec(), None, None, cursor.as_ref(), live)
    }

    /// [`Self::plan`] for the folder as `scan` lists it now. The folder is
    /// listed while the index is held, so listings are applied in the order
    /// they are made (other pages wait for the listing; nothing else does).
    /// A record the previous listing had and this one does not is asked for
    /// by name (`find`) before it is taken as gone. One that is not found by
    /// name either while it is being replaced (`replacing`) is asked for
    /// again after each of a few short waits ([`REPLACED_WAITS_MS`], per
    /// listing, not per record); one that is not being replaced is asked for
    /// once more (the replacement may just have finished). `stamp` gives a
    /// record's [`Stamp`] now (asked for damaged records only).
    #[allow(clippy::too_many_arguments)]
    pub fn plan_listing(
        &self,
        root: &Path,
        scan: impl FnOnce() -> Vec<Listed>,
        find: impl Fn(&str) -> Option<Listed>,
        replacing: impl Fn(&str) -> bool,
        stamp: impl Fn(&str) -> Option<Stamp>,
        after: Option<&str>,
        live: &dyn Fn(&str) -> bool,
    ) -> Result<Plan, ResearchError> {
        let cursor = after.map(Cursor::parse).transpose()?;
        let mut guard = self.lock();
        self.scans.fetch_add(1, Ordering::Relaxed);
        let mut files = scan();
        if let Some(state) = guard.as_ref().filter(|state| state.root == root) {
            let listed: HashSet<&str> = files.iter().map(|file| file.at.id.as_str()).collect();
            let mut found: Vec<Listed> = Vec::new();
            let mut missing: Vec<&str> = state
                .known
                .iter()
                .filter(|(id, seen)| seen.present && !listed.contains(id.as_str()))
                .map(|(id, _)| id.as_str())
                .collect();
            let mut ask = |missing: &mut Vec<&str>| {
                missing.retain(|id| match find(id) {
                    Some(file) => {
                        found.push(file);
                        false
                    }
                    None if replacing(id) => true,
                    None => {
                        found.extend(find(id));
                        false
                    }
                });
            };
            ask(&mut missing);
            for wait in REPLACED_WAITS_MS {
                if missing.is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(wait));
                ask(&mut missing);
            }
            if !found.is_empty() {
                files.extend(found);
                files.sort_by(|a, b| b.at.cmp(&a.at));
            }
        }
        let sizes: HashMap<&str, u64> = files.iter().map(|file| (file.at.id.as_str(), file.bytes)).collect();
        let listing = files.iter().map(|file| file.at.clone()).collect();
        let counted = |id: &str| {
            self.stamps.fetch_add(1, Ordering::Relaxed);
            stamp(id)
        };
        Self::state(&mut guard, root).plan(listing, Some(&sizes), Some(&counted), cursor.as_ref(), live)
    }

    fn noted(&self, root: &Path, plan: &Plan, note: impl FnOnce(&mut State)) {
        if let Some(state) = self.lock().as_mut()
            && state.root == root
            && state.epoch == plan.epoch
        {
            note(state);
        }
    }

    /// A record of `bytes` bytes that was read and is not a manifest of its
    /// run; `stamp` is its [`Stamp`] taken before it was read.
    pub fn note_damaged(&self, root: &Path, plan: &Plan, file: &Position, bytes: u64, stamp: Option<Stamp>) {
        self.noted(root, plan, |state| {
            state.unopened.remove(&file.id);
            state.damaged.insert(file.id.clone(), Damaged { modified: file.modified, bytes, stamp });
        });
    }

    /// A record that exists and could not be opened.
    pub fn note_unopened(&self, root: &Path, plan: &Plan, file: &Position) {
        self.noted(root, plan, |state| {
            state.unopened.insert(file.id.clone());
        });
    }

    /// Records that were read as manifests (or are gone).
    pub fn note_settled(&self, root: &Path, plan: &Plan, ids: &[&str]) {
        self.noted(root, plan, |state| {
            if !state.unopened.is_empty() {
                for id in ids {
                    state.unopened.remove(*id);
                }
            }
        });
    }

    /// Records of `root` that are in no page: damaged, or not opened when
    /// last tried.
    pub fn unreadable(&self, root: &Path) -> usize {
        self.lock().as_ref().filter(|state| state.root == root).map_or(0, |state| {
            state.damaged.len() + state.unopened.iter().filter(|id| !state.damaged.contains_key(*id)).count()
        })
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

    const STAMP: Option<Stamp> = Some(Stamp([1, 2, 3, 4]));

    #[test]
    fn damaged_records_are_skipped_until_their_time_or_size_changes() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let files = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let sized = |files: &[Position], bytes: u64| -> Vec<Listed> {
            files.iter().map(|at| Listed { at: at.clone(), bytes: if at.id == "run-b" { bytes } else { 7 } }).collect()
        };
        let same = |_: &str| STAMP;
        let plan = index.plan_listing(root, || sized(&files, 1), |_| None, |_| false, same, None, NOT_LIVE).unwrap();
        index.note_damaged(root, &plan, &at(20, "run-b"), 1, STAMP);
        assert_eq!(index.unreadable(root), 1);
        let plan = index.plan_listing(root, || sized(&files, 1), |_| None, |_| false, same, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates, vec![at(30, "run-c"), at(10, "run-a")]);
        // Another size at the same time: read again.
        let plan = index.plan_listing(root, || sized(&files, 900), |_| None, |_| false, same, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates.len(), 3);
        assert_eq!(index.unreadable(root), 0);
        // Another time.
        index.note_damaged(root, &plan, &at(20, "run-b"), 900, STAMP);
        let repaired = listing(&[(40, "run-b"), (30, "run-c"), (10, "run-a")]);
        assert_eq!(
            index
                .plan_listing(root, || sized(&repaired, 900), |_| None, |_| false, same, None, NOT_LIVE)
                .unwrap()
                .candidates
                .len(),
            3
        );
        assert_eq!(index.unreadable(root), 0);
    }

    #[test]
    fn damaged_records_are_read_again_when_their_stamp_changes_or_is_unknown() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let files = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let sized = || -> Vec<Listed> { files.iter().map(|at| Listed { at: at.clone(), bytes: 7 }).collect() };
        let current = std::cell::Cell::new(STAMP);
        let asked = std::cell::Cell::new(0);
        let stamp = |id: &str| {
            assert_eq!(id, "run-b", "only damaged records are stamped");
            asked.set(asked.get() + 1);
            current.get()
        };
        let plan = index.plan_listing(root, sized, |_| None, |_| false, stamp, None, NOT_LIVE).unwrap();
        assert_eq!((plan.candidates.len(), asked.get()), (3, 0), "nothing is damaged yet");
        index.note_damaged(root, &plan, &at(20, "run-b"), 7, STAMP);
        let before = index.counters().stamps_checked;
        let plan = index.plan_listing(root, sized, |_| None, |_| false, stamp, None, NOT_LIVE).unwrap();
        assert_eq!((plan.candidates.len(), asked.get()), (2, 1), "unchanged: skipped after one stamp");
        assert_eq!(index.counters().stamps_checked - before, 1);
        // The same time and size, another file (replaced, or rewritten where the stamp shows it).
        current.set(Some(Stamp([1, 9, 3, 4])));
        let plan = index.plan_listing(root, sized, |_| None, |_| false, stamp, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates.len(), 3, "read again");
        assert_eq!(index.unreadable(root), 0);
        // Read as damaged without a stamp: nothing proves it unchanged.
        index.note_damaged(root, &plan, &at(20, "run-b"), 7, None);
        assert_eq!(index.unreadable(root), 1);
        let plan = index.plan_listing(root, sized, |_| None, |_| false, stamp, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates.len(), 3, "read again");
        // No stamp now: read again too.
        index.note_damaged(root, &plan, &at(20, "run-b"), 7, STAMP);
        current.set(None);
        let plan = index.plan_listing(root, sized, |_| None, |_| false, stamp, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates.len(), 3, "read again");
    }

    #[test]
    fn a_stamp_follows_a_replacement_and_holds_for_an_unchanged_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let runs = root.join(".exposition").join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let file = runs.join("run-ab.json");
        std::fs::write(&file, "one").unwrap();
        let first = stamp(root, "run-ab");
        assert!(first.is_some());
        assert_eq!(stamp(root, "run-ab"), first, "an unchanged file keeps its stamp");
        assert_eq!(file_stamp(&std::fs::File::open(&file).unwrap()), first, "by name and by handle");
        let fresh = runs.join("run-ab.fresh");
        std::fs::write(&fresh, "two").unwrap();
        std::fs::rename(&fresh, &file).unwrap();
        assert_ne!(stamp(root, "run-ab"), first, "a file put in its place has another stamp");
        // Rewritten in place with its size and modification time kept.
        let replaced = stamp(root, "run-ab");
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(40));
        std::fs::write(&file, "TWO").unwrap();
        std::fs::OpenOptions::new().write(true).open(&file).unwrap().set_modified(modified).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), modified);
        assert_ne!(stamp(root, "run-ab"), replaced, "a rewrite in place moves the change time");
        assert_eq!(stamp(root, "run-cd"), None, "no such record");
        assert_eq!(stamp(root, "../x"), None, "not a run id");
    }

    #[test]
    fn a_record_missing_from_a_listing_is_asked_for_by_name() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let files = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let sized = |files: &[Position]| -> Vec<Listed> {
            files.iter().map(|at| Listed { at: at.clone(), bytes: 7 }).collect()
        };
        index.plan_listing(root, || sized(&files), |_| None, |_| false, |_| None, None, NOT_LIVE).unwrap();
        // The listing leaves run-b out although the file is there.
        let short = listing(&[(30, "run-c"), (10, "run-a")]);
        let asked = std::cell::RefCell::new(Vec::new());
        let find = |id: &str| {
            asked.borrow_mut().push(id.to_owned());
            Some(Listed { at: at(20, id), bytes: 7 })
        };
        let plan = index.plan_listing(root, || sized(&short), find, |_| false, |_| None, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates, files);
        assert_eq!(plan.listing.len(), 3);
        assert_eq!(*asked.borrow(), vec!["run-b".to_owned()], "only the record that went missing");
        // Really gone: not in the page, and not asked for again.
        let plan = index.plan_listing(root, || sized(&short), |_| None, |_| false, |_| None, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates, short);
        index
            .plan_listing(root, || sized(&short), |_| panic!("asked again"), |_| false, |_| None, None, NOT_LIVE)
            .unwrap();
    }

    #[test]
    fn a_record_not_found_while_it_is_replaced_is_asked_for_again() {
        let root = Path::new("/project");
        let files = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let short = listing(&[(30, "run-c"), (10, "run-a")]);
        let sized = |files: &[Position]| -> Vec<Listed> {
            files.iter().map(|at| Listed { at: at.clone(), bytes: 7 }).collect()
        };
        let known = || {
            let index = HistoryIndex::default();
            index.plan_listing(root, || sized(&files), |_| None, |_| false, |_| None, None, NOT_LIVE).unwrap();
            index
        };
        let counter = std::cell::Cell::new(0);
        let asks = &counter;
        let found_from = |ask: usize| {
            asks.set(0);
            move |id: &str| {
                asks.set(asks.get() + 1);
                (asks.get() >= ask).then(|| Listed { at: at(25, id), bytes: 7 })
            }
        };
        let replaced = listing(&[(30, "run-c"), (25, "run-b"), (10, "run-a")]);
        // Being replaced: neither listed nor found for two asks.
        let plan =
            known().plan_listing(root, || sized(&short), found_from(3), |_| true, |_| None, None, NOT_LIVE).unwrap();
        assert_eq!((asks.get(), &plan.candidates), (3, &replaced));
        // A replacement that never ends: asked for after every wait, then gone.
        let plan =
            known().plan_listing(root, || sized(&short), found_from(99), |_| true, |_| None, None, NOT_LIVE).unwrap();
        assert_eq!((asks.get(), &plan.candidates), (1 + REPLACED_WAITS_MS.len(), &short));
        // Not being replaced: the replacement may have ended since the first
        // ask, so it is asked for once more, without a wait.
        let started = std::time::Instant::now();
        let plan =
            known().plan_listing(root, || sized(&short), found_from(2), |_| false, |_| None, None, NOT_LIVE).unwrap();
        assert_eq!((asks.get(), &plan.candidates), (2, &replaced));
        let plan =
            known().plan_listing(root, || sized(&short), found_from(99), |_| false, |_| None, None, NOT_LIVE).unwrap();
        assert_eq!((asks.get(), &plan.candidates), (2, &short));
        assert!(
            started.elapsed().as_millis() < u128::from(REPLACED_WAITS_MS[0]) * 4,
            "no wait for a record that is gone"
        );
    }

    #[test]
    fn a_record_that_could_not_be_opened_is_counted_and_tried_again() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let files = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let plan = index.plan(root, &files, None, NOT_LIVE).unwrap();
        index.note_unopened(root, &plan, &at(20, "run-b"));
        assert_eq!(index.unreadable(root), 1);
        let plan = index.plan(root, &files, None, NOT_LIVE).unwrap();
        assert_eq!(plan.candidates.len(), 3, "not skipped");
        index.note_settled(root, &plan, &["run-b"]);
        assert_eq!(index.unreadable(root), 0);
        // Gone: not counted any more.
        index.note_unopened(root, &plan, &at(10, "run-a"));
        index.plan(root, &listing(&[(30, "run-c"), (20, "run-b")]), None, NOT_LIVE).unwrap();
        assert_eq!(index.unreadable(root), 0);
    }

    #[test]
    fn a_listing_older_than_the_one_before_it_forgets_no_record() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let older = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let newer = listing(&[(40, "run-x"), (30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let page = index.plan(root, &newer, None, NOT_LIVE).unwrap();
        let next = page.continuation(&page.candidates[1]);
        index.plan(root, &older, None, NOT_LIVE).unwrap();
        // run-x comes back as it was: the continuation holds.
        assert_eq!(index.plan(root, &newer, Some(&next), NOT_LIVE).unwrap().candidates.len(), 2);
        // run-x comes back with a time after the continuation: refused.
        let moved = listing(&[(30, "run-c"), (20, "run-b"), (15, "run-x"), (10, "run-a")]);
        assert_eq!(index.plan(root, &moved, Some(&next), NOT_LIVE).unwrap_err().code(), HISTORY_CHANGED);
    }

    #[test]
    fn a_record_deleted_and_put_back_with_another_time_refuses_the_continuation() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let all = listing(&[(40, "run-d"), (30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let page = index.plan(root, &all, None, NOT_LIVE).unwrap();
        let first = page.continuation(&page.candidates[0]);
        // run-d (returned) is deleted; the next page is read.
        let without = listing(&[(30, "run-c"), (20, "run-b"), (10, "run-a")]);
        let page = index.plan(root, &without, Some(&first), NOT_LIVE).unwrap();
        let second = page.continuation(&page.candidates[0]);
        // Put back as it was: the continuation holds and does not list it.
        let plan = index.plan(root, &all, Some(&second), NOT_LIVE).unwrap();
        assert_eq!(plan.candidates, vec![at(20, "run-b"), at(10, "run-a")]);
        index.plan(root, &without, Some(&second), NOT_LIVE).unwrap();
        // Put back with a time after the continuation: it would be listed twice.
        let back = listing(&[(30, "run-c"), (20, "run-b"), (15, "run-d"), (10, "run-a")]);
        assert_eq!(index.plan(root, &back, Some(&second), NOT_LIVE).unwrap_err().code(), HISTORY_CHANGED);
    }

    #[test]
    fn dropping_the_times_of_gone_records_refuses_earlier_continuations() {
        let index = HistoryIndex::default();
        let root = Path::new("/project");
        let kept = listing(&[(3, "run-c"), (2, "run-b"), (1, "run-a")]);
        let many: Vec<Position> = (0..GONE_KEPT as u128 + 1).map(|i| at(100 + i, &format!("run-{i:08x}"))).collect();
        let mut all = many.clone();
        all.extend(kept.iter().cloned());
        all.sort_by(|a, b| b.cmp(a));
        let page = index.plan(root, &all, None, NOT_LIVE).unwrap();
        let next = page.continuation(&page.candidates[0]);
        assert!(index.plan(root, &all, Some(&next), NOT_LIVE).is_ok());
        // More records are gone than the index remembers.
        index.plan(root, &kept, None, NOT_LIVE).unwrap();
        assert_eq!(index.plan(root, &kept, Some(&next), NOT_LIVE).unwrap_err().code(), HISTORY_CHANGED);
        // A continuation made now holds.
        let page = index.plan(root, &kept, None, NOT_LIVE).unwrap();
        let next = page.continuation(&page.candidates[0]);
        assert_eq!(index.plan(root, &kept, Some(&next), NOT_LIVE).unwrap().candidates.len(), 2);
    }
}
