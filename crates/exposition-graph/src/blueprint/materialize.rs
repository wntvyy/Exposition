//! Materializing a validated blueprint into a project folder.
//!
//! Source files and the graph are not one transaction, so creation follows
//! the cross-service journal: intent is recorded first ([`Creator::begin`],
//! with the full blueprint kept beside the journal in application data), then
//! folders and files are created with create-new semantics (an existing file
//! is never replaced) and each created file is journaled; the caller opens the
//! project (graph attaches); then resources are admitted, workflow drafts
//! saved, and the blueprint's entities/relations applied as one service batch
//! through the graph validator ([`Creator::apply_graph`]); finally the
//! operation is completed. An interruption leaves the operation pending; it is
//! then reported as needing reconciliation with what was created and what is
//! missing ([`Creator::statuses`]) and the explicit choices to finish or keep
//! it as is — never replayed automatically, never reported complete.
//!
//! Creating a project authorizes only this scaffold: nothing here grants
//! trust, runs code, installs packages, or sends anything to a model.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use super::{BlueprintRef, ProjectBlueprint, blueprint_origin, paths};
use crate::admission::{Admission, classify};
use crate::ids::{EntityId, OperationId, ProjectId, WorkflowId};
use crate::journal::{OperationKind, OperationState};
use crate::model::{
    AssertionStatus, BindingState, GraphState, Lifecycle, Origin, SourceBinding, SourceSelector, SourceVersion,
};
use crate::service::{GraphError, GraphResult, GraphService};
use crate::store::{GraphStore, Writer, now_ms};
use crate::validate::{BatchRef, EntityUpsert, RelationUpsert, ServiceBatch};
use crate::vocabulary::{EntityKind, RelationKind};
use crate::workflow::{DraftState, SaveWorkflowRequest, ValidationContext};

const RECORD_DIR: &str = "project-creations";
const MAX_RECORD_BYTES: u64 = 32 * 1024 * 1024;
const SERVICE: &str = "projects";

pub const STEP_FILE: &str = "file";
pub const STEP_OPEN: &str = "open";
pub const STEP_WORKFLOW: &str = "workflow";
pub const STEP_GRAPH: &str = "graph";
pub const STEP_SETTINGS: &str = "settings";

/// The blueprint and expected file digests of one creation, kept in
/// application data beside the operation journal (for recovery).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreationRecord {
    pub operation: OperationId,
    pub root: String,
    pub blueprint: ProjectBlueprint,
    /// Path → SHA-256 of the content this creation writes.
    pub hashes: BTreeMap<String, String>,
    pub created_at_ms: u64,
    /// Who stands behind the blueprint (records without it are treated as imported).
    #[serde(default)]
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CreationFileState {
    /// On disk with exactly the content the setup wrote.
    Created,
    /// Not on disk.
    Missing,
    /// On disk with other content (partly written, or edited since).
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreationFile {
    pub path: String,
    pub state: CreationFileState,
}

/// Explicit choices for an interrupted creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CreationAction {
    /// Create the missing files (never replacing existing ones), save the
    /// workflow drafts, and apply the blueprint's graph content.
    Finish,
    /// Keep the files that exist (admitted as resources) and stop here.
    Keep,
}

/// A project creation that did not complete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreationStatus {
    pub operation: OperationId,
    pub title: String,
    pub root: String,
    pub state: OperationState,
    /// Still running in this session (not interrupted).
    pub in_progress: bool,
    pub files: Vec<CreationFile>,
    pub created: u32,
    pub missing: u32,
    pub changed: u32,
    pub workflows_saved: u32,
    pub workflows_total: u32,
    /// The blueprint's entities and relations were applied to the graph.
    pub graph_applied: bool,
    /// The blueprint is still available, so the setup can be finished.
    pub record_available: bool,
    pub actions: Vec<CreationAction>,
    pub started_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreatedResource {
    pub path: WorkspacePath,
    pub entity: EntityId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreatedBlueprintEntity {
    pub temp_id: String,
    pub entity: EntityId,
    pub kind: EntityKind,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorkflow {
    pub temp_id: String,
    pub workflow: WorkflowId,
    pub revision: u64,
    pub entity: Option<EntityId>,
    pub state: DraftState,
}

/// What the graph step admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct GraphApplied {
    pub project: Option<ProjectId>,
    pub graph_revision: u64,
    pub resources: Vec<CreatedResource>,
    pub entities: Vec<CreatedBlueprintEntity>,
    pub workflows: Vec<CreatedWorkflow>,
    pub research_space: Option<EntityId>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_file(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 64 * 1024 * 1024 {
        return None;
    }
    fs::read(path).ok().map(|b| sha256_hex(&b))
}

fn io_err(code: &str, what: impl std::fmt::Display, e: &std::io::Error) -> GraphError {
    GraphError::new(code, format!("{what}: {e}"))
}

fn same_folder(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    canon(a) == canon(b)
}

fn join(root: &Path, path: &WorkspacePath) -> PathBuf {
    let mut out = root.to_path_buf();
    for c in path.components() {
        out.push(c);
    }
    out
}

/// Create `dir` (relative to the canonical `root`) one component at a time,
/// refusing links and anything that leads outside the project.
fn ensure_dir(root: &Path, dir: &[&str]) -> GraphResult<PathBuf> {
    let mut current = root.to_path_buf();
    for part in dir {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(GraphError::new(
                    "project.link",
                    format!("{} is a link; nothing was created through it", current.display()),
                ));
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(GraphError::new(
                    "project.collision",
                    format!("{} exists and is not a folder", current.display()),
                ));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => match fs::create_dir(&current) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_err("project.io", current.display(), &e)),
            },
            Err(e) => return Err(io_err("project.io", current.display(), &e)),
        }
        let canonical = fs::canonicalize(&current).map_err(|e| io_err("project.io", current.display(), &e))?;
        if !canonical.starts_with(root) {
            return Err(GraphError::new(
                "project.outside",
                format!("{} leads outside the project folder", current.display()),
            ));
        }
        // The folder must be the one named, not another one reached through
        // an alias (a Windows short name such as `EXPOSI~1`).
        let actual = canonical.file_name().map(|n| n.to_string_lossy().to_lowercase());
        if actual.as_deref() != Some(part.to_lowercase().as_str()) {
            return Err(GraphError::new(
                "project.alias",
                format!(
                    "{} names another folder ({}); nothing was created there",
                    current.display(),
                    canonical.display()
                ),
            ));
        }
    }
    Ok(current)
}

