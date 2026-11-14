//! Task persistence in SQLite (application data directory).
//!
//! Stores tasks (spec, status, usage), the full provider message history as
//! content-block JSON (append-only), transcript notices, tool calls and
//! results, checkpoints, per-file review state, and every change record with
//! its patches and inverse patches. Schema versions are tracked with
//! `PRAGMA user_version` and applied in order.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use exposition_core::document::TextPatch;
use exposition_core::path::WorkspacePath;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::model::{
    Checkpoint, NoticeLevel, ReviewChangeKind, ReviewState, TaskId, TaskSpec, TaskStatus, TaskUsage, ToolCallStatus,
};

/// Current schema version.
pub const SCHEMA_VERSION: i64 = 3;

const MIGRATIONS: &[(i64, &str)] = &[
    (
        1,
        r#"
    CREATE TABLE tasks (
        id TEXT PRIMARY KEY,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        title TEXT NOT NULL,
        spec TEXT NOT NULL,
        status TEXT NOT NULL,
        usage TEXT NOT NULL,
        next_seq INTEGER NOT NULL DEFAULT 1
    );
    CREATE TABLE transcript (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        kind TEXT NOT NULL,
        role TEXT,
        content TEXT NOT NULL,
        PRIMARY KEY (task_id, seq)
    );
    CREATE TABLE tool_calls (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        tool_use_id TEXT NOT NULL,
        turn INTEGER NOT NULL,
        name TEXT NOT NULL,
        input TEXT NOT NULL,
        status TEXT NOT NULL,
        result TEXT,
        is_error INTEGER NOT NULL DEFAULT 0,
        started_at INTEGER,
        finished_at INTEGER,
        PRIMARY KEY (task_id, tool_use_id)
    );
    CREATE TABLE checkpoints (
        task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
        created_at INTEGER NOT NULL,
        documents TEXT NOT NULL
    );
    CREATE TABLE task_files (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        kind TEXT NOT NULL,
        document TEXT,
        checkpoint_text TEXT NOT NULL,
        baseline_text TEXT NOT NULL,
        tip_text TEXT NOT NULL,
        tip_revision INTEGER,
        entangled INTEGER NOT NULL DEFAULT 0,
        state TEXT NOT NULL,
        change_count INTEGER NOT NULL DEFAULT 0,
        message TEXT,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (task_id, path)
    );
    CREATE TABLE changes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        tool_use_id TEXT,
        path TEXT NOT NULL,
        document TEXT,
        kind TEXT NOT NULL,
        previous_revision INTEGER,
        revision INTEGER,
        patches TEXT NOT NULL,
        inverse TEXT NOT NULL,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX changes_by_task ON changes(task_id, id);
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    ),
    // The project graph: the graph a task is bound to, the graph contexts
    // attached to its runs, the files it resolved to graph identities, the
    // graph changes it proposed, and the investigation the user kept.
    (
        2,
        r#"
    CREATE TABLE task_graph (
        task_id TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
        project TEXT,
        investigation TEXT,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE task_graph_contexts (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        context TEXT NOT NULL,
        PRIMARY KEY (task_id, seq)
    );
    CREATE TABLE task_targets (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        record TEXT NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (task_id, path)
    );
    CREATE TABLE task_proposals (
        task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
        tool_use_id TEXT NOT NULL,
        record TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (task_id, tool_use_id)
    );
    "#,
    ),
    // P1 project operations: app-managed recoverable deletion of a task
    // (hidden from the task list until restored or purged).
    (
        3,
        r#"
    ALTER TABLE tasks ADD COLUMN deleted_at INTEGER;
    "#,
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("task database error: {0}")]
pub struct StoreError(pub String);

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError(e.to_string())
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError(format!("corrupt record: {e}"))
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: TaskId,
    pub title: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub spec: TaskSpec,
    pub status: TaskStatus,
    pub usage: TaskUsage,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptEntry {
    Message { role: String, content: Vec<Value> },
    Notice { level: NoticeLevel, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptRow {
    pub seq: u64,
    pub created_at_ms: u64,
    pub entry: TranscriptEntry,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRow {
    pub tool_use_id: String,
    pub turn: u32,
    pub name: String,
    pub input: Value,
    pub status: ToolCallStatus,
    pub result: Option<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskFileRow {
    pub path: WorkspacePath,
    pub kind: ReviewChangeKind,
    pub document: Option<String>,
    /// Text before this task first changed the file.
    pub checkpoint_text: String,
    /// The file without this task's changes, as of `tip_revision`.
    pub baseline_text: String,
    /// The file after this task's latest change.
    pub tip_text: String,
    pub tip_revision: Option<u64>,
    pub entangled: bool,
    pub state: ReviewState,
    pub change_count: u32,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChangeRow {
    pub id: i64,
    pub tool_use_id: Option<String>,
    pub path: WorkspacePath,
    pub document: Option<String>,
    /// `edit`, `create`, `revert`, `accept`, or `delete`.
    pub kind: String,
    pub previous_revision: Option<u64>,
    pub revision: Option<u64>,
    pub patches: Vec<TextPatch>,
    pub inverse: Vec<TextPatch>,
    pub created_at_ms: u64,
}

pub struct Store {
    conn: Mutex<Connection>,
    /// The database file (`None`: in memory).
    path: Option<PathBuf>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn i(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn u(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn to_json<T: Serialize>(value: &T) -> StoreResult<String> {
    Ok(serde_json::to_string(value)?)
}

fn from_json<T: DeserializeOwned>(raw: &str) -> StoreResult<T> {
    Ok(serde_json::from_str(raw)?)
}

impl Store {
    pub fn open(path: &Path) -> StoreResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError(format!("cannot create data directory: {e}")))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Some(path.to_path_buf()))
    }

    pub fn open_in_memory() -> StoreResult<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, path: Option<PathBuf>) -> StoreResult<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn), path })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn schema_version(&self) -> StoreResult<i64> {
        Ok(self.lock().pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    // ---- settings

    pub fn setting(&self, key: &str) -> StoreResult<Option<String>> {
        Ok(self.lock().query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> StoreResult<()> {
        self.lock().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- tasks

    pub fn insert_task(&self, row: &TaskRow) -> StoreResult<()> {
        self.lock().execute(
            "INSERT INTO tasks (id, created_at, updated_at, title, spec, status, usage) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.id.0,
                i(row.created_at_ms),
                i(row.updated_at_ms),
                row.title,
                to_json(&row.spec)?,
                to_json(&row.status)?,
                to_json(&row.usage)?
            ],
        )?;
        Ok(())
    }

    pub fn update_task(&self, id: &TaskId, status: &TaskStatus, usage: &TaskUsage) -> StoreResult<u64> {
        let now = now_ms();
        self.lock().execute(
            "UPDATE tasks SET status = ?2, usage = ?3, updated_at = ?4 WHERE id = ?1",
            params![id.0, to_json(status)?, to_json(usage)?, i(now)],
        )?;
        Ok(now)
    }

    pub fn update_task_spec(&self, id: &TaskId, spec: &TaskSpec) -> StoreResult<()> {
        self.lock().execute("UPDATE tasks SET spec = ?2 WHERE id = ?1", params![id.0, to_json(spec)?])?;
        Ok(())
    }

    pub fn task(&self, id: &TaskId) -> StoreResult<Option<TaskRow>> {
        let conn = self.lock();
        let raw = conn
            .query_row(
                "SELECT id, title, created_at, updated_at, spec, status, usage FROM tasks WHERE id = ?1 AND deleted_at IS NULL",
                [&id.0],
                raw_task,
            )
            .optional()?;
        raw.map(parse_task).transpose()
    }

    /// All tasks, newest first.
    pub fn tasks(&self) -> StoreResult<Vec<TaskRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, title, created_at, updated_at, spec, status, usage FROM tasks WHERE deleted_at IS NULL
             ORDER BY created_at DESC, id",
        )?;
        let raws = stmt.query_map([], raw_task)?.collect::<Result<Vec<_>, _>>()?;
        raws.into_iter().map(parse_task).collect()
    }

    // ---- P1 project operations: recoverable deletion, purge, sizes

    /// A task, deleted or not, with when it was deleted.
    pub fn task_any(&self, id: &TaskId) -> StoreResult<Option<(TaskRow, Option<u64>)>> {
        let conn = self.lock();
        let raw = conn
            .query_row(
                "SELECT id, title, created_at, updated_at, spec, status, usage, deleted_at FROM tasks WHERE id = ?1",
                [&id.0],
                |r| Ok((raw_task(r)?, r.get::<_, Option<i64>>(7)?)),
            )
            .optional()?;
        raw.map(|(raw, deleted)| Ok((parse_task(raw)?, deleted.map(u)))).transpose()
    }

    /// Hide a task (`Some(at)`) or bring it back (`None`); whether it changed.
    pub fn set_task_deleted(&self, id: &TaskId, at: Option<u64>) -> StoreResult<bool> {
        Ok(self.lock().execute("UPDATE tasks SET deleted_at = ?2 WHERE id = ?1", params![id.0, at.map(i)])? > 0)
    }

    /// Recoverably deleted tasks, newest deletion first.
    pub fn deleted_tasks(&self) -> StoreResult<Vec<(TaskRow, u64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, title, created_at, updated_at, spec, status, usage, deleted_at FROM tasks
             WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC, id",
        )?;
        let raws = stmt.query_map([], |r| Ok((raw_task(r)?, r.get::<_, i64>(7)?)))?.collect::<Result<Vec<_>, _>>()?;
        raws.into_iter().map(|(raw, at)| Ok((parse_task(raw)?, u(at)))).collect()
    }

    /// Remove a task and everything recorded for it (transcript, tool calls,
    /// checkpoints, change records, graph context) for good.
    pub fn purge_task(&self, id: &TaskId) -> StoreResult<bool> {
        Ok(self.lock().execute("DELETE FROM tasks WHERE id = ?1", [&id.0])? > 0)
    }

    /// Stored bytes of each task's records (every table that belongs to it).
    pub fn task_bytes(&self, id: &TaskId) -> StoreResult<u64> {
        let conn = self.lock();
        let sum = |sql: &str| -> StoreResult<i64> {
            Ok(conn.query_row(sql, [&id.0], |r| r.get::<_, Option<i64>>(0))?.unwrap_or(0))
        };
        let total = sum(
            "SELECT length(title) + length(spec) + length(status) + length(usage) FROM tasks WHERE id = ?1",
        )? + sum("SELECT sum(length(content)) FROM transcript WHERE task_id = ?1")?
            + sum("SELECT sum(length(input) + coalesce(length(result), 0)) FROM tool_calls WHERE task_id = ?1")?
            + sum("SELECT sum(length(documents)) FROM checkpoints WHERE task_id = ?1")?
            + sum(
                "SELECT sum(length(checkpoint_text) + length(baseline_text) + length(tip_text)) FROM task_files WHERE task_id = ?1",
            )?
            + sum("SELECT sum(length(patches) + length(inverse)) FROM changes WHERE task_id = ?1")?
            + sum("SELECT sum(length(context)) FROM task_graph_contexts WHERE task_id = ?1")?
            + sum("SELECT sum(length(record)) FROM task_targets WHERE task_id = ?1")?
            + sum("SELECT sum(length(record)) FROM task_proposals WHERE task_id = ?1")?;
        Ok(u(total))
    }

    /// Project files a task changed or resolved as targets.
    pub fn task_paths(&self, id: &TaskId) -> StoreResult<Vec<WorkspacePath>> {
        let conn = self.lock();
        let mut out: Vec<WorkspacePath> = Vec::new();
        for sql in
            ["SELECT path FROM task_files WHERE task_id = ?1", "SELECT path FROM task_targets WHERE task_id = ?1"]
        {
            let mut stmt = conn.prepare(sql)?;
            for path in stmt.query_map([&id.0], |r| r.get::<_, String>(0))? {
                if let Ok(p) = WorkspacePath::parse(&path?)
                    && !out.contains(&p)
                {
                    out.push(p);
                }
            }
        }
        Ok(out)
    }

    /// Bytes the database occupies on disk now (with its write-ahead log).
    pub fn file_bytes(&self) -> Option<u64> {
        let path = self.path.as_ref()?;
        let size = |p: PathBuf| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        Some(size(path.clone()) + size(PathBuf::from(format!("{}-wal", path.display()))))
    }

    /// Give removed records' space back (VACUUM, truncate the log); returns
    /// the on-disk size before and after.
    pub fn compact(&self) -> StoreResult<(Option<u64>, Option<u64>)> {
        let before = self.file_bytes();
        {
            let conn = self.lock();
            conn.execute_batch("VACUUM;")?;
            let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        }
        Ok((before, self.file_bytes()))
    }

    /// Mark tasks left active by a previous process as interrupted.
    pub fn mark_interrupted(&self) -> StoreResult<Vec<TaskId>> {
        let mut changed = Vec::new();
        for task in self.tasks()? {
            if task.status.is_active() {
                self.update_task(&task.id, &TaskStatus::Interrupted, &task.usage)?;
                self.lock().execute(
                    "UPDATE tool_calls SET status = ?2 WHERE task_id = ?1 AND status IN (?3, ?4, ?5)",
                    params![
                        task.id.0,
                        to_json(&ToolCallStatus::NotRun)?,
                        to_json(&ToolCallStatus::Running)?,
                        to_json(&ToolCallStatus::AwaitingApproval)?,
                        to_json(&ToolCallStatus::Pending)?
                    ],
                )?;
                changed.push(task.id);
            }
        }
        Ok(changed)
    }

    // ---- transcript

    fn next_seq(conn: &Connection, task: &TaskId) -> StoreResult<u64> {
        let seq: i64 = conn.query_row("SELECT next_seq FROM tasks WHERE id = ?1", [&task.0], |r| r.get(0))?;
        conn.execute("UPDATE tasks SET next_seq = next_seq + 1 WHERE id = ?1", [&task.0])?;
        Ok(u(seq))
    }

    pub fn append_message(&self, task: &TaskId, role: &str, content: &[Value]) -> StoreResult<u64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let seq = Self::next_seq(&tx, task)?;
        tx.execute(
            "INSERT INTO transcript (task_id, seq, created_at, kind, role, content) VALUES (?1, ?2, ?3, 'message', ?4, ?5)",
            params![task.0, i(seq), i(now_ms()), role, to_json(&content)?],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    pub fn append_notice(&self, task: &TaskId, level: NoticeLevel, message: &str) -> StoreResult<u64> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let seq = Self::next_seq(&tx, task)?;
        tx.execute(
            "INSERT INTO transcript (task_id, seq, created_at, kind, role, content) VALUES (?1, ?2, ?3, 'notice', ?4, ?5)",
            params![task.0, i(seq), i(now_ms()), to_json(&level)?, message],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    pub fn transcript(&self, task: &TaskId) -> StoreResult<Vec<TranscriptRow>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT seq, created_at, kind, role, content FROM transcript WHERE task_id = ?1 ORDER BY seq")?;
        let raws = stmt
            .query_map([&task.0], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        raws.into_iter()
            .map(|(seq, created, kind, role, content)| {
                let entry = if kind == "message" {
                    TranscriptEntry::Message { role: role.unwrap_or_default(), content: from_json(&content)? }
                } else {
                    TranscriptEntry::Notice {
                        level: from_json(role.as_deref().unwrap_or("\"info\""))?,
                        message: content,
                    }
                };
                Ok(TranscriptRow { seq: u(seq), created_at_ms: u(created), entry })
            })
            .collect()
    }

    /// Provider message history, in order.
    pub fn messages(&self, task: &TaskId) -> StoreResult<Vec<(String, Vec<Value>)>> {
        Ok(self
            .transcript(task)?
            .into_iter()
            .filter_map(|row| match row.entry {
                TranscriptEntry::Message { role, content } => Some((role, content)),
                TranscriptEntry::Notice { .. } => None,
            })
            .collect())
    }

    // ---- tool calls

    pub fn upsert_tool_call(&self, task: &TaskId, row: &ToolCallRow) -> StoreResult<()> {
        let now = i(now_ms());
        let finished = row.status.is_finished();
        // A queued call has not started yet; it starts when it first runs.
        let started = (row.status != ToolCallStatus::Pending).then_some(now);
        self.lock().execute(
            "INSERT INTO tool_calls (task_id, tool_use_id, turn, name, input, status, result, is_error, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(task_id, tool_use_id) DO UPDATE SET status = excluded.status, result = excluded.result,
               is_error = excluded.is_error, started_at = COALESCE(tool_calls.started_at, excluded.started_at),
               finished_at = excluded.finished_at",
            params![
                task.0,
                row.tool_use_id,
                row.turn,
                row.name,
                to_json(&row.input)?,
                to_json(&row.status)?,
                row.result,
                row.is_error,
                started,
                if finished { Some(now) } else { None }
            ],
        )?;
        Ok(())
    }

    pub fn tool_calls(&self, task: &TaskId) -> StoreResult<Vec<ToolCallRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tool_use_id, turn, name, input, status, result, is_error FROM tool_calls WHERE task_id = ?1 ORDER BY rowid",
        )?;
        let raws = stmt
            .query_map([&task.0], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        raws.into_iter()
            .map(|(tool_use_id, turn, name, input, status, result, is_error)| {
                Ok(ToolCallRow {
                    tool_use_id,
                    turn,
                    name,
                    input: from_json(&input)?,
                    status: from_json(&status)?,
                    result,
                    is_error,
                })
            })
            .collect()
    }

    // ---- checkpoints

    pub fn insert_checkpoint(&self, task: &TaskId, checkpoint: &Checkpoint) -> StoreResult<()> {
        self.lock().execute(
            "INSERT OR IGNORE INTO checkpoints (task_id, created_at, documents) VALUES (?1, ?2, ?3)",
            params![task.0, i(checkpoint.created_at_ms), to_json(&checkpoint.documents)?],
        )?;
        Ok(())
    }

    pub fn checkpoint(&self, task: &TaskId) -> StoreResult<Option<Checkpoint>> {
        let conn = self.lock();
        let raw = conn
            .query_row("SELECT created_at, documents FROM checkpoints WHERE task_id = ?1", [&task.0], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?;
        raw.map(|(created, docs)| Ok(Checkpoint { created_at_ms: u(created), documents: from_json(&docs)? }))
            .transpose()
    }

    // ---- review

    pub fn upsert_task_file(&self, task: &TaskId, row: &TaskFileRow) -> StoreResult<()> {
        self.lock().execute(
            "INSERT INTO task_files (task_id, path, kind, document, checkpoint_text, baseline_text, tip_text, tip_revision,
                entangled, state, change_count, message, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(task_id, path) DO UPDATE SET kind = excluded.kind, document = excluded.document,
               baseline_text = excluded.baseline_text, tip_text = excluded.tip_text, tip_revision = excluded.tip_revision,
               entangled = excluded.entangled, state = excluded.state, change_count = excluded.change_count,
               message = excluded.message, updated_at = excluded.updated_at",
            params![
                task.0,
                row.path.as_str(),
                to_json(&row.kind)?,
                row.document,
                row.checkpoint_text,
                row.baseline_text,
                row.tip_text,
                row.tip_revision.map(i),
                row.entangled,
                to_json(&row.state)?,
                row.change_count,
                row.message,
                i(now_ms())
            ],
        )?;
        Ok(())
    }

    pub fn task_file(&self, task: &TaskId, path: &WorkspacePath) -> StoreResult<Option<TaskFileRow>> {
        Ok(self.task_files(task)?.into_iter().find(|f| &f.path == path))
    }

    pub fn task_files(&self, task: &TaskId) -> StoreResult<Vec<TaskFileRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT path, kind, document, checkpoint_text, baseline_text, tip_text, tip_revision, entangled, state,
                change_count, message FROM task_files WHERE task_id = ?1 ORDER BY path",
        )?;
        type Raw =
            (String, String, Option<String>, String, String, String, Option<i64>, bool, String, u32, Option<String>);
        let raws: Vec<Raw> = stmt
            .query_map([&task.0], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        raws.into_iter()
            .map(|(path, kind, document, checkpoint, baseline, tip, tip_rev, entangled, state, count, message)| {
                Ok(TaskFileRow {
                    path: WorkspacePath::parse(&path).map_err(|e| StoreError(e.to_string()))?,
                    kind: from_json(&kind)?,
                    document,
                    checkpoint_text: checkpoint,
                    baseline_text: baseline,
                    tip_text: tip,
                    tip_revision: tip_rev.map(u),
                    entangled,
                    state: from_json(&state)?,
                    change_count: count,
                    message,
                })
            })
            .collect()
    }

    pub fn insert_change(&self, task: &TaskId, row: &ChangeRow) -> StoreResult<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO changes (task_id, tool_use_id, path, document, kind, previous_revision, revision, patches, inverse, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                task.0,
                row.tool_use_id,
                row.path.as_str(),
                row.document,
                row.kind,
                row.previous_revision.map(i),
                row.revision.map(i),
                to_json(&row.patches)?,
                to_json(&row.inverse)?,
                i(row.created_at_ms)
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn changes(&self, task: &TaskId) -> StoreResult<Vec<ChangeRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, tool_use_id, path, document, kind, previous_revision, revision, patches, inverse, created_at
             FROM changes WHERE task_id = ?1 ORDER BY id",
        )?;
        type Raw = (i64, Option<String>, String, Option<String>, String, Option<i64>, Option<i64>, String, String, i64);
        let raws: Vec<Raw> = stmt
            .query_map([&task.0], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        raws.into_iter()
            .map(|(id, tool_use_id, path, document, kind, prev, rev, patches, inverse, created)| {
                Ok(ChangeRow {
                    id,
                    tool_use_id,
                    path: WorkspacePath::parse(&path).map_err(|e| StoreError(e.to_string()))?,
                    document,
                    kind,
                    previous_revision: prev.map(u),
                    revision: rev.map(u),
                    patches: from_json(&patches)?,
                    inverse: from_json(&inverse)?,
                    created_at_ms: u(created),
                })
            })
            .collect()
    }

    // ---- project graph

    /// The project graph a task is bound to (recorded with its first context).
    pub fn graph_project(&self, task: &TaskId) -> StoreResult<Option<exposition_graph::ProjectId>> {
        let raw: Option<Option<String>> = self
            .lock()
            .query_row("SELECT project FROM task_graph WHERE task_id = ?1", [&task.0], |r| r.get(0))
            .optional()?;
        raw.flatten().map(|p| exposition_graph::ProjectId::parse(&p).map_err(|e| StoreError(e.to_string()))).transpose()
    }

    pub fn investigation(&self, task: &TaskId) -> StoreResult<Option<exposition_graph::EntityId>> {
        let raw: Option<Option<String>> = self
            .lock()
            .query_row("SELECT investigation FROM task_graph WHERE task_id = ?1", [&task.0], |r| r.get(0))
            .optional()?;
        raw.flatten().map(|p| exposition_graph::EntityId::parse(&p).map_err(|e| StoreError(e.to_string()))).transpose()
    }

    pub fn set_investigation(&self, task: &TaskId, entity: &exposition_graph::EntityId) -> StoreResult<()> {
        self.lock().execute(
            "INSERT INTO task_graph (task_id, investigation, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(task_id) DO UPDATE SET investigation = excluded.investigation, updated_at = excluded.updated_at",
            params![task.0, entity.as_str(), i(now_ms())],
        )?;
        Ok(())
    }

    /// Record a graph context attached to a run; binds the task to the
    /// context's project graph the first time.
    pub fn insert_graph_context(&self, task: &TaskId, context: &crate::graph::GraphContext) -> StoreResult<()> {
        let now = i(now_ms());
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT coalesce(max(seq), 0) + 1 FROM task_graph_contexts WHERE task_id = ?1",
            [&task.0],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO task_graph_contexts (task_id, seq, created_at, context) VALUES (?1, ?2, ?3, ?4)",
            params![task.0, seq, now, to_json(context)?],
        )?;
        tx.execute(
            "INSERT INTO task_graph (task_id, project, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(task_id) DO UPDATE SET project = coalesce(task_graph.project, excluded.project),
             updated_at = excluded.updated_at",
            params![task.0, context.project.as_str(), now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The latest graph context of a task and how many were attached.
    pub fn graph_context(&self, task: &TaskId) -> StoreResult<(Option<crate::graph::GraphContext>, u32)> {
        let conn = self.lock();
        let count: i64 =
            conn.query_row("SELECT count(*) FROM task_graph_contexts WHERE task_id = ?1", [&task.0], |r| r.get(0))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT context FROM task_graph_contexts WHERE task_id = ?1 ORDER BY seq DESC LIMIT 1",
                [&task.0],
                |r| r.get(0),
            )
            .optional()?;
        Ok((raw.map(|r| from_json(&r)).transpose()?, count as u32))
    }

    /// Record (or merge into) a task target.
    pub fn upsert_target(&self, task: &TaskId, target: &crate::graph::TaskTarget) -> StoreResult<()> {
        let mut merged = target.clone();
        if let Some(existing) = self.targets(task)?.into_iter().find(|t| t.path == target.path) {
            let mut uses = existing.uses;
            for u in &target.uses {
                if !uses.contains(u) {
                    uses.push(*u);
                }
            }
            uses.sort();
            merged.uses = uses;
            merged.entity = target.entity.clone().or(existing.entity);
            merged.operation = target.operation.clone().or(existing.operation);
        }
        self.lock().execute(
            "INSERT INTO task_targets (task_id, path, record, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(task_id, path) DO UPDATE SET record = excluded.record, updated_at = excluded.updated_at",
            params![task.0, merged.path.as_str(), to_json(&merged)?, i(merged.updated_at_ms)],
        )?;
        Ok(())
    }

    pub fn targets(&self, task: &TaskId) -> StoreResult<Vec<crate::graph::TaskTarget>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT record FROM task_targets WHERE task_id = ?1 ORDER BY path LIMIT 2000")?;
        let raws = stmt.query_map([&task.0], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        raws.iter().map(|r| from_json(r)).collect()
    }

    pub fn upsert_proposal(&self, task: &TaskId, proposal: &crate::graph::TaskProposal) -> StoreResult<()> {
        self.lock().execute(
            "INSERT INTO task_proposals (task_id, tool_use_id, record, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(task_id, tool_use_id) DO UPDATE SET record = excluded.record",
            params![task.0, proposal.tool_use_id, to_json(proposal)?, i(proposal.created_at_ms)],
        )?;
        Ok(())
    }

    pub fn proposal(&self, task: &TaskId, tool_use_id: &str) -> StoreResult<Option<crate::graph::TaskProposal>> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT record FROM task_proposals WHERE task_id = ?1 AND tool_use_id = ?2",
                params![task.0, tool_use_id],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|r| from_json(&r)).transpose()
    }

    pub fn proposals(&self, task: &TaskId) -> StoreResult<Vec<crate::graph::TaskProposal>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT record FROM task_proposals WHERE task_id = ?1 ORDER BY created_at, tool_use_id LIMIT 1000",
        )?;
        let raws = stmt.query_map([&task.0], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        raws.iter().map(|r| from_json(r)).collect()
    }
}

type RawTask = (String, String, i64, i64, String, String, String);

fn raw_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawTask> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
}

fn parse_task((id, title, created, updated, spec, status, usage): RawTask) -> StoreResult<TaskRow> {
    Ok(TaskRow {
        id: TaskId(id),
        title,
        created_at_ms: u(created),
        updated_at_ms: u(updated),
        spec: from_json(&spec)?,
        status: from_json(&status)?,
        usage: from_json(&usage)?,
    })
}

fn migrate(conn: &Connection) -> StoreResult<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError(format!(
            "the task database uses schema version {current}, newer than this build supports ({SCHEMA_VERSION})"
        )));
    }
    for (version, sql) in MIGRATIONS {
        if *version > current {
            conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn spec() -> TaskSpec {
        TaskSpec {
            prompt: "Fix the intro".into(),
            workspace_root: "C:/project".into(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::Disabled,
            consent: ContextConsent::ScopeContent,
            budgets: Budgets::default(),
            provider: ProviderChoice {
                provider: ProviderKind::Anthropic,
                model: DEFAULT_MODEL.into(),
                effort: Effort::Medium,
            },
        }
    }

    #[test]
    fn persists_tasks_history_and_marks_interrupted_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("agents.sqlite3");
        let id = TaskId::new();
        {
            let store = Store::open(&db).unwrap();
            assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
            store
                .insert_task(&TaskRow {
                    id: id.clone(),
                    title: "Fix the intro".into(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                    spec: spec(),
                    status: TaskStatus::Running,
                    usage: TaskUsage::default(),
                })
                .unwrap();
            store.append_message(&id, "user", &[serde_json::json!({"type": "text", "text": "hi"})]).unwrap();
            store.append_notice(&id, NoticeLevel::Warning, "careful").unwrap();
            store
                .append_message(
                    &id,
                    "assistant",
                    &[serde_json::json!({"type": "thinking", "thinking": "", "signature": "abc"})],
                )
                .unwrap();
        }
        let store = Store::open(&db).unwrap();
        assert_eq!(store.mark_interrupted().unwrap(), vec![id.clone()]);
        let task = store.task(&id).unwrap().unwrap();
        assert_eq!(task.status, TaskStatus::Interrupted);
        assert_eq!(task.spec, spec());
        let messages = store.messages(&id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].1[0]["signature"], "abc");
        let transcript = store.transcript(&id).unwrap();
        assert_eq!(transcript.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    /// P1: a schema-2 task database keeps its tasks when it gains
    /// `deleted_at`; deleted tasks leave the list and lookups until restored,
    /// and a purge removes every record of the task.
    #[test]
    fn version_2_tasks_survive_and_deletion_is_recoverable_then_purgeable() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("agents.sqlite3");
        let id = TaskId::new();
        {
            let conn = Connection::open(&db).unwrap();
            for (version, sql) in &MIGRATIONS[..2] {
                conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;")).unwrap();
            }
            conn.execute(
                "INSERT INTO tasks (id, created_at, updated_at, title, spec, status, usage) VALUES (?1, 1, 1, 'Old', ?2, ?3, ?4)",
                params![
                    id.0,
                    serde_json::to_string(&spec()).unwrap(),
                    serde_json::to_string(&TaskStatus::Completed).unwrap(),
                    serde_json::to_string(&TaskUsage::default()).unwrap()
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO transcript (task_id, seq, created_at, kind, content) VALUES (?1, 1, 1, 'notice', '{}')",
                [&id.0],
            )
            .unwrap();
        }
        let store = Store::open(&db).unwrap();
        assert_eq!(store.schema_version().unwrap(), 3);
        assert_eq!(store.tasks().unwrap().len(), 1, "existing tasks stay loadable");
        assert!(store.task_bytes(&id).unwrap() > 0);

        assert!(store.set_task_deleted(&id, Some(10)).unwrap());
        assert!(store.tasks().unwrap().is_empty());
        assert!(store.task(&id).unwrap().is_none());
        assert_eq!(store.deleted_tasks().unwrap().len(), 1);
        assert_eq!(store.task_any(&id).unwrap().unwrap().1, Some(10));
        assert!(store.set_task_deleted(&id, None).unwrap());
        assert_eq!(store.tasks().unwrap().len(), 1, "restored");

        assert!(store.purge_task(&id).unwrap());
        assert!(store.task_any(&id).unwrap().is_none());
        let left: i64 = store.lock().query_row("SELECT count(*) FROM transcript", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "the purge removes the task's transcript too");
        let (before, after) = store.compact().unwrap();
        assert!(before.is_some() && after.is_some());
    }

    #[test]
    fn refuses_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("agents.sqlite3");
        {
            let conn = Connection::open(&db).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        }
        assert!(Store::open(&db).is_err());
    }
}
