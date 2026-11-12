//! `graph_propose`: typed graph operations from an agent become a validated
//! proposal (the graph service's one validator for people, presets, agents,
//! and services), applied through the central gate.
//!
//! - The operations are a closed set with a strict schema; the backend sets
//!   the origin (`Agent { task }`), so agent relations are always suggestions,
//!   and agents can neither accept/reject relations, create indexed kinds,
//!   nor mint service-owned evidence (the validator refuses all three).
//! - Saved investigations and workflows are deliberate admissions made
//!   elsewhere (the user keeps a task as an investigation; workflow drafts
//!   have their own tool), so agents cannot create those kinds here.
//! - Every entity, relation, and file the operations name must be visible to
//!   the task (scope rules as for files); in a narrower scope, new entities
//!   must be bound to a file in it.
//! - In a review-edits task the user approves the complete payload (every
//!   effect and every operation); in an auto-apply task it is applied
//!   directly; a read-only task is refused before anything is proposed.

use std::collections::BTreeMap;
use std::path::Path;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_graph::{
    ApplyChangeRequest, ApplyOutcome, ChangeProposal, ChangeRequest, DiagnosticSeverity, EntityId, EntityKind,
    EntityRef, EvidenceId, EvidenceKind, GraphDiagnostic, GraphOperation, Lifecycle, NewBinding, Origin, RelationId,
    RelationKind, SourceSelector,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::context::Visibility;
use super::{GraphEnv, ProposalState, TaskProposal, bound_status, refusal};
use crate::model::{ContextConsent, TaskId, TaskSpec};
use crate::store::Store;
use crate::tools::exec::{ToolOutput, codes, shown};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    pub name: String,
    pub value: String,
}

