//! Project operations on files and folders (P1): delete (OS trash or
//! permanent), duplicate, paste (copy into a folder) and restore from the
//! trash, for paths of the open project and — as paste sources only —
//! external files the user explicitly pasted.
//!
//! Planning normalizes the selection (aliases resolve to one entry, entries
//! inside a selected folder are covered by it), refuses what file operations
//! may never touch (outside the project, the project's application data,
//! read-only entries, a folder into itself, links that cannot be copied
//! safely) and lists concrete consequences: unsaved editor contents, lost
//! recovery for permanent deletion, and copies of a source with unsaved edits
//! (their SAVED bytes are copied). Applying acts on a fresh plan (the caller
//! re-plans and compares hashes) and reports one outcome per target; partial
//! success is normal and never hidden. Nothing here falls back from the trash
//! to permanent removal, overwrites or merges an existing entry, or follows a
//! link out of the project.

pub mod fsops;
pub mod names;
pub mod policy;
pub mod trash;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use exposition_core::document::DocumentInfo;
use exposition_core::ops::{
    ImpactKind, OpActor, OpChoice, OpChoiceOption, OpConsumer, OpDestination, OpEffect, OpImpact, OpKind, OpOutcome,
    OpRecovery, OpRefusal, OpStatus, OpTarget,
};
use exposition_core::path::WorkspacePath;

use crate::root::{WorkspaceRoot, display_path};
use crate::service::WorkspaceService;
use fsops::{EntryKind, PublishError};
pub use trash::{OsTrash, TrashBin, TrashedItem};

/// Choice key for open documents with unsaved changes in deleted paths.
pub const CHOICE_UNSAVED: &str = "unsavedBuffers";
pub const UNSAVED_PRESERVE: &str = "preserve";
pub const UNSAVED_DISCARD: &str = "discard";
pub const UNSAVED_CANCEL: &str = "cancel";

#[derive(Clone, Debug)]
enum Source {
    Project(WorkspacePath),
    External(PathBuf),
}

#[derive(Clone, Debug)]
struct Item {
    /// Position in the request (outcomes keep the request's order).
    order: usize,
    /// As requested (outcomes are reported against it).
    target: OpTarget,
    source: Source,
    /// The entry itself: canonical parent folder + its name on disk.
    abs: PathBuf,
    kind: EntryKind,
    readonly: bool,
    name: String,
    /// Workspace parent folder (`None`: the project root), for duplicates.
    parent: Option<WorkspacePath>,
    parent_abs: PathBuf,
    planned: Option<WorkspacePath>,
    /// Open documents with unsaved changes at or under the entry.
    dirty: Vec<DocumentInfo>,
    /// Open documents without unsaved changes at or under the entry.
    clean: Vec<DocumentInfo>,
    walk: Option<fsops::Walk>,
}

impl Item {
    fn shown(&self) -> String {
        match &self.source {
            Source::Project(path) => path.to_string(),
            Source::External(path) => display_path(path),
        }
    }

    fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

#[derive(Clone, Debug)]
struct Dest {
    folder: Option<WorkspacePath>,
    abs: PathBuf,
}

/// The file side of an operation plan.
#[derive(Clone, Debug, Default)]
pub struct PathPlan {
    pub effects: Vec<OpEffect>,
    pub refusals: Vec<OpRefusal>,
    pub impacts: Vec<OpImpact>,
    pub choices: Vec<OpChoice>,
    /// Everything the decision rests on (entries, sizes, times, open buffers).
    pub fingerprint: Vec<String>,
    /// Project paths this plan deletes (normalized), for the data side.
    pub deleted: Vec<WorkspacePath>,
    /// Project paths this plan copies from (normalized).
    pub copied: Vec<WorkspacePath>,
    /// Selected targets inside another selected folder: (target, covering target).
    pub covered: Vec<(OpTarget, OpTarget)>,
    kind: Option<OpKind>,
    items: Vec<Item>,
    dest: Option<Dest>,
}

impl PathPlan {
    /// Folders copies are staged in (to clean up after an interruption).
    pub fn staging_folders(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = match (&self.kind, &self.dest) {
            (Some(OpKind::Paste { .. }), Some(dest)) => vec![dest.abs.clone()],
            (Some(OpKind::Duplicate), _) => self.items.iter().map(|i| i.parent_abs.clone()).collect(),
            _ => Vec::new(),
        };
        out.sort();
        out.dedup();
        out
    }
}

/// One applied target and, for a trashed entry the app can find again, the
/// trash item (the caller turns it into a restore token).
#[derive(Clone, Debug)]
pub struct PathOutcome {
    pub outcome: OpOutcome,
    pub trashed: Option<TrashedItem>,
    /// The project path the outcome concerns (normalized), when it is one.
    pub path: Option<WorkspacePath>,
    /// A target inside another selected folder: it shares that folder's
    /// outcome and was not acted on separately.
    pub covered: bool,
}

pub struct ApplyOptions<'a> {
    pub choices: &'a BTreeMap<String, String>,
    pub trash: &'a dyn TrashBin,
    /// Targets the data side could not prepare: skipped with that refusal.
    pub skip: &'a [OpRefusal],
    /// Short operation tag used in staging names.
    pub tag: &'a str,
}

fn refusal(target: &OpTarget, code: &str, message: impl Into<String>) -> OpRefusal {
    OpRefusal { target: target.clone(), code: code.to_owned(), message: message.into() }
}