enum Written {
    Created,
    Exists,
}

/// Create one file with create-new semantics (never replacing anything).
fn create_file(root: &Path, path: &WorkspacePath, content: &[u8]) -> GraphResult<Written> {
    let parts: Vec<&str> = path.components().collect();
    let dir = ensure_dir(root, &parts[..parts.len() - 1])?;
    let target = dir.join(parts[parts.len() - 1]);
    let mut file = match fs::OpenOptions::new().write(true).create_new(true).open(&target) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Ok(Written::Exists),
        Err(e) => return Err(io_err("project.io", path, &e)),
    };
    file.write_all(content).map_err(|e| io_err("project.io", path, &e))?;
    file.sync_all().map_err(|e| io_err("project.io", path, &e))?;
    Ok(Written::Created)
}

/// Folders, and files with their content, a blueprint creates.
type Planned<'a> = (Vec<WorkspacePath>, Vec<(WorkspacePath, &'a str)>);

/// Every path a blueprint creates, validated (the blueprint must already be valid).
fn planned_paths(bp: &ProjectBlueprint) -> GraphResult<Planned<'_>> {
    let research = bp.research.as_ref().map(|r| r.id.as_str());
    let check = |raw: &str| {
        paths::check(raw, research).map_err(|m| GraphError::new("blueprint.invalid", format!("{raw}: {m}")))
    };
    let mut folders = Vec::new();
    if let Some(r) = &bp.research {
        folders
            .push(WorkspacePath::parse(&r.folder()).map_err(|e| GraphError::new("blueprint.invalid", e.to_string()))?);
    }
    for f in &bp.folders {
        folders.push(check(f)?);
    }
    let mut files = Vec::new();
    for f in &bp.files {
        files.push((check(&f.path)?, f.content.as_str()));
    }
    Ok((folders, files))
}

/// Refuse if any path the blueprint would create already exists (nothing is
/// written then). Existing folders are fine; existing files are collisions.
pub fn precheck(root: &Path, bp: &ProjectBlueprint) -> GraphResult<()> {
    let (folders, files) = planned_paths(bp)?;
    let mut collisions = Vec::new();
    for (p, _) in &files {
        if fs::symlink_metadata(join(root, p)).is_ok() {
            collisions.push(p.to_string());
        }
    }
    for p in &folders {
        if let Ok(meta) = fs::symlink_metadata(join(root, p))
            && !meta.is_dir()
        {
            collisions.push(p.to_string());
        }
    }
    if collisions.is_empty() {
        Ok(())
    } else {
        collisions.truncate(10);
        Err(GraphError::new(
            "project.collision",
            format!("These already exist and would not be replaced: {}. Nothing was created.", collisions.join(", ")),
        ))
    }
}

/// Who stands behind a blueprint's graph content. Only a built-in preset
/// asserts; agent proposals and imported blueprints only suggest (their
/// relations start as suggestions a person accepts or rejects), whatever
/// their self-declared `source` says.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Provenance {
    /// A built-in preset generated by this application.
    Preset,
    /// Proposed by an agent task.
    Agent { task_id: String },
    /// Any other blueprint (imported, pasted, or of unknown origin).
    #[default]
    Imported,
}

impl Provenance {
    fn origin(&self, bp: &ProjectBlueprint) -> Origin {
        match self {
            Provenance::Preset => blueprint_origin(bp),
            Provenance::Agent { task_id } => {
                Origin::Preset { blueprint: format!("agent-task:{}", crate::store::truncate(task_id, 48)) }
            }
            Provenance::Imported => Origin::Preset { blueprint: "import".into() },
        }
    }

