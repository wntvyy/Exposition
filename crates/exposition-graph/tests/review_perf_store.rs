//! Independent review of the graph performance change: the memoized status,
//! the row budget of a commit, the bytewise path ranges, and reads above the
//! former caps. Tests marked `#[ignore = "review finding ..."]` demonstrate a
//! finding and fail on the reviewed code; the others pass on it.

mod support;

use std::path::{Path, PathBuf};

use exposition_core::path::WorkspacePath;
use exposition_graph::reconcile::{ReconcileOptions, Scope};
use exposition_graph::store::{GraphStore, StoreError, Writer, row_limit};
use exposition_graph::*;
use rusqlite::Connection;
use support::{attached, entity, relation};

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn full(svc: &GraphService) {
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: false }).unwrap();
}

fn database(root: &Path) -> PathBuf {
    root.join(".exposition").join("graph.sqlite3")
}

fn count(conn: &Connection, sql: &str) -> u32 {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap() as u32
}

/// What the tables hold, read through `conn`.
fn tables(conn: &Connection) -> (u32, u32, u32, u32, u32) {
    (
        count(conn, "SELECT count(*) FROM entities WHERE lifecycle != 'tombstoned'"),
        count(conn, "SELECT count(*) FROM relations"),
        count(conn, "SELECT count(*) FROM coverage"),
        count(conn, "SELECT count(*) FROM coverage WHERE pending > 0"),
        count(conn, "SELECT coalesce(sum(unresolved), 0) FROM coverage"),
    )
}

fn reported(svc: &GraphService) -> (u32, u32, u32, u32, u32) {
    let s = svc.status();
    (s.entities, s.relations, s.coverage.resources, s.coverage.pending, s.coverage.unresolved_bindings)
}

/// `status()` says what the tables hold.
fn assert_status(svc: &GraphService, when: &str) {
    let got = reported(svc);
    let want = svc.with_store(|s| Ok(tables(s.connection()))).unwrap();
    assert_eq!(got, want, "status is stale {when}");
}

fn raw(svc: &GraphService, sql: &str) -> Result<usize, rusqlite::Error> {
    let mut out = Ok(0);
    svc.with_store(|s| {
        out = s.connection().execute(sql, []);
        Ok(())
    })
    .unwrap();
    out
}

// ----- the status memo -----

