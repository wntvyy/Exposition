//! Durable workflow runs: app-local SQLite (`<data dir>/workflow-runs.sqlite3`),
//! keyed by the canonical project root.
//!
//! The run record (plan as approved, steps, attempts, validated outputs) is
//! written through on every change, so a dispatch intent is on disk before the
//! service is called and the service operation id as soon as it is known.
//! Rows are read back as untrusted data: bounded sizes, strict decoding, and
//! consistency with the indexed columns; a row that fails is skipped.
//! [`RunStore::reconcile`] runs when the service starts: whatever was running
//! is interrupted (never resumed or replayed).

use std::path::{Path, PathBuf};

use exposition_graph::{WorkflowId, WorkflowRunId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{
    MAX_ATTEMPT_LOG_CHARS, WorkflowAttemptState, WorkflowRun, WorkflowRunError, WorkflowRunState, WorkflowRunSummary,
    WorkflowStepState,
};

/// Bodies larger than this are treated as hostile.
const MAX_BODY: usize = 32 * 1024 * 1024;
/// At most this many runs are listed.
pub const MAX_LIST: usize = 200;
const MAX_ATTEMPTS: usize = 8;

fn store_error(e: impl std::fmt::Display) -> WorkflowRunError {
    WorkflowRunError::new("workflow.store", format!("The workflow run history could not be used: {e}"))
}

pub fn state_name(state: WorkflowRunState) -> &'static str {
    match state {
        WorkflowRunState::Running => "running",
        WorkflowRunState::Succeeded => "succeeded",
        WorkflowRunState::Failed => "failed",
        WorkflowRunState::Canceled => "canceled",
        WorkflowRunState::Interrupted => "interrupted",
    }
}

pub struct RunStore {
    conn: Connection,
    /// The database file (`None`: in memory).
    path: Option<PathBuf>,
}

/// A run removed from the history with app-managed recovery (P1 project
/// operations): hidden from listings and lookups until restored or purged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletedRun {
    pub summary: WorkflowRunSummary,
    pub deleted_at_ms: u64,
    pub bytes: u64,
}

/// Store schema (`PRAGMA user_version`): 1 adds `runs.deleted_at`.
const STORE_VERSION: i64 = 1;

impl RunStore {
    /// Open (or create) the store; `None` keeps it in memory.
    pub fn open(path: Option<&Path>) -> Result<Self, WorkflowRunError> {
        let conn = match path {
            Some(p) => {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(store_error)?;
                }
                Connection::open(p).map_err(store_error)?
            }
            None => Connection::open_in_memory().map_err(store_error)?,
        };
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(store_error)?;
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
        )
        .map_err(store_error)?;
        migrate(&conn)?;
        Ok(Self { conn, path: path.map(Path::to_path_buf) })
    }

    /// Insert or replace a run record (one transaction).
    pub fn put(&self, project: &str, run: &WorkflowRun) -> Result<(), WorkflowRunError> {
        let body = serde_json::to_string(run).map_err(store_error)?;
        if body.len() > MAX_BODY {
            return Err(WorkflowRunError::new("workflow.tooLarge", "The run record is too large to keep."));
        }
        self.conn
            .execute(
                "INSERT INTO runs (id, project, workflow, state, started_at, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET state = excluded.state, body = excluded.body",
                params![
                    run.id.as_str(),
                    project,
                    run.workflow.as_str(),
                    state_name(run.state),
                    run.started_at_ms as i64,
                    body
                ],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// A run and the project it belongs to.
    pub fn get(&self, id: &WorkflowRunId) -> Result<Option<(String, WorkflowRun)>, WorkflowRunError> {
        let row: Option<(String, String, String, String)> = self
            .conn
            .query_row(
                "SELECT id, project, state, body FROM runs WHERE id = ?1 AND deleted_at IS NULL",
                [id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(store_error)?;
        match row {
            None => Ok(None),
            Some((rid, project, state, body)) => decode(&rid, &state, &body)
                .map(|run| Some((project, run)))
                .map_err(|m| WorkflowRunError::new("workflow.untrusted", m)),
        }
    }

    /// Runs of a project, newest first (bounded; undecodable rows skipped).
    pub fn list(
        &self,
        project: &str,
        workflow: Option<&WorkflowId>,
        limit: usize,
    ) -> Result<Vec<WorkflowRunSummary>, WorkflowRunError> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT CAST(id AS TEXT), CAST(state AS TEXT), CAST(body AS TEXT) FROM runs
                 WHERE project = ?1 AND (?2 IS NULL OR workflow = ?2) AND deleted_at IS NULL
                 ORDER BY started_at DESC, id LIMIT ?3",
            )
            .map_err(store_error)?;
        let rows = stmt
            .query_map(params![project, workflow.map(|w| w.as_str()), limit.min(MAX_LIST) as i64], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))
            })
            .map_err(store_error)?;
        let mut out = Vec::new();
        for row in rows {
            // A malformed row is skipped, never fatal.
            let Ok((Some(id), Some(state), Some(body))) = row else { continue };
            if let Ok(run) = decode(&id, &state, &body) {
                out.push(summary(&run));
            }
        }
        Ok(out)
    }

    /// After a restart: every run still marked running is interrupted, its
    /// in-flight attempts interrupted, and its pending steps skipped. Nothing
    /// is re-dispatched. Returns the number of runs interrupted.
    pub fn reconcile(&self, now_ms: u64) -> Result<u32, WorkflowRunError> {
        type Row = (String, Option<String>, String, Option<String>);
        let rows: Vec<Row> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT CAST(id AS TEXT), CAST(project AS TEXT), 'running', CAST(body AS TEXT) FROM runs
                     WHERE state = 'running' AND id IS NOT NULL LIMIT 10000",
                )
                .map_err(store_error)?;
            let mapped =
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(store_error)?;
            mapped.filter_map(Result::ok).collect()
        };
        let mut count = 0;
        for (id, project, state, body) in rows {
            let decoded = match (&project, &body) {
                (Some(_), Some(body)) => decode(&id, &state, body),
                _ => Err("malformed run row".to_owned()),
            };
            match decoded {
                Ok(mut run) => {
                    interrupt(&mut run, now_ms);
                    self.put(project.as_deref().unwrap_or_default(), &run)?;
                }
                Err(_) => {
                    // Unreadable: never resumed; the row stays out of listings.
                    self.conn
                        .execute("UPDATE runs SET state = 'interrupted' WHERE id = ?1", [id.as_str()])
                        .map_err(store_error)?;
                }
            }
            count += 1;
        }
        Ok(count)
    }
}

