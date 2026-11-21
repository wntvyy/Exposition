//! SQLite persistence of one project graph (`.exposition/graph.sqlite3`).
//!
//! The database is project data and may come from anywhere (a cloned
//! repository, a shared folder), so it is opened as untrusted input:
//! extension loading stays disabled, the connection is defensive (no writes to
//! the schema through pragmas, no triggers or views run), `trusted_schema` is
//! off, the schema must contain exactly the objects this version created, and
//! a newer schema version is refused rather than misread. Every statement is
//! parameterized and every read is bounded.
//!
//! Curated records (entities people/agents admitted, accepted relations,
//! evidence, views, workflow definitions) are not a cache: nothing here
//! replaces a damaged database with an empty one.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

use crate::ids::{EntityId, EvidenceId, ProjectId, RelationId, ViewId};
use crate::model::{
    AssertionStatus, BindingState, CoverageState, CoverageSummary, Entity, Evidence, GraphDelta, Lifecycle, Origin,
    Relation, ResourceCoverage, SourceBinding, SourceSelector, SourceVersion,
};
use crate::query::{GraphView, GraphViewSummary, ViewScope};
use crate::vocabulary::{EntityKind, EvidenceKind, RelationKind, ResourceRole, VOCABULARY_VERSION};

/// Current schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: u32 = 5;

/// File name of the graph database inside `.exposition/`.
pub const DATABASE_FILE: &str = "graph.sqlite3";

/// Largest graph database file opened (a hostile or runaway file is refused).
pub const MAX_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;

const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        r#"
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE entities (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        roles TEXT NOT NULL,
        label TEXT NOT NULL,
        key TEXT,
        lifecycle TEXT NOT NULL,
        tombstone_reason TEXT,
        revision INTEGER NOT NULL,
        origin TEXT NOT NULL,
        attributes TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE INDEX entities_kind ON entities(kind);
    CREATE INDEX entities_key ON entities(key);
    CREATE TABLE bindings (
        entity_id TEXT NOT NULL REFERENCES entities(id),
        ordinal INTEGER NOT NULL,
        path TEXT NOT NULL,
        selector TEXT NOT NULL,
        range_start INTEGER,
        range_end INTEGER,
        content_hash TEXT,
        revision INTEGER,
        saved INTEGER NOT NULL DEFAULT 1,
        fingerprint TEXT,
        state TEXT NOT NULL,
        detail TEXT,
        PRIMARY KEY (entity_id, ordinal)
    );
    CREATE INDEX bindings_path ON bindings(path);
    CREATE TABLE relations (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        from_id TEXT NOT NULL REFERENCES entities(id),
        to_id TEXT NOT NULL REFERENCES entities(id),
        origin TEXT NOT NULL,
        status TEXT NOT NULL,
        evidence_kind TEXT,
        evidence TEXT NOT NULL,
        from_version TEXT,
        to_version TEXT,
        note TEXT,
        revision INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE INDEX relations_from ON relations(from_id);
    CREATE INDEX relations_to ON relations(to_id);
    CREATE TABLE evidence (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        subject TEXT,
        body TEXT NOT NULL,
        recorded_at INTEGER NOT NULL
    );
    CREATE INDEX evidence_subject ON evidence(subject);
    CREATE TABLE changes (
        revision INTEGER PRIMARY KEY,
        at INTEGER NOT NULL,
        origin TEXT NOT NULL,
        summary TEXT NOT NULL,
        delta TEXT NOT NULL
    );
    CREATE TABLE views (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        scope TEXT NOT NULL,
        body TEXT NOT NULL,
        revision INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE workflows (
        id TEXT NOT NULL,
        revision INTEGER NOT NULL,
        hash TEXT NOT NULL,
        state TEXT NOT NULL,
        body TEXT NOT NULL,
        saved_at INTEGER NOT NULL,
        PRIMARY KEY (id, revision)
    );
    CREATE TABLE coverage (
        path TEXT PRIMARY KEY,
        extractor TEXT,
        state TEXT NOT NULL,
        unresolved INTEGER NOT NULL DEFAULT 0,
        detail TEXT,
        content_hash TEXT,
        indexed_at INTEGER NOT NULL
    );
    "#,
    ),
    (
        // Reconciliation checkpoints: the file state last indexed (size,
        // modification time, physical identity for rename confirmation) and
        // the rebuildable per-file source index (labels, symbolic links).
        2,
        r#"
    ALTER TABLE coverage ADD COLUMN size INTEGER;
    ALTER TABLE coverage ADD COLUMN modified_ms INTEGER;
    ALTER TABLE coverage ADD COLUMN file_identity TEXT;
    ALTER TABLE coverage ADD COLUMN pending INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE source_index (
        path TEXT PRIMARY KEY,
        content_hash TEXT NOT NULL,
        extractor TEXT NOT NULL,
        body TEXT NOT NULL,
        wanted TEXT NOT NULL DEFAULT '[]'
    );
    "#,
    ),
    (
        // Evidence recorded by this installation carries a MAC under an
        // app-local key; evidence without a valid one was imported.
        3,
        r#"
    CREATE TABLE evidence_seals (id TEXT PRIMARY KEY, mac TEXT NOT NULL);
    "#,
    ),
    (
        // Project operations (P1): app-managed recoverable deletions of
        // graph-held records (a snapshot to restore or purge), and saved
        // workflows removed from active use whose pinned revisions are kept
        // for the composites that run them.
        4,
        r#"
    CREATE TABLE recoverable (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        label TEXT NOT NULL,
        body TEXT NOT NULL,
        deleted_at INTEGER NOT NULL
    );
    CREATE TABLE retired_workflows (id TEXT PRIMARY KEY, retired_at INTEGER NOT NULL);
    "#,
    ),
    (
        // The statement a person reviewed a `formalizes` link against (a
        // digest of the bound text, see `review`). Existing relations keep
        // NULL: what their review covered was never recorded.
        5,
        r#"
    ALTER TABLE relations ADD COLUMN reviewed_statement TEXT;
    "#,
    ),
];

/// Every schema object a valid database of `version` has (type, name).
fn expected_objects(version: u32) -> Vec<(&'static str, &'static str)> {
    let mut objects = vec![
        ("table", "meta"),
        ("table", "entities"),
        ("index", "entities_kind"),
        ("index", "entities_key"),
        ("table", "bindings"),
        ("index", "bindings_path"),
        ("table", "relations"),
        ("index", "relations_from"),
        ("index", "relations_to"),
        ("table", "evidence"),
        ("index", "evidence_subject"),
        ("table", "changes"),
        ("table", "views"),
        ("table", "workflows"),
        ("table", "coverage"),
    ];
    if version >= 2 {
        objects.push(("table", "source_index"));
    }
    if version >= 3 {
        objects.push(("table", "evidence_seals"));
    }
    if version >= 4 {
        objects.push(("table", "recoverable"));
        objects.push(("table", "retired_workflows"));
    }
    objects
}

/// Columns of each table of `version` (a hostile file may reuse our table
/// names with other columns).
fn expected_columns(version: u32) -> Vec<(&'static str, Vec<&'static str>)> {
    let mut coverage = vec!["path", "extractor", "state", "unresolved", "detail", "content_hash", "indexed_at"];
    if version >= 2 {
        coverage.extend(["size", "modified_ms", "file_identity", "pending"]);
    }
    let mut tables = vec![
        ("meta", vec!["key", "value"]),
        (
            "entities",
            vec![
                "id",
                "kind",
                "roles",
                "label",
                "key",
                "lifecycle",
                "tombstone_reason",
                "revision",
                "origin",
                "attributes",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "bindings",
            vec![
                "entity_id",
                "ordinal",
                "path",
                "selector",
                "range_start",
                "range_end",
                "content_hash",
                "revision",
                "saved",
                "fingerprint",
                "state",
                "detail",
            ],
        ),
        (
            "relations",
            vec![
                "id",
                "kind",
                "from_id",
                "to_id",
                "origin",
                "status",
                "evidence_kind",
                "evidence",
                "from_version",
                "to_version",
                "note",
                "revision",
                "created_at",
                "updated_at",
            ],
        ),
        ("evidence", vec!["id", "kind", "subject", "body", "recorded_at"]),
        ("changes", vec!["revision", "at", "origin", "summary", "delta"]),
        ("views", vec!["id", "name", "scope", "body", "revision", "updated_at"]),
        ("workflows", vec!["id", "revision", "hash", "state", "body", "saved_at"]),
        ("coverage", coverage),
    ];
    if version >= 2 {
        tables.push(("source_index", vec!["path", "content_hash", "extractor", "body", "wanted"]));
    }
    if version >= 3 {
        tables.push(("evidence_seals", vec!["id", "mac"]));
    }
    if version >= 4 {
        tables.push(("recoverable", vec!["id", "kind", "label", "body", "deleted_at"]));
        tables.push(("retired_workflows", vec!["id", "retired_at"]));
    }
    if version >= 5
        && let Some((_, columns)) = tables.iter_mut().find(|(t, _)| *t == "relations")
    {
        columns.push("reviewed_statement");
    }
    tables
}

/// Row limits per table: a database beyond them is refused rather than read.
const TABLES: usize = 13;
const ROW_LIMITS: [(&str, i64); TABLES] = [
    ("meta", 1_000),
    ("entities", 500_000),
    ("bindings", 1_000_000),
    ("relations", 2_000_000),
    ("evidence", 200_000),
    ("changes", 20_000),
    ("views", 10_000),
    ("workflows", 100_000),
    ("coverage", 500_000),
    ("source_index", 500_000),
    ("evidence_seals", 200_000),
    ("recoverable", 100_000),
    ("retired_workflows", 100_000),
];

/// Upper bound on what one bounded read returns. Reads that must be complete
/// (an entity's relations for impact, validation and reconciliation; coverage;
/// a file's bindings) are bounded by their table's row limit instead, and the
/// reads that stay bounded here say when they left rows out.
pub const MAX_ROWS: usize = 20_000;
/// Text fields read from the database are bounded (hostile files).
const MAX_FIELD: usize = 256 * 1024;
/// Short fields (hashes, versions, fingerprints, extractor names).
const MAX_SHORT_FIELD: usize = 256;
/// Largest stored source index body (the validated open refuses larger).
pub const MAX_INDEX_BODY: usize = 16 * MAX_FIELD;
/// Rows one read of the entities with a natural key returns.
pub const KEY_READ_LIMIT: usize = 64;
/// Bindings read per entity (more would be silently dropped on read).
pub const MAX_BINDINGS_PER_ENTITY: usize = 64;

/// The row limit of `table` the validated open enforces.
pub fn row_limit(table: &str) -> i64 {
    ROW_LIMITS.iter().find(|(t, _)| *t == table).map(|(_, n)| *n).unwrap_or(i64::MAX)
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "the graph database was written by a newer version of Exposition (schema {found}, this version reads {supported})"
    )]
    NewerSchema { found: u32, supported: u32 },
    #[error("the graph database is damaged: {0}")]
    Corrupt(String),
    #[error("the graph database has unexpected contents: {0}")]
    Untrusted(String),
    #[error("graph database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("graph record could not be encoded: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// A write would exceed a bound the validated open enforces (the next
    /// open would refuse the database): refused before it happens.
    #[error("{0}")]
    Limit(String),
}

pub type StoreResult<T> = Result<T, StoreError>;

fn limit(what: impl Into<String>) -> StoreError {
    StoreError::Limit(what.into())
}

fn check_field(value: Option<&str>, max: usize, what: &str) -> StoreResult<()> {
    match value {
        Some(v) if v.len() > max => Err(limit(format!("{what} is longer than {max} bytes"))),
        _ => Ok(()),
    }
}