/// One operation as the agent writes it (a flat, strict shape per kind).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentOperation {
    CreateEntity {
        temp_id: String,
        kind: String,
        label: String,
        attributes: Vec<Attribute>,
        bind_path: String,
        bind_start: i64,
        bind_end: i64,
    },
    AddRelation {
        relation: String,
        from: String,
        to: String,
        evidence_kind: String,
        evidence: Vec<String>,
        note: String,
    },
    UpdateEntity {
        entity: String,
        expected_revision: u64,
        label: String,
        attributes: Vec<Attribute>,
        replace_attributes: bool,
    },
    SetLifecycle {
        entity: String,
        expected_revision: u64,
        lifecycle: String,
    },
    RemoveRelation {
        relation: String,
    },
    BindSource {
        entity: String,
        bind_path: String,
        bind_start: i64,
        bind_end: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProposeInput {
    pub summary: String,
    pub base_revision: u64,
    pub operations: Vec<AgentOperation>,
}

/// Kinds an agent may create here.
pub const AGENT_CREATABLE: [EntityKind; 16] = [
    EntityKind::Collection,
    EntityKind::ExternalResource,
    EntityKind::Definition,
    EntityKind::Assumption,
    EntityKind::Conjecture,
    EntityKind::Lemma,
    EntityKind::Theorem,
    EntityKind::Proposition,
    EntityKind::Corollary,
    EntityKind::Claim,
    EntityKind::Proof,
    EntityKind::Question,
    EntityKind::MathObject,
    EntityKind::Reference,
    EntityKind::Experiment,
    EntityKind::Finding,
];

/// Relation kinds people and agents may add.
pub fn manual_relations() -> Vec<RelationKind> {
    RelationKind::ALL.into_iter().filter(|k| k.is_manual()).collect()
}

/// A failure to translate, with its stable code.
pub struct Invalid {
    pub code: &'static str,
    pub message: String,
}

fn invalid(message: impl Into<String>) -> Invalid {
    Invalid { code: codes::GRAPH_INVALID, message: message.into() }
}

fn entity_ref(raw: &str) -> Result<EntityRef, Invalid> {
    let raw = raw.trim();
    if let Ok(id) = EntityId::parse(raw) {
        return Ok(EntityRef::Id { id });
    }
    let temp_ok =
        !raw.is_empty() && raw.len() <= 32 && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if temp_ok {
        Ok(EntityRef::Temp { temp_id: raw.to_owned() })
    } else {
        Err(invalid(format!("{raw:?} is neither an entity id (ent-…) nor a temp_id created earlier in this change")))
    }
}

fn binding(path: &str, start: i64, end: i64) -> Result<Option<NewBinding>, Invalid> {
    let path = path.trim();
    if path.is_empty() {
        if start != -1 || end != -1 {
            return Err(invalid("bind_start/bind_end need a bind_path"));
        }
        return Ok(None);
    }
    let path = WorkspacePath::parse(path)
        .map_err(|e| Invalid { code: codes::INVALID_PATH, message: format!("invalid bind_path {path:?}: {e}") })?;
    let (selector, range) = match (start, end) {
        (-1, -1) => (SourceSelector::WholeFile, None),
        (s, e) if s >= 0 && e >= s && e <= u32::MAX as i64 => {
            (SourceSelector::Region, Some(TextRange { start: s as u32, end: e as u32 }))
        }
        _ => {
            return Err(invalid(
                "bind_start and bind_end are UTF-16 offsets with start ≤ end, or both -1 for the whole file",
            ));
        }
    };
    Ok(Some(NewBinding { path, selector, range }))
}

fn attributes(list: &[Attribute]) -> Result<BTreeMap<String, String>, Invalid> {
    let mut map = BTreeMap::new();
    for a in list {
        if map.insert(a.name.clone(), a.value.clone()).is_some() {
            return Err(invalid(format!("attribute {:?} is given twice", a.name)));
        }
    }
    Ok(map)
}

/// The typed change request, and the paths the operations bind.
pub fn translate(input: &GraphProposeInput) -> Result<ChangeRequest, Invalid> {
    if input.operations.is_empty() {
        return Err(invalid("operations must contain at least one operation"));
    }
    if input.operations.len() > 100 {
        return Err(invalid("at most 100 operations per proposal"));
    }
    if input.summary.len() > 500 {
        return Err(invalid("the summary is longer than 500 bytes"));
    }
    let mut operations = Vec::with_capacity(input.operations.len());
    for (i, op) in input.operations.iter().enumerate() {
        let n = i + 1;
        let at = |e: Invalid| Invalid { code: e.code, message: format!("operation #{n}: {}", e.message) };
        operations.push(match op {
            AgentOperation::CreateEntity {
                temp_id,
                kind,
                label,
                attributes: attrs,
                bind_path,
                bind_start,
                bind_end,
            } => {
                let kind =
                    EntityKind::parse(kind).ok_or_else(|| at(invalid(format!("unknown entity kind {kind:?}"))))?;
                if matches!(kind, EntityKind::Investigation | EntityKind::Workflow) {
                    return Err(at(Invalid {
                        code: codes::GRAPH_RESERVED,
                        message: format!(
                            "{} entities are deliberate admissions: the user keeps a task as an investigation, and \
                             workflow drafts are saved through their own reviewed tool",
                            kind.title()
                        ),
                    }));
                }
                GraphOperation::CreateEntity {
                    temp_id: temp_id.clone(),
                    kind,
                    roles: Vec::new(),
                    label: label.clone(),
                    key: None,
                    attributes: attributes(attrs).map_err(at)?,
                    binding: binding(bind_path, *bind_start, *bind_end).map_err(at)?,
                }
            }
            AgentOperation::AddRelation { relation, from, to, evidence_kind, evidence, note } => {
                let kind = RelationKind::parse(relation)
                    .ok_or_else(|| at(invalid(format!("unknown relation {relation:?}"))))?;
                let evidence_kind = match evidence_kind.trim() {
                    "" => None,
                    raw => Some(
                        EvidenceKind::parse(raw)
                            .ok_or_else(|| at(invalid(format!("unknown evidence kind {raw:?}"))))?,
                    ),
                };
                let evidence = evidence
                    .iter()
                    .map(|e| EvidenceId::parse(e.trim()).map_err(|_| at(invalid(format!("invalid evidence id {e:?}")))))
                    .collect::<Result<Vec<_>, _>>()?;
                GraphOperation::AddRelation {
                    kind,
                    from: entity_ref(from).map_err(at)?,
                    to: entity_ref(to).map_err(at)?,
                    evidence_kind,
                    evidence,
                    note: (!note.trim().is_empty()).then(|| note.clone()),
                }
            }
            AgentOperation::UpdateEntity {
                entity,
                expected_revision,
                label,
                attributes: attrs,
                replace_attributes,
            } => {
                if !replace_attributes && !attrs.is_empty() {
                    return Err(at(invalid("attributes are only applied with replace_attributes: true")));
                }
                GraphOperation::UpdateEntity {
                    entity: EntityId::parse(entity.trim()).map_err(|_| at(invalid("entity must be an entity id")))?,
                    expected_revision: *expected_revision,
                    label: (!label.trim().is_empty()).then(|| label.clone()),
                    attributes: if *replace_attributes { Some(attributes(attrs).map_err(at)?) } else { None },
                }
            }
            AgentOperation::SetLifecycle { entity, expected_revision, lifecycle } => GraphOperation::SetLifecycle {
                entity: EntityId::parse(entity.trim()).map_err(|_| at(invalid("entity must be an entity id")))?,
                expected_revision: *expected_revision,
                lifecycle: match lifecycle.as_str() {
                    "active" => Lifecycle::Active,
                    "archived" => Lifecycle::Archived,
                    "discarded" => Lifecycle::Discarded,
                    other => {
                        return Err(at(invalid(format!(
                            "lifecycle must be active, archived, or discarded, not {other:?}"
                        ))));
                    }
                },
            },
            AgentOperation::RemoveRelation { relation } => GraphOperation::RemoveRelation {
                relation: RelationId::parse(relation.trim())
                    .map_err(|_| at(invalid("relation must be a relation id")))?,
            },
            AgentOperation::BindSource { entity, bind_path, bind_start, bind_end } => GraphOperation::BindSource {
                entity: entity_ref(entity).map_err(at)?,
                binding: binding(bind_path, *bind_start, *bind_end)
                    .map_err(at)?
                    .ok_or_else(|| at(invalid("bind_source needs a bind_path")))?,
            },
        });
    }
    let summary = input.summary.trim();
    Ok(ChangeRequest {
        base_revision: input.base_revision,
        operations,
        summary: (!summary.is_empty()).then(|| summary.to_owned()),
    })
}

/// Paths the operations bind (scope-checked by the gate like any path).
pub fn bound_paths(input: &GraphProposeInput) -> Vec<&str> {
    input
        .operations
        .iter()
        .filter_map(|op| match op {
            AgentOperation::CreateEntity { bind_path, .. } | AgentOperation::BindSource { bind_path, .. } => {
                Some(bind_path.trim()).filter(|p| !p.is_empty())
            }
            _ => None,
        })
        .collect()
}

/// A validated proposal, as shown for approval.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub proposal: ChangeProposal,
    /// Complete approval text (effects and every operation).
    pub detail: String,
    pub title: String,
}

