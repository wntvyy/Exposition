//! `graphProposal` (formal.associateEvidence): select declarations from the
//! upstream audit's returned list and propose their evidence associations for
//! review. Nothing is recorded here: the user's decision
//! (`workflow_association_decide`) records evidence through the Lean evidence
//! path for the accepted declarations only. Never asserts that a formal
//! declaration is equivalent to an informal statement.

use std::path::Path;

use exposition_graph::OperationRef;
use exposition_graph::workflow::registry::PortType;

use super::{StepAdapter, StepControl, StepFailure, StepRequest, StepResult};
use crate::model::{WorkflowOutputValue, WorkflowProposalStatus};

/// At most this many declarations in one proposal.
pub const MAX_PROPOSED: usize = 200;

pub struct AssociateAdapter;

/// Names chosen in the `declarations` parameter (one per line; blank lines ignored).
pub fn chosen(parameter: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in parameter.unwrap_or_default().lines() {
        let name = line.trim();
        if !name.is_empty() && !out.iter().any(|n| n == name) {
            out.push(name.to_owned());
        }
    }
    out
}

impl StepAdapter for AssociateAdapter {
    fn availability(&self, _root: &Path) -> Result<(), String> {
        Ok(())
    }

    fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
        let evidence: Vec<_> = request.inputs_on("evidence").collect();
        let [bound] = evidence.as_slice() else {
            return Err(StepFailure::new("workflow.invalidInput", "the step needs exactly one audit result"));
        };
        let WorkflowOutputValue::FormalEvidence { declarations, .. } = &bound.value else {
            return Err(StepFailure::new("workflow.invalidInput", "the input is not a Lean audit result"));
        };
        let audited: Vec<&str> = declarations.iter().map(|d| d.name.as_str()).collect();
        let wanted = chosen(request.text_parameter("declarations"));
        let selected: Vec<String> = if wanted.is_empty() {
            audited.iter().map(|n| (*n).to_owned()).collect()
        } else {
            let missing: Vec<&String> = wanted.iter().filter(|w| !audited.contains(&w.as_str())).collect();
            if !missing.is_empty() {
                return Err(StepFailure::new(
                    "workflow.declarationNotAudited",
                    format!(
                        "{} {} not in the audit's declaration list; only audited declarations can be associated",
                        missing.iter().take(5).map(|m| format!("“{m}”")).collect::<Vec<_>>().join(", "),
                        if missing.len() == 1 { "is" } else { "are" }
                    ),
                ));
            }
            wanted
        };
        if selected.is_empty() {
            return Err(StepFailure::new(
                "workflow.nothingAudited",
                "The audit returned no declarations to associate.",
            ));
        }
        if selected.len() > MAX_PROPOSED {
            return Err(StepFailure::new(
                "workflow.tooLarge",
                format!("at most {MAX_PROPOSED} declarations can be proposed at once"),
            ));
        }
        let proposal = format!("assoc-{}", uuid::Uuid::new_v4().simple());
        let operation = OperationRef { kind: "associationProposal".into(), id: proposal.clone() };
        (control.on_dispatched)(&operation);
        let value = WorkflowOutputValue::AssociationProposal {
            proposal,
            declarations: selected,
            status: WorkflowProposalStatus::Pending,
            accepted: Vec::new(),
            evidence: Vec::new(),
        };
        Ok(StepResult {
            operation: Some(operation.clone()),
            outputs: vec![request.output("proposal", PortType::AssociationProposal, value, &operation)],
            ..Default::default()
        })
    }
}
