//! Independent review of the run-history listing (`RunStore::list`): the
//! kept summaries (`ListedSummaries`) and the `runs_listing` index.
//!
//! The listing is compared with what the listing did before the change: the
//! original statement run on another connection, every listed record decoded
//! in full. Random sequences of operations go through the listing store, a
//! second store, and a raw connection that edits rows the way another
//! process (or a hostile file) could.

use super::*;
use crate::store::{MAX_LIST, RunStore, state_name, summary};

/// The statement the listing used before the index (commit 8c65dc5).
const BASE_LIST_SQL: &str = "SELECT CAST(id AS TEXT), CAST(state AS TEXT), CAST(body AS TEXT) FROM runs
     WHERE project = ?1 AND (?2 IS NULL OR workflow = ?2) AND deleted_at IS NULL
     ORDER BY started_at DESC, id LIMIT ?3";

/// What a listing without any kept summary returns: the base statement and a
/// full decode of every row (the generated rows stay inside the size limits,
/// so the id/state consistency check is the whole validation).
fn expected(
    oracle: &rusqlite::Connection,
    project: &str,
    workflow: Option<&str>,
    limit: usize,
) -> Vec<WorkflowRunSummary> {
    let mut stmt = oracle.prepare_cached(BASE_LIST_SQL).unwrap();
    let rows = stmt
        .query_map(rusqlite::params![project, workflow, limit.min(MAX_LIST) as i64], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))
        })
        .unwrap();
    let mut out = Vec::new();
    for row in rows {
        let Ok((Some(id), Some(state), Some(body))) = row else { continue };
        let Ok(run) = serde_json::from_str::<WorkflowRun>(&body) else { continue };
        if run.id.as_str() != id || state_name(run.state) != state {
            continue;
        }
        out.push(summary(&run));
    }
    out
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const STATES: [WorkflowRunState; 5] = [
    WorkflowRunState::Running,
    WorkflowRunState::Succeeded,
    WorkflowRunState::Failed,
    WorkflowRunState::Canceled,
    WorkflowRunState::Interrupted,
];

/// A small finished run to copy (the plan and step detail are dropped: only
/// the listing matters here, and the sequences write thousands of records).
fn template() -> WorkflowRun {
    let f = fixture();
    let id = f.save(numerical(false));
    let mut run = f.run_to_end(&id);
    for step in &mut run.steps {
        step.attempts.clear();
    }
    run.plan.steps.clear();
    run.plan.inputs.clear();
    run
}

fn fresh(template: &WorkflowRun, rng: &mut Rng, workflows: &[WorkflowId]) -> WorkflowRun {
    let mut run = template.clone();
    run.id = exposition_graph::WorkflowRunId::mint();
    run.workflow = workflows[rng.below(workflows.len())].clone();
    // Few distinct start times: ties are ordered by id.
    run.started_at_ms = 1000 + rng.below(6) as u64;
    mutate(&mut run, rng);
    run
}

fn mutate(run: &mut WorkflowRun, rng: &mut Rng) {
    match rng.below(5) {
        0 => run.title = format!("Title {}", rng.below(1000)),
        1 => run.acknowledged = !run.acknowledged,
        2 => run.state = STATES[rng.below(STATES.len())],
        3 => run.finished_at_ms = Some(2000 + rng.below(1000) as u64),
        _ => {
            if !run.steps.is_empty() {
                let i = rng.below(run.steps.len());
                run.steps[i].state = if run.steps[i].state == WorkflowStepState::Succeeded {
                    WorkflowStepState::Pending
                } else {
                    WorkflowStepState::Succeeded
                };
            }
        }
    }
}

struct World {
    listing: RunStore,
    second: RunStore,
    raw: rusqlite::Connection,
    oracle: rusqlite::Connection,
    projects: Vec<String>,
    workflows: Vec<WorkflowId>,
    /// Every run ever written (as last written through a store), with its project.
    known: Vec<(String, WorkflowRun)>,
    template: WorkflowRun,
}