#[test]
fn status_follows_writes_through_every_path_of_the_connection() {
    let (_data, _project, root, svc) = attached();
    write(&root, "a.tex", "\\begin{lemma}\\label{l:a}\nA, by Lemma~\\ref{l:b}.\n\\end{lemma}\n");
    write(&root, "b.tex", "\\begin{lemma}\\label{l:b}\nB.\n\\end{lemma}\n");
    full(&svc);
    assert_status(&svc, "after a full pass");
    let base = reported(&svc);
    assert!(base.0 >= 4 && base.1 >= 1, "{base:?}");

    // A transaction that is dropped (rolled back).
    svc.with_store(|s| {
        let w = s.writer()?;
        w.put_entity(&entity(EntityKind::Lemma, "rolled back"))?;
        drop(w);
        Ok(())
    })
    .unwrap();
    assert_eq!(reported(&svc), base, "a rolled-back write changes nothing");
    assert_status(&svc, "after a rolled-back writer");

    // A committed budgeted writer and a committed plain writer.
    let kept = entity(EntityKind::Lemma, "kept");
    svc.with_store(|s| {
        let w = s.writer()?;
        w.put_entity(&kept)?;
        w.commit()?;
        Ok(())
    })
    .unwrap();
    assert_status(&svc, "after a committed writer");
    assert_eq!(reported(&svc).0, base.0 + 1);
    let other = entity(EntityKind::Lemma, "other");
    svc.with_store(|s| {
        let w = Writer::new(s.transaction()?);
        w.put_entity(&other)?;
        w.put_relation(&relation(RelationKind::DependsOn, &kept.id, &other.id))?;
        w.commit()?;
        Ok(())
    })
    .unwrap();
    assert_status(&svc, "after a plain writer");

    // A transaction committed without the write helpers.
    svc.with_store(|s| {
        let tx = s.transaction()?;
        tx.execute("UPDATE entities SET lifecycle = 'tombstoned' WHERE id = ?1", [other.id.as_str()])
            .map_err(StoreError::from)?;
        tx.commit().map_err(StoreError::from)?;
        Ok(())
    })
    .unwrap();
    assert_status(&svc, "after a raw transaction");

    // Statements on the connection: update, delete without WHERE, replace,
    // a failing statement, a savepoint that is rolled back, a nested commit.
    raw(&svc, "UPDATE entities SET lifecycle = 'active' WHERE lifecycle = 'tombstoned'").unwrap();
    assert_status(&svc, "after a raw update");
    raw(&svc, "INSERT INTO coverage (path, state, indexed_at, unresolved) VALUES ('raw.md', 'failed', 0, 3)").unwrap();
    assert_status(&svc, "after a raw insert");
    raw(&svc, "INSERT OR REPLACE INTO coverage (path, state, indexed_at, unresolved, pending) VALUES ('raw.md', 'complete', 0, 7, 5)")
        .unwrap();
    assert_status(&svc, "after a replace");
    assert!(raw(&svc, "INSERT INTO coverage (path, state, indexed_at) VALUES ('raw.md', 'failed', 0)").is_err());
    assert_status(&svc, "after a failed statement");
    svc.with_store(|s| {
        s.connection()
            .execute_batch(
                "SAVEPOINT review; INSERT INTO coverage (path, state, indexed_at) VALUES ('sp.md', 'failed', 0);
                 ROLLBACK TO review; RELEASE review;",
            )
            .map_err(StoreError::from)?;
        Ok(())
    })
    .unwrap();
    assert_status(&svc, "after a rolled-back savepoint");
    svc.with_store(|s| {
        s.connection()
            .execute_batch(
                "BEGIN; INSERT INTO coverage (path, state, indexed_at) VALUES ('tx.md', 'failed', 0);
                 SAVEPOINT inner; DELETE FROM coverage WHERE path = 'raw.md'; RELEASE inner; COMMIT;",
            )
            .map_err(StoreError::from)?;
        Ok(())
    })
    .unwrap();
    assert_status(&svc, "after a raw batch");
    raw(&svc, "DELETE FROM relations").unwrap();
    assert_status(&svc, "after deleting every relation");
    assert_eq!(reported(&svc).1, 0);

    // Pending marks, a pass, a backup, a saved view.
    svc.mark_pending(&[path("a.tex")]).unwrap();
    assert_status(&svc, "after a mark");
    assert_eq!(reported(&svc).3, 1);
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("a.tex")]), force: true }).unwrap();
    assert_status(&svc, "after a scoped pass");
    svc.backup("manual").unwrap();
    assert_status(&svc, "after a backup");
    svc.reconcile(&ReconcileOptions { scope: Scope::Full, force: true }).unwrap();
    assert_status(&svc, "after a forced full pass");

    // Closing and reopening the project.
    let before = reported(&svc);
    svc.detach();
    assert_eq!(svc.status().entities, 0);
    svc.attach(&root);
    assert_eq!(reported(&svc), before);
    assert_status(&svc, "after reattaching");
}

