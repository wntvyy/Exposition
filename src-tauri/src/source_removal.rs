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
//! Those reads are bounded: when one of them did not see everything (the
//! graph's read of the item's relationships, the Lean files searched), the
//! plan says that its list of uses is incomplete, as one more consequence in
//! the same decision. The removal stays possible because it is an unsaved
//! edit the person reviews before saving; the decision never reads as "used
//! by nothing" when that was not established.
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
    AssertionStatus, BindingState, CoverageState, EntityDetail, Lifecycle, Origin, SourceBinding, SourceSelector,
    SyncState,
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
/// Relationships to an item the graph's detail read returns at most.
const MAX_RELATIONS_READ: usize = exposition_graph::store::MAX_ROWS;
/// Files that could refer to an item checked against what the graph has read
/// (bounded walk).
const MAX_SOURCE_FILES: usize = 2000;

/// What the graph has read of the project's files.
#[derive(Clone, Debug, Default)]
pub struct Indexing {
    /// Every file the graph knows, with its extraction state.
    pub files: BTreeMap<WorkspacePath, CoverageState>,
    /// No pass has completed since the project was opened: files changed
    /// while it was closed may not have been read yet.
    pub first_read: bool,
}

/// What `graph` has read of the project's files (`None`: no graph).
pub fn indexing_of(graph: &exposition_graph::GraphService) -> Option<Indexing> {
    let files = graph.with_store(|s| Ok(s.all_coverage()?)).ok()?;
    let freshness = graph.freshness();
    Some(Indexing {
        files: files.into_iter().map(|c| (c.path, c.state)).collect(),
        first_read: matches!(freshness.state, SyncState::Pending | SyncState::Reconciling)
            && freshness.checkpoint_ms.is_none(),
    })
}

/// The kinds of file (extensions) whose text can refer to an item declared
/// with `syntax`: a LaTeX item from LaTeX, a Typst item from Typst, a BibTeX
/// entry from the files that cite (LaTeX, Typst, Markdown), a Markdown item
/// from Markdown. Never code or Lean (a Lean declaration's uses are searched
/// in the Lean files themselves).
fn referring_extensions(syntax: &str) -> &'static [&'static str] {
    match syntax.split('.').next() {
        Some("latex") => &["tex", "ltx"],
        Some("typst") => &["typ"],
        Some("bibtex") => &["tex", "ltx", "typ", "md", "markdown"],
        Some("markdown") => &["md", "markdown"],
        _ => &[],
    }
}

