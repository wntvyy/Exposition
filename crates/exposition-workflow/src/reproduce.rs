//! Reproduce an earlier run (M9 reproducibility).
//!
//! A reproduction is a normal reviewable plan of the same revision with every
//! external input pinned to the SHA-256 the original run read (a file that
//! changed since blocks the plan with the normal `workflow.inputChanged`), and
//! with the cause `Reproduce { of }`. Before approval it shows the
//! environment drift between what each step recorded and what it would get
//! now (image identity, interpreter and toolchain versions, package lists,
//! model), assembled without running anything — as structured items and
//! summarized in the plan's notices. Starting it is the normal approval path
//! ([`WorkflowRunService::start`]): the plan is recomputed with the same pins
//! and must still match its approval hash, and the run records the cause.
//! Batch steps receive the seeds the original steps received (seeds derive
//! from the original run; see `environment::seed_basis`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use exposition_graph::WorkflowRunId;
use exposition_graph::workflow::registry::Containment;
use exposition_research::batch::{BatchMode, BatchRunner};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::adapters::agent::{AgentReviewAdapter, AgentReviewer};
use crate::adapters::batch::BatchAdapter;
use crate::adapters::{Adapters, runner_ids};
use crate::environment::{self, WorkflowEnvironment, WorkflowEnvironmentDrift};
use crate::model::{WorkflowPlanStep, WorkflowRun, WorkflowRunCause, WorkflowRunError, WorkflowRunPlan};
use crate::service::{PlanOptions, WorkflowRunService};

/// A reproduction plan: start `plan` with the normal start command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowReproducePlan {
    /// The normal plan (pinned inputs, cause Reproduce; drift summarized in
    /// its notices).
    pub plan: WorkflowRunPlan,
    /// The run it reproduces.
    pub reproduces: WorkflowRunId,
    /// Environment drift per step, recorded → now.
    pub drift: Vec<WorkflowEnvironmentDrift>,
}

/// The environment each step would get now (never runs anything).
pub type CurrentEnvironment<'a> = dyn Fn(&WorkflowRunPlan, &WorkflowPlanStep) -> Option<WorkflowEnvironment> + 'a;

/// Adapters that can describe the current environment of their steps: the
/// batch runner's configured image and caches, the agents' provider
/// settings. (Lean steps are described from files; see [`current_environment`].)
pub fn environment_sources(batch: Option<Arc<dyn BatchRunner>>, reviewer: Option<Arc<dyn AgentReviewer>>) -> Adapters {
    let mut adapters = Adapters::new();
    adapters.insert(
        runner_ids::RESTRICTED_PYTHON.into(),
        Arc::new(BatchAdapter::new(batch.clone(), BatchMode::Restricted)),
    );
    adapters.insert(runner_ids::NATIVE_PYTHON.into(), Arc::new(BatchAdapter::new(batch, BatchMode::TrustedNative)));
    adapters.insert(runner_ids::READ_ONLY_AGENT.into(), Arc::new(AgentReviewAdapter { reviewer }));
    adapters
}

/// The environment `step` of `plan` would get now in the project at `root`:
/// what its runner's adapter knows, Lean audits from the files around the
/// audited file, otherwise `None` (only the base environment is known).
pub fn current_environment(
    root: &Path,
    adapters: &Adapters,
    plan: &WorkflowRunPlan,
    step: &WorkflowPlanStep,
) -> Option<WorkflowEnvironment> {
    if step.runner == runner_ids::LEAN_AUDIT {
        return crate::adapters::lean::current_audit_environment(root, &plan.inputs, step);
    }
    adapters.get(&step.runner).and_then(|a| a.current_environment(root, step))
}

fn short(hash: &str) -> String {
    crate::adapters::bare_hash(hash).chars().take(12).collect()
}

/// Environment drift of every step of `original` that recorded an
/// environment, instances of repeated steps included (`unrecorded` items for
/// steps that ran without one, an `unknown` item when the current
/// environment cannot be determined). Identical items of several instances
/// are reported once; an item that holds for only some instances names them.
pub fn reproduction_drift(original: &WorkflowRun, current: &CurrentEnvironment<'_>) -> Vec<WorkflowEnvironmentDrift> {
    let mut out = Vec::new();
    for planned in &original.plan.steps {
        let Some(step) = original.steps.iter().find(|s| s.step == planned.step) else { continue };
        let recorded = environment::recorded_all(step);
        let ran = !step.attempts.is_empty() || step.instances.iter().any(|i| !i.attempts.is_empty());
        let item = |aspect: &str, message: &str| WorkflowEnvironmentDrift {
            step: Some(planned.step.clone()),
            aspect: aspect.into(),
            recorded: None,
            current: None,
            message: message.into(),
        };
        if recorded.is_empty() {
            if ran {
                out.push(item("unrecorded", "no environment was recorded (the run predates environment capture)"));
            }
            continue;
        }
        let now = match current(&original.plan, planned) {
            Some(now) => now,
            // An application service step has nothing beyond its base.
            None if planned.containment == Containment::ApplicationService => WorkflowEnvironment::of_step(planned),
            None => {
                out.push(item(
                    "unknown",
                    "the environment it would get now cannot be determined without running anything (is the runner configured and available?)",
                ));
                continue;
            }
        };
        // (item, instances it holds for), in first-seen order.
        let mut groups: Vec<(WorkflowEnvironmentDrift, Vec<u32>)> = Vec::new();
        for (instance, env) in &recorded {
            for d in environment::drift(env, &now) {
                let d = WorkflowEnvironmentDrift { step: Some(planned.step.clone()), ..d };
                match groups.iter_mut().find(|(g, _)| g == &d) {
                    Some((_, instances)) => instances.extend(*instance),
                    None => groups.push((d, instance.iter().copied().collect())),
                }
            }
        }
        let repeated = recorded.iter().filter(|(i, _)| i.is_some()).count();
        for (mut d, instances) in groups {
            if repeated > 0 && instances.len() < repeated {
                let list = instances.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                d.message = format!("{} (instance{} {list})", d.message, if instances.len() == 1 { "" } else { "s" });
            }
            out.push(d);
        }
    }
    out
}