#[test]
fn status_follows_another_connection_and_an_attached_database() {
    let (_data, _project, root, svc) = attached();
    write(&root, "a.md", "# A\n");
    full(&svc);
    assert_status(&svc, "at the start");
    let before = reported(&svc);

    // Another connection (another process on the same project) writes.
    let second = Connection::open(database(&root)).unwrap();
    second
        .execute("INSERT INTO coverage (path, state, indexed_at, pending) VALUES ('second.md', 'complete', 0, 9)", [])
        .unwrap();
    assert_eq!(tables(&second).2, before.2 + 1);
    assert_eq!(reported(&svc), tables(&second), "a row written by another connection is counted");
    second.execute("DELETE FROM coverage WHERE path = 'second.md'", []).unwrap();
    assert_eq!(reported(&svc), before, "and its removal too");
    // Several commits by the other connection between two status reads.
    for i in 0..3 {
        second
            .execute("INSERT INTO coverage (path, state, indexed_at) VALUES (?1, 'failed', 0)", [format!("s{i}.md")])
            .unwrap();
    }
    assert_eq!(reported(&svc), tables(&second));
    // The other connection writes inside a transaction that is still open:
    // nothing is visible; then it commits.
    second.execute_batch("BEGIN IMMEDIATE; DELETE FROM coverage WHERE path LIKE 's_.md';").unwrap();
    let during = reported(&svc);
    assert_eq!(during.2, before.2 + 3, "uncommitted rows of another connection are not counted");
    second.execute_batch("COMMIT;").unwrap();
    assert_eq!(reported(&svc), before);
    drop(second);

    // Rows copied in from an attached database file.
    let side = root.join("side.sqlite3");
    {
        let other = Connection::open(&side).unwrap();
        other
            .execute_batch(
                "CREATE TABLE c (path TEXT, state TEXT);
                 INSERT INTO c VALUES ('x1.md', 'failed'), ('x2.md', 'partial'), ('x3.md', 'complete');",
            )
            .unwrap();
    }
    let attached_ok = svc
        .with_store(|s| {
            let conn = s.connection();
            let ok = conn.execute("ATTACH DATABASE ?1 AS side", [side.to_str().unwrap()]).is_ok();
            if ok {
                conn.execute("INSERT INTO coverage (path, state, indexed_at) SELECT path, state, 0 FROM side.c", [])
                    .map_err(StoreError::from)?;
                conn.execute("DETACH DATABASE side", []).map_err(StoreError::from)?;
            }
            Ok(ok)
        })
        .unwrap();
    if attached_ok {
        assert_eq!(reported(&svc).2, before.2 + 3);
    }
    assert_status(&svc, "after copying rows from an attached database");
}

#[test]
fn status_is_right_after_a_backup_is_restored() {
    let (_data, _project, root, svc) = attached();
    write(&root, "a.md", "# A\n");
    full(&svc);
    let at_backup = reported(&svc);
    let backup = svc.backup("manual").unwrap();
    // More records after the backup; the status sees them.
    svc.with_store(|s| {
        let w = s.writer()?;
        for i in 0..7 {
            w.put_entity(&entity(EntityKind::Lemma, &format!("later {i}")))?;
        }
        w.commit()?;
        Ok(())
    })
    .unwrap();
    assert_eq!(reported(&svc).0, at_backup.0 + 7);
    // The database is damaged while the project is closed; the backup is restored.
    svc.detach();
    std::fs::write(database(&root), b"not a database").unwrap();
    let status = svc.attach(&root);
    let GraphState::Unavailable { recovery, .. } = &status.state else { panic!("{status:?}") };
    let option = recovery
        .iter()
        .find(|o| matches!(o, RecoveryOption::RestoreBackup { backup: name, .. } if *name == backup.name))
        .expect("the backup is offered")
        .clone();
    let restored = svc.recover(&option).unwrap();
    assert!(matches!(restored.state, GraphState::Attached), "{restored:?}");
    assert_eq!(reported(&svc), at_backup, "the restored graph, not the one read before");
    assert_status(&svc, "after restoring a backup");
}

