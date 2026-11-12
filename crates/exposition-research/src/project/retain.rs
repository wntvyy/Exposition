//! Retained outcomes: keeping a run's output admits it as one resource
//! identity (artifact role; dataset + artifact for tables) with
//! service-owned computational evidence and `derivedFrom` provenance.
//! Unretained output stays operational history (run manifests).

use exposition_core::artifact::{ArtifactKind, ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use exposition_graph::{
    AssertionStatus, Entity, EntityId, EntityKind, Evidence, EvidenceId, EvidenceKind, EvidenceOutcome, EvidenceOutput,
    Lifecycle, OperationRef, RelationKind, ResourceRole, VersionedInput,
};

use super::types::{RetainOutcome, RetainRequest, RetainState, SetResultStateRequest};
use super::{ResearchProject, bare_hash, now_ms, origin};
use crate::error::ResearchError;
use crate::types::RunManifest;

/// Evidence of `run` for an artifact (inputs exactly as the run pinned them).
pub(crate) fn computational_evidence(
    manifest: &RunManifest,
    artifact: &ArtifactRecord,
    subject: &EntityId,
) -> Evidence {
    let path = artifact.path.clone().expect("project artifact");
    let outcome = match &manifest.record.status {
        RunStatus::Succeeded => EvidenceOutcome::Succeeded,
        RunStatus::Failed { message } => EvidenceOutcome::Failed { message: message.clone() },
        RunStatus::Canceled => EvidenceOutcome::Canceled,
        RunStatus::Interrupted => EvidenceOutcome::Interrupted,
        RunStatus::Running => EvidenceOutcome::Interrupted,
    };
    let mut environment: Vec<(String, String)> =
        manifest.record.environment.iter().map(|t| (t.name.clone(), t.version.clone())).collect();
    let interpreter = &manifest.session.interpreter;
    if !interpreter.implementation.is_empty() {
        environment.push((format!("interpreter:{}", interpreter.implementation), interpreter.version.clone()));
    }
    environment.push(("execution".into(), "trusted native (user account privileges)".into()));
    environment.push(("code sha256".into(), manifest.code.sha256.clone()));
    Evidence {
        id: EvidenceId::mint(),
        kind: EvidenceKind::Computational,
        subject: Some(subject.clone()),
        operation: OperationRef { kind: "researchRun".into(), id: manifest.record.id.0.clone() },
        inputs: manifest
            .record
            .inputs
            .iter()
            .map(|i| VersionedInput {
                path: i.path.clone(),
                content_hash: i.content_hash.as_deref().map(bare_hash),
                revision: i.revision,
                unsaved: manifest.unsaved_inputs.contains(&i.path),
            })
            .collect(),
        outcome,
        environment,
        outputs: vec![EvidenceOutput {
            path,
            content_hash: bare_hash(&artifact.content_hash),
            media_type: artifact.media_type.clone(),
        }],
        formal: None,
        recorded_by: origin(),
        recorded_at_ms: now_ms(),
    }
}

fn kind_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Figure => "figure",
        ArtifactKind::Table => "table",
        _ => "data",
    }
}

impl ResearchProject {
    /// The artifact of a run in the open workspace, with its file verified
    /// against the digest the run recorded (an altered file is refused).
    pub(crate) fn verified_artifact(
        &self,
        run_id: &str,
        artifact_id: &str,
    ) -> Result<(RunManifest, ArtifactRecord), ResearchError> {
        let (record, file) = self.research.artifact(run_id, artifact_id)?;
        let manifest = self.research.run_manifest(run_id)?;
        if manifest.record.status == RunStatus::Running {
            return Err(ResearchError::coded(
                "research.runActive",
                "The run is still running; retain its output when it finishes.",
            ));
        }
        let found = crate::service::hash_file(&file).map_err(|e| ResearchError::io("reading the artifact", e))?;
        if bare_hash(&record.content_hash) != found {
            return Err(ResearchError::coded(
                "research.artifactChanged",
                format!(
                    "{} changed since the run recorded it (expected sha256 {}, found {}); it is not used",
                    record.path.as_ref().map(|p| p.to_string()).unwrap_or_default(),
                    &bare_hash(&record.content_hash)[..12],
                    &found[..12]
                ),
            ));
        }
        Ok((manifest, record))
    }

    /// Keep, archive, or discard a run's output (see the module docs).
    pub fn retain(&self, request: &RetainRequest) -> Result<RetainOutcome, ResearchError> {
        let _ = self.graph_root()?;
        let (manifest, record) = self.verified_artifact(&request.run_id, &request.artifact_id)?;
        let path = record.path.clone().ok_or_else(|| ResearchError::UnknownArtifact(request.artifact_id.clone()))?;
        if request.state == RetainState::Discarded && self.graph.resource(&path)?.is_none() {
            return Err(ResearchError::coded(
                "research.notRetained",
                "Only a retained result can be discarded; output that was never kept stays in the run history.",
            ));
        }
        if let Some(experiment) = &request.experiment
            && self.entity(experiment)?.kind != EntityKind::Experiment
        {
            return Err(ResearchError::coded("research.notExperiment", "That entity is not an experiment."));
        }
        self.retain_verified(&manifest, &record, request.state, request.experiment.as_ref())
    }

