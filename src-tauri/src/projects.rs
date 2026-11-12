//! Project creation: preset catalog, blueprint validation, materialization
//! into a new or empty folder, and recovery of interrupted creation.
//!
//! The blueprint is parsed and validated in Rust (`exposition-graph`) before
//! anything is written. The destination must be a new folder or an empty one
//! that the workspace would open (not too broad, not the application's data
//! folder). Files are created with create-new semantics under the operation
//! journal; the project is then opened exactly as File ▸ Open Folder opens it
//! (graph attached), and the blueprint's graph content is applied.
//!
//! Creating a project authorizes only its scaffold: no trust grant, no code
//! execution, no package installation, and nothing is sent to a model.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use exposition_core::build::{DocumentFormat, Engine};
use exposition_core::path::WorkspacePath;
use exposition_graph::blueprint::materialize::{
    CreationAction, CreationStatus, Creator, GraphApplied, Provenance, precheck,
};
use exposition_graph::blueprint::presets::{ProjectPresetInfo, ProjectPresetRequest, ResearchRuntime};
use exposition_graph::blueprint::{
    BlueprintEngine, BlueprintEntry, BlueprintValidation, CapabilityId, CapabilityStatus, ProjectBlueprint,
    validate_blueprint_text,
};
use exposition_graph::{GraphError, GraphService, OperationId};
use exposition_workspace::WorkspaceService;
use exposition_workspace::root::WorkspaceRoot;
use exposition_workspace::settings::{ProjectSettings, ProjectTarget};
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use crate::CommandError;
use crate::state::AppState;

/// Creations running in this process (others found pending were interrupted).
static RUNNING: Mutex<BTreeSet<OperationId>> = Mutex::new(BTreeSet::new());

fn running() -> BTreeSet<OperationId> {
    RUNNING.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

fn set_running(op: &OperationId, on: bool) {
    let mut set = RUNNING.lock().unwrap_or_else(|p| p.into_inner());
    if on {
        set.insert(op.clone());
    } else {
        set.remove(op);
    }
}

async fn blocking<T, F>(f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CommandError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Project task failed: {e}")))?
}

/// A research runtime the Computational study can declare.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeChoice {
    pub runtime: ResearchRuntime,
    pub available: bool,
    /// Version (or the reason it is unavailable).
    pub detail: String,
}

/// The presets (answered at once; tool availability comes separately).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PresetCatalog {
    pub presets: Vec<ProjectPresetInfo>,
    /// Toolchain written to `lean-toolchain` when none is installed.
    pub default_lean_toolchain: String,
}

/// What this computer has for the capabilities presets declare (from the
/// existing toolchain and research-environment discovery; nothing is
/// installed, and no project code runs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReport {
    pub capabilities: Vec<CapabilityStatus>,
    pub runtimes: Vec<RuntimeChoice>,
    /// Installed Lean toolchains (newest first); never installed by Exposition.
    pub lean_toolchains: Vec<String>,
}

/// Where a new project would be created.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DestinationCheck {
    pub path: String,
    pub ok: bool,
    /// Why it cannot be used.
    pub message: Option<String>,
    /// The folder does not exist yet and will be created.
    pub creates_folder: bool,
    /// The location is inside this trusted folder: native tools (Lean,
    /// LaTeX, research kernels) may run the new project's files there.
    /// Creating it there needs the user's explicit acknowledgement.
    pub trusted_via: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProjectRequest {
    /// Absolute path of a new folder (its parent must exist) or an empty one.
    pub destination: String,
    /// The blueprint as JSON text (validated strictly in Rust).
    pub blueprint: String,
    /// The user acknowledged creating the project inside a trusted folder.
    pub accept_trusted_location: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreated {
    pub root: String,
    pub operation: OperationId,
    pub title: String,
    pub applied: GraphApplied,
    /// What to open (Editor file, Research page, or files).
    pub entry: BlueprintEntry,
    /// The project lies inside this (already) trusted folder.
    pub trusted_via: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CreationResolution {
    /// Remaining unfinished creations of the open project.
    pub statuses: Vec<CreationStatus>,
    pub applied: Option<GraphApplied>,
    pub entry: Option<BlueprintEntry>,
}

fn graph_error(e: GraphError) -> CommandError {
    CommandError::new(e.code, e.message)
}

// ----- destination -----

enum Contents {
    Empty,
    /// It already holds an Exposition project graph (from an earlier open).
    ExistingProject,
    Other,
}

/// What an existing destination holds. Only an empty folder (or one with an
/// empty `.exposition`) receives a new project: an existing project graph
/// would carry its old identities and records into the "new" project.
fn contents(dir: &Path) -> std::io::Result<Contents> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() != ".exposition" || !entry.file_type()?.is_dir() {
            return Ok(Contents::Other);
        }
        if std::fs::read_dir(entry.path())?.next().is_some() {
            return Ok(Contents::ExistingProject);
        }
    }
    Ok(Contents::Empty)
}