fn check_record(json: &str, what: &str) -> StoreResult<()> {
    if json.len() > 4 * MAX_FIELD {
        return Err(limit(format!("{what} is larger than {} KiB", 4 * MAX_FIELD / 1024)));
    }
    Ok(())
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// One open project graph database.
pub struct GraphStore {
    conn: Connection,
    path: Option<PathBuf>,
    project: ProjectId,
    evidence_key: Option<[u8; 32]>,
    /// Row headroom per table, so a commit does not count every table
    /// (see [`Writer::commit`]).
    budget: RowBudget,
    /// The status aggregates as of one write stamp (see [`Self::status_summary`]).
    summary: RefCell<Option<StatusSummary>>,
}

/// The status aggregates and the write stamp they were computed at.
type StatusSummary = (WriteStamp, (u32, u32), CoverageSummary);

/// Identifies the database contents as this connection sees them: the rows
/// this connection changed so far, and SQLite's counter of commits made by
/// any other connection. Equal stamps mean no row changed in between.
type WriteStamp = (u64, i64);

/// What is known about how many rows each table can still take.
///
/// Owner: the open [`GraphStore`]. Key: the write stamp it was computed at.
/// A commit subtracts every row changed since then from every table's
/// headroom (an over-estimate of what the table gained) and counts a table
/// again only when that estimate is used up, so the limit the validated
/// open enforces is never exceeded and the cost follows the size of the
/// change. Dropped with the store; unknown after another connection wrote.
#[derive(Default)]
struct RowBudget {
    state: Cell<Option<BudgetState>>,
}

#[derive(Clone, Copy)]
struct BudgetState {
    stamp: WriteStamp,
    headroom: [i64; TABLES],
}

impl GraphStore {
    /// Open (or, when `create`, create) the database at `path` as untrusted
    /// input. Fails with [`StoreError::NewerSchema`], [`StoreError::Corrupt`],
    /// or [`StoreError::Untrusted`] instead of guessing.
    pub fn open(path: &Path, create: bool) -> StoreResult<Self> {
        if let Ok(meta) = std::fs::metadata(path)
            && meta.len() > MAX_DATABASE_BYTES
        {
            return Err(StoreError::Untrusted(format!(
                "the file is {} MiB; graphs larger than {} MiB are not opened",
                meta.len() / 1_048_576,
                MAX_DATABASE_BYTES / 1_048_576
            )));
        }
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let conn = Connection::open_with_flags(path, flags)?;
        Self::init(conn, Some(path.to_path_buf()))
    }

    /// Open an existing graph without writing to it (read-only storage): the
    /// file size, schema objects and project identity are checked; it is
    /// never migrated. Copy it (`vacuum_into`) to work with it.
    pub fn open_read_only(path: &Path) -> StoreResult<Self> {
        if let Ok(meta) = std::fs::metadata(path)
            && meta.len() > MAX_DATABASE_BYTES
        {
            return Err(StoreError::Untrusted("the graph file is too large".into()));
        }
        let conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
        conn.pragma_update(None, "trusted_schema", "OFF")?;
        conn.pragma_update(None, "cell_size_check", "ON")?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema { found: version, supported: SCHEMA_VERSION });
        }
        if version == 0 {
            return Err(StoreError::Untrusted("the file is not a project graph".into()));
        }
        let check: String = conn
            .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        if check != "ok" {
            return Err(StoreError::Corrupt(truncate(&check, 300)));
        }
        verify_schema_objects(&conn, version)?;
        let raw: Option<String> =
            conn.query_row("SELECT value FROM meta WHERE key = 'project_id'", [], |r| r.get(0)).optional()?;
        let project = raw
            .as_deref()
            .and_then(|r| ProjectId::parse(r).ok())
            .ok_or_else(|| StoreError::Untrusted("invalid project identifier".into()))?;
        Ok(Self {
            conn,
            path: Some(path.to_path_buf()),
            project,
            evidence_key: None,
            budget: RowBudget::default(),
            summary: RefCell::new(None),
        })
    }

    /// An in-memory graph (tests, temporary contexts).
    pub fn open_in_memory() -> StoreResult<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(conn: Connection, path: Option<PathBuf>) -> StoreResult<Self> {
        harden(&conn)?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema { found: version, supported: SCHEMA_VERSION });
        }
        if version > 0 {
            let check: String = conn
                .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
                .map_err(|e| StoreError::Corrupt(e.to_string()))?;
            if check != "ok" {
                return Err(StoreError::Corrupt(truncate(&check, 300)));
            }
            verify_schema_objects(&conn, version)?;
        } else {
            // A database with user_version 0 must be empty before we create
            // our schema in it (never adopt someone else's tables).
            let objects: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            if objects > 0 {
                return Err(StoreError::Untrusted("the file contains tables this version did not create".into()));
            }
        }
        let mut store = Self {
            conn,
            path,
            project: ProjectId::mint(),
            evidence_key: None,
            budget: RowBudget::default(),
            summary: RefCell::new(None),
        };
        if version > 0 && version < SCHEMA_VERSION {
            // Keep a consistent copy of the older graph before changing it.
            if let Some(dir) = store.path.as_deref().and_then(Path::parent) {
                let backups = dir.join(crate::backup::BACKUP_DIR);
                crate::backup::create(&store, &backups, "premigration").map_err(|e| {
                    StoreError::Corrupt(format!("the graph could not be backed up before upgrading it: {e}"))
                })?;
            }
        }
        store.migrate(version)?;
        if version > 0 {
            store.verify_contents()?;
        }
        store.project = store.load_or_create_project()?;
        Ok(store)
    }

    /// Validate every stored record of an existing database (it may come
    /// from anywhere): row limits per table, no dangling references, every
    /// row decodable within field bounds, ordered ranges, valid ids, relation
    /// endpoints that satisfy the vocabulary, at most one project entity.
    /// Refused databases are left unchanged for recovery.
    pub fn verify_contents(&self) -> StoreResult<()> {
        for (table, limit) in &ROW_LIMITS {
            let n: i64 = self.conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            if n > *limit {
                return Err(StoreError::Untrusted(format!("{table} holds {n} rows (at most {limit} are read)")));
            }
        }
        {
            let mut stmt = self.conn.prepare("PRAGMA foreign_key_check")?;
            let mut rows = stmt.query([])?;
            if let Some(row) = rows.next()? {
                let table: String = row.get(0).unwrap_or_default();
                return Err(StoreError::Corrupt(format!(
                    "a record in {} refers to an entity that does not exist",
                    truncate(&table, 40)
                )));
            }
        }
        for (key, value) in self.meta_rows()? {
            if key.len() > MAX_SHORT_FIELD || value.len() > 4096 {
                return Err(untrusted("metadata"));
            }
        }
        if let Some(rev) = self.meta("revision")?
            && rev.parse::<u64>().is_err()
        {
            return Err(untrusted("graph revision"));
        }
        let mut kinds: std::collections::HashMap<EntityId, EntityKind> = std::collections::HashMap::new();
        let mut projects = 0usize;
        {
            let mut stmt = self.conn.prepare(&format!("SELECT {ENTITY_COLUMNS} FROM entities"))?;
            for row in stmt.query_map([], read_entity)? {
                let e = row??;
                if e.kind == EntityKind::Project {
                    projects += 1;
                }
                if e.kind != EntityKind::Resource && !e.roles.is_empty() {
                    return Err(untrusted("entity roles"));
                }
                kinds.insert(e.id, e.kind);
            }
        }
        if projects > 1 {
            return Err(StoreError::Untrusted("more than one project entity".into()));
        }
        {
            let mut stmt = self.conn.prepare(&format!("SELECT {BINDING_COLUMNS} FROM bindings"))?;
            for row in stmt.query_map([], read_binding)? {
                row??;
            }
        }
        {
            let mut stmt = self.conn.prepare(&format!("SELECT {RELATION_COLUMNS} FROM relations"))?;
            for row in stmt.query_map([], read_relation)? {
                let r = row??;
                let (Some(from), Some(to)) = (kinds.get(&r.from), kinds.get(&r.to)) else {
                    return Err(StoreError::Corrupt("a relation refers to an entity that does not exist".into()));
                };
                if !r.kind.allows(*from, *to) || r.from == r.to {
                    return Err(StoreError::Untrusted(format!(
                        "a {} relation between a {} and a {} breaks the vocabulary's endpoint rules",
                        r.kind.as_str(),
                        from.as_str(),
                        to.as_str()
                    )));
                }
                if r.evidence_kind.is_some() != (r.kind == RelationKind::EvidenceFor) {
                    return Err(untrusted("relation evidence kind"));
                }
            }
        }
        {
            let mut stmt = self.conn.prepare("SELECT id, kind, subject, body FROM evidence")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (id, kind, subject, body) = row?;
                let e: Evidence = decode(&body)?;
                if e.id.as_str() != id
                    || EvidenceKind::parse(&kind) != Some(e.kind)
                    || subject.as_deref() != e.subject.as_ref().map(EntityId::as_str)
                {
                    return Err(untrusted("evidence record"));
                }
            }
        }
        {
            let mut stmt = self.conn.prepare(&format!("SELECT {COVERAGE_COLUMNS} FROM coverage"))?;
            for row in stmt.query_map([], read_coverage)? {
                row??;
            }
        }
        {
            let mut stmt = self.conn.prepare("SELECT body FROM views")?;
            for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
                decode::<GraphView>(&row?)?;
            }
        }
        {
            let mac_bad: i64 = self.conn.query_row(
                "SELECT count(*) FROM evidence_seals WHERE length(mac) > ?1 OR length(id) > ?1",
                [MAX_SHORT_FIELD as i64],
                |r| r.get(0),
            )?;
            if mac_bad > 0 {
                return Err(untrusted("evidence seal"));
            }
            let mut stmt = self.conn.prepare("SELECT path, content_hash, extractor, length(body) FROM source_index")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
            })?;
            for row in rows {
                let (path, hash, extractor, len) = row?;
                WorkspacePath::parse(&path).map_err(|_| untrusted("index path"))?;
                if hash.len() > MAX_SHORT_FIELD || extractor.len() > MAX_SHORT_FIELD || len > 16 * MAX_FIELD as i64 {
                    return Err(untrusted("source index record"));
                }
            }
        }
        {
            let n: i64 = self.conn.query_row(
                "SELECT count(*) FROM workflows WHERE length(body) > ?1 OR length(hash) > ?2 OR length(state) > ?2",
                params![16 * MAX_FIELD as i64, MAX_SHORT_FIELD as i64],
                |r| r.get(0),
            )?;
            if n > 0 {
                return Err(untrusted("workflow record"));
            }
        }
        {
            // Recoverable snapshots are decoded strictly when used; here only
            // their bounds and kinds are checked.
            let n: i64 = self.conn.query_row(
                "SELECT count(*) FROM recoverable WHERE length(id) > ?2 OR length(label) > ?2
                 OR length(body) > ?1 OR kind NOT IN ('entity', 'relation', 'view', 'workflow')",
                params![16 * MAX_FIELD as i64, MAX_SHORT_FIELD as i64],
                |r| r.get(0),
            )?;
            let m: i64 = self.conn.query_row(
                "SELECT count(*) FROM retired_workflows WHERE length(id) > ?1",
                [MAX_SHORT_FIELD as i64],
                |r| r.get(0),
            )?;
            if n > 0 || m > 0 {
                return Err(untrusted("recoverable record"));
            }
        }
        Ok(())
    }

    fn meta_rows(&self) -> StoreResult<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM meta LIMIT 1001")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Write a consistent copy of the whole database to `dest` (`VACUUM
    /// INTO`; correct even while the database is open, never a raw file copy).
    pub fn vacuum_into(&self, dest: &Path) -> StoreResult<()> {
        let dest = dest.to_str().ok_or_else(|| StoreError::Corrupt("backup path is not valid text".into()))?;
        self.conn.execute("VACUUM INTO ?1", [dest])?;
        Ok(())
    }

    fn migrate(&mut self, from: u32) -> StoreResult<()> {
        for (version, sql) in MIGRATIONS {
            if *version > from {
                let tx = self.conn.transaction()?;
                tx.execute_batch(sql)?;
                tx.pragma_update(None, "user_version", version)?;
                tx.commit()?;
            }
        }
        Ok(())
    }

    fn load_or_create_project(&mut self) -> StoreResult<ProjectId> {
        if let Some(raw) = self.meta("project_id")? {
            let id = ProjectId::parse(&raw).map_err(|_| StoreError::Untrusted("invalid project identifier".into()))?;
            if let Some(v) = self.meta("vocabulary_version")? {
                let v: u32 = v.parse().map_err(|_| StoreError::Untrusted("invalid vocabulary version".into()))?;
                if v > VOCABULARY_VERSION {
                    return Err(StoreError::NewerSchema { found: v, supported: VOCABULARY_VERSION });
                }
            }
            return Ok(id);
        }
        let id = ProjectId::mint();
        let tx = self.conn.transaction()?;
        set_meta(&tx, "project_id", id.as_str())?;
        set_meta(&tx, "created_at", &now_ms().to_string())?;
        set_meta(&tx, "revision", "0")?;
        set_meta(&tx, "vocabulary_version", &VOCABULARY_VERSION.to_string())?;
        tx.commit()?;
        Ok(id)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn project(&self) -> &ProjectId {
        &self.project
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Start a write transaction.
    pub fn transaction(&mut self) -> StoreResult<Transaction<'_>> {
        Ok(self.conn.transaction()?)
    }

    /// Start a write transaction with the write helpers. Its commit enforces
    /// the row limits from this store's row budget instead of counting every
    /// table (see [`Writer::commit`]).
    pub fn writer(&mut self) -> StoreResult<Writer<'_>> {
        let tx = self.conn.transaction()?;
        Ok(Writer { tx, budget: Some(&self.budget) })
    }

    /// The current write stamp: it changes whenever a row of the database
    /// changed (through this connection or, once committed, any other).
    fn write_stamp(&self) -> StoreResult<WriteStamp> {
        let mut stmt = self.conn.prepare_cached("PRAGMA data_version")?;
        let version: i64 = stmt.query_row([], |r| r.get(0))?;
        Ok((self.conn.total_changes(), version))
    }

    /// Entity and relation counts and the coverage summary. They are
    /// aggregates over whole tables, read by every status request, so they
    /// are computed once per write stamp: a status read after nothing was
    /// written costs no scan.
    ///
    /// Cache owner: this store; key: the write stamp; one entry, replaced on
    /// the first read after any write and dropped with the store.
    pub fn status_summary(&self) -> StoreResult<((u32, u32), CoverageSummary)> {
        let stamp = self.write_stamp()?;
        if let Some((at, counts, coverage)) = self.summary.borrow().as_ref()
            && *at == stamp
        {
            return Ok((*counts, coverage.clone()));
        }
        let counts = self.count_rows()?;
        let coverage = self.summarize_coverage()?;
        *self.summary.borrow_mut() = Some((stamp, counts, coverage.clone()));
        Ok((counts, coverage))
    }

    pub fn meta(&self, key: &str) -> StoreResult<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    /// Current project graph revision.
    pub fn revision(&self) -> StoreResult<u64> {
        Ok(self.meta("revision")?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn counts(&self) -> StoreResult<(u32, u32)> {
        Ok(self.status_summary()?.0)
    }

    pub(crate) fn count_rows(&self) -> StoreResult<(u32, u32)> {
        let entities: i64 =
            self.conn.query_row("SELECT count(*) FROM entities WHERE lifecycle != 'tombstoned'", [], |r| r.get(0))?;
        let relations: i64 = self.conn.query_row("SELECT count(*) FROM relations", [], |r| r.get(0))?;
        Ok((entities as u32, relations as u32))
    }

    // ----- entities -----

    pub fn entity(&self, id: &EntityId) -> StoreResult<Option<Entity>> {
        let mut stmt = self.conn.prepare_cached(&format!("SELECT {ENTITY_COLUMNS} FROM entities WHERE id = ?1"))?;
        stmt.query_row([id.as_str()], read_entity).optional()?.transpose()
    }

    /// Entities matching kinds/text (bounded by `limit`); returns the page and the total.
    pub fn find_entities(
        &self,
        kinds: Option<&[EntityKind]>,
        text: Option<&str>,
        include_inactive: bool,
        limit: usize,
    ) -> StoreResult<(Vec<Entity>, usize)> {
        let mut sql = format!("SELECT {ENTITY_COLUMNS} FROM entities WHERE 1=1");
        let mut args: Vec<String> = Vec::new();
        if !include_inactive {
            sql.push_str(" AND lifecycle = 'active'");
        }
        if let Some(kinds) = kinds {
            if kinds.is_empty() {
                return Ok((Vec::new(), 0));
            }
            let marks: Vec<String> = kinds
                .iter()
                .map(|k| {
                    args.push(k.as_str().to_owned());
                    format!("?{}", args.len())
                })
                .collect();
            sql.push_str(&format!(" AND kind IN ({})", marks.join(",")));
        }
        if let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) {
            args.push(format!("%{}%", escape_like(&text.to_lowercase())));
            let n = args.len();
            sql.push_str(&format!(
                " AND (lower(label) LIKE ?{n} ESCAPE '\\' OR lower(coalesce(key,'')) LIKE ?{n} ESCAPE '\\')"
            ));
        }
        sql.push_str(" ORDER BY kind, label, id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), read_entity)?;
        let mut out = Vec::new();
        let mut total = 0usize;
        for row in rows {
            total += 1;
            if total > MAX_ROWS {
                break;
            }
            if out.len() < limit {
                out.push(row??);
            }
        }
        Ok((out, total))
    }

    /// Entities by natural key (e.g. a resource path), any lifecycle.
    pub fn entities_by_key(&self, kind: EntityKind, key: &str) -> StoreResult<Vec<Entity>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {ENTITY_COLUMNS} FROM entities WHERE kind = ?1 AND key = ?2 LIMIT 64"))?;
        let rows = stmt.query_map(params![kind.as_str(), key], read_entity)?;
        rows.map(|r| r?).collect()
    }

    /// Non-tombstoned entities of any kind with this natural key: at most
    /// [`KEY_READ_LIMIT`]. A result of that length may be incomplete; a
    /// caller that counts or decides uses [`Self::count_live_entities_with_key`]
    /// and [`Self::live_entities_with_key_after`].
    pub fn live_entities_with_key(&self, key: &str) -> StoreResult<Vec<Entity>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {ENTITY_COLUMNS} FROM entities WHERE key = ?1 AND lifecycle != 'tombstoned' LIMIT {KEY_READ_LIMIT}"
        ))?;
        let rows = stmt.query_map([key], read_entity)?;
        rows.map(|r| r?).collect()
    }

    /// How many non-tombstoned entities of each kind have this natural key
    /// (complete: counted, not read).
    pub fn count_live_entities_with_key(&self, key: &str) -> StoreResult<Vec<(EntityKind, usize)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT kind, count(*) FROM entities WHERE key = ?1 AND lifecycle != 'tombstoned' GROUP BY kind",
        )?;
        let rows = stmt.query_map([key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (kind, n) = row?;
            let kind = EntityKind::parse(&kind).ok_or_else(|| untrusted("entity kind"))?;
            out.push((kind, n.max(0) as usize));
        }
        Ok(out)
    }

    /// One page (at most [`KEY_READ_LIMIT`]) of the non-tombstoned entities
    /// with this natural key, in id order, after `after`.
    pub fn live_entities_with_key_after(&self, key: &str, after: Option<&EntityId>) -> StoreResult<Vec<Entity>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {ENTITY_COLUMNS} FROM entities WHERE key = ?1 AND lifecycle != 'tombstoned' AND id > ?2
             ORDER BY id LIMIT {KEY_READ_LIMIT}"
        ))?;
        let rows = stmt.query_map(params![key, after.map(|a| a.as_str()).unwrap_or("")], read_entity)?;
        rows.map(|r| r?).collect()
    }

    /// Files waiting for link targets, with the targets they wait for.
    pub fn index_wanted(&self) -> StoreResult<Vec<(WorkspacePath, Vec<String>)>> {
        let mut stmt =
            self.conn.prepare_cached("SELECT path, wanted FROM source_index WHERE wanted != '[]' LIMIT 500000")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (path, wanted) = row?;
            let path = WorkspacePath::parse(&path).map_err(|_| untrusted("index path"))?;
            out.push((path, decode::<Vec<String>>(&wanted)?));
        }
        Ok(out)
    }

    /// Every stored source index body's labels (path, body), for building the
    /// in-memory project label index once per session.
    pub fn all_source_index(&self) -> StoreResult<Vec<IndexRecord>> {
        let mut stmt =
            self.conn.prepare_cached("SELECT path, content_hash, extractor, body FROM source_index LIMIT 500000")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (path, content_hash, extractor, body) = row?;
            if body.len() > 16 * MAX_FIELD {
                return Err(untrusted("source index record"));
            }
            out.push(IndexRecord {
                path: WorkspacePath::parse(&path).map_err(|_| untrusted("index path"))?,
                content_hash: short(content_hash)?,
                extractor: short(extractor)?,
                body,
            });
        }
        Ok(out)
    }

    /// The entities with these ids that exist (every id is looked up: the
    /// caller bounds the list).
    pub fn entities(&self, ids: &[EntityId]) -> StoreResult<Vec<Entity>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(e) = self.entity(id)? {
                out.push(e);
            }
        }
        Ok(out)
    }

    // ----- bindings -----

    pub fn bindings(&self, entity: &EntityId) -> StoreResult<Vec<SourceBinding>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {BINDING_COLUMNS} FROM bindings WHERE entity_id = ?1 ORDER BY ordinal LIMIT 64"
        ))?;
        let rows = stmt.query_map([entity.as_str()], read_binding)?;
        rows.map(|r| r?).collect()
    }

    /// Every binding in `path`. Complete (bounded by the table's row limit):
    /// what is bound in a file decides impact, removal and reconciliation.
    pub fn bindings_at_path(&self, path: &WorkspacePath) -> StoreResult<Vec<SourceBinding>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {BINDING_COLUMNS} FROM bindings WHERE path = ?1 ORDER BY entity_id, ordinal LIMIT 1000000"
        ))?;
        let rows = stmt.query_map([path.as_str()], read_binding)?;
        rows.map(|r| r?).collect()
    }

    // ----- relations -----

    /// Fill in the computed review standing of an accepted `formalizes`
    /// link (see [`crate::review`]). Every relation this store hands out
    /// goes through here, so no reader can present a link as reviewed
    /// without the comparison.
    fn qualified(&self, mut r: Relation) -> StoreResult<Relation> {
        r.review = None;
        if r.kind == RelationKind::Formalizes && r.status == AssertionStatus::Accepted {
            let entity = self.entity(&r.to)?;
            let bindings = self.bindings(&r.to)?;
            r.review = Some(crate::review::link_review(r.reviewed_statement.as_deref(), entity.as_ref(), &bindings));
        }
        Ok(r)
    }

    fn qualified_all(&self, relations: Vec<Relation>) -> StoreResult<Vec<Relation>> {
        relations.into_iter().map(|r| self.qualified(r)).collect()
    }

    pub fn relation(&self, id: &RelationId) -> StoreResult<Option<Relation>> {
        let found = {
            let mut stmt =
                self.conn.prepare_cached(&format!("SELECT {RELATION_COLUMNS} FROM relations WHERE id = ?1"))?;
            stmt.query_row([id.as_str()], read_relation).optional()?.transpose()?
        };
        found.map(|r| self.qualified(r)).transpose()
    }

    /// Every relation from `entity`, ordered by kind then id. Complete: an
    /// entity's relations are what impact, validation and reconciliation
    /// decide on, so nothing is left out (the bound is the table's row limit).
    pub fn relations_from(&self, entity: &EntityId) -> StoreResult<Vec<Relation>> {
        Ok(self.relations_at(Endpoint::From, entity, RELATION_ROWS)?.0)
    }

    /// Every relation to `entity`, ordered by kind then id. Complete (see
    /// [`Self::relations_from`]).
    pub fn relations_to(&self, entity: &EntityId) -> StoreResult<Vec<Relation>> {
        Ok(self.relations_at(Endpoint::To, entity, RELATION_ROWS)?.0)
    }

    /// At most `limit` relations from `entity` (kind, id order) for a bounded
    /// presentation, and whether more exist. A reader that decides anything
    /// from the relations uses [`Self::relations_from`].
    pub fn relations_from_bounded(&self, entity: &EntityId, limit: usize) -> StoreResult<(Vec<Relation>, bool)> {
        self.relations_at(Endpoint::From, entity, limit)
    }

    /// At most `limit` relations to `entity`; see [`Self::relations_from_bounded`].
    pub fn relations_to_bounded(&self, entity: &EntityId, limit: usize) -> StoreResult<(Vec<Relation>, bool)> {
        self.relations_at(Endpoint::To, entity, limit)
    }

    fn relations_at(&self, end: Endpoint, entity: &EntityId, limit: usize) -> StoreResult<(Vec<Relation>, bool)> {
        let mut relations: Vec<Relation> = Vec::new();
        let mut more = false;
        {
            let mut stmt = self.conn.prepare_cached(end.select())?;
            let mut rows = stmt.query(params![entity.as_str(), limit.min(RELATION_ROWS) as i64 + 1])?;
            while let Some(row) = rows.next()? {
                if relations.len() >= limit {
                    more = true;
                    break;
                }
                relations.push(read_relation(row)??);
            }
        }
        Ok((self.qualified_all(relations)?, more))
    }

    /// Visit every relation at one end of `entity` (kind, id order) without
    /// holding them all; `visit` returns `false` to stop early. For walks
    /// that have their own work bound and report reaching it.
    pub fn visit_relations(
        &self,
        end: Endpoint,
        entity: &EntityId,
        mut visit: impl FnMut(Relation) -> StoreResult<bool>,
    ) -> StoreResult<()> {
        // Not from the statement cache: a visitor may read this entity's
        // relations again while the walk is open.
        let mut stmt = self.conn.prepare(end.select())?;
        let mut rows = stmt.query(params![entity.as_str(), RELATION_ROWS as i64])?;
        while let Some(row) = rows.next()? {
            let relation = self.qualified(read_relation(row)??)?;
            if !visit(relation)? {
                break;
            }
        }
        Ok(())
    }

    /// Every relation of one kind (bounded), e.g. the whole containment tree.
    pub fn relations_of_kind(&self, kind: RelationKind) -> StoreResult<Vec<Relation>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {RELATION_COLUMNS} FROM relations WHERE kind = ?1 LIMIT 1000000"))?;
        let rows = stmt.query_map([kind.as_str()], read_relation)?;
        let relations: Vec<Relation> = rows.map(|r| r?).collect::<StoreResult<_>>()?;
        drop(stmt);
        self.qualified_all(relations)
    }

    /// Relations whose both endpoints are in `ids` (ordered by source, kind,
    /// id), at most [`MAX_ROWS`]. A caller that must know whether that left
    /// relations out uses [`Self::relations_among_bounded`].
    pub fn relations_among(&self, ids: &BTreeSet<EntityId>) -> StoreResult<Vec<Relation>> {
        Ok(self.relations_among_bounded(ids, MAX_ROWS)?.0)
    }

    /// At most `limit` relations whose both endpoints are in `ids` (ordered
    /// by source, kind, id), and whether more exist.
    pub fn relations_among_bounded(
        &self,
        ids: &BTreeSet<EntityId>,
        limit: usize,
    ) -> StoreResult<(Vec<Relation>, bool)> {
        let mut out = Vec::new();
        let mut more = false;
        self.visit_relations_among(ids, |r| {
            if out.len() >= limit {
                more = true;
                return Ok(false);
            }
            out.push(r);
            Ok(true)
        })?;
        Ok((out, more))
    }

    /// Visit every relation whose both endpoints are in `ids` (ordered by
    /// source, kind, id); `visit` returns `false` to stop early. The
    /// endpoint filter runs in the database: relations that leave the set
    /// (a hub's other links) are neither decoded nor qualified.
    pub fn visit_relations_among(
        &self,
        ids: &BTreeSet<EntityId>,
        mut visit: impl FnMut(Relation) -> StoreResult<bool>,
    ) -> StoreResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let list = serde_json::to_string(&ids.iter().map(EntityId::as_str).collect::<Vec<_>>())?;
        let columns: Vec<String> = RELATION_COLUMNS.split(", ").map(|c| format!("r.{c}")).collect();
        // The id list drives the join (CROSS JOIN fixes the order): one index
        // seek per listed entity, then a membership probe per outgoing row.
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {} FROM json_each(?1) AS f CROSS JOIN relations AS r
             WHERE r.from_id = f.value AND r.to_id IN (SELECT value FROM json_each(?1))
             ORDER BY r.from_id, r.kind, r.id",
            columns.join(", ")
        ))?;
        let mut rows = stmt.query([list])?;
        while let Some(row) = rows.next()? {
            let relation = self.qualified(read_relation(row)??)?;
            if !visit(relation)? {
                break;
            }
        }
        Ok(())
    }

    /// An existing relation of this kind between these endpoints (not rejected).
    pub fn find_relation(&self, kind: RelationKind, from: &EntityId, to: &EntityId) -> StoreResult<Option<Relation>> {
        let found = {
            let mut stmt = self.conn.prepare_cached(&find_relation_sql(kind))?;
            stmt.query_row(params![kind.as_str(), from.as_str(), to.as_str()], read_relation).optional()?.transpose()?
        };
        found.map(|r| self.qualified(r)).transpose()
    }

    // ----- evidence -----

    pub fn evidence(&self, id: &EvidenceId) -> StoreResult<Option<Evidence>> {
        let body: Option<String> =
            self.conn.query_row("SELECT body FROM evidence WHERE id = ?1", [id.as_str()], |r| r.get(0)).optional()?;
        body.map(|b| decode::<Evidence>(&b)).transpose()
    }

    /// Whether this installation recorded the evidence: its stored MAC
    /// matches the app-local key. Evidence without one (copied in with the
    /// project's graph, or written by anything else) is imported: a
    /// historical claim of its recorded origin.
    pub fn evidence_is_local(&self, id: &EvidenceId) -> bool {
        let Some(key) = &self.evidence_key else { return false };
        let row: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT e.body, s.mac FROM evidence e LEFT JOIN evidence_seals s ON s.id = e.id WHERE e.id = ?1",
                [id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        match row {
            Some((body, Some(mac))) => crate::mac::verify_evidence_mac(key, id.as_str(), &body, &mac),
            _ => false,
        }
    }

    /// The app-local key that seals evidence recorded by this installation.
    pub fn set_evidence_key(&mut self, key: [u8; 32]) {
        self.evidence_key = Some(key);
    }

    pub fn evidence_for_subject(&self, subject: &EntityId) -> StoreResult<Vec<Evidence>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT body FROM evidence WHERE subject = ?1 ORDER BY recorded_at DESC, id LIMIT 200")?;
        let rows = stmt.query_map([subject.as_str()], |r| r.get::<_, String>(0))?;
        rows.map(|r| decode::<Evidence>(&r?)).collect()
    }

    /// The newest record of `kind` about `subject` (latest first, ties by
    /// id, as [`Self::evidence_for_subject`] orders them), wherever it falls
    /// among the subject's records.
    pub fn latest_evidence_of_kind(&self, subject: &EntityId, kind: EvidenceKind) -> StoreResult<Option<Evidence>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT body FROM evidence WHERE subject = ?1 AND kind = ?2 ORDER BY recorded_at DESC, id LIMIT 1",
        )?;
        let body: Option<String> = stmt.query_row(params![subject.as_str(), kind.as_str()], |r| r.get(0)).optional()?;
        body.map(|b| decode::<Evidence>(&b)).transpose()
    }

    /// At most `limit` of the newest records about `subject`, and whether
    /// older ones exist.
    pub fn evidence_for_subject_bounded(&self, subject: &EntityId, limit: usize) -> StoreResult<(Vec<Evidence>, bool)> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT body FROM evidence WHERE subject = ?1 ORDER BY recorded_at DESC, id LIMIT ?2")?;
        let rows =
            stmt.query_map(params![subject.as_str(), limit.saturating_add(1) as i64], |r| r.get::<_, String>(0))?;
        let mut out: Vec<Evidence> = rows.map(|r| decode::<Evidence>(&r?)).collect::<StoreResult<_>>()?;
        let more = out.len() > limit;
        out.truncate(limit);
        Ok((out, more))
    }

    // ----- coverage -----

    pub fn coverage(&self, path: &WorkspacePath) -> StoreResult<Option<ResourceCoverage>> {
        let mut stmt = self.conn.prepare_cached(&format!("SELECT {COVERAGE_COLUMNS} FROM coverage WHERE path = ?1"))?;
        stmt.query_row([path.as_str()], read_coverage).optional()?.transpose()
    }

    /// Coverage of every indexed file, by path. Complete (bounded by the
    /// table's row limit): a summary computed from it counts every file.
    pub fn all_coverage(&self) -> StoreResult<Vec<ResourceCoverage>> {
        let mut stmt =
            self.conn.prepare_cached(&format!("SELECT {COVERAGE_COLUMNS} FROM coverage ORDER BY path LIMIT 500000"))?;
        let rows = stmt.query_map([], read_coverage)?;
        rows.map(|r| r?).collect()
    }

    /// Reconciliation checkpoints of every indexed file (bounded by the
    /// coverage row limit).
    pub fn checkpoints(&self) -> StoreResult<BTreeMap<WorkspacePath, Checkpoint>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COVERAGE_COLUMNS}, size, modified_ms, file_identity FROM coverage LIMIT 500000"
        ))?;
        let rows = stmt.query_map([], |r| {
            let coverage = read_coverage_stored(r)?;
            let size: Option<i64> = r.get(7)?;
            let modified: Option<i64> = r.get(8)?;
            let identity: Option<String> = r.get(9)?;
            Ok((coverage, size, modified, identity))
        })?;
        let mut out = BTreeMap::new();
        for row in rows {
            let (coverage, size, modified, identity) = row?;
            let coverage = coverage?;
            out.insert(
                coverage.path.clone(),
                Checkpoint {
                    size: size.map(|s| s.max(0) as u64),
                    modified_ms: modified.map(|m| m.max(0) as u64),
                    identity: identity.map(short).transpose()?,
                    coverage,
                },
            );
        }
        Ok(out)
    }

    /// Checkpoints of the files at or under `prefixes`.
    pub fn checkpoints_under(&self, prefixes: &[WorkspacePath]) -> StoreResult<BTreeMap<WorkspacePath, Checkpoint>> {
        let mut out = BTreeMap::new();
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COVERAGE_COLUMNS}, size, modified_ms, file_identity FROM coverage
             WHERE path >= ?1 AND path < ?3 AND (path = ?1 OR path >= ?2) LIMIT 500000"
        ))?;
        for prefix in prefixes {
            let (low, high) = under(prefix);
            let rows = stmt.query_map(params![prefix.as_str(), low, high], |r| {
                let coverage = read_coverage_stored(r)?;
                Ok((coverage, r.get::<_, Option<i64>>(7)?, r.get::<_, Option<i64>>(8)?, r.get::<_, Option<String>>(9)?))
            })?;
            for row in rows {
                let (coverage, size, modified, identity) = row?;
                let coverage = coverage?;
                out.insert(
                    coverage.path.clone(),
                    Checkpoint {
                        size: size.map(|s| s.max(0) as u64),
                        modified_ms: modified.map(|m| m.max(0) as u64),
                        identity: identity.map(short).transpose()?,
                        coverage,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Resource identities by path, without decoding whole records:
    /// `(path, id, roles, tombstoned, created_at_ms)`. With `prefixes`, only
    /// resources at or under them.
    pub fn resource_keys(&self, prefixes: Option<&[WorkspacePath]>) -> StoreResult<Vec<ResourceKey>> {
        let mut out = Vec::new();
        // Prefixes may overlap (a folder and a file beneath it, nested
        // folders, one path named twice): each resource is returned once.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut read = |rows: rusqlite::MappedRows<'_, _>| -> StoreResult<()> {
            for row in rows {
                let (id, key, roles, lifecycle, created): (String, Option<String>, String, String, i64) = row?;
                let Some(path) = key.as_deref().and_then(|k| WorkspacePath::parse(k).ok()) else { continue };
                if !seen.insert(id.clone()) {
                    continue;
                }
                out.push(ResourceKey {
                    path,
                    id: EntityId::parse(&id).map_err(|_| untrusted("entity id"))?,
                    roles: decode::<Vec<ResourceRole>>(&roles)?,
                    tombstoned: Lifecycle::parse(&lifecycle).ok_or_else(|| untrusted("lifecycle"))?
                        == Lifecycle::Tombstoned,
                    created_at_ms: created.max(0) as u64,
                });
            }
            Ok(())
        };
        let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(String, Option<String>, String, String, i64)> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        };
        match prefixes {
            None => {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT id, key, roles, lifecycle, created_at FROM entities WHERE kind = 'resource' LIMIT 500000",
                )?;
                read(stmt.query_map([], map)?)?;
            }
            Some(prefixes) => {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT id, key, roles, lifecycle, created_at FROM entities
                     WHERE key >= ?1 AND key < ?3 AND (key = ?1 OR key >= ?2) AND +kind = 'resource' LIMIT 500000",
                )?;
                for prefix in prefixes {
                    let (low, high) = under(prefix);
                    read(stmt.query_map(params![prefix.as_str(), low, high], map)?)?;
                }
            }
        }
        Ok(out)
    }

    /// The live resource of `path`: the oldest non-tombstoned identity with
    /// that key (the one a full pass keeps as the file's resource).
    pub fn live_resource_id(&self, path: &WorkspacePath) -> StoreResult<Option<EntityId>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM entities WHERE kind = 'resource' AND key = ?1 AND lifecycle != 'tombstoned'
             ORDER BY created_at, id LIMIT 1",
        )?;
        let id: Option<String> = stmt.query_row([path.as_str()], |r| r.get(0)).optional()?;
        id.map(|i| EntityId::parse(&i).map_err(|_| untrusted("entity id"))).transpose()
    }

    /// Paths of the live resources whose file name is one of `names`
    /// (e.g. the Lake package files), without decoding whole records.
    pub fn live_resource_paths_named(&self, names: &[&str]) -> StoreResult<Vec<WorkspacePath>> {
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare_cached(
            "SELECT key FROM entities WHERE kind = 'resource' AND lifecycle != 'tombstoned'
             AND (key = ?1 OR key LIKE ?2 ESCAPE '\\') LIMIT 500000",
        )?;
        for name in names {
            let like = format!("%/{}", escape_like(name));
            let rows = stmt.query_map(params![name, like], |r| r.get::<_, Option<String>>(0))?;
            for key in rows {
                // The pattern ignores ASCII case; the file name must match exactly.
                if let Some(path) = key?.as_deref().and_then(|k| WorkspacePath::parse(k).ok())
                    && path.file_name() == *name
                {
                    out.push(path);
                }
            }
        }
        Ok(out)
    }

    /// The stored source index of a file (rebuildable projection).
    pub fn source_index(&self, path: &WorkspacePath) -> StoreResult<Option<IndexRecord>> {
        let mut stmt =
            self.conn.prepare_cached("SELECT content_hash, extractor, body FROM source_index WHERE path = ?1")?;
        let row = stmt
            .query_row([path.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })
            .optional()?;
        let Some((content_hash, extractor, body)) = row else { return Ok(None) };
        if body.len() > 16 * MAX_FIELD {
            return Err(untrusted("source index record"));
        }
        Ok(Some(IndexRecord {
            path: path.clone(),
            content_hash: short(content_hash)?,
            extractor: short(extractor)?,
            body,
        }))
    }

    /// Every resource entity (any lifecycle), bounded.
    pub fn all_of_kind(&self, kind: EntityKind) -> StoreResult<Vec<Entity>> {
        let mut stmt =
            self.conn.prepare_cached(&format!("SELECT {ENTITY_COLUMNS} FROM entities WHERE kind = ?1 LIMIT 500000"))?;
        let rows = stmt.query_map([kind.as_str()], read_entity)?;
        rows.map(|r| r?).collect()
    }

    /// Every binding (bounded), for reconciliation.
    pub fn all_bindings(&self) -> StoreResult<Vec<SourceBinding>> {
        let mut stmt = self.conn.prepare_cached(&format!("SELECT {BINDING_COLUMNS} FROM bindings LIMIT 1000000"))?;
        let rows = stmt.query_map([], read_binding)?;
        rows.map(|r| r?).collect()
    }

    /// Coverage as last reconciled, ignoring a pending change (the
    /// reconciler's own view; others use [`Self::coverage`]).
    pub fn coverage_stored(&self, path: &WorkspacePath) -> StoreResult<Option<ResourceCoverage>> {
        let mut stmt = self.conn.prepare_cached(&format!("SELECT {COVERAGE_COLUMNS} FROM coverage WHERE path = ?1"))?;
        stmt.query_row([path.as_str()], read_coverage_stored).optional()?.transpose()
    }

    pub fn coverage_summary(&self) -> StoreResult<CoverageSummary> {
        Ok(self.status_summary()?.1)
    }

    pub(crate) fn summarize_coverage(&self) -> StoreResult<CoverageSummary> {
        let mut summary = CoverageSummary::default();
        let mut stmt = self.conn.prepare_cached(
            "SELECT CASE WHEN pending > 0 THEN 'pending' ELSE state END AS s, count(*), sum(unresolved)
             FROM coverage GROUP BY s",
        )?;
        let rows =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?)))?;
        for row in rows {
            let (state, count, unresolved) = row?;
            let count = count.max(0) as u32;
            summary.resources += count;
            summary.unresolved_bindings += unresolved.unwrap_or(0).max(0) as u32;
            match CoverageState::parse(&state) {
                Some(CoverageState::Complete) => summary.complete += count,
                Some(CoverageState::Partial) => summary.partial += count,
                Some(CoverageState::Unsupported) => summary.unsupported += count,
                Some(CoverageState::Pending) => summary.pending += count,
                Some(CoverageState::Failed) | None => summary.failed += count,
            }
        }
        Ok(summary)
    }

    // ----- views -----

    pub fn views(&self) -> StoreResult<Vec<GraphViewSummary>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, name, scope, revision, updated_at FROM views ORDER BY name, id LIMIT 500")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, scope, revision, updated) = row?;
            out.push(GraphViewSummary {
                id: ViewId::parse(&id).map_err(|_| StoreError::Untrusted("invalid view id".into()))?,
                name: bounded(name)?,
                scope: decode::<ViewScope>(&scope)?,
                revision: revision.max(0) as u64,
                updated_at_ms: updated.max(0) as u64,
            });
        }
        Ok(out)
    }

    pub fn view(&self, id: &ViewId) -> StoreResult<Option<GraphView>> {
        let body: Option<String> =
            self.conn.query_row("SELECT body FROM views WHERE id = ?1", [id.as_str()], |r| r.get(0)).optional()?;
        body.map(|b| decode::<GraphView>(&b)).transpose()
    }

    pub fn put_view(&mut self, view: &GraphView) -> StoreResult<()> {
        let body = serde_json::to_string(view)?;
        check_record(&body, "the view")?;
        check_field(Some(&view.name), MAX_FIELD, "the view name")?;
        let exists: bool = self
            .conn
            .query_row("SELECT 1 FROM views WHERE id = ?1", [view.id.as_str()], |_| Ok(()))
            .optional()?
            .is_some();
        if !exists {
            let n: i64 = self.conn.query_row("SELECT count(*) FROM views", [], |r| r.get(0))?;
            if n >= row_limit("views") {
                return Err(limit(format!(
                    "the project holds the most saved views a graph can keep ({n}); delete one first"
                )));
            }
        }
        self.conn.execute(
            "INSERT INTO views (id, name, scope, body, revision, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, scope = excluded.scope, body = excluded.body,
             revision = excluded.revision, updated_at = excluded.updated_at",
            params![
                view.id.as_str(),
                view.name,
                serde_json::to_string(&view.scope)?,
                body,
                view.revision as i64,
                view.updated_at_ms as i64
            ],
        )?;
        Ok(())
    }
}

