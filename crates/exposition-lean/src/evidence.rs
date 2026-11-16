//! Formal evidence in the project graph, from the Lean service's own audit.
//!
//! Evidence is recorded only from a [`LeanAuditRecord`] the service computed
//! for an exact revision (never from data a view supplies), one record per
//! declaration the audit listed: its printed statement, axioms, the file's
//! verdict and the declaration's own verdict, the toolchain, and the digest
//! of the exact text checked. The audit does not report declaration
//! dependencies, so each record says so and carries no dependency
//! fingerprints: its freshness against imported modules is unknown, never
//! "current". A declaration missing from the audit's list (or beyond a
//! truncated list) gets no evidence from the file's badge.
//!
//! Associating a formal declaration with an informal statement is a separate
//! `formalizes` relation a person reviews: a correspondence assertion, not a
//! machine proof that the two say the same thing.

use std::collections::BTreeMap;

use exposition_core::path::WorkspacePath;
use exposition_graph::{
    AssertionStatus, BatchRef, ChangeRequest, EntityId, EntityKind, EntityUpsert, Evidence, EvidenceId, EvidenceKind,
    EvidenceOutcome, FormalDependencies, FormalEvidence, FormalVerdict, GraphError, GraphOperation, GraphResult,
    GraphService, Lifecycle, OperationRef, Origin, RelationId, RelationKind, RelationUpsert, ServiceBatch,
    SourceBinding, SourceSelector, SourceVersion, VersionedInput,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::types::{AuditedDeclaration, DeclarationVerdict, LeanAuditRecord, LeanStatus};

/// The service name evidence and declarations are attributed to.
pub const SERVICE: &str = "lean";
/// Syntax of a declaration binding in a `.lean` file.
pub const DECLARATION_SYNTAX: &str = "lean.declaration";
/// Why formal dependencies are absent from every record.
pub const DEPENDENCIES_UNAVAILABLE: &str = "the Lean audit does not report declaration dependencies";

/// The file verdict of a finished status (`None` for unfinished ones).
pub fn file_verdict(status: &LeanStatus) -> Option<FormalVerdict> {
    Some(match status {
        LeanStatus::Checked { .. } => FormalVerdict::Checked,
        LeanStatus::CheckedWithSorry { .. } => FormalVerdict::Incomplete,
        LeanStatus::CheckedWithAxioms { .. } => FormalVerdict::CheckedWithAxioms,
        LeanStatus::ReliesOnCompiledCode { .. } => FormalVerdict::ReliesOnCompiledCode,
        LeanStatus::PartiallyChecked { .. } => FormalVerdict::PartiallyChecked,
        LeanStatus::Unverified { .. } => FormalVerdict::NotVerified,
        LeanStatus::Errors { .. } => FormalVerdict::Errors,
        _ => return None,
    })
}

pub fn declaration_verdict(verdict: DeclarationVerdict) -> FormalVerdict {
    match verdict {
        DeclarationVerdict::Checked => FormalVerdict::Checked,
        DeclarationVerdict::CheckedWithAxioms => FormalVerdict::CheckedWithAxioms,
        DeclarationVerdict::Incomplete => FormalVerdict::Incomplete,
        DeclarationVerdict::ReliesOnCompiledCode => FormalVerdict::ReliesOnCompiledCode,
    }
}

/// Environment key of the Lake package folder an audit ran in.
pub const ENV_PACKAGE: &str = "lake package";
/// Environment key of the digest of the package's `lake-manifest.json`.
pub const ENV_MANIFEST: &str = "lake-manifest.json sha256";
/// Environment key of the Mathlib commit the manifest pins.
pub const ENV_MATHLIB: &str = "mathlib revision";

fn short(hash: &str) -> String {
    hash[..hash.len().min(16)].to_owned()
}

/// The identifier audits had before the Lean toolchain became part of it
/// (path, revision, text digest, manifest digest). Records stored under it
/// stay readable; see [`same_audit`].
fn legacy_audit_id(record: &LeanAuditRecord) -> String {
    let base = format!("{}@{}:{}", record.path, record.revision, short(&record.content_hash));
    match record.package.as_ref().and_then(|p| p.manifest.as_ref()) {
        Some(manifest) => format!("{base}+{}", short(&manifest.sha256)),
        None => base,
    }
}

/// Identifier of an audit: path, revision, and digest of the text checked,
/// for a package with a manifest the digest of the pinned dependencies, and
/// the Lean environment (the exact toolchain, its `lean --version`, and the
/// digest of the package's `lean-toolchain`). The same text audited against
/// other dependencies or with another Lean is another audit.
pub fn audit_id(record: &LeanAuditRecord) -> String {
    let mut lean = Sha256::new();
    for part in [
        Some(record.toolchain.as_str()),
        record.toolchain_version.as_deref(),
        record.package.as_ref().and_then(|p| p.toolchain_file.as_ref()).map(|f| f.sha256.as_str()),
    ] {
        // Length-prefixed, so absent and empty parts differ and parts cannot run together.
        match part {
            Some(text) => lean.update(format!("{}:{text}", text.len())),
            None => lean.update("-"),
        }
        lean.update([0u8]);
    }
    let lean: String = lean.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("{}~{}", legacy_audit_id(record), short(&lean))
}

/// `evidence` was recorded for exactly the audit `record` describes. A record
/// stored before the toolchain was part of the identifier counts only when
/// everything the identifier now covers is the same in it: the toolchain,
/// its version, and the digest of the package's `lean-toolchain`.
fn same_audit(evidence: &Evidence, record: &LeanAuditRecord) -> bool {
    if evidence.operation.kind != "leanAudit" {
        return false;
    }
    if evidence.operation.id == audit_id(record) {
        return true;
    }
    if evidence.operation.id != legacy_audit_id(record) {
        return false;
    }
    let version = evidence.environment.iter().find(|(key, _)| key == "lean --version").map(|(_, v)| v.as_str());
    let pin = record.package.as_ref().and_then(|p| p.toolchain_file.as_ref());
    let recorded_pin = pin.and_then(|file| {
        evidence.inputs.iter().find(|input| input.path == file.path).and_then(|input| input.content_hash.as_deref())
    });
    evidence.formal.as_ref().is_some_and(|f| f.toolchain == record.toolchain)
        && version == record.toolchain_version.as_deref()
        && recorded_pin == pin.map(|file| file.sha256.as_str())
}

/// The formal part of an evidence record for one audited declaration.
pub fn formal_evidence(record: &LeanAuditRecord, decl: &AuditedDeclaration) -> Option<FormalEvidence> {
    Some(FormalEvidence {
        declaration: decl.name.clone(),
        declaration_kind: decl.kind.clone(),
        statement: decl.statement.clone(),
        axioms: decl.axioms.clone(),
        file_verdict: file_verdict(&record.status)?,
        declaration_verdict: Some(declaration_verdict(decl.verdict)),
        toolchain: record.toolchain.clone(),
        source_path: record.path.clone(),
        source_digest: record.content_hash.clone(),
        revision: Some(record.revision),
        dependency_fingerprints: None,
        dependencies: FormalDependencies::Unavailable { reason: DEPENDENCIES_UNAVAILABLE.into() },
    })
}

/// Evidence for one declaration of the audit; `saved` tells whether the
/// audited revision was the file's saved text.
pub fn evidence_record(
    record: &LeanAuditRecord,
    decl: &AuditedDeclaration,
    subject: &EntityId,
    saved: bool,
    now_ms: u64,
) -> Option<Evidence> {
    let formal = formal_evidence(record, decl)?;
    let verdict = formal.declaration_verdict.unwrap_or(formal.file_verdict);
    let mut environment = vec![("lean toolchain".to_owned(), record.toolchain.clone())];
    if let Some(version) = &record.toolchain_version {
        environment.push(("lean --version".to_owned(), version.clone()));
    }
    let mut inputs = vec![VersionedInput {
        path: record.path.clone(),
        content_hash: Some(record.content_hash.clone()),
        revision: Some(record.revision),
        unsaved: !saved,
    }];
    // The dependency identity the audit ran against: the files that pin the
    // Lean version and every dependency revision are inputs of the record,
    // so a change to either makes it historical.
    if let Some(package) = &record.package {
        environment.push((
            ENV_PACKAGE.to_owned(),
            if package.folder.is_empty() { ".".to_owned() } else { package.folder.clone() },
        ));
        if let Some(manifest) = &package.manifest {
            environment.push((ENV_MANIFEST.to_owned(), manifest.sha256.clone()));
        }
        if let Some(revision) = &package.mathlib_revision {
            environment.push((ENV_MATHLIB.to_owned(), revision.clone()));
        }
        for file in [&package.toolchain_file, &package.manifest].into_iter().flatten() {
            inputs.push(VersionedInput {
                path: file.path.clone(),
                content_hash: Some(file.sha256.clone()),
                revision: None,
                unsaved: false,
            });
        }
    }
    Some(Evidence {
        id: EvidenceId::mint(),
        kind: EvidenceKind::FormalCheck,
        subject: Some(subject.clone()),
        operation: OperationRef { kind: "leanAudit".into(), id: audit_id(record) },
        inputs,
        outcome: EvidenceOutcome::Verdict { verdict },
        environment,
        outputs: Vec::new(),
        formal: Some(formal),
        recorded_by: Origin::Service { service: SERVICE.into() },
        recorded_at_ms: now_ms,
    })
}

/// Evidence recorded (or found already recorded) for one declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RecordedEvidence {
    pub declaration: String,
    /// The formal-declaration entity.
    pub entity: EntityId,
    pub evidence: EvidenceId,
    /// True when this exact audit's evidence already existed.
    pub existing: bool,
}