/// Diagnostics as the task may see them: without content consent the
/// messages (which quote labels) are replaced by the rule they broke.
pub fn visible_diagnostics(diagnostics: &[GraphDiagnostic], consent: ContextConsent) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|d| {
            let mut v = json!({"operation": d.operation, "code": d.code, "severity": d.severity});
            if consent == ContextConsent::ScopeContent {
                v["message"] = json!(d.message);
            }
            v
        })
        .collect()
}

/// Effects as the task may see them (labels only with content consent).
pub fn visible_effects(proposal: &ChangeProposal, consent: ContextConsent) -> Vec<String> {
    if consent == ContextConsent::ScopeContent {
        return proposal.effects.clone();
    }
    proposal
        .operations
        .iter()
        .map(|op| match op {
            GraphOperation::CreateEntity { kind, .. } => format!("create {}", kind.as_str()),
            GraphOperation::UpdateEntity { entity, .. } => format!("update {entity}"),
            GraphOperation::SetLifecycle { entity, lifecycle, .. } => format!("mark {entity} {}", lifecycle.as_str()),
            GraphOperation::AddRelation { kind, .. } => format!("suggest {}", kind.as_str()),
            GraphOperation::SetRelationStatus { relation, .. } => format!("review {relation}"),
            GraphOperation::RemoveRelation { relation } => format!("remove {relation}"),
            GraphOperation::BindSource { binding, .. } => format!("bind to {}", binding.path),
        })
        .collect()
}

