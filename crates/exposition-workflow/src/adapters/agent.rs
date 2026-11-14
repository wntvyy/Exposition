//! `readOnlyAgent` (agent.review): a read-only agent task over the step's
//! declared material, through the existing agents service.
//!
//! The task is read-only (no file or graph changes), with process execution
//! disabled (no Python, Lean, or commands), scoped to the declared material
//! files, and sends that material to the model provider (the plan says so).
//! Its turns count against the run's model-request budget. The final answer
//! becomes the step's review report; the report is prose, never evidence.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_agents::{
    AgentService, Budgets, ContextConsent, CreateTaskRequest, ExecutionMode, PermissionProfile, ProviderKind, TaskId,
    TaskScope, TaskStatus, TranscriptItem,
};
use exposition_core::path::WorkspacePath;
use exposition_graph::OperationRef;
use exposition_graph::workflow::StepPayload;
use exposition_graph::workflow::registry::PortType;

use super::{StepAdapter, StepControl, StepFailure, StepRequest, StepResult};
use crate::environment::{WorkflowEnvironment, WorkflowEnvironmentModel};
use crate::model::{MAX_REPORT_CHARS, WorkflowOutputValue, WorkflowPlanStep};

/// What one review asks for.
pub struct ReviewRequest {
    pub root: PathBuf,
    pub prompt: String,
    pub files: Vec<WorkspacePath>,
    pub max_requests: u32,
    pub time_limit: Duration,
}

/// What a finished review returns.
pub struct ReviewOutcome {
    pub text: String,
    pub task: String,
    pub requests: u32,
}

/// The model behind a review (environment capture).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewModel {
    /// `anthropic`, `scripted`.
    pub provider: String,
    /// The model the task was created with (or the settings' model).
    pub configured: String,
    /// The model that served the latest request, when the provider said.
    pub served_by: Option<String>,
    pub effort: Option<String>,
}

/// Read-only agent reviews (a seam for tests).
pub trait AgentReviewer: Send + Sync {
    fn availability(&self, root: &Path) -> Result<(), String>;
    fn review(
        &self,
        request: &ReviewRequest,
        canceled: &(dyn Fn() -> bool + Sync),
        on_task: &(dyn Fn(&str) + Sync),
    ) -> Result<ReviewOutcome, StepFailure>;
    /// The provider and model of review task `task` (`None`: of a new task
    /// now, from the provider settings). Reads records only.
    fn model(&self, _task: Option<&str>) -> Option<ReviewModel> {
        None
    }
}

/// The environment of a review with `model`.
pub fn review_environment(step: &WorkflowPlanStep, model: Option<ReviewModel>) -> WorkflowEnvironment {
    let mut env = WorkflowEnvironment::of_step(step);
    match model {
        Some(m) => {
            let served = m.served_by.clone().unwrap_or_else(|| m.configured.clone());
            if served != m.configured {
                env.note(format!("Configured model {}; the provider served {served}.", m.configured));
            }
            if let Some(effort) = &m.effort {
                env.note(format!("Effort {effort}."));
            }
            env.model =
                Some(WorkflowEnvironmentModel { provider: m.provider, model: m.configured, served_by: m.served_by });
        }
        None => env.note("The review's model was not reported."),
    }
    env
}

pub struct AgentReviewAdapter {
    pub reviewer: Option<Arc<dyn AgentReviewer>>,
}

const NO_AGENTS: &str = "The agents service is not available.";
const MAX_MATERIAL_TEXT: usize = 32 * 1024;