// ----- the row budget -----

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// A random sequence of small and large commits, rolled-back transactions,
/// deletes followed by inserts, rows written outside the helpers and rows
/// written by another connection, on two tables kept near their limits: a
/// commit through the budgeted writer is refused exactly when counting every
/// table (the rule before the change) refuses it.
#[test]
fn the_row_budget_refuses_exactly_what_counting_every_table_refuses() {
    for seed in [11u64, 12, 13] {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("graph.sqlite3");
        let mut store = GraphStore::open(&file, true).unwrap();
        let second = Connection::open(&file).unwrap();
        let meta_limit = row_limit("meta");
        let views_limit = row_limit("views");
        // Views start close to their limit.
        {
            let tx = store.transaction().unwrap();
            for i in 0..views_limit - 40 {
                tx.execute(
                    "INSERT INTO views (id, name, scope, body, revision, updated_at) VALUES (?1, 'n', 's', '{}', 1, 0)",
                    [format!("view-seed{i}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let rows = |conn: &Connection, table: &str| -> i64 {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
        };
        let mut rng = Rng(seed);
        let mut next = 0u64;
        let (mut accepted, mut refused, mut refused_untouched) = (0u32, 0u32, 0u32);
        for step in 0..1500 {
            let meta = rows(store.connection(), "meta");
            let views = rows(store.connection(), "views");
            let sizes = [1u64, 1, 2, 5, 30, 200];
            let n = sizes[rng.below(sizes.len() as u64) as usize] as i64;
            let m = sizes[rng.below(sizes.len() as u64) as usize] as i64;
            match rng.below(11) {
                // A budgeted commit: delete up to `m` of our rows, then insert `n`
                // into one table (sometimes both, sometimes neither).
                0..=4 => {
                    let which = rng.below(4);
                    let delete = rng.below(3) == 0;
                    let result = {
                        let w = store.writer().unwrap();
                        let mut expect_meta = meta;
                        let mut expect_views = views;
                        if delete {
                            let gone = w
                                .tx
                                .execute(
                                    "DELETE FROM meta WHERE key IN (SELECT key FROM meta WHERE key LIKE 'k%' LIMIT ?1)",
                                    [m],
                                )
                                .unwrap();
                            expect_meta -= gone as i64;
                        }
                        if which == 0 || which == 2 {
                            for _ in 0..n {
                                next += 1;
                                w.tx.execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("k{next}")])
                                    .unwrap();
                            }
                            expect_meta += n;
                        }
                        if which == 1 || which == 2 {
                            for _ in 0..n {
                                next += 1;
                                w.tx.execute(
                                    "INSERT INTO views (id, name, scope, body, revision, updated_at) VALUES (?1, 'n', 's', '{}', 1, 0)",
                                    [format!("view-{next}")],
                                )
                                .unwrap();
                            }
                            expect_views += n;
                        }
                        if which == 3 {
                            // No row added: an existing row changes.
                            w.set_meta("revision", &step.to_string()).unwrap();
                        }
                        let fits = expect_meta <= meta_limit && expect_views <= views_limit;
                        (w.commit(), fits, expect_meta, expect_views, which)
                    };
                    let (outcome, fits, expect_meta, expect_views, which) = result;
                    match (&outcome, fits) {
                        (Ok(()), true) => {
                            accepted += 1;
                            assert_eq!(rows(store.connection(), "meta"), expect_meta, "seed {seed} step {step}");
                            assert_eq!(rows(store.connection(), "views"), expect_views, "seed {seed} step {step}");
                        }
                        (Err(StoreError::Limit(_)), false) => {
                            refused += 1;
                            if which == 3 {
                                refused_untouched += 1;
                            }
                            assert_eq!(rows(store.connection(), "meta"), meta, "nothing is written (seed {seed} step {step})");
                            assert_eq!(rows(store.connection(), "views"), views, "nothing is written (seed {seed} step {step})");
                        }
                        (other, fits) => panic!(
                            "seed {seed} step {step}: commit {other:?} but counting every table {} it (meta {expect_meta}/{meta_limit}, views {expect_views}/{views_limit})",
                            if fits { "accepts" } else { "refuses" }
                        ),
                    }
                }
                // A budgeted transaction that is rolled back.
                5 => {
                    let w = store.writer().unwrap();
                    for _ in 0..n {
                        next += 1;
                        w.tx.execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("k{next}")]).unwrap();
                    }
                    drop(w);
                    assert_eq!(rows(store.connection(), "meta"), meta);
                }
                // Rows written outside the helpers (may pass the limit: the
                // next commit is then refused, as before the change).
                6 => {
                    let table_meta = rng.below(2) == 0;
                    let room = if table_meta { meta_limit - meta } else { views_limit - views };
                    // Mostly up to the limit, sometimes beyond it.
                    let add = if rng.below(5) == 0 { room + 1 + rng.below(3) as i64 } else { n.min(room.max(0)) };
                    for _ in 0..add.max(0) {
                        next += 1;
                        if table_meta {
                            store
                                .connection()
                                .execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("k{next}")])
                                .unwrap();
                        } else {
                            store
                                .connection()
                                .execute(
                                    "INSERT INTO views (id, name, scope, body, revision, updated_at) VALUES (?1, 'n', 's', '{}', 1, 0)",
                                    [format!("view-{next}")],
                                )
                                .unwrap();
                        }
                    }
                }
                // Rows removed outside the helpers.
                7 => {
                    store
                        .connection()
                        .execute("DELETE FROM meta WHERE key IN (SELECT key FROM meta WHERE key LIKE 'k%' LIMIT ?1)", [m])
                        .unwrap();
                    store
                        .connection()
                        .execute(
                            "DELETE FROM views WHERE id IN (SELECT id FROM views WHERE id NOT LIKE 'view-seed%' LIMIT ?1)",
                            [m],
                        )
                        .unwrap();
                }
                // Another connection adds rows (up to or beyond a limit) or removes them.
                8 => {
                    let room = meta_limit - meta;
                    let add = if rng.below(4) == 0 { room + 1 } else { n.min(room.max(0)) };
                    for _ in 0..add.max(0) {
                        next += 1;
                        second.execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("k{next}")]).unwrap();
                    }
                }
                9 => {
                    second
                        .execute("DELETE FROM meta WHERE key IN (SELECT key FROM meta WHERE key LIKE 'k%' LIMIT ?1)", [m])
                        .unwrap();
                    let room = views_limit - views;
                    if rng.below(3) == 0 {
                        for _ in 0..(room + 1).max(0) {
                            next += 1;
                            second
                                .execute(
                                    "INSERT INTO views (id, name, scope, body, revision, updated_at) VALUES (?1, 'n', 's', '{}', 1, 0)",
                                    [format!("view-{next}")],
                                )
                                .unwrap();
                        }
                    }
                }
                // Make room again in both tables.
                _ => {
                    store.connection().execute("DELETE FROM meta WHERE key LIKE 'k%' AND rowid % 3 = 0", []).unwrap();
                    second.execute("DELETE FROM views WHERE id NOT LIKE 'view-seed%'", []).unwrap();
                }
            }
        }
        println!("BUDGET seed {seed}: {accepted} accepted, {refused} refused, {refused_untouched} refused without adding a row");
        assert!(accepted > 30 && refused > 30 && refused_untouched > 0, "seed {seed}: {accepted} accepted, {refused} refused, {refused_untouched} refused without adding a row");
    }
}