/// A declaration that got no evidence, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RefusedEvidence {
    pub declaration: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceOutcomeReport {
    pub recorded: Vec<RecordedEvidence>,
    pub refused: Vec<RefusedEvidence>,
}

fn label_of(name: &str) -> String {
    let one_line: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if one_line.len() <= 200 {
        return one_line;
    }
    let mut end = 197;
    while !one_line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &one_line[..end])
}

/// Why `name` cannot get evidence from `record`, if it cannot.
pub fn refusal(record: &LeanAuditRecord, name: &str) -> Option<String> {
    if file_verdict(&record.status).is_none() {
        return Some("the audit of this revision did not finish".into());
    }
    if record.declarations.iter().any(|d| d.name == name) {
        return None;
    }
    Some(if record.truncated {
        format!(
            "{name} is not among the {} declarations kept from the audit of revision {} (the list is truncated); a file verdict is not evidence for a declaration",
            record.declarations.len(),
            record.revision
        )
    } else {
        format!(
            "the audit of revision {} did not report {name}; a file verdict is not evidence for a declaration",
            record.revision
        )
    })
}

/// The formal-declaration entity of `name` in `path`, if the graph has one
/// (a declaration with the same name bound to another file is a different
/// entity: nothing is merged by name alone).
pub fn find_declaration(graph: &GraphService, path: &WorkspacePath, name: &str) -> GraphResult<Option<EntityId>> {
    graph.with_store(|store| {
        for entity in store.entities_by_key(EntityKind::FormalDeclaration, name)? {
            if entity.lifecycle == Lifecycle::Tombstoned {
                continue;
            }
            if store.bindings(&entity.id)?.iter().any(|b| &b.path == path) {
                return Ok(Some(entity.id));
            }
        }
        Ok(None)
    })
}

