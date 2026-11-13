//! `researchRetention` (artifact.retain) and `reviewedPromotion`
//! (manuscript.requestPromotion) over the research project service.
//!
//! Retention keeps a validated research-run output (`ResearchProject::retain`,
//! which re-verifies the artifact digest itself). Promotion prepares the
//! existing reviewed insertion of a retained artifact into a manuscript; the
//! user approves or cancels it through the existing promotion review.

use std::path::Path;
use std::sync::Arc;

use exposition_core::artifact::AttachmentPlacement;
use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::registry::PortType;
use exposition_graph::{EntityId, EvidenceId, OperationRef};
use exposition_research::ResearchProject;
use exposition_research::project::types::{PromotionRequest, RetainRequest, RetainState};
use exposition_workspace::WorkspaceService;

use super::{StepAdapter, StepControl, StepFailure, StepRequest, StepResult, bare_hash};
use crate::model::{WorkflowOutputValue, WorkflowProposalStatus};

/// A retained result as the research service reports it.
#[derive(Clone, Debug)]
pub struct RetainedRecord {
    pub entity: EntityId,
    pub path: WorkspacePath,
    pub sha256: String,
    pub evidence: Option<EvidenceId>,
}

/// The research operations workflow steps use (a seam for tests).
pub trait ResearchActions: Send + Sync {
    fn available(&self, root: &Path) -> Result<(), String>;
    fn retain(&self, research_run: &str, artifact: &str) -> Result<RetainedRecord, StepFailure>;
    /// Prepare a reviewed promotion; returns the promotion id.
    fn prepare_promotion(
        &self,
        research_run: &str,
        artifact: &str,
        document: &WorkspacePath,
        caption: Option<String>,
    ) -> Result<String, StepFailure>;
}

/// The research project service of the open project.
pub struct ProjectResearch {
    pub project: Arc<ResearchProject>,
    pub workspace: Arc<WorkspaceService>,
}

fn research_failure(e: exposition_research::ResearchError) -> StepFailure {
    StepFailure::new(&e.code(), e.to_string())
}

impl ResearchActions for ProjectResearch {
    fn available(&self, root: &Path) -> Result<(), String> {
        match self.project.graph().root() {
            Some(graph_root) if graph_root == root => Ok(()),
            Some(_) | None => Err("The project graph is not attached for this project.".into()),
        }
    }

    fn retain(&self, research_run: &str, artifact: &str) -> Result<RetainedRecord, StepFailure> {
        let outcome = self
            .project
            .retain(&RetainRequest {
                run_id: research_run.to_owned(),
                artifact_id: artifact.to_owned(),
                state: RetainState::Active,
                experiment: None,
            })
            .map_err(research_failure)?;
        let sha256 = outcome.result.content_hash.as_deref().map(bare_hash).unwrap_or_default();
        Ok(RetainedRecord {
            entity: outcome.result.entity,
            path: outcome.result.path,
            sha256,
            evidence: outcome.evidence,
        })
    }

    fn prepare_promotion(
        &self,
        research_run: &str,
        artifact: &str,
        document: &WorkspacePath,
        caption: Option<String>,
    ) -> Result<String, StepFailure> {
        let opened = self
            .workspace
            .open_document(document)
            .map_err(|e| StepFailure::new(e.code, format!("{document} could not be opened: {}", e.message)))?;
        let info = opened.state.info;
        // Insert at the end of the manuscript (the review shows exactly where).
        let offset = opened.state.text.encode_utf16().count() as u32;
        let preview = self
            .project
            .prepare_promotion(&PromotionRequest {
                run_id: research_run.to_owned(),
                artifact_id: artifact.to_owned(),
                document: info.id,
                base_revision: Some(info.revision),
                placement: AttachmentPlacement::Insert { offset },
                caption,
                attachment_key: None,
            })
            .map_err(research_failure)?;
        Ok(preview.promotion)
    }
}