/// A folder path as people read it (without the verbatim `\\?\` prefix).
fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_owned()
}

/// Check a destination without changing anything.
pub fn check_destination(raw: &str, data_dir: &Path) -> Result<DestinationCheck, CommandError> {
    let refuse = |code: &str, m: String| Err(CommandError::new(code, m));
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 1024 || raw.contains('\0') {
        return refuse("project.destination", "Choose a folder for the new project.".into());
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return refuse("project.destination", format!("{raw} is not an absolute folder path."));
    }
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return refuse("project.destination", format!("{raw} is not a folder."));
            }
            let root = WorkspaceRoot::open_excluding(&path, data_dir).map_err(CommandError::from)?;
            match contents(root.path()).map_err(|e| CommandError::new("project.destination", e.to_string()))? {
                Contents::Empty => {}
                Contents::ExistingProject => {
                    return refuse(
                        "project.existingProject",
                        format!("{raw} already holds an Exposition project. Open it instead, or choose a new folder."),
                    );
                }
                Contents::Other => {
                    return refuse(
                        "project.notEmpty",
                        format!(
                            "{raw} is not empty. Choose a new or empty folder; existing files are never overwritten."
                        ),
                    );
                }
            }
            Ok(DestinationCheck {
                path: root.display_path(),
                ok: true,
                message: None,
                creates_folder: false,
                trusted_via: None,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
                return refuse("project.destination", format!("{raw} cannot be created."));
            };
            if WorkspacePath::parse(name).map(|p| p.as_str() != name).unwrap_or(true) {
                return refuse("project.destination", format!("{name:?} is not a valid folder name."));
            }
            let parent = std::fs::canonicalize(parent).map_err(|_| {
                CommandError::new(
                    "project.destination",
                    format!("The folder that would contain {name} does not exist."),
                )
            })?;
            if !parent.is_dir() {
                return refuse("project.destination", format!("{} is not a folder.", parent.display()));
            }
            let candidate = parent.join(name);
            if let Some(reason) = exposition_core::roots::too_broad_reason(&candidate) {
                return refuse("workspace.tooBroad", format!("{reason}. Choose a specific project folder instead."));
            }
            let data = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
            let lower = |p: &Path| p.to_string_lossy().to_lowercase();
            if lower(&candidate).starts_with(&lower(&data))
                || exposition_core::identity::within_by_identity(&parent, &data)
            {
                return refuse(
                    "workspace.tooBroad",
                    "That location is inside Exposition's own data folder, which is never a project.".into(),
                );
            }
            Ok(DestinationCheck {
                path: display_path(&candidate),
                ok: true,
                message: None,
                creates_folder: true,
                trusted_via: None,
            })
        }
        Err(e) => refuse("project.destination", format!("{raw} cannot be used: {e}.")),
    }
}

/// Create the destination folder when needed and open it as a workspace root
/// would be opened (the authoritative check). Returns the canonical root.
fn prepare_destination(raw: &str, data_dir: &Path) -> Result<(PathBuf, String), CommandError> {
    let check = check_destination(raw, data_dir)?;
    let target = PathBuf::from(&check.path);
    if check.creates_folder {
        std::fs::create_dir(&target).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => {
                CommandError::new("project.notEmpty", format!("{} appeared meanwhile; choose it again.", check.path))
            }
            _ => CommandError::new("project.destination", format!("{} could not be created: {e}.", check.path)),
        })?;
    }
    match WorkspaceRoot::open_excluding(&target, data_dir) {
        Ok(root) => Ok((root.path().to_path_buf(), root.display_path())),
        Err(e) => {
            if check.creates_folder {
                // Only an empty folder we just created is removed.
                let _ = std::fs::remove_dir(&target);
            }
            Err(CommandError::from(e))
        }
    }
}

