//! Versioned project blueprints (`exposition.blueprint/1`).
//!
//! A blueprint declares a project's purpose, authored files and folders, an
//! optional research space, typed entities and relations (the closed graph
//! vocabulary, with temporary references bound to the resources actually
//! created), collections, optional workflow drafts, required capabilities,
//! LaTeX/Typst build targets, and what to open first. It cannot carry
//! authority grants, trust, execution, or startup hooks: the schema has no
//! such fields (unknown fields are refused) and its paths may not create files
//! that tools run automatically.
//!
//! Built-in presets ([`presets`]), agent proposals, and imported blueprints
//! go through the same [`validate_blueprint`] and the same materialization
//! ([`materialize`]): validation happens before anything is written, files are
//! created with create-new semantics, and the graph content is applied
//! through the graph's own validator.

pub mod materialize;
pub mod paths;
pub mod presets;

use std::collections::{BTreeMap, BTreeSet};

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::Origin;
use crate::ops::DiagnosticSeverity;
use crate::vocabulary::{EntityKind, RelationKind, ResourceRole};
use crate::workflow::{ValidationContext, WorkflowDefinition, validate_workflow};

pub const BLUEPRINT_SCHEMA: &str = "exposition.blueprint/1";

/// Raw JSON larger than this is refused before parsing.
pub const MAX_BLUEPRINT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FILES: usize = 200;
pub const MAX_FOLDERS: usize = 200;
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ENTITIES: usize = 500;
pub const MAX_RELATIONS: usize = 1000;
pub const MAX_COLLECTIONS: usize = 50;
pub const MAX_MEMBERS: usize = 500;
pub const MAX_WORKFLOWS: usize = 10;
pub const MAX_CAPABILITIES: usize = 16;
pub const MAX_BUILD_TARGETS: usize = 8;
pub const MAX_TITLE: usize = 120;
pub const MAX_PURPOSE: usize = 4000;

/// Relation kinds a blueprint may assert. Structure (`contains`,
/// `anchoredIn`, `imports`) follows the files and is maintained by indexing;
/// evidence and formal associations come only from their services' records.
pub const BLUEPRINT_RELATIONS: &[RelationKind] = &[
    RelationKind::MemberOf,
    RelationKind::UsesAsset,
    RelationKind::DependsOn,
    RelationKind::Assumes,
    RelationKind::ProofOf,
    RelationKind::References,
    RelationKind::DerivedFrom,
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectBlueprint {
    /// Must be [`BLUEPRINT_SCHEMA`].
    pub schema: String,
    /// Where it came from: `preset:<id>`, `agent`, or `import` (`[a-z0-9:._-]{1,64}`).
    pub source: String,
    pub title: String,
    pub purpose: String,
    /// Folders to create (relative paths).
    pub folders: Vec<String>,
    pub files: Vec<BlueprintFile>,
    pub research: Option<BlueprintResearch>,
    pub entities: Vec<BlueprintEntity>,
    pub relations: Vec<BlueprintRelation>,
    pub collections: Vec<BlueprintCollection>,
    pub workflows: Vec<BlueprintWorkflow>,
    pub capabilities: Vec<RequiredCapability>,
    pub build_targets: Vec<BlueprintBuildTarget>,
    pub entry: BlueprintEntry,
}

/// An authored text file (UTF-8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintFile {
    pub path: String,
    pub content: String,
}

/// The project's research space (`.exposition/research/<id>/`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintResearch {
    /// `rs-` followed by 4–32 lowercase letters or digits.
    pub id: String,
    pub label: String,
}

impl BlueprintResearch {
    pub fn folder(&self) -> String {
        format!(".exposition/research/{}", self.id)
    }
}

/// A meaningful entity to admit (claim, question, experiment, ...).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintEntity {
    /// Request-local id (`[A-Za-z0-9_-]{1,32}`), shared with collections and workflows.
    pub temp_id: String,
    pub kind: EntityKind,
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    /// The blueprint file whose text holds it (bound to the whole file).
    pub source: Option<String>,
}