impl World {
    fn check(&self, what: &str) {
        for project in &self.projects {
            for workflow in std::iter::once(None).chain(self.workflows.iter().map(Some)) {
                for limit in [1usize, 3, 7, 500] {
                    let got = self.listing.list(project, workflow, limit).unwrap();
                    let want = expected(&self.oracle, project, workflow.map(|w| w.as_str()), limit);
                    assert_eq!(got, want, "after {what}: project {project}, workflow {workflow:?}, limit {limit}");
                }
            }
        }
    }

    /// Some listings (not all), so the kept summaries cover varying subsets.
    fn touch(&self, rng: &mut Rng) {
        let project = &self.projects[rng.below(self.projects.len())];
        let workflow = match rng.below(3) {
            0 => None,
            n => Some(&self.workflows[(n - 1) % self.workflows.len()]),
        };
        let limit = [1usize, 2, 5, 200][rng.below(4)];
        let got = self.listing.list(project, workflow, limit).unwrap();
        let want = expected(&self.oracle, project, workflow.map(|w| w.as_str()), limit);
        assert_eq!(got, want, "partial listing: project {project}, workflow {workflow:?}, limit {limit}");
    }

    fn pick(&self, rng: &mut Rng) -> Option<usize> {
        (!self.known.is_empty()).then(|| rng.below(self.known.len()))
    }