impl RunStore {
    /// A run and its project, deleted or not (restore and purge).
    pub fn get_any(&self, id: &WorkflowRunId) -> Result<Option<(String, WorkflowRun, Option<u64>)>, WorkflowRunError> {
        let row: Option<(String, String, String, String, Option<i64>)> = self
            .conn
            .query_row("SELECT id, project, state, body, deleted_at FROM runs WHERE id = ?1", [id.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()
            .map_err(store_error)?;
        match row {
            None => Ok(None),
            Some((rid, project, state, body, deleted)) => decode(&rid, &state, &body)
                .map(|run| Some((project, run, deleted.map(|d| d.max(0) as u64))))
                .map_err(|m| WorkflowRunError::new("workflow.untrusted", m)),
        }
    }

    /// Hide a run from the history (`Some(at)`) or bring it back (`None`).
    /// Returns whether a row changed.
    pub fn set_deleted(&self, id: &WorkflowRunId, at: Option<u64>) -> Result<bool, WorkflowRunError> {
        let n = self
            .conn
            .execute("UPDATE runs SET deleted_at = ?2 WHERE id = ?1", params![id.as_str(), at.map(|a| a as i64)])
            .map_err(store_error)?;
        Ok(n > 0)
    }

    /// Remove a run record for good.
    pub fn remove(&self, id: &WorkflowRunId) -> Result<bool, WorkflowRunError> {
        Ok(self.conn.execute("DELETE FROM runs WHERE id = ?1", [id.as_str()]).map_err(store_error)? > 0)
    }

    /// Deleted (recoverable) runs of a project, newest deletion first.
    pub fn deleted(&self, project: &str) -> Result<Vec<DeletedRun>, WorkflowRunError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT CAST(id AS TEXT), CAST(state AS TEXT), CAST(body AS TEXT), deleted_at, length(body) FROM runs
                 WHERE project = ?1 AND deleted_at IS NOT NULL ORDER BY deleted_at DESC, id LIMIT 2000",
            )
            .map_err(store_error)?;
        let rows = stmt
            .query_map([project], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })
            .map_err(store_error)?;
        let mut out = Vec::new();
        for row in rows {
            let Ok((Some(id), Some(state), Some(body), Some(at), bytes)) = row else { continue };
            if let Ok(run) = decode(&id, &state, &body) {
                out.push(DeletedRun {
                    summary: summary(&run),
                    deleted_at_ms: at.max(0) as u64,
                    bytes: bytes.unwrap_or(0).max(0) as u64,
                });
            }
        }
        Ok(out)
    }

    /// Stored size of each (not deleted) run record of a project.
    pub fn record_bytes(&self, project: &str) -> Result<Vec<(String, u64)>, WorkflowRunError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT CAST(id AS TEXT), length(body) FROM runs WHERE project = ?1 AND deleted_at IS NULL LIMIT 20000",
            )
            .map_err(store_error)?;
        let rows = stmt
            .query_map([project], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?)))
            .map_err(store_error)?;
        Ok(rows.filter_map(Result::ok).filter_map(|(id, n)| Some((id?, n.unwrap_or(0).max(0) as u64))).collect())
    }

    /// Bytes the history occupies on disk now (database and write-ahead log).
    pub fn file_bytes(&self) -> Option<u64> {
        let path = self.path.as_ref()?;
        let size = |p: PathBuf| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        Some(size(path.clone()) + size(PathBuf::from(format!("{}-wal", path.display()))))
    }

    /// Give the space of removed records back to the file system (VACUUM,
    /// then truncate the write-ahead log). Returns the on-disk size before
    /// and after, so callers report what was actually reclaimed.
    pub fn compact(&self) -> Result<(Option<u64>, Option<u64>), WorkflowRunError> {
        let before = self.file_bytes();
        self.conn.execute_batch("VACUUM;").map_err(store_error)?;
        let _ = self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok((before, self.file_bytes()))
    }
}

