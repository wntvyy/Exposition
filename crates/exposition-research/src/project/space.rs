//! The research space: one typed container per project and its managed
//! folders under `.exposition/research/<space-id>/`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use exposition_core::path::WorkspacePath;
use exposition_graph::{
    BatchRef, Entity, EntityId, EntityKind, EntityUpsert, Lifecycle, RelationKind, RelationUpsert, ResourceRole,
    ServiceBatch,
};
use exposition_workspace::WorkspaceService;

use super::types::{
    CreateSpaceFileRequest, ExperimentSummary, ExternalReference, ImportMode, ImportOrigin, ImportOutcome,
    ImportRequest, ResearchListing, ResearchSpaceInfo, RetainedResult, SpaceFile, SpaceGroup, WorkflowSummary,
};
use super::{ResearchProject, label, now_ms, origin};
use crate::error::ResearchError;
use crate::store::ensure_dir_within;

/// Where research spaces live inside a project.
pub const RESEARCH_DIR: &str = ".exposition/research";
/// Files listed per group.
const LIST_LIMIT: usize = 1000;
/// Folder depth listed under a group.
const LIST_DEPTH: usize = 6;
/// Largest file imported by copy.
const IMPORT_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
/// Resources admitted while listing (the rest on the next listing).
const ADMIT_PER_LISTING: usize = 200;

pub(crate) fn attr(entity: &Entity, key: &str) -> Option<String> {
    entity.attributes.get(key).cloned()
}

fn space_info(entity: &Entity) -> Option<ResearchSpaceInfo> {
    let space_id = entity.key.clone()?;
    let root = WorkspacePath::parse(&format!("{RESEARCH_DIR}/{space_id}")).ok()?;
    Some(ResearchSpaceInfo {
        entity: entity.id.clone(),
        space_id,
        label: entity.label.clone(),
        root,
        created_at_ms: entity.created_at_ms,
    })
}

fn valid_space_id(id: &str) -> bool {
    id.len() <= 40 && id.starts_with("rs-") && id[3..].chars().all(|c| c.is_ascii_hexdigit()) && id.len() > 3
}

fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
}

fn origin_of(entity: &Entity) -> Option<ImportOrigin> {
    let mode = match attr(entity, "import.mode")?.as_str() {
        "copy" => ImportMode::Copy,
        "reference" => ImportMode::Reference,
        _ => return None,
    };
    Some(ImportOrigin {
        source: attr(entity, "import.source")?,
        content_hash: attr(entity, "import.hash").unwrap_or_default(),
        size_bytes: attr(entity, "import.size").and_then(|s| s.parse().ok()).unwrap_or(0),
        mode,
        imported_at_ms: attr(entity, "import.at").and_then(|s| s.parse().ok()).unwrap_or(0),
    })
}

fn import_attributes(origin: &ImportOrigin) -> BTreeMap<String, String> {
    let mut a = BTreeMap::new();
    let mut source = origin.source.clone();
    if source.len() > 4000 {
        source.truncate(4000);
    }
    a.insert("import.source".into(), source);
    a.insert("import.hash".into(), origin.content_hash.clone());
    a.insert("import.size".into(), origin.size_bytes.to_string());
    a.insert("import.at".into(), origin.imported_at_ms.to_string());
    a.insert(
        "import.mode".into(),
        match origin.mode {
            ImportMode::Copy => "copy".into(),
            ImportMode::Reference => "reference".into(),
        },
    );
    a
}