/// A reference to something the blueprint creates (or the project itself).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum BlueprintRef {
    /// An entity, collection, or workflow of this blueprint.
    Temp { temp_id: String },
    /// A file of this blueprint (its resource entity once created).
    File { path: String },
    /// The research space.
    ResearchSpace,
    /// The project entity.
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintRelation {
    pub kind: RelationKind,
    pub from: BlueprintRef,
    pub to: BlueprintRef,
    pub note: Option<String>,
}

/// An intentional organizational collection of the blueprint's entities and files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintCollection {
    pub temp_id: String,
    pub label: String,
    pub members: Vec<BlueprintRef>,
}

/// A workflow draft saved with the project (never run).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintWorkflow {
    pub temp_id: String,
    pub definition: WorkflowDefinition,
}

/// A capability the project needs. Declared only: nothing is installed or run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CapabilityId {
    Typst,
    Markdown,
    PdfLatex,
    LuaLatex,
    Python,
    R,
    Julia,
    Lean,
}

impl CapabilityId {
    pub fn title(self) -> &'static str {
        match self {
            CapabilityId::Typst => "Typst",
            CapabilityId::Markdown => "Markdown (print profile)",
            CapabilityId::PdfLatex => "LaTeX (pdfLaTeX)",
            CapabilityId::LuaLatex => "LaTeX (LuaLaTeX)",
            CapabilityId::Python => "Python",
            CapabilityId::R => "R",
            CapabilityId::Julia => "Julia",
            CapabilityId::Lean => "Lean",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequiredCapability {
    pub capability: CapabilityId,
    pub purpose: String,
}

/// Whether this computer has a declared capability (reported, never fixed
/// by installing anything).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityStatus {
    pub capability: CapabilityId,
    pub title: String,
    pub available: bool,
    /// Version or location when available, the reason when not.
    pub detail: String,
    /// What the user can do (Exposition never installs toolchains).
    pub guidance: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum BlueprintEngine {
    Typst,
    PdfLatex,
    LuaLatex,
    MarkdownPrint,
}

impl BlueprintEngine {
    fn extensions(self) -> &'static [&'static str] {
        match self {
            BlueprintEngine::Typst => &["typ"],
            BlueprintEngine::PdfLatex | BlueprintEngine::LuaLatex => &["tex"],
            BlueprintEngine::MarkdownPrint => &["md", "markdown"],
        }
    }
}

/// A named build target (written to the project's settings).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintBuildTarget {
    pub name: String,
    pub entry: String,
    pub engine: BlueprintEngine,
}