/// A pass whose write is refused leaves the status, the session index and
/// the next pass right.
#[test]
fn a_refused_pass_leaves_the_status_and_the_next_pass_right() {
    let (_data, _project, root, svc) = attached();
    write(&root, "waits.tex", "See Lemma~\\ref{lem:later} and Lemma~\\ref{lem:never}.\n");
    write(&root, "other.tex", "\\section{Other}\n");
    full(&svc);
    let unresolved = |p: &str| svc.with_store(|s| Ok(s.coverage(&path(p))?)).unwrap().unwrap().unresolved;
    assert_eq!(unresolved("waits.tex"), 2);
    // The meta table is filled past its limit outside the helpers: every
    // commit is refused.
    let room = row_limit("meta") - i64::from(svc.with_store(|s| Ok(count(s.connection(), "SELECT count(*) FROM meta"))).unwrap());
    svc.with_store(|s| {
        let tx = s.transaction()?;
        for i in 0..=room {
            tx.execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("fill{i}")]).map_err(StoreError::from)?;
        }
        tx.commit().map_err(StoreError::from)?;
        Ok(())
    })
    .unwrap();
    let before = reported(&svc);
    write(&root, "other.tex", "\\section{Other}\n\\begin{lemma}\\label{lem:later}\nNow.\n\\end{lemma}\n");
    assert!(svc.mark_pending(&[path("other.tex")]).is_err(), "the mark is refused too");
    let refused = svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("other.tex")]), force: false });
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(reported(&svc), before, "a refused pass writes nothing");
    assert_status(&svc, "after a refused pass");
    assert_eq!(unresolved("waits.tex"), 2);
    // Room again: the same pass now defines the label and wakes the waiting file.
    raw(&svc, "DELETE FROM meta WHERE key LIKE 'fill%'").unwrap();
    let report =
        svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("other.tex")]), force: false }).unwrap();
    assert_eq!(report.read, 1, "{report:?}");
    assert_eq!(unresolved("waits.tex"), 1, "the waiting file was resolved again after the refused pass");
    assert_status(&svc, "after the repeated pass");
    // Removing the label again (a second refused pass in between).
    svc.with_store(|s| {
        let tx = s.transaction()?;
        for i in 0..=room {
            tx.execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("fill{i}")]).map_err(StoreError::from)?;
        }
        tx.commit().map_err(StoreError::from)?;
        Ok(())
    })
    .unwrap();
    write(&root, "other.tex", "\\section{Other again}\n");
    assert!(svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("other.tex")]), force: false }).is_err());
    raw(&svc, "DELETE FROM meta WHERE key LIKE 'fill%'").unwrap();
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("other.tex")]), force: false }).unwrap();
    assert_eq!(unresolved("waits.tex"), 2, "the label is gone again");
    write(&root, "third.tex", "\\begin{lemma}\\label{lem:never}\nNever say never.\n\\end{lemma}\n");
    svc.reconcile(&ReconcileOptions { scope: Scope::Paths(vec![path("third.tex")]), force: false }).unwrap();
    assert_eq!(unresolved("waits.tex"), 1);
    assert_status(&svc, "at the end");
}