    fn step(&mut self, rng: &mut Rng, two: bool) -> String {
        let op = rng.below(if two { 22 } else { 9 });
        match op {
            // ---- the listing store's own connection ----
            0..=2 => {
                let project = self.projects[rng.below(self.projects.len())].clone();
                let run = fresh(&self.template, rng, &self.workflows);
                self.listing.put(&project, &run).unwrap();
                self.known.push((project, run));
                "put new".into()
            }
            3 | 4 => {
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let (project, mut run) = self.known[i].clone();
                mutate(&mut run, rng);
                self.listing.put(&project, &run).unwrap();
                self.known[i].1 = run;
                "put changed".into()
            }
            5 => {
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let at = (rng.below(2) == 0).then_some(77);
                self.listing.set_deleted(&self.known[i].1.id, at).unwrap();
                format!("set_deleted {at:?}")
            }
            6 => {
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                // The freed rowid is taken by the next new record when it was the highest.
                self.listing.remove(&self.known[i].1.id).unwrap();
                "remove".into()
            }
            7 => {
                if rng.below(4) == 0 {
                    self.listing.compact().unwrap();
                    "compact".into()
                } else {
                    "nothing".into()
                }
            }
            8 => {
                if rng.below(4) == 0 {
                    self.listing.reconcile(9999).unwrap();
                    "reconcile".into()
                } else {
                    "nothing".into()
                }
            }
            // ---- a second store (another connection of this or another process) ----
            9 | 10 => {
                let project = self.projects[rng.below(self.projects.len())].clone();
                let run = fresh(&self.template, rng, &self.workflows);
                self.second.put(&project, &run).unwrap();
                self.known.push((project, run));
                "second: put new".into()
            }
            11 | 12 => {
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let (project, mut run) = self.known[i].clone();
                mutate(&mut run, rng);
                self.second.put(&project, &run).unwrap();
                self.known[i].1 = run;
                "second: put changed".into()
            }
            13 => {
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                match rng.below(3) {
                    0 => self.second.remove(&self.known[i].1.id).map(|_| ()).unwrap(),
                    1 => self.second.set_deleted(&self.known[i].1.id, Some(5)).map(|_| ()).unwrap(),
                    _ => self.second.set_deleted(&self.known[i].1.id, None).map(|_| ()).unwrap(),
                }
                "second: remove/delete/restore".into()
            }
            14 => {
                if rng.below(4) == 0 {
                    self.second.compact().unwrap();
                    "second: compact".into()
                } else {
                    "nothing".into()
                }
            }
            // ---- raw edits ----
            15 => {
                // Same row, same id and state, different record text.
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let mut run = self.known[i].1.clone();
                run.title = format!("Edited underneath {}", rng.below(1000));
                run.acknowledged = !run.acknowledged;
                let body = serde_json::to_string(&run).unwrap();
                self.raw
                    .execute("UPDATE runs SET body = ?2 WHERE id = ?1", rusqlite::params![run.id.as_str(), body])
                    .unwrap();
                "raw: body replaced (same id and state)".into()
            }
            16 => {
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let state = state_name(STATES[rng.below(STATES.len())]);
                self.raw
                    .execute(
                        "UPDATE runs SET state = ?2 WHERE id = ?1",
                        rusqlite::params![self.known[i].1.id.as_str(), state],
                    )
                    .unwrap();
                "raw: state column changed".into()
            }
            17 => {
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let body = ["{\"id\": 1}", "", "not json", "null"][rng.below(4)];
                self.raw
                    .execute(
                        "UPDATE runs SET body = ?2 WHERE id = ?1",
                        rusqlite::params![self.known[i].1.id.as_str(), body],
                    )
                    .unwrap();
                "raw: body corrupted".into()
            }
            18 => {
                // Another record takes the row (same rowid) of a listed one.
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let old = self.known[i].1.id.clone();
                let rowid: Option<i64> =
                    self.raw.query_row("SELECT rowid FROM runs WHERE id = ?1", [old.as_str()], |r| r.get(0)).ok();
                let Some(rowid) = rowid else { return "nothing".into() };
                let project = self.known[i].0.clone();
                let mut run = fresh(&self.template, rng, &self.workflows);
                // Often the same start time and state as the record it replaces.
                if rng.below(2) == 0 {
                    run.started_at_ms = self.known[i].1.started_at_ms;
                    run.state = self.known[i].1.state;
                }
                let tx = self.raw.unchecked_transaction().unwrap();
                tx.execute("DELETE FROM runs WHERE rowid = ?1", [rowid]).unwrap();
                tx.execute(
                    "INSERT INTO runs (rowid, id, project, workflow, state, started_at, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        rowid,
                        run.id.as_str(),
                        project,
                        run.workflow.as_str(),
                        state_name(run.state),
                        run.started_at_ms as i64,
                        serde_json::to_string(&run).unwrap()
                    ],
                )
                .unwrap();
                tx.commit().unwrap();
                self.known.push((project, run));
                "raw: row replaced by another record (same rowid)".into()
            }
            19 => {
                // Two rows exchange their record texts and ids' columns stay: both stop matching,
                // or (when re-run) match again.
                let (Some(a), Some(b)) = (self.pick(rng), self.pick(rng)) else { return "nothing".into() };
                let (a, b) = (self.known[a].1.id.clone(), self.known[b].1.id.clone());
                let tx = self.raw.unchecked_transaction().unwrap();
                let body = |id: &str| -> Option<String> {
                    tx.query_row("SELECT body FROM runs WHERE id = ?1", [id], |r| r.get(0)).ok()
                };
                if let (Some(body_a), Some(body_b)) = (body(a.as_str()), body(b.as_str())) {
                    tx.execute("UPDATE runs SET body = ?2 WHERE id = ?1", rusqlite::params![a.as_str(), body_b])
                        .unwrap();
                    tx.execute("UPDATE runs SET body = ?2 WHERE id = ?1", rusqlite::params![b.as_str(), body_a])
                        .unwrap();
                }
                tx.commit().unwrap();
                "raw: two records exchanged their text".into()
            }
            20 => {
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let id = self.known[i].1.id.clone();
                let (sql, value): (&str, rusqlite::types::Value) = match rng.below(4) {
                    0 => ("UPDATE runs SET started_at = ?2 WHERE id = ?1", (1000 + rng.below(6) as i64).into()),
                    1 => (
                        "UPDATE runs SET project = ?2 WHERE id = ?1",
                        self.projects[rng.below(self.projects.len())].clone().into(),
                    ),
                    2 => (
                        "UPDATE runs SET workflow = ?2 WHERE id = ?1",
                        self.workflows[rng.below(self.workflows.len())].as_str().to_owned().into(),
                    ),
                    _ => (
                        "UPDATE runs SET deleted_at = ?2 WHERE id = ?1",
                        if rng.below(2) == 0 { rusqlite::types::Value::Null } else { 9i64.into() },
                    ),
                };
                self.raw.execute(sql, rusqlite::params![id.as_str(), value]).unwrap();
                "raw: an indexed column changed".into()
            }
            _ => {
                // The row's id changes (the text still names the old one), or changes back.
                let Some(i) = self.pick(rng) else { return "nothing".into() };
                let id = self.known[i].1.id.clone();
                let other = exposition_graph::WorkflowRunId::mint();
                let _ = self.raw.execute("UPDATE runs SET id = ?2 WHERE id = ?1", [id.as_str(), other.as_str()]);
                "raw: id column changed".into()
            }
        }
    }
}

