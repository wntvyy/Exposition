//! "Delete from Source" for graph items declared in a source (a theorem or
//! proof environment, a Lean declaration, a BibTeX entry, …), as the source
//! side of a P1 delete request.
//!
//! Such an item exists because its source says so; deleting it from the graph
//! alone would bring it back at the next reindex. Delete therefore removes its
//! text from the source through the document service: ONE unsaved transaction
//! per file in the shared undo history (Undo in the editor brings it back;
//! nothing is written to disk until the person saves; the graph follows on
//! save). The plan locates each item in the file's CURRENT text (an open
//! buffer may have unsaved edits) and lists what still uses it — `\ref`s and
//! citations, its proof (removed with it unless the person keeps it), curated
//! relationships, literal mentions of a Lean name — as one combined decision.
//! Its fingerprint covers the text it was computed from, so any edit between
//! plan and apply makes the plan stale and nothing is removed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, SelectionHint, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::ops::{
    ImpactKind, OpActor, OpChoice, OpChoiceOption, OpConsumer, OpEffect, OpImpact, OpKind, OpOutcome, OpRecovery,
    OpRefusal, OpRequest, OpStatus, OpTarget, SourceEditAt,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_graph::model::{
    AssertionStatus, BindingState, EntityDetail, Lifecycle, Origin, SourceBinding, SourceSelector,
};
use exposition_graph::source_removal::{self as sr, NotLocated};
use exposition_graph::vocabulary::{EntityKind, RelationKind};
use exposition_workspace::WorkspaceService;

use crate::project_ops::DataSide;

/// Choice key: the proofs of removed statements.
pub const CHOICE_PROOFS: &str = "proofs";
/// Lean files read for name mentions (bounded walk).
const MAX_LEAN_FILES: usize = 2000;
/// Consumers named per impact.
const MAX_CONSUMERS: usize = 25;

fn quoted(s: &str) -> String {
    format!("“{s}”")
}

/// One item to remove.
struct Removal {
    target: OpTarget,
    label: String,
    path: WorkspacePath,
    /// What to remove (whole lines when the construct stands alone).
    range: TextRange,
    lines: Option<(usize, usize)>,
    /// Its proofs (path, removal range, label), removed unless kept.
    proofs: Vec<(WorkspacePath, TextRange, String)>,
}

/// The source side's part of a plan.
#[derive(Default)]
pub struct SourcePlan {
    /// Targets this side owns (left out of the data side's request).
    pub handled: Vec<OpTarget>,
    pub effects: Vec<OpEffect>,
    pub refusals: Vec<OpRefusal>,
    pub impacts: Vec<OpImpact>,
    pub choices: Vec<OpChoice>,
    pub fingerprint: Vec<String>,
    removals: Vec<Removal>,
    /// Content hash of each file's text the plan was computed from.
    hashes: BTreeMap<WorkspacePath, String>,
}

/// Current texts: an open document's buffer, else the file as a document would read it.
struct Texts<'a> {
    ws: &'a WorkspaceService,
    cache: HashMap<WorkspacePath, Result<String, String>>,
}

impl<'a> Texts<'a> {
    fn new(ws: &'a WorkspaceService) -> Self {
        Self { ws, cache: HashMap::new() }
    }

    fn get(&mut self, path: &WorkspacePath) -> Result<&str, String> {
        let ws = self.ws;
        let entry = self.cache.entry(path.clone()).or_insert_with(|| {
            match ws.documents().find_by_path(path).and_then(|id| ws.documents().state(&id).ok()) {
                Some(state) => Ok(state.text),
                None => ws.read_text(path).map_err(|e| e.message),
            }
        });
        entry.as_deref().map_err(Clone::clone)
    }
}