// ----- writes inside a transaction -----

/// Write helpers used inside one transaction. Each graph change bumps the
/// project revision exactly once (`record_change`).
pub struct Writer<'a> {
    pub tx: Transaction<'a>,
    /// The store's row budget ([`GraphStore::writer`]); without one the
    /// commit counts every table.
    budget: Option<&'a RowBudget>,
}

impl<'a> Writer<'a> {
    pub fn new(tx: Transaction<'a>) -> Self {
        Self { tx, budget: None }
    }

    /// Commit, unless the transaction would leave a table beyond the row
    /// limit the validated open enforces (then nothing is written).
    ///
    /// With the store's row budget ([`GraphStore::writer`]) a table is
    /// counted only when the rows changed since its last count could have
    /// filled it: every row this connection changed since then (whatever
    /// statement changed it, in this transaction or outside the helpers) is
    /// charged against every table's headroom, so the estimate never
    /// exceeds the truth and the refusal is exactly as strict as counting
    /// every table.
    pub fn commit(self) -> StoreResult<()> {
        let count = |table: &str, max: i64| -> StoreResult<i64> {
            let n: i64 = self.tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            if n > max {
                return Err(limit(format!(
                    "the change would leave {n} rows in {table} (a project graph holds at most {max}); nothing was written"
                )));
            }
            Ok(n)
        };
        let Some(budget) = self.budget else {
            for (table, max) in &ROW_LIMITS {
                count(table, *max)?;
            }
            return Ok(self.tx.commit()?);
        };
        let version: i64 = self.tx.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        let stamp: WriteStamp = (self.tx.total_changes(), version);
        // An estimate made before another connection wrote is not used.
        let prior = budget.state.get().filter(|s| s.stamp.1 == version && s.stamp.0 <= stamp.0);
        let mut headroom = [0i64; TABLES];
        for (i, (table, max)) in ROW_LIMITS.iter().enumerate() {
            let estimate = prior.and_then(|s| {
                let spent = i64::try_from(stamp.0 - s.stamp.0).ok()?;
                (s.headroom[i] >= spent).then_some(s.headroom[i] - spent)
            });
            headroom[i] = match estimate {
                Some(left) => left,
                None => *max - count(table, *max)?,
            };
        }
        self.tx.commit()?;
        // Recorded only for a transaction that was written: a refused or
        // failed one leaves the earlier (still conservative) estimate.
        budget.state.set(Some(BudgetState { stamp, headroom }));
        Ok(())
    }