/// The notices a reproduction of `original` shows: what it reproduces, the
/// drift summarized per step, and the seeds (per instance of a repeated step).
pub fn reproduction_notices(original: &WorkflowRun, drift: &[WorkflowEnvironmentDrift]) -> Vec<String> {
    let mut notices = vec![format!(
        "This plan reproduces run {} (revision {}): every input is pinned to the SHA-256 that run read, and a changed input blocks the plan. Environment differences are shown so you can judge them; they do not block the run.",
        original.id, original.revision
    )];
    let label = |id: &str| {
        original.plan.steps.iter().find(|s| s.step == id).map(|s| s.label.clone()).unwrap_or_else(|| id.to_owned())
    };
    let mut by_step: BTreeMap<usize, (String, Vec<&str>)> = BTreeMap::new();
    for d in drift {
        let Some(step) = &d.step else { continue };
        let order = original.plan.steps.iter().position(|s| &s.step == step).unwrap_or(usize::MAX);
        by_step.entry(order).or_insert_with(|| (label(step), Vec::new())).1.push(&d.message);
    }
    for (step, messages) in by_step.values() {
        notices.push(format!("Environment drift in “{step}”: {}.", messages.join("; ")));
    }
    let mut any_recorded = false;
    let mut seeds = Vec::new();
    for planned in &original.plan.steps {
        let Some(step) = original.steps.iter().find(|s| s.step == planned.step) else { continue };
        for (instance, env) in environment::recorded_all(step) {
            any_recorded = true;
            if let Some(seed) = env.seed {
                seeds.push(match instance {
                    Some(k) => format!("“{}” #{k} {seed}", planned.label),
                    None => format!("“{}” {seed}", planned.label),
                });
            }
        }
    }
    if any_recorded && drift.is_empty() {
        notices.push(
            "No environment drift found in what can be checked without running anything (image identity, interpreter and toolchain versions, package lists, model)."
                .to_owned(),
        );
    }
    if !seeds.is_empty() {
        notices.push(format!(
            "Batch steps receive the same {} values as in the original run ({}).",
            environment::SEED_VAR,
            seeds.join(", ")
        ));
    }
    notices
}

impl WorkflowRunService {
    /// A reviewable plan reproducing run `of` (see the module docs). Start
    /// its `plan` with [`WorkflowRunService::start`] like any plan.
    pub fn reproduce_plan(
        &self,
        of: &WorkflowRunId,
        current: &CurrentEnvironment<'_>,
    ) -> Result<WorkflowReproducePlan, WorkflowRunError> {
        let original = self.get(of)?;
        if !original.state.is_terminal() {
            return Err(WorkflowRunError::new(
                "workflow.runActive",
                "This run is still active; reproduce it after it finishes.",
            ));
        }
        let pins: BTreeMap<String, String> =
            original.plan.inputs.iter().map(|i| (i.id.clone(), crate::adapters::bare_hash(&i.sha256))).collect();
        let drift = reproduction_drift(&original, current);
        let notices = reproduction_notices(&original, &drift);
        let options = PlanOptions { cause: WorkflowRunCause::Reproduce { of: of.clone() }, pins, notices };
        let mut plan = self.plan_with(&original.workflow, Some(original.revision), options)?;
        if plan.definition_hash != original.definition_hash {
            return Err(WorkflowRunError::new(
                "workflow.reproduceRevisionChanged",
                format!(
                    "Revision {} of this workflow no longer has the content run {of} ran (definition {} → {}); it cannot be reproduced.",
                    original.revision,
                    short(&original.definition_hash),
                    short(&plan.definition_hash)
                ),
            ));
        }
        for blocker in plan.blockers.iter_mut().filter(|b| b.code == "workflow.inputChanged") {
            let Some(input) = blocker.input.as_deref() else { continue };
            let (Some(recorded), Some(now)) =
                (original.plan.inputs.iter().find(|i| i.id == input), plan.inputs.iter().find(|i| i.id == input))
            else {
                continue;
            };
            blocker.message = format!(
                "{} changed since run {of} read it (recorded sha256 {}, now {}); restore that version to reproduce the run.",
                recorded.path,
                short(&recorded.sha256),
                short(&now.sha256)
            );
        }
        Ok(WorkflowReproducePlan { plan, reproduces: of.clone(), drift })
    }
}