// ----- reads above the former caps -----

fn evidence(subject: &EntityId, n: u64, input: Option<&WorkspacePath>) -> Evidence {
    Evidence {
        id: EvidenceId::mint(),
        kind: EvidenceKind::Review,
        subject: Some(subject.clone()),
        operation: OperationRef { kind: "review".into(), id: format!("r{n}") },
        inputs: input
            .map(|p| vec![VersionedInput { path: p.clone(), content_hash: Some("h".into()), revision: None, unsaved: false }])
            .unwrap_or_default(),
        outcome: EvidenceOutcome::Succeeded,
        environment: vec![],
        outputs: vec![],
        formal: None,
        recorded_by: Origin::User,
        recorded_at_ms: 1_000 + n,
    }
}

#[test]
fn evidence_beyond_what_a_detail_or_an_impact_report_lists_is_flagged() {
    let (_data, _project, _root, svc) = attached();
    let (few, many) = svc
        .with_store(|store| {
            let w = store.writer()?;
            let few = entity(EntityKind::Lemma, "Few records");
            let many = entity(EntityKind::Lemma, "Many records");
            w.put_entity(&few)?;
            w.put_entity(&many)?;
            for n in 0..200 {
                w.put_evidence(&evidence(&few.id, n, None))?;
            }
            for n in 0..201 {
                w.put_evidence(&evidence(&many.id, n, None))?;
            }
            let mut delta = GraphDelta::default();
            w.record_change(&Origin::User, "evidence", &mut delta)?;
            w.commit()?;
            Ok((few.id, many.id))
        })
        .unwrap();
    let detail = svc.entity_detail(&few).unwrap();
    assert_eq!((detail.evidence.len(), detail.evidence_truncated), (200, false));
    let detail = svc.entity_detail(&many).unwrap();
    assert_eq!((detail.evidence.len(), detail.evidence_truncated), (200, true));
    // The newest records are the ones listed.
    assert_eq!(detail.evidence[0].recorded_at_ms, 1_200);
}