// ----- capabilities -----

/// Installed Lean toolchains, read from elan's toolchains folder (nothing runs).
fn lean_toolchains() -> Vec<String> {
    let Some(home) = exposition_lean::toolchain::elan_home(|n| std::env::var(n).ok()) else { return Vec::new() };
    let mut out: Vec<String> = std::fs::read_dir(home.join("toolchains"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let dir = e.file_name().to_string_lossy().into_owned();
            let (origin, release) = dir.split_once("---")?;
            let name = format!("{}:{release}", origin.replace("--", "/"));
            matches!(
                exposition_lean::toolchain::resolve(Some(&home), &name),
                exposition_lean::toolchain::Resolution::Installed(_)
            )
            .then_some(name)
        })
        .collect();
    out.sort_by(|a, b| version_key(b).cmp(&version_key(a)).then_with(|| b.cmp(a)));
    out
}

fn version_key(name: &str) -> Vec<u64> {
    name.rsplit(':')
        .next()
        .unwrap_or("")
        .trim_start_matches('v')
        .split(['.', '-'])
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

struct HostCapabilities {
    statuses: Vec<CapabilityStatus>,
    runtimes: Vec<RuntimeChoice>,
    lean: Vec<String>,
}

async fn host_capabilities(state: &AppState) -> HostCapabilities {
    let tools = state.execution.toolchains().await;
    let tool = |id: &str| tools.iter().find(|t| t.id == id);
    let env = state.research.environments(false).await.ok();
    let lean = tokio::task::spawn_blocking(lean_toolchains).await.unwrap_or_default();
    let mut statuses = Vec::new();
    let mut runtimes = Vec::new();
    let status = |capability: CapabilityId, available: bool, detail: String, guidance: Option<String>| {
        CapabilityStatus { capability, title: capability.title().to_owned(), available, detail, guidance }
    };
    statuses.push(status(CapabilityId::Typst, true, "Built into Exposition".into(), None));
    statuses.push(status(CapabilityId::Markdown, true, "Built into Exposition (Typst print profile)".into(), None));
    for (cap, id) in [(CapabilityId::PdfLatex, "pdflatex"), (CapabilityId::LuaLatex, "lualatex")] {
        match tool(id) {
            Some(t) if t.status == exposition_execution::toolchains::ToolStatus::Available => statuses.push(status(
                cap,
                true,
                t.version.clone().or_else(|| t.path.clone()).unwrap_or_else(|| "Found".into()),
                None,
            )),
            Some(t) => statuses.push(status(
                cap,
                false,
                t.detail.clone().unwrap_or_else(|| format!("{id} was not found")),
                t.guidance.clone().or_else(|| Some("Install a TeX distribution (e.g. TeX Live or MiKTeX), or set its location in Settings › Tools.".into())),
            )),
            None => statuses.push(status(cap, false, format!("{id} was not found"), Some("Install a TeX distribution, then refresh Toolchains.".into()))),
        }
    }
    use exposition_research::types::{Availability, RuntimeLanguage};
    for (cap, runtime, language) in [
        (CapabilityId::Python, ResearchRuntime::Python, RuntimeLanguage::Python),
        (CapabilityId::R, ResearchRuntime::R, RuntimeLanguage::R),
        (CapabilityId::Julia, ResearchRuntime::Julia, RuntimeLanguage::Julia),
    ] {
        let found = env
            .as_ref()
            .map(|e| e.runtimes.iter().filter(|r| r.language == language).collect::<Vec<_>>())
            .unwrap_or_default();
        let available = found.iter().find(|r| r.availability == Availability::Available);
        let (ok, detail, guidance) = match (available, found.first()) {
            (Some(r), _) => (
                true,
                format!("{}{}", r.display_name, r.version.as_ref().map(|v| format!(" {v}")).unwrap_or_default()),
                None,
            ),
            (None, Some(r)) => match &r.availability {
                Availability::Unavailable { reason, guidance } => (false, reason.clone(), Some(guidance.clone())),
                Availability::Available => (true, r.display_name.clone(), None),
            },
            (None, None) => (
                false,
                format!("No {} environment was found", cap.title()),
                Some("Install it, then refresh Research environments. Exposition never installs runtimes.".into()),
            ),
        };
        statuses.push(status(cap, ok, detail.clone(), guidance));
        runtimes.push(RuntimeChoice { runtime, available: ok, detail });
    }
    statuses.push(if lean.is_empty() {
        status(
            CapabilityId::Lean,
            false,
            "No Lean toolchain is installed".into(),
            Some("Install one with elan (elan toolchain install <version>); Exposition never installs toolchains implicitly.".into()),
        )
    } else {
        status(CapabilityId::Lean, true, lean.join(", "), None)
    });
    HostCapabilities { statuses, runtimes, lean }
}

/// The trusted folder that would cover a new project at this destination.
fn trusted_via(execution: &exposition_execution::ExecutionService, check: &DestinationCheck) -> Option<String> {
    let path = PathBuf::from(&check.path);
    let probe = if check.creates_folder { path.parent()?.to_path_buf() } else { path };
    let status = execution.trust_status(&probe.to_string_lossy()).ok()?;
    status.trusted.then(|| status.trusted_via.unwrap_or(status.root))
}

/// Built-in presets assert their graph content; any other blueprint (whatever
/// `source` it declares) only suggests. A blueprint claiming a preset must
/// name one that exists.
fn provenance_of(bp: &ProjectBlueprint) -> Provenance {
    let preset = bp.source.strip_prefix("preset:");
    let known = |id: &str| {
        id == exposition_graph::blueprint::presets::EMPTY.to_ascii_lowercase()
            || exposition_graph::blueprint::presets::catalog().iter().any(|p| p.id.to_ascii_lowercase() == id)
    };
    match preset {
        Some(id) if known(id) => Provenance::Preset,
        _ => Provenance::Imported,
    }
}

// ----- commands -----

#[tauri::command]
pub async fn project_presets() -> Result<PresetCatalog, CommandError> {
    Ok(PresetCatalog {
        presets: exposition_graph::blueprint::presets::catalog(),
        default_lean_toolchain: exposition_graph::blueprint::presets::DEFAULT_LEAN_TOOLCHAIN.to_owned(),
    })
}

/// Availability of every capability a preset can declare. This may wait for
/// first-time tool discovery, so the New Project page asks for it separately.
#[tauri::command]
pub async fn project_capabilities(state: State<'_, AppState>) -> Result<CapabilityReport, CommandError> {
    let host = host_capabilities(&state).await;
    Ok(CapabilityReport { capabilities: host.statuses, runtimes: host.runtimes, lean_toolchains: host.lean })
}

/// The blueprint of a preset with the chosen options (fresh ids each time).
#[tauri::command]
pub async fn project_preset_blueprint(request: ProjectPresetRequest) -> Result<ProjectBlueprint, CommandError> {
    exposition_graph::blueprint::presets::blueprint(&request).map_err(|m| CommandError::new("project.preset", m))
}

/// Validate a blueprint (JSON text: presets, agent proposals, imports alike).
#[tauri::command]
pub async fn project_blueprint_validate(blueprint: String) -> Result<BlueprintValidation, CommandError> {
    let (_, validation) = blocking(move || Ok(validate_blueprint_text(&blueprint))).await?;
    Ok(validation)
}

#[tauri::command]
pub async fn project_check_destination(
    state: State<'_, AppState>,
    destination: String,
) -> Result<DestinationCheck, CommandError> {
    let data_dir = state.data_dir.clone();
    let execution = state.execution.clone();
    blocking(move || {
        Ok(match check_destination(&destination, &data_dir) {
            Ok(mut c) => {
                c.trusted_via = trusted_via(&execution, &c);
                c
            }
            Err(e) => DestinationCheck {
                path: destination.trim().to_owned(),
                ok: false,
                message: Some(e.message),
                creates_folder: false,
                trusted_via: None,
            },
        })
    })
    .await
}

/// Simulated crash for e2e recovery tests (debug builds only): abort the
/// process after this many files.
fn interrupt_hook() -> impl Fn(usize) -> bool {
    #[cfg(debug_assertions)]
    let limit: Option<usize> =
        std::env::var("EXPOSITION_TEST_INTERRUPT_PROJECT_CREATION").ok().and_then(|v| v.trim().parse().ok());
    #[cfg(not(debug_assertions))]
    let limit: Option<usize> = None;
    move |n| {
        if limit == Some(n) {
            eprintln!("simulated crash during project creation after {n} file(s)");
            std::process::abort();
        }
        false
    }
}

fn target_settings(bp: &ProjectBlueprint, current: ProjectSettings) -> Option<ProjectSettings> {
    if bp.build_targets.is_empty() || !current.targets.is_empty() {
        return None;
    }
    let mut settings = current;
    for t in &bp.build_targets {
        let entry = WorkspacePath::parse(&t.entry).ok()?;
        let (format, engine) = match t.engine {
            BlueprintEngine::Typst => (DocumentFormat::Typst, Engine::Typst),
            BlueprintEngine::PdfLatex => (DocumentFormat::Latex, Engine::PdfLatex),
            BlueprintEngine::LuaLatex => (DocumentFormat::Latex, Engine::LuaLatex),
            BlueprintEngine::MarkdownPrint => (DocumentFormat::Markdown, Engine::MarkdownPrint),
        };
        settings.targets.push(ProjectTarget { name: t.name.clone(), format, engine, entry });
    }
    if settings.entry.is_none() {
        settings.entry = settings.targets.first().map(|t| t.entry.clone());
    }
    if settings.default_target.is_none() {
        settings.default_target = settings.targets.first().map(|t| t.name.clone());
    }
    Some(settings)
}

/// Write the blueprint's build targets into the project settings (only when
/// the project has none). Recorded in the operation journal.
fn apply_settings(
    workspace: &WorkspaceService,
    creator: &Creator<'_>,
    root: &Path,
    op: &OperationId,
    bp: &ProjectBlueprint,
) {
    let Ok(current) = workspace.project_settings() else { return };
    let Some(settings) = target_settings(bp, current.settings) else { return };
    let result = workspace.update_project_settings(&settings);
    let mut refs = std::collections::BTreeMap::new();
    refs.insert("targets".to_owned(), settings.targets.len().to_string());
    creator.record_step(
        root,
        op,
        exposition_graph::blueprint::materialize::STEP_SETTINGS,
        refs,
        result.is_ok(),
        result.err().map(|e| e.message),
    );
}

/// Create a project from a blueprint in a new or empty folder, open it, and
/// apply its graph content. An interruption leaves a recoverable record.
#[tauri::command]
pub async fn project_create(
    state: State<'_, AppState>,
    request: CreateProjectRequest,
) -> Result<ProjectCreated, CommandError> {
    let (bp, validation) = blocking({
        let text = request.blueprint.clone();
        move || Ok(validate_blueprint_text(&text))
    })
    .await?;
    let bp = match bp {
        Some(bp) if validation.valid => bp,
        _ => {
            let first: Vec<String> = validation
                .diagnostics
                .iter()
                .filter(|d| d.severity == exposition_graph::DiagnosticSeverity::Error)
                .take(3)
                .map(|d| d.message.clone())
                .collect();
            return Err(CommandError::new(
                "blueprint.invalid",
                format!("The blueprint is not valid; nothing was created. {}", first.join(" ")),
            ));
        }
    };
    let data_dir = state.data_dir.clone();
    let graph: Arc<GraphService> = Arc::clone(&state.graph);
    // Inside a trusted folder, native tools may run the new project's files:
    // only with the user's explicit acknowledgement.
    let trusted = {
        let execution = state.execution.clone();
        let data_dir = data_dir.clone();
        let destination = request.destination.clone();
        blocking(move || Ok(check_destination(&destination, &data_dir).ok().and_then(|c| trusted_via(&execution, &c))))
            .await?
    };
    if let Some(via) = &trusted
        && !request.accept_trusted_location
    {
        return Err(CommandError::new(
            "project.trustedLocation",
            format!(
                "This location is inside the trusted folder {via}, where Exposition's native tools may run the project's files. Confirm creating it there, or choose another location."
            ),
        ));
    }
    let provenance = provenance_of(&bp);
    let (root, display, op) = blocking({
        let bp = bp.clone();
        let graph = Arc::clone(&graph);
        let data_dir = data_dir.clone();
        let destination = request.destination.clone();
        let provenance = provenance.clone();
        move || {
            let (root, display) = prepare_destination(&destination, &data_dir)?;
            precheck(&root, &bp).map_err(graph_error)?;
            let creator = Creator { graph: &graph, data_dir: &data_dir };
            let op = creator.begin(&root, &bp, &provenance).map_err(graph_error)?;
            set_running(&op, true);
            let interrupt = interrupt_hook();
            if let Err(e) = creator.write_files(&root, &op, &bp, &interrupt) {
                set_running(&op, false);
                return Err(graph_error(e));
            }
            Ok((root, display, op))
        }
    })
    .await?;

    // Open it like File ▸ Open Folder (registers execution, attaches the graph).
    if let Err(e) = crate::workspace::workspace_open(state.clone(), display.clone()).await {
        Creator { graph: &graph, data_dir: &data_dir }.stopped(
            &root,
            &op,
            &format!("The files were created but the project could not be opened: {}", e.message),
        );
        set_running(&op, false);
        return Err(e);
    }

    let workspace = Arc::clone(&state.workspace);
    let outcome = blocking({
        let bp = bp.clone();
        let op = op.clone();
        move || {
            let creator = Creator { graph: &graph, data_dir: &data_dir };
            let result = (|| -> Result<GraphApplied, CommandError> {
                creator.record_open(&root, &op).map_err(graph_error)?;
                let applied = creator.apply_graph(&root, &op, &bp, &provenance).map_err(graph_error)?;
                apply_settings(&workspace, &creator, &root, &op, &bp);
                creator.complete(&root, &op).map_err(graph_error)?;
                Ok(applied)
            })();
            set_running(&op, false);
            result
        }
    })
    .await?;
    Ok(ProjectCreated {
        root: display,
        operation: op,
        title: bp.title.clone(),
        applied: outcome,
        entry: bp.entry.clone(),
        trusted_via: trusted,
    })
}

/// Unfinished project creations of the open project (interrupted ones are
/// reported as needing reconciliation, never re-run).
#[tauri::command]
pub async fn project_creation_status(state: State<'_, AppState>) -> Result<Vec<CreationStatus>, CommandError> {
    let graph = Arc::clone(&state.graph);
    let data_dir = state.data_dir.clone();
    blocking(move || {
        Ok(match graph.root() {
            Some(root) => Creator { graph: &graph, data_dir: &data_dir }.statuses(&root, &running()),
            None => Vec::new(),
        })
    })
    .await
}

/// Resolve an interrupted creation with the user's explicit choice.
#[tauri::command]
pub async fn project_creation_resolve(
    state: State<'_, AppState>,
    operation: OperationId,
    action: CreationAction,
) -> Result<CreationResolution, CommandError> {
    if running().contains(&operation) {
        return Err(CommandError::new("project.inProgress", "That project is still being created."));
    }
    let graph = Arc::clone(&state.graph);
    let data_dir = state.data_dir.clone();
    let workspace = Arc::clone(&state.workspace);
    let root = graph
        .root()
        .ok_or_else(|| CommandError::new("graph.noProject", "Open the project folder to finish or keep its setup."))?;
    blocking(move || {
        let creator = Creator { graph: &graph, data_dir: &data_dir };
        let (applied, entry) = match action {
            CreationAction::Finish => {
                let (applied, bp) = creator.finish(&root, &operation).map_err(graph_error)?;
                apply_settings(&workspace, &creator, &root, &operation, &bp);
                (Some(applied), Some(bp.entry))
            }
            CreationAction::Keep => {
                creator.keep(&root, &operation).map_err(graph_error)?;
                (None, None)
            }
        };
        let statuses = creator.statuses(&root, &running());
        Ok(CreationResolution { statuses, applied, entry })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temporary folder removed on drop (no dev-dependencies here).
    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("exposition-projects-test-{}", uuid_like()));
            std::fs::create_dir_all(&p).unwrap();
            Temp(std::fs::canonicalize(&p).unwrap())
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn uuid_like() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        format!("{}-{t}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
    }

    #[test]
    fn destinations_must_be_new_or_empty_and_specific() {
        let data = Temp::new();
        let parent = Temp::new();
        // A new folder under an existing parent.
        let new = parent.path().join("paper");
        let c = check_destination(new.to_str().unwrap(), data.path()).unwrap();
        assert!(c.ok && c.creates_folder);
        // An empty existing folder.
        std::fs::create_dir(&new).unwrap();
        assert!(!check_destination(new.to_str().unwrap(), data.path()).unwrap().creates_folder);
        // An empty `.exposition` is fine; an existing project graph is not
        // carried into a "new" project.
        std::fs::create_dir(new.join(".exposition")).unwrap();
        assert!(check_destination(new.to_str().unwrap(), data.path()).is_ok());
        std::fs::write(new.join(".exposition").join("graph.sqlite3"), b"").unwrap();
        assert_eq!(check_destination(new.to_str().unwrap(), data.path()).unwrap_err().code, "project.existingProject");
        std::fs::remove_file(new.join(".exposition").join("graph.sqlite3")).unwrap();
        // Anything else makes it non-empty.
        std::fs::write(new.join("notes.txt"), b"mine").unwrap();
        assert_eq!(check_destination(new.to_str().unwrap(), data.path()).unwrap_err().code, "project.notEmpty");
        // Relative, missing parent, invalid names.
        assert!(check_destination("relative/path", data.path()).is_err());
        assert!(check_destination(parent.path().join("missing").join("x").to_str().unwrap(), data.path()).is_err());
        assert!(check_destination(parent.path().join("CON").to_str().unwrap(), data.path()).is_err());
        // The application's data folder (and anything inside it).
        assert_eq!(
            check_destination(data.path().join("proj").to_str().unwrap(), data.path()).unwrap_err().code,
            "workspace.tooBroad"
        );
    }

    #[test]
    fn too_broad_destinations_are_refused() {
        let data = Temp::new();
        if let Some(profile) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
            let err = check_destination(&profile.to_string_lossy(), data.path()).unwrap_err();
            assert_eq!(err.code, "workspace.tooBroad", "{}", err.message);
        }
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        assert_eq!(check_destination(root, data.path()).unwrap_err().code, "workspace.tooBroad");
    }

    #[test]
    fn build_targets_fill_empty_project_settings_only() {
        let bp = exposition_graph::blueprint::presets::blueprint(&ProjectPresetRequest {
            preset: "paper".into(),
            title: None,
            format: Some(exposition_graph::blueprint::presets::ManuscriptFormat::LatexLua),
            runtime: None,
            lean_toolchain: None,
            include_workflow: false,
            purpose: None,
        })
        .unwrap();
        let s = target_settings(&bp, ProjectSettings::default()).unwrap();
        assert_eq!(s.targets[0].engine, Engine::LuaLatex);
        assert_eq!(s.entry.as_ref().unwrap().as_str(), "main.tex");
        assert!(s.validate().is_ok());
        assert!(target_settings(&bp, s).is_none(), "existing targets are not replaced");
    }

    #[test]
    fn only_real_presets_assert_their_graph_content() {
        let mut bp = exposition_graph::blueprint::presets::blueprint(&ProjectPresetRequest {
            preset: "paperLean".into(),
            title: None,
            format: Some(exposition_graph::blueprint::presets::ManuscriptFormat::Typst),
            runtime: None,
            lean_toolchain: None,
            include_workflow: true,
            purpose: None,
        })
        .unwrap();
        assert_eq!(provenance_of(&bp), Provenance::Preset);
        for source in ["preset:invented", "agent", "import", "presetpaper"] {
            bp.source = source.into();
            assert_eq!(provenance_of(&bp), Provenance::Imported, "{source}");
        }
    }

    #[test]
    fn unc_destinations_keep_their_share_prefix() {
        assert_eq!(display_path(Path::new(r"\\?\UNC\srv\share\p\new")), r"\\srv\share\p\new");
        assert_eq!(display_path(Path::new(r"\\?\C:\p\new")), r"C:\p\new");
    }

    #[test]
    fn version_ordering_prefers_newest() {
        let mut v = ["leanprover/lean4:v4.9.0".to_owned(), "leanprover/lean4:v4.35.0".to_owned()];
        v.sort_by_key(|n| std::cmp::Reverse(version_key(n)));
        assert_eq!(v[0], "leanprover/lean4:v4.35.0");
    }
}