/// Find or admit the formal-declaration entity of an audited declaration,
/// anchored in its `.lean` resource.
pub fn ensure_declaration(
    graph: &GraphService,
    record: &LeanAuditRecord,
    decl: &AuditedDeclaration,
    saved: bool,
) -> GraphResult<EntityId> {
    if let Some(id) = find_declaration(graph, &record.path, &decl.name)? {
        return Ok(id);
    }
    let resource = graph.ensure_resource(&record.path, None, false, SERVICE)?;
    let mut attributes = BTreeMap::new();
    attributes.insert("lean.kind".to_owned(), label_of(&decl.kind));
    let binding = SourceBinding {
        entity: EntityId::mint(),
        path: record.path.clone(),
        selector: SourceSelector::Declaration {
            syntax: DECLARATION_SYNTAX.into(),
            name: Some(decl.name.clone()),
            ordinal: None,
        },
        range: decl.range,
        version: Some(SourceVersion {
            content_hash: record.content_hash.clone(),
            revision: Some(record.revision),
            saved,
        }),
        fingerprint: None,
        state: exposition_graph::BindingState::Unresolved,
        detail: Some(format!(
            "Located by the Lean audit of revision {}; not yet confirmed against the current source.",
            record.revision
        )),
    };
    let batch = ServiceBatch {
        entities: vec![EntityUpsert {
            id: None,
            kind: EntityKind::FormalDeclaration,
            roles: Vec::new(),
            label: label_of(&decl.name),
            key: Some(decl.name.clone()),
            attributes,
            lifecycle: None,
            tombstone_reason: None,
            bindings: Some(vec![binding]),
        }],
        relations: vec![RelationUpsert {
            kind: RelationKind::AnchoredIn,
            from: BatchRef::Batch(0),
            to: BatchRef::Existing(resource.id),
            status: None,
            evidence_kind: None,
            evidence: Vec::new(),
            note: None,
        }],
        summary: format!("Admit formal declaration {}", label_of(&decl.name)),
        ..Default::default()
    };
    let outcome = graph.apply_service_batch(&Origin::Service { service: SERVICE.into() }, &batch)?;
    outcome.entity_ids.into_iter().next().ok_or_else(|| GraphError::new("graph.storage", "admission failed"))
}

/// Record formal evidence for `names` from `record` (idempotent per audit
/// and declaration). Declarations the audit did not list are refused.
pub fn record_evidence(
    graph: &GraphService,
    record: &LeanAuditRecord,
    names: &[String],
    saved: bool,
    now_ms: u64,
) -> GraphResult<EvidenceOutcomeReport> {
    let mut report = EvidenceOutcomeReport::default();
    let origin = Origin::Service { service: SERVICE.into() };
    for name in names.iter().take(200) {
        if let Some(reason) = refusal(record, name) {
            report.refused.push(RefusedEvidence { declaration: name.clone(), reason });
            continue;
        }
        let decl = record.declarations.iter().find(|d| &d.name == name).expect("checked by refusal");
        let entity = ensure_declaration(graph, record, decl, saved)?;
        let existing = graph.with_store(|store| {
            Ok(store.evidence_for_subject(&entity)?.into_iter().find(|e| {
                e.kind == EvidenceKind::FormalCheck
                    && same_audit(e, record)
                    && e.formal.as_ref().is_some_and(|f| f.declaration == decl.name)
            }))
        })?;
        if let Some(e) = existing {
            report.recorded.push(RecordedEvidence {
                declaration: name.clone(),
                entity,
                evidence: e.id,
                existing: true,
            });
            continue;
        }
        let Some(evidence) = evidence_record(record, decl, &entity, saved, now_ms) else {
            report.refused.push(RefusedEvidence {
                declaration: name.clone(),
                reason: "the audit of this revision did not finish".into(),
            });
            continue;
        };
        let id = graph.register_evidence(&origin, evidence)?;
        report.recorded.push(RecordedEvidence { declaration: name.clone(), entity, evidence: id, existing: false });
    }
    Ok(report)
}

/// A reviewed `formalizes` association (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FormalAssociation {
    pub declaration: EntityId,
    pub target: EntityId,
    pub relation: RelationId,
    /// Always `accepted`: a person reviewed the correspondence.
    pub status: AssertionStatus,
    /// The relation already existed and was kept (or a suggestion was accepted).
    pub existing: bool,
}

