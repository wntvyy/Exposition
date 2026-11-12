//! Saved experiments: an objective and procedure (a research script), its
//! declared inputs (`experiment derivedFrom input`), and its retained
//! outcomes (`outcome derivedFrom experiment`). Attempts are the runs of the
//! procedure, listed from run history (operational records, not entities).
//! A historical run never becomes an experiment by itself.

use std::collections::{BTreeMap, BTreeSet};

use exposition_core::path::WorkspacePath;
use exposition_graph::{
    ApplyOutcome, AssertionStatus, ChangeRequest, Entity, EntityId, EntityKind, EntityRef, GraphOperation, Lifecycle,
    Origin, RelationKind, ResourceRole,
};

use super::space::experiment_summary;
use super::types::{ExperimentDetail, ExperimentFile, ExperimentInput};
use super::{ResearchProject, label};
use crate::error::ResearchError;
use crate::types::CodeOrigin;

const MAX_INPUTS: usize = 32;
const MAX_OUTCOMES: usize = 64;
const MAX_OBJECTIVE: usize = 4000;
const MAX_ATTEMPTS: usize = 50;

fn invalid(message: impl Into<String>) -> ResearchError {
    ResearchError::coded("research.invalidExperiment", message)
}

impl ResearchProject {
    fn experiment_entity(&self, id: &EntityId) -> Result<Entity, ResearchError> {
        let e = self.entity(id)?;
        if e.kind != EntityKind::Experiment {
            return Err(ResearchError::coded("research.notExperiment", "That entity is not an experiment."));
        }
        Ok(e)
    }

    /// Create or update an experiment. What it asserts is the person's own
    /// statement: it goes through the validated change path as the user.
    pub fn save_experiment(&self, input: &ExperimentInput) -> Result<ExperimentDetail, ResearchError> {
        let _ = self.graph_root()?;
        if input.title.trim().is_empty() {
            return Err(invalid("Give the experiment a title."));
        }
        if input.objective.len() > MAX_OBJECTIVE {
            return Err(invalid(format!("The objective is longer than {MAX_OBJECTIVE} bytes.")));
        }
        if input.inputs.len() > MAX_INPUTS || input.outcomes.len() > MAX_OUTCOMES {
            return Err(invalid(format!("At most {MAX_INPUTS} inputs and {MAX_OUTCOMES} outcomes.")));
        }
        // Resource identities of the procedure and declared inputs.
        let mut targets: Vec<EntityId> = Vec::new();
        if let Some(procedure) = &input.procedure {
            let e = self.graph.ensure_resource(procedure, None, false, super::SERVICE)?;
            if !e.roles.contains(&ResourceRole::Script) {
                return Err(invalid(format!("{procedure} is not a Python, R, or Julia script.")));
            }
            targets.push(e.id);
        }
        for path in &input.inputs {
            let e = self.graph.ensure_resource(path, None, false, super::SERVICE)?;
            if !targets.contains(&e.id) {
                targets.push(e.id);
            }
        }
        for outcome in &input.outcomes {
            let e = self.entity(outcome)?;
            if e.kind != EntityKind::Resource || !e.roles.contains(&ResourceRole::Artifact) {
                return Err(invalid(format!("{} is not a retained result.", e.label)));
            }
        }
        let mut attributes = BTreeMap::new();
        attributes.insert("objective".to_owned(), input.objective.clone());
        if let Some(p) = &input.procedure {
            attributes.insert("procedure".to_owned(), p.to_string());
        }
        let space = self.space()?;
        let base = self.graph.with_store(|s| Ok(s.revision()?))?;
        let mut ops = Vec::new();
        let me: EntityRef;
        let mut existing_out: Vec<(EntityId, exposition_graph::RelationId, AssertionStatus)> = Vec::new();
        let mut existing_in: Vec<(EntityId, exposition_graph::RelationId, AssertionStatus)> = Vec::new();
        let mut member = false;
        match &input.entity {
            None => {
                ops.push(GraphOperation::CreateEntity {
                    temp_id: "experiment".into(),
                    kind: EntityKind::Experiment,
                    roles: Vec::new(),
                    label: label(&input.title),
                    key: None,
                    attributes,
                    binding: None,
                });
                me = EntityRef::Temp { temp_id: "experiment".into() };
            }
            Some(id) => {
                let e = self.experiment_entity(id)?;
                let expected = input
                    .expected_revision
                    .ok_or_else(|| invalid("Updating an experiment needs the revision it was read at."))?;
                ops.push(GraphOperation::UpdateEntity {
                    entity: id.clone(),
                    expected_revision: expected,
                    label: Some(label(&input.title)),
                    attributes: Some(attributes),
                });
                if e.lifecycle == Lifecycle::Tombstoned {
                    return Err(invalid("That experiment no longer exists."));
                }
                let (out, inc) = self.graph.with_store(|s| Ok((s.relations_from(id)?, s.relations_to(id)?)))?;
                for r in out {
                    if r.kind == RelationKind::DerivedFrom {
                        existing_out.push((r.to, r.id, r.status));
                    } else if r.kind == RelationKind::MemberOf && space.as_ref().is_some_and(|s| s.entity == r.to) {
                        member = true;
                    }
                }
                for r in inc {
                    if r.kind == RelationKind::DerivedFrom {
                        existing_in.push((r.from, r.id, r.status));
                    }
                }
                me = EntityRef::Id { id: id.clone() };
            }
        }
        if let Some(space) = &space
            && !member
        {
            ops.push(GraphOperation::AddRelation {
                kind: RelationKind::MemberOf,
                from: me.clone(),
                to: EntityRef::Id { id: space.entity.clone() },
                evidence_kind: None,
                evidence: Vec::new(),
                note: None,
            });
        }
        let live = |status: AssertionStatus| status != AssertionStatus::Rejected;
        // Declared procedure/inputs.
        let wanted: BTreeSet<&EntityId> = targets.iter().collect();
        for (to, rel, status) in &existing_out {
            if !wanted.contains(to) && *status != AssertionStatus::Observed && live(*status) {
                ops.push(GraphOperation::RemoveRelation { relation: rel.clone() });
            }
        }
        for t in &targets {
            if !existing_out.iter().any(|(to, _, s)| to == t && live(*s)) {
                ops.push(GraphOperation::AddRelation {
                    kind: RelationKind::DerivedFrom,
                    from: me.clone(),
                    to: EntityRef::Id { id: t.clone() },
                    evidence_kind: None,
                    evidence: Vec::new(),
                    note: Some("declared input of the experiment".into()),
                });
            }
        }
        // Outcomes.
        let outcomes: BTreeSet<&EntityId> = input.outcomes.iter().collect();
        for (from, rel, status) in &existing_in {
            if !outcomes.contains(from) && *status != AssertionStatus::Observed && live(*status) {
                ops.push(GraphOperation::RemoveRelation { relation: rel.clone() });
            }
        }
        for o in &input.outcomes {
            if !existing_in.iter().any(|(from, _, s)| from == o && live(*s)) {
                ops.push(GraphOperation::AddRelation {
                    kind: RelationKind::DerivedFrom,
                    from: EntityRef::Id { id: o.clone() },
                    to: me.clone(),
                    evidence_kind: None,
                    evidence: Vec::new(),
                    note: Some("outcome of the experiment".into()),
                });
            }
        }
        let request = ChangeRequest {
            base_revision: base,
            operations: ops,
            summary: Some(format!("Save experiment {}", label(&input.title))),
        };
        let (proposal, outcome) = self.graph.propose_and_apply(Origin::User, &request)?;
        let id = match outcome {
            ApplyOutcome::Applied { created, .. } => match &input.entity {
                Some(id) => id.clone(),
                None => created
                    .into_iter()
                    .find(|c| c.temp_id == "experiment")
                    .map(|c| c.id)
                    .ok_or_else(|| ResearchError::coded("graph.storage", "the experiment was not created"))?,
            },
            ApplyOutcome::Conflict { diagnostics, .. } => {
                return Err(ResearchError::coded(
                    "research.experimentChanged",
                    diagnostics
                        .first()
                        .map(|d| d.message.clone())
                        .unwrap_or_else(|| "The experiment changed since it was opened; reopen it.".into()),
                ));
            }
            ApplyOutcome::Invalid { diagnostics, .. }
                if diagnostics.iter().chain(&proposal.diagnostics).any(|d| d.code == "graph.staleEntity") =>
            {
                return Err(ResearchError::coded(
                    "research.experimentChanged",
                    "The experiment changed since it was opened; reopen it and save again.",
                ));
            }
            ApplyOutcome::Invalid { diagnostics, .. } => {
                let message = diagnostics
                    .iter()
                    .chain(&proposal.diagnostics)
                    .map(|d| d.message.clone())
                    .next()
                    .unwrap_or_else(|| "The experiment is invalid.".into());
                return Err(invalid(message));
            }
        };
        self.experiment(&id)
    }