fn has_extension(path: &WorkspacePath, extensions: &[&str]) -> bool {
    path.extension().is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn quoted(s: &str) -> String {
    format!("“{s}”")
}

/// One item to remove.
struct Removal {
    target: OpTarget,
    label: String,
    path: WorkspacePath,
    /// What to remove (whole lines when the construct stands alone; a
    /// Typst label that follows the construct goes with it), sorted.
    ranges: Vec<TextRange>,
    lines: Option<(usize, usize)>,
    /// Its proofs (path, removal ranges, label), removed unless kept.
    proofs: Vec<(WorkspacePath, Vec<TextRange>, String)>,
    /// Why the uses listed for it may not be all of them (empty: the reads
    /// were complete).
    unseen: Vec<String>,
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
/// A located item: the ranges to remove, the item's lines, and the text's content hash.
type Located = (Vec<TextRange>, Option<(usize, usize)>, String);
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
    let (ranges, lines) = sr::removal_ranges(text, &declaration)
        .ok_or_else(|| ("ops.failed", format!("The text of {label} in {path} could not be measured.")))?;
    Ok((ranges, lines, sr::content_hash(text)))
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
    // What the graph has read (read once, when needed), and the project's
    // files that could refer to an item, by kind (walked once per kind).
    let mut indexing: Option<Option<Indexing>> = None;
    let mut walks: HashMap<&'static [&'static str], FileWalk> = HashMap::new();
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
        let (ranges, lines, hash) = match locate(&mut texts, &label, binding) {
            Ok(found) => found,
            Err((code, message)) => {
                refuse(code, message);
                continue;
            }
        };
        out.hashes.insert(binding.path.clone(), hash.clone());
        out.fingerprint.push(format!("src:{entity}:{}:{hash}:{}", binding.path, spans(&ranges)));

        // Its proofs (removed with it by default) and what else uses it.
        let mut proofs: Vec<(WorkspacePath, Vec<TextRange>, String)> = Vec::new();
        let mut proof_consumers: Vec<OpConsumer> = Vec::new();
        let mut unlocated_proofs: Vec<String> = Vec::new();
        let mut users: Vec<(OpConsumer, String)> = Vec::new();
        let neighbors: HashMap<&str, &exposition_graph::model::Entity> =
            detail.neighbors.iter().map(|n| (n.id.as_str(), n)).collect();
        // What the read of the relationships TO it may have left out: more
        // of them than it reads (it stopped at its bound), or an item that
        // uses it beyond the neighbors it lists. Items it uses itself
        // (outgoing) never use it.
        let mut unread_users = detail.relations_truncated && detail.incoming.len() >= MAX_RELATIONS_READ;
        for r in &detail.incoming {
            let from = r.from.as_str();
            if r.status == AssertionStatus::Rejected || selected.contains(from) {
                continue;
            }
            let Some(n) = neighbors.get(from) else {
                unread_users |= detail.neighbors_truncated && (r.kind == RelationKind::ProofOf || uses_target(r.kind));
                continue;
            };
            if n.lifecycle != Lifecycle::Active {
                continue;
            }
            if r.kind == RelationKind::ProofOf {
                let located = data.entity_detail(from).and_then(|pd| {
                    let b = declaration_binding(&pd).ok()?.clone();
                    let found = locate(&mut texts, &quoted(&n.label), &b).ok()?;
                    Some((b.path, found))
                });
                match located {
                    Some((path, (pranges, plines, phash))) => {
                        out.fingerprint.push(format!("srcproof:{from}:{path}:{phash}:{}", spans(&pranges)));
                        out.hashes.insert(path.clone(), phash);
                        // The label names the file already ("Proof (paper.tex)"); the lines say where.
                        let same_file = path == binding.path;
                        proof_consumers.push(OpConsumer {
                            kind: "entity".into(),
                            id: from.to_owned(),
                            label: format!("{}{}", n.label, lines_text(plines)),
                            path: (!same_file).then(|| path.clone()),
                        });
                        proofs.push((path, pranges, n.label.clone()));
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
        let mut unseen = Vec::new();
        if unread_users {
            unseen.push("it has more relationships than this plan reads".to_owned());
        }
        // Its uses come from what the graph has read of the files that can
        // refer to it; a Lean declaration's are searched in the Lean files
        // themselves (below).
        let extensions = match &binding.selector {
            SourceSelector::Declaration { syntax, .. } => referring_extensions(syntax),
            _ => &[],
        };
        if !extensions.is_empty()
            && let Some(read) = indexing.get_or_insert_with(|| data.indexing()).as_ref()
        {
            let walk = walks.entry(extensions).or_insert_with(|| match ws.open_root() {
                Some(root) => source_files(&root, extensions, MAX_SOURCE_FILES),
                None => FileWalk::default(),
            });
            unseen.extend(unread_files(read, extensions, walk));
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
            ranges,
            lines,
            proofs,
            unseen,
        });
    }

    if !lean.is_empty() {
        lean_mentions(ws, &mut texts, &mut out, &lean);
    }
    for removal in &out.removals {
        if removal.unseen.is_empty() {
            continue;
        }
        out.impacts.push(OpImpact {
            kind: ImpactKind::SourceDependents,
            target: removal.target.clone(),
            consumers: Vec::new(),
            message: format!(
                "Not every use of {} could be checked: {}. Uses not listed here may break too; check the sources before you save.",
                quoted(&removal.label),
                removal.unseen.join("; ")
            ),
        });
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

/// The project's files of some kind as one bounded walk found them, and
/// what the walk could not see.
#[derive(Debug, Default)]
struct FileWalk {
    files: Vec<WorkspacePath>,
    /// More such files exist than `files` lists (the walk stopped).
    capped: bool,
    /// Folders that could not be listed in full (`""`: the project folder).
    unlisted: Vec<String>,
}

/// `N folders could not be listed (…)`, naming the project folder as such.
fn unlisted_reason(unlisted: &[String]) -> String {
    let folders: Vec<String> =
        unlisted.iter().map(|f| if f.is_empty() { "the project folder".to_owned() } else { f.clone() }).collect();
    format!("{} could not be listed ({})", plural(folders.len(), "folder", "folders"), some_of(&folders))
}

/// The project's files with one of `extensions` that the graph's scan
/// admits (the same folders skipped and depth bound, links not followed),
/// at most `limit`.
fn source_files(root: &Path, extensions: &[&str], limit: usize) -> FileWalk {
    use exposition_graph::admission::{Admission, classify, scan_directory};
    let mut walk = FileWalk::default();
    let mut stack: Vec<(PathBuf, Option<WorkspacePath>, usize)> = vec![(root.to_path_buf(), None, 0)];
    while let Some((dir, rel, depth)) = stack.pop() {
        let shown = rel.as_ref().map(|r| r.as_str().to_owned()).unwrap_or_default();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            walk.unlisted.push(shown);
            continue;
        };
        let mut listed = true;
        for entry in entries {
            let Ok(entry) = entry else {
                listed = false;
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                listed = false;
                continue;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let path = match &rel {
                Some(d) => d.join(&name),
                None => WorkspacePath::parse(&name),
            };
            let Ok(path) = path else { continue };
            if kind.is_dir() {
                if depth < exposition_graph::reconcile::scan::MAX_DEPTH && scan_directory(&path) {
                    stack.push((entry.path(), Some(path), depth + 1));
                }
            } else if kind.is_file()
                && has_extension(&path, extensions)
                && matches!(classify(&path), Admission::Admit(_))
            {
                if walk.files.len() >= limit {
                    walk.capped = true;
                    return walk;
                }
                walk.files.push(path);
            }
        }
        if !listed {
            walk.unlisted.push(shown);
        }
    }
    walk.files.sort();
    walk
}

/// Why the graph's relations may not hold every use of an item whose uses
/// are written in files with one of `extensions`: such files it has not read
/// yet (pending, or on disk and unknown to it), could not read (failed), or
/// may not have read since the project was opened; and what the walk of
/// those files could not see.
fn unread_files(read: &Indexing, extensions: &[&str], walk: &FileWalk) -> Vec<String> {
    let mut out = Vec::new();
    if read.first_read {
        out.push("the project's files are still being read".to_owned());
    }
    let state_of = |state: CoverageState| -> BTreeSet<String> {
        read.files
            .iter()
            .filter(|(p, s)| **s == state && has_extension(p, extensions))
            .map(|(p, _)| p.as_str().to_owned())
            .collect()
    };
    let mut not_read = state_of(CoverageState::Pending);
    not_read.extend(walk.files.iter().filter(|p| !read.files.contains_key(*p)).map(|p| p.as_str().to_owned()));
    if !not_read.is_empty() {
        let names: Vec<String> = not_read.into_iter().collect();
        out.push(format!("{} not indexed yet ({})", plural_is(names.len(), "file", "files"), some_of(&names)));
    }
    let failed: Vec<String> = state_of(CoverageState::Failed).into_iter().collect();
    if !failed.is_empty() {
        out.push(format!("{} could not be indexed ({})", plural(failed.len(), "file", "files"), some_of(&failed)));
    }
    if walk.capped {
        out.push(format!("only the first {MAX_SOURCE_FILES} files that could refer to it were checked"));
    }
    if !walk.unlisted.is_empty() {
        out.push(unlisted_reason(&walk.unlisted));
    }
    out
}

/// `.lean` files of the project (at most `limit`; tool and build folders
/// skipped; links not followed).
fn lean_files(root: &Path, limit: usize) -> FileWalk {
    const SKIP: &[&str] = &[".lake", ".git", ".exposition", "node_modules", "target", "build", ".elan"];
    let mut walk = FileWalk::default();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            walk.unlisted.push(rel);
            continue;
        };
        let mut listed = true;
        for entry in entries {
            let Ok(entry) = entry else {
                listed = false;
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                listed = false;
                continue;
            };
            let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if kind.is_dir() && !SKIP.contains(&name.as_str()) {
                stack.push((entry.path(), child));
            } else if kind.is_file()
                && name.ends_with(".lean")
                && let Ok(p) = WorkspacePath::parse(&child)
            {
                if walk.files.len() >= limit {
                    walk.capped = true;
                    return walk;
                }
                walk.files.push(p);
            }
        }
        if !listed {
            walk.unlisted.push(rel);
        }
    }
    walk.files.sort();
    walk
}

/// `a, b and 2 more`: at most three names, then how many others.
fn some_of(names: &[String]) -> String {
    let shown: Vec<&str> = names.iter().take(3).map(String::as_str).collect();
    match names.len().saturating_sub(shown.len()) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `1 file is` / `2 files are`.
fn plural_is(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one} is") } else { format!("{n} {many} are") }
}

/// Literal mentions of removed Lean declarations elsewhere in the project's
/// Lean text (open buffers as they are now), outside every removed range.
fn lean_mentions(ws: &WorkspaceService, texts: &mut Texts<'_>, out: &mut SourcePlan, lean: &[(usize, String)]) {
    let Some(root) = ws.open_root() else { return };
    let walk = lean_files(&root, MAX_LEAN_FILES);
    let mut files: BTreeSet<WorkspacePath> = walk.files.iter().cloned().collect();
    for info in ws.documents().list() {
        if let Some(p) = info.path.filter(|p| p.as_str().ends_with(".lean")) {
            files.insert(p);
        }
    }
    let mut exclude: HashMap<WorkspacePath, Vec<TextRange>> = HashMap::new();
    for r in &out.removals {
        exclude.entry(r.path.clone()).or_default().extend(r.ranges.iter().copied());
    }
    // What the search could not see applies to every name looked for.
    let mut unseen: Vec<String> = Vec::new();
    if walk.capped {
        unseen.push(format!("only the first {MAX_LEAN_FILES} Lean files of the project were searched"));
    }
    if !walk.unlisted.is_empty() {
        unseen.push(unlisted_reason(&walk.unlisted));
    }
    let unreadable: Vec<String> =
        files.iter().filter(|p| texts.get(p).is_err()).map(|p| p.as_str().to_owned()).collect();
    if !unreadable.is_empty() {
        unseen.push(format!(
            "{} could not be read ({})",
            plural(unreadable.len(), "Lean file", "Lean files"),
            some_of(&unreadable)
        ));
    }
    let removed_names: HashSet<&str> = lean.iter().map(|(_, n)| n.as_str()).collect();
    for (index, name) in lean {
        let mut consumers: Vec<OpConsumer> = Vec::new();
        let mut places: Vec<String> = Vec::new();
        // Every distinct place, also those past the ones the decision names.
        let mut seen: HashSet<String> = HashSet::new();
        let mut count = 0usize;
        let mut crowded: Vec<String> = Vec::new();
        for path in &files {
            let Ok(text) = texts.get(path) else { continue };
            let skip = exclude.get(path).cloned().unwrap_or_default();
            let (mentions, more) = sr::lean_mentions_bounded(path, text, name, &skip);
            if more {
                crowded.push(path.as_str().to_owned());
            }
            for m in mentions {
                if m.declaration.as_deref().is_some_and(|d| removed_names.contains(d)) {
                    continue;
                }
                count += 1;
                let label = m.declaration.clone().unwrap_or_else(|| format!("line {}", m.line));
                let id = format!("{path}:{label}");
                if seen.insert(id.clone()) && consumers.len() < MAX_CONSUMERS {
                    places.push(format!("{} ({path})", quoted(&label)));
                    consumers.push(OpConsumer { kind: "entity".into(), id, label, path: Some(path.clone()) });
                }
            }
        }
        let removal = &mut out.removals[*index];
        removal.unseen.extend(unseen.iter().cloned());
        if !crowded.is_empty() {
            removal.unseen.push(format!(
                "only the first {} mentions per file were read in {}",
                sr::MAX_MENTIONS,
                some_of(&crowded)
            ));
        }
        let removal = &out.removals[*index];
        out.fingerprint.push(format!("srcmention:{}:{count}", removal.target_key()));
        if consumers.is_empty() {
            continue;
        }
        let shown: Vec<&str> = places.iter().take(5).map(String::as_str).collect();
        let more = seen.len().saturating_sub(shown.len());
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

/// `a-b,c-d`: the removal ranges, for a plan's fingerprint.
fn spans(ranges: &[TextRange]) -> String {
    ranges.iter().map(|r| format!("{}-{}", r.start, r.end)).collect::<Vec<_>>().join(",")
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
        ranges.entry(r.path.clone()).or_default().extend(r.ranges.iter().copied());
        labels.entry(r.path.clone()).or_default().push(r.label.clone());
        if !keep_proofs {
            for (path, proof, _) in &r.proofs {
                ranges.entry(path.clone()).or_default().extend(proof.iter().copied());
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
                            at: after_removal(merged, r.ranges.first().map_or(0, |x| x.start)),
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
    fn the_lean_walk_says_when_it_stopped_at_its_bound() {
        let root = std::env::temp_dir().join(format!("exposition-lean-walk-{}", std::process::id()));
        std::fs::create_dir_all(root.join("a/.lake")).unwrap();
        for name in ["A.lean", "a/B.lean", "a/C.lean", "a/.lake/D.lean", "a/notes.md"] {
            std::fs::write(root.join(name), "theorem t : True := trivial\n").unwrap();
        }
        let exact = lean_files(&root, 3);
        let capped = lean_files(&root, 2);
        let _ = std::fs::remove_dir_all(&root);
        let names: Vec<&str> = exact.files.iter().map(|p| p.as_str()).collect();
        assert_eq!(names, ["A.lean", "a/B.lean", "a/C.lean"], "tool folders are skipped");
        assert!(!exact.capped && exact.unlisted.is_empty(), "{exact:?}");
        assert_eq!((capped.files.len(), capped.capped), (2, true), "{capped:?}");
    }

    #[test]
    fn removal_points_map_into_the_edited_text() {
        let merged = vec![TextRange::new(10, 20), TextRange::new(30, 45)];
        assert_eq!(after_removal(&merged, 10), 10);
        assert_eq!(after_removal(&merged, 30), 20);
        assert_eq!(after_removal(&merged, 35), 20, "inside a merged range: its start");
    }
}