fn outcome(target: &OpTarget, status: OpStatus, code: Option<&str>, message: Option<String>) -> OpOutcome {
    OpOutcome {
        target: target.clone(),
        status,
        code: code.map(str::to_owned),
        message,
        created: None,
        recovery: OpRecovery::None,
        restore: None,
    }
}

fn io_code(error: &io::Error) -> &'static str {
    // ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION.
    if cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33)) {
        return "ops.inUse";
    }
    match error.kind() {
        io::ErrorKind::NotFound => "ops.notFound",
        io::ErrorKind::PermissionDenied => "ops.permissionDenied",
        _ => "ops.failed",
    }
}

fn size_text(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / 1_048_576.0),
        b => format!("{:.1} GB", b as f64 / 1_073_741_824.0),
    }
}

fn folder_summary(walk: &fsops::Walk) -> String {
    let files = if walk.files == 1 { "1 file".to_owned() } else { format!("{} files", walk.files) };
    if walk.truncated { format!("{files} or more") } else { format!("{files}, {}", size_text(walk.bytes)) }
}

fn list_some(items: &[String], max: usize) -> String {
    let shown: Vec<&str> = items.iter().take(max).map(String::as_str).collect();
    if items.len() > max { format!("{} and {} more", shown.join(", "), items.len() - max) } else { shown.join(", ") }
}

/// Is `path` equal to `ancestor` or inside it (component-wise, case-insensitive on Windows)?
fn within(path: &Path, ancestor: &Path) -> bool {
    let key = |p: &Path| -> Vec<String> {
        Path::new(&display_path(p))
            .components()
            .map(|c| {
                let part = c.as_os_str().to_string_lossy().into_owned();
                if cfg!(windows) { part.to_lowercase() } else { part }
            })
            .collect()
    };
    let (p, a) = (key(path), key(ancestor));
    a.len() <= p.len() && p[..a.len()] == a[..]
}

/// The entry's name as stored on disk (case and long name), without following it.
fn real_name(parent: &Path, name: &str, kind: EntryKind) -> String {
    if kind != EntryKind::Link
        && let Ok(canonical) = std::fs::canonicalize(parent.join(name))
        && let Some(real) = canonical.file_name().and_then(|n| n.to_str())
    {
        return real.to_owned();
    }
    if let Ok(read) = std::fs::read_dir(parent) {
        let mut fallback = None;
        for entry in read.flatten() {
            let Some(entry_name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if entry_name == name {
                return entry_name;
            }
            if cfg!(windows) && entry_name.eq_ignore_ascii_case(name) {
                fallback = Some(entry_name);
            }
        }
        if let Some(found) = fallback {
            return found;
        }
    }
    name.to_owned()
}

struct Located {
    path: WorkspacePath,
    abs: PathBuf,
    parent: Option<WorkspacePath>,
    parent_abs: PathBuf,
    info: fsops::EntryInfo,
}

/// Find a project entry without following its final component; the folders
/// above it are resolved (links inside the project allowed) and must stay
/// inside the project.
fn locate(root: &WorkspaceRoot, path: &WorkspacePath) -> Result<Located, (&'static str, String)> {
    let (parent, parent_abs) = match path.parent() {
        None => (None, root.path().to_path_buf()),
        Some(parent) => {
            let abs = root.resolve_existing(&parent).map_err(|e| match e.code {
                "path.outsideWorkspace" => ("ops.outsideProject", format!("{path} resolves outside the project.")),
                "path.notFound" | "path.brokenLink" => ("ops.notFound", format!("{path} does not exist.")),
                _ => ("ops.failed", e.message),
            })?;
            if !abs.is_dir() {
                return Err(("ops.notFound", format!("{path} does not exist.")));
            }
            (root.relativize(&abs), abs)
        }
    };
    let joined = parent_abs.join(path.file_name());
    let info = fsops::inspect(&joined).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => ("ops.notFound", format!("{path} does not exist.")),
        _ => (io_code(&e), format!("{path} cannot be read: {e}.")),
    })?;
    let name = real_name(&parent_abs, path.file_name(), info.kind);
    let normalized = names::join(parent.as_ref(), &name)
        .map_err(|_| ("ops.notSupported", format!("{path} has a name Exposition cannot handle.")))?;
    let abs = parent_abs.join(&name);
    // A folder or file reached through its real name resolves inside the
    // project (the parent already does; this catches nothing new for
    // ordinary entries but keeps the rule explicit).
    if info.kind != EntryKind::Link && !root.contains(&std::fs::canonicalize(&abs).unwrap_or_else(|_| abs.clone())) {
        return Err(("ops.outsideProject", format!("{path} resolves outside the project.")));
    }
    Ok(Located { path: normalized, abs, parent, parent_abs, info })
}