/// Declared in a source and deleted through it (mirrors the graph's
/// `ops.sourceDerived` rule; file-backed items, the project, the research
/// space and workflows have their own routes; a tombstoned item is only a
/// record and is deleted as one).
fn source_declared(detail: &EntityDetail) -> bool {
    let e = &detail.entity;
    if e.lifecycle == Lifecycle::Tombstoned
        || e.kind.is_file_backed()
        || matches!(e.kind, EntityKind::Project | EntityKind::ResearchSpace | EntityKind::Workflow)
    {
        return false;
    }
    matches!(e.origin, Origin::Extracted { .. })
        || e.kind == EntityKind::FormalDeclaration
        || detail.bindings.iter().any(|b| matches!(b.selector, SourceSelector::Declaration { .. }))
}

fn declaration_binding(detail: &EntityDetail) -> Result<&SourceBinding, String> {
    let label = quoted(&detail.entity.label);
    let declared: Vec<&SourceBinding> =
        detail.bindings.iter().filter(|b| matches!(b.selector, SourceSelector::Declaration { .. })).collect();
    match declared.as_slice() {
        [b] if b.state == BindingState::Resolved => Ok(b),
        [b] => Err(format!(
            "{label} cannot be located in {} now{}; open the source to remove it.",
            b.path,
            b.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()
        )),
        [] => Err(format!("{label} has no known place in a source; edit the source to remove it.")),
        _ => Err(format!("{label} is declared in more than one place; remove the one you mean in the editor.")),
    }
}

fn lines_text(lines: Option<(usize, usize)>) -> String {
    match lines {
        Some((a, b)) if a == b => format!(", line {a}"),
        Some((a, b)) => format!(", lines {a}–{b}"),
        None => String::new(),
    }
}