fn world(dir: &Path, template: &WorkflowRun) -> World {
    let db = dir.join("workflow-runs.sqlite3");
    let listing = RunStore::open(Some(&db)).unwrap();
    let second = RunStore::open(Some(&db)).unwrap();
    let raw = rusqlite::Connection::open(&db).unwrap();
    raw.busy_timeout(Duration::from_secs(5)).unwrap();
    let oracle = rusqlite::Connection::open(&db).unwrap();
    oracle.busy_timeout(Duration::from_secs(5)).unwrap();
    World {
        listing,
        second,
        raw,
        oracle,
        projects: vec!["P1".into(), "P2".into()],
        workflows: vec![template.workflow.clone(), WorkflowId::mint()],
        known: Vec::new(),
        template: template.clone(),
    }
}

fn run_sequences(two: bool, seeds: std::ops::Range<u64>, steps: usize) {
    let template = template();
    for seed in seeds {
        let dir = tempfile::tempdir().unwrap();
        let mut w = world(dir.path(), &template);
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (seed + 1).wrapping_mul(0xD1B5_4A32_D192_ED03));
        let mut trail: Vec<String> = Vec::new();
        for n in 0..steps {
            let what = w.step(&mut rng, two);
            trail.push(what);
            let context = || format!("seed {seed} step {n}: {:?}", &trail[trail.len().saturating_sub(6)..]);
            if rng.below(3) == 0 {
                w.touch(&mut rng);
            } else {
                let c = context();
                w.check(&c);
                // Asked again at once (now answered from the kept summaries).
                w.check(&c);
            }
        }
        // A store opened now (nothing kept) lists the same.
        let fresh = RunStore::open(Some(&dir.path().join("workflow-runs.sqlite3"))).unwrap();
        for project in &w.projects {
            assert_eq!(
                w.listing.list(project, None, 500).unwrap(),
                fresh.list(project, None, 500).unwrap(),
                "seed {seed}: fresh store"
            );
        }
        if !two {
            // The comparison above is only meaningful if summaries were in fact kept.
            let counters = w.listing.counters();
            assert!(counters.bodies_decoded > 0);
        }
    }
}

#[test]
fn review_listing_equals_a_full_decode_after_random_operations_on_one_connection() {
    run_sequences(false, 0..6, 160);
}

#[test]
fn review_listing_equals_a_full_decode_after_random_operations_on_several_connections() {
    run_sequences(true, 100..108, 220);
}

