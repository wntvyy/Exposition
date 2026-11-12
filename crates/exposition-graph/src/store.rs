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
pub const SCHEMA_VERSION: u32 = 1;

/// File name of the graph database inside `.exposition/`.
pub const DATABASE_FILE: &str = "graph.sqlite3";

const MIGRATIONS: &[(u32, &str)] = &[(
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
)];

/// Every schema object a valid version-1 database has (type, name).
const EXPECTED_OBJECTS: &[(&str, &str)] = &[
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

/// Upper bounds on what one read returns.
pub const MAX_ROWS: usize = 20_000;
/// Text fields read from the database are bounded (hostile files).
const MAX_FIELD: usize = 256 * 1024;

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
}

pub type StoreResult<T> = Result<T, StoreError>;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// One open project graph database.
pub struct GraphStore {
    conn: Connection,
    path: Option<PathBuf>,
    project: ProjectId,
}

impl GraphStore {
    /// Open (or, when `create`, create) the database at `path` as untrusted
    /// input. Fails with [`StoreError::NewerSchema`], [`StoreError::Corrupt`],
    /// or [`StoreError::Untrusted`] instead of guessing.
    pub fn open(path: &Path, create: bool) -> StoreResult<Self> {
        let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
        let conn = Connection::open_with_flags(path, flags)?;
        Self::init(conn, Some(path.to_path_buf()))
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
            verify_schema_objects(&conn)?;
        } else {
            // A database with user_version 0 must be empty before we create
            // our schema in it (never adopt someone else's tables).
            let objects: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            if objects > 0 {
                return Err(StoreError::Untrusted("the file contains tables this version did not create".into()));
            }
        }
        let mut store = Self { conn, path, project: ProjectId::mint() };
        store.migrate(version)?;
        store.project = store.load_or_create_project()?;
        Ok(store)
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