fn migrate(conn: &Connection) -> Result<(), WorkflowRunError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(store_error)?;
    if version >= STORE_VERSION {
        return Ok(());
    }
    let has: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('runs') WHERE name = 'deleted_at'")
        .and_then(|mut s| s.exists([]))
        .map_err(store_error)?;
    if !has {
        conn.execute_batch("ALTER TABLE runs ADD COLUMN deleted_at INTEGER;").map_err(store_error)?;
    }
    conn.pragma_update(None, "user_version", STORE_VERSION).map_err(store_error)?;
    Ok(())
}

/// Mark a run that was active when the application stopped.
pub fn interrupt(run: &mut WorkflowRun, now_ms: u64) {
    run.state = WorkflowRunState::Interrupted;
    run.code = Some("workflow.interrupted".into());
    run.message = Some(
        "Exposition stopped while this run was active. Nothing was resumed or repeated; review what happened and start a new run if needed."
            .into(),
    );
    run.finished_at_ms = Some(now_ms);
    run.acknowledged = false;
    const RUNNING: &str = "Exposition stopped while this was running; its result is unknown and it was not repeated.";
    const PENDING: &str = "Exposition stopped before this could start; nothing resumes.";
    let mark = |state: &mut WorkflowStepState, reason: &mut Option<String>| match *state {
        WorkflowStepState::Running => {
            *state = WorkflowStepState::Interrupted;
            reason.get_or_insert_with(|| RUNNING.to_owned());
        }
        WorkflowStepState::Pending => {
            *state = WorkflowStepState::Skipped;
            reason.get_or_insert_with(|| PENDING.to_owned());
        }
        _ => {}
    };
    for step in &mut run.steps {
        mark(&mut step.state, &mut step.reason);
        for instance in &mut step.instances {
            mark(&mut instance.state, &mut instance.reason);
        }
        let attempts = step.attempts.iter_mut().chain(step.instances.iter_mut().flat_map(|i| i.attempts.iter_mut()));
        for attempt in attempts {
            if matches!(attempt.state, WorkflowAttemptState::Intent | WorkflowAttemptState::Dispatched) {
                attempt.state = WorkflowAttemptState::Interrupted;
                attempt.code = Some("workflow.interrupted".into());
                attempt.message = Some(match attempt.operation {
                    Some(_) => "The step was dispatched when Exposition stopped; its outcome is unknown and it was not repeated.".into(),
                    None => "The step was about to be dispatched when Exposition stopped; it was not repeated.".into(),
                });
            }
        }
    }
    run.explain_outcomes();
}

fn decode(id: &str, state: &str, body: &str) -> Result<WorkflowRun, String> {
    if body.len() > MAX_BODY {
        return Err("run record too large".into());
    }
    let run: WorkflowRun = serde_json::from_str(body).map_err(|e| format!("unreadable run record: {e}"))?;
    if run.id.as_str() != id || state_name(run.state) != state {
        return Err("run record does not match its index".into());
    }
    let attempts_ok = |attempts: &[crate::model::WorkflowStepAttempt]| {
        attempts.len() <= MAX_ATTEMPTS
            && attempts.iter().all(|a| a.log.as_ref().is_none_or(|l| l.chars().count() <= MAX_ATTEMPT_LOG_CHARS + 1))
    };
    // Plans are flat after composite expansion; repeated steps have instances.
    if run.steps.len() > exposition_graph::workflow::MAX_EXPANDED_STEPS
        || run.steps.iter().any(|s| {
            !attempts_ok(&s.attempts)
                || s.instances.len() > crate::plan::MAX_FOR_EACH
                || s.instances.iter().any(|i| !attempts_ok(&i.attempts))
        })
    {
        return Err("run record exceeds its limits".into());
    }
    Ok(run)
}

pub fn summary(run: &WorkflowRun) -> WorkflowRunSummary {
    WorkflowRunSummary {
        id: run.id.clone(),
        workflow: run.workflow.clone(),
        revision: run.revision,
        title: run.title.clone(),
        state: run.state,
        started_at_ms: run.started_at_ms,
        finished_at_ms: run.finished_at_ms,
        // Steps that finished as planned: succeeded, or skipped by their own condition.
        steps_done: run
            .steps
            .iter()
            .filter(|s| matches!(s.state, WorkflowStepState::Succeeded | WorkflowStepState::ConditionNotMet))
            .count() as u32,
        steps_total: run.steps.len() as u32,
        acknowledged: run.acknowledged,
        cause: run.cause.clone(),
    }
}