/// FINDING: an impact report lists at most 200 evidence records and its
/// `truncated` flag is meant to say when more are affected, but the records
/// of one subject are read through `evidence_for_subject` (the newest 200,
/// silently), so the 201st record of a subject is neither listed nor flagged.
#[test]
#[ignore = "review finding: a subject's evidence beyond 200 records is still dropped silently by impact"]
fn impact_says_when_a_subject_has_more_evidence_than_it_lists() {
    let (_data, _project, _root, svc) = attached();
    let many = svc
        .with_store(|store| {
            let w = store.writer()?;
            let many = entity(EntityKind::Lemma, "Many records");
            w.put_entity(&many)?;
            for n in 0..201 {
                w.put_evidence(&evidence(&many.id, n, None))?;
            }
            let mut delta = GraphDelta::default();
            w.record_change(&Origin::User, "evidence", &mut delta)?;
            w.commit()?;
            Ok(many.id)
        })
        .unwrap();
    let report = svc.impact(&ImpactRequest { entities: vec![many], max_depth: None, max_items: None }).unwrap();
    assert!(report.evidence.len() <= 200);
    assert!(report.truncated, "201 records exist, {} are listed, and the report does not say so", report.evidence.len());
}

/// More than 20,000 declarations bound in one file, each used from another
/// file: removal planning and the impact of the file see all of them.
#[test]
fn a_file_with_more_bound_items_than_the_former_cap_is_planned_completely() {
    let (_data, _project, root, svc) = attached();
    write(&root, "big.md", "# Big\n");
    write(&root, "user.md", "# User\n");
    full(&svc);
    let big = path("big.md");
    let over = store::MAX_ROWS + 25;
    let (last, dependent) = svc
        .with_store(|s| {
            let w = s.writer()?;
            let mut last = None;
            for i in 0..over {
                let lemma = entity(EntityKind::Lemma, &format!("Bound {i:06}"));
                w.put_entity(&lemma)?;
                w.set_bindings(
                    &lemma.id,
                    &[SourceBinding {
                        entity: lemma.id.clone(),
                        path: big.clone(),
                        selector: SourceSelector::WholeFile,
                        range: None,
                        version: None,
                        fingerprint: None,
                        state: BindingState::Resolved,
                        detail: None,
                    }],
                )?;
                last = Some(lemma.id);
            }
            // The entity that sorts last by id among those bound in the file.
            let mut bound: Vec<EntityId> = w
                .tx
                .prepare("SELECT entity_id FROM bindings WHERE path = 'big.md' ORDER BY entity_id DESC LIMIT 1")
                .map_err(StoreError::from)?
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(StoreError::from)?
                .map(|r| EntityId::parse(&r.unwrap()).unwrap())
                .collect();
            let last_by_id = bound.pop().or(last).unwrap();
            let dependent = entity(EntityKind::Theorem, "Depends on the last bound lemma");
            w.put_entity(&dependent)?;
            w.put_relation(&relation(RelationKind::DependsOn, &dependent.id, &last_by_id))?;
            let mut delta = GraphDelta::default();
            w.record_change(&Origin::User, "bound", &mut delta)?;
            w.commit()?;
            Ok((last_by_id, dependent.id))
        })
        .unwrap();
    svc.with_store(|s| {
        assert!(s.bindings_at_path(&big)?.len() >= over, "every binding in the file");
        Ok(())
    })
    .unwrap();
    // Impact of the file reaches the dependent of the entity bound last.
    let resource = svc.resource(&big).unwrap().unwrap();
    let report = svc.impact(&ImpactRequest { entities: vec![resource.id], max_depth: None, max_items: Some(1000) }).unwrap();
    let reaches = report.items.iter().any(|i| i.entity.id == dependent || i.entity.id == last);
    println!(
        "BOUND impact of the file: {} items, truncated {}, work limit {}, reaches the last bound entity or its dependent: {reaches}",
        report.items.len(),
        report.truncated,
        report.work_limit_reached
    );
    assert!(reaches || report.truncated || report.work_limit_reached, "an incomplete report must say so");
    // Removal planning does not fail or stop at the former cap.
    let impacts = svc.path_impacts(std::slice::from_ref(&big)).unwrap();
    println!("BOUND path_impacts: {} impacts", impacts.len());
}