/// The kept summaries really are used in the one-connection sequences (so
/// the differential test exercises them), and reads by other connections do
/// not discard them.
#[test]
fn review_reads_by_other_connections_do_not_discard_kept_summaries_and_writes_do() {
    let template = template();
    let dir = tempfile::tempdir().unwrap();
    let mut w = world(dir.path(), &template);
    let mut rng = Rng(7);
    for _ in 0..10 {
        let run = fresh(&w.template, &mut rng, &w.workflows.clone());
        w.listing.put("P1", &run).unwrap();
        w.known.push(("P1".into(), run));
    }
    let decoded = |w: &World| w.listing.counters().bodies_decoded;
    assert_eq!(w.listing.list("P1", None, 200).unwrap().len(), 10);
    let after_first = decoded(&w);
    // Another connection reads, and another store is opened and lists: nothing is decoded again.
    assert_eq!(expected(&w.oracle, "P1", None, 200).len(), 10);
    let third = RunStore::open(Some(&dir.path().join("workflow-runs.sqlite3"))).unwrap();
    assert_eq!(third.list("P1", None, 200).unwrap().len(), 10);
    assert_eq!(w.listing.list("P1", None, 200).unwrap().len(), 10);
    assert_eq!(decoded(&w), after_first, "reads elsewhere keep the summaries");
    // A commit elsewhere that changes one record's text (same id, same state) discards all of them,
    // and the listing shows the change.
    let mut run = w.known[0].1.clone();
    run.title = "Edited by another connection".into();
    w.raw
        .execute(
            "UPDATE runs SET body = ?2 WHERE id = ?1",
            rusqlite::params![run.id.as_str(), serde_json::to_string(&run).unwrap()],
        )
        .unwrap();
    let listed = w.listing.list("P1", None, 200).unwrap();
    assert_eq!(listed.iter().find(|s| s.id == run.id).unwrap().title, "Edited by another connection");
    assert_eq!(decoded(&w), after_first + 10, "a commit elsewhere discards every kept summary");
}

