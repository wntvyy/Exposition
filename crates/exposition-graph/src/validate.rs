//! Validation and execution of typed graph operations and service batches.
//!
//! One validator serves people, presets, agents, and services. It runs inside
//! a SQLite transaction: proposals validate in a transaction that is rolled
//! back; application re-validates against the then-current state and commits
//! once, or applies nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use exposition_core::path::WorkspacePath;

use crate::ids::{EntityId, EvidenceId, RelationId};
use crate::model::{
    AssertionStatus, BindingState, Entity, Evidence, GraphDelta, Lifecycle, Origin, Relation, ResourceCoverage,
    SourceBinding, SourceSelector,
};
use crate::ops::{CreatedEntity, EntityRef, GraphDiagnostic, GraphOperation, NewBinding};
use crate::store::{StoreResult, Writer, now_ms};
use crate::vocabulary::{EntityKind, EvidenceKind, RelationKind, ResourceRole};

pub const MAX_OPERATIONS: usize = 500;
pub const MAX_LABEL: usize = 200;
pub const MAX_KEY: usize = 1024;
pub const MAX_NOTE: usize = 500;
pub const MAX_ATTRIBUTES: usize = 32;
pub const MAX_ATTRIBUTE_VALUE: usize = 4096;
/// Ids listed per delta field before `truncated` is set.
const DELTA_LIMIT: usize = 500;

/// The outcome of validating (and optionally executing) operations.
pub struct Run {
    pub diagnostics: Vec<GraphDiagnostic>,
    pub effects: Vec<String>,
    pub conflict: bool,
    pub delta: GraphDelta,
    pub created: Vec<CreatedEntity>,
    /// Something was written that the delta does not list (e.g. only
    /// bindings changed): the transaction must still be committed.
    pub wrote: bool,
}

impl Run {
    pub fn valid(&self) -> bool {
        !self.diagnostics.iter().any(|d| d.severity == crate::ops::DiagnosticSeverity::Error)
    }
}

/// Validate text supplied by a person or agent: bounded, no control characters.
pub fn check_text(value: &str, max: usize, what: &str) -> Result<(), String> {
    if value.len() > max {
        return Err(format!("{what} is longer than {max} bytes"));
    }
    if value.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(format!("{what} contains control characters"));
    }
    Ok(())
}

pub fn check_label(label: &str) -> Result<(), String> {
    if label.trim().is_empty() {
        return Err("the label is empty".into());
    }
    if label.contains('\n') {
        return Err("the label must be one line".into());
    }
    check_text(label, MAX_LABEL, "the label")
}

pub fn check_attributes(attributes: &BTreeMap<String, String>) -> Result<(), String> {
    if attributes.len() > MAX_ATTRIBUTES {
        return Err(format!("at most {MAX_ATTRIBUTES} attributes"));
    }
    for (k, v) in attributes {
        let key_ok = !k.is_empty()
            && k.len() <= 64
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
        if !key_ok {
            return Err(format!("invalid attribute name {:?}", crate::store::truncate(k, 40)));
        }
        check_text(v, MAX_ATTRIBUTE_VALUE, "an attribute value")?;
    }
    Ok(())
}