    pub fn meta(&self, key: &str) -> StoreResult<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    /// Current project graph revision.
    pub fn revision(&self) -> StoreResult<u64> {
        Ok(self.meta("revision")?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn counts(&self) -> StoreResult<(u32, u32)> {
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

    pub fn entities(&self, ids: &[EntityId]) -> StoreResult<Vec<Entity>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids.iter().take(MAX_ROWS) {
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

    pub fn bindings_at_path(&self, path: &WorkspacePath) -> StoreResult<Vec<SourceBinding>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {BINDING_COLUMNS} FROM bindings WHERE path = ?1 ORDER BY entity_id, ordinal LIMIT {MAX_ROWS}"
        ))?;
        let rows = stmt.query_map([path.as_str()], read_binding)?;
        rows.map(|r| r?).collect()
    }

    // ----- relations -----

    pub fn relation(&self, id: &RelationId) -> StoreResult<Option<Relation>> {
        let mut stmt = self.conn.prepare_cached(&format!("SELECT {RELATION_COLUMNS} FROM relations WHERE id = ?1"))?;
        stmt.query_row([id.as_str()], read_relation).optional()?.transpose()
    }

    pub fn relations_from(&self, entity: &EntityId) -> StoreResult<Vec<Relation>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {RELATION_COLUMNS} FROM relations WHERE from_id = ?1 ORDER BY kind, id LIMIT {MAX_ROWS}"
        ))?;
        let rows = stmt.query_map([entity.as_str()], read_relation)?;
        rows.map(|r| r?).collect()
    }

    pub fn relations_to(&self, entity: &EntityId) -> StoreResult<Vec<Relation>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {RELATION_COLUMNS} FROM relations WHERE to_id = ?1 ORDER BY kind, id LIMIT {MAX_ROWS}"
        ))?;
        let rows = stmt.query_map([entity.as_str()], read_relation)?;
        rows.map(|r| r?).collect()
    }

    /// Relations whose both endpoints are in `ids`.
    pub fn relations_among(&self, ids: &BTreeSet<EntityId>) -> StoreResult<Vec<Relation>> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for id in ids {
            for r in self.relations_from(id)? {
                if ids.contains(&r.to) && seen.insert(r.id.clone()) {
                    out.push(r);
                    if out.len() >= MAX_ROWS {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    /// An existing relation of this kind between these endpoints (not rejected).
    pub fn find_relation(&self, kind: RelationKind, from: &EntityId, to: &EntityId) -> StoreResult<Option<Relation>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {RELATION_COLUMNS} FROM relations WHERE kind = ?1 AND from_id = ?2 AND to_id = ?3 LIMIT 1"
        ))?;
        stmt.query_row(params![kind.as_str(), from.as_str(), to.as_str()], read_relation).optional()?.transpose()
    }

    // ----- evidence -----

    pub fn evidence(&self, id: &EvidenceId) -> StoreResult<Option<Evidence>> {
        let body: Option<String> =
            self.conn.query_row("SELECT body FROM evidence WHERE id = ?1", [id.as_str()], |r| r.get(0)).optional()?;
        body.map(|b| decode::<Evidence>(&b)).transpose()
    }

    pub fn evidence_for_subject(&self, subject: &EntityId) -> StoreResult<Vec<Evidence>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT body FROM evidence WHERE subject = ?1 ORDER BY recorded_at DESC, id LIMIT 200")?;
        let rows = stmt.query_map([subject.as_str()], |r| r.get::<_, String>(0))?;
        rows.map(|r| decode::<Evidence>(&r?)).collect()
    }

    // ----- coverage -----

    pub fn coverage(&self, path: &WorkspacePath) -> StoreResult<Option<ResourceCoverage>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT path, extractor, state, unresolved, detail, content_hash FROM coverage WHERE path = ?1",
        )?;
        stmt.query_row([path.as_str()], read_coverage).optional()?.transpose()
    }

    pub fn all_coverage(&self) -> StoreResult<Vec<ResourceCoverage>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT path, extractor, state, unresolved, detail, content_hash FROM coverage ORDER BY path LIMIT {MAX_ROWS}"
        ))?;
        let rows = stmt.query_map([], read_coverage)?;
        rows.map(|r| r?).collect()
    }

    pub fn coverage_summary(&self) -> StoreResult<CoverageSummary> {
        let mut summary = CoverageSummary::default();
        let mut stmt =
            self.conn.prepare_cached("SELECT state, count(*), sum(unresolved) FROM coverage GROUP BY state")?;
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
        self.conn.execute(
            "INSERT INTO views (id, name, scope, body, revision, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, scope = excluded.scope, body = excluded.body,
             revision = excluded.revision, updated_at = excluded.updated_at",
            params![
                view.id.as_str(),
                view.name,
                serde_json::to_string(&view.scope)?,
                serde_json::to_string(view)?,
                view.revision as i64,
                view.updated_at_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn delete_view(&mut self, id: &ViewId) -> StoreResult<bool> {
        Ok(self.conn.execute("DELETE FROM views WHERE id = ?1", [id.as_str()])? > 0)
    }
}

// ----- writes inside a transaction -----

/// Write helpers used inside one transaction. Each graph change bumps the
/// project revision exactly once (`record_change`).
pub struct Writer<'a> {
    pub tx: Transaction<'a>,
}

impl<'a> Writer<'a> {
    pub fn new(tx: Transaction<'a>) -> Self {
        Self { tx }
    }

    pub fn commit(self) -> StoreResult<()> {
        Ok(self.tx.commit()?)
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

    pub fn relation(&self, id: &RelationId) -> StoreResult<Option<Relation>> {
        let mut stmt = self.tx.prepare_cached(&format!("SELECT {RELATION_COLUMNS} FROM relations WHERE id = ?1"))?;
        stmt.query_row([id.as_str()], read_relation).optional()?.transpose()
    }

    pub fn put_entity(&self, e: &Entity) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO entities (id, kind, roles, label, key, lifecycle, tombstone_reason, revision, origin, attributes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, roles = excluded.roles, label = excluded.label,
             key = excluded.key, lifecycle = excluded.lifecycle, tombstone_reason = excluded.tombstone_reason,
             revision = excluded.revision, origin = excluded.origin, attributes = excluded.attributes,
             updated_at = excluded.updated_at",
            params![
                e.id.as_str(),
                e.kind.as_str(),
                serde_json::to_string(&e.roles)?,
                e.label,
                e.key,
                e.lifecycle.as_str(),
                e.tombstone_reason,
                e.revision as i64,
                serde_json::to_string(&e.origin)?,
                serde_json::to_string(&e.attributes)?,
                e.created_at_ms as i64,
                e.updated_at_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Replace all bindings of an entity.
    pub fn set_bindings(&self, entity: &EntityId, bindings: &[SourceBinding]) -> StoreResult<()> {
        self.tx.execute("DELETE FROM bindings WHERE entity_id = ?1", [entity.as_str()])?;
        for (ordinal, b) in bindings.iter().enumerate() {
            self.tx.execute(
                "INSERT INTO bindings (entity_id, ordinal, path, selector, range_start, range_end, content_hash, revision, saved, fingerprint, state, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
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
        self.tx.execute(
            "INSERT INTO relations (id, kind, from_id, to_id, origin, status, evidence_kind, evidence, from_version, to_version, note, revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status, evidence_kind = excluded.evidence_kind,
             evidence = excluded.evidence, from_version = excluded.from_version, to_version = excluded.to_version,
             note = excluded.note, revision = excluded.revision, origin = excluded.origin, updated_at = excluded.updated_at",
            params![
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
                r.updated_at_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn delete_relation(&self, id: &RelationId) -> StoreResult<bool> {
        Ok(self.tx.execute("DELETE FROM relations WHERE id = ?1", [id.as_str()])? > 0)
    }

    pub fn find_relation(&self, kind: RelationKind, from: &EntityId, to: &EntityId) -> StoreResult<Option<Relation>> {
        let mut stmt = self.tx.prepare_cached(&format!(
            "SELECT {RELATION_COLUMNS} FROM relations WHERE kind = ?1 AND from_id = ?2 AND to_id = ?3 LIMIT 1"
        ))?;
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
        self.tx.execute(
            "INSERT INTO evidence (id, kind, subject, body, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                e.id.as_str(),
                kind,
                e.subject.as_ref().map(|s| s.as_str().to_owned()),
                serde_json::to_string(e)?,
                e.recorded_at_ms as i64
            ],
        )?;
        Ok(())
    }

    pub fn evidence_exists(&self, id: &EvidenceId) -> StoreResult<bool> {
        Ok(self.tx.query_row("SELECT 1 FROM evidence WHERE id = ?1", [id.as_str()], |_| Ok(())).optional()?.is_some())
    }

    pub fn set_coverage(&self, c: &ResourceCoverage) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO coverage (path, extractor, state, unresolved, detail, content_hash, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET extractor = excluded.extractor, state = excluded.state,
             unresolved = excluded.unresolved, detail = excluded.detail, content_hash = excluded.content_hash,
             indexed_at = excluded.indexed_at",
            params![
                c.path.as_str(),
                c.extractor,
                c.state.as_str(),
                c.unresolved as i64,
                c.detail,
                c.content_hash,
                now_ms() as i64
            ],
        )?;
        Ok(())
    }

    pub fn remove_coverage(&self, path: &WorkspacePath) -> StoreResult<()> {
        self.tx.execute("DELETE FROM coverage WHERE path = ?1", [path.as_str()])?;
        Ok(())
    }

    /// Bump the project revision and journal the change. Returns the new revision.
    pub fn record_change(&self, origin: &Origin, summary: &str, delta: &mut GraphDelta) -> StoreResult<u64> {
        let revision = self.revision()? + 1;
        delta.revision = revision;
        set_meta(&self.tx, "revision", &revision.to_string())?;
        self.tx.execute(
            "INSERT INTO changes (revision, at, origin, summary, delta) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                revision as i64,
                now_ms() as i64,
                serde_json::to_string(origin)?,
                truncate(summary, 2000),
                serde_json::to_string(delta)?
            ],
        )?;
        // Keep the journal bounded (the latest 10 000 changes).
        self.tx.execute("DELETE FROM changes WHERE revision <= ?1", [revision as i64 - 10_000])?;
        Ok(revision)
    }
}

// ----- row decoding -----

const ENTITY_COLUMNS: &str =
    "id, kind, roles, label, key, lifecycle, tombstone_reason, revision, origin, attributes, created_at, updated_at";
const BINDING_COLUMNS: &str =
    "entity_id, path, selector, range_start, range_end, content_hash, revision, saved, fingerprint, state, detail";
const RELATION_COLUMNS: &str = "id, kind, from_id, to_id, origin, status, evidence_kind, evidence, from_version, to_version, note, revision, created_at, updated_at";

type Decoded<T> = StoreResult<T>;

fn untrusted(what: &str) -> StoreError {
    StoreError::Untrusted(format!("invalid {what}"))
}

fn bounded(s: String) -> StoreResult<String> {
    if s.len() > MAX_FIELD { Err(untrusted("oversized field")) } else { Ok(s) }
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
            version: hash.map(|content_hash| SourceVersion {
                content_hash,
                revision: revision.map(|r| r.max(0) as u64),
                saved: saved != 0,
            }),
            fingerprint,
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
            from_version,
            to_version,
            note: note.map(bounded).transpose()?,
            revision: revision.max(0) as u64,
            created_at_ms: created.max(0) as u64,
            updated_at_ms: updated.max(0) as u64,
        })
    })())
}

fn read_coverage(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decoded<ResourceCoverage>> {
    let path: String = r.get(0)?;
    let extractor: Option<String> = r.get(1)?;
    let state: String = r.get(2)?;
    let unresolved: i64 = r.get(3)?;
    let detail: Option<String> = r.get(4)?;
    let hash: Option<String> = r.get(5)?;
    Ok((|| {
        Ok(ResourceCoverage {
            path: WorkspacePath::parse(&path).map_err(|_| untrusted("coverage path"))?,
            extractor,
            state: CoverageState::parse(&state).ok_or_else(|| untrusted("coverage state"))?,
            unresolved: unresolved.clamp(0, u32::MAX as i64) as u32,
            detail: detail.map(bounded).transpose()?,
            content_hash: hash,
        })
    })())
}

fn set_meta(tx: &Transaction<'_>, key: &str, value: &str) -> StoreResult<()> {
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

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
    Ok(())
}

/// The schema must contain exactly the objects this version creates (no
/// triggers, views, or extra tables a hostile file could plant).
fn verify_schema_objects(conn: &Connection) -> StoreResult<()> {
    let mut stmt =
        conn.prepare("SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_autoindex_%' LIMIT 1000")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut found = BTreeSet::new();
    for row in rows {
        let (ty, name) = row?;
        if !EXPECTED_OBJECTS.iter().any(|(t, n)| *t == ty && *n == name) {
            return Err(StoreError::Untrusted(format!("unexpected {ty} {}", truncate(&name, 80))));
        }
        found.insert(name);
    }
    for (_, name) in EXPECTED_OBJECTS {
        if !found.contains(*name) {
            return Err(StoreError::Corrupt(format!("missing {name}")));
        }
    }
    Ok(())
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
}