/// Locate `binding`'s declaration in the current text and the range to remove.
/// A located item: the range to remove, the construct's lines, and the text's content hash.
type Located = (TextRange, Option<(usize, usize)>, String);
/// Why an item was not located: a refusal code and message.
type NotFound = (&'static str, String);

fn locate(texts: &mut Texts<'_>, label: &str, binding: &SourceBinding) -> Result<Located, NotFound> {
    let path = &binding.path;
    let text = texts.get(path).map_err(|e| ("ops.failed", format!("{path} cannot be read: {e}")))?;
    let declaration = sr::locate(path, text, binding).map_err(|e| match e {
        NotLocated::Changed => (
            "ops.sourceChanged",
            format!(
                "{} has unsaved changes around {label}, so it cannot be located exactly. Save {path} (the graph reads it again) and try again, or remove it in the editor.",
                path
            ),
        ),
        NotLocated::Ambiguous => (
            "ops.sourceChanged",
            format!("{label} is declared more than once in {path}; remove the one you mean in the editor."),
        ),
        NotLocated::Missing => {
            ("ops.notFound", format!("{label} is no longer in {path} (the graph follows when the file is saved)."))
        }
        NotLocated::NotDeclared | NotLocated::NoExtractor => {
            ("ops.sourceDerived", format!("{label} is declared in {path}; edit the source to remove it."))
        }
    })?;
    let range = sr::removal_range(text, declaration.range)
        .ok_or_else(|| ("ops.failed", format!("The text of {label} in {path} could not be measured.")))?;
    Ok((range, sr::lines_of(text, declaration.range), sr::content_hash(text)))
}

/// Relations whose source item breaks when the target is removed.
fn uses_target(kind: RelationKind) -> bool {
    matches!(
        kind,
        RelationKind::References
            | RelationKind::DependsOn
            | RelationKind::Assumes
            | RelationKind::Formalizes
            | RelationKind::EvidenceFor
            | RelationKind::UsesAsset
    )
}

/// Plan the source-declared graph entities of a delete request. Every other
/// target is left to the file and data sides. `deleted` are the project paths
/// the same request deletes (an item in a deleted file needs no edit).
pub fn plan(ws: &WorkspaceService, data: &dyn DataSide, request: &OpRequest, deleted: &[WorkspacePath]) -> SourcePlan {
    let mut out = SourcePlan::default();
    let OpKind::Delete { permanent } = request.kind else { return out };
    let selected: HashSet<&str> = request
        .targets
        .iter()
        .filter_map(|t| match t {
            OpTarget::GraphEntity { entity } => Some(entity.as_str()),
            _ => None,
        })
        .collect();
    let mut texts = Texts::new(ws);
    // Lean declarations whose name mentions are looked up after every removal is known.
    let mut lean: Vec<(usize, String)> = Vec::new();
    let mut proofs_found = false;

    for target in &request.targets {
        let OpTarget::GraphEntity { entity } = target else { continue };
        let Some(detail) = data.entity_detail(entity) else { continue };
        if !source_declared(&detail) {
            continue;
        }
        out.handled.push(target.clone());
        let label = quoted(&detail.entity.label);
        let mut refuse = |code: &str, message: String| {
            out.refusals.push(OpRefusal { target: target.clone(), code: code.into(), message });
        };
        if permanent {
            refuse(
                "ops.notSupported",
                format!(
                    "{label} is removed from its source as an edit you review and save; use Delete (it can be undone in the editor)."
                ),
            );
            continue;
        }
        if let OpActor::Agent { .. } = request.actor {
            refuse(
                "ops.agentScope",
                format!(
                    "{label} is declared in a source; agents change sources with their file tools, as reviewed edits."
                ),
            );
            continue;
        }
        let binding = match declaration_binding(&detail) {
            Ok(b) => b,
            Err(message) => {
                refuse("ops.sourceDerived", message);
                continue;
            }
        };
        let inside = |d: &WorkspacePath| binding.path.as_str().starts_with(&format!("{}/", d.as_str()));
        if deleted.iter().any(|d| binding.path == *d || inside(d)) {
            refuse("ops.covered", format!("Its source {} is deleted in this operation.", binding.path));
            continue;
        }
        let (range, lines, hash) = match locate(&mut texts, &label, binding) {
            Ok(found) => found,
            Err((code, message)) => {
                refuse(code, message);
                continue;
            }
        };
        out.hashes.insert(binding.path.clone(), hash.clone());
        out.fingerprint.push(format!("src:{entity}:{}:{hash}:{}-{}", binding.path, range.start, range.end));

        // Its proofs (removed with it by default) and what else uses it.
        let mut proofs: Vec<(WorkspacePath, TextRange, String)> = Vec::new();
        let mut proof_consumers: Vec<OpConsumer> = Vec::new();
        let mut unlocated_proofs: Vec<String> = Vec::new();
        let mut users: Vec<(OpConsumer, String)> = Vec::new();
        let neighbors: HashMap<&str, &exposition_graph::model::Entity> =
            detail.neighbors.iter().map(|n| (n.id.as_str(), n)).collect();
        for r in &detail.incoming {
            let from = r.from.as_str();
            if r.status == AssertionStatus::Rejected || selected.contains(from) {
                continue;
            }
            let Some(n) = neighbors.get(from).filter(|n| n.lifecycle == Lifecycle::Active) else { continue };
            if r.kind == RelationKind::ProofOf {
                let located = data.entity_detail(from).and_then(|pd| {
                    let b = declaration_binding(&pd).ok()?.clone();
                    let found = locate(&mut texts, &quoted(&n.label), &b).ok()?;
                    Some((b.path, found))
                });
                match located {
                    Some((path, (prange, plines, phash))) => {
                        out.fingerprint.push(format!("srcproof:{from}:{path}:{phash}:{}-{}", prange.start, prange.end));
                        out.hashes.insert(path.clone(), phash);
                        // The label names the file already ("Proof (paper.tex)"); the lines say where.
                        let same_file = path == binding.path;
                        proof_consumers.push(OpConsumer {
                            kind: "entity".into(),
                            id: from.to_owned(),
                            label: format!("{}{}", n.label, lines_text(plines)),
                            path: (!same_file).then(|| path.clone()),
                        });
                        proofs.push((path, prange, n.label.clone()));
                    }
                    None => unlocated_proofs.push(quoted(&n.label)),
                }
            } else if uses_target(r.kind) {
                let consumer =
                    OpConsumer { kind: "entity".into(), id: from.to_owned(), label: n.label.clone(), path: None };
                if !users.iter().any(|(c, _)| c.id == consumer.id) {
                    users.push((consumer, format!("{} {} it", quoted(&n.label), r.kind.reading())));
                }
            }
        }
        if !proofs.is_empty() {
            proofs_found = true;
            out.impacts.push(OpImpact {
                kind: ImpactKind::SourceDependents,
                target: target.clone(),
                consumers: proof_consumers,
                message: format!(
                    "{label} has {} in the source ({}). {} removed with it unless you keep {}.",
                    if proofs.len() == 1 { "a proof" } else { "proofs" },
                    proofs.iter().map(|(_, _, l)| quoted(l)).collect::<Vec<_>>().join(", "),
                    if proofs.len() == 1 { "It is" } else { "They are" },
                    if proofs.len() == 1 { "it" } else { "them" },
                ),
            });
        }
        if !users.is_empty() || !unlocated_proofs.is_empty() {
            let mut message = String::new();
            if !users.is_empty() {
                let shown: Vec<&str> = users.iter().take(5).map(|(_, s)| s.as_str()).collect();
                let more = users.len().saturating_sub(shown.len());
                message.push_str(&format!(
                    "{}{}. After removal {} until you edit {}.",
                    shown.join("; "),
                    if more > 0 { format!("; and {more} more") } else { String::new() },
                    if users.len() == 1 {
                        "that reference shows as undefined"
                    } else {
                        "those references show as undefined"
                    },
                    if users.len() == 1 { "it" } else { "them" },
                ));
            }
            if !unlocated_proofs.is_empty() {
                if !message.is_empty() {
                    message.push(' ');
                }
                message.push_str(&format!(
                    "Its proof {} cannot be located exactly, so it stays in the source.",
                    unlocated_proofs.join(", ")
                ));
            }
            out.impacts.push(OpImpact {
                kind: ImpactKind::SourceDependents,
                target: target.clone(),
                consumers: users.into_iter().take(MAX_CONSUMERS).map(|(c, _)| c).collect(),
                message,
            });
        }
        if let SourceSelector::Declaration { syntax, name: Some(name), .. } = &binding.selector
            && syntax.starts_with("lean.")
        {
            lean.push((out.removals.len(), name.clone()));
        }
        out.effects.push(OpEffect {
            target: target.clone(),
            description: format!(
                "Remove {label} from {}{} as an unsaved edit (review it and save; Undo in the editor brings it back)",
                binding.path,
                lines_text(lines)
            ),
            destination: None,
            recovery: OpRecovery::SourceEdit { edit: None },
        });
        out.removals.push(Removal {
            target: target.clone(),
            label: detail.entity.label.clone(),
            path: binding.path.clone(),
            range,
            lines,
            proofs,
        });
    }

    if !lean.is_empty() {
        lean_mentions(ws, &mut texts, &mut out, &lean);
    }
    if proofs_found {
        out.choices.push(OpChoice {
            key: CHOICE_PROOFS.into(),
            label: "Proofs of the removed statements".into(),
            options: vec![
                OpChoiceOption { value: "remove".into(), label: "Remove the proof too".into() },
                OpChoiceOption { value: "keep".into(), label: "Keep the proof in the source".into() },
            ],
            default: "remove".into(),
        });
    }
    out
}

/// `.lean` files of the project (bounded; tool and build folders skipped; links not followed).
fn lean_files(root: &Path) -> Vec<WorkspacePath> {
    const SKIP: &[&str] = &[".lake", ".git", ".exposition", "node_modules", "target", "build", ".elan"];
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else { continue };
            let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if kind.is_dir() && !SKIP.contains(&name.as_str()) {
                stack.push((entry.path(), child));
            } else if kind.is_file()
                && name.ends_with(".lean")
                && let Ok(p) = WorkspacePath::parse(&child)
            {
                out.push(p);
                if out.len() >= MAX_LEAN_FILES {
                    return out;
                }
            }
        }
    }
    out.sort();
    out
}