    /// An experiment with its procedure, inputs, outcomes, and attempts.
    pub fn experiment(&self, id: &EntityId) -> Result<ExperimentDetail, ResearchError> {
        let e = self.experiment_entity(id)?;
        let summary = experiment_summary(&e);
        let (out, inc) = self.graph.with_store(|s| Ok((s.relations_from(id)?, s.relations_to(id)?)))?;
        let mut procedure = None;
        let mut inputs = Vec::new();
        for r in out.iter().filter(|r| r.kind == RelationKind::DerivedFrom && r.status != AssertionStatus::Rejected) {
            let t = self.entity(&r.to)?;
            let Some(path) = t.key.as_deref().and_then(|k| WorkspacePath::parse(k).ok()) else { continue };
            if t.kind != EntityKind::Resource {
                continue;
            }
            let file = ExperimentFile { path: path.clone(), entity: t.id.clone() };
            if summary.procedure.as_ref() == Some(&path) {
                procedure = Some(file);
            } else {
                inputs.push(file);
            }
        }
        let mut outcomes = Vec::new();
        for r in inc.iter().filter(|r| r.kind == RelationKind::DerivedFrom && r.status != AssertionStatus::Rejected) {
            let o = self.entity(&r.from)?;
            if o.kind == EntityKind::Resource && o.roles.contains(&ResourceRole::Artifact) {
                outcomes.push(self.retained_result(&o)?);
            }
        }
        let outcome_runs: BTreeSet<String> = outcomes.iter().filter_map(|o| o.run_id.clone()).collect();
        let attempts = match self.research.runs() {
            Ok(runs) => runs
                .into_iter()
                .filter(|run| {
                    let of_procedure = match (&run.origin, &summary.procedure) {
                        (CodeOrigin::File { path, .. }, Some(p)) => path == p,
                        (CodeOrigin::Selection { path: Some(path), .. }, Some(p)) => path == p,
                        _ => false,
                    };
                    of_procedure || outcome_runs.contains(&run.id.0)
                })
                .take(MAX_ATTEMPTS)
                .collect(),
            Err(_) => Vec::new(),
        };
        Ok(ExperimentDetail {
            summary,
            procedure,
            inputs,
            outcomes,
            attempts,
            graph_revision: self.graph.with_store(|s| Ok(s.revision()?))?,
        })
    }
}
