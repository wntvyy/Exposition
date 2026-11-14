//! P1 project operations — the data side (graph, workflows, runs, agent tasks,
//! research records, triggers, Storage & History).
//!
//! OWNER: lane L2. The file side and the orchestrator (`project_ops.rs`, lane
//! L1) call ONLY these functions; their signatures are the shared seam and
//! change only through the coordinator. The work is done by
//! `exposition_workflow::project_data` over the owning services (graph,
//! workflow runs, triggers, research records, agent tasks); this module only
//! builds its context from the application state.
//!
//! Seam addition (L2, for the coordinator): [`restore_data`] restores an
//! app-managed recoverable deletion by its token (`<kind>:<id>`, the
//! `OpOutcome.restore` of a data target); it returns `None` for any other
//! token (a file restore), so `project_op_restore` can try it first.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use exposition_core::ops::{OpKind, OpOutcome, OpPlan, OpRefusal, OpRequest, OpRestore, StorageOverview};
use exposition_core::path::WorkspacePath;
use exposition_workflow::project_data::{self as data, DataContext, ResearchServiceRecords};

use crate::state::AppState;

/// The data side's part of a plan: effects/refusals/impacts/choices for
/// non-path targets, and impacts of path targets on project data.
pub use exposition_workflow::project_data::DataPlan;

/// Research follows the workspace service's open folder (as the research
/// commands do); trust stays the execution service's decision.
fn sync_research(state: &AppState) {
    match state.workspace.open_root() {
        Some(root) => {
            let canonical = std::fs::canonicalize(&root).ok();
            if canonical.is_none() || state.research.workspace_root() != canonical {
                let _ = state.research.set_workspace(&root, false);
            }
        }
        None => state.research.clear_workspace(),
    }
}

/// Whether a project file exists inside the open project (links leading
/// outside count as missing).
fn exists_in(root: &Option<PathBuf>, path: &WorkspacePath) -> bool {
    let Some(root) = root else { return false };
    let file = path.components().fold(root.clone(), |p, c| p.join(c));
    std::fs::canonicalize(&file)
        .is_ok_and(|c| std::fs::canonicalize(root).is_ok_and(|r| c.starts_with(r)) && c.is_file())
}

/// Run `f` with the data context of the open project.
fn with_context<T>(state: &AppState, f: impl FnOnce(&DataContext<'_>) -> T) -> T {
    sync_research(state);
    let root = state.workspace.open_root();
    let research = ResearchServiceRecords {
        service: std::sync::Arc::clone(&state.research),
        runtime: tauri::async_runtime::handle().inner().clone(),
    };
    let exists_root = root.clone();
    let exists = move |p: &WorkspacePath| exists_in(&exists_root, p);
    let cx = DataContext {
        graph: &state.graph,
        runs: &state.workflow_runs,
        triggers: Some(&state.workflow_triggers),
        research: Some(&research),
        tasks: Some(&state.agents),
        file_exists: Some(&exists),
        root,
    };
    f(&cx)
}

/// Concrete consequences of deleting (or, for `SavedVersionCopied`-style
/// notes, copying) these project paths for graph/workflow/evidence/trigger/
/// active-work consumers. Graph membership alone is never an impact.
pub fn impacts_for_paths(state: &AppState, paths: &[WorkspacePath], kind: &OpKind) -> DataPlan {
    with_context(state, |cx| data::impacts_for_paths(cx, paths, kind))
}

/// Plan the non-path targets of `request` (workflows, revisions, runs, tasks,
/// research runs, graph entities/relations/views, recoverable items).
pub fn plan_data(state: &AppState, request: &OpRequest) -> DataPlan {
    with_context(state, |cx| data::plan_data(cx, request))
}

/// Apply the non-path part of an approved plan (choices already resolved,
/// defaults filled). Returns one outcome per non-path target.
pub fn apply_data(state: &AppState, plan: &OpPlan, choices: &BTreeMap<String, String>) -> Vec<OpOutcome> {
    with_context(state, |cx| data::apply_data(cx, plan, choices))
}

/// Before file work: stop/disable what the user chose to stop (active runs or
/// tasks using the paths, enabled triggers watching them), coordinated so no
/// worker writes into a deleted target. Returns per-path refusals when work
/// could not be stopped (those paths are then not deleted).
pub fn prepare_paths(state: &AppState, plan: &OpPlan, choices: &BTreeMap<String, String>) -> Vec<OpRefusal> {
    with_context(state, |cx| data::prepare_paths(cx, plan, choices))
}

/// After file work: reconcile project data with the ACTUAL outcomes (deleted
/// paths → tombstones / missing dependencies; created copies → fresh
/// identities with copied-from provenance; restored paths → reconciled).
pub fn after_paths(state: &AppState, outcomes: &[OpOutcome]) {
    with_context(state, |cx| data::after_paths(cx, outcomes))
}

/// Settings → Storage & History.
pub fn storage_overview(state: &AppState) -> StorageOverview {
    with_context(state, data::storage_overview)
}

/// Restore an app-managed recoverable deletion by its token; `None` when the
/// token is not a data token (a file restore). Never overwrites; nothing
/// restarts; a restored workflow's triggers stay off.
pub fn restore_data(state: &AppState, restore: &OpRestore) -> Option<OpOutcome> {
    with_context(state, |cx| data::restore_data(cx, restore))
}