fn temp_id_ok(t: &str) -> bool {
    !t.is_empty() && t.len() <= 32 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// May this origin create entities of `kind` / add relations of `kind`
/// through operations? Services and presets use batches instead.
fn manual_origin(origin: &Origin) -> bool {
    matches!(origin, Origin::User | Origin::Agent { .. })
}

/// Status a new relation gets from its origin.
pub fn initial_status(origin: &Origin) -> AssertionStatus {
    match origin {
        Origin::User | Origin::Preset { .. } => AssertionStatus::Accepted,
        Origin::Agent { .. } => AssertionStatus::Suggested,
        Origin::Extracted { .. } | Origin::Service { .. } => AssertionStatus::Observed,
    }
}

struct Ctx<'w, 'a> {
    w: &'w Writer<'a>,
    origin: Origin,
    apply: bool,
    diagnostics: Vec<GraphDiagnostic>,
    effects: Vec<String>,
    conflict: bool,
    temps: HashMap<String, Entity>,
    delta: GraphDelta,
    created: Vec<CreatedEntity>,
    /// Relations added in this request (kind, from, to) to catch duplicates.
    pending_relations: BTreeSet<(RelationKind, EntityId, EntityId)>,
    /// `contains` targets added in this request.
    pending_contained: BTreeSet<EntityId>,
}

impl Ctx<'_, '_> {
    fn err(&mut self, op: usize, code: &str, message: impl Into<String>) {
        self.diagnostics.push(GraphDiagnostic::error(Some(op), code, message));
    }

    fn resolve(&mut self, op: usize, r: &EntityRef) -> StoreResult<Option<Entity>> {
        match r {
            EntityRef::Temp { temp_id } => {
                let found = self.temps.get(temp_id).cloned();
                if found.is_none() {
                    self.err(
                        op,
                        "graph.unknownTemp",
                        format!("no entity with temporary id {temp_id:?} was created earlier in this change"),
                    );
                }
                Ok(found)
            }
            EntityRef::Id { id } => match self.w.entity(id)? {
                Some(e) if e.lifecycle == Lifecycle::Tombstoned => {
                    self.err(op, "graph.tombstoned", format!("{} no longer exists in the project", e.label));
                    Ok(None)
                }
                Some(e) => Ok(Some(e)),
                None => {
                    self.conflict = true;
                    self.err(op, "graph.unknownEntity", format!("entity {id} does not exist"));
                    Ok(None)
                }
            },
        }
    }

    fn mark_entity(&mut self, list: fn(&mut GraphDelta) -> &mut Vec<EntityId>, id: &EntityId) {
        let v = list(&mut self.delta);
        if v.len() < DELTA_LIMIT {
            if !v.contains(id) {
                v.push(id.clone());
            }
        } else {
            self.delta.truncated = true;
        }
    }

    fn mark_relation(&mut self, list: fn(&mut GraphDelta) -> &mut Vec<RelationId>, id: &RelationId) {
        let v = list(&mut self.delta);
        if v.len() < DELTA_LIMIT {
            if !v.contains(id) {
                v.push(id.clone());
            }
        } else {
            self.delta.truncated = true;
        }
    }
}

fn new_binding(entity: &EntityId, b: &NewBinding) -> SourceBinding {
    SourceBinding {
        entity: entity.clone(),
        path: b.path.clone(),
        selector: b.selector.clone(),
        range: b.range,
        version: None,
        fingerprint: None,
        // Resolved against the source by reconciliation; never assumed.
        state: BindingState::Unresolved,
        detail: Some("Waiting for the source to be indexed.".into()),
    }
}

fn check_binding(b: &NewBinding) -> Result<(), String> {
    if let Some(r) = b.range
        && !r.is_ordered()
    {
        return Err("the source range is inverted".into());
    }
    if matches!(b.selector, SourceSelector::Region) && b.range.is_none() {
        return Err("a region binding needs a range".into());
    }
    if let SourceSelector::Declaration { syntax, name, .. } = &b.selector {
        check_text(syntax, 64, "the declaration syntax")?;
        if let Some(n) = name {
            check_text(n, MAX_KEY, "the declaration name")?;
        }
    }
    Ok(())
}

/// Validate (and with `apply`, execute) operations inside `w`'s transaction.
pub fn run_operations(w: &Writer<'_>, origin: &Origin, ops: &[GraphOperation], apply: bool) -> StoreResult<Run> {
    let mut cx = Ctx {
        w,
        origin: origin.clone(),
        apply,
        diagnostics: Vec::new(),
        effects: Vec::new(),
        conflict: false,
        temps: HashMap::new(),
        delta: GraphDelta::default(),
        created: Vec::new(),
        pending_relations: BTreeSet::new(),
        pending_contained: BTreeSet::new(),
    };
    if ops.is_empty() {
        cx.diagnostics.push(GraphDiagnostic::error(None, "graph.empty", "the change contains no operations"));
    }
    if ops.len() > MAX_OPERATIONS {
        cx.diagnostics.push(GraphDiagnostic::error(
            None,
            "graph.tooLarge",
            format!("a change can contain at most {MAX_OPERATIONS} operations"),
        ));
        return Ok(finish(cx));
    }
    let now = now_ms();
    for (i, op) in ops.iter().enumerate() {
        match op {
            GraphOperation::CreateEntity { temp_id, kind, roles, label, key, attributes, binding } => {
                if !temp_id_ok(temp_id) {
                    cx.err(i, "graph.invalidTemp", "temporary ids are 1–32 letters, digits, '-' or '_'");
                    continue;
                }
                if cx.temps.contains_key(temp_id) {
                    cx.err(i, "graph.duplicateTemp", format!("temporary id {temp_id:?} is used twice"));
                    continue;
                }
                if manual_origin(&cx.origin) && !kind.is_creatable() {
                    cx.err(
                        i,
                        "graph.notCreatable",
                        format!(
                            "{} entities come from the project's files and indexing; they cannot be created by hand",
                            kind.title()
                        ),
                    );
                    continue;
                }
                if let Err(m) = check_label(label) {
                    cx.err(i, "graph.invalidLabel", m);
                    continue;
                }
                if let Some(k) = key
                    && let Err(m) = check_text(k, MAX_KEY, "the key")
                {
                    cx.err(i, "graph.invalidKey", m);
                    continue;
                }
                if let Err(m) = check_attributes(attributes) {
                    cx.err(i, "graph.invalidAttributes", m);
                    continue;
                }
                if *kind != EntityKind::Resource && !roles.is_empty() {
                    cx.err(i, "graph.invalidRoles", "only resources have roles");
                    continue;
                }
                if let Some(b) = binding
                    && let Err(m) = check_binding(b)
                {
                    cx.err(i, "graph.invalidBinding", m);
                    continue;
                }
                let entity = Entity {
                    id: EntityId::mint(),
                    kind: *kind,
                    roles: dedup_roles(roles),
                    label: label.trim().to_owned(),
                    key: key.clone(),
                    lifecycle: Lifecycle::Active,
                    tombstone_reason: None,
                    revision: 1,
                    origin: cx.origin.clone(),
                    attributes: attributes.clone(),
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                cx.effects.push(format!("Create {} “{}”", kind.title().to_lowercase(), entity.label));
                if apply {
                    w.put_entity(&entity)?;
                    if let Some(b) = binding {
                        w.set_bindings(&entity.id, &[new_binding(&entity.id, b)])?;
                    }
                    cx.created.push(CreatedEntity { temp_id: temp_id.clone(), id: entity.id.clone() });
                    let id = entity.id.clone();
                    cx.mark_entity(|d| &mut d.entities_added, &id);
                }
                cx.temps.insert(temp_id.clone(), entity);
            }
            GraphOperation::UpdateEntity { entity, expected_revision, label, attributes } => {
                let Some(mut e) = cx.resolve(i, &EntityRef::Id { id: entity.clone() })? else { continue };
                if e.revision != *expected_revision {
                    cx.conflict = true;
                    cx.err(
                        i,
                        "graph.staleEntity",
                        format!(
                            "{} changed since it was read (revision {} → {})",
                            e.label, expected_revision, e.revision
                        ),
                    );
                    continue;
                }
                if let Some(l) = label {
                    if let Err(m) = check_label(l) {
                        cx.err(i, "graph.invalidLabel", m);
                        continue;
                    }
                    if e.kind.is_file_backed() {
                        cx.err(i, "graph.fileBacked", "a file's label is its name; rename the file instead");
                        continue;
                    }
                }
                if let Some(a) = attributes
                    && let Err(m) = check_attributes(a)
                {
                    cx.err(i, "graph.invalidAttributes", m);
                    continue;
                }
                // The label and attributes of an item declared in a source
                // (or admitted by the Lean service) say what the source says.
                // Indexing writes them: an edit here would misstate the
                // source until the next index silently undid it.
                if (label.is_some() || attributes.is_some()) && source_stated(&e) {
                    cx.err(
                        i,
                        "graph.sourceDerived",
                        format!(
                            "the name and attributes of “{}” come from its source; edit the source to change them",
                            e.label
                        ),
                    );
                    continue;
                }
                if let Some(l) = label {
                    e.label = l.trim().to_owned();
                }
                if let Some(a) = attributes {
                    e.attributes = a.clone();
                }
                e.revision += 1;
                e.updated_at_ms = now;
                cx.effects.push(format!("Update {} “{}”", e.kind.title().to_lowercase(), e.label));
                if apply {
                    w.put_entity(&e)?;
                    cx.mark_entity(|d| &mut d.entities_updated, &e.id);
                }
            }
            GraphOperation::SetLifecycle { entity, expected_revision, lifecycle } => {
                let Some(mut e) = cx.resolve(i, &EntityRef::Id { id: entity.clone() })? else { continue };
                if *lifecycle == Lifecycle::Tombstoned {
                    cx.err(i, "graph.lifecycle", "only indexing marks entities as no longer existing");
                    continue;
                }
                if e.kind == EntityKind::Project {
                    cx.err(i, "graph.lifecycle", "the project entity cannot be archived");
                    continue;
                }
                if e.revision != *expected_revision {
                    cx.conflict = true;
                    cx.err(i, "graph.staleEntity", format!("{} changed since it was read", e.label));
                    continue;
                }
                e.lifecycle = *lifecycle;
                e.revision += 1;
                e.updated_at_ms = now;
                cx.effects.push(format!("Mark {} “{}” {}", e.kind.title().to_lowercase(), e.label, lifecycle.as_str()));
                if apply {
                    w.put_entity(&e)?;
                    cx.mark_entity(|d| &mut d.entities_updated, &e.id);
                }
            }
            GraphOperation::AddRelation { kind, from, to, evidence_kind, evidence, note } => {
                let from_e = cx.resolve(i, from)?;
                let to_e = cx.resolve(i, to)?;
                let (Some(from_e), Some(to_e)) = (from_e, to_e) else { continue };
                add_relation(&mut cx, i, *kind, &from_e, &to_e, *evidence_kind, evidence, note.as_deref(), None, now)?;
            }
            GraphOperation::SetRelationStatus { relation, status } => {
                let Some(mut r) = w.relation(relation)? else {
                    cx.conflict = true;
                    cx.err(i, "graph.unknownRelation", format!("relation {relation} does not exist"));
                    continue;
                };
                if cx.origin != Origin::User {
                    cx.err(i, "graph.reviewOnly", "only a person can accept or reject a relationship");
                    continue;
                }
                if !matches!(status, AssertionStatus::Accepted | AssertionStatus::Rejected) {
                    cx.err(i, "graph.invalidStatus", "a relationship can only be accepted or rejected");
                    continue;
                }
                if r.status == AssertionStatus::Observed {
                    cx.err(
                        i,
                        "graph.observed",
                        "this relationship was observed in the sources; change the source instead",
                    );
                    continue;
                }
                // Accepting (or confirming again) a formal link records the
                // statement the person reviewed it against; nothing else
                // ever sets or clears that record.
                let confirming = r.status == AssertionStatus::Accepted && *status == AssertionStatus::Accepted;
                let statement = if r.kind == RelationKind::Formalizes && *status == AssertionStatus::Accepted {
                    Some(statement_of(w, &r.to)?)
                } else {
                    None
                };
                // Confirming a formal link records the statement as it is
                // now. With no statement to record it would change nothing,
                // so it is refused with what to do, never shown as done.
                if confirming && let Some(crate::review::StatementNow::Unlocated { why, rebindable }) = &statement {
                    cx.err(
                        i,
                        "graph.statementNotLocated",
                        format!("Nothing was confirmed. {}", crate::review::unlocated_reason(why, *rebindable)),
                    );
                    continue;
                }
                r.status = *status;
                // Accepting a suggestion whose statement cannot be located
                // records none: the link then reads as needing re-review.
                r.reviewed_statement = statement.and_then(|s| s.digest());
                r.revision += 1;
                r.updated_at_ms = now;
                cx.effects.push(format!(
                    "{} relationship {}",
                    match (status, confirming) {
                        (AssertionStatus::Accepted, true) => "Confirm",
                        (AssertionStatus::Accepted, false) => "Accept",
                        _ => "Reject",
                    },
                    r.kind.as_str()
                ));
                if apply {
                    w.put_relation(&r)?;
                    cx.mark_relation(|d| &mut d.relations_updated, &r.id);
                }
            }
            GraphOperation::RemoveRelation { relation } => {
                let Some(r) = w.relation(relation)? else {
                    cx.conflict = true;
                    cx.err(i, "graph.unknownRelation", format!("relation {relation} does not exist"));
                    continue;
                };
                if r.status == AssertionStatus::Observed {
                    cx.err(i, "graph.observed", "this relationship was observed in the sources; change the source (or reject a suggestion) instead");
                    continue;
                }
                if let Origin::Agent { task_id } = &cx.origin {
                    let own = matches!(&r.origin, Origin::Agent { task_id: t } if t == task_id);
                    if !(own && r.status == AssertionStatus::Suggested) {
                        cx.err(i, "graph.reviewOnly", "an agent can only withdraw its own suggestions");
                        continue;
                    }
                }
                cx.effects.push(format!("Remove relationship {}", r.kind.as_str()));
                if apply {
                    w.delete_relation(&r.id)?;
                    cx.mark_relation(|d| &mut d.relations_removed, &r.id);
                }
            }
            GraphOperation::BindSource { entity, binding } => {
                let Some(e) = cx.resolve(i, entity)? else { continue };
                if e.kind.is_file_backed() || e.kind == EntityKind::Project {
                    cx.err(i, "graph.fileBacked", "files and folders are bound by their path");
                    continue;
                }
                if let Err(m) = check_binding(binding) {
                    cx.err(i, "graph.invalidBinding", m);
                    continue;
                }
                cx.effects.push(format!("Bind “{}” to {}", e.label, binding.path));
                if apply {
                    w.set_bindings(&e.id, &[new_binding(&e.id, binding)])?;
                    if !cx.temps.values().any(|t| t.id == e.id) {
                        cx.mark_entity(|d| &mut d.entities_updated, &e.id);
                    }
                }
            }
        }
    }
    Ok(finish(cx))
}

fn finish(cx: Ctx<'_, '_>) -> Run {
    Run {
        diagnostics: cx.diagnostics,
        effects: cx.effects,
        conflict: cx.conflict,
        delta: cx.delta,
        created: cx.created,
        wrote: false,
    }
}

/// Whether an entity's label and attributes are written from its source:
/// everything an extractor admitted, and formal declarations (admitted by
/// the extractor or by the Lean service from an audit).
fn source_stated(e: &Entity) -> bool {
    matches!(e.origin, Origin::Extracted { .. }) || e.kind == EntityKind::FormalDeclaration
}

fn dedup_roles(roles: &[ResourceRole]) -> Vec<ResourceRole> {
    let mut v = roles.to_vec();
    v.sort();
    v.dedup();
    v
}

/// The digest of the statement a `formalizes` link is reviewed against, as
/// it is located now (`None` for other kinds, and when the statement cannot
/// be located: the review then covers an unknown text and reads as such).
fn reviewed_statement(w: &Writer<'_>, kind: RelationKind, to: &EntityId) -> StoreResult<Option<String>> {
    if kind != RelationKind::Formalizes {
        return Ok(None);
    }
    Ok(statement_of(w, to)?.digest())
}

/// The statement `to` stands for, as it is located now.
fn statement_of(w: &Writer<'_>, to: &EntityId) -> StoreResult<crate::review::StatementNow> {
    Ok(match w.entity(to)? {
        Some(entity) => crate::review::statement_now(&entity, &w.bindings(to)?),
        None => crate::review::StatementNow::Unlocated {
            why: "It is no longer in the project graph.".into(),
            rebindable: false,
        },
    })
}

/// Content hash of the first binding (endpoint version at assertion time).
fn version_of(w: &Writer<'_>, id: &EntityId) -> StoreResult<Option<String>> {
    let hash: Option<String> =
        w.tx.query_row(
            "SELECT content_hash FROM bindings WHERE entity_id = ?1 ORDER BY ordinal LIMIT 1",
            [id.as_str()],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    Ok(hash)
}

#[allow(clippy::too_many_arguments)]
fn add_relation(
    cx: &mut Ctx<'_, '_>,
    i: usize,
    kind: RelationKind,
    from: &Entity,
    to: &Entity,
    evidence_kind: Option<EvidenceKind>,
    evidence: &[EvidenceId],
    note: Option<&str>,
    status_override: Option<AssertionStatus>,
    now: u64,
) -> StoreResult<()> {
    if manual_origin(&cx.origin) && !kind.is_manual() {
        cx.err(
            i,
            "graph.notManual",
            format!("“{}” relationships follow the project's files and are maintained by indexing", kind.reading()),
        );
        return Ok(());
    }
    if from.id == to.id {
        cx.err(i, "graph.selfRelation", "an entity cannot relate to itself");
        return Ok(());
    }
    if !kind.allows(from.kind, to.kind) {
        cx.err(
            i,
            "graph.endpointRule",
            format!(
                "{} “{}” cannot {} {} “{}”. {}",
                from.kind.title(),
                from.label,
                kind.reading(),
                to.kind.title().to_lowercase(),
                to.label,
                kind.rule()
            ),
        );
        return Ok(());
    }
    if let Some(n) = note
        && let Err(m) = check_text(n, MAX_NOTE, "the note")
    {
        cx.err(i, "graph.invalidNote", m);
        return Ok(());
    }
    // Evidence: only `evidenceFor` carries a kind and cites records; a cited
    // record must be of that kind, and a formal check is cited only from its
    // own declaration (`citation_refusal`), so a relation can never relabel
    // a record or state a Lean verdict for another item. Privileged kinds
    // can only cite records their service registered.
    match (kind, evidence_kind) {
        (RelationKind::EvidenceFor, None) => {
            cx.err(i, "graph.evidenceKind", "an evidence relationship needs the kind of support it gives");
            return Ok(());
        }
        (RelationKind::EvidenceFor, Some(ek)) => {
            if ek.is_service_owned() && manual_origin(&cx.origin) && evidence.is_empty() {
                cx.err(
                    i,
                    "graph.serviceOwnedEvidence",
                    format!("{} evidence is recorded only by its service; cite an existing record", ek.as_str()),
                );
                return Ok(());
            }
        }
        (_, Some(_)) => {
            cx.err(i, "graph.evidenceKind", "only evidence relationships carry an evidence kind");
            return Ok(());
        }
        (_, None) => {
            if !evidence.is_empty() {
                cx.err(
                    i,
                    "graph.evidenceNotCarried",
                    format!(
                        "only “{}” relationships cite evidence records; “{}” cannot carry one",
                        RelationKind::EvidenceFor.reading(),
                        kind.reading()
                    ),
                );
                return Ok(());
            }
        }
    }
    if evidence.len() > 64 {
        cx.err(i, "graph.tooLarge", "at most 64 evidence records per relationship");
        return Ok(());
    }
    if let Some(ek) = evidence_kind {
        for id in evidence {
            let Some((record_kind, subject)) = cx.w.evidence_head(id)? else {
                cx.err(i, "graph.unknownEvidence", format!("evidence {id} does not exist"));
                return Ok(());
            };
            if let Some((code, why)) = citation_refusal(ek, &from.id, record_kind, subject.as_ref()) {
                cx.err(i, code, format!("evidence {id} {why}"));
                return Ok(());
            }
        }
    }
    let key = (kind, from.id.clone(), to.id.clone());
    if cx.pending_relations.contains(&key) {
        cx.err(i, "graph.duplicateRelation", "this relationship is added twice");
        return Ok(());
    }
    let existing = cx.w.find_relation(kind, &from.id, &to.id)?;
    if let Some(ex) = &existing
        && ex.status != AssertionStatus::Rejected
    {
        cx.err(
            i,
            "graph.duplicateRelation",
            format!("“{}” already {} “{}” ({})", from.label, kind.reading(), to.label, ex.status.as_str()),
        );
        return Ok(());
    }
    // A rejected relationship is a person's decision, kept so that it is not
    // proposed again: an agent cannot replace it. A person (or a preset or
    // service acting for one) can assert it after all, which continues the
    // same record.
    if existing.is_some() && matches!(cx.origin, Origin::Agent { .. }) {
        cx.err(
            i,
            "graph.rejected",
            format!(
                "the user rejected “{}” {} “{}”; only the user can add it again",
                from.label,
                kind.reading(),
                to.label
            ),
        );
        return Ok(());
    }
    if kind == RelationKind::Contains {
        if cx.pending_contained.contains(&to.id) || cx.w.container_of(&to.id)?.is_some() {
            cx.err(i, "graph.alreadyContained", format!("“{}” already has a container", to.label));
            return Ok(());
        }
        if creates_containment_cycle(cx.w, &from.id, &to.id)? {
            cx.err(i, "graph.cycle", "containment cannot form a cycle");
            return Ok(());
        }
        cx.pending_contained.insert(to.id.clone());
    }
    cx.pending_relations.insert(key);
    let status = status_override.unwrap_or_else(|| initial_status(&cx.origin));
    cx.effects.push(format!(
        "{}: “{}” {} “{}”",
        if status == AssertionStatus::Suggested { "Suggest" } else { "Add" },
        from.label,
        kind.reading(),
        to.label
    ));
    if cx.apply {
        // Asserting a rejected relationship again continues its record: the
        // revision only moves forward and the creation time is kept.
        let (revision, created_at_ms) =
            existing.as_ref().map(|e| (e.revision.saturating_add(1), e.created_at_ms)).unwrap_or((1, now));
        let relation = Relation {
            id: existing.map(|e| e.id).unwrap_or_else(RelationId::mint),
            kind,
            from: from.id.clone(),
            to: to.id.clone(),
            origin: cx.origin.clone(),
            status,
            evidence_kind,
            evidence: evidence.to_vec(),
            from_version: version_of(cx.w, &from.id)?,
            to_version: version_of(cx.w, &to.id)?,
            note: note.map(str::to_owned),
            // A link a person asserts directly is reviewed as asserted.
            reviewed_statement: if status == AssertionStatus::Accepted && cx.origin == Origin::User {
                reviewed_statement(cx.w, kind, &to.id)?
            } else {
                None
            },
            review: None,
            revision,
            created_at_ms,
            updated_at_ms: now,
        };
        cx.w.put_relation(&relation)?;
        cx.mark_relation(|d| &mut d.relations_added, &relation.id);
    }
    Ok(())
}

/// Why an `evidenceFor` relation of `evidence_kind` from `from` may not cite
/// a record of `record_kind` about `subject` (`None`: it may). One rule for
/// the validator and for reading stored relations:
/// - the record is of the relation's kind (a formal check is never cited as
///   a review, a review never as a computation);
/// - a formal check is cited only from the declaration it was recorded for:
///   Lean evidence is on the declaration, and no relation states it for
///   another item. Other kinds may be cited from an item that reports them
///   (a finding citing the computational record of a retained result).
pub fn citation_refusal(
    evidence_kind: EvidenceKind,
    from: &EntityId,
    record_kind: Option<EvidenceKind>,
    subject: Option<&EntityId>,
) -> Option<(&'static str, String)> {
    if record_kind != Some(evidence_kind) {
        return Some(("graph.evidenceKind", format!("is not {} evidence", evidence_kind.as_str())));
    }
    if evidence_kind == EvidenceKind::FormalCheck && subject != Some(from) {
        return Some((
            "graph.evidenceSubject",
            "was recorded for another item; formal-check evidence is cited only from its own declaration".into(),
        ));
    }
    None
}

/// Whether a stored relation's citation of `record` counts when evidence is
/// read: rows written before the rule above (or restored, or imported with
/// the graph) are kept as they are and simply not read as support.
pub fn cites(relation: &Relation, record: &Evidence) -> bool {
    relation.kind == RelationKind::EvidenceFor
        && relation.status != AssertionStatus::Rejected
        && relation.evidence.contains(&record.id)
        && relation.evidence_kind.is_some_and(|ek| {
            citation_refusal(ek, &relation.from, Some(record.kind), record.subject.as_ref()).is_none()
        })
}

fn creates_containment_cycle(w: &Writer<'_>, parent: &EntityId, child: &EntityId) -> StoreResult<bool> {
    // Walk up from `parent`; reaching `child` means child is an ancestor.
    let mut current = parent.clone();
    for _ in 0..256 {
        if &current == child {
            return Ok(true);
        }
        let up: Option<String> =
            w.tx.query_row(
                "SELECT from_id FROM relations WHERE kind = 'contains' AND to_id = ?1 LIMIT 1",
                [current.as_str()],
                |r| r.get(0),
            )
            .ok();
        match up.and_then(|u| EntityId::parse(&u).ok()) {
            Some(next) => current = next,
            None => return Ok(false),
        }
    }
    Ok(true)
}

// ----- service batches -----

/// An entity a service admits or updates. With `id`, the existing entity is
/// updated (kind must match); without, a new identity is minted.
#[derive(Clone, Debug)]
pub struct EntityUpsert {
    pub id: Option<EntityId>,
    pub kind: EntityKind,
    pub roles: Vec<ResourceRole>,
    pub label: String,
    pub key: Option<String>,
    pub attributes: BTreeMap<String, String>,
    /// `None` keeps the current lifecycle (Active for new entities).
    pub lifecycle: Option<Lifecycle>,
    pub tombstone_reason: Option<String>,
    /// `Some` replaces all bindings (their `entity` field is ignored).
    pub bindings: Option<Vec<SourceBinding>>,
}

/// A relation a service observes (or a preset asserts).
#[derive(Clone, Debug)]
pub struct RelationUpsert {
    pub kind: RelationKind,
    /// Index into the batch's `entities` or an existing id.
    pub from: BatchRef,
    pub to: BatchRef,
    pub status: Option<AssertionStatus>,
    pub evidence_kind: Option<EvidenceKind>,
    pub evidence: Vec<EvidenceId>,
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub enum BatchRef {
    Existing(EntityId),
    /// Position in `ServiceBatch::entities`.
    Batch(usize),
}

/// Internal write path for services (indexing, research, Lean, attachment,
/// blueprint materialization). Not reachable from IPC or agent tools.
#[derive(Clone, Debug, Default)]
pub struct ServiceBatch {
    pub entities: Vec<EntityUpsert>,
    pub relations: Vec<RelationUpsert>,
    pub remove_relations: Vec<RelationId>,
    pub evidence: Vec<Evidence>,
    pub coverage: Vec<ResourceCoverage>,
    pub remove_coverage: Vec<WorkspacePath>,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct BatchOutcome {
    pub revision: u64,
    pub delta: GraphDelta,
    /// Ids of `entities`, in order.
    pub entity_ids: Vec<EntityId>,
    pub relation_ids: Vec<RelationId>,
}

/// Apply a service batch (validated against the same vocabulary). Returns the
/// run and the ids of relations added, in batch order (duplicates skipped).
pub fn run_batch(w: &Writer<'_>, origin: &Origin, batch: &ServiceBatch) -> Result<(Run, Vec<RelationId>), String> {
    let run = (|| -> StoreResult<Result<(Run, Vec<RelationId>), String>> {
        let now = now_ms();
        let mut cx = Ctx {
            w,
            origin: origin.clone(),
            apply: true,
            diagnostics: Vec::new(),
            effects: Vec::new(),
            conflict: false,
            temps: HashMap::new(),
            delta: GraphDelta::default(),
            created: Vec::new(),
            pending_relations: BTreeSet::new(),
            pending_contained: BTreeSet::new(),
        };
        let mut ids: Vec<Entity> = Vec::with_capacity(batch.entities.len());
        let mut wrote_bindings = false;
        for (i, up) in batch.entities.iter().enumerate() {
            if let Err(m) = check_label(&up.label) {
                return Ok(Err(format!("entity {i}: {m}")));
            }
            if let Err(m) = check_attributes(&up.attributes) {
                return Ok(Err(format!("entity {i}: {m}")));
            }
            if up.kind != EntityKind::Resource && !up.roles.is_empty() {
                return Ok(Err(format!("entity {i}: only resources have roles")));
            }
            // One identity per file: a new resource whose key (workspace
            // path) already has a live resource updates that resource.
            let target = match (&up.id, up.kind, &up.key) {
                (None, EntityKind::Resource, Some(key)) => w.live_resource(key)?.map(|e| e.id),
                (id, _, _) => id.clone(),
            };
            let entity = match &target {
                Some(id) => {
                    let Some(mut e) = w.entity(id)? else { return Ok(Err(format!("entity {id} does not exist"))) };
                    if e.kind != up.kind {
                        return Ok(Err(format!("entity {id} is a {}, not a {}", e.kind.as_str(), up.kind.as_str())));
                    }
                    let lifecycle = up.lifecycle.unwrap_or(e.lifecycle);
                    let changed = e.label != up.label
                        || e.roles != dedup_roles(&up.roles)
                        || e.key != up.key
                        || e.attributes != up.attributes
                        || e.lifecycle != lifecycle
                        || e.tombstone_reason != up.tombstone_reason;
                    if changed {
                        e.label = up.label.clone();
                        e.roles = dedup_roles(&up.roles);
                        e.key = up.key.clone();
                        e.attributes = up.attributes.clone();
                        e.lifecycle = lifecycle;
                        e.tombstone_reason =
                            if lifecycle == Lifecycle::Tombstoned { up.tombstone_reason.clone() } else { None };
                        e.revision += 1;
                        e.updated_at_ms = now;
                        w.put_entity(&e)?;
                        if lifecycle == Lifecycle::Tombstoned {
                            cx.mark_entity(|d| &mut d.entities_removed, &e.id);
                        } else {
                            cx.mark_entity(|d| &mut d.entities_updated, &e.id);
                        }
                    }
                    e
                }
                None => {
                    let e = Entity {
                        id: EntityId::mint(),
                        kind: up.kind,
                        roles: dedup_roles(&up.roles),
                        label: up.label.clone(),
                        key: up.key.clone(),
                        lifecycle: up.lifecycle.unwrap_or(Lifecycle::Active),
                        tombstone_reason: up.tombstone_reason.clone(),
                        revision: 1,
                        origin: origin.clone(),
                        attributes: up.attributes.clone(),
                        created_at_ms: now,
                        updated_at_ms: now,
                    };
                    w.put_entity(&e)?;
                    cx.mark_entity(|d| &mut d.entities_added, &e.id);
                    e
                }
            };
            if let Some(bindings) = &up.bindings {
                let bindings: Vec<SourceBinding> =
                    bindings.iter().map(|b| SourceBinding { entity: entity.id.clone(), ..b.clone() }).collect();
                if w.bindings(&entity.id)? != bindings {
                    w.set_bindings(&entity.id, &bindings)?;
                    // A binding change (state, range, version) is graph state
                    // that freshness consumers must see.
                    if !cx.delta.entities_added.contains(&entity.id) && !cx.delta.entities_removed.contains(&entity.id)
                    {
                        cx.mark_entity(|d| &mut d.entities_updated, &entity.id);
                    }
                    wrote_bindings = true;
                }
            }
            ids.push(entity);
        }
        for e in &batch.evidence {
            w.put_evidence(e)?;
        }
        // Removals first, so a batch can replace a relation (e.g. move a
        // resource to another container) in one transaction.
        for id in &batch.remove_relations {
            if w.delete_relation(id)? {
                cx.mark_relation(|d| &mut d.relations_removed, id);
            }
        }
        let mut relation_ids = Vec::new();
        for (i, r) in batch.relations.iter().enumerate() {
            let get = |br: &BatchRef| -> StoreResult<Option<Entity>> {
                Ok(match br {
                    BatchRef::Batch(n) => ids.get(*n).cloned(),
                    BatchRef::Existing(id) => w.entity(id)?,
                })
            };
            let (Some(from), Some(to)) = (get(&r.from)?, get(&r.to)?) else {
                return Ok(Err(format!("relation {i}: unknown endpoint")));
            };
            let before = cx.diagnostics.len();
            let added_before = cx.delta.relations_added.len();
            add_relation(
                &mut cx,
                i,
                r.kind,
                &from,
                &to,
                r.evidence_kind,
                &r.evidence,
                r.note.as_deref(),
                r.status,
                now,
            )?;
            if cx.diagnostics.len() > before {
                // Duplicates are not errors for services (idempotent observation).
                let d = cx.diagnostics.pop().unwrap_or_else(|| GraphDiagnostic::error(None, "", ""));
                if d.code != "graph.duplicateRelation" {
                    return Ok(Err(format!("relation {i}: {}", d.message)));
                }
            }
            if cx.delta.relations_added.len() > added_before
                && let Some(id) = cx.delta.relations_added.last()
            {
                relation_ids.push(id.clone());
            }
        }
        for c in &batch.coverage {
            w.set_coverage(c)?;
        }
        for p in &batch.remove_coverage {
            w.remove_coverage(p)?;
        }
        cx.created = ids.iter().map(|e| CreatedEntity { temp_id: String::new(), id: e.id.clone() }).collect();
        let mut run = finish(cx);
        run.wrote = wrote_bindings;
        Ok(Ok((run, relation_ids)))
    })();
    match run {
        Ok(r) => r,
        Err(e) => Err(e.to_string()),
    }
}