/// Validate the call: scope of every referenced record, then the graph's own
/// validation. Invalid proposals are refused before anyone is asked.
pub fn prepare(
    graph: GraphEnv<'_>,
    root: &Path,
    spec: &TaskSpec,
    task: &TaskId,
    input: &GraphProposeInput,
) -> Result<Prepared, ToolOutput> {
    bound_status(graph, root)?;
    let request = translate(input).map_err(|e| refusal(e.code, e.message))?;
    let vis = Visibility::new(root, &spec.scope, spec.consent);
    let checked = graph.service.with_store(|store| {
        let out_of_scope = |what: String| {
            Err(refusal(
                codes::OUT_OF_SCOPE,
                format!(
                    "{what} is not visible to this task (it does not exist, or it is outside the task's scope: {}). The \
                     application refused the change; work within the scope.",
                    spec.scope.describe()
                ),
            ))
        };
        for (i, op) in request.operations.iter().enumerate() {
            let n = i + 1;
            let mut ids: Vec<&EntityId> = Vec::new();
            match op {
                GraphOperation::CreateEntity { binding, .. } => {
                    if !vis.whole_project() && binding.is_none() {
                        return Ok(Err(refusal(
                            codes::OUT_OF_SCOPE,
                            format!(
                                "operation #{n}: this task is scoped to {}; a new graph entity must be bound to a file \
                                 in that scope (bind_path).",
                                spec.scope.describe()
                            ),
                        )));
                    }
                }
                GraphOperation::UpdateEntity { entity, .. } | GraphOperation::SetLifecycle { entity, .. } => {
                    ids.push(entity)
                }
                GraphOperation::AddRelation { from, to, .. } => {
                    for r in [from, to] {
                        if let EntityRef::Id { id } = r {
                            ids.push(id);
                        }
                    }
                }
                GraphOperation::BindSource { entity: EntityRef::Id { id }, .. } => ids.push(id),
                GraphOperation::BindSource { .. } => {}
                GraphOperation::RemoveRelation { relation } | GraphOperation::SetRelationStatus { relation, .. } => {
                    match store.relation(relation)? {
                        Some(r) => {
                            for end in [&r.from, &r.to] {
                                if vis.entity_id(store, end)?.is_none() {
                                    return Ok(out_of_scope(format!("operation #{n}: relation {relation}")));
                                }
                            }
                        }
                        None => return Ok(out_of_scope(format!("operation #{n}: relation {relation}"))),
                    }
                }
            }
            for id in ids {
                if store.entity(id)?.is_some() && vis.entity_id(store, id)?.is_none() {
                    return Ok(out_of_scope(format!("operation #{n}: entity {id}")));
                }
            }
        }
        Ok(Ok(()))
    });
    checked.map_err(|e| super::unavailable(&e.message))??;
    let origin = Origin::Agent { task_id: task.0.clone() };
    let proposal = graph.service.propose_change(origin, &request).map_err(|e| super::unavailable(&e.message))?;
    if !proposal.valid {
        let errors = proposal.diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Error).count();
        return Err(ToolOutput::error_with(
            format!(
                "The graph change is invalid ({errors} error(s)); nothing was proposed to the user or applied. Fix the \
                 operations and call graph_propose again."
            ),
            json!({
                "code": codes::GRAPH_INVALID,
                "diagnostics": visible_diagnostics(&proposal.diagnostics, spec.consent),
            }),
        ));
    }
    let detail = approval_detail(&proposal);
    let title = format!(
        "Propose {} graph change{}",
        proposal.operations.len(),
        if proposal.operations.len() == 1 { "" } else { "s" }
    );
    Ok(Prepared { proposal, detail, title })
}

/// The complete approval payload: every effect and every operation.
fn approval_detail(proposal: &ChangeProposal) -> String {
    let mut detail = format!(
        "Change the project graph ({} operation(s), based on graph revision {}). Relationships the agent adds are \
         recorded as suggestions; you can accept or reject them later.",
        proposal.operations.len(),
        proposal.base_revision
    );
    let request_summary = proposal.diagnostics.iter().filter(|d| d.severity != DiagnosticSeverity::Error);
    detail.push_str("\n\nEffects:\n");
    for effect in &proposal.effects {
        detail.push_str(&format!("- {}\n", shown(effect)));
    }
    let warnings: Vec<String> = request_summary.map(|d| format!("- {} ({})", shown(&d.message), d.code)).collect();
    if !warnings.is_empty() {
        detail.push_str("\nWarnings:\n");
        detail.push_str(&warnings.join("\n"));
        detail.push('\n');
    }
    let operations = serde_json::to_string_pretty(&proposal.operations).unwrap_or_default();
    detail.push_str("\nOperations (complete):\n");
    detail.push_str(&shown(&operations));
    detail
}

