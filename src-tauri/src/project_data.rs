//! P1 project operations — the data side (graph, workflows, runs, agent tasks,
//! research records, triggers, Storage & History).
//!
//! OWNER: lane L2. The file side and the orchestrator (`project_ops.rs`, lane
//! L1) call ONLY these functions; their signatures are the shared seam and
//! change only through the coordinator. Until L2 implements them they report
//! nothing (no impacts, no data targets supported, no reconciliation).
#![allow(dead_code)]

use std::collections::BTreeMap;

use exposition_core::ops::{OpImpact, OpKind, OpOutcome, OpPlan, OpRefusal, OpRequest, StorageOverview};
use exposition_core::path::WorkspacePath;

use crate::state::AppState;

/// The data side's part of a plan: effects/refusals/impacts/choices for
/// non-path targets, and impacts of path targets on project data.
#[derive(Default)]
pub struct DataPlan {
    pub effects: Vec<exposition_core::ops::OpEffect>,
    pub refusals: Vec<OpRefusal>,
    pub impacts: Vec<OpImpact>,
    pub choices: Vec<exposition_core::ops::OpChoice>,
    /// Material the plan hash must cover (revisions, digests, states) so a
    /// change between plan and apply is detected.
    pub fingerprint: Vec<String>,
}

/// Concrete consequences of deleting (or, for `SavedVersionCopied`-style
/// notes, copying) these project paths for graph/workflow/evidence/trigger/
/// active-work consumers. Graph membership alone is never an impact.
pub fn impacts_for_paths(_state: &AppState, _paths: &[WorkspacePath], _kind: &OpKind) -> DataPlan {
    DataPlan::default()
}

/// Plan the non-path targets of `request` (workflows, revisions, runs, tasks,
/// research runs, graph entities/relations/views, recoverable items).
pub fn plan_data(_state: &AppState, request: &OpRequest) -> DataPlan {
    let refusals = request
        .targets
        .iter()
        .filter(|t| {
            !matches!(t, exposition_core::ops::OpTarget::Path { .. } | exposition_core::ops::OpTarget::External { .. })
        })
        .map(|t| OpRefusal {
            target: t.clone(),
            code: "ops.notSupported".into(),
            message: "This kind of item cannot be changed this way yet.".into(),
        })
        .collect();
    DataPlan { refusals, ..DataPlan::default() }
}

/// Apply the non-path part of an approved plan (choices already resolved,
/// defaults filled). Returns one outcome per non-path target.
pub fn apply_data(_state: &AppState, _plan: &OpPlan, _choices: &BTreeMap<String, String>) -> Vec<OpOutcome> {
    Vec::new()
}

/// Before file work: stop/disable what the user chose to stop (active runs or
/// tasks using the paths, enabled triggers watching them), coordinated so no
/// worker writes into a deleted target. Returns per-path refusals when work
/// could not be stopped (those paths are then not deleted).
pub fn prepare_paths(_state: &AppState, _plan: &OpPlan, _choices: &BTreeMap<String, String>) -> Vec<OpRefusal> {
    Vec::new()
}

/// After file work: reconcile project data with the ACTUAL outcomes (deleted
/// paths → tombstones / missing dependencies; created copies → fresh
/// identities with copied-from provenance; restored paths → reconciled).
pub fn after_paths(_state: &AppState, _outcomes: &[OpOutcome]) {}

/// Settings → Storage & History.
pub fn storage_overview(_state: &AppState) -> StorageOverview {
    StorageOverview { categories: Vec::new(), excluded: Vec::new() }
}