pub struct RetainAdapter {
    pub research: Option<Arc<dyn ResearchActions>>,
}

pub struct PromotionAdapter {
    pub research: Option<Arc<dyn ResearchActions>>,
}

const NO_RESEARCH: &str = "The research project service is not available.";

fn run_artifact(value: &WorkflowOutputValue) -> Option<(&WorkspacePath, &str, &str)> {
    match value {
        WorkflowOutputValue::File { path, research_run: Some(run), artifact: Some(artifact), .. } => {
            Some((path, run.as_str(), artifact.as_str()))
        }
        WorkflowOutputValue::Retained { path, research_run: Some(run), artifact: Some(artifact), .. } => {
            Some((path, run.as_str(), artifact.as_str()))
        }
        _ => None,
    }
}

impl StepAdapter for RetainAdapter {
    fn availability(&self, root: &Path) -> Result<(), String> {
        self.research.as_ref().ok_or_else(|| NO_RESEARCH.to_owned())?.available(root)
    }

    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let research =
            self.research.as_ref().ok_or_else(|| StepFailure::new("workflow.runnerUnavailable", NO_RESEARCH))?;
        let bound: Vec<_> = request.inputs_on("artifact").collect();
        let [bound] = bound.as_slice() else {
            return Err(StepFailure::new("workflow.invalidInput", "the step needs exactly one output to keep"));
        };
        let Some((_, run, artifact)) = run_artifact(&bound.value) else {
            return Err(StepFailure::new(
                "workflow.notRunOutput",
                "Only an output of a research run can be retained (a project file is already part of the project).",
            ));
        };
        let operation = OperationRef { kind: "retain".into(), id: format!("{run}/{artifact}") };
        (control.on_dispatched)(&operation);
        let kept = research.retain(run, artifact)?;
        let value = WorkflowOutputValue::Retained {
            entity: kept.entity,
            path: kept.path,
            sha256: kept.sha256,
            evidence: kept.evidence,
            research_run: Some(run.to_owned()),
            artifact: Some(artifact.to_owned()),
        };
        Ok(StepResult {
            operation: Some(operation.clone()),
            outputs: vec![request.output("retained", PortType::RetainedArtifact, value, &operation)],
            ..Default::default()
        })
    }
}

impl StepAdapter for PromotionAdapter {
    fn availability(&self, root: &Path) -> Result<(), String> {
        self.research.as_ref().ok_or_else(|| NO_RESEARCH.to_owned())?.available(root)
    }

    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let research =
            self.research.as_ref().ok_or_else(|| StepFailure::new("workflow.runnerUnavailable", NO_RESEARCH))?;
        let bound: Vec<_> = request.inputs_on("artifact").collect();
        let [bound] = bound.as_slice() else {
            return Err(StepFailure::new("workflow.invalidInput", "the step needs exactly one retained artifact"));
        };
        let Some((_, run, artifact)) = run_artifact(&bound.value) else {
            return Err(StepFailure::new("workflow.invalidInput", "the input is not a retained research output"));
        };
        let document =
            request.text_parameter("document").and_then(|d| WorkspacePath::parse(d).ok()).ok_or_else(|| {
                StepFailure::new("workflow.invalidParameter", "the manuscript path is missing or invalid")
            })?;
        let caption = request.text_parameter("caption").map(str::to_owned).filter(|c| !c.trim().is_empty());
        let promotion = research.prepare_promotion(run, artifact, &document, caption)?;
        let operation = OperationRef { kind: "promotion".into(), id: promotion.clone() };
        (control.on_dispatched)(&operation);
        let value =
            WorkflowOutputValue::PromotionProposal { promotion, document, status: WorkflowProposalStatus::Pending };
        Ok(StepResult {
            operation: Some(operation.clone()),
            outputs: vec![request.output("proposal", PortType::PromotionProposal, value, &operation)],
            ..Default::default()
        })
    }
}
