//! `graph_query`: bounded, scope- and consent-filtered reads of the project
//! graph (entity search, one entity, a neighborhood, what is in a file, and
//! change impact). A graph query is never a side channel around the task's
//! scope or consent: invisible entities are never returned, never traversed
//! through, and never matched by text the task could not read.

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;

use exposition_core::path::WorkspacePath;
use exposition_graph::store::GraphStore;
use exposition_graph::{
    AssertionStatus, EntityId, EntityKind, Evidence, EvidenceKind, EvidenceOutcome, Freshness, ImpactRequest,
    Lifecycle, RelationKind, service::evidence_freshness,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::context::Visibility;
use super::{ContextEntity, ContextRelation, GraphEnv, bound_status, header, refusal, truncate_chars};
use crate::model::TaskSpec;
use crate::tools::exec::{ToolOutput, codes};

pub const MAX_LIMIT: u32 = 100;
pub const MAX_DEPTH: u32 = 3;
const MAX_RELATIONS: usize = 300;
/// Serialized size bound of one result.
const MAX_RESULT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphQueryInput {
    pub query: String,
    pub text: String,
    pub kinds: Vec<String>,
    pub entity: String,
    pub path: String,
    pub depth: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    Search,
    Entity,
    Neighborhood,
    Inspect,
    Impact,
}

impl GraphQueryInput {
    /// Validate beyond the schema.
    pub fn validate(&self) -> Result<QueryKind, String> {
        let kind = match self.query.as_str() {
            "search" => QueryKind::Search,
            "entity" => QueryKind::Entity,
            "neighborhood" => QueryKind::Neighborhood,
            "inspect" => QueryKind::Inspect,
            "impact" => QueryKind::Impact,
            other => return Err(format!("unknown query {other:?}")),
        };
        if self.text.len() > 200 {
            return Err("text is longer than 200 bytes".into());
        }
        if !(1..=MAX_LIMIT).contains(&self.limit) {
            return Err(format!("limit must be between 1 and {MAX_LIMIT}"));
        }
        if !(1..=MAX_DEPTH).contains(&self.depth) {
            return Err(format!("depth must be between 1 and {MAX_DEPTH}"));
        }
        for k in &self.kinds {
            if EntityKind::parse(k).is_none() {
                return Err(format!("unknown entity kind {k:?}"));
            }
        }
        if matches!(kind, QueryKind::Entity | QueryKind::Neighborhood | QueryKind::Impact) {
            EntityId::parse(self.entity.trim()).map_err(|_| "entity must be an entity id (ent-…)".to_owned())?;
        }
        if kind == QueryKind::Inspect && self.path.trim().is_empty() {
            return Err("inspect needs a path".into());
        }
        Ok(kind)
    }

    /// The project path the query names (inspect only).
    pub fn named_path(&self) -> Option<&str> {
        (self.query == "inspect").then_some(self.path.as_str())
    }
}

/// How evidence comes to exist and where it is recorded, stated with every
/// result that shows formal declarations or their associations, so that a
/// task does not have to guess why a check it ran left no evidence.
pub const FORMAL_EVIDENCE_NOTE: &str = "\
    Evidence is recorded only by Exposition's services, on the entity it is about. check_lean returns its result \
    to you and records no evidence. Formal (Lean) evidence is recorded for a formal declaration when the user \
    opens the Lean file and chooses Record evidence (or Associate with…) for that declaration in the Lean panel, \
    or accepts the association a workflow's Lean audit proposes. It is never recorded on the statement the \
    declaration formalizes: a formalizes relation, suggested or accepted, is a correspondence a person reviews, \
    not a proof that the two say the same. Query entity on a statement lists the evidence of its associated \
    declarations under formalEvidence.";

/// What an impact result for a file covers.
pub const FILE_IMPACT_NOTE: &str = "\
    For a file, the items located in it are listed (declared in it or bound to it; locatedIn names the file). \
    Each may or may not change when the file does, so it is informational and what depends on it is not followed \
    here: query impact for an item to follow its relations. evidence lists the recorded evidence that a change \
    would make historical.";

/// One evidence record as a task may see it: kind, outcome kind, freshness;
/// the formal statement and axioms only with content consent.
fn evidence_item(store: &GraphStore, vis: &Visibility<'_>, record: &Evidence) -> Value {
    let freshness = match evidence_freshness(store, record) {
        Freshness::Current => "current",
        Freshness::Stale { .. } => "stale",
        Freshness::Unknown { .. } => "unknown",
    };
    let outcome = match &record.outcome {
        EvidenceOutcome::Succeeded => json!("succeeded"),
        EvidenceOutcome::Failed { .. } => json!("failed"),
        EvidenceOutcome::Canceled => json!("canceled"),
        EvidenceOutcome::Interrupted => json!("interrupted"),
        EvidenceOutcome::Verdict { verdict } => json!({"verdict": verdict}),
        EvidenceOutcome::Reported { .. } => json!("reported"),
    };
    let mut item = json!({
        "id": record.id,
        "kind": record.kind,
        "operation": record.operation.kind,
        "outcome": outcome,
        "freshness": freshness,
        "recordedAtMs": record.recorded_at_ms,
    });
    if let Some(formal) = &record.formal {
        item["declarationVerdict"] = json!(formal.declaration_verdict);
        item["fileVerdict"] = json!(formal.file_verdict);
        if vis.content() {
            item["declaration"] = json!(formal.declaration);
            item["statement"] = json!(truncate_chars(&formal.statement, 2000));
            item["axioms"] = json!(formal.axioms);
        }
    }
    item
}

/// Whether a result shows formal declarations or formal associations.
fn mentions_formal(entities: &[ContextEntity], relations: &[ContextRelation]) -> bool {
    entities.iter().any(|e| e.kind == EntityKind::FormalDeclaration)
        || relations.iter().any(|r| r.kind == RelationKind::Formalizes)
}

/// The id is not echoed: the refusal reads the same whether the entity is
/// outside the scope or does not exist.
fn not_visible(_id: &str) -> ToolOutput {
    refusal(
        codes::OUT_OF_SCOPE,
        "That entity is not visible to this task (it does not exist, or it is outside the task's scope). The \
         application refused the access; work within the scope.",
    )
}

/// Trim a result to its size bound.
fn bounded(mut out: Value) -> Value {
    for key in ["relations", "entities", "items", "neighbors"] {
        while serde_json::to_string(&out).map(|s| s.len()).unwrap_or(0) > MAX_RESULT_BYTES {
            let Some(list) = out.get_mut(key).and_then(Value::as_array_mut) else { break };
            if list.is_empty() {
                break;
            }
            let keep = list.len() / 2;
            list.truncate(keep);
            out["truncated"] = json!(true);
        }
    }
    out
}

/// Run a validated query.
pub fn run(
    graph: GraphEnv<'_>,
    root: &Path,
    spec: &TaskSpec,
    input: &GraphQueryInput,
) -> Result<ToolOutput, ToolOutput> {
    let kind = input.validate().map_err(ToolOutput::error)?;
    let status = bound_status(graph, root)?;
    let vis = Visibility::new(root, &spec.scope, spec.consent);
    let limit = input.limit as usize;
    let mut out = match kind {
        QueryKind::Search => search(graph, &vis, input, limit)?,
        QueryKind::Entity => entity(graph, &vis, input)?,
        QueryKind::Neighborhood => neighborhood(graph, &vis, input, limit)?,
        QueryKind::Inspect => inspect(graph, &vis, input, limit)?,
        QueryKind::Impact => impact(graph, &vis, input, limit)?,
    };
    out["query"] = json!(input.query);
    out["graph"] = header(&status);
    if !vis.content() {
        out["consent"] = json!("metadataOnly: labels, notes, and reasons are withheld");
    }
    Ok(ToolOutput::ok(bounded(out)))
}

fn store_error(e: exposition_graph::GraphError) -> ToolOutput {
    super::unavailable(&e.message)
}

fn search(
    graph: GraphEnv<'_>,
    vis: &Visibility<'_>,
    input: &GraphQueryInput,
    limit: usize,
) -> Result<Value, ToolOutput> {
    let mut kinds: Vec<EntityKind> = input.kinds.iter().filter_map(|k| EntityKind::parse(k)).collect();
    let text = input.text.trim();
    // Without content consent, text matches only what the task may read:
    // file and folder paths (their labels are their names).
    if !vis.content() && !text.is_empty() {
        let files = [EntityKind::Resource, EntityKind::Folder];
        kinds =
            if kinds.is_empty() { files.to_vec() } else { kinds.into_iter().filter(|k| files.contains(k)).collect() };
        if kinds.is_empty() {
            return Ok(json!({"entities": [], "relations": [], "truncated": false}));
        }
    }
    graph
        .service
        .with_store(|store| {
            let (found, _) = store.find_entities(
                (!kinds.is_empty()).then_some(kinds.as_slice()),
                (!text.is_empty()).then_some(text),
                false,
                2000,
            )?;
            let mut entities: Vec<ContextEntity> = Vec::new();
            let mut ids = BTreeSet::new();
            let mut truncated = false;
            for e in found {
                let Some(bindings) = vis.entity(store, &e)? else { continue };
                if entities.len() >= limit {
                    truncated = true;
                    break;
                }
                ids.insert(e.id.clone());
                entities.push(vis.describe(store, &e, &bindings)?);
            }
            let relations: Vec<ContextRelation> =
                store.relations_among(&ids)?.iter().take(MAX_RELATIONS).map(|r| vis.relation(r)).collect();
            Ok(json!({"entities": entities, "relations": relations, "truncated": truncated}))
        })
        .map_err(store_error)
}

fn entity(graph: GraphEnv<'_>, vis: &Visibility<'_>, input: &GraphQueryInput) -> Result<Value, ToolOutput> {
    let id = EntityId::parse(input.entity.trim()).map_err(|e| ToolOutput::error(e.to_string()))?;
    graph
        .service
        .with_store(|store| {
            let Some((e, bindings)) = vis.entity_id(store, &id)? else { return Ok(Err(not_visible(id.as_str()))) };
            let described = vis.describe(store, &e, &bindings)?;
            let mut relations = Vec::new();
            let mut neighbors = Vec::new();
            let mut seen = BTreeSet::new();
            let mut all = store.relations_from(&id)?;
            all.extend(store.relations_to(&id)?);
            let mut truncated = false;
            for r in all {
                let other = if r.from == id { &r.to } else { &r.from };
                let Some((o, ob)) = vis.entity_id(store, other)? else { continue };
                if relations.len() >= MAX_RELATIONS {
                    truncated = true;
                    break;
                }
                relations.push(vis.relation(&r));
                if seen.insert(o.id.clone()) {
                    neighbors.push(vis.describe(store, &o, &ob)?);
                }
            }
            // Evidence about the entity itself.
            let evidence: Vec<Value> =
                store.evidence_for_subject(&id)?.iter().take(20).map(|r| evidence_item(store, vis, r)).collect();
            // Formal evidence belongs to a declaration, never to what it
            // formalizes: for an informal entity, the evidence of each
            // associated declaration is listed apart, with the state of the
            // association (rejected ones are left out).
            let mut formal_evidence = Vec::new();
            if e.kind != EntityKind::FormalDeclaration {
                for r in store.relations_to(&id)? {
                    if r.kind != RelationKind::Formalizes || r.status == AssertionStatus::Rejected {
                        continue;
                    }
                    let Some((declaration, _)) = vis.entity_id(store, &r.from)? else { continue };
                    if formal_evidence.len() >= 10 {
                        truncated = true;
                        break;
                    }
                    let mut records: Vec<Evidence> = store
                        .evidence_for_subject(&declaration.id)?
                        .into_iter()
                        .filter(|x| x.kind == EvidenceKind::FormalCheck)
                        .collect();
                    records.sort_by_key(|x| std::cmp::Reverse(x.recorded_at_ms));
                    let records: Vec<Value> = records.iter().take(5).map(|x| evidence_item(store, vis, x)).collect();
                    formal_evidence.push(json!({
                        "declaration": declaration.id,
                        "label": vis.label(&declaration),
                        "relation": r.id,
                        "association": r.status,
                        "evidence": records,
                    }));
                }
            }
            let formal = e.kind == EntityKind::FormalDeclaration
                || !formal_evidence.is_empty()
                || mentions_formal(&neighbors, &relations);
            let mut out = json!({
                "entity": described,
                "relations": relations,
                "neighbors": neighbors,
                "evidence": evidence,
                "truncated": truncated,
            });
            if e.kind != EntityKind::FormalDeclaration && !formal_evidence.is_empty() {
                out["formalEvidence"] = json!(formal_evidence);
            }
            if formal {
                out["evidenceNote"] = json!(FORMAL_EVIDENCE_NOTE);
            }
            if vis.content() && !e.attributes.is_empty() {
                let attributes: serde_json::Map<String, Value> =
                    e.attributes.iter().map(|(k, v)| (k.clone(), json!(truncate_chars(v, 500)))).collect();
                out["attributes"] = Value::Object(attributes);
            }
            Ok(Ok(out))
        })
        .map_err(store_error)?
}

/// Breadth-first over visible entities only (never through invisible ones).
fn neighborhood(
    graph: GraphEnv<'_>,
    vis: &Visibility<'_>,
    input: &GraphQueryInput,
    limit: usize,
) -> Result<Value, ToolOutput> {
    let start = EntityId::parse(input.entity.trim()).map_err(|e| ToolOutput::error(e.to_string()))?;
    let depth = input.depth.min(MAX_DEPTH);
    graph
        .service
        .with_store(|store| {
            let Some((first, fb)) = vis.entity_id(store, &start)? else {
                return Ok(Err(not_visible(start.as_str())));
            };
            let mut entities = vec![vis.describe(store, &first, &fb)?];
            let mut seen: BTreeSet<EntityId> = BTreeSet::from([start.clone()]);
            let mut queue = VecDeque::from([(start.clone(), 0u32)]);
            let mut truncated = false;
            let mut work = 0usize;
            'walk: while let Some((current, d)) = queue.pop_front() {
                if d >= depth {
                    continue;
                }
                let mut rels = store.relations_from(&current)?;
                rels.extend(store.relations_to(&current)?);
                for r in rels {
                    work += 1;
                    if work > 20_000 {
                        truncated = true;
                        break 'walk;
                    }
                    let other = if r.from == current { r.to.clone() } else { r.from.clone() };
                    if seen.contains(&other) {
                        continue;
                    }
                    let Some((o, ob)) = vis.entity_id(store, &other)? else { continue };
                    if o.lifecycle != Lifecycle::Active {
                        continue;
                    }
                    if entities.len() >= limit {
                        truncated = true;
                        break 'walk;
                    }
                    seen.insert(other.clone());
                    entities.push(vis.describe(store, &o, &ob)?);
                    queue.push_back((other, d + 1));
                }
            }
            let relations: Vec<ContextRelation> =
                store.relations_among(&seen)?.iter().take(MAX_RELATIONS).map(|r| vis.relation(r)).collect();
            let formal = mentions_formal(&entities, &relations);
            let mut out = json!({"entities": entities, "relations": relations, "truncated": truncated});
            if formal {
                out["evidenceNote"] = json!(FORMAL_EVIDENCE_NOTE);
            }
            Ok(Ok(out))
        })
        .map_err(store_error)?
}