/// Literal mentions of removed Lean declarations elsewhere in the project's
/// Lean text (open buffers as they are now), outside every removed range.
fn lean_mentions(ws: &WorkspaceService, texts: &mut Texts<'_>, out: &mut SourcePlan, lean: &[(usize, String)]) {
    let Some(root) = ws.open_root() else { return };
    let mut files: BTreeSet<WorkspacePath> = lean_files(&root).into_iter().collect();
    for info in ws.documents().list() {
        if let Some(p) = info.path.filter(|p| p.as_str().ends_with(".lean")) {
            files.insert(p);
        }
    }
    let mut exclude: HashMap<WorkspacePath, Vec<TextRange>> = HashMap::new();
    for r in &out.removals {
        exclude.entry(r.path.clone()).or_default().push(r.range);
    }
    let removed_names: HashSet<&str> = lean.iter().map(|(_, n)| n.as_str()).collect();
    for (index, name) in lean {
        let removal = &out.removals[*index];
        let mut consumers: Vec<OpConsumer> = Vec::new();
        let mut places: Vec<String> = Vec::new();
        let mut count = 0usize;
        for path in &files {
            let Ok(text) = texts.get(path) else { continue };
            let skip = exclude.get(path).cloned().unwrap_or_default();
            for m in sr::lean_mentions(path, text, name, &skip) {
                if m.declaration.as_deref().is_some_and(|d| removed_names.contains(d)) {
                    continue;
                }
                count += 1;
                let label = m.declaration.clone().unwrap_or_else(|| format!("line {}", m.line));
                let id = format!("{path}:{label}");
                if !consumers.iter().any(|c| c.id == id) && consumers.len() < MAX_CONSUMERS {
                    places.push(format!("{} ({path})", quoted(&label)));
                    consumers.push(OpConsumer { kind: "entity".into(), id, label, path: Some(path.clone()) });
                }
            }
        }
        out.fingerprint.push(format!("srcmention:{}:{count}", removal.target_key()));
        if consumers.is_empty() {
            continue;
        }
        let shown: Vec<&str> = places.iter().take(5).map(String::as_str).collect();
        let more = places.len().saturating_sub(shown.len());
        out.impacts.push(OpImpact {
            kind: ImpactKind::SourceDependents,
            target: removal.target.clone(),
            consumers,
            message: format!(
                "{} is mentioned by name in {}{}. Those uses no longer compile until you edit them.",
                quoted(&removal.label),
                shown.join(", "),
                if more > 0 { format!(" and {more} more") } else { String::new() },
            ),
        });
    }
}