    /// Retain an artifact already verified against its recorded digest.
    pub(crate) fn retain_verified(
        &self,
        manifest: &RunManifest,
        record: &ArtifactRecord,
        state: RetainState,
        experiment: Option<&EntityId>,
    ) -> Result<RetainOutcome, ResearchError> {
        let path = record.path.clone().ok_or_else(|| ResearchError::UnknownArtifact(record.id.0.clone()))?;
        let hash = bare_hash(&record.content_hash);
        let entity = self.graph.ensure_resource(&path, Some(&hash), true, super::SERVICE)?;
        let mut attributes = entity.attributes.clone();
        attributes.insert("retained.run".into(), manifest.record.id.0.clone());
        attributes.insert("retained.artifact".into(), record.id.0.clone());
        attributes.insert("retained.kind".into(), kind_name(record.kind).into());
        attributes.insert("retained.mediaType".into(), record.media_type.clone());
        attributes.insert("retained.contentHash".into(), hash.clone());
        attributes.entry("retained.at".into()).or_insert_with(|| now_ms().to_string());
        self.update_entity(
            &entity,
            Some(attributes),
            Some(state.lifecycle()),
            &format!(
                "{} {path}",
                match state {
                    RetainState::Active => "Keep",
                    RetainState::Archived => "Archive",
                    RetainState::Discarded => "Discard",
                }
            ),
        )?;
        // Evidence of the run that produced it (one record per run and output).
        let existing = self.graph.with_store(|s| {
            Ok(s.evidence_for_subject(&entity.id)?.into_iter().find(|e| {
                e.kind == EvidenceKind::Computational
                    && e.operation.kind == "researchRun"
                    && e.operation.id == manifest.record.id.0
                    && e.outputs.iter().any(|o| o.path == path && o.content_hash == hash)
            }))
        })?;
        let (evidence, evidence_existing) = match existing {
            Some(e) => (Some(e.id), true),
            None => {
                let e = computational_evidence(manifest, record, &entity.id);
                (Some(self.graph.register_evidence(&origin(), e)?), false)
            }
        };
        // Provenance: derived from the run's pinned inputs (project files) and
        // from the experiment it is an outcome of; a member of the space.
        let mut relations = Vec::new();
        for input in &manifest.record.inputs {
            if input.path == path {
                continue;
            }
            match self.graph.ensure_resource(&input.path, None, false, super::SERVICE) {
                Ok(source) => relations.push((
                    RelationKind::DerivedFrom,
                    entity.id.clone(),
                    source.id,
                    None,
                    Some(format!("input of run {}", manifest.record.id.0)),
                )),
                Err(e) if e.code == "graph.excluded" => {}
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(experiment) = experiment {
            relations.push((
                RelationKind::DerivedFrom,
                entity.id.clone(),
                experiment.clone(),
                Some(AssertionStatus::Accepted),
                Some("outcome of the experiment".into()),
            ));
        }
        if let Some(space) = self.space()? {
            relations.push((RelationKind::MemberOf, entity.id.clone(), space.entity, None, None));
        }
        let graph_revision = self.add_relations(relations, &format!("Provenance of {path}"))?;
        let entity = self.entity(&entity.id)?;
        Ok(RetainOutcome { result: self.retained_result(&entity)?, evidence, evidence_existing, graph_revision })
    }

    /// Change the lifecycle of a retained result. Discarding never deletes
    /// the file, a manuscript attachment, source data, or evidence.
    pub fn set_result_state(&self, request: &SetResultStateRequest) -> Result<RetainOutcome, ResearchError> {
        let _ = self.graph_root()?;
        let entity: Entity = self.entity(&request.entity)?;
        if entity.kind != EntityKind::Resource || !entity.roles.contains(&ResourceRole::Artifact) {
            return Err(ResearchError::coded("research.notRetained", "That entity is not a retained result."));
        }
        if entity.lifecycle == Lifecycle::Tombstoned {
            return Err(ResearchError::coded("graph.tombstoned", format!("{} no longer exists", entity.label)));
        }
        self.update_entity(
            &entity,
            None,
            Some(request.state.lifecycle()),
            &format!("Set {} {:?}", entity.label, request.state),
        )?;
        let entity = self.entity(&request.entity)?;
        let path: Option<WorkspacePath> = entity.key.as_deref().and_then(|k| WorkspacePath::parse(k).ok());
        let evidence = match path {
            Some(_) => self.graph.with_store(|s| {
                Ok(s.evidence_for_subject(&entity.id)?
                    .into_iter()
                    .find(|e| e.kind == EvidenceKind::Computational)
                    .map(|e| e.id))
            })?,
            None => None,
        };
        Ok(RetainOutcome {
            result: self.retained_result(&entity)?,
            evidence_existing: evidence.is_some(),
            evidence,
            graph_revision: self.graph.with_store(|s| Ok(s.revision()?))?,
        })
    }
}