/// Entities bound in one file (the gate checked the path against the scope).
fn inspect(
    graph: GraphEnv<'_>,
    vis: &Visibility<'_>,
    input: &GraphQueryInput,
    limit: usize,
) -> Result<Value, ToolOutput> {
    let path = WorkspacePath::parse(input.path.trim())
        .map_err(|e| refusal(codes::INVALID_PATH, format!("invalid path {:?}: {e}", input.path)))?;
    if !vis.path(&path) {
        return Err(refusal(
            codes::OUT_OF_SCOPE,
            format!("{path} is outside this task's scope. The application refused the access; work within the scope."),
        ));
    }
    graph
        .service
        .with_store(|store| {
            let resource = store
                .entities_by_key(EntityKind::Resource, path.as_str())?
                .into_iter()
                .find(|e| e.lifecycle != Lifecycle::Tombstoned);
            let resource = match resource {
                Some(r) => match vis.entity(store, &r)? {
                    Some(b) => Some(vis.describe(store, &r, &b)?),
                    None => None,
                },
                None => None,
            };
            let mut entities = Vec::new();
            let mut ids = BTreeSet::new();
            let mut truncated = false;
            for b in store.bindings_at_path(&path)? {
                if ids.contains(&b.entity) {
                    continue;
                }
                let Some((e, eb)) = vis.entity_id(store, &b.entity)? else { continue };
                if e.kind == EntityKind::Resource {
                    continue;
                }
                if entities.len() >= limit {
                    truncated = true;
                    break;
                }
                ids.insert(e.id.clone());
                entities.push(vis.describe(store, &e, &eb)?);
            }
            let coverage = store.coverage(&path)?.map(|c| {
                let mut v = json!({"state": c.state, "unresolved": c.unresolved, "extractor": c.extractor});
                if vis.content()
                    && let Some(detail) = &c.detail
                {
                    v["detail"] = json!(truncate_chars(detail, 500));
                }
                v
            });
            let relations: Vec<ContextRelation> =
                store.relations_among(&ids)?.iter().take(MAX_RELATIONS).map(|r| vis.relation(r)).collect();
            let formal = mentions_formal(&entities, &relations);
            let mut out = json!({
                "path": path,
                "resource": resource,
                "coverage": coverage,
                "entities": entities,
                "relations": relations,
                "truncated": truncated,
            });
            if formal {
                out["evidenceNote"] = json!(FORMAL_EVIDENCE_NOTE);
            }
            Ok(out)
        })
        .map_err(store_error)
}

