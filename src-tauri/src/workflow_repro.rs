//! Workflow run reproducibility commands (M9): run manifests (JSON, and an
//! export to a file the user chooses) and reproduction plans.
//!
//! The run service builds both from the stored run record; this module only
//! supplies what the application knows: the secret redactor of the execution
//! service and the folders that must not appear in a manifest, and the
//! sources that describe the environment a step would get now (the batch
//! runner's configured image and caches, the agents' provider settings, the
//! files around a Lean file) — none of which runs anything. A reproduction
//! plan is started with the normal `workflow_run_start`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_graph::WorkflowRunId;
use exposition_workflow::WorkflowRunError;
use exposition_workflow::adapters::agent::{AgentReviewer, AgentServiceReviewer};
use exposition_workflow::manifest::{self, ManifestScrub, WorkflowManifestExport};
use exposition_workflow::reproduce::{self, WorkflowReproducePlan};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::CommandError;
use crate::state::AppState;

/// The services these commands use (cloned handles, for blocking tasks).
struct Ctx {
    workspace: Arc<exposition_workspace::WorkspaceService>,
    execution: exposition_execution::ExecutionService,
    research: Arc<exposition_research::ResearchService>,
    agents: exposition_agents::AgentService,
    runs: exposition_workflow::WorkflowRunService,
    data_dir: PathBuf,
}

impl Ctx {
    fn of(state: &AppState) -> Self {
        Self {
            workspace: Arc::clone(&state.workspace),
            execution: state.execution.clone(),
            research: Arc::clone(&state.research),
            agents: state.agents.clone(),
            runs: state.workflow_runs.clone(),
            data_dir: state.data_dir.clone(),
        }
    }
}

/// The open project's canonical root.
fn project_root(state: &Ctx) -> Result<PathBuf, CommandError> {
    let root =
        state.workspace.open_root().ok_or_else(|| CommandError::new("workflow.noProject", "Open a project first."))?;
    Ok(std::fs::canonicalize(&root).unwrap_or(root))
}

/// Folders replaced in a manifest's free text.
fn private_folders(state: &Ctx, root: &Path) -> Vec<(PathBuf, String)> {
    let mut folders = vec![(root.to_path_buf(), "<project>".to_owned())];
    if let Some(open) = state.workspace.open_root() {
        folders.push((open, "<project>".to_owned()));
    }
    folders.push((state.data_dir.clone(), "<app data>".to_owned()));
    for var in ["USERPROFILE", "HOME"] {
        if let Some(home) = std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()) {
            folders.push((home, "~".to_owned()));
        }
    }
    // Longest first, so a folder inside another is replaced as a whole.
    folders.sort_by_key(|(p, _)| std::cmp::Reverse(p.as_os_str().len()));
    folders
}

async fn blocking<T, F>(state: &State<'_, AppState>, f: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce(&Ctx) -> Result<T, CommandError> + Send + 'static,
{
    let ctx = Ctx::of(state.inner());
    tauri::async_runtime::spawn_blocking(move || f(&ctx))
        .await
        .map_err(|e| CommandError::new("internal.task", format!("Workflow reproducibility task failed: {e}")))?
}

fn manifest_json(state: &Ctx, run: &WorkflowRunId) -> Result<(String, String), CommandError> {
    let root = project_root(state)?;
    let execution = state.execution.clone();
    let redact = move |text: &str| execution.redact(text);
    let scrub = ManifestScrub { redact: &redact, folders: private_folders(state, &root) };
    let json = state.runs.run_manifest_json(run, &scrub).map_err(CommandError::from)?;
    let title = state.runs.get(run).map(|r| r.title).unwrap_or_default();
    Ok((json, title))
}

/// The run's manifest as canonical JSON (sorted keys; no logs, secrets, or
/// absolute paths).
#[tauri::command]
pub async fn workflow_run_manifest(state: State<'_, AppState>, run: WorkflowRunId) -> Result<String, CommandError> {
    blocking(&state, move |s| manifest_json(s, &run).map(|(json, _)| json)).await
}

fn file_name_for(title: &str) -> String {
    let stem: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(60)
        .collect();
    format!("{}-run-manifest.json", if stem.is_empty() { "workflow" } else { &stem })
}

/// Where an export may write: the same rules as workflow bundle exports
/// (an absolute `.json` path in an existing folder, never inside the
/// project's `.exposition` folder or the app data folder, never a folder or a
/// link).
fn export_target(raw: &str, state: &Ctx) -> Result<PathBuf, CommandError> {
    crate::workflows::chosen_file(raw, &state.workspace.open_root(), &state.data_dir)
}

/// Export the run's manifest to `destination`, or to a file chosen in the
/// save dialog when `destination` is `None` (`workflow.exportCanceled` when
/// the dialog is dismissed). Either way the bundle-export location rules
/// apply, and an existing file is replaced only when it is an earlier run
/// manifest.
#[tauri::command]
pub async fn workflow_run_manifest_export(
    app: AppHandle,
    state: State<'_, AppState>,
    run: WorkflowRunId,
    destination: Option<String>,
) -> Result<WorkflowManifestExport, CommandError> {
    blocking(&state, move |s| {
        let (json, title) = manifest_json(s, &run)?;
        let raw = match destination {
            Some(path) => path,
            None => app
                .dialog()
                .file()
                .set_title("Export run manifest")
                .add_filter("JSON", &["json"])
                .set_file_name(file_name_for(&title))
                .blocking_save_file()
                .and_then(|picked| picked.as_path().map(|p| p.display().to_string()))
                .ok_or_else(|| CommandError::new("workflow.exportCanceled", "The export was canceled."))?,
        };
        let target = export_target(&raw, s)?;
        let written = manifest::write_manifest_file(&target, &json).map_err(CommandError::from)?;
        // The location as the person chose it (as bundle exports report it).
        Ok(WorkflowManifestExport { path: raw, ..written })
    })
    .await
}

/// A reproduction of `run`: the normal plan of the same revision with every
/// input pinned to the digest the run read (cause Reproduce), and the
/// environment drift per step. Start `plan` with `workflow_run_start`.
#[tauri::command]
pub async fn workflow_run_reproduce_plan(
    state: State<'_, AppState>,
    run: WorkflowRunId,
) -> Result<WorkflowReproducePlan, CommandError> {
    blocking(&state, move |s| {
        let root = project_root(s)?;
        let batch = exposition_research::sandbox::batch_runner(Arc::clone(&s.research), s.execution.clone());
        let reviewer: Arc<dyn AgentReviewer> = Arc::new(AgentServiceReviewer { agents: s.agents.clone() });
        let sources = reproduce::environment_sources(Some(batch), Some(reviewer));
        let current = |plan: &exposition_workflow::WorkflowRunPlan, step: &exposition_workflow::WorkflowPlanStep| {
            reproduce::current_environment(&root, &sources, plan, step)
        };
        s.runs.reproduce_plan(&run, &current).map_err(|e: WorkflowRunError| CommandError::from(e))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::file_name_for;

    #[test]
    fn suggested_file_names_are_plain() {
        assert_eq!(file_name_for("Numerical experiment"), "Numerical-experiment-run-manifest.json");
        assert_eq!(file_name_for("../x:y"), "x-y-run-manifest.json");
        assert_eq!(file_name_for("///"), "workflow-run-manifest.json");
    }
}