/// Record that a person reviewed the correspondence between a formal
/// declaration and an informal entity (`declaration formalizes target`),
/// through the same validated change path as every manual graph edit.
pub fn associate(
    graph: &GraphService,
    declaration: &EntityId,
    target: &EntityId,
    note: Option<String>,
) -> GraphResult<FormalAssociation> {
    let (kind, existing, base) = graph.with_store(|store| {
        let kind = store.entity(declaration)?.map(|e| e.kind);
        let existing = store.find_relation(RelationKind::Formalizes, declaration, target)?;
        Ok((kind, existing, store.revision()?))
    })?;
    if kind != Some(EntityKind::FormalDeclaration) {
        return Err(GraphError::new("lean.notDeclaration", "Only a formal declaration can formalize a statement."));
    }
    let operations = match &existing {
        // An accepted association whose statement is the reviewed one is
        // kept as it is. One whose review no longer covers the statement is
        // confirmed again: associating is the person's review of it now.
        Some(r)
            if r.status == AssertionStatus::Accepted
                && r.review.as_ref().is_none_or(|v| v.state == exposition_graph::LinkReviewState::Current) =>
        {
            return Ok(FormalAssociation {
                declaration: declaration.clone(),
                target: target.clone(),
                relation: r.id.clone(),
                status: r.status,
                existing: true,
            });
        }
        Some(r) if matches!(r.status, AssertionStatus::Suggested | AssertionStatus::Accepted) => {
            vec![GraphOperation::SetRelationStatus { relation: r.id.clone(), status: AssertionStatus::Accepted }]
        }
        _ => vec![GraphOperation::AddRelation {
            kind: RelationKind::Formalizes,
            from: exposition_graph::EntityRef::Id { id: declaration.clone() },
            to: exposition_graph::EntityRef::Id { id: target.clone() },
            evidence_kind: None,
            evidence: Vec::new(),
            note,
        }],
    };
    let request = ChangeRequest {
        base_revision: base,
        operations,
        summary: Some("Associate a formal declaration (reviewed correspondence)".into()),
    };
    let (proposal, outcome) = graph.propose_and_apply(Origin::User, &request)?;
    match outcome {
        exposition_graph::ApplyOutcome::Applied { .. } => {}
        exposition_graph::ApplyOutcome::Conflict { diagnostics, .. }
        | exposition_graph::ApplyOutcome::Invalid { diagnostics, .. } => {
            let message = diagnostics
                .iter()
                .chain(proposal.diagnostics.iter())
                .map(|d| d.message.clone())
                .next()
                .unwrap_or_else(|| "the association was refused".into());
            return Err(GraphError::new("lean.associationRefused", message));
        }
    }
    let relation = graph
        .with_store(|store| Ok(store.find_relation(RelationKind::Formalizes, declaration, target)?))?
        .ok_or_else(|| GraphError::new("graph.storage", "the association was not recorded"))?;
    Ok(FormalAssociation {
        declaration: declaration.clone(),
        target: target.clone(),
        relation: relation.id,
        status: relation.status,
        existing: existing.is_some(),
    })
}

/// Which declaration to associate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum DeclarationRef {
    /// A declaration of an open Lean document's last audit (its evidence is
    /// recorded from that audit first).
    Audited { document: exposition_core::document::DocumentId, name: String },
    /// An existing formal-declaration entity.
    Entity { entity: EntityId },
}

/// `lean_associate_declaration` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeanAssociateRequest {
    pub declaration: DeclarationRef,
    /// A statement, definition, assumption, or mathematical object.
    pub target: EntityId,
    /// Why the correspondence holds (≤ 500 characters, optional).
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct LeanAssociation {
    pub association: FormalAssociation,
    /// Evidence recorded from the audit (for `Audited` references).
    pub evidence: Option<EvidenceOutcomeReport>,
}

/// One evidence record with its applicability to the current text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceView {
    pub evidence: Evidence,
    /// Never `current`: the audit does not fingerprint dependencies.
    pub freshness: exposition_graph::Freshness,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AssociationView {
    pub relation: RelationId,
    pub target: exposition_graph::Entity,
    pub status: AssertionStatus,
    /// Whether the review still covers the target's current statement
    /// (accepted associations).
    pub review: Option<exposition_graph::LinkReview>,
    pub note: Option<String>,
}

/// A formal declaration of a file with its evidence history and associations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationEvidence {
    pub entity: EntityId,
    pub name: String,
    /// Newest first.
    pub evidence: Vec<EvidenceView>,
    pub associations: Vec<AssociationView>,
}

/// SHA-256 (hex) of text, as the Lean service hashes the text it checks.
pub fn text_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Applicability of formal evidence to the file's current text.
pub fn formal_freshness(evidence: &Evidence, current_digest: Option<&str>) -> exposition_graph::Freshness {
    use exposition_graph::Freshness;
    let Some(formal) = &evidence.formal else {
        return Freshness::Unknown { reason: "not formal evidence".into() };
    };
    match current_digest {
        Some(digest) if digest != formal.source_digest => Freshness::Stale {
            reasons: vec![format!(
                "{} changed since revision {} was checked; this is a historical record of that text",
                formal.source_path,
                formal.revision.map(|r| r.to_string()).unwrap_or_else(|| "?".into())
            )],
        },
        Some(_) => Freshness::Unknown {
            reason: "the text is unchanged, but the audit does not fingerprint the declaration's dependencies; changes in imported modules are not detected".into(),
        },
        None => Freshness::Unknown { reason: "the file's current text is not available".into() },
    }
}

/// Whether the package files a record was checked against (its inputs other
/// than the Lean source: `lean-toolchain`, `lake-manifest.json`) still have
/// the recorded digests in the project index. A changed or deleted one makes
/// the record historical; one the index cannot tell about is never reported.
fn dependency_staleness(store: &exposition_graph::store::GraphStore, evidence: &Evidence) -> Vec<String> {
    let Some(formal) = &evidence.formal else { return Vec::new() };
    let mut reasons = Vec::new();
    for input in evidence.inputs.iter().filter(|i| i.path != formal.source_path) {
        let Some(recorded) = &input.content_hash else { continue };
        match store.coverage(&input.path) {
            Ok(Some(coverage)) => {
                if coverage.content_hash.as_ref().is_some_and(|current| current != recorded) {
                    reasons.push(format!(
                        "{} changed since this was checked: the Lean version or dependency revisions it was checked against are no longer the package's",
                        input.path
                    ));
                }
            }
            Ok(None) => {
                let deleted = store
                    .entities_by_key(EntityKind::Resource, input.path.as_str())
                    .map(|es| !es.is_empty() && es.iter().all(|e| e.lifecycle == Lifecycle::Tombstoned))
                    .unwrap_or(false);
                if deleted {
                    reasons.push(format!("{} was deleted since this was checked", input.path));
                }
            }
            Err(_) => {}
        }
    }
    reasons
}

