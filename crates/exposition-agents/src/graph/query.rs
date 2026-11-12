//! `graph_query`: bounded, scope- and consent-filtered reads of the project
//! graph (entity search, one entity, a neighborhood, what is in a file, and
//! change impact). A graph query is never a side channel around the task's
//! scope or consent: invisible entities are never returned, never traversed
//! through, and never matched by text the task could not read.

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;

use exposition_core::path::WorkspacePath;
use exposition_graph::{
    EntityId, EntityKind, EvidenceOutcome, Freshness, ImpactRequest, Lifecycle, service::evidence_freshness,
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
            // Evidence about the entity: kind, outcome kind, freshness; the
            // formal statement and axioms only with content consent.
            let mut evidence = Vec::new();
            for record in store.evidence_for_subject(&id)?.iter().take(20) {
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
                evidence.push(item);
            }
            let mut out = json!({
                "entity": described,
                "relations": relations,
                "neighbors": neighbors,
                "evidence": evidence,
                "truncated": truncated,
            });
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
            Ok(Ok(json!({"entities": entities, "relations": relations, "truncated": truncated})))
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
            Ok(json!({
                "path": path,
                "resource": resource,
                "coverage": coverage,
                "entities": entities,
                "relations": relations,
                "truncated": truncated,
            }))
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
    let visible = graph.service.with_store(|store| Ok(vis.entity_id(store, &id)?.is_some())).map_err(store_error)?;
    if !visible {
        return Err(not_visible(id.as_str()));
    }
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
                if vis.content() {
                    v["reason"] = json!(truncate_chars(&item.reason, 300));
                }
                items.push(v);
            }
            let unknown: Vec<&WorkspacePath> = report.unknown_coverage.iter().filter(|p| vis.path(p)).collect();
            Ok(json!({"root": id, "items": items, "unknownCoverage": unknown, "truncated": truncated}))
        })
        .map_err(store_error)
}