    fn suggests(&self) -> bool {
        !matches!(self, Provenance::Preset)
    }
}

/// Project creation steps over one graph service and its operation journal.
pub struct Creator<'a> {
    pub graph: &'a GraphService,
    /// Application data (journal and creation records live here).
    pub data_dir: &'a Path,
}

impl Creator<'_> {
    fn record_path(&self, op: &OperationId) -> PathBuf {
        self.data_dir.join(RECORD_DIR).join(format!("{op}.json"))
    }

    pub fn record(&self, op: &OperationId) -> Option<CreationRecord> {
        let path = self.record_path(op);
        let meta = fs::metadata(&path).ok()?;
        if meta.len() > MAX_RECORD_BYTES {
            return None;
        }
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    /// Record the intent (journal + blueprint) before anything is written.
    /// `root` must be the canonical, already-checked destination.
    pub fn begin(&self, root: &Path, bp: &ProjectBlueprint, provenance: &Provenance) -> GraphResult<OperationId> {
        let plan = vec![
            "Create folders and files (never replacing existing files)".to_owned(),
            "Open the project".to_owned(),
            "Admit resources, save workflow drafts, apply the blueprint's entities and relations".to_owned(),
            "Record the graph revision".to_owned(),
        ];
        let description = format!("Create “{}” from {}", bp.title, bp.source);
        let journal = self.graph.journal();
        let record = journal
            .begin(OperationKind::ProjectCreation, root, None, &description, plan)
            .map_err(|e| GraphError::new("project.journal", e.to_string()))?;
        let creation = CreationRecord {
            operation: record.id.clone(),
            root: root.to_string_lossy().into_owned(),
            blueprint: bp.clone(),
            hashes: bp.files.iter().map(|f| (f.path.clone(), sha256_hex(f.content.as_bytes()))).collect(),
            created_at_ms: now_ms(),
            provenance: provenance.clone(),
        };
        let path = self.record_path(&record.id);
        let write = || -> std::io::Result<()> {
            fs::create_dir_all(path.parent().expect("record dir"))?;
            let tmp = path.with_extension("json.tmp");
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec(&creation).map_err(std::io::Error::other)?)?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            let _ = journal.fail(root, &record.id, &format!("The creation record could not be saved: {e}"));
            return Err(io_err("project.journal", "the creation record could not be saved", &e));
        }
        Ok(record.id)
    }

    /// Create the folders and files. `interrupt(n)` is consulted after each
    /// file (tests and debug builds simulate a crash there); when it returns
    /// true this stops with the operation still pending.
    pub fn write_files(
        &self,
        root: &Path,
        op: &OperationId,
        bp: &ProjectBlueprint,
        interrupt: &dyn Fn(usize) -> bool,
    ) -> GraphResult<Vec<WorkspacePath>> {
        let journal = self.graph.journal();
        let (folders, files) = planned_paths(bp)?;
        for folder in &folders {
            let parts: Vec<&str> = folder.components().collect();
            if let Err(e) = ensure_dir(root, &parts) {
                let _ = journal.needs_reconciliation(root, op, &e.message);
                return Err(e);
            }
        }
        let mut created = Vec::new();
        for (i, (path, content)) in files.iter().enumerate() {
            match create_file(root, path, content.as_bytes()) {
                Ok(Written::Created) => {
                    let mut refs = BTreeMap::new();
                    refs.insert("path".to_owned(), path.to_string());
                    refs.insert("sha256".to_owned(), sha256_hex(content.as_bytes()));
                    journal
                        .record_step(root, op, STEP_FILE, refs, true, None)
                        .map_err(|e| GraphError::new("project.journal", e.to_string()))?;
                    created.push(path.clone());
                }
                Ok(Written::Exists) => {
                    let message = format!(
                        "{path} appeared while the project was being created; it was left untouched and the setup stopped."
                    );
                    let _ = journal.needs_reconciliation(root, op, &message);
                    return Err(GraphError::new("project.collision", message));
                }
                Err(e) => {
                    let _ = journal.needs_reconciliation(root, op, &e.message);
                    return Err(e);
                }
            }
            if interrupt(i + 1) {
                return Err(GraphError::new("project.interrupted", "Project creation was interrupted."));
            }
        }
        Ok(created)
    }

    /// Note that the project was opened (graph attached).
    pub fn record_open(&self, root: &Path, op: &OperationId) -> GraphResult<()> {
        let mut refs = BTreeMap::new();
        if let Some(p) = self.graph.status().project {
            refs.insert("project".to_owned(), p.to_string());
        }
        self.graph
            .journal()
            .record_step(root, op, STEP_OPEN, refs, true, None)
            .map(|_| ())
            .map_err(|e| GraphError::new("project.journal", e.to_string()))
    }

    /// Admit the created resources, save workflow drafts, and apply the
    /// blueprint's entities and relations. Requires the project at `root` to
    /// be open with its graph attached. Idempotent: entities already created by
    /// this operation are reused, journaled workflow drafts are not saved
    /// again, entities this operation already created are referenced as they
    /// are (never overwritten), and duplicate relations are skipped.
    /// `provenance` decides whether relations are asserted (presets) or
    /// suggested (agents, imports).
    pub fn apply_graph(
        &self,
        root: &Path,
        op: &OperationId,
        bp: &ProjectBlueprint,
        provenance: &Provenance,
    ) -> GraphResult<GraphApplied> {
        let journal = self.graph.journal();
        let result = self.apply_graph_inner(root, op, bp, provenance);
        match &result {
            Ok(applied) => {
                let mut refs = BTreeMap::new();
                refs.insert("graphRevision".to_owned(), applied.graph_revision.to_string());
                refs.insert("entities".to_owned(), applied.entities.len().to_string());
                refs.insert("resources".to_owned(), applied.resources.len().to_string());
                journal
                    .record_step(root, op, STEP_GRAPH, refs, true, None)
                    .map_err(|e| GraphError::new("project.journal", e.to_string()))?;
            }
            Err(e) => {
                let _ = journal.record_step(root, op, STEP_GRAPH, BTreeMap::new(), false, Some(e.message.clone()));
                let _ = journal.needs_reconciliation(
                    root,
                    op,
                    &format!("The files were created but the project graph was not updated: {}", e.message),
                );
            }
        }
        result
    }

    fn apply_graph_inner(
        &self,
        root: &Path,
        op: &OperationId,
        bp: &ProjectBlueprint,
        provenance: &Provenance,
    ) -> GraphResult<GraphApplied> {
        let status = self.graph.status();
        let attached =
            matches!(status.state, GraphState::Attached) && self.graph.root().is_some_and(|r| same_folder(&r, root));
        if !attached {
            return Err(GraphError::unavailable(
                "The new project's graph is not open, so its structure could not be recorded.",
            ));
        }
        let origin = provenance.origin(bp);
        let (_, files) = planned_paths(bp)?;

        // Resources: one identity per created file (the same rule indexing uses).
        let mut resources: BTreeMap<String, EntityId> = BTreeMap::new();
        let mut created_resources = Vec::new();
        let mut hashes: BTreeMap<String, String> = BTreeMap::new();
        for (path, _) in &files {
            let Some(hash) = hash_file(&join(root, path)) else { continue };
            hashes.insert(path.to_string(), hash.clone());
            if !matches!(classify(path), Admission::Admit(_)) {
                continue;
            }
            let entity = self.graph.ensure_resource(path, Some(&hash), false, SERVICE)?;
            resources.insert(path.to_string(), entity.id.clone());
            created_resources.push(CreatedResource { path: path.clone(), entity: entity.id });
        }

        // Workflow drafts (journaled one by one, never saved twice).
        let saved_before: BTreeMap<String, WorkflowId> = journal_workflows(self.graph, root, op);
        let mut workflows = Vec::new();
        let exists = |p: &WorkspacePath| join(root, p).is_file();
        for w in &bp.workflows {
            let saved = match saved_before.get(&w.temp_id) {
                Some(id) => self.graph.workflow(id, None)?,
                None => {
                    let saved = self.graph.save_definition(
                        &origin,
                        &SaveWorkflowRequest { id: None, expected_revision: None, definition: w.definition.clone() },
                        &ValidationContext { ceiling: None, file_exists: Some(&exists) },
                        *provenance == Provenance::Imported,
                    )?;
                    let mut refs = BTreeMap::new();
                    refs.insert("tempId".to_owned(), w.temp_id.clone());
                    refs.insert("workflow".to_owned(), saved.id.to_string());
                    refs.insert("revision".to_owned(), saved.revision.to_string());
                    self.graph
                        .journal()
                        .record_step(root, op, STEP_WORKFLOW, refs, true, None)
                        .map_err(|e| GraphError::new("project.journal", e.to_string()))?;
                    saved
                }
            };
            workflows.push(CreatedWorkflow {
                temp_id: w.temp_id.clone(),
                workflow: saved.id.clone(),
                revision: saved.revision,
                entity: saved.entity.clone(),
                state: saved.validation.state,
            });
        }

        // Existing anchors.
        let (project_id, existing_research, keyed) = self.graph.with_store(|store| {
            let project = store
                .find_entities(Some(&[EntityKind::Project]), None, true, 1)?
                .0
                .into_iter()
                .next()
                .map(|e| e.id)
                .ok_or_else(|| GraphError::new("graph.storage", "the project entity is missing"))?;
            let research = match &bp.research {
                Some(r) => {
                    let mine = store
                        .entities_by_key(EntityKind::ResearchSpace, &r.folder())?
                        .into_iter()
                        .find(|e| e.lifecycle != Lifecycle::Tombstoned)
                        .map(|e| e.id);
                    if mine.is_none() {
                        let others = store.find_entities(Some(&[EntityKind::ResearchSpace]), None, false, 1)?.0;
                        if let Some(other) = others.first() {
                            return Err(GraphError::new(
                                "project.researchSpace",
                                format!(
                                    "The project already has a research space (“{}”); a project has one.",
                                    other.label
                                ),
                            ));
                        }
                    }
                    mine
                }
                None => None,
            };
            let mut keyed: BTreeMap<String, EntityId> = BTreeMap::new();
            for e in &bp.entities {
                if let Some(found) = store
                    .entities_by_key(e.kind, &entity_key(op, &e.temp_id))?
                    .into_iter()
                    .find(|x| x.lifecycle != Lifecycle::Tombstoned)
                {
                    keyed.insert(e.temp_id.clone(), found.id);
                }
            }
            for c in &bp.collections {
                if let Some(found) = store
                    .entities_by_key(EntityKind::Collection, &entity_key(op, &c.temp_id))?
                    .into_iter()
                    .find(|x| x.lifecycle != Lifecycle::Tombstoned)
                {
                    keyed.insert(c.temp_id.clone(), found.id);
                }
            }
            Ok((project, research, keyed))
        })?;

        let mut anchors = Anchors {
            project: BatchRef::Existing(project_id),
            research: existing_research.clone().map(BatchRef::Existing),
            files: resources.iter().map(|(p, id)| (p.clone(), BatchRef::Existing(id.clone()))).collect(),
            workflows: workflows
                .iter()
                .filter_map(|w| w.entity.clone().map(|e| (w.temp_id.clone(), BatchRef::Existing(e))))
                .collect(),
        };
        let mut prefix = Vec::new();
        let mut research_index = None;
        if let (Some(r), None) = (&bp.research, &existing_research) {
            prefix.push(research_upsert(r));
            anchors.research = Some(BatchRef::Batch(0));
            research_index = Some(0usize);
        }
        let batch = build_batch(bp, op, &anchors, prefix, &keyed, &hashes, provenance.suggests());
        let outcome = self.graph.apply_service_batch(&origin, &batch.batch)?;
        let research_space = match research_index {
            Some(i) => outcome.entity_ids.get(i).cloned(),
            None => existing_research,
        };
        let mut entities = Vec::new();
        for (temp, kind, label, slot) in &batch.order {
            let id = match slot {
                Slot::Batch(i) => outcome.entity_ids.get(*i),
                Slot::Existing(id) => Some(id),
            };
            if let Some(id) = id {
                entities.push(CreatedBlueprintEntity {
                    temp_id: temp.clone(),
                    entity: id.clone(),
                    kind: *kind,
                    label: label.clone(),
                });
            }
        }
        Ok(GraphApplied {
            project: self.graph.status().project,
            graph_revision: outcome.revision,
            resources: created_resources,
            entities,
            workflows,
            research_space,
        })
    }

    /// Record a completed app-level step (e.g. project settings).
    pub fn record_step(
        &self,
        root: &Path,
        op: &OperationId,
        name: &str,
        refs: BTreeMap<String, String>,
        ok: bool,
        message: Option<String>,
    ) {
        let _ = self.graph.journal().record_step(root, op, name, refs, ok, message);
    }

    pub fn complete(&self, root: &Path, op: &OperationId) -> GraphResult<()> {
        self.graph
            .journal()
            .complete(root, op)
            .map(|_| ())
            .map_err(|e| GraphError::new("project.journal", e.to_string()))
    }

    /// Mark a creation that stopped (after files may have been written) as
    /// needing reconciliation, with the reason.
    pub fn stopped(&self, root: &Path, op: &OperationId, message: &str) {
        let _ = self.graph.journal().needs_reconciliation(root, op, message);
    }

    /// Project creations of `root` that did not complete. Pending ones not
    /// in `running` were interrupted and are relabelled as needing
    /// reconciliation (never re-run).
    pub fn statuses(&self, root: &Path, running: &BTreeSet<OperationId>) -> Vec<CreationStatus> {
        let journal = self.graph.journal();
        let mut out = Vec::new();
        for mut rec in journal.list(root) {
            if rec.kind != OperationKind::ProjectCreation {
                continue;
            }
            let in_progress = running.contains(&rec.id);
            match rec.state {
                OperationState::Pending if !in_progress => {
                    if let Ok(r) = journal.needs_reconciliation(
                        root,
                        &rec.id,
                        "Exposition stopped before this project was fully created. Check what was created, then finish the setup or keep it as is.",
                    ) {
                        rec = r;
                    }
                }
                OperationState::Pending | OperationState::NeedsReconciliation { .. } => {}
                _ => continue,
            }
            let record = self.record(&rec.id);
            let mut files = Vec::new();
            let (mut created, mut missing, mut changed) = (0, 0, 0);
            let mut workflows_total = 0;
            if let Some(record) = &record {
                workflows_total = record.blueprint.workflows.len() as u32;
                for f in &record.blueprint.files {
                    let Ok(p) = WorkspacePath::parse(&f.path) else { continue };
                    let state = match hash_file(&join(root, &p)) {
                        None => CreationFileState::Missing,
                        Some(h) if record.hashes.get(&f.path) == Some(&h) => CreationFileState::Created,
                        Some(_) => CreationFileState::Changed,
                    };
                    match state {
                        CreationFileState::Created => created += 1,
                        CreationFileState::Missing => missing += 1,
                        CreationFileState::Changed => changed += 1,
                    }
                    files.push(CreationFile { path: f.path.clone(), state });
                }
            }
            let graph_applied = rec.steps.iter().any(|s| s.name == STEP_GRAPH && s.completed);
            let workflows_saved = rec.steps.iter().filter(|s| s.name == STEP_WORKFLOW && s.completed).count() as u32;
            let actions = if in_progress {
                Vec::new()
            } else if record.is_some() {
                vec![CreationAction::Finish, CreationAction::Keep]
            } else {
                vec![CreationAction::Keep]
            };
            out.push(CreationStatus {
                operation: rec.id.clone(),
                title: record.as_ref().map(|r| r.blueprint.title.clone()).unwrap_or_else(|| rec.description.clone()),
                root: display_root(&rec.project_root),
                state: rec.state.clone(),
                in_progress,
                files,
                created,
                missing,
                changed,
                workflows_saved,
                workflows_total,
                graph_applied,
                record_available: record.is_some(),
                actions,
                started_at_ms: rec.created_at_ms,
            });
        }
        out
    }

    fn unresolved_record(&self, root: &Path, op: &OperationId) -> GraphResult<crate::journal::OperationRecord> {
        let rec = self.graph.journal().get(root, op).map_err(|_| {
            GraphError::new("project.unknownOperation", "That project creation is not recorded for this folder.")
        })?;
        if rec.kind != OperationKind::ProjectCreation
            || !matches!(rec.state, OperationState::Pending | OperationState::NeedsReconciliation { .. })
        {
            return Err(GraphError::new("project.resolved", "That project creation needs no further action."));
        }
        Ok(rec)
    }

    /// Finish an interrupted creation (explicit user action): create only the
    /// files that are missing (existing ones, changed or not, are left
    /// alone), save the remaining workflow drafts, apply the graph content,
    /// and resolve the operation. The project must be open.
    pub fn finish(&self, root: &Path, op: &OperationId) -> GraphResult<(GraphApplied, ProjectBlueprint)> {
        self.unresolved_record(root, op)?;
        let record = self.record(op).ok_or_else(|| {
            GraphError::new("project.recordMissing", "The blueprint of this creation is no longer available.")
        })?;
        let bp = record.blueprint;
        // The record is application data, but check it as if it were not.
        let validation = super::validate_blueprint(&bp);
        if !validation.valid {
            return Err(GraphError::new(
                "blueprint.invalid",
                "The recorded blueprint of this creation is not valid any more; keep the project as it is instead.",
            ));
        }
        let (folders, files) = planned_paths(&bp)?;
        for folder in &folders {
            let parts: Vec<&str> = folder.components().collect();
            ensure_dir(root, &parts)?;
        }
        let mut recreated = 0;
        for (path, content) in &files {
            if fs::symlink_metadata(join(root, path)).is_ok() {
                continue;
            }
            if let Written::Created = create_file(root, path, content.as_bytes())? {
                recreated += 1;
                let mut refs = BTreeMap::new();
                refs.insert("path".to_owned(), path.to_string());
                refs.insert("sha256".to_owned(), sha256_hex(content.as_bytes()));
                refs.insert("recovery".to_owned(), "true".to_owned());
                let _ = self.graph.journal().record_step(root, op, STEP_FILE, refs, true, None);
            }
        }
        let applied = self.apply_graph(root, op, &bp, &record.provenance)?;
        self.graph
            .journal()
            .resolve(
                root,
                op,
                &format!(
                    "Finished after the interruption: {recreated} missing file(s) created; the graph was updated."
                ),
            )
            .map_err(|e| GraphError::new("project.journal", e.to_string()))?;
        Ok((applied, bp))
    }

    /// Keep an interrupted creation as it is (explicit user action): the files
    /// that exist are admitted as resources when the graph is open; nothing is
    /// created or applied.
    pub fn keep(&self, root: &Path, op: &OperationId) -> GraphResult<u32> {
        self.unresolved_record(root, op)?;
        let mut admitted = 0;
        if let Some(record) = self.record(op)
            && self.graph.root().is_some_and(|r| same_folder(&r, root))
        {
            for f in &record.blueprint.files {
                let Ok(p) = WorkspacePath::parse(&f.path) else { continue };
                let Some(hash) = hash_file(&join(root, &p)) else { continue };
                if matches!(classify(&p), Admission::Admit(_))
                    && self.graph.ensure_resource(&p, Some(&hash), false, SERVICE).is_ok()
                {
                    admitted += 1;
                }
            }
        }
        self.graph
            .journal()
            .resolve(root, op, "Kept as created; the rest of the setup was not applied.")
            .map_err(|e| GraphError::new("project.journal", e.to_string()))?;
        Ok(admitted)
    }
}