fn bounded(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    // The ellipsis counts toward the limit (the runner refuses longer reports).
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

impl StepAdapter for AgentReviewAdapter {
    fn availability(&self, root: &Path) -> Result<(), String> {
        self.reviewer.as_ref().ok_or_else(|| NO_AGENTS.to_owned())?.availability(root)
    }

    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let reviewer =
            self.reviewer.as_ref().ok_or_else(|| StepFailure::new("workflow.runnerUnavailable", NO_AGENTS))?;
        let Some(StepPayload::Prompt { text }) = &request.step.payload else {
            return Err(StepFailure::new("workflow.missingPrompt", "The review step has no instructions."));
        };
        if request.model_requests_remaining == 0 {
            return Err(StepFailure::new("workflow.budgetExhausted", "The run has no model requests left."));
        }
        let mut files: Vec<WorkspacePath> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for bound in request.inputs_on("material") {
            match &bound.value {
                WorkflowOutputValue::File { path, .. } | WorkflowOutputValue::Retained { path, .. } => {
                    if !files.contains(path) {
                        files.push(path.clone());
                    }
                }
                WorkflowOutputValue::FormalEvidence { file, verdict, declarations, .. } => {
                    if !files.contains(file) {
                        files.push(file.clone());
                    }
                    let mut note = format!("Lean audit of {file}: {verdict}.");
                    for d in declarations {
                        note.push_str(&format!(
                            "\n- {} ({}){}{}",
                            d.name,
                            d.verdict,
                            d.statement.as_deref().map(|s| format!(": {s}")).unwrap_or_default(),
                            if d.axioms.is_empty() {
                                String::new()
                            } else {
                                format!("; axioms: {}", d.axioms.join(", "))
                            }
                        ));
                    }
                    notes.push(note);
                }
                WorkflowOutputValue::Report { text, .. } => notes.push(format!("Earlier review report:\n{text}")),
                _ => {}
            }
        }
        if files.is_empty() {
            return Err(StepFailure::new(
                "workflow.invalidInput",
                "A review step needs at least one file among its declared material.",
            ));
        }
        let mut prompt = format!(
            "Workflow review step “{}” (read-only: you cannot change files or the project graph, run code, or check Lean; answer with your review).\n\nDeclared material:\n{}",
            request.step.label,
            files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
        );
        if !notes.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&bounded(&notes.join("\n\n"), MAX_MATERIAL_TEXT));
        }
        prompt.push_str("\n\nInstructions:\n");
        prompt.push_str(text);
        let review = ReviewRequest {
            root: request.root.to_path_buf(),
            prompt,
            files,
            max_requests: request.model_requests_remaining,
            time_limit: request.time_limit,
        };
        let started: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        let on_task = |task: &str| {
            *started.lock().unwrap_or_else(|p| p.into_inner()) = Some(task.to_owned());
            (control.on_dispatched)(&OperationRef { kind: "agentTask".into(), id: task.into() })
        };
        let environment = || {
            let task = started.lock().unwrap_or_else(|p| p.into_inner()).clone();
            review_environment(request.step, task.as_deref().and_then(|t| reviewer.model(Some(t))))
        };
        let outcome = reviewer
            .review(&review, control.canceled, &on_task)
            .map_err(|f| StepFailure { environment: Some(environment()), ..f })?;
        let operation = OperationRef { kind: "agentTask".into(), id: outcome.task.clone() };
        let value =
            WorkflowOutputValue::Report { text: bounded(&outcome.text, MAX_REPORT_CHARS), task: Some(outcome.task) };
        Ok(StepResult {
            operation: Some(operation.clone()),
            outputs: vec![request.output("report", PortType::ReviewReport, value, &operation)],
            model_requests: outcome.requests,
            environment: Some(environment()),
            ..Default::default()
        })
    }

    fn current_environment(&self, _root: &Path, step: &WorkflowPlanStep) -> Option<WorkflowEnvironment> {
        let model = self.reviewer.as_ref()?.model(None)?;
        Some(review_environment(step, Some(model)))
    }
}

/// The application's agents service.
pub struct AgentServiceReviewer {
    pub agents: AgentService,
}

fn display_root(root: &Path) -> String {
    let s = root.to_string_lossy().into_owned();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_owned(),
        _ => s,
    }
}