/// Change impact, keeping only items whose whole path is visible.
fn impact(
    graph: GraphEnv<'_>,
    vis: &Visibility<'_>,
    input: &GraphQueryInput,
    limit: usize,
) -> Result<Value, ToolOutput> {
    let id = EntityId::parse(input.entity.trim()).map_err(|e| ToolOutput::error(e.to_string()))?;
    let root =
        graph.service.with_store(|store| Ok(vis.entity_id(store, &id)?.map(|(e, _)| e.kind))).map_err(store_error)?;
    let Some(root_kind) = root else {
        return Err(not_visible(id.as_str()));
    };
    // The service call takes the store lock itself: never call it inside `with_store`.
    let report = graph
        .service
        .impact(&ImpactRequest { entities: vec![id.clone()], max_depth: Some(input.depth), max_items: Some(1000) })
        .map_err(store_error)?;
    graph
        .service
        .with_store(|store| {
            let mut items = Vec::new();
            let mut truncated = report.truncated || report.work_limit_reached;
            for item in &report.items {
                let mut ok = true;
                for step in &item.path {
                    for end in [&step.from, &step.to] {
                        if vis.entity_id(store, end)?.is_none() {
                            ok = false;
                        }
                    }
                }
                if let Some(file) = &item.located_in
                    && vis.entity_id(store, file)?.is_none()
                {
                    ok = false;
                }
                let Some((e, eb)) = vis.entity_id(store, &item.entity.id)? else { continue };
                if !ok {
                    continue;
                }
                if items.len() >= limit {
                    truncated = true;
                    break;
                }
                let mut v = json!({
                    "entity": vis.describe(store, &e, &eb)?,
                    "depth": item.depth,
                    "action": item.action,
                    "via": item.path.iter().map(|s| json!({"relation": s.relation, "kind": s.kind, "from": s.from, "to": s.to})).collect::<Vec<_>>(),
                });
                if let Some(file) = &item.located_in {
                    v["locatedIn"] = json!(file);
                }
                if vis.content() {
                    v["reason"] = json!(truncate_chars(&item.reason, 300));
                }
                items.push(v);
            }
            // Evidence the change would affect, with its current
            // applicability (only evidence of entities the task may see).
            let mut evidence = Vec::new();
            for hit in report.evidence.iter().take(50) {
                if vis.entity_id(store, &hit.subject)?.is_none() {
                    continue;
                }
                let freshness = match &hit.freshness {
                    Freshness::Current => "current",
                    Freshness::Stale { .. } => "stale",
                    Freshness::Unknown { .. } => "unknown",
                };
                let mut v = json!({
                    "id": hit.evidence,
                    "kind": hit.kind,
                    "subject": hit.subject,
                    "freshness": freshness,
                });
                if vis.content() {
                    v["reason"] = json!(truncate_chars(&hit.reason, 300));
                }
                evidence.push(v);
            }
            let unknown: Vec<&WorkspacePath> = report.unknown_coverage.iter().filter(|p| vis.path(p)).collect();
            let mut out = json!({
                "root": id,
                "items": items,
                "evidence": evidence,
                "unknownCoverage": unknown,
                "truncated": truncated,
            });
            if root_kind == EntityKind::Resource {
                out["scope"] = json!(FILE_IMPACT_NOTE);
            }
            Ok(out)
        })
        .map_err(store_error)
}