impl WorkspaceService {
    /// Plan the file part of an operation (only `Path` and `External`
    /// targets are considered; others belong to the data side).
    pub fn plan_paths(&self, kind: &OpKind, targets: &[OpTarget], actor: &OpActor) -> PathPlan {
        let mut plan = PathPlan { kind: Some(kind.clone()), ..PathPlan::default() };
        let root = match self.root() {
            Ok(root) => root,
            Err(_) => {
                for t in targets.iter().filter(|t| matches!(t, OpTarget::Path { .. } | OpTarget::External { .. })) {
                    plan.refusals.push(refusal(t, "ops.noProject", "No project is open."));
                }
                return plan;
            }
        };
        plan.fingerprint.push(format!("root:{}", root.display_path()));
        let is_paste = matches!(kind, OpKind::Paste { .. });

        // Destination of a paste.
        if let OpKind::Paste { destination } = kind {
            match self.paste_destination(&root, destination) {
                Ok(dest) => {
                    plan.fingerprint.push(format!("dest:{}", display_path(&dest.abs)));
                    plan.dest = Some(dest);
                }
                Err((code, message)) => {
                    for t in targets.iter().filter(|t| matches!(t, OpTarget::Path { .. } | OpTarget::External { .. })) {
                        plan.refusals.push(refusal(t, code, message.clone()));
                    }
                    return plan;
                }
            }
        }

        // Locate every target (a target listed twice counts once).
        let mut items: Vec<Item> = Vec::new();
        let mut seen: HashSet<&OpTarget> = HashSet::new();
        for (order, target) in targets.iter().enumerate() {
            if !seen.insert(target) {
                continue;
            }
            let located = match target {
                OpTarget::Path { path } => locate(&root, path).map(|l| (Source::Project(l.path.clone()), l)),
                OpTarget::External { path } => {
                    if !is_paste {
                        plan.refusals.push(refusal(
                            target,
                            "ops.notSupported",
                            "Files outside the project can only be pasted into it.",
                        ));
                        continue;
                    }
                    if !matches!(actor, OpActor::User) {
                        plan.refusals.push(refusal(
                            target,
                            "ops.agentScope",
                            "Agents cannot import files from outside the project.",
                        ));
                        continue;
                    }
                    self.locate_external(&root, path)
                }
                _ => continue,
            };
            let (source, located) = match located {
                Ok(found) => found,
                Err((code, message)) => {
                    plan.refusals.push(refusal(target, code, message));
                    continue;
                }
            };
            let info = &located.info;
            plan.fingerprint.push(format!(
                "entry:{}|{:?}|{}|{:?}|{}",
                display_path(&located.abs),
                info.kind,
                info.len,
                info.modified_ms,
                info.readonly
            ));
            items.push(Item {
                order,
                target: target.clone(),
                source,
                kind: info.kind,
                readonly: info.readonly,
                name: located.path.file_name().to_owned(),
                abs: located.abs,
                parent: located.parent,
                parent_abs: located.parent_abs,
                planned: None,
                dirty: Vec::new(),
                clean: Vec::new(),
                walk: None,
            });
        }

        // Normalize overlapping selections: an entry inside (or equal to) a
        // selected folder is covered by it.
        items.sort_by_key(|a| display_path(&a.abs).to_lowercase());
        let mut kept: Vec<Item> = Vec::new();
        for item in items {
            if let Some(cover) = kept.iter().find(|k| (k.is_dir() || k.abs == item.abs) && within(&item.abs, &k.abs)) {
                plan.covered.push((item.target.clone(), cover.target.clone()));
                continue;
            }
            kept.push(item);
        }
        kept.sort_by_key(|i| i.order);

        let mut planned_names: HashSet<String> = HashSet::new();
        for mut item in kept {
            match kind {
                OpKind::Delete { permanent } => {
                    if self.plan_delete(&mut plan, &mut item, *permanent) {
                        plan.items.push(item);
                    }
                }
                OpKind::Duplicate | OpKind::Paste { .. } => {
                    if self.plan_copy(&mut plan, &mut item, &mut planned_names) {
                        plan.items.push(item);
                    }
                }
            }
        }
        // A target inside a refused folder shares its refusal.
        let covered = std::mem::take(&mut plan.covered);
        for (target, cover) in covered {
            match plan.refusals.iter().find(|r| r.target == cover).cloned() {
                Some(r) => plan.refusals.push(refusal(&target, &r.code, r.message)),
                None => plan.covered.push((target, cover)),
            }
        }
        if plan.impacts.iter().any(|i| i.kind == ImpactKind::UnsavedBuffer) {
            plan.choices.push(OpChoice {
                key: CHOICE_UNSAVED.into(),
                label: "Unsaved changes in the editor".into(),
                options: vec![
                    OpChoiceOption {
                        value: UNSAVED_PRESERVE.into(),
                        label: "Keep the unsaved text open in the editor (it is not saved to the deleted file)".into(),
                    },
                    OpChoiceOption { value: UNSAVED_DISCARD.into(), label: "Discard the unsaved changes".into() },
                    OpChoiceOption {
                        value: UNSAVED_CANCEL.into(),
                        label: "Don't delete files that have unsaved changes".into(),
                    },
                ],
                default: UNSAVED_PRESERVE.into(),
            });
        }
        plan
    }

    fn paste_destination(
        &self,
        root: &WorkspaceRoot,
        destination: &OpDestination,
    ) -> Result<Dest, (&'static str, String)> {
        match destination {
            OpDestination::Project => Err(("ops.notSupported", "Choose a folder to paste files into.".into())),
            OpDestination::Folder { folder: None } => Ok(Dest { folder: None, abs: root.path().to_path_buf() }),
            OpDestination::Folder { folder: Some(folder) } => {
                let abs = root.resolve_existing(folder).map_err(|e| match e.code {
                    "path.outsideWorkspace" => {
                        ("ops.outsideProject", format!("{folder} resolves outside the project."))
                    }
                    "path.notFound" | "path.brokenLink" => ("ops.notFound", format!("{folder} does not exist.")),
                    _ => ("ops.failed", e.message),
                })?;
                if !abs.is_dir() {
                    return Err(("ops.notSupported", format!("{folder} is not a folder; paste into its folder.")));
                }
                let normalized = root.relativize(&abs);
                if let Some(path) = &normalized
                    && let Some(message) = policy::refuse_destination(path)
                {
                    return Err(("ops.protected", message));
                }
                Ok(Dest { folder: normalized, abs })
            }
        }
    }