/// Formal declarations bound to `path`, their evidence (with freshness
/// against `current_digest` and the package files it was checked against),
/// and their `formalizes` associations.
pub fn declaration_evidence(
    graph: &GraphService,
    path: &WorkspacePath,
    current_digest: Option<&str>,
) -> GraphResult<Vec<DeclarationEvidence>> {
    graph.with_store(|store| {
        let mut ids: Vec<EntityId> = store.bindings_at_path(path)?.into_iter().map(|b| b.entity).collect();
        ids.sort();
        ids.dedup();
        let mut out = Vec::new();
        for entity in store.entities(&ids)? {
            if entity.kind != EntityKind::FormalDeclaration || entity.lifecycle == Lifecycle::Tombstoned {
                continue;
            }
            let mut evidence: Vec<Evidence> = store
                .evidence_for_subject(&entity.id)?
                .into_iter()
                .filter(|e| e.kind == EvidenceKind::FormalCheck)
                .collect();
            evidence.sort_by_key(|e| std::cmp::Reverse(e.recorded_at_ms));
            evidence.truncate(50);
            let mut associations = Vec::new();
            for r in store.relations_from(&entity.id)? {
                if r.kind != RelationKind::Formalizes || r.status == AssertionStatus::Rejected {
                    continue;
                }
                if let Some(target) = store.entity(&r.to)? {
                    associations.push(AssociationView {
                        relation: r.id,
                        target,
                        status: r.status,
                        review: r.review,
                        note: r.note,
                    });
                }
            }
            out.push(DeclarationEvidence {
                name: entity.key.clone().unwrap_or_else(|| entity.label.clone()),
                entity: entity.id,
                evidence: evidence
                    .into_iter()
                    .map(|e| {
                        let mut freshness = formal_freshness(&e, current_digest);
                        let dependencies = dependency_staleness(store, &e);
                        if !dependencies.is_empty() {
                            freshness = match freshness {
                                exposition_graph::Freshness::Stale { mut reasons } => {
                                    reasons.extend(dependencies);
                                    exposition_graph::Freshness::Stale { reasons }
                                }
                                _ => exposition_graph::Freshness::Stale { reasons: dependencies },
                            };
                        }
                        EvidenceView { freshness, evidence: e }
                    })
                    .collect(),
                associations,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use exposition_core::document::DocumentId;
    use exposition_graph::{Freshness, GraphConfig};

    fn record(status: LeanStatus, decls: Vec<AuditedDeclaration>, truncated: bool) -> LeanAuditRecord {
        LeanAuditRecord {
            document: DocumentId("doc-1".into()),
            path: WorkspacePath::parse("Proofs/Main.lean").unwrap(),
            revision: 7,
            content_hash: "ab".repeat(32),
            toolchain: "leanprover/lean4:v4.35.0".into(),
            toolchain_version: Some("Lean (version 4.35.0)".into()),
            status,
            declaration_count: decls.len() as u32 + u32::from(truncated),
            declarations: decls,
            truncated,
            finished_at_ms: 1,
            package: None,
        }
    }

    fn decl(name: &str, verdict: DeclarationVerdict, axioms: &[&str]) -> AuditedDeclaration {
        AuditedDeclaration {
            name: name.into(),
            kind: "theorem".into(),
            statement: format!("{name} : 1 + 1 = 2"),
            range: None,
            line: Some(3),
            axioms: axioms.iter().map(|a| a.to_string()).collect(),
            verdict,
        }
    }

    fn checked() -> LeanStatus {
        LeanStatus::Checked {
            revision: 7,
            toolchain: "leanprover/lean4:v4.35.0".into(),
            warnings: 0,
            axioms: vec!["propext".into()],
            declarations: Vec::new(),
            declaration_count: 0,
        }
    }

    fn graph() -> (tempfile::TempDir, std::sync::Arc<GraphService>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(root.join("Proofs")).unwrap();
        std::fs::write(root.join("Proofs/Main.lean"), "theorem a : 1 + 1 = 2 := rfl\n").unwrap();
        let graph = GraphService::new(GraphConfig { data_dir: dir.path().join("data") });
        let status = graph.attach(&std::fs::canonicalize(&root).unwrap());
        assert!(status.is_usable(), "{status:?}");
        (dir, graph)
    }

    #[test]
    fn every_verdict_is_preserved_exactly() {
        let toolchain = String::from("t");
        let cases = [
            (checked(), FormalVerdict::Checked),
            (
                LeanStatus::CheckedWithSorry {
                    revision: 1,
                    toolchain: toolchain.clone(),
                    count: 1,
                    locations: vec![],
                    warnings: 0,
                },
                FormalVerdict::Incomplete,
            ),
            (
                LeanStatus::CheckedWithAxioms {
                    revision: 1,
                    toolchain: toolchain.clone(),
                    axioms: vec![],
                    declarations: vec![],
                    warnings: 0,
                },
                FormalVerdict::CheckedWithAxioms,
            ),
            (
                LeanStatus::ReliesOnCompiledCode {
                    revision: 1,
                    toolchain: toolchain.clone(),
                    axioms: vec![],
                    declarations: vec![],
                    warnings: 0,
                },
                FormalVerdict::ReliesOnCompiledCode,
            ),
            (
                LeanStatus::PartiallyChecked {
                    revision: 1,
                    toolchain: toolchain.clone(),
                    line: None,
                    reason: "x".into(),
                },
                FormalVerdict::PartiallyChecked,
            ),
            (
                LeanStatus::Unverified { revision: 1, toolchain: toolchain.clone(), reason: "x".into() },
                FormalVerdict::NotVerified,
            ),
            (LeanStatus::Errors { revision: 1, toolchain, count: 1, sorry_count: 0 }, FormalVerdict::Errors),
        ];
        for (status, verdict) in cases {
            assert_eq!(file_verdict(&status), Some(verdict), "{status:?}");
        }
        assert_eq!(file_verdict(&LeanStatus::NotStarted), None);
        assert_eq!(declaration_verdict(DeclarationVerdict::Incomplete), FormalVerdict::Incomplete);
    }

    #[test]
    fn formal_evidence_carries_the_audit_and_unknown_dependencies() {
        let r = record(checked(), vec![decl("Main.a", DeclarationVerdict::Checked, &["propext"])], false);
        let subject = EntityId::mint();
        let e = evidence_record(&r, &r.declarations[0], &subject, false, 5).unwrap();
        assert_eq!(e.kind, EvidenceKind::FormalCheck);
        assert_eq!(e.operation.kind, "leanAudit");
        assert!(e.inputs[0].unsaved, "an unsaved audited buffer is labelled");
        let f = e.formal.unwrap();
        assert_eq!(f.statement, "Main.a : 1 + 1 = 2");
        assert_eq!(f.axioms, vec!["propext"]);
        assert_eq!(f.file_verdict, FormalVerdict::Checked);
        assert_eq!(f.declaration_verdict, Some(FormalVerdict::Checked));
        assert_eq!(f.dependency_fingerprints, None);
        assert_eq!(f.dependencies, FormalDependencies::Unavailable { reason: DEPENDENCIES_UNAVAILABLE.into() });
        assert_eq!(f.revision, Some(7));
        assert_eq!(f.source_digest, r.content_hash);
    }

    #[test]
    fn missing_or_truncated_declarations_get_no_evidence_from_the_file_badge() {
        let (_dir, graph) = graph();
        let r = record(checked(), vec![decl("Main.a", DeclarationVerdict::Checked, &[])], false);
        let out = record_evidence(&graph, &r, &["Main.b".into()], true, 1).unwrap();
        assert!(out.recorded.is_empty());
        assert!(out.refused[0].reason.contains("did not report Main.b"), "{:?}", out.refused);
        let truncated = record(checked(), vec![decl("Main.a", DeclarationVerdict::Checked, &[])], true);
        let out = record_evidence(&graph, &truncated, &["Main.z".into()], true, 1).unwrap();
        assert!(out.refused[0].reason.contains("truncated"), "{:?}", out.refused);
        // An audit that did not finish gives nothing.
        let unfinished = record(LeanStatus::NotStarted, vec![decl("Main.a", DeclarationVerdict::Checked, &[])], false);
        let out = record_evidence(&graph, &unfinished, &["Main.a".into()], true, 1).unwrap();
        assert!(out.recorded.is_empty() && out.refused.len() == 1);
    }

    #[test]
    fn evidence_is_service_owned_idempotent_and_never_current_without_fingerprints() {
        let (_dir, graph) = graph();
        let r = record(
            LeanStatus::CheckedWithAxioms {
                revision: 7,
                toolchain: "t".into(),
                axioms: vec!["Main.ax".into()],
                declarations: vec![],
                warnings: 0,
            },
            vec![
                decl("Main.a", DeclarationVerdict::Checked, &["propext"]),
                decl("Main.b", DeclarationVerdict::CheckedWithAxioms, &["Main.ax"]),
            ],
            false,
        );
        let first = record_evidence(&graph, &r, &["Main.a".into(), "Main.b".into()], true, 1).unwrap();
        assert_eq!(first.recorded.len(), 2);
        assert!(first.recorded.iter().all(|x| !x.existing));
        let again = record_evidence(&graph, &r, &["Main.a".into()], true, 2).unwrap();
        assert!(again.recorded[0].existing, "same audit, same declaration: one record");
        assert_eq!(again.recorded[0].evidence, first.recorded[0].evidence);
        assert_eq!(again.recorded[0].entity, first.recorded[0].entity, "one entity per declaration and file");

        let detail = graph.entity_detail(&first.recorded[1].entity).unwrap();
        assert_eq!(detail.entity.kind, EntityKind::FormalDeclaration);
        assert_eq!(detail.entity.key.as_deref(), Some("Main.b"));
        assert!(detail.outgoing.iter().any(|r| r.kind == RelationKind::AnchoredIn));
        let formal = detail.evidence[0].formal.as_ref().unwrap();
        assert_eq!(formal.file_verdict, FormalVerdict::CheckedWithAxioms);
        assert_eq!(formal.declaration_verdict, Some(FormalVerdict::CheckedWithAxioms));
        // Freshness: unknown (no index yet / no dependency fingerprints), never current.
        assert!(matches!(detail.evidence_freshness[0].1, Freshness::Unknown { .. }), "{:?}", detail.evidence_freshness);

        // A different revision of the file is a different audit: new evidence, same entity.
        let mut later = r.clone();
        later.revision = 8;
        later.content_hash = "cd".repeat(32);
        let next = record_evidence(&graph, &later, &["Main.a".into()], true, 3).unwrap();
        assert!(!next.recorded[0].existing);
        assert_eq!(next.recorded[0].entity, first.recorded[0].entity);
        let history = graph.entity_detail(&first.recorded[0].entity).unwrap();
        assert_eq!(history.evidence.len(), 2, "old evidence stays as history");

        // Evidence views: the newer audit matches the current text (unknown,
        // not current); the older one is a historical record (stale).
        let views = declaration_evidence(&graph, &r.path, Some(&later.content_hash)).unwrap();
        let a = views.iter().find(|v| v.name == "Main.a").unwrap();
        assert_eq!(a.evidence.len(), 2);
        assert!(matches!(a.evidence[0].freshness, Freshness::Unknown { .. }), "{:?}", a.evidence[0].freshness);
        assert!(matches!(a.evidence[1].freshness, Freshness::Stale { .. }), "{:?}", a.evidence[1].freshness);
    }

    #[test]
    fn the_dependency_identity_is_part_of_the_evidence_and_of_the_audit() {
        use crate::types::{LeanAuditFile, LeanAuditPackage};
        let (_dir, graph) = graph();
        let package = |manifest: &str| LeanAuditPackage {
            folder: "Proofs".into(),
            toolchain_file: Some(LeanAuditFile {
                path: WorkspacePath::parse("Proofs/lean-toolchain").unwrap(),
                sha256: "11".repeat(32),
            }),
            manifest: Some(LeanAuditFile {
                path: WorkspacePath::parse("Proofs/lake-manifest.json").unwrap(),
                sha256: manifest.repeat(32),
            }),
            mathlib_revision: Some("56a005855ea17e0b0958f73ec801ae140b0a1679".into()),
            dependency_count: 9,
        };
        let mut r = record(checked(), vec![decl("Main.a", DeclarationVerdict::Checked, &["propext"])], false);
        let standalone = audit_id(&r);
        r.package = Some(package("aa"));
        assert_ne!(audit_id(&r), standalone, "the pinned dependencies identify the audit");
        let subject = EntityId::mint();
        let evidence = evidence_record(&r, &r.declarations[0], &subject, true, 1).unwrap();
        let environment: std::collections::BTreeMap<_, _> = evidence.environment.iter().cloned().collect();
        assert_eq!(environment[ENV_PACKAGE], "Proofs");
        assert_eq!(environment[ENV_MANIFEST], "aa".repeat(32));
        assert_eq!(environment[ENV_MATHLIB], "56a005855ea17e0b0958f73ec801ae140b0a1679");
        assert_eq!(environment["lean toolchain"], "leanprover/lean4:v4.35.0");
        // The source first, then the files that pin the Lean version and the dependencies.
        let inputs: Vec<(&str, Option<&str>)> =
            evidence.inputs.iter().map(|i| (i.path.as_str(), i.content_hash.as_deref())).collect();
        assert_eq!(inputs[0].0, "Proofs/Main.lean");
        assert_eq!(inputs[1], ("Proofs/lean-toolchain", Some("11".repeat(32).as_str())));
        assert_eq!(inputs[2], ("Proofs/lake-manifest.json", Some("aa".repeat(32).as_str())));
        // The verdict and what it certifies are unchanged by the package context.
        let formal = evidence.formal.as_ref().unwrap();
        assert_eq!(formal.declaration_verdict, Some(FormalVerdict::Checked));
        assert!(formal.dependency_fingerprints.is_none(), "no declaration-level fingerprints are claimed");

        // The same text audited against other pinned dependencies is another
        // audit: new evidence, and the old record is not reused for it.
        let first = record_evidence(&graph, &r, &["Main.a".into()], true, 1).unwrap();
        let mut upgraded = r.clone();
        upgraded.package = Some(package("bb"));
        let second = record_evidence(&graph, &upgraded, &["Main.a".into()], true, 2).unwrap();
        assert!(!second.recorded[0].existing);
        assert_ne!(second.recorded[0].evidence, first.recorded[0].evidence);
        assert!(record_evidence(&graph, &upgraded, &["Main.a".into()], true, 3).unwrap().recorded[0].existing);
        // Without an index of the package files nothing is called stale or current.
        let views = declaration_evidence(&graph, &r.path, Some(&r.content_hash)).unwrap();
        assert!(views[0].evidence.iter().all(|e| matches!(e.freshness, Freshness::Unknown { .. })), "{views:?}");
    }

    /// The Lean environment is part of the audit's identity: another
    /// toolchain, another `lean --version`, or another `lean-toolchain` file
    /// is another audit, for a package and for a standalone file.
    #[test]
    fn the_lean_environment_identifies_the_audit() {
        use crate::types::{LeanAuditFile, LeanAuditPackage};
        let base = record(checked(), vec![decl("Main.a", DeclarationVerdict::Checked, &[])], false);
        let mut toolchain = base.clone();
        toolchain.toolchain = "leanprover/lean4:v4.36.0".into();
        let mut version = base.clone();
        version.toolchain_version = Some("Lean (version 4.35.0, commit 0000)".into());
        let mut unprobed = base.clone();
        unprobed.toolchain_version = None;
        let pin = |digest: &str| {
            let mut r = base.clone();
            r.package = Some(LeanAuditPackage {
                folder: "Proofs".into(),
                toolchain_file: Some(LeanAuditFile {
                    path: WorkspacePath::parse("Proofs/lean-toolchain").unwrap(),
                    sha256: digest.repeat(32),
                }),
                manifest: None,
                mathlib_revision: None,
                dependency_count: 0,
            });
            r
        };
        let ids: Vec<String> =
            [&base, &toolchain, &version, &unprobed, &pin("11"), &pin("22")].into_iter().map(audit_id).collect();
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(audit_id(&base), audit_id(&base.clone()));
    }

    /// Evidence stored before the toolchain was part of the identifier stays
    /// readable and is still the record of the same audit, but never of an
    /// audit with another Lean.
    #[test]
    fn evidence_stored_under_the_earlier_identifier_is_reused_only_for_the_same_lean() {
        let (_dir, graph) = graph();
        let r = record(checked(), vec![decl("Main.a", DeclarationVerdict::Checked, &[])], false);
        let entity = ensure_declaration(&graph, &r, &r.declarations[0], true).unwrap();
        let mut stored = evidence_record(&r, &r.declarations[0], &entity, true, 1).unwrap();
        stored.operation.id = legacy_audit_id(&r);
        assert_ne!(stored.operation.id, audit_id(&r));
        let stored_id = graph.register_evidence(&Origin::Service { service: SERVICE.into() }, stored).unwrap();
        let views = declaration_evidence(&graph, &r.path, Some(&r.content_hash)).unwrap();
        assert_eq!(views[0].evidence.len(), 1, "the stored record is listed");

        let same = record_evidence(&graph, &r, &["Main.a".into()], true, 2).unwrap();
        assert!(same.recorded[0].existing && same.recorded[0].evidence == stored_id);
        let mut other = r.clone();
        other.toolchain = "leanprover/lean4:v4.36.0".into();
        other.toolchain_version = Some("Lean (version 4.36.0)".into());
        let new = record_evidence(&graph, &other, &["Main.a".into()], true, 3).unwrap();
        assert!(!new.recorded[0].existing && new.recorded[0].evidence != stored_id);
        assert_eq!(declaration_evidence(&graph, &r.path, Some(&r.content_hash)).unwrap()[0].evidence.len(), 2);
    }

    #[test]
    fn people_cannot_mint_formal_evidence_but_can_review_an_association() {
        let (_dir, graph) = graph();
        let r = record(checked(), vec![decl("Main.a", DeclarationVerdict::Checked, &[])], false);
        let rec = record_evidence(&graph, &r, &["Main.a".into()], true, 1).unwrap();
        let decl_id = rec.recorded[0].entity.clone();
        // An informal theorem (created by a person).
        let (_, outcome) = graph
            .propose_and_apply(
                Origin::User,
                &ChangeRequest {
                    base_revision: 0,
                    operations: vec![GraphOperation::CreateEntity {
                        temp_id: "t".into(),
                        kind: EntityKind::Theorem,
                        roles: vec![],
                        label: "Theorem 1".into(),
                        key: None,
                        attributes: BTreeMap::new(),
                        binding: None,
                    }],
                    summary: None,
                },
            )
            .unwrap();
        let exposition_graph::ApplyOutcome::Applied { created, .. } = outcome else { panic!("{outcome:?}") };
        let theorem = created[0].id.clone();
        // The webview/agent path cannot register formal evidence.
        let forged =
            Evidence { id: EvidenceId::mint(), ..evidence_record(&r, &r.declarations[0], &decl_id, true, 1).unwrap() };
        assert!(graph.register_evidence(&Origin::User, forged.clone()).is_err());
        assert!(graph.register_evidence(&Origin::Agent { task_id: "t".into() }, forged).is_err());
        // Nor cite formal-check support without an existing service record.
        let (p, _) = graph
            .propose_and_apply(
                Origin::User,
                &ChangeRequest {
                    base_revision: 0,
                    operations: vec![GraphOperation::AddRelation {
                        kind: RelationKind::EvidenceFor,
                        from: exposition_graph::EntityRef::Id { id: decl_id.clone() },
                        to: exposition_graph::EntityRef::Id { id: theorem.clone() },
                        evidence_kind: Some(EvidenceKind::FormalCheck),
                        evidence: vec![],
                        note: None,
                    }],
                    summary: None,
                },
            )
            .unwrap();
        assert!(!p.valid);
        // The reviewed association is an accepted `formalizes` relation, kept separate from the verdict.
        let a = associate(&graph, &decl_id, &theorem, Some("matches Theorem 1".into())).unwrap();
        assert_eq!(a.status, AssertionStatus::Accepted);
        assert!(!a.existing);
        let again = associate(&graph, &decl_id, &theorem, None).unwrap();
        assert!(again.existing);
        assert_eq!(again.relation, a.relation);
        // The statement changes (an unbound item is its label): the reviewed
        // association needs re-review, and associating again — the person's
        // review of the statement as it is now — confirms it.
        let review = |graph: &GraphService| {
            graph.with_store(|s| Ok(s.relation(&a.relation)?)).unwrap().and_then(|r| r.review).map(|r| r.state)
        };
        assert_eq!(review(&graph), Some(exposition_graph::LinkReviewState::Current));
        let entity = graph.entity_detail(&theorem).unwrap().entity;
        let (p, _) = graph
            .propose_and_apply(
                Origin::User,
                &ChangeRequest {
                    base_revision: graph.status().revision,
                    operations: vec![GraphOperation::UpdateEntity {
                        entity: theorem.clone(),
                        expected_revision: entity.revision,
                        label: Some("A different statement".into()),
                        attributes: None,
                    }],
                    summary: None,
                },
            )
            .unwrap();
        assert!(p.valid, "{:?}", p.diagnostics);
        assert_eq!(review(&graph), Some(exposition_graph::LinkReviewState::StatementChanged));
        let listed = declaration_evidence(&graph, &r.path, Some(&r.content_hash)).unwrap();
        let shown = listed.iter().flat_map(|d| d.associations.iter()).find(|v| v.relation == a.relation).unwrap();
        assert!(shown.review.as_ref().is_some_and(|v| v.needs_re_review()), "the Lean panel is told: {shown:?}");
        let confirmed = associate(&graph, &decl_id, &theorem, None).unwrap();
        assert!(confirmed.existing);
        assert_eq!(confirmed.relation, a.relation);
        assert_eq!(review(&graph), Some(exposition_graph::LinkReviewState::Current));
        // Only formal declarations formalize.
        assert!(associate(&graph, &theorem, &decl_id, None).is_err());
        let math = graph.math_overview().unwrap();
        let row = math.rows.iter().find(|r| r.entity.id == theorem).unwrap();
        assert_eq!(row.formal_association, exposition_graph::FormalAssociationState::Reviewed);
        assert_eq!(row.verdict, Some(FormalVerdict::Checked));
        assert!(!matches!(row.freshness, Some(Freshness::Current)), "unknown dependency freshness is not current");
    }
}