impl AgentServiceReviewer {
    fn stop(&self, task: &TaskId) {
        let _ = self.agents.cancel_task(task);
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.agents.is_running(task) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The serde name of a provider kind or effort (`anthropic`, `medium`).
fn wire_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

impl AgentReviewer for AgentServiceReviewer {
    fn model(&self, task: Option<&str>) -> Option<ReviewModel> {
        match task {
            Some(task) => {
                let summary = self.agents.task(&TaskId(task.to_owned())).ok()?;
                Some(ReviewModel {
                    provider: wire_name(&summary.spec.provider.provider),
                    configured: summary.spec.provider.model.clone(),
                    served_by: summary.usage.served_by.clone(),
                    effort: Some(wire_name(&summary.spec.provider.effort)),
                })
            }
            None => {
                let settings = self.agents.provider_status().settings;
                Some(ReviewModel {
                    provider: wire_name(&settings.provider),
                    configured: settings.model.clone(),
                    served_by: None,
                    effort: Some(wire_name(&settings.effort)),
                })
            }
        }
    }

    fn availability(&self, _root: &Path) -> Result<(), String> {
        let status = self.agents.provider_status();
        match status.settings.provider {
            ProviderKind::Scripted if status.scripted_available => Ok(()),
            ProviderKind::Scripted => Err("The scripted provider is not available in this build.".into()),
            ProviderKind::Anthropic if status.configured => Ok(()),
            ProviderKind::Anthropic => Err(status
                .credential_error
                .unwrap_or_else(|| "No model provider key is configured; add one in Settings.".into())),
        }
    }

    fn review(
        &self,
        request: &ReviewRequest,
        canceled: &(dyn Fn() -> bool + Sync),
        on_task: &(dyn Fn(&str) + Sync),
    ) -> Result<ReviewOutcome, StepFailure> {
        let wall_ms = request.time_limit.as_millis().clamp(1_000, 24 * 60 * 60 * 1000) as u64;
        let budgets = Budgets {
            max_turns: request.max_requests.clamp(1, 500),
            max_wall_ms: wall_ms,
            max_commands: 0,
            ..Budgets::default()
        };
        let summary = self
            .agents
            .create_task(CreateTaskRequest {
                prompt: request.prompt.clone(),
                workspace_root: display_root(&request.root),
                scope: TaskScope::Files { paths: request.files.clone() },
                permission: PermissionProfile::ReadOnly,
                execution: ExecutionMode::Disabled,
                consent: Some(ContextConsent::ScopeContent),
                budgets: Some(budgets),
            })
            .map_err(|e| StepFailure::new(&e.code, e.message))?;
        let task = summary.id.clone();
        on_task(&task.0);
        let deadline = Instant::now() + request.time_limit + Duration::from_secs(30);
        // A failed review still made its requests: report them.
        let failure = |code: &str, message: String| StepFailure {
            operation: Some(OperationRef { kind: "agentTask".into(), id: task.0.clone() }),
            model_requests: self.agents.task(&task).map(|s| s.usage.turns).unwrap_or(0),
            ..StepFailure::new(code, message)
        };
        let summary = loop {
            if canceled() {
                self.stop(&task);
                return Err(failure("workflow.canceled", "The review was canceled.".into()));
            }
            if Instant::now() > deadline {
                self.stop(&task);
                return Err(failure("workflow.timeout", "The review did not finish within its time limit.".into()));
            }
            let summary = self.agents.task(&task).map_err(|e| failure(&e.code, e.message))?;
            match &summary.status {
                TaskStatus::Queued | TaskStatus::Running => std::thread::sleep(Duration::from_millis(200)),
                TaskStatus::AwaitingApproval => {
                    self.stop(&task);
                    return Err(failure(
                        "agent.approvalRequested",
                        "The read-only review asked for an approval; workflow review steps never approve anything, so it was stopped."
                            .into(),
                    ));
                }
                TaskStatus::Completed => break summary,
                TaskStatus::Canceled => {
                    return Err(failure("workflow.canceled", "The review task was canceled.".into()));
                }
                TaskStatus::BudgetExceeded { detail, .. } => {
                    return Err(failure("workflow.budgetExhausted", format!("The review ran out of budget: {detail}")));
                }
                TaskStatus::Refused { explanation, .. } => {
                    return Err(failure(
                        "agent.refused",
                        explanation.clone().unwrap_or_else(|| "The model declined the request.".into()),
                    ));
                }
                TaskStatus::Failed { message } => return Err(failure("agent.failed", message.clone())),
                TaskStatus::Stopped { message, .. } => return Err(failure("agent.stopped", message.clone())),
                other => return Err(failure("agent.failed", format!("The review task ended: {other:?}"))),
            }
        };
        let detail = self.agents.task_detail(&task).map_err(|e| failure(&e.code, e.message))?;
        let text = detail
            .transcript
            .iter()
            .rev()
            .find_map(|item| match item {
                TranscriptItem::AssistantText { text, .. } if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            })
            .ok_or_else(|| failure("agent.noAnswer", "The review finished without an answer.".into()))?;
        Ok(ReviewOutcome { text, task: task.0, requests: summary.usage.turns })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bounded_report_fits_the_runner_limit() {
        let long = "x".repeat(MAX_REPORT_CHARS + 10);
        let out = bounded(&long, MAX_REPORT_CHARS);
        assert_eq!(out.chars().count(), MAX_REPORT_CHARS, "truncated text plus the ellipsis fit the limit");
        assert!(out.ends_with('…'));
        let exact = "y".repeat(MAX_REPORT_CHARS);
        assert_eq!(bounded(&exact, MAX_REPORT_CHARS), exact, "text at the limit is kept whole");
    }
}