    pub fn revision(&self) -> StoreResult<u64> {
        let v: Option<String> =
            self.tx.query_row("SELECT value FROM meta WHERE key = 'revision'", [], |r| r.get(0)).optional()?;
        Ok(v.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn entity(&self, id: &EntityId) -> StoreResult<Option<Entity>> {
        let mut stmt = self.tx.prepare_cached(&format!("SELECT {ENTITY_COLUMNS} FROM entities WHERE id = ?1"))?;
        stmt.query_row([id.as_str()], read_entity).optional()?.transpose()
    }

    /// Current bindings of an entity (inside the transaction).
    pub fn bindings(&self, entity: &EntityId) -> StoreResult<Vec<SourceBinding>> {
        let mut stmt = self.tx.prepare_cached(&format!(
            "SELECT {BINDING_COLUMNS} FROM bindings WHERE entity_id = ?1 ORDER BY ordinal LIMIT 64"
        ))?;
        let rows = stmt.query_map([entity.as_str()], read_binding)?;
        rows.map(|r| r?).collect()
    }

    /// The live resource whose key is `key` (its workspace path), if any.
    pub fn live_resource(&self, key: &str) -> StoreResult<Option<Entity>> {
        let mut stmt = self.tx.prepare_cached(&format!(
            "SELECT {ENTITY_COLUMNS} FROM entities WHERE kind = 'resource' AND key = ?1 AND lifecycle != 'tombstoned'
             ORDER BY created_at, id LIMIT 1"
        ))?;
        stmt.query_row([key], read_entity).optional()?.transpose()
    }

    pub fn relation(&self, id: &RelationId) -> StoreResult<Option<Relation>> {
        let mut stmt = self.tx.prepare_cached(&format!("SELECT {RELATION_COLUMNS} FROM relations WHERE id = ?1"))?;
        stmt.query_row([id.as_str()], read_relation).optional()?.transpose()
    }

    pub fn put_entity(&self, e: &Entity) -> StoreResult<()> {
        check_field(Some(&e.label), MAX_FIELD, "the label")?;
        check_field(e.key.as_deref(), MAX_FIELD, "the key")?;
        check_field(e.tombstone_reason.as_deref(), MAX_FIELD, "the tombstone reason")?;
        let (roles, origin, attributes) = (
            serde_json::to_string(&e.roles)?,
            serde_json::to_string(&e.origin)?,
            serde_json::to_string(&e.attributes)?,
        );
        for (json, what) in [(&roles, "the roles"), (&origin, "the origin"), (&attributes, "the attributes")] {
            check_record(json, what)?;
        }
        self.tx.prepare_cached("INSERT INTO entities (id, kind, roles, label, key, lifecycle, tombstone_reason, revision, origin, attributes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, roles = excluded.roles, label = excluded.label,
             key = excluded.key, lifecycle = excluded.lifecycle, tombstone_reason = excluded.tombstone_reason,
             revision = excluded.revision, origin = excluded.origin, attributes = excluded.attributes,
             updated_at = excluded.updated_at")?.execute(params![
                e.id.as_str(),
                e.kind.as_str(),
                roles,
                e.label,
                e.key,
                e.lifecycle.as_str(),
                e.tombstone_reason,
                e.revision as i64,
                origin,
                attributes,
                e.created_at_ms as i64,
                e.updated_at_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Replace all bindings of an entity.
    pub fn set_bindings(&self, entity: &EntityId, bindings: &[SourceBinding]) -> StoreResult<()> {
        if bindings.len() > MAX_BINDINGS_PER_ENTITY {
            return Err(limit(format!("an entity has at most {MAX_BINDINGS_PER_ENTITY} source bindings")));
        }
        for b in bindings {
            check_field(b.detail.as_deref(), MAX_FIELD, "a binding detail")?;
            check_field(b.fingerprint.as_deref(), MAX_SHORT_FIELD, "a binding fingerprint")?;
            check_field(b.version.as_ref().map(|v| v.content_hash.as_str()), MAX_SHORT_FIELD, "a content hash")?;
            check_record(&serde_json::to_string(&b.selector)?, "a binding selector")?;
        }
        self.tx.prepare_cached("DELETE FROM bindings WHERE entity_id = ?1")?.execute([entity.as_str()])?;
        for (ordinal, b) in bindings.iter().enumerate() {
            self.tx.prepare_cached("INSERT INTO bindings (entity_id, ordinal, path, selector, range_start, range_end, content_hash, revision, saved, fingerprint, state, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)")?.execute(params![
                    entity.as_str(),
                    ordinal as i64,
                    b.path.as_str(),
                    serde_json::to_string(&b.selector)?,
                    b.range.map(|r| r.start as i64),
                    b.range.map(|r| r.end as i64),
                    b.version.as_ref().map(|v| v.content_hash.clone()),
                    b.version.as_ref().and_then(|v| v.revision).map(|r| r as i64),
                    b.version.as_ref().map(|v| v.saved).unwrap_or(true) as i64,
                    b.fingerprint,
                    b.state.as_str(),
                    b.detail
                ],
            )?;
        }
        Ok(())
    }

    pub fn put_relation(&self, r: &Relation) -> StoreResult<()> {
        check_field(r.note.as_deref(), MAX_FIELD, "the note")?;
        check_field(r.from_version.as_deref(), MAX_SHORT_FIELD, "a version")?;
        check_field(r.to_version.as_deref(), MAX_SHORT_FIELD, "a version")?;
        check_field(r.reviewed_statement.as_deref(), MAX_SHORT_FIELD, "a reviewed statement digest")?;
        check_record(&serde_json::to_string(&r.origin)?, "the origin")?;
        check_record(&serde_json::to_string(&r.evidence)?, "the evidence list")?;
        self.tx.prepare_cached("INSERT INTO relations (id, kind, from_id, to_id, origin, status, evidence_kind, evidence, from_version, to_version, note, revision, created_at, updated_at, reviewed_statement)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status, evidence_kind = excluded.evidence_kind,
             evidence = excluded.evidence, from_version = excluded.from_version, to_version = excluded.to_version,
             note = excluded.note, revision = excluded.revision, origin = excluded.origin, updated_at = excluded.updated_at,
             reviewed_statement = excluded.reviewed_statement")?.execute(params![
                r.id.as_str(),
                r.kind.as_str(),
                r.from.as_str(),
                r.to.as_str(),
                serde_json::to_string(&r.origin)?,
                r.status.as_str(),
                r.evidence_kind.map(|k| k.as_str()),
                serde_json::to_string(&r.evidence)?,
                r.from_version,
                r.to_version,
                r.note,
                r.revision as i64,
                r.created_at_ms as i64,
                r.updated_at_ms as i64,
                r.reviewed_statement
            ],
        )?;
        Ok(())
    }

    pub fn delete_relation(&self, id: &RelationId) -> StoreResult<bool> {
        Ok(self.tx.prepare_cached("DELETE FROM relations WHERE id = ?1")?.execute([id.as_str()])? > 0)
    }

    pub fn find_relation(&self, kind: RelationKind, from: &EntityId, to: &EntityId) -> StoreResult<Option<Relation>> {
        let mut stmt = self.tx.prepare_cached(&find_relation_sql(kind))?;
        stmt.query_row(params![kind.as_str(), from.as_str(), to.as_str()], read_relation).optional()?.transpose()
    }

    /// The container (`contains` source) of an entity, if any.
    pub fn container_of(&self, entity: &EntityId) -> StoreResult<Option<RelationId>> {
        let id: Option<String> = self
            .tx
            .query_row(
                "SELECT id FROM relations WHERE kind = 'contains' AND to_id = ?1 LIMIT 1",
                [entity.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|i| RelationId::parse(&i).map_err(|_| StoreError::Untrusted("invalid relation id".into()))).transpose()
    }

    pub fn put_evidence(&self, e: &Evidence) -> StoreResult<()> {
        let kind = e.kind.as_str();
        let body = serde_json::to_string(e)?;
        check_record(&body, "the evidence record")?;
        self.tx
            .prepare_cached("INSERT INTO evidence (id, kind, subject, body, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)")?
            .execute(params![
                e.id.as_str(),
                kind,
                e.subject.as_ref().map(|s| s.as_str().to_owned()),
                body,
                e.recorded_at_ms as i64
            ])?;
        Ok(())
    }

    /// Seal an evidence record as recorded by this installation: a MAC over
    /// its id and stored body under the app-local key.
    pub fn seal_evidence(&self, id: &EvidenceId, key: &[u8; 32]) -> StoreResult<()> {
        let body: String = self.tx.query_row("SELECT body FROM evidence WHERE id = ?1", [id.as_str()], |r| r.get(0))?;
        self.tx
            .prepare_cached(
                "INSERT INTO evidence_seals (id, mac) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET mac = excluded.mac",
            )?
            .execute(params![id.as_str(), crate::mac::evidence_mac(key, id.as_str(), &body)])?;
        Ok(())
    }

    pub fn evidence_exists(&self, id: &EvidenceId) -> StoreResult<bool> {
        Ok(self.tx.query_row("SELECT 1 FROM evidence WHERE id = ?1", [id.as_str()], |_| Ok(())).optional()?.is_some())
    }

    /// The kind and subject of an evidence record (`None`: no such record).
    /// A kind this version does not know reads as `None`, never as a match.
    pub fn evidence_head(&self, id: &EvidenceId) -> StoreResult<Option<(Option<EvidenceKind>, Option<EntityId>)>> {
        let row: Option<(String, Option<String>)> = self
            .tx
            .query_row("SELECT kind, subject FROM evidence WHERE id = ?1", [id.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        Ok(row.map(|(kind, subject)| (EvidenceKind::parse(&kind), subject.and_then(|s| EntityId::parse(&s).ok()))))
    }

    pub fn set_coverage(&self, c: &ResourceCoverage) -> StoreResult<()> {
        check_field(c.detail.as_deref(), MAX_FIELD, "a coverage detail")?;
        check_field(c.extractor.as_deref(), MAX_SHORT_FIELD, "an extractor name")?;
        check_field(c.content_hash.as_deref(), MAX_SHORT_FIELD, "a content hash")?;
        self.tx
            .prepare_cached(
                "INSERT INTO coverage (path, extractor, state, unresolved, detail, content_hash, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET extractor = excluded.extractor, state = excluded.state,
             unresolved = excluded.unresolved, detail = excluded.detail, content_hash = excluded.content_hash,
             indexed_at = excluded.indexed_at",
            )?
            .execute(params![
                c.path.as_str(),
                c.extractor,
                c.state.as_str(),
                c.unresolved as i64,
                c.detail,
                c.content_hash,
                now_ms() as i64
            ])?;
        Ok(())
    }

    /// Mark files (and everything under folders) as changed and not yet
    /// reconciled. `token` increases monotonically; a pass clears only marks
    /// made before it started.
    pub fn mark_pending(&self, paths: &[WorkspacePath], token: u64) -> StoreResult<usize> {
        let mut n = 0;
        let mut stmt = self.tx.prepare_cached(
            "UPDATE coverage SET pending = ?4 WHERE path >= ?1 AND path < ?3 AND (path = ?1 OR path >= ?2)",
        )?;
        for p in paths {
            let (low, high) = under(p);
            n += stmt.execute(params![p.as_str(), low, high, token as i64])?;
        }
        Ok(n)
    }

    /// Clear pending marks made up to `token` for the files a pass covered
    /// (`None`: every file).
    pub fn clear_pending(&self, prefixes: Option<&[WorkspacePath]>, token: u64) -> StoreResult<()> {
        match prefixes {
            None => {
                self.tx
                    .execute("UPDATE coverage SET pending = 0 WHERE pending > 0 AND pending <= ?1", [token as i64])?;
            }
            Some(prefixes) => {
                let mut stmt = self.tx.prepare_cached(
                    "UPDATE coverage SET pending = 0 WHERE pending > 0 AND pending <= ?4
                     AND path >= ?1 AND path < ?3 AND (path = ?1 OR path >= ?2)",
                )?;
                for p in prefixes {
                    let (low, high) = under(p);
                    stmt.execute(params![p.as_str(), low, high, token as i64])?;
                }
            }
        }
        Ok(())
    }

    pub fn remove_coverage(&self, path: &WorkspacePath) -> StoreResult<()> {
        self.tx.prepare_cached("DELETE FROM coverage WHERE path = ?1")?.execute([path.as_str()])?;
        Ok(())
    }

    /// Record the file state a coverage row was computed from (the
    /// incremental-reindex checkpoint). The coverage row must exist.
    pub fn set_file_state(
        &self,
        path: &WorkspacePath,
        size: Option<u64>,
        modified_ms: Option<u64>,
        identity: Option<&str>,
    ) -> StoreResult<()> {
        self.tx
            .prepare_cached("UPDATE coverage SET size = ?2, modified_ms = ?3, file_identity = ?4 WHERE path = ?1")?
            .execute(params![path.as_str(), size.map(|s| s as i64), modified_ms.map(|m| m as i64), identity])?;
        Ok(())
    }

    pub fn put_source_index(&self, record: &IndexRecord) -> StoreResult<()> {
        if record.body.len() > MAX_INDEX_BODY {
            return Err(limit("a source index record is too large"));
        }
        check_field(Some(&record.content_hash), MAX_SHORT_FIELD, "a content hash")?;
        check_field(Some(&record.extractor), MAX_SHORT_FIELD, "an extractor name")?;
        self.tx
            .prepare_cached(
                "INSERT INTO source_index (path, content_hash, extractor, body) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET content_hash = excluded.content_hash, extractor = excluded.extractor,
             body = excluded.body",
            )?
            .execute(params![record.path.as_str(), record.content_hash, record.extractor, record.body])?;
        Ok(())
    }

    /// Link targets a file's references are waiting for (`name:<label or
    /// key>`, `path:<workspace path>`), so it is resolved again only when one
    /// of them appears.
    pub fn set_index_wanted(&self, path: &WorkspacePath, wanted: &[String]) -> StoreResult<()> {
        self.tx
            .prepare_cached("UPDATE source_index SET wanted = ?2 WHERE path = ?1")?
            .execute(params![path.as_str(), serde_json::to_string(wanted)?])?;
        Ok(())
    }

    pub fn remove_source_index(&self, path: &WorkspacePath) -> StoreResult<()> {
        self.tx.prepare_cached("DELETE FROM source_index WHERE path = ?1")?.execute([path.as_str()])?;
        Ok(())
    }

    pub fn set_meta(&self, key: &str, value: &str) -> StoreResult<()> {
        if key.len() > MAX_SHORT_FIELD || value.len() > 4096 {
            return Err(limit("graph metadata is too long"));
        }
        set_meta(&self.tx, key, value)
    }

    /// Bump the project revision and journal the change. Returns the new revision.
    pub fn record_change(&self, origin: &Origin, summary: &str, delta: &mut GraphDelta) -> StoreResult<u64> {
        let revision = self.revision()? + 1;
        delta.revision = revision;
        set_meta(&self.tx, "revision", &revision.to_string())?;
        self.tx
            .prepare_cached("INSERT INTO changes (revision, at, origin, summary, delta) VALUES (?1, ?2, ?3, ?4, ?5)")?
            .execute(params![
                revision as i64,
                now_ms() as i64,
                serde_json::to_string(origin)?,
                truncate(summary, 2000),
                serde_json::to_string(delta)?
            ])?;
        // Keep the journal bounded (the latest 10 000 changes).
        self.tx.prepare_cached("DELETE FROM changes WHERE revision <= ?1")?.execute([revision as i64 - 10_000])?;
        Ok(revision)
    }
}

/// The file state a coverage row was computed from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub coverage: ResourceCoverage,
    pub size: Option<u64>,
    pub modified_ms: Option<u64>,
    /// Opaque physical file identity (rename confirmation), when known.
    pub identity: Option<String>,
}

/// A resource's identity by path (see [`GraphStore::resource_keys`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceKey {
    pub path: WorkspacePath,
    pub id: EntityId,
    pub roles: Vec<ResourceRole>,
    pub tombstoned: bool,
    pub created_at_ms: u64,
}

/// One file's rebuildable source index (JSON body owned by the reconciler).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexRecord {
    pub path: WorkspacePath,
    pub content_hash: String,
    pub extractor: String,
    pub body: String,
}

// ----- row decoding -----

/// The last column, `pending`, is a change token: non-zero while the file
/// changed and has not been reconciled since (read as `CoverageState::Pending`
/// by [`GraphStore::coverage`]; the stored state is kept underneath).
const COVERAGE_COLUMNS: &str = "path, extractor, state, unresolved, detail, content_hash, pending";

const ENTITY_COLUMNS: &str =
    "id, kind, roles, label, key, lifecycle, tombstone_reason, revision, origin, attributes, created_at, updated_at";
const BINDING_COLUMNS: &str =
    "entity_id, path, selector, range_start, range_end, content_hash, revision, saved, fingerprint, state, detail";
const RELATION_COLUMNS: &str = "id, kind, from_id, to_id, origin, status, evidence_kind, evidence, from_version, to_version, note, revision, created_at, updated_at, reviewed_statement";

/// Rows one complete relation read can return: the table's row limit.
const RELATION_ROWS: usize = 2_000_000;

/// The lookup of one relation by kind and endpoints, seeking from the end
/// that has few relations: a container holds many things (seek by the
/// contained item); for every other kind it is the target that many
/// relations share (a file its declarations are anchored in, a result many
/// others depend on), so the source is the seek. Without this the planner
/// may walk every relation of the busy end for each lookup. The row found
/// is the same either way (the earliest such relation).
fn find_relation_sql(kind: RelationKind) -> String {
    let ends = match kind {
        RelationKind::Contains => "+from_id = ?2 AND to_id = ?3",
        _ => "from_id = ?2 AND +to_id = ?3",
    };
    format!("SELECT {RELATION_COLUMNS} FROM relations WHERE kind = ?1 AND {ends} LIMIT 1")
}

/// Which end of a relation an entity is looked up at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    From,
    To,
}

impl Endpoint {
    fn select(self) -> &'static str {
        match self {
            Endpoint::From => {
                "SELECT id, kind, from_id, to_id, origin, status, evidence_kind, evidence, from_version, to_version, note, revision, created_at, updated_at, reviewed_statement FROM relations WHERE from_id = ?1 ORDER BY kind, id LIMIT ?2"
            }
            Endpoint::To => {
                "SELECT id, kind, from_id, to_id, origin, status, evidence_kind, evidence, from_version, to_version, note, revision, created_at, updated_at, reviewed_statement FROM relations WHERE to_id = ?1 ORDER BY kind, id LIMIT ?2"
            }
        }
    }
}

type Decoded<T> = StoreResult<T>;

fn untrusted(what: &str) -> StoreError {
    StoreError::Untrusted(format!("invalid {what}"))
}

fn bounded(s: String) -> StoreResult<String> {
    if s.len() > MAX_FIELD { Err(untrusted("oversized field")) } else { Ok(s) }
}

fn short(s: String) -> StoreResult<String> {
    if s.len() > MAX_SHORT_FIELD || s.chars().any(char::is_control) { Err(untrusted("short field")) } else { Ok(s) }
}

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> StoreResult<T> {
    if raw.len() > 4 * MAX_FIELD {
        return Err(untrusted("oversized record"));
    }
    serde_json::from_str(raw).map_err(|e| StoreError::Untrusted(format!("unreadable record: {e}")))
}

fn read_entity(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decoded<Entity>> {
    let id: String = r.get(0)?;
    let kind: String = r.get(1)?;
    let roles: String = r.get(2)?;
    let label: String = r.get(3)?;
    let key: Option<String> = r.get(4)?;
    let lifecycle: String = r.get(5)?;
    let tombstone_reason: Option<String> = r.get(6)?;
    let revision: i64 = r.get(7)?;
    let origin: String = r.get(8)?;
    let attributes: String = r.get(9)?;
    let created: i64 = r.get(10)?;
    let updated: i64 = r.get(11)?;
    Ok((|| {
        Ok(Entity {
            id: EntityId::parse(&id).map_err(|_| untrusted("entity id"))?,
            kind: EntityKind::parse(&kind).ok_or_else(|| untrusted("entity kind"))?,
            roles: decode::<Vec<ResourceRole>>(&roles)?,
            label: bounded(label)?,
            key: key.map(bounded).transpose()?,
            lifecycle: Lifecycle::parse(&lifecycle).ok_or_else(|| untrusted("lifecycle"))?,
            tombstone_reason: tombstone_reason.map(bounded).transpose()?,
            revision: revision.max(0) as u64,
            origin: decode::<Origin>(&origin)?,
            attributes: decode::<BTreeMap<String, String>>(&attributes)?,
            created_at_ms: created.max(0) as u64,
            updated_at_ms: updated.max(0) as u64,
        })
    })())
}

fn read_binding(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decoded<SourceBinding>> {
    let entity: String = r.get(0)?;
    let path: String = r.get(1)?;
    let selector: String = r.get(2)?;
    let start: Option<i64> = r.get(3)?;
    let end: Option<i64> = r.get(4)?;
    let hash: Option<String> = r.get(5)?;
    let revision: Option<i64> = r.get(6)?;
    let saved: i64 = r.get(7)?;
    let fingerprint: Option<String> = r.get(8)?;
    let state: String = r.get(9)?;
    let detail: Option<String> = r.get(10)?;
    Ok((|| {
        let range = match (start, end) {
            (Some(s), Some(e)) if s >= 0 && e >= s && e <= u32::MAX as i64 => Some(TextRange::new(s as u32, e as u32)),
            (None, None) => None,
            _ => return Err(untrusted("binding range")),
        };
        Ok(SourceBinding {
            entity: EntityId::parse(&entity).map_err(|_| untrusted("entity id"))?,
            path: WorkspacePath::parse(&path).map_err(|_| untrusted("binding path"))?,
            selector: decode::<SourceSelector>(&selector)?,
            range,
            version: hash
                .map(|content_hash| -> StoreResult<SourceVersion> {
                    Ok(SourceVersion {
                        content_hash: short(content_hash)?,
                        revision: revision.map(|r| r.max(0) as u64),
                        saved: saved != 0,
                    })
                })
                .transpose()?,
            fingerprint: fingerprint.map(short).transpose()?,
            state: BindingState::parse(&state).ok_or_else(|| untrusted("binding state"))?,
            detail: detail.map(bounded).transpose()?,
        })
    })())
}

fn read_relation(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decoded<Relation>> {
    let id: String = r.get(0)?;
    let kind: String = r.get(1)?;
    let from: String = r.get(2)?;
    let to: String = r.get(3)?;
    let origin: String = r.get(4)?;
    let status: String = r.get(5)?;
    let evidence_kind: Option<String> = r.get(6)?;
    let evidence: String = r.get(7)?;
    let from_version: Option<String> = r.get(8)?;
    let to_version: Option<String> = r.get(9)?;
    let note: Option<String> = r.get(10)?;
    let revision: i64 = r.get(11)?;
    let created: i64 = r.get(12)?;
    let updated: i64 = r.get(13)?;
    let reviewed_statement: Option<String> = r.get(14)?;
    Ok((|| {
        Ok(Relation {
            id: RelationId::parse(&id).map_err(|_| untrusted("relation id"))?,
            kind: RelationKind::parse(&kind).ok_or_else(|| untrusted("relation kind"))?,
            from: EntityId::parse(&from).map_err(|_| untrusted("relation endpoint"))?,
            to: EntityId::parse(&to).map_err(|_| untrusted("relation endpoint"))?,
            origin: decode::<Origin>(&origin)?,
            status: AssertionStatus::parse(&status).ok_or_else(|| untrusted("assertion status"))?,
            evidence_kind: evidence_kind
                .map(|k| EvidenceKind::parse(&k).ok_or_else(|| untrusted("evidence kind")))
                .transpose()?,
            evidence: decode::<Vec<EvidenceId>>(&evidence)?,
            from_version: from_version.map(short).transpose()?,
            to_version: to_version.map(short).transpose()?,
            note: note.map(bounded).transpose()?,
            reviewed_statement: reviewed_statement.map(short).transpose()?,
            // Computed by `GraphStore::qualified` for what the store hands out.
            review: None,
            revision: revision.max(0) as u64,
            created_at_ms: created.max(0) as u64,
            updated_at_ms: updated.max(0) as u64,
        })
    })())
}

/// Coverage as others see it: a pending change reads as `Pending`.
fn read_coverage(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decoded<ResourceCoverage>> {
    let pending: i64 = r.get(6)?;
    Ok(read_coverage_stored(r)?.map(|mut c| {
        if pending > 0 {
            c.state = CoverageState::Pending;
        }
        c
    }))
}

/// Coverage as last reconciled.
fn read_coverage_stored(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decoded<ResourceCoverage>> {
    let path: String = r.get(0)?;
    let extractor: Option<String> = r.get(1)?;
    let state: String = r.get(2)?;
    let unresolved: i64 = r.get(3)?;
    let detail: Option<String> = r.get(4)?;
    let hash: Option<String> = r.get(5)?;
    Ok((|| {
        Ok(ResourceCoverage {
            path: WorkspacePath::parse(&path).map_err(|_| untrusted("coverage path"))?,
            extractor: extractor.map(short).transpose()?,
            state: CoverageState::parse(&state).ok_or_else(|| untrusted("coverage state"))?,
            unresolved: unresolved.clamp(0, u32::MAX as i64) as u32,
            detail: detail.map(bounded).transpose()?,
            content_hash: hash.map(short).transpose()?,
        })
    })())
}

fn set_meta(tx: &Transaction<'_>, key: &str, value: &str) -> StoreResult<()> {
    tx.prepare_cached(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?
    .execute(params![key, value])?;
    Ok(())
}

/// Prepared statements kept per connection (bounded, least recently used
/// first out; freed with the connection).
const STATEMENT_CACHE: usize = 128;

/// Defensive connection settings for a database that may be hostile.
fn harden(conn: &Connection) -> StoreResult<()> {
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    conn.pragma_update(None, "cell_size_check", "ON")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // A project folder may be synced or on a share; a rollback journal works
    // there (WAL does not), and a single connection writes.
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // The store keeps more distinct statements than rusqlite's default
    // cache (16) holds; a miss compiles the statement again.
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    Ok(())
}

/// The schema must contain exactly the objects `version` creates (no
/// triggers, views, or extra tables a hostile file could plant), and each
/// table exactly its columns.
fn verify_schema_objects(conn: &Connection, version: u32) -> StoreResult<()> {
    let expected = expected_objects(version);
    let mut stmt =
        conn.prepare("SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_autoindex_%' LIMIT 1000")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut found = BTreeSet::new();
    for row in rows {
        let (ty, name) = row?;
        if !expected.iter().any(|(t, n)| *t == ty && *n == name) {
            return Err(StoreError::Untrusted(format!("unexpected {ty} {}", truncate(&name, 80))));
        }
        found.insert(name);
    }
    for (_, name) in &expected {
        if !found.contains(*name) {
            return Err(StoreError::Corrupt(format!("missing {name}")));
        }
    }
    for (table, columns) in expected_columns(version) {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
        let actual: Vec<String> = stmt.query_map([table], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
        if actual.len() != columns.len() || actual.iter().zip(&columns).any(|(a, b)| a != b) {
            return Err(StoreError::Untrusted(format!("table {table} has unexpected columns")));
        }
    }
    Ok(())
}

/// The key range of the paths beneath folder `prefix`: `prefix/` <= path <
/// `prefix0` (`0` follows `/`), compared bytewise as the path columns are.
/// Unlike a `LIKE 'prefix/%'` pattern it is answered from the path index, and
/// it needs no escaping of `%` and `_`. The statements that also match
/// the path itself seek one range, `prefix` <= path < `prefix0`, and keep
/// the path and what is beneath it (a sibling such as `prefix.tex` falls in
/// the range and is filtered out).
fn under(prefix: &WorkspacePath) -> (String, String) {
    (format!("{}/", prefix.as_str()), format!("{}0", prefix.as_str()))
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DATABASE_FILE);
        (dir, path)
    }

    #[test]
    fn creates_and_reopens_with_the_same_project_id() {
        let (_dir, path) = temp_db();
        let first = GraphStore::open(&path, true).unwrap().project().clone();
        let again = GraphStore::open(&path, false).unwrap().project().clone();
        assert_eq!(first, again);
    }

    #[test]
    fn refuses_a_newer_schema() {
        let (_dir, path) = temp_db();
        drop(GraphStore::open(&path, true).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        match GraphStore::open(&path, false) {
            Err(StoreError::NewerSchema { found: 99, .. }) => {}
            other => panic!("expected NewerSchema, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn refuses_planted_triggers_and_foreign_tables() {
        let (_dir, path) = temp_db();
        drop(GraphStore::open(&path, true).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TRIGGER evil AFTER INSERT ON entities BEGIN DELETE FROM relations; END;").unwrap();
        drop(conn);
        assert!(matches!(GraphStore::open(&path, false), Err(StoreError::Untrusted(_))));

        let (_dir2, other) = temp_db();
        let conn = Connection::open(&other).unwrap();
        conn.execute_batch("CREATE TABLE stolen (x TEXT);").unwrap();
        drop(conn);
        assert!(matches!(GraphStore::open(&other, true), Err(StoreError::Untrusted(_))));
    }

    #[test]
    fn refuses_garbage_files() {
        let (_dir, path) = temp_db();
        std::fs::write(&path, b"this is not a database at all, just text").unwrap();
        assert!(GraphStore::open(&path, false).is_err());
    }

    /// A valid graph with one project entity, then `sql` applied through a
    /// plain (non-defensive) connection, as a hostile file would be crafted.
    fn crafted(sql: &str) -> (tempfile::TempDir, PathBuf) {
        let (dir, path) = temp_db();
        {
            let mut store = GraphStore::open(&path, true).unwrap();
            let w = Writer::new(store.transaction().unwrap());
            let now = now_ms();
            w.put_entity(&Entity {
                id: EntityId::parse("ent-project").unwrap(),
                kind: EntityKind::Project,
                roles: vec![],
                label: "P".into(),
                key: None,
                lifecycle: Lifecycle::Active,
                tombstone_reason: None,
                revision: 1,
                origin: Origin::User,
                attributes: BTreeMap::new(),
                created_at_ms: now,
                updated_at_ms: now,
            })
            .unwrap();
            w.commit().unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute_batch(sql).unwrap();
        drop(conn);
        (dir, path)
    }

    const ENTITY: &str = "INSERT INTO entities (id, kind, roles, label, key, lifecycle, tombstone_reason, revision, origin, attributes, created_at, updated_at) VALUES";

    #[test]
    fn crafted_rows_are_refused_without_panicking() {
        let huge = "x".repeat(MAX_FIELD + 10);
        let cases: Vec<(&str, String)> = vec![
            ("bad kind", format!("{ENTITY} ('ent-a', 'bogus', '[]', 'A', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);")),
            ("bad json", format!("{ENTITY} ('ent-a', 'theorem', '[', 'A', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);")),
            ("bad id", format!("{ENTITY} ('ent-../x', 'theorem', '[]', 'A', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);")),
            ("huge text", format!("{ENTITY} ('ent-a', 'theorem', '[]', '{huge}', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);")),
            ("roles on a theorem", format!("{ENTITY} ('ent-a', 'theorem', '[\"manuscript\"]', 'A', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);")),
            ("second project", format!("{ENTITY} ('ent-b', 'project', '[]', 'B', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);")),
            (
                "inverted range",
                format!("{ENTITY} ('ent-a', 'theorem', '[]', 'A', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);
                 INSERT INTO bindings (entity_id, ordinal, path, selector, range_start, range_end, state) VALUES ('ent-a', 0, 'a.tex', '{{\"kind\":\"region\"}}', 10, 2, 'resolved');"),
            ),
            (
                "traversing binding path",
                format!("{ENTITY} ('ent-a', 'theorem', '[]', 'A', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);
                 INSERT INTO bindings (entity_id, ordinal, path, selector, state) VALUES ('ent-a', 0, '../x.tex', '{{\"kind\":\"wholeFile\"}}', 'resolved');"),
            ),
            (
                "dangling reference",
                "INSERT INTO relations (id, kind, from_id, to_id, origin, status, evidence, revision, created_at, updated_at) VALUES ('rel-a', 'references', 'ent-project', 'ent-missing', '{\"kind\":\"user\"}', 'accepted', '[]', 1, 0, 0);".to_owned(),
            ),
            (
                "endpoint rule",
                format!("{ENTITY} ('ent-a', 'theorem', '[]', 'A', NULL, 'active', NULL, 1, '{{\"kind\":\"user\"}}', '{{}}', 0, 0);
                 INSERT INTO relations (id, kind, from_id, to_id, origin, status, evidence, revision, created_at, updated_at) VALUES ('rel-a', 'proofOf', 'ent-a', 'ent-project', '{{\"kind\":\"user\"}}', 'accepted', '[]', 1, 0, 0);"),
            ),
            (
                "forged evidence",
                "INSERT INTO evidence (id, kind, subject, body, recorded_at) VALUES ('evd-a', 'formalCheck', NULL, '{\"id\":\"evd-b\"}', 0);".to_owned(),
            ),
            ("long hash", format!("INSERT INTO coverage (path, state, indexed_at, content_hash) VALUES ('a.tex', 'complete', 0, '{}');", "f".repeat(300))),
            ("coverage state", "INSERT INTO coverage (path, state, indexed_at) VALUES ('a.tex', 'perfect', 0);".to_owned()),
            ("view body", "INSERT INTO views (id, name, scope, body, revision, updated_at) VALUES ('view-a', 'V', '{}', 'nope', 1, 0);".to_owned()),
            ("revision text", "UPDATE meta SET value = 'many' WHERE key = 'revision';".to_owned()),
            ("too many rows", (0..1001).map(|i| format!("INSERT INTO meta (key, value) VALUES ('k{i}', 'v');")).collect::<String>()),
            ("extra column", "ALTER TABLE entities ADD COLUMN payload TEXT;".to_owned()),
        ];
        for (name, sql) in cases {
            let (_dir, path) = crafted(&sql);
            let before = std::fs::read(&path).unwrap();
            let result = std::panic::catch_unwind(|| GraphStore::open(&path, false).map(|_| ()));
            let outcome = result.unwrap_or_else(|_| panic!("{name}: opening panicked"));
            assert!(
                matches!(outcome, Err(StoreError::Untrusted(_) | StoreError::Corrupt(_))),
                "{name}: expected a refusal, got {outcome:?}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{name}: a refused database is left unchanged");
        }
    }

    #[test]
    fn writes_are_refused_before_they_exceed_a_bound_the_open_enforces() {
        let (_dir, path) = temp_db();
        let mut store = GraphStore::open(&path, true).unwrap();
        // Rows: a transaction that would leave a table over its limit is
        // refused as a whole.
        let w = Writer::new(store.transaction().unwrap());
        for i in 0..row_limit("meta") {
            w.tx.execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("k{i}")]).unwrap();
        }
        assert!(matches!(w.commit(), Err(StoreError::Limit(_))));
        assert!(store.meta("k1").unwrap().is_none(), "nothing was written");
        // Fields and records.
        let w = Writer::new(store.transaction().unwrap());
        let mut e = Entity {
            id: EntityId::mint(),
            kind: EntityKind::Claim,
            roles: vec![],
            label: "x".repeat(MAX_FIELD + 1),
            key: None,
            lifecycle: Lifecycle::Active,
            tombstone_reason: None,
            revision: 1,
            origin: Origin::User,
            attributes: BTreeMap::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        assert!(matches!(w.put_entity(&e), Err(StoreError::Limit(_))));
        e.label = "ok".into();
        w.put_entity(&e).unwrap();
        assert!(matches!(w.set_meta("k", &"v".repeat(5000)), Err(StoreError::Limit(_))));
        drop(w);
        // The database still opens.
        drop(store);
        GraphStore::open(&path, false).unwrap();
    }

    /// The row budget refuses exactly what counting every table refuses:
    /// across commits, right after a refused transaction, for rows written
    /// outside the write helpers, and after rows were deleted.
    #[test]
    fn the_row_budget_never_lets_a_table_pass_its_limit() {
        let (_dir, path) = temp_db();
        let mut store = GraphStore::open(&path, true).unwrap();
        let limit = row_limit("meta");
        let rows = |store: &GraphStore| -> i64 {
            store.connection().query_row("SELECT count(*) FROM meta", [], |r| r.get(0)).unwrap()
        };
        let add = |store: &mut GraphStore, from: i64, n: i64| -> StoreResult<()> {
            let w = store.writer()?;
            for i in from..from + n {
                w.tx.execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("k{i}")])?;
            }
            w.commit()
        };
        // Up to 100 rows below the limit, over two commits.
        let room = limit - rows(&store);
        add(&mut store, 0, room - 300).unwrap();
        add(&mut store, 1_000_000, 200).unwrap();
        assert_eq!(rows(&store), limit - 100);
        // 101 more rows would pass the limit: refused, nothing written.
        assert!(matches!(add(&mut store, 2_000_000, 101), Err(StoreError::Limit(_))));
        assert!(store.meta("k2000000").unwrap().is_none());
        assert_eq!(rows(&store), limit - 100);
        // Right after the refusal, rows that fit are accepted.
        add(&mut store, 3_000_000, 60).unwrap();
        // Rows written outside the helpers use up the table too.
        for i in 0..40 {
            store.connection().execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [format!("raw{i}")]).unwrap();
        }
        assert_eq!(rows(&store), limit);
        assert!(matches!(add(&mut store, 4_000_000, 1), Err(StoreError::Limit(_))));
        // A change that adds no row is still accepted in a full table.
        let w = store.writer().unwrap();
        w.set_meta("k0", "changed").unwrap();
        w.commit().unwrap();
        assert_eq!(store.meta("k0").unwrap().as_deref(), Some("changed"));
        // Deleted rows make room again, and no more than that.
        store.connection().execute("DELETE FROM meta WHERE key LIKE 'raw%'", []).unwrap();
        add(&mut store, 5_000_000, 40).unwrap();
        assert!(matches!(add(&mut store, 6_000_000, 1), Err(StoreError::Limit(_))));
        assert_eq!(rows(&store), limit);
        // The database the budget allowed is one the validated open accepts.
        drop(store);
        GraphStore::open(&path, false).unwrap();
    }

    /// A commit through the store's writer counts a table only when the rows
    /// changed since its last count could have filled it.
    #[test]
    fn a_small_commit_does_not_count_the_tables() {
        let mut store = GraphStore::open_in_memory().unwrap();
        let commit = |store: &mut GraphStore, key: &str| {
            let w = store.writer().unwrap();
            w.set_meta(key, "v").unwrap();
            w.commit().unwrap();
        };
        commit(&mut store, "first");
        let counted = |store: &GraphStore| store.budget.state.get().map(|s| s.headroom).unwrap();
        let before = counted(&store);
        commit(&mut store, "second");
        let after = counted(&store);
        // Every table's headroom went down by the one changed row: none was
        // recounted (a recount of `meta` would show the same value here, a
        // recount of an empty table its full limit).
        for (i, (table, _)) in ROW_LIMITS.iter().enumerate() {
            assert_eq!(after[i], before[i] - 1, "{table}");
        }
    }

    #[test]
    fn a_pass_clears_only_pending_marks_made_before_it_started() {
        let mut store = GraphStore::open_in_memory().unwrap();
        let path = WorkspacePath::parse("a.md").unwrap();
        let coverage = ResourceCoverage {
            path: path.clone(),
            extractor: Some("markdown/1".into()),
            state: CoverageState::Complete,
            unresolved: 0,
            detail: None,
            content_hash: Some("h".into()),
        };
        let w = Writer::new(store.transaction().unwrap());
        w.set_coverage(&coverage).unwrap();
        w.mark_pending(std::slice::from_ref(&path), 10).unwrap();
        w.commit().unwrap();
        assert_eq!(store.coverage(&path).unwrap().unwrap().state, CoverageState::Pending);
        assert_eq!(store.coverage_stored(&path).unwrap().unwrap().state, CoverageState::Complete);
        // A pass that started before the mark (token 9) leaves it.
        let w = Writer::new(store.transaction().unwrap());
        w.clear_pending(None, 9).unwrap();
        w.commit().unwrap();
        assert_eq!(store.coverage(&path).unwrap().unwrap().state, CoverageState::Pending);
        let w = Writer::new(store.transaction().unwrap());
        w.clear_pending(Some(std::slice::from_ref(&path)), 10).unwrap();
        w.commit().unwrap();
        assert_eq!(store.coverage(&path).unwrap().unwrap().state, CoverageState::Complete);
        assert_eq!(store.coverage_summary().unwrap().pending, 0);
    }

    #[test]
    fn a_version_1_graph_is_backed_up_then_migrated() {
        let (dir, path) = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0].1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute_batch(
                "INSERT INTO meta (key, value) VALUES ('project_id', 'prj-old'), ('revision', '7'), ('vocabulary_version', '1');
                 INSERT INTO coverage (path, state, indexed_at, content_hash) VALUES ('a.tex', 'complete', 0, 'abc');",
            )
            .unwrap();
        }
        let store = GraphStore::open(&path, false).unwrap();
        assert_eq!(store.project().as_str(), "prj-old");
        assert_eq!(store.revision().unwrap(), 7);
        let version: u32 = store.connection().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let checkpoints = store.checkpoints().unwrap();
        assert_eq!(checkpoints.len(), 1, "coverage survives the migration");
        let backups = crate::backup::list(&dir.path().join(crate::backup::BACKUP_DIR));
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].reason, "premigration");
        assert_eq!(backups[0].revision, 7);
        // The backup is the old schema, intact.
        let old = Connection::open(dir.path().join(crate::backup::BACKUP_DIR).join(&backups[0].name)).unwrap();
        let v: u32 = old.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, 1);
    }

    /// P1: a schema-3 graph with records keeps them all when it gains the
    /// recoverable-deletion tables (after a premigration backup).
    #[test]
    fn a_version_3_graph_keeps_its_records_when_upgraded_to_version_4() {
        let (dir, path) = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            for (_, sql) in &MIGRATIONS[..3] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 3).unwrap();
            conn.execute_batch(
                "INSERT INTO meta (key, value) VALUES ('project_id', 'prj-old'), ('revision', '4'), ('vocabulary_version', '1');
                 INSERT INTO entities (id, kind, roles, label, key, lifecycle, tombstone_reason, revision, origin, attributes, created_at, updated_at)
                 VALUES ('ent-00000000000000000000000000000001', 'claim', '[]', 'A claim', NULL, 'active', NULL, 1, '{\"kind\":\"user\"}', '{}', 0, 0);
                 INSERT INTO workflows (id, revision, hash, state, body, saved_at)
                 VALUES ('wf-00000000000000000000000000000001', 1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'incomplete', '{}', 0);",
            )
            .unwrap();
        }
        let store = GraphStore::open(&path, false).unwrap();
        let version: u32 = store.connection().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(store.revision().unwrap(), 4);
        let claim = EntityId::parse("ent-00000000000000000000000000000001").unwrap();
        assert_eq!(store.entity(&claim).unwrap().unwrap().label, "A claim");
        let rows: i64 = store.connection().query_row("SELECT count(*) FROM workflows", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "workflow revisions survive");
        let empty: i64 = store.connection().query_row("SELECT count(*) FROM recoverable", [], |r| r.get(0)).unwrap();
        assert_eq!(empty, 0);
        let backups = crate::backup::list(&dir.path().join(crate::backup::BACKUP_DIR));
        assert!(backups.iter().any(|b| b.reason == "premigration"), "{backups:?}");
        drop(store);
        // Re-opening the upgraded file validates it like any other.
        GraphStore::open(&path, false).unwrap();
    }

    /// A schema-4 graph with a reviewed formal link stays readable when the
    /// reviewed-statement column is added: the link is kept, and since what
    /// its review covered was never recorded it reads as such, not as
    /// current and not as changed.
    #[test]
    fn a_version_4_graph_keeps_its_reviewed_links_and_reads_their_review_as_unrecorded() {
        let (dir, path) = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            for (_, sql) in &MIGRATIONS[..4] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 4).unwrap();
            conn.execute_batch(
                "INSERT INTO meta (key, value) VALUES ('project_id', 'prj-old'), ('revision', '9'), ('vocabulary_version', '1');
                 INSERT INTO entities (id, kind, roles, label, key, lifecycle, tombstone_reason, revision, origin, attributes, created_at, updated_at)
                 VALUES ('ent-00000000000000000000000000000001', 'theorem', '[]', 'A theorem', NULL, 'active', NULL, 1, '{\"kind\":\"user\"}', '{}', 0, 0),
                        ('ent-00000000000000000000000000000002', 'formalDeclaration', '[]', 'thm', 'thm', 'active', NULL, 1, '{\"kind\":\"service\",\"service\":\"lean\"}', '{}', 0, 0);
                 INSERT INTO relations (id, kind, from_id, to_id, origin, status, evidence_kind, evidence, from_version, to_version, note, revision, created_at, updated_at)
                 VALUES ('rel-00000000000000000000000000000001', 'formalizes', 'ent-00000000000000000000000000000002', 'ent-00000000000000000000000000000001',
                         '{\"kind\":\"user\"}', 'accepted', NULL, '[]', NULL, NULL, 'reviewed in an older version', 2, 0, 0);",
            )
            .unwrap();
        }
        let store = GraphStore::open(&path, false).unwrap();
        let version: u32 = store.connection().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(store.revision().unwrap(), 9);
        let id = RelationId::parse("rel-00000000000000000000000000000001").unwrap();
        let link = store.relation(&id).unwrap().expect("the link survives the upgrade");
        assert_eq!(link.status, AssertionStatus::Accepted);
        assert_eq!(link.note.as_deref(), Some("reviewed in an older version"));
        assert_eq!(link.reviewed_statement, None);
        assert_eq!(link.review.map(|r| r.state), Some(crate::model::LinkReviewState::Unrecorded));
        let backups = crate::backup::list(&dir.path().join(crate::backup::BACKUP_DIR));
        assert!(backups.iter().any(|b| b.reason == "premigration" && b.revision == 9), "{backups:?}");
        drop(store);
        // The upgraded file validates like any other, and a planted digest
        // beyond the field bound is refused like every other short field.
        GraphStore::open(&path, false).unwrap();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("UPDATE relations SET reviewed_statement = ?1", [&"x".repeat(MAX_SHORT_FIELD + 1)]).unwrap();
        }
        assert!(matches!(GraphStore::open(&path, false), Err(StoreError::Untrusted(_))));
    }

    #[test]
    fn a_planted_recoverable_kind_is_refused() {
        let (_dir, path) = temp_db();
        {
            let store = GraphStore::open(&path, true).unwrap();
            store
                .connection()
                .execute(
                    "INSERT INTO recoverable (id, kind, label, body, deleted_at) VALUES ('x', 'program', 'x', '{}', 0)",
                    [],
                )
                .unwrap();
        }
        assert!(matches!(GraphStore::open(&path, false), Err(StoreError::Untrusted(_))));
    }
}