/// A folder path as people read it (without the verbatim `\\?\` prefix).
fn display_root(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    raw.strip_prefix(r"\\?\").unwrap_or(raw).to_owned()
}

fn journal_workflows(graph: &GraphService, root: &Path, op: &OperationId) -> BTreeMap<String, WorkflowId> {
    let Ok(rec) = graph.journal().get(root, op) else { return BTreeMap::new() };
    rec.steps
        .iter()
        .filter(|s| s.name == STEP_WORKFLOW && s.completed)
        .filter_map(|s| Some((s.refs.get("tempId")?.clone(), WorkflowId::parse(s.refs.get("workflow")?).ok()?)))
        .collect()
}

/// Natural key of an entity a blueprint operation created (idempotency aid).
fn entity_key(op: &OperationId, temp: &str) -> String {
    format!("blueprint:{op}/{temp}")
}

fn research_upsert(r: &super::BlueprintResearch) -> EntityUpsert {
    let mut attributes = BTreeMap::new();
    attributes.insert("research.id".to_owned(), r.id.clone());
    attributes.insert("research.folder".to_owned(), r.folder());
    EntityUpsert {
        id: None,
        kind: EntityKind::ResearchSpace,
        roles: Vec::new(),
        label: r.label.clone(),
        key: Some(r.folder()),
        attributes,
        lifecycle: None,
        tombstone_reason: None,
        bindings: None,
    }
}

struct Anchors {
    project: BatchRef,
    research: Option<BatchRef>,
    files: BTreeMap<String, BatchRef>,
    workflows: BTreeMap<String, BatchRef>,
}

/// Where a blueprint entity's id comes from after the batch.
enum Slot {
    /// Index in the batch's entities (its new id is in the outcome).
    Batch(usize),
    /// Already created by this operation.
    Existing(EntityId),
}

struct BuiltBatch {
    batch: ServiceBatch,
    /// (temp id, kind, label, slot) of the blueprint entities and collections.
    order: Vec<(String, EntityKind, String, Slot)>,
}

fn build_batch(
    bp: &ProjectBlueprint,
    op: &OperationId,
    anchors: &Anchors,
    prefix: Vec<EntityUpsert>,
    existing: &BTreeMap<String, EntityId>,
    hashes: &BTreeMap<String, String>,
    suggested: bool,
) -> BuiltBatch {
    let mut entities = prefix;
    let mut temps: BTreeMap<String, BatchRef> = BTreeMap::new();
    let mut order = Vec::new();
    for e in &bp.entities {
        // Created earlier by this operation (interrupted after the graph
        // step): reference it as it is now, never overwrite later edits.
        if let Some(id) = existing.get(&e.temp_id) {
            temps.insert(e.temp_id.clone(), BatchRef::Existing(id.clone()));
            order.push((e.temp_id.clone(), e.kind, e.label.clone(), Slot::Existing(id.clone())));
            continue;
        }
        let bindings = e.source.as_ref().and_then(|src| {
            let path = WorkspacePath::parse(src).ok()?;
            let version =
                hashes.get(src).map(|h| SourceVersion { content_hash: h.clone(), revision: None, saved: true });
            let state = if version.is_some() { BindingState::Resolved } else { BindingState::Missing };
            Some(vec![SourceBinding {
                entity: EntityId::mint(),
                path,
                selector: SourceSelector::WholeFile,
                range: None,
                version,
                fingerprint: None,
                detail: (state == BindingState::Missing).then(|| "The file was not created.".to_owned()),
                state,
            }])
        });
        temps.insert(e.temp_id.clone(), BatchRef::Batch(entities.len()));
        order.push((e.temp_id.clone(), e.kind, e.label.clone(), Slot::Batch(entities.len())));
        entities.push(EntityUpsert {
            id: None,
            kind: e.kind,
            roles: Vec::new(),
            label: e.label.trim().to_owned(),
            key: Some(entity_key(op, &e.temp_id)),
            attributes: e.attributes.clone(),
            lifecycle: None,
            tombstone_reason: None,
            bindings,
        });
    }
    for c in &bp.collections {
        if let Some(id) = existing.get(&c.temp_id) {
            temps.insert(c.temp_id.clone(), BatchRef::Existing(id.clone()));
            order.push((c.temp_id.clone(), EntityKind::Collection, c.label.clone(), Slot::Existing(id.clone())));
            continue;
        }
        temps.insert(c.temp_id.clone(), BatchRef::Batch(entities.len()));
        order.push((c.temp_id.clone(), EntityKind::Collection, c.label.clone(), Slot::Batch(entities.len())));
        entities.push(EntityUpsert {
            id: None,
            kind: EntityKind::Collection,
            roles: Vec::new(),
            label: c.label.trim().to_owned(),
            key: Some(entity_key(op, &c.temp_id)),
            attributes: BTreeMap::new(),
            lifecycle: None,
            tombstone_reason: None,
            bindings: None,
        });
    }
    let resolve = |r: &BlueprintRef| -> Option<BatchRef> {
        match r {
            BlueprintRef::Temp { temp_id } => {
                temps.get(temp_id).cloned().or_else(|| anchors.workflows.get(temp_id).cloned())
            }
            BlueprintRef::File { path } => anchors.files.get(path).cloned(),
            BlueprintRef::ResearchSpace => anchors.research.clone(),
            BlueprintRef::Project => Some(anchors.project.clone()),
        }
    };
    let status = suggested.then_some(AssertionStatus::Suggested);
    let mut relations = Vec::new();
    for c in &bp.collections {
        let Some(collection) = temps.get(&c.temp_id).cloned() else { continue };
        for m in &c.members {
            if let Some(member) = resolve(m) {
                relations.push(RelationUpsert {
                    kind: RelationKind::MemberOf,
                    from: member,
                    to: collection.clone(),
                    status,
                    evidence_kind: None,
                    evidence: Vec::new(),
                    note: None,
                });
            }
        }
    }
    for r in &bp.relations {
        if let (Some(from), Some(to)) = (resolve(&r.from), resolve(&r.to)) {
            relations.push(RelationUpsert {
                kind: r.kind,
                from,
                to,
                status,
                evidence_kind: None,
                evidence: Vec::new(),
                note: r.note.clone(),
            });
        }
    }
    BuiltBatch {
        batch: ServiceBatch {
            entities,
            relations,
            summary: format!("Create project “{}” from blueprint ({op})", crate::store::truncate(&bp.title, 120)),
            ..Default::default()
        },
        order,
    }
}

/// Run the blueprint's graph content through the graph validator in an
/// in-memory graph (nothing persists). Catches anything the static checks
/// missed, with the exact rules the real application uses.
pub(super) fn dry_run_graph(bp: &ProjectBlueprint, files: &BTreeMap<String, WorkspacePath>) -> Result<(), String> {
    let mut store = GraphStore::open_in_memory().map_err(|e| e.to_string())?;
    let w = Writer::new(store.transaction().map_err(|e| e.to_string())?);
    let origin = blueprint_origin(bp);
    let mut prefix = vec![EntityUpsert {
        id: None,
        kind: EntityKind::Project,
        roles: Vec::new(),
        label: "Project".into(),
        key: None,
        attributes: BTreeMap::new(),
        lifecycle: None,
        tombstone_reason: None,
        bindings: None,
    }];
    let mut anchors =
        Anchors { project: BatchRef::Batch(0), research: None, files: BTreeMap::new(), workflows: BTreeMap::new() };
    if let Some(r) = &bp.research {
        anchors.research = Some(BatchRef::Batch(prefix.len()));
        prefix.push(research_upsert(r));
    }
    for (raw, path) in files {
        if let Admission::Admit(roles) = classify(path) {
            anchors.files.insert(raw.clone(), BatchRef::Batch(prefix.len()));
            prefix.push(EntityUpsert {
                id: None,
                kind: EntityKind::Resource,
                roles,
                label: crate::store::truncate(path.file_name(), 200),
                key: Some(path.to_string()),
                attributes: BTreeMap::new(),
                lifecycle: None,
                tombstone_reason: None,
                bindings: None,
            });
        }
    }
    for wf in &bp.workflows {
        anchors.workflows.insert(wf.temp_id.clone(), BatchRef::Batch(prefix.len()));
        prefix.push(EntityUpsert {
            id: None,
            kind: EntityKind::Workflow,
            roles: Vec::new(),
            label: "Workflow".into(),
            key: None,
            attributes: BTreeMap::new(),
            lifecycle: None,
            tombstone_reason: None,
            bindings: None,
        });
    }
    let op = OperationId::mint();
    let hashes: BTreeMap<String, String> = files.keys().map(|k| (k.clone(), "0".repeat(64))).collect();
    let built = build_batch(bp, &op, &anchors, prefix, &BTreeMap::new(), &hashes, false);
    crate::validate::run_batch(&w, &origin, &built.batch).map(|_| ())
    // `w` is dropped without commit.
}

#[cfg(all(test, windows))]
mod alias_tests {
    use super::*;

    /// The 8.3 short name Windows assigned to `dir/name`, if the volume makes them.
    fn short_name(dir: &Path, name: &str) -> Option<String> {
        let out = std::process::Command::new("cmd").args(["/c", "dir", "/x", "/a:d"]).current_dir(dir).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .filter(|l| l.trim_end().ends_with(name))
            .find_map(|l| l.split_whitespace().rev().nth(1).filter(|s| s.contains('~')).map(str::to_owned))
    }

    #[test]
    fn folders_reached_through_a_short_name_alias_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(root.join(".exposition")).unwrap();
        let Some(alias) = short_name(&root, ".exposition") else {
            eprintln!("SKIPPED: this volume does not create short (8.3) names");
            return;
        };
        let err = ensure_dir(&root, &[alias.as_str()]).unwrap_err();
        assert_eq!(err.code, "project.alias", "{}", err.message);
        // The ordinary name is fine.
        assert!(ensure_dir(&root, &[".exposition", "research"]).is_ok());
    }
}