/// More listed records than the bound on kept summaries: listings stay
/// correct while the kept set is dropped and rebuilt, and hold at most the
/// bound's worth of decodes per pass afterwards.
#[test]
fn review_listing_is_correct_beyond_the_bound_on_kept_summaries() {
    let template = template();
    let dir = tempfile::tempdir().unwrap();
    let w = world(dir.path(), &template);
    let mut rng = Rng(11);
    // 12 projects x 200 listed rows = 2400 > 2048, written in one transaction.
    let projects: Vec<String> = (0..12).map(|p| format!("B{p}")).collect();
    let tx = w.raw.unchecked_transaction().unwrap();
    for project in &projects {
        for _ in 0..205 {
            let run = fresh(&w.template, &mut rng, &w.workflows);
            tx.execute(
                "INSERT INTO runs (id, project, workflow, state, started_at, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    run.id.as_str(),
                    project,
                    run.workflow.as_str(),
                    state_name(run.state),
                    run.started_at_ms as i64,
                    serde_json::to_string(&run).unwrap()
                ],
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
    for pass in 0..3 {
        for project in &projects {
            let got = w.listing.list(project, None, 200).unwrap();
            assert_eq!(got.len(), 200);
            assert_eq!(got, expected(&w.oracle, project, None, 200), "pass {pass}, project {project}");
        }
    }
    // One changed record is seen whatever the kept set holds.
    let (id, body): (String, String) = w
        .oracle
        .query_row("SELECT id, body FROM runs WHERE project = 'B3' LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    let mut run: WorkflowRun = serde_json::from_str(&body).unwrap();
    run.title = "Changed beyond the bound".into();
    w.listing.put("B3", &run).unwrap();
    let got = w.listing.list("B3", None, 200).unwrap();
    assert_eq!(got, expected(&w.oracle, "B3", None, 200));
    let _ = id;
}

// ---- opening an existing history ------------------------------------------

/// A history as the base version left it: version 1, no `runs_listing`
/// index, cleanly closed.
fn base_history(dir: &Path, template: &WorkflowRun, n: usize) -> PathBuf {
    let db = dir.join("workflow-runs.sqlite3");
    let store = RunStore::open(Some(&db)).unwrap();
    for i in 0..n {
        let mut run = template.clone();
        run.id = exposition_graph::WorkflowRunId::mint();
        run.started_at_ms += i as u64;
        store.put("P1", &run).unwrap();
    }
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("DROP INDEX runs_listing; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(conn);
    db
}

/// What the base version ran when it opened a history (store.rs at 8c65dc5),
/// then its listing statement. Returns the number of rows listed.
fn base_open_and_list(db: &Path) -> rusqlite::Result<usize> {
    let conn = rusqlite::Connection::open(db)?;
    conn.busy_timeout(Duration::from_secs(1))?;
    let _version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA trusted_schema = OFF;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS runs (
             id TEXT PRIMARY KEY,
             project TEXT NOT NULL,
             workflow TEXT NOT NULL,
             state TEXT NOT NULL,
             started_at INTEGER NOT NULL,
             body TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS runs_by_project ON runs(project, started_at DESC);",
    )?;
    let mut stmt = conn.prepare(BASE_LIST_SQL)?;
    let rows =
        stmt.query_map(rusqlite::params!["P1", Option::<String>::None, 200], |r| r.get::<_, Option<String>>(0))?;
    Ok(rows.filter_map(Result::ok).count())
}

#[test]
fn review_a_history_from_before_the_index_is_upgraded_in_place_and_lists_the_same() {
    let template = template();
    let dir = tempfile::tempdir().unwrap();
    let db = base_history(dir.path(), &template, 5);
    assert_eq!(base_open_and_list(&db).unwrap(), 5);
    let store = RunStore::open(Some(&db)).unwrap();
    assert_eq!(store.list("P1", None, 200).unwrap().len(), 5);
    // Nothing was set aside or backed up for it (the store version is unchanged).
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.contains("premigration") && !n.contains(".bad-")), "{names:?}");
    // The base statement still works on the upgraded file (an older version can read it).
    drop(store);
    assert_eq!(base_open_and_list(&db).unwrap(), 5);
}

/// A history the base version could open and list, but to which the index
/// cannot be added (here: the file is read-only), must still be listed.
/// The base version listed it; this version cannot open it at all.
#[test]
#[ignore = "review: defect"]
fn review_a_history_that_cannot_take_the_index_is_still_listed() {
    let template = template();
    let dir = tempfile::tempdir().unwrap();
    let db = base_history(dir.path(), &template, 5);
    let mut perms = std::fs::metadata(&db).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&db, perms.clone()).unwrap();
    let base = base_open_and_list(&db);
    let head = RunStore::open(Some(&db)).and_then(|store| store.list("P1", None, 200)).map(|l| l.len());
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&db, perms).unwrap();
    assert_eq!(base.unwrap(), 5, "the base version lists the read-only history");
    assert_eq!(head.map_err(|e| e.message), Ok(5), "this version must list it too");
}

/// The same history at the service: the base version started with it; this
/// version sets it aside and starts an empty one.
#[test]
#[ignore = "review: defect"]
fn review_the_service_keeps_a_history_that_cannot_take_the_index() {
    let f = fixture();
    let template = {
        let id = f.save(numerical(false));
        f.run_to_end(&id)
    };
    let dir = tempfile::tempdir().unwrap();
    let db = base_history(dir.path(), &template, 5);
    let mut perms = std::fs::metadata(&db).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&db, perms.clone()).unwrap();
    assert_eq!(base_open_and_list(&db).unwrap(), 5, "the base version lists the read-only history");
    let service = new_service(&db, &f.host, &f.batch, &f.lean, &f.research);
    drop(service);
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        if let Ok(meta) = entry.metadata() {
            let mut p = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(false);
            let _ = std::fs::set_permissions(entry.path(), p);
        }
    }
    assert!(names.iter().all(|n| !n.contains(".bad-")), "the history was set aside: {names:?}");
}