/// Apply a prepared proposal (after the gate and any approval).
pub fn apply(
    graph: GraphEnv<'_>,
    root: &Path,
    spec: &TaskSpec,
    task: &TaskId,
    store: &Store,
    tool_use_id: &str,
    prepared: &Prepared,
) -> Result<ToolOutput, ToolOutput> {
    if let Err(refused) = bound_status(graph, root) {
        record(store, task, tool_use_id, prepared, ProposalState::Failed, None, Some(error_text(&refused)));
        return Err(refused);
    }
    let origin = Origin::Agent { task_id: task.0.clone() };
    let request = ApplyChangeRequest {
        proposal: prepared.proposal.id.clone(),
        expected_revision: prepared.proposal.base_revision,
    };
    let outcome = match graph.service.apply_change(&origin, &request) {
        Ok(outcome) => outcome,
        Err(e) => {
            let refused = refusal(codes::GRAPH_UNAVAILABLE, format!("The graph change was not applied: {}", e.message));
            record(store, task, tool_use_id, prepared, ProposalState::Failed, None, Some(e.message));
            return Err(refused);
        }
    };
    match outcome {
        ApplyOutcome::Applied { revision, rebased, created, delta } => {
            record(store, task, tool_use_id, prepared, ProposalState::Applied, Some((revision, created.clone())), None);
            Ok(ToolOutput::ok(json!({
                "applied": true,
                "proposal": prepared.proposal.id,
                "revision": revision,
                "rebased": rebased,
                "created": created,
                "delta": delta,
                "effects": visible_effects(&prepared.proposal, spec.consent),
                "note": "Applied to the project graph. Relationships you added are suggestions until a person accepts them.",
            })))
        }
        ApplyOutcome::Conflict { current_revision, diagnostics }
        | ApplyOutcome::Invalid { current_revision, diagnostics } => {
            let conflict = matches!(outcome_kind(&diagnostics), Some(true));
            let (state, code) = if conflict {
                (ProposalState::Conflict, codes::GRAPH_CONFLICT)
            } else {
                (ProposalState::Invalid, codes::GRAPH_INVALID)
            };
            let mut stored = prepared.clone();
            stored.proposal.diagnostics = diagnostics.clone();
            record(store, task, tool_use_id, &stored, state, None, None);
            Err(ToolOutput::error_with(
                "The graph changed since the proposal was validated and the change no longer applies; nothing was \
                 applied. Query the graph again and propose the change anew.",
                json!({
                    "code": code,
                    "currentRevision": current_revision,
                    "diagnostics": visible_diagnostics(&diagnostics, spec.consent),
                }),
            ))
        }
    }
}

/// A conflict is reported with stale/unknown records; anything else is invalid.
fn outcome_kind(diagnostics: &[GraphDiagnostic]) -> Option<bool> {
    Some(
        diagnostics
            .iter()
            .any(|d| matches!(d.code.as_str(), "graph.staleEntity" | "graph.unknownEntity" | "graph.unknownRelation")),
    )
}

fn error_text(output: &ToolOutput) -> String {
    output.content.get("error").and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Record a proposal's state with the task.
pub fn record(
    store: &Store,
    task: &TaskId,
    tool_use_id: &str,
    prepared: &Prepared,
    state: ProposalState,
    applied: Option<(u64, Vec<exposition_graph::CreatedEntity>)>,
    message: Option<String>,
) {
    let now = crate::store::now_ms();
    let existing = store.proposal(task, tool_use_id).ok().flatten();
    let (revision, created) = match applied {
        Some((r, c)) => (Some(r), c),
        None => (None, Vec::new()),
    };
    let record = TaskProposal {
        tool_use_id: tool_use_id.to_owned(),
        proposal: prepared.proposal.id.clone(),
        state,
        summary: None,
        base_revision: prepared.proposal.base_revision,
        effects: prepared.proposal.effects.clone(),
        operations: serde_json::to_value(&prepared.proposal.operations).unwrap_or(Value::Null),
        diagnostics: prepared.proposal.diagnostics.clone(),
        revision,
        created,
        message,
        created_at_ms: existing.as_ref().map(|p| p.created_at_ms).unwrap_or(now),
        updated_at_ms: now,
    };
    if let Err(e) = store.upsert_proposal(task, &record) {
        tracing::error!("failed to record a graph proposal: {e}");
    }
}