/// Files of one group folder (bounded, links never followed).
fn walk(root: &Path, group_path: &WorkspacePath, group: SpaceGroup, out: &mut Vec<SpaceFile>, truncated: &mut bool) {
    let mut stack: Vec<(WorkspacePath, usize)> = vec![(group_path.clone(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let abs = dir.components().fold(root.to_path_buf(), |p, c| p.join(c));
        let Ok(entries) = std::fs::read_dir(&abs) else { continue };
        let mut names: Vec<(String, std::fs::FileType, Option<u64>)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_owned();
                let meta = std::fs::symlink_metadata(e.path()).ok()?;
                Some((name, meta.file_type(), meta.is_file().then_some(meta.len())))
            })
            .collect();
        names.sort_by_key(|n| n.0.to_lowercase());
        for (name, file_type, size) in names {
            if name.starts_with('.') || name.contains(".exposition-save-") || file_type.is_symlink() {
                continue;
            }
            let Ok(path) = dir.join(&name) else { continue };
            if file_type.is_dir() {
                if depth + 1 < LIST_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if out.len() >= LIST_LIMIT {
                *truncated = true;
                return;
            }
            out.push(SpaceFile { path, name, group, size_bytes: size, entity: None, roles: Vec::new(), origin: None });
        }
    }
}

impl ResearchProject {
    /// The project's research space, if Research is enabled.
    pub fn space(&self) -> Result<Option<ResearchSpaceInfo>, ResearchError> {
        let spaces =
            self.graph.with_store(|s| Ok(s.find_entities(Some(&[EntityKind::ResearchSpace]), None, false, 8)?.0))?;
        Ok(spaces.iter().filter(|e| e.key.as_deref().is_some_and(valid_space_id)).find_map(space_info))
    }

    /// Enable Research: create the project's one research space (a typed
    /// container contained by the project) and its folders. Idempotent.
    /// Launches no code and needs no manuscript.
    pub fn enable(&self) -> Result<ResearchSpaceInfo, ResearchError> {
        let _guard = self.enable.lock().unwrap_or_else(|p| p.into_inner());
        let root = self.graph_root()?;
        let info = match self.space()? {
            Some(info) => info,
            None => {
                let project = self
                    .graph
                    .with_store(|s| Ok(s.find_entities(Some(&[EntityKind::Project]), None, true, 1)?.0))?
                    .into_iter()
                    .next()
                    .ok_or_else(|| ResearchError::coded("graph.storage", "the project graph has no project entity"))?;
                let space_id = format!("rs-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
                let mut attributes = BTreeMap::new();
                attributes.insert("research.path".into(), format!("{RESEARCH_DIR}/{space_id}"));
                let batch = ServiceBatch {
                    entities: vec![EntityUpsert {
                        id: None,
                        kind: EntityKind::ResearchSpace,
                        roles: Vec::new(),
                        label: "Research".into(),
                        key: Some(space_id.clone()),
                        attributes,
                        lifecycle: None,
                        tombstone_reason: None,
                        bindings: None,
                    }],
                    relations: vec![RelationUpsert {
                        kind: RelationKind::Contains,
                        from: BatchRef::Existing(project.id),
                        to: BatchRef::Batch(0),
                        status: None,
                        evidence_kind: None,
                        evidence: Vec::new(),
                        note: None,
                    }],
                    summary: "Enable Research".into(),
                    ..Default::default()
                };
                let outcome = self.graph.apply_service_batch(&origin(), &batch)?;
                let id = outcome
                    .entity_ids
                    .first()
                    .cloned()
                    .ok_or_else(|| ResearchError::coded("graph.storage", "the research space was not created"))?;
                space_info(&self.entity(&id)?)
                    .ok_or_else(|| ResearchError::coded("graph.storage", "the research space is invalid"))?
            }
        };
        // Folders after the entity: a failure here leaves the identity, and
        // enabling again creates the folders under the same id.
        for group in SpaceGroup::ALL {
            ensure_dir_within(&root, &info.root.join(group.dir()).expect("static name"))?;
        }
        Ok(info)
    }

    fn group_path(info: &ResearchSpaceInfo, group: SpaceGroup) -> WorkspacePath {
        info.root.join(group.dir()).expect("static name")
    }

    fn require_space(&self) -> Result<ResearchSpaceInfo, ResearchError> {
        self.space()?
            .ok_or_else(|| ResearchError::coded("research.notEnabled", "Research is not enabled for this project."))
    }

    /// Admit a research file (if needed) and make it a member of the space.
    fn admit_member(
        &self,
        space: &ResearchSpaceInfo,
        path: &WorkspacePath,
        hash: Option<&str>,
    ) -> Result<Entity, ResearchError> {
        let entity = self.graph.ensure_resource(path, hash, false, super::SERVICE)?;
        self.add_relations(
            vec![(RelationKind::MemberOf, entity.id.clone(), space.entity.clone(), None, None)],
            &format!("Add {path} to the research space"),
        )?;
        Ok(entity)
    }

    /// Everything the Research explorer shows. Lists the space's folders as
    /// they are on disk (works while the graph is unavailable, without
    /// identities); admits research files the graph does not know yet.
    pub fn listing(&self) -> Result<ResearchListing, ResearchError> {
        let root = self.root()?;
        let usable = self.graph_root();
        let mut listing = ResearchListing {
            space: None,
            sources: Vec::new(),
            data: Vec::new(),
            notes: Vec::new(),
            references: Vec::new(),
            experiments: Vec::new(),
            results: Vec::new(),
            workflows: Vec::new(),
            truncated: false,
            graph_revision: None,
            graph_unavailable: None,
        };
        let space = match &usable {
            Ok(_) => self.space()?,
            Err(e) => {
                listing.graph_unavailable = Some(e.to_string());
                // Without the graph, a single research folder on disk is still listed.
                let dir = root.join(".exposition").join("research");
                let ids: Vec<String> = std::fs::read_dir(dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                    .filter(|n| valid_space_id(n))
                    .collect();
                match ids.as_slice() {
                    [only] => WorkspacePath::parse(&format!("{RESEARCH_DIR}/{only}")).ok().map(|p| ResearchSpaceInfo {
                        entity: EntityId::parse("ent-unavailable").expect("valid"),
                        space_id: only.clone(),
                        label: "Research".into(),
                        root: p,
                        created_at_ms: 0,
                    }),
                    _ => None,
                }
            }
        };
        let Some(space) = space else { return Ok(listing) };
        let mut truncated = false;
        let mut files = Vec::new();
        for group in SpaceGroup::ALL {
            walk(&root, &Self::group_path(&space, group), group, &mut files, &mut truncated);
        }
        listing.truncated = truncated;
        if usable.is_ok() {
            let mut admitted = 0;
            for file in &mut files {
                let existing = self.graph.resource(&file.path)?;
                let entity = match existing {
                    Some(e) => Some(e),
                    None if admitted < ADMIT_PER_LISTING => {
                        admitted += 1;
                        self.admit_member(&space, &file.path, None).ok()
                    }
                    None => None,
                };
                if let Some(e) = entity {
                    file.origin = origin_of(&e);
                    file.roles = e.roles.clone();
                    file.entity = Some(e.id);
                }
            }
            listing.graph_revision = Some(self.graph.with_store(|s| Ok(s.revision()?))?);
            self.list_graph_groups(&space, &mut listing)?;
        }
        for file in files {
            match file.group {
                SpaceGroup::Sources => listing.sources.push(file),
                SpaceGroup::Data => listing.data.push(file),
                SpaceGroup::Notes => listing.notes.push(file),
            }
        }
        listing.space = Some(space);
        Ok(listing)
    }

    fn list_graph_groups(&self, space: &ResearchSpaceInfo, listing: &mut ResearchListing) -> Result<(), ResearchError> {
        // External references and experiments that are members of the space.
        let members: Vec<Entity> = self.graph.with_store(|s| {
            let ids: Vec<EntityId> = s
                .relations_to(&space.entity)?
                .into_iter()
                .filter(|r| r.kind == RelationKind::MemberOf || r.kind == RelationKind::Contains)
                .map(|r| r.from)
                .collect();
            Ok(s.entities(&ids)?)
        })?;
        for e in &members {
            if e.lifecycle == Lifecycle::Tombstoned {
                continue;
            }
            match e.kind {
                EntityKind::ExternalResource => {
                    if let Some(origin) = origin_of(e) {
                        listing.references.push(ExternalReference {
                            entity: e.id.clone(),
                            label: e.label.clone(),
                            origin,
                        });
                    }
                }
                EntityKind::Experiment => listing.experiments.push(experiment_summary(e)),
                _ => {}
            }
        }
        listing.experiments.sort_by_key(|e| e.label.to_lowercase());
        // Retained results: every resource with the artifact role.
        for e in self.entities_of(&[EntityKind::Resource], 5000)? {
            if e.lifecycle == Lifecycle::Tombstoned || !e.roles.contains(&ResourceRole::Artifact) {
                continue;
            }
            listing.results.push(self.retained_result(&e)?);
        }
        let rank = |l: Lifecycle| match l {
            Lifecycle::Active => 0,
            Lifecycle::Archived => 1,
            Lifecycle::Discarded => 2,
            Lifecycle::Tombstoned => 3,
        };
        listing.results.sort_by(|a, b| rank(a.lifecycle).cmp(&rank(b.lifecycle)).then(b.path.cmp(&a.path)));
        for e in self.entities_of(&[EntityKind::Workflow], 500)? {
            if e.lifecycle != Lifecycle::Tombstoned {
                listing.workflows.push(WorkflowSummary { entity: e.id, label: e.label, lifecycle: e.lifecycle });
            }
        }
        Ok(())
    }

    pub(crate) fn retained_result(&self, e: &Entity) -> Result<RetainedResult, ResearchError> {
        let (incoming, labels) = self.graph.with_store(|s| {
            let incoming = s.relations_to(&e.id)?;
            let outgoing = s.relations_from(&e.id)?;
            let mut ids: Vec<EntityId> = incoming.iter().map(|r| r.from.clone()).collect();
            ids.extend(outgoing.iter().map(|r| r.to.clone()));
            let mut labels: HashMap<EntityId, Entity> = HashMap::new();
            for n in s.entities(&ids)? {
                labels.insert(n.id.clone(), n);
            }
            Ok((incoming.into_iter().chain(outgoing).collect::<Vec<_>>(), labels))
        })?;
        let used_by = incoming
            .iter()
            .filter(|r| r.kind == RelationKind::UsesAsset && r.to == e.id)
            .filter_map(|r| {
                let n = labels.get(&r.from)?;
                Some(super::types::ResultUse {
                    entity: n.id.clone(),
                    label: n.label.clone(),
                    path: n.key.as_deref().and_then(|k| WorkspacePath::parse(k).ok()),
                    relation: r.id.clone(),
                })
            })
            .collect();
        let experiments = incoming
            .iter()
            .filter(|r| r.kind == RelationKind::DerivedFrom && r.from == e.id)
            .filter(|r| labels.get(&r.to).is_some_and(|n| n.kind == EntityKind::Experiment))
            .map(|r| r.to.clone())
            .collect();
        let path = e
            .key
            .as_deref()
            .and_then(|k| WorkspacePath::parse(k).ok())
            .ok_or_else(|| ResearchError::coded("graph.storage", format!("{} has no path", e.label)))?;
        Ok(RetainedResult {
            entity: e.id.clone(),
            label: e.label.clone(),
            path,
            lifecycle: e.lifecycle,
            roles: e.roles.clone(),
            run_id: attr(e, "retained.run"),
            artifact_id: attr(e, "retained.artifact"),
            kind: attr(e, "retained.kind"),
            media_type: attr(e, "retained.mediaType"),
            content_hash: attr(e, "retained.contentHash"),
            used_by,
            experiments,
        })
    }

    /// Create a new, empty research file through the document service (it
    /// opens as a document) and make it a member of the space.
    pub fn create_file(
        &self,
        workspace: &WorkspaceService,
        request: &CreateSpaceFileRequest,
    ) -> Result<SpaceFile, ResearchError> {
        let space = self.require_space()?;
        let _ = self.graph_root()?;
        if !valid_file_name(&request.name) {
            return Err(ResearchError::InvalidRequest(format!("{:?} is not a valid file name", request.name)));
        }
        let path = Self::group_path(&space, request.group)
            .join(&request.name)
            .map_err(|e| ResearchError::InvalidRequest(e.to_string()))?;
        if matches!(exposition_graph::classify(&path), exposition_graph::Admission::Exclude(_)) {
            return Err(ResearchError::InvalidRequest(format!(
                "{} is not a research file type Exposition recognises (code, data, or notes)",
                request.name
            )));
        }
        workspace.create_document(&path, "").map_err(|e| ResearchError::coded(e.code, e.message))?;
        let entity = self.admit_member(&space, &path, Some(&crate::helper::sha256_hex(b"")))?;
        Ok(SpaceFile {
            name: request.name.clone(),
            group: request.group,
            size_bytes: Some(0),
            roles: entity.roles.clone(),
            entity: Some(entity.id),
            origin: None,
            path,
        })
    }

    /// Import a file the person chose: an explicit copy into the space (the
    /// copy is a project file with its recorded origin), or a read-only
    /// external reference (never crawled or executed).
    pub fn import(&self, request: &ImportRequest) -> Result<ImportOutcome, ResearchError> {
        let space = self.require_space()?;
        let root = self.graph_root()?;
        let source = std::path::PathBuf::from(&request.source);
        if !source.is_absolute() {
            return Err(ResearchError::InvalidRequest("choose a file by its full location".into()));
        }
        let meta = std::fs::symlink_metadata(&source)
            .map_err(|e| ResearchError::InvalidRequest(format!("{} cannot be read: {e}", request.source)))?;
        if !meta.is_file() {
            return Err(ResearchError::InvalidRequest(format!("{} is not a regular file", request.source)));
        }
        let canonical = std::fs::canonicalize(&source).map_err(|e| ResearchError::io("resolving the file", e))?;
        if canonical.starts_with(root.join(".exposition")) {
            return Err(ResearchError::InvalidRequest("that file is part of Exposition's project metadata".into()));
        }
        let name = canonical
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| valid_file_name(n))
            .ok_or_else(|| ResearchError::InvalidRequest("the file name cannot be used in a project".into()))?
            .to_owned();
        let hash = crate::service::hash_file(&canonical).map_err(|e| ResearchError::io("reading the file", e))?;
        let origin_record = ImportOrigin {
            source: crate::service::display_path(&canonical),
            content_hash: hash.clone(),
            size_bytes: meta.len(),
            mode: request.mode,
            imported_at_ms: now_ms(),
        };
        match request.mode {
            ImportMode::Reference => {
                let batch = ServiceBatch {
                    entities: vec![EntityUpsert {
                        id: None,
                        kind: EntityKind::ExternalResource,
                        roles: Vec::new(),
                        label: label(&name),
                        key: Some(origin_record.source.clone()),
                        attributes: import_attributes(&origin_record),
                        lifecycle: None,
                        tombstone_reason: None,
                        bindings: None,
                    }],
                    relations: vec![RelationUpsert {
                        kind: RelationKind::MemberOf,
                        from: BatchRef::Batch(0),
                        to: BatchRef::Existing(space.entity.clone()),
                        status: None,
                        evidence_kind: None,
                        evidence: Vec::new(),
                        note: None,
                    }],
                    summary: format!("Reference {name} (read-only)"),
                    ..Default::default()
                };
                let outcome = self.graph.apply_service_batch(&origin(), &batch)?;
                let entity = outcome.entity_ids.into_iter().next().expect("one entity");
                Ok(ImportOutcome::Referenced {
                    reference: ExternalReference { entity, label: label(&name), origin: origin_record },
                })
            }
            ImportMode::Copy => {
                if meta.len() > IMPORT_LIMIT {
                    return Err(ResearchError::InvalidRequest("the file is too large to copy into the project".into()));
                }
                let group_dir = ensure_dir_within(&root, &Self::group_path(&space, request.group))?;
                let (target_name, target) = unique_target(&group_dir, &name)?;
                copy_new(&canonical, &target)?;
                let copied =
                    crate::service::hash_file(&target).map_err(|e| ResearchError::io("verifying the copy", e))?;
                if copied != hash {
                    let _ = std::fs::remove_file(&target);
                    return Err(ResearchError::InvalidRequest(
                        "the file changed while it was copied; try again".into(),
                    ));
                }
                let path = Self::group_path(&space, request.group)
                    .join(&target_name)
                    .map_err(|e| ResearchError::InvalidRequest(e.to_string()))?;
                let entity = self.admit_member(&space, &path, Some(&hash))?;
                let mut attributes = entity.attributes.clone();
                attributes.extend(import_attributes(&origin_record));
                self.update_entity(&entity, Some(attributes), None, &format!("Record the origin of {path}"))?;
                Ok(ImportOutcome::Copied {
                    file: SpaceFile {
                        path,
                        name: target_name,
                        group: request.group,
                        size_bytes: Some(meta.len()),
                        entity: Some(entity.id),
                        roles: entity.roles,
                        origin: Some(origin_record),
                    },
                })
            }
        }
    }
}

pub(crate) fn experiment_summary(e: &Entity) -> ExperimentSummary {
    ExperimentSummary {
        entity: e.id.clone(),
        label: e.label.clone(),
        lifecycle: e.lifecycle,
        revision: e.revision,
        objective: attr(e, "objective").unwrap_or_default(),
        procedure: attr(e, "procedure").and_then(|p| WorkspacePath::parse(&p).ok()),
    }
}

/// A file name in `dir` that does not exist yet (`name`, `name (2)`, ...).
fn unique_target(dir: &Path, name: &str) -> Result<(String, std::path::PathBuf), ResearchError> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.to_owned(), String::new()),
    };
    for n in 1..1000 {
        let candidate = if n == 1 { name.to_owned() } else { format!("{stem} ({n}){ext}") };
        let path = dir.join(&candidate);
        if std::fs::symlink_metadata(&path).is_err() {
            return Ok((candidate, path));
        }
    }
    Err(ResearchError::InvalidRequest(format!("too many files named like {name}")))
}

/// Copy into a new file (never replacing one).
fn copy_new(from: &Path, to: &Path) -> Result<(), ResearchError> {
    let mut input = std::fs::File::open(from).map_err(|e| ResearchError::io("reading the file", e))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| ResearchError::io("creating the copy", e))?;
    if let Err(e) = std::io::copy(&mut input, &mut output).and_then(|_| output.sync_all()) {
        drop(output);
        let _ = std::fs::remove_file(to);
        return Err(ResearchError::io("copying the file", e));
    }
    Ok(())
}

/// For tests: the graph's research spaces.
#[cfg(test)]
pub(crate) fn spaces(graph: &exposition_graph::GraphService) -> Vec<Entity> {
    graph.with_store(|s| Ok(s.find_entities(Some(&[EntityKind::ResearchSpace]), None, true, 10)?.0)).unwrap()
}