    fn locate_external(&self, root: &WorkspaceRoot, raw: &str) -> Result<(Source, Located), (&'static str, String)> {
        let path = PathBuf::from(raw);
        if raw.is_empty() || !path.is_absolute() {
            return Err(("ops.notFound", format!("{raw} is not an absolute path.")));
        }
        let info = fsops::inspect(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => ("ops.notFound", format!("{raw} does not exist.")),
            _ => (io_code(&e), format!("{raw} cannot be read: {e}.")),
        })?;
        if info.kind == EntryKind::Link {
            return Err((
                "ops.linkNotCopied",
                format!(
                    "{raw} is a link; links are not copied (Exposition copies files and folders, not link targets)."
                ),
            ));
        }
        let canonical =
            std::fs::canonicalize(&path).map_err(|e| (io_code(&e), format!("{raw} cannot be read: {e}.")))?;
        if let Some(inside) = root.relativize(&canonical) {
            // A project entry reached by its absolute path: an ordinary source.
            let located = locate(root, &inside)?;
            return Ok((Source::Project(located.path.clone()), located));
        }
        let data_dir = std::fs::canonicalize(self.data_dir()).unwrap_or_else(|_| self.data_dir().to_path_buf());
        if within(&canonical, &data_dir) || within(&data_dir, &canonical) {
            return Err((
                "ops.protected",
                format!("{raw} is or contains Exposition's own data folder, which is never copied into a project."),
            ));
        }
        if info.kind == EntryKind::Dir
            && let Some(reason) = exposition_core::roots::too_broad_reason(&canonical)
        {
            return Err(("ops.notSupported", format!("{reason}; paste a specific file or folder instead.")));
        }
        if !matches!(info.kind, EntryKind::File | EntryKind::Dir) {
            return Err(("ops.notSupported", format!("{raw} is not a file or folder.")));
        }
        let name = canonical
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| WorkspacePath::parse(n).is_ok())
            .ok_or_else(|| ("ops.notSupported", format!("{raw} has a name that cannot be used in a project.")))?
            .to_owned();
        let parent_abs = canonical.parent().map(Path::to_path_buf).unwrap_or_default();
        Ok((
            Source::External(canonical.clone()),
            Located {
                path: WorkspacePath::parse(&name)
                    .map_err(|_| ("ops.notSupported", format!("{raw} has an invalid name.")))?,
                abs: canonical,
                parent: None,
                parent_abs,
                info,
            },
        ))
    }

    /// Returns false when the item is refused.
    fn plan_delete(&self, plan: &mut PathPlan, item: &mut Item, permanent: bool) -> bool {
        let Source::Project(path) = item.source.clone() else { return false };
        if let Some(message) = policy::refuse_change(&path) {
            plan.refusals.push(refusal(&item.target, "ops.protected", message));
            return false;
        }
        if item.kind == EntryKind::Dir {
            let walk = fsops::walk(&item.abs);
            plan.fingerprint.push(format!("tree:{}|{}", path, walk.digest));
            if !walk.readonly.is_empty() {
                let shown: Vec<String> = walk.readonly.iter().map(|r| format!("{path}/{r}")).collect();
                plan.refusals.push(refusal(
                    &item.target,
                    "ops.readOnly",
                    format!(
                        "{path} contains read-only files ({}); they are not deleted. Clear the read-only setting first.",
                        list_some(&shown, 3)
                    ),
                ));
                return false;
            }
            item.walk = Some(walk);
        } else if item.readonly && item.kind != EntryKind::Link {
            plan.refusals.push(refusal(
                &item.target,
                "ops.readOnly",
                format!("{path} is read-only; it is not deleted. Clear the read-only setting first."),
            ));
            return false;
        }
        if item.kind != EntryKind::Link {
            for doc in self.documents_under(&path) {
                plan.fingerprint.push(format!("doc:{}|{}|{:?}", doc.id.0, doc.revision, doc.saved_revision));
                if doc.saved_revision == Some(doc.revision) {
                    item.clean.push(doc);
                } else {
                    item.dirty.push(doc);
                }
            }
        }
        if !item.dirty.is_empty() {
            let names: Vec<String> = item.dirty.iter().filter_map(|d| d.path.as_ref().map(|p| p.to_string())).collect();
            plan.impacts.push(OpImpact {
                kind: ImpactKind::UnsavedBuffer,
                target: item.target.clone(),
                consumers: item
                    .dirty
                    .iter()
                    .map(|d| OpConsumer {
                        kind: "buffer".into(),
                        id: d.id.0.clone(),
                        label: d.path.as_ref().map(|p| p.file_name().to_owned()).unwrap_or_default(),
                        path: d.path.clone(),
                    })
                    .collect(),
                message: format!(
                    "{} {} unsaved changes in the editor.",
                    list_some(&names, 5),
                    if names.len() == 1 { "has" } else { "have" }
                ),
            });
        }
        let bin = trash::OsTrash.name();
        let what = match (&item.kind, &item.walk) {
            (EntryKind::Dir, Some(walk)) => format!("{path} ({})", folder_summary(walk)),
            (EntryKind::Link, _) => format!("the link {path} (its target is not changed)"),
            (_, _) => format!("{path} ({})", size_text(fsops::inspect(&item.abs).map(|i| i.len).unwrap_or(0))),
        };
        if permanent {
            if item.kind != EntryKind::Link && !policy::disposable_cache(&path) {
                plan.impacts.push(OpImpact {
                    kind: ImpactKind::RecoveryLoss,
                    target: item.target.clone(),
                    consumers: Vec::new(),
                    message: format!(
                        "{what} will be deleted permanently: it cannot be restored from {bin} or by Exposition."
                    ),
                });
            }
            plan.effects.push(OpEffect {
                target: item.target.clone(),
                description: format!("Delete {what} permanently"),
                destination: None,
                recovery: OpRecovery::None,
            });
        } else {
            plan.effects.push(OpEffect {
                target: item.target.clone(),
                description: format!("Move {what} to {bin}"),
                destination: None,
                recovery: OpRecovery::OsTrash,
            });
        }
        plan.deleted.push(path);
        true
    }

    /// Returns false when the item is refused.
    fn plan_copy(&self, plan: &mut PathPlan, item: &mut Item, planned_names: &mut HashSet<String>) -> bool {
        let is_paste = matches!(plan.kind, Some(OpKind::Paste { .. }));
        if let Source::Project(path) = &item.source {
            if let Some(message) = policy::refuse_source(path) {
                plan.refusals.push(refusal(&item.target, "ops.protected", message));
                return false;
            }
            if !is_paste && let Some(message) = policy::refuse_change(path) {
                plan.refusals.push(refusal(&item.target, "ops.protected", message));
                return false;
            }
        } else if !is_paste {
            plan.refusals.push(refusal(&item.target, "ops.notSupported", "Only project files can be duplicated."));
            return false;
        }
        match item.kind {
            EntryKind::Link => {
                plan.refusals.push(refusal(
                    &item.target,
                    "ops.linkNotCopied",
                    format!(
                        "{} is a link; links are not copied (copying it would copy whatever it points to).",
                        item.shown()
                    ),
                ));
                return false;
            }
            EntryKind::Other => {
                plan.refusals.push(refusal(
                    &item.target,
                    "ops.notSupported",
                    format!("{} is not a file or folder.", item.shown()),
                ));
                return false;
            }
            _ => {}
        }
        let (parent, parent_abs) = if is_paste {
            let dest = plan.dest.as_ref().expect("paste plans have a destination");
            (dest.folder.clone(), dest.abs.clone())
        } else {
            if let Some(parent) = &item.parent
                && let Some(message) = policy::refuse_destination(parent)
            {
                plan.refusals.push(refusal(&item.target, "ops.protected", message));
                return false;
            }
            (item.parent.clone(), item.parent_abs.clone())
        };
        // Never copy a folder into itself or one of its own folders.
        if item.is_dir() && within(&parent_abs, &item.abs) {
            plan.refusals.push(refusal(
                &item.target,
                "ops.intoItself",
                format!("{} cannot be copied into itself or one of its own folders.", item.shown()),
            ));
            return false;
        }
        if let Source::External(abs) = &item.source
            && within(self.root().map(|r| r.path().to_path_buf()).as_deref().unwrap_or(Path::new("")), abs)
        {
            plan.refusals.push(refusal(
                &item.target,
                "ops.intoItself",
                format!("{} contains this project; it cannot be copied into it.", item.shown()),
            ));
            return false;
        }
        let mut description_extra = String::new();
        if item.is_dir() {
            let walk = fsops::walk(&item.abs);
            plan.fingerprint.push(format!("tree:{}|{}", display_path(&item.abs), walk.digest));
            if !walk.links.is_empty() {
                description_extra = format!(
                    "; {} inside {} not copied",
                    if walk.links.len() == 1 { "1 link".to_owned() } else { format!("{} links", walk.links.len()) },
                    if walk.links.len() == 1 { "is" } else { "are" }
                );
            }
            item.walk = Some(walk);
        }
        if let Source::Project(path) = &item.source {
            for doc in self.documents_under(path) {
                if doc.saved_revision != Some(doc.revision) {
                    plan.fingerprint.push(format!("doc:{}|{}|{:?}", doc.id.0, doc.revision, doc.saved_revision));
                    item.dirty.push(doc);
                }
            }
            if !item.dirty.is_empty() {
                let names: Vec<String> =
                    item.dirty.iter().filter_map(|d| d.path.as_ref().map(|p| p.to_string())).collect();
                plan.impacts.push(OpImpact {
                    kind: ImpactKind::SavedVersionCopied,
                    target: item.target.clone(),
                    consumers: item
                        .dirty
                        .iter()
                        .map(|d| OpConsumer {
                            kind: "buffer".into(),
                            id: d.id.0.clone(),
                            label: d.path.as_ref().map(|p| p.file_name().to_owned()).unwrap_or_default(),
                            path: d.path.clone(),
                        })
                        .collect(),
                    message: format!(
                        "{} {} unsaved changes; the saved version on disk was copied, not the text in the editor.",
                        list_some(&names, 5),
                        if names.len() == 1 { "has" } else { "have" }
                    ),
                });
            }
            plan.copied.push(path.clone());
        }
        let planned = names::first_free(parent.as_ref(), &item.name, item.is_dir(), is_paste, |candidate| {
            let key = candidate.as_str().to_lowercase();
            planned_names.contains(&key) || {
                let name = candidate.file_name();
                std::fs::symlink_metadata(parent_abs.join(name)).is_ok()
            }
        });
        let Some(planned) = planned else {
            plan.refusals.push(refusal(
                &item.target,
                "ops.nameUnavailable",
                format!("No free name was found for a copy of {}.", item.shown()),
            ));
            return false;
        };
        planned_names.insert(planned.as_str().to_lowercase());
        plan.effects.push(OpEffect {
            target: item.target.clone(),
            description: format!("Copy {} to {planned}{description_extra}", item.shown()),
            destination: Some(OpTarget::Path { path: planned.clone() }),
            recovery: OpRecovery::None,
        });
        item.planned = Some(planned);
        true
    }

    /// Apply a (freshly computed) path plan. Returns one outcome per planned
    /// item and per covered target; plan refusals are reported by the caller.
    /// `on_outcome` sees each outcome as soon as it is final (for journaling).
    pub fn apply_paths(
        &self,
        plan: &PathPlan,
        options: &ApplyOptions<'_>,
        on_outcome: &mut dyn FnMut(&PathOutcome),
    ) -> Vec<PathOutcome> {
        let mut out: Vec<PathOutcome> = Vec::new();
        let skip = |target: &OpTarget| options.skip.iter().find(|r| &r.target == target);
        let mut eligible: Vec<&Item> = Vec::new();
        for item in &plan.items {
            if let Some(r) = skip(&item.target) {
                let o = PathOutcome {
                    outcome: outcome(&item.target, OpStatus::Skipped, Some(&r.code), Some(r.message.clone())),
                    trashed: None,
                    path: project_path(item),
                    covered: false,
                };
                on_outcome(&o);
                out.push(o);
            } else {
                eligible.push(item);
            }
        }
        match &plan.kind {
            Some(OpKind::Delete { permanent }) => {
                self.apply_delete(&eligible, *permanent, options, on_outcome, &mut out)
            }
            Some(OpKind::Duplicate) => self.apply_copy(&eligible, None, false, options, on_outcome, &mut out),
            Some(OpKind::Paste { .. }) => {
                self.apply_copy(&eligible, plan.dest.as_ref(), true, options, on_outcome, &mut out)
            }
            None => {}
        }
        for (target, cover) in &plan.covered {
            let covering = out.iter().find(|o| &o.outcome.target == cover).map(|o| o.outcome.clone());
            let (status, code) = match &covering {
                Some(c) => (c.status, c.code.clone()),
                None => (OpStatus::Skipped, Some("ops.covered".to_owned())),
            };
            let shown = match cover {
                OpTarget::Path { path } => path.to_string(),
                OpTarget::External { path } => path.clone(),
                _ => String::new(),
            };
            let o = PathOutcome {
                outcome: OpOutcome {
                    target: target.clone(),
                    status,
                    code,
                    message: Some(format!("Included in {shown}.")),
                    created: None,
                    recovery: covering.map(|c| c.recovery).unwrap_or(OpRecovery::None),
                    restore: None,
                },
                trashed: None,
                path: None,
                covered: true,
            };
            on_outcome(&o);
            out.push(o);
        }
        out
    }

    fn apply_delete(
        &self,
        items: &[&Item],
        permanent: bool,
        options: &ApplyOptions<'_>,
        on_outcome: &mut dyn FnMut(&PathOutcome),
        out: &mut Vec<PathOutcome>,
    ) {
        let choice = options.choices.get(CHOICE_UNSAVED).map(String::as_str).unwrap_or(UNSAVED_PRESERVE);
        let bin = options.trash.name();
        let mut ready: Vec<&Item> = Vec::new();
        for item in items {
            if !item.dirty.is_empty() && choice == UNSAVED_CANCEL {
                let o = PathOutcome {
                    outcome: outcome(
                        &item.target,
                        OpStatus::Skipped,
                        Some("ops.cancelled"),
                        Some(format!("{} has unsaved changes and was not deleted.", item.shown())),
                    ),
                    trashed: None,
                    path: project_path(item),
                    covered: false,
                };
                on_outcome(&o);
                out.push(o);
                continue;
            }
            // Before the file disappears: no journal may bring it back.
            for doc in &item.dirty {
                let _ = self.suspend_document(&doc.id);
            }
            ready.push(item);
        }
        let results: Vec<Result<Option<TrashedItem>, (String, String)>> = if permanent {
            ready
                .iter()
                .map(|item| match fsops::remove_permanently(&item.abs) {
                    Ok(0) => Ok(None),
                    Ok(left) => Err((
                        "ops.partial".to_owned(),
                        format!("{} was only partly deleted: {left} items could not be removed.", item.shown()),
                    )),
                    Err(e) => Err((io_code(&e).to_owned(), format!("{} could not be deleted: {e}.", item.shown()))),
                })
                .collect()
        } else {
            let paths: Vec<PathBuf> = ready.iter().map(|i| i.abs.clone()).collect();
            options
                .trash
                .delete(&paths)
                .into_iter()
                .map(|r| r.map_err(|reason| ("ops.trashUnavailable".to_owned(), reason)))
                .collect()
        };
        for (item, result) in ready.iter().zip(results) {
            let gone = matches!(fsops::inspect(&item.abs), Err(e) if e.kind() == io::ErrorKind::NotFound);
            let mut o = outcome(&item.target, OpStatus::Succeeded, None, None);
            let mut trashed = None;
            match result {
                Ok(found) => {
                    o.recovery = if permanent { OpRecovery::None } else { OpRecovery::OsTrash };
                    if !permanent && found.is_none() {
                        o.message = Some(format!(
                            "Moved to {bin}. Exposition cannot identify it there reliably; restore it from {bin} if needed."
                        ));
                    }
                    trashed = found;
                }
                Err((code, reason)) if !permanent && gone => {
                    // The shell reported a problem, but the entry left the project.
                    o.recovery = OpRecovery::OsTrash;
                    o.code = Some(code);
                    o.message = Some(format!(
                        "{bin} reported a problem ({reason}), but {} is no longer in the project; look for it in {bin}.",
                        item.shown()
                    ));
                }
                Err((code, reason)) => {
                    o.status = OpStatus::Failed;
                    o.message = Some(if permanent {
                        reason
                    } else {
                        format!(
                            "{} could not be moved to {bin} ({reason}). Nothing was deleted permanently; use Delete Permanently to remove it without {bin}.",
                            item.shown()
                        )
                    });
                    o.code = Some(code);
                }
            }
            if o.status == OpStatus::Succeeded {
                // Unsaved text: discarded (closed, journal removed) or kept
                // with its file suspended. Clean documents of deleted files close.
                if options.choices.get(CHOICE_UNSAVED).map(String::as_str) == Some(UNSAVED_DISCARD) {
                    for doc in &item.dirty {
                        let _ = self.close_document(&doc.id);
                    }
                }
                for doc in &item.clean {
                    if let Ok(now) = self.documents().info(&doc.id)
                        && now.revision == doc.revision
                    {
                        let _ = self.close_document(&doc.id);
                    }
                }
            } else if let Source::Project(path) = &item.source {
                if gone {
                    // Partly removed: keep the unsaved text suspended.
                } else {
                    self.resume_documents_under(path);
                }
            }
            let po = PathOutcome { outcome: o, trashed, path: project_path(item), covered: false };
            on_outcome(&po);
            out.push(po);
        }
    }

    fn apply_copy(
        &self,
        items: &[&Item],
        dest: Option<&Dest>,
        keep_original: bool,
        options: &ApplyOptions<'_>,
        on_outcome: &mut dyn FnMut(&PathOutcome),
        out: &mut Vec<PathOutcome>,
    ) {
        for item in items {
            let (parent, parent_abs) = match dest {
                Some(d) => (d.folder.clone(), d.abs.clone()),
                None => (item.parent.clone(), item.parent_abs.clone()),
            };
            let mut o = outcome(&item.target, OpStatus::Succeeded, None, None);
            match copy_and_publish(item, parent.as_ref(), &parent_abs, keep_original, options.tag) {
                Ok((created, report)) => {
                    o.created = Some(OpTarget::Path { path: created.clone() });
                    if !report.links_skipped.is_empty() {
                        o.code = Some("ops.linkNotCopied".into());
                        o.message = Some(format!(
                            "Copied to {created} without {} (links are not copied): {}.",
                            if report.links_skipped.len() == 1 {
                                "1 link".to_owned()
                            } else {
                                format!("{} links", report.links_skipped.len())
                            },
                            list_some(&report.links_skipped, 5)
                        ));
                    }
                }
                Err((code, message)) => {
                    o.status = OpStatus::Failed;
                    o.code = Some(code.to_owned());
                    o.message = Some(message);
                }
            }
            let po = PathOutcome { outcome: o, trashed: None, path: project_path(item), covered: false };
            on_outcome(&po);
            out.push(po);
        }
    }

    /// Restore a trashed entry that the app recorded at deletion. Nothing at
    /// the original location is overwritten: when it is occupied the entry
    /// gets a fresh name (`rename`, the default) or nothing happens (`cancel`).
    pub fn restore_trashed(
        &self,
        original: &WorkspacePath,
        item: &TrashedItem,
        on_collision: Option<&str>,
        trash: &dyn TrashBin,
        tag: &str,
    ) -> OpOutcome {
        let target = OpTarget::Path { path: original.clone() };
        let fail = |code: &str, message: String| outcome(&target, OpStatus::Failed, Some(code), Some(message));
        let cancel_on_collision = match on_collision {
            None | Some("rename") => false,
            Some("cancel") => true,
            Some(other) => return fail("ops.invalidRequest", format!("Unknown collision choice {other:?}.")),
        };
        let root = match self.root() {
            Ok(root) => root,
            Err(_) => return fail("ops.noProject", "No project is open.".into()),
        };
        let bin = trash.name();
        match trash.contains(item) {
            Ok(true) => {}
            Ok(false) => {
                return fail(
                    "ops.restoreUnavailable",
                    format!("{original} is no longer in {bin} (it may have been restored or emptied)."),
                );
            }
            Err(e) => return fail("ops.restoreUnavailable", format!("{bin} cannot be read: {e}.")),
        }
        let parent = original.parent();
        if let Some(parent) = &parent
            && let Some(message) = policy::refuse_destination(parent)
        {
            return fail("ops.protected", message);
        }
        // The original folder (recreated inside the project when it is gone).
        let parent_abs = match &parent {
            None => root.path().to_path_buf(),
            Some(parent) => match root.resolve_for_write(parent) {
                Ok(abs) => {
                    if let Err(e) = std::fs::create_dir_all(&abs) {
                        return fail(io_code(&e), format!("{parent} could not be recreated: {e}."));
                    }
                    match root.resolve_existing(parent) {
                        Ok(abs) => abs,
                        Err(e) => return fail("ops.outsideProject", e.message),
                    }
                }
                Err(e) if e.code == "path.outsideWorkspace" => return fail("ops.outsideProject", e.message),
                Err(e) => return fail("ops.failed", e.message),
            },
        };
        let occupied = std::fs::symlink_metadata(parent_abs.join(original.file_name())).is_ok();
        if occupied && cancel_on_collision {
            return outcome(
                &target,
                OpStatus::Skipped,
                Some("ops.restoreCollision"),
                Some(format!("{original} exists again, so nothing was restored; the item is still in {bin}.")),
            );
        }
        let staged = fsops::staging_path(&parent_abs, tag);
        let staged_name = staged.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        if let Err(e) = trash.restore_to(item, &parent_abs, &staged_name) {
            return fail("ops.restoreFailed", format!("{original} could not be restored from {bin}: {e}."));
        }
        let is_dir = std::fs::symlink_metadata(&staged).map(|m| m.is_dir()).unwrap_or(false);
        let mut published = None;
        for candidate in names::candidates(original.file_name(), is_dir, true) {
            let Ok(path) = names::join(parent.as_ref(), &candidate) else { continue };
            let result = if is_dir {
                fsops::publish_dir(&staged, &parent_abs.join(&candidate))
            } else {
                fsops::publish_file(&staged, &parent_abs.join(&candidate))
            };
            match result {
                Ok(()) => {
                    published = Some(path);
                    break;
                }
                Err(PublishError::Exists) => continue,
                Err(PublishError::Io(e)) => {
                    return fail(
                        io_code(&e),
                        format!(
                            "{original} was taken out of {bin} but could not be put back in place ({e}); it is in {} under a temporary name.",
                            parent.as_ref().map(|p| p.to_string()).unwrap_or_else(|| "the project folder".into())
                        ),
                    );
                }
            }
        }
        let Some(restored) = published else {
            return fail("ops.nameUnavailable", format!("No free name was found to restore {original}."));
        };
        let mut o = outcome(&target, OpStatus::Succeeded, None, None);
        if &restored == original {
            self.resume_documents_under(original);
        } else {
            o.message = Some(format!("{original} exists, so the restored item is {restored}; both are kept."));
        }
        o.created = Some(OpTarget::Path { path: restored });
        o
    }

    /// Remove staging leftovers of operation `tag` in `folders` (after an
    /// interruption). Returns how many were removed.
    pub fn remove_staging_leftovers(&self, folders: &[PathBuf], tag: &str) -> usize {
        let Ok(root) = self.root() else { return 0 };
        let mut removed = 0;
        for folder in folders {
            if !root.contains(&std::fs::canonicalize(folder).unwrap_or_else(|_| folder.clone())) {
                continue;
            }
            let Ok(read) = std::fs::read_dir(folder) else { continue };
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if fsops::is_staging_name(&name, tag) && fsops::remove_staging(&entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }
}

fn project_path(item: &Item) -> Option<WorkspacePath> {
    match &item.source {
        Source::Project(path) => Some(path.clone()),
        Source::External(_) => None,
    }
}

fn copy_and_publish(
    item: &Item,
    parent: Option<&WorkspacePath>,
    parent_abs: &Path,
    keep_original: bool,
    tag: &str,
) -> Result<(WorkspacePath, fsops::CopyReport), (&'static str, String)> {
    let staged = fsops::staging_path(parent_abs, tag);
    let copied = if item.is_dir() {
        fsops::copy_tree(&item.abs, &staged)
    } else {
        fsops::copy_file(&item.abs, &staged).map(|bytes| fsops::CopyReport {
            files: 1,
            bytes,
            links_skipped: Vec::new(),
        })
    };
    let report = match copied {
        Ok(report) => report,
        Err(e) => {
            let _ = fsops::remove_staging(&staged);
            return Err((io_code(&e), format!("{} could not be copied: {e}.", item.shown())));
        }
    };
    for candidate in names::candidates(&item.name, item.is_dir(), keep_original) {
        let Ok(path) = names::join(parent, &candidate) else { continue };
        let target = parent_abs.join(&candidate);
        let result =
            if item.is_dir() { fsops::publish_dir(&staged, &target) } else { fsops::publish_file(&staged, &target) };
        match result {
            Ok(()) => return Ok((path, report)),
            Err(PublishError::Exists) => continue,
            Err(PublishError::Io(e)) => {
                let _ = fsops::remove_staging(&staged);
                return Err((io_code(&e), format!("The copy of {} could not be saved: {e}.", item.shown())));
            }
        }
    }
    let _ = fsops::remove_staging(&staged);
    Err(("ops.nameUnavailable", format!("No free name was found for a copy of {}.", item.shown())))
}