/// What opens after creation (never a graph setup screen).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum BlueprintEntry {
    /// Open this file in the Editor.
    Editor { path: String },
    /// Show the Research page.
    Research,
    /// Show the project's files.
    Files,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintDiagnostic {
    pub severity: DiagnosticSeverity,
    /// Stable code, e.g. `blueprint.path`.
    pub code: String,
    pub message: String,
    /// The path or temporary id it concerns.
    pub subject: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SummaryFile {
    pub path: String,
    pub bytes: u64,
    /// Resource roles it is admitted with (none: created but not a graph resource).
    pub roles: Vec<ResourceRole>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SummaryEntity {
    pub kind: EntityKind,
    pub label: String,
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SummaryWorkflow {
    pub title: String,
    pub well_formed: bool,
    pub errors: u32,
}

/// What creating the project will do, for preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintSummary {
    pub title: String,
    pub purpose: String,
    pub source: String,
    pub folders: Vec<String>,
    pub files: Vec<SummaryFile>,
    pub total_bytes: u64,
    pub research: Option<String>,
    pub has_manuscript: bool,
    pub entities: Vec<SummaryEntity>,
    /// One line per relation ("Main result depends on Key definition").
    pub relations: Vec<String>,
    pub collections: Vec<String>,
    pub workflows: Vec<SummaryWorkflow>,
    pub capabilities: Vec<RequiredCapability>,
    pub build_targets: Vec<BlueprintBuildTarget>,
    pub entry: BlueprintEntry,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintValidation {
    /// No error diagnostics: the blueprint may be materialized.
    pub valid: bool,
    pub diagnostics: Vec<BlueprintDiagnostic>,
    /// Present when the blueprint parsed.
    pub summary: Option<BlueprintSummary>,
}

fn diag(
    severity: DiagnosticSeverity,
    code: &str,
    message: impl Into<String>,
    subject: Option<&str>,
) -> BlueprintDiagnostic {
    BlueprintDiagnostic {
        severity,
        code: code.to_owned(),
        message: message.into(),
        subject: subject.map(|s| crate::store::truncate(s, 300)),
    }
}

/// Parse a blueprint from untrusted JSON text (presets, agents, imports):
/// bounded size, strict schema. Unknown fields such as `trust`, `run`,
/// `onOpen`, or `grants` are refused.
pub fn parse_blueprint(text: &str) -> Result<ProjectBlueprint, BlueprintDiagnostic> {
    if text.len() > MAX_BLUEPRINT_BYTES {
        return Err(diag(
            DiagnosticSeverity::Error,
            "blueprint.tooLarge",
            format!("the blueprint is larger than {} MiB", MAX_BLUEPRINT_BYTES / (1024 * 1024)),
            None,
        ));
    }
    serde_json::from_str(text).map_err(|e| {
        diag(
            DiagnosticSeverity::Error,
            "blueprint.schema",
            format!("the blueprint does not match {BLUEPRINT_SCHEMA}: {e}"),
            None,
        )
    })
}

/// Parse and validate in one step (what IPC and agents use).
pub fn validate_blueprint_text(text: &str) -> (Option<ProjectBlueprint>, BlueprintValidation) {
    match parse_blueprint(text) {
        Ok(bp) => {
            let v = validate_blueprint(&bp);
            (Some(bp), v)
        }
        Err(d) => (None, BlueprintValidation { valid: false, diagnostics: vec![d], summary: None }),
    }
}

fn temp_ok(t: &str) -> bool {
    !t.is_empty() && t.len() <= 32 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn source_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, ':' | '.' | '_' | '-'))
}

/// `main` (the single research space agents create by convention) or `rs-`
/// followed by 4–32 lowercase letters or digits (what presets mint).
pub fn research_id_ok(id: &str) -> bool {
    id == "main"
        || id.strip_prefix("rs-").is_some_and(|rest| {
            (4..=32).contains(&rest.len()) && rest.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// The kind each reference resolves to (for endpoint rules).
struct Resolver<'a> {
    files: &'a BTreeMap<String, WorkspacePath>,
    temps: &'a BTreeMap<String, EntityKind>,
    research: bool,
}

impl Resolver<'_> {
    fn kind(&self, r: &BlueprintRef) -> Result<EntityKind, String> {
        match r {
            BlueprintRef::Temp { temp_id } => {
                self.temps.get(temp_id).copied().ok_or_else(|| format!("no entity {temp_id:?} in this blueprint"))
            }
            BlueprintRef::File { path } => {
                let p = self.files.get(path).ok_or_else(|| format!("{path} is not a file of this blueprint"))?;
                match crate::admission::classify(p) {
                    crate::admission::Admission::Admit(_) => Ok(EntityKind::Resource),
                    _ => Err(format!("{path} is created but is not a project resource, so it cannot be related")),
                }
            }
            BlueprintRef::ResearchSpace if self.research => Ok(EntityKind::ResearchSpace),
            BlueprintRef::ResearchSpace => Err("the blueprint has no research space".into()),
            BlueprintRef::Project => Ok(EntityKind::Project),
        }
    }

    fn label(r: &BlueprintRef, labels: &BTreeMap<String, String>) -> String {
        match r {
            BlueprintRef::Temp { temp_id } => labels.get(temp_id).cloned().unwrap_or_else(|| temp_id.clone()),
            BlueprintRef::File { path } => path.clone(),
            BlueprintRef::ResearchSpace => "the research space".into(),
            BlueprintRef::Project => "the project".into(),
        }
    }
}

/// Validate a blueprint before anything is written.
pub fn validate_blueprint(bp: &ProjectBlueprint) -> BlueprintValidation {
    let mut ds: Vec<BlueprintDiagnostic> = Vec::new();
    let mut err = |code: &str, message: String, subject: Option<&str>| {
        ds.push(diag(DiagnosticSeverity::Error, code, message, subject));
    };

    if bp.schema != BLUEPRINT_SCHEMA {
        err(
            "blueprint.schema",
            format!(
                "unsupported blueprint schema {:?} (this version reads {BLUEPRINT_SCHEMA})",
                crate::store::truncate(&bp.schema, 60)
            ),
            None,
        );
    }
    if !source_ok(&bp.source) {
        err("blueprint.source", "the blueprint source is invalid".into(), None);
    }
    if let Err(m) = crate::validate::check_label(&bp.title) {
        err("blueprint.title", format!("title: {m}"), None);
    } else if bp.title.len() > MAX_TITLE {
        err("blueprint.title", format!("the title is longer than {MAX_TITLE} bytes"), None);
    }
    if let Err(m) = crate::validate::check_text(&bp.purpose, MAX_PURPOSE, "the purpose") {
        err("blueprint.purpose", m, None);
    }
    let limits = [
        (bp.files.len(), MAX_FILES, "files"),
        (bp.folders.len(), MAX_FOLDERS, "folders"),
        (bp.entities.len(), MAX_ENTITIES, "entities"),
        (bp.relations.len(), MAX_RELATIONS, "relations"),
        (bp.collections.len(), MAX_COLLECTIONS, "collections"),
        (bp.workflows.len(), MAX_WORKFLOWS, "workflow drafts"),
        (bp.capabilities.len(), MAX_CAPABILITIES, "capabilities"),
        (bp.build_targets.len(), MAX_BUILD_TARGETS, "build targets"),
    ];
    let mut over = false;
    for (n, max, what) in limits {
        if n > max {
            err("blueprint.tooLarge", format!("a blueprint can have at most {max} {what} (it has {n})"), None);
            over = true;
        }
    }
    if over {
        return BlueprintValidation { valid: false, diagnostics: ds, summary: None };
    }

    // ----- research space -----
    let research_id = match &bp.research {
        Some(r) => {
            if !research_id_ok(&r.id) {
                err(
                    "blueprint.research",
                    "the research space id must be `main` or rs- followed by 4–32 lowercase letters or digits".into(),
                    Some(&r.id),
                );
            }
            if let Err(m) = crate::validate::check_label(&r.label) {
                err("blueprint.research", format!("research space label: {m}"), None);
            }
            Some(r.id.as_str())
        }
        None => None,
    };

    // ----- folders and files -----
    let mut folded: BTreeMap<String, &str> = BTreeMap::new();
    let mut folder_paths: Vec<WorkspacePath> = Vec::new();
    let mut files: BTreeMap<String, WorkspacePath> = BTreeMap::new();
    let mut summary_files = Vec::new();
    let mut total: u64 = 0;
    let mut has_manuscript = false;
    for raw in &bp.folders {
        match paths::check(raw, research_id) {
            Ok(p) => {
                if folded.insert(paths::fold(&p), "folder").is_some() {
                    err("blueprint.collision", format!("{raw} is declared twice"), Some(raw));
                } else {
                    folder_paths.push(p);
                }
            }
            Err(m) => err("blueprint.path", format!("{raw:?}: {m}"), Some(raw)),
        }
    }
    for f in &bp.files {
        let p = match paths::check(&f.path, research_id) {
            Ok(p) => p,
            Err(m) => {
                err("blueprint.path", format!("{:?}: {m}", f.path), Some(&f.path));
                continue;
            }
        };
        if let Some(kind) = folded.insert(paths::fold(&p), "file") {
            let what = if kind == "folder" { "a folder of the same name" } else { "another file" };
            err(
                "blueprint.collision",
                format!("{} collides with {what} (names are case-insensitive)", f.path),
                Some(&f.path),
            );
            continue;
        }
        if f.content.len() > MAX_FILE_BYTES {
            err(
                "blueprint.tooLarge",
                format!("{} is larger than {} KiB", f.path, MAX_FILE_BYTES / 1024),
                Some(&f.path),
            );
        }
        if f.content.contains('\0') {
            err(
                "blueprint.content",
                format!("{} contains NUL characters; blueprint files are text", f.path),
                Some(&f.path),
            );
        }
        total += f.content.len() as u64;
        let roles = match crate::admission::classify(&p) {
            crate::admission::Admission::Admit(r) => r,
            _ => Vec::new(),
        };
        has_manuscript |= roles.contains(&ResourceRole::Manuscript) && !p.as_str().starts_with(".exposition/");
        summary_files.push(SummaryFile { path: p.to_string(), bytes: f.content.len() as u64, roles });
        files.insert(f.path.clone(), p);
    }
    if total > MAX_TOTAL_BYTES as u64 {
        err("blueprint.tooLarge", format!("the files total more than {} MiB", MAX_TOTAL_BYTES / (1024 * 1024)), None);
    }
    // A file cannot be the parent of another path.
    for (key, kind) in &folded {
        if *kind != "file" {
            continue;
        }
        let prefix = format!("{key}/");
        if folded.keys().any(|k| k.starts_with(&prefix)) {
            err("blueprint.collision", format!("{key} is a file but other paths are inside it"), Some(key));
        }
    }

    // ----- temp ids: entities, collections, workflows -----
    let mut temps: BTreeMap<String, EntityKind> = BTreeMap::new();
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    let mut claim_temp = |temp: &str, kind: EntityKind, label: &str, errs: &mut Vec<BlueprintDiagnostic>| {
        if !temp_ok(temp) {
            errs.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.tempId",
                "temporary ids are 1–32 letters, digits, '-' or '_'",
                Some(temp),
            ));
            return false;
        }
        if temps.insert(temp.to_owned(), kind).is_some() {
            errs.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.tempId",
                format!("temporary id {temp:?} is used twice"),
                Some(temp),
            ));
            return false;
        }
        labels.insert(temp.to_owned(), label.to_owned());
        true
    };
    let mut entity_summary = Vec::new();
    for e in &bp.entities {
        if !claim_temp(&e.temp_id, e.kind, &e.label, &mut ds) {
            continue;
        }
        let mut err = |code: &str, m: String| ds.push(diag(DiagnosticSeverity::Error, code, m, Some(&e.temp_id)));
        if !e.kind.is_creatable() || matches!(e.kind, EntityKind::Collection | EntityKind::Workflow) {
            let why = match e.kind {
                EntityKind::Collection => "declare collections under `collections`".to_owned(),
                EntityKind::Workflow => "declare workflow drafts under `workflows`".to_owned(),
                k => format!("{} entities come from files, indexing, and services", k.title()),
            };
            err("blueprint.entityKind", format!("{:?} cannot be declared: {why}", e.label));
        }
        if let Err(m) = crate::validate::check_label(&e.label) {
            err("blueprint.label", format!("entity {:?}: {m}", e.temp_id));
        }
        if let Err(m) = crate::validate::check_attributes(&e.attributes) {
            err("blueprint.attributes", format!("entity {:?}: {m}", e.temp_id));
        }
        if let Some(src) = &e.source {
            match files.get(src) {
                None => err(
                    "blueprint.source",
                    format!("entity {:?} is bound to {src}, which is not a file of this blueprint", e.label),
                ),
                Some(p) if !matches!(crate::admission::classify(p), crate::admission::Admission::Admit(_)) => {
                    err("blueprint.source", format!("{src} is not a project resource, so nothing can be bound to it"))
                }
                Some(_) => {}
            }
        }
        entity_summary.push(SummaryEntity { kind: e.kind, label: e.label.clone(), source: e.source.clone() });
    }
    for c in &bp.collections {
        claim_temp(&c.temp_id, EntityKind::Collection, &c.label, &mut ds);
    }
    let mut workflow_summary = Vec::new();
    let file_set: BTreeSet<WorkspacePath> = files.values().cloned().collect();
    for w in &bp.workflows {
        if !claim_temp(&w.temp_id, EntityKind::Workflow, &w.definition.title, &mut ds) {
            continue;
        }
        let size = serde_json::to_vec(&w.definition).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > crate::workflow::MAX_DEFINITION_BYTES {
            ds.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.tooLarge",
                "a workflow draft is too large",
                Some(&w.temp_id),
            ));
            continue;
        }
        let exists = |p: &WorkspacePath| file_set.contains(p);
        let v = validate_workflow(&w.definition, &ValidationContext { ceiling: None, file_exists: Some(&exists) });
        if v.errors > 0 {
            let first =
                v.diagnostics.iter().find(|d| d.severity == DiagnosticSeverity::Error).map(|d| d.message.clone());
            ds.push(diag(
                DiagnosticSeverity::Warning,
                "blueprint.workflowIncomplete",
                format!(
                    "workflow draft “{}” will be saved incomplete ({} problem{}{})",
                    w.definition.title,
                    v.errors,
                    if v.errors == 1 { "" } else { "s" },
                    first.map(|m| format!(": {m}")).unwrap_or_default()
                ),
                Some(&w.temp_id),
            ));
        }
        workflow_summary.push(SummaryWorkflow {
            title: w.definition.title.clone(),
            well_formed: v.errors == 0,
            errors: v.errors,
        });
    }

    let resolver = Resolver { files: &files, temps: &temps, research: bp.research.is_some() };
    let mut collection_summary = Vec::new();
    for c in &bp.collections {
        if let Err(m) = crate::validate::check_label(&c.label) {
            ds.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.label",
                format!("collection {:?}: {m}", c.temp_id),
                Some(&c.temp_id),
            ));
        }
        if c.members.len() > MAX_MEMBERS {
            ds.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.tooLarge",
                format!("a collection has at most {MAX_MEMBERS} members"),
                Some(&c.temp_id),
            ));
            continue;
        }
        let mut seen = BTreeSet::new();
        for m in &c.members {
            if !seen.insert(m.clone()) {
                ds.push(diag(
                    DiagnosticSeverity::Error,
                    "blueprint.duplicate",
                    format!("{} is listed twice in “{}”", Resolver::label(m, &labels), c.label),
                    Some(&c.temp_id),
                ));
                continue;
            }
            match resolver.kind(m) {
                Ok(k) if RelationKind::MemberOf.allows(k, EntityKind::Collection) => {}
                Ok(k) => ds.push(diag(
                    DiagnosticSeverity::Error,
                    "blueprint.endpointRule",
                    format!("{} cannot be a member of a collection", k.title()),
                    Some(&c.temp_id),
                )),
                Err(m) => ds.push(diag(DiagnosticSeverity::Error, "blueprint.reference", m, Some(&c.temp_id))),
            }
        }
        collection_summary.push(format!(
            "{} ({} member{})",
            c.label,
            c.members.len(),
            if c.members.len() == 1 { "" } else { "s" }
        ));
    }

    // ----- relations -----
    let mut relation_summary = Vec::new();
    let mut seen_relations = BTreeSet::new();
    for (i, r) in bp.relations.iter().enumerate() {
        let subject = format!("relation {i}");
        let mut err = |code: &str, m: String| ds.push(diag(DiagnosticSeverity::Error, code, m, Some(&subject)));
        if !BLUEPRINT_RELATIONS.contains(&r.kind) {
            let why = match r.kind {
                RelationKind::Contains | RelationKind::AnchoredIn | RelationKind::Imports => {
                    "it follows the project's files and is maintained by indexing"
                }
                RelationKind::EvidenceFor => "evidence comes only from recorded runs, audits, and reviews",
                RelationKind::Formalizes => "formal associations need audited declarations from the Lean service",
                _ => "it is not available to blueprints",
            };
            err("blueprint.relationKind", format!("a blueprint cannot assert “{}”: {why}", r.kind.reading()));
            continue;
        }
        let (from, to) = match (resolver.kind(&r.from), resolver.kind(&r.to)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(m), _) | (_, Err(m)) => {
                err("blueprint.reference", m);
                continue;
            }
        };
        if r.from == r.to {
            err("blueprint.selfRelation", "an entity cannot relate to itself".into());
            continue;
        }
        if !r.kind.allows(from, to) {
            err(
                "blueprint.endpointRule",
                format!(
                    "{} cannot {} {}. {}",
                    from.title(),
                    r.kind.reading(),
                    to.title().to_lowercase(),
                    r.kind.rule()
                ),
            );
            continue;
        }
        if let Some(n) = &r.note
            && let Err(m) = crate::validate::check_text(n, crate::validate::MAX_NOTE, "the note")
        {
            err("blueprint.note", m);
            continue;
        }
        if !seen_relations.insert((r.kind, r.from.clone(), r.to.clone())) {
            err("blueprint.duplicate", "this relation is declared twice".into());
            continue;
        }
        relation_summary.push(format!(
            "{} {} {}",
            Resolver::label(&r.from, &labels),
            r.kind.reading(),
            Resolver::label(&r.to, &labels)
        ));
    }

    // ----- capabilities, targets, entry -----
    let mut caps = BTreeSet::new();
    for c in &bp.capabilities {
        if !caps.insert(c.capability) {
            ds.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.duplicate",
                format!("{} is declared twice", c.capability.title()),
                None,
            ));
        }
        if let Err(m) = crate::validate::check_text(&c.purpose, 500, "the capability purpose") {
            ds.push(diag(DiagnosticSeverity::Error, "blueprint.capability", m, None));
        }
    }
    let mut target_names = BTreeSet::new();
    for t in &bp.build_targets {
        let name_ok = t.name.trim() == t.name && t.name.len() <= 64 && crate::validate::check_label(&t.name).is_ok();
        if !name_ok || !target_names.insert(t.name.to_lowercase()) {
            ds.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.buildTarget",
                format!("invalid or duplicate build target name {:?}", t.name),
                None,
            ));
        }
        match files.get(&t.entry) {
            Some(p) => {
                let ext = p.extension().map(|e| e.to_ascii_lowercase()).unwrap_or_default();
                if !t.engine.extensions().contains(&ext.as_str()) {
                    ds.push(diag(
                        DiagnosticSeverity::Error,
                        "blueprint.buildTarget",
                        format!("{} cannot be built with this engine", t.entry),
                        Some(&t.entry),
                    ));
                }
            }
            None => ds.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.buildTarget",
                format!("build target entry {} is not a file of this blueprint", t.entry),
                Some(&t.entry),
            )),
        }
    }
    match &bp.entry {
        BlueprintEntry::Editor { path } if !files.contains_key(path) => {
            ds.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.entry",
                format!("{path} is not a file of this blueprint"),
                Some(path),
            ));
        }
        BlueprintEntry::Research if bp.research.is_none() => {
            ds.push(diag(
                DiagnosticSeverity::Error,
                "blueprint.entry",
                "the Research page needs a research space",
                None,
            ));
        }
        _ => {}
    }

    // ----- the graph validator itself (dry run in memory) -----
    if !ds.iter().any(|d| d.severity == DiagnosticSeverity::Error)
        && let Err(m) = materialize::dry_run_graph(bp, &files)
    {
        ds.push(diag(DiagnosticSeverity::Error, "blueprint.graph", m, None));
    }

    let valid = !ds.iter().any(|d| d.severity == DiagnosticSeverity::Error);
    let summary = BlueprintSummary {
        title: bp.title.clone(),
        purpose: bp.purpose.clone(),
        source: bp.source.clone(),
        folders: folder_paths.iter().map(|p| p.to_string()).collect(),
        files: summary_files,
        total_bytes: total,
        research: bp.research.as_ref().map(|r| r.label.clone()),
        has_manuscript,
        entities: entity_summary,
        relations: relation_summary,
        collections: collection_summary,
        workflows: workflow_summary,
        capabilities: bp.capabilities.clone(),
        build_targets: bp.build_targets.clone(),
        entry: bp.entry.clone(),
    };
    BlueprintValidation { valid, diagnostics: ds, summary: Some(summary) }
}

/// The origin graph content of a blueprint is attributed to.
pub fn blueprint_origin(bp: &ProjectBlueprint) -> Origin {
    Origin::Preset { blueprint: bp.source.clone() }
}

#[cfg(test)]
mod tests;