impl Removal {
    fn target_key(&self) -> String {
        match &self.target {
            OpTarget::GraphEntity { entity } => entity.clone(),
            other => format!("{other:?}"),
        }
    }
}

fn outcome(target: &OpTarget, status: OpStatus, code: &str, message: String, recovery: OpRecovery) -> OpOutcome {
    OpOutcome {
        target: target.clone(),
        status,
        code: Some(code.into()),
        message: Some(message),
        created: None,
        recovery,
        restore: None,
    }
}

/// Where `at` (an offset before the edit, at the start of a removed range)
/// lands after removing every range of `merged`.
fn after_removal(merged: &[TextRange], at: u32) -> u32 {
    let before: u32 = merged.iter().filter(|m| m.end <= at).map(TextRange::len).sum();
    let containing = merged.iter().find(|m| m.start <= at && at < m.end.max(m.start + 1));
    containing.map_or(at, |m| m.start).saturating_sub(before)
}

/// One file's applied edit: its document, the revision produced, the ranges removed.
type Edited = (exposition_core::document::DocumentId, u64, Vec<TextRange>);

/// Apply the source side of an approved plan: one unsaved transaction per
/// file. Returns one outcome per planned removal.
pub fn apply(ws: &WorkspaceService, plan: &SourcePlan, choices: &BTreeMap<String, String>) -> Vec<OpOutcome> {
    let keep_proofs = choices.get(CHOICE_PROOFS).is_some_and(|v| v == "keep");
    let mut ranges: BTreeMap<WorkspacePath, Vec<TextRange>> = BTreeMap::new();
    let mut labels: BTreeMap<WorkspacePath, Vec<String>> = BTreeMap::new();
    for r in &plan.removals {
        ranges.entry(r.path.clone()).or_default().push(r.range);
        labels.entry(r.path.clone()).or_default().push(r.label.clone());
        if !keep_proofs {
            for (path, range, _) in &r.proofs {
                ranges.entry(path.clone()).or_default().push(*range);
            }
        }
    }
    // Per file: Ok((document, revision, merged ranges)) or the failure.
    let mut edited: BTreeMap<WorkspacePath, Result<Edited, String>> = BTreeMap::new();
    for (path, list) in ranges {
        let merged = sr::merge(list);
        let result = (|| {
            let opened = ws.open_document(&path).map_err(|e| e.message)?;
            let state = opened.state;
            if plan.hashes.get(&path).is_some_and(|h| *h != sr::content_hash(&state.text)) {
                return Err(format!("{path} changed while it was being edited; nothing was removed from it."));
            }
            let names = labels.get(&path).map(|l| l.as_slice()).unwrap_or_default();
            let label = match names {
                [one] => format!("Delete {} from the source", quoted(one)),
                [] => "Delete proofs from the source".to_owned(),
                many => format!("Delete {} items from the source", many.len()),
            };
            let first = merged.first().map_or(0, |r| r.start);
            let request = TransactionRequest {
                id: TransactionId(format!("tx-src-{}", exposition_workspace::fsutil::now_millis())),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: state.info.id.clone(),
                    base_revision: state.info.revision,
                    patches: merged.iter().map(|r| TextPatch { range: *r, text: String::new() }).collect(),
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: Some(label),
                selection: Some(SelectionHint { before: merged.clone(), after: vec![TextRange::empty(first)] }),
            };
            let applied = ws.documents().apply(request).map_err(|e| e.to_string())?;
            let revision = applied.applied.first().map_or(state.info.revision, |a| a.revision);
            Ok((state.info.id, revision, merged))
        })();
        edited.insert(path, result);
    }

    let mut outcomes = Vec::new();
    for r in &plan.removals {
        match edited.get(&r.path) {
            Some(Ok((document, revision, merged))) => {
                let proofs_note = if r.proofs.is_empty() {
                    String::new()
                } else if keep_proofs {
                    "; its proof stays".to_owned()
                } else {
                    let failed: Vec<&str> = r
                        .proofs
                        .iter()
                        .filter(|(p, _, _)| !matches!(edited.get(p), Some(Ok(_))))
                        .map(|(_, _, l)| l.as_str())
                        .collect();
                    if failed.is_empty() {
                        format!(" with {}", if r.proofs.len() == 1 { "its proof" } else { "its proofs" })
                    } else {
                        format!("; its proof {} could not be removed", failed.join(", "))
                    }
                };
                outcomes.push(outcome(
                    &r.target,
                    OpStatus::Succeeded,
                    "ops.sourceEdited",
                    format!(
                        "Removed from {}{}{proofs_note} as an unsaved edit: review it and save (Undo in the editor brings it back). The graph follows when you save.",
                        r.path,
                        lines_text(r.lines)
                    ),
                    OpRecovery::SourceEdit {
                        edit: Some(SourceEditAt {
                            path: r.path.clone(),
                            document: document.clone(),
                            revision: *revision,
                            at: after_removal(merged, r.range.start),
                        }),
                    },
                ));
            }
            Some(Err(message)) => outcomes.push(outcome(
                &r.target,
                OpStatus::Failed,
                "ops.failed",
                format!("{} was not removed: {message}", quoted(&r.label)),
                OpRecovery::None,
            )),
            None => {}
        }
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_points_map_into_the_edited_text() {
        let merged = vec![TextRange::new(10, 20), TextRange::new(30, 45)];
        assert_eq!(after_removal(&merged, 10), 10);
        assert_eq!(after_removal(&merged, 30), 20);
        assert_eq!(after_removal(&merged, 35), 20, "inside a merged range: its start");
    }
}
