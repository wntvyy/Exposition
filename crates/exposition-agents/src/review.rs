//! Change review: every agent change is recorded per task and file, and can be
//! kept (accepted) or reverted.
//!
//! For each file a task changed we keep three texts:
//! - `checkpoint`: the text before the task first changed it (evidence);
//! - `tip`: the text right after the task's latest change (at `tip_revision`);
//! - `baseline`: the same file *without* the task's changes, as of the tip.
//!
//! When other edits (the user typing) land between two agent changes, the
//! baseline absorbs them by rebasing the task's net inverse over them; if they
//! overlap the agent's changes the file is marked *entangled* and automatic
//! revert is refused rather than guessed.
//!
//! Reverting applies `diff(tip → baseline)` as an inverse transaction based on
//! `tip_revision`, so the document service rebases it over everything that
//! happened later; when that history touches the agent's text (a later edit,
//! or an undo and redo), the inverse is rebased over the current text instead,
//! and refused on overlap. Every revert is itself recorded.
//!
//! Review state follows the content: undo history is shared by every origin,
//! so the user's Ctrl+Z can remove an agent change (shown as undone, nothing to
//! revert) and undoing a revert brings it back (pending again).

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionError, TransactionId,
    TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_workspace::{DocumentService, WorkspaceService};

use crate::model::{FileDiff, FileReview, ReviewActionResult, ReviewChangeKind, ReviewState, TaskId};
use crate::patch;
use crate::store::{ChangeRow, Store, StoreResult, TaskFileRow, now_ms};
use crate::workspace::{self, FileError, ProjectFiles};

pub struct EditRecord<'a> {
    pub tool_use_id: &'a str,
    pub path: &'a WorkspacePath,
    pub document: &'a DocumentId,
    pub previous_revision: u64,
    pub revision: u64,
    pub patches: &'a [TextPatch],
    /// Text at `previous_revision`, when known exactly.
    pub before: Option<&'a str>,
    /// Text at `revision`, when known exactly.
    pub after: Option<&'a str>,
}

/// Record an applied agent edit and update the file's review state.
pub fn record_edit(store: &Store, task: &TaskId, rec: &EditRecord<'_>) -> StoreResult<()> {
    let mut row = match store.task_file(task, rec.path)? {
        Some(row) => row,
        None => {
            let before = rec.before.unwrap_or_default().to_owned();
            TaskFileRow {
                path: rec.path.clone(),
                kind: ReviewChangeKind::Edited,
                document: Some(rec.document.0.clone()),
                checkpoint_text: before.clone(),
                baseline_text: before.clone(),
                tip_text: before,
                tip_revision: Some(rec.previous_revision),
                entangled: rec.before.is_none(),
                state: ReviewState::Pending,
                change_count: 0,
                message: None,
            }
        }
    };
    match (rec.before, rec.after) {
        (Some(before), Some(after)) => {
            let continuous = row.document.as_deref() == Some(rec.document.0.as_str())
                && row.tip_revision == Some(rec.previous_revision);
            if !continuous && !row.entangled {
                // Other edits happened since the task's last change: carry them
                // into the baseline, or give up if they overlap the agent's work.
                let foreign = patch::diff(&row.tip_text, before);
                let net_inverse = patch::diff(&row.tip_text, &row.baseline_text);
                match patch::rebase(&net_inverse, &foreign).and_then(|inv| patch::apply(before, &inv).ok()) {
                    Some(baseline) => row.baseline_text = baseline,
                    None => row.entangled = true,
                }
            }
            row.tip_text = after.to_owned();
        }
        (_, after) => {
            row.entangled = true;
            if let Some(after) = after {
                row.tip_text = after.to_owned();
            }
        }
    }
    row.tip_revision = Some(rec.revision);
    row.document = Some(rec.document.0.clone());
    row.change_count += 1;
    row.state = ReviewState::Pending;
    row.message = None;
    store.upsert_task_file(task, &row)?;
    let inverse = rec.before.and_then(|b| patch::invert(b, rec.patches).ok()).unwrap_or_default();
    store.insert_change(
        task,
        &ChangeRow {
            id: 0,
            tool_use_id: Some(rec.tool_use_id.to_owned()),
            path: rec.path.clone(),
            document: Some(rec.document.0.clone()),
            kind: "edit".into(),
            previous_revision: Some(rec.previous_revision),
            revision: Some(rec.revision),
            patches: rec.patches.to_vec(),
            inverse,
            created_at_ms: now_ms(),
        },
    )?;
    Ok(())
}

/// Record a file the agent created (opened as `document` at `revision`).
pub fn record_create(
    store: &Store,
    task: &TaskId,
    tool_use_id: &str,
    path: &WorkspacePath,
    content: &str,
    document: Option<(&DocumentId, u64)>,
) -> StoreResult<()> {
    let row = TaskFileRow {
        path: path.clone(),
        kind: ReviewChangeKind::Created,
        document: document.map(|(id, _)| id.0.clone()),
        checkpoint_text: String::new(),
        baseline_text: String::new(),
        tip_text: content.to_owned(),
        tip_revision: document.map(|(_, rev)| rev),
        entangled: false,
        state: ReviewState::Pending,
        change_count: 1,
        message: None,
    };
    store.upsert_task_file(task, &row)?;
    let len = u32::try_from(exposition_core::text::utf16_len(content)).unwrap_or(u32::MAX);
    store.insert_change(
        task,
        &ChangeRow {
            id: 0,
            tool_use_id: Some(tool_use_id.to_owned()),
            path: path.clone(),
            document: row.document.clone(),
            kind: "create".into(),
            previous_revision: None,
            revision: document.map(|(_, rev)| rev),
            patches: vec![TextPatch::insert(0, content)],
            inverse: vec![TextPatch::delete(0, len)],
            created_at_ms: now_ms(),
        },
    )?;
    Ok(())
}

/// What a file holds now, for deciding its review state from content.
pub enum Current {
    /// The open document's text (unsaved agent edits live here).
    Open(std::sync::Arc<str>),
    /// The file on disk (no document is open for it).
    Disk(String),
    /// Neither open nor on disk.
    Missing,
    /// It could not be read; the stored state stands.
    Unknown,
}

impl Current {
    pub fn text(&self) -> Option<&str> {
        match self {
            Current::Open(text) => Some(text),
            Current::Disk(text) => Some(text),
            Current::Missing | Current::Unknown => None,
        }
    }
}

/// The file's current content: the open document, else the file on disk.
pub fn current(path: &WorkspacePath, documents: &DocumentService, files: &dyn ProjectFiles) -> Current {
    if let Some(snapshot) = documents.find_by_path(path).and_then(|id| documents.snapshot(&id).ok()) {
        return Current::Open(snapshot.text);
    }
    match files.read_text(path) {
        Ok(loaded) => Current::Disk(loaded.text),
        Err(FileError::NotFound(_)) => Current::Missing,
        Err(_) => Current::Unknown,
    }
}

/// Whether a file's current text still contains the task's net change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// The change is there (possibly with separable later edits around it).
    Present,
    /// The change is gone: the agent's regions hold the text without it, and
    /// any other differences are separate from them.
    Absent,
    /// Later edits overlap the agent's regions; neither can be told apart.
    Overlapped,
}

/// Decide from content alone whether the change `baseline → tip` is in `current`.
pub fn presence(baseline: &str, tip: &str, current: &str) -> Presence {
    let forward = patch::diff(baseline, tip);
    if forward.is_empty() || current == tip {
        return Presence::Present;
    }
    if current == baseline {
        return Presence::Absent;
    }
    let inverse = patch::diff(tip, baseline);
    if patch::rebase(&inverse, &patch::diff(tip, current)).is_some() {
        return Presence::Present;
    }
    if patch::rebase(&forward, &patch::diff(baseline, current)).is_some() {
        return Presence::Absent;
    }
    Presence::Overlapped
}

/// The review state a file is actually in, judged from its current content,
/// and the message explaining a state that differs from the stored one.
///
/// The stored state records review actions; content can move on without one:
/// the user's Ctrl+Z (undo history is shared by every origin) or discarding
/// unsaved changes removes a pending change, and undoing a revert brings the
/// change back. Accepted files stay accepted whatever happens to them later.
pub fn effective_state(row: &TaskFileRow, current: &Current) -> (ReviewState, Option<String>) {
    match (row.kind, row.state) {
        (ReviewChangeKind::Edited, ReviewState::Pending | ReviewState::Reverted) if !row.entangled => {
            let Some(text) = current.text() else { return (row.state, row.message.clone()) };
            match (row.state, presence(&row.baseline_text, &row.tip_text, text)) {
                (ReviewState::Pending, Presence::Absent) => (
                    ReviewState::Undone,
                    Some("The agent's changes are no longer in the file: they were undone or discarded.".into()),
                ),
                (ReviewState::Reverted, Presence::Present) if text != row.baseline_text.as_str() => (
                    ReviewState::Pending,
                    Some("The agent's changes are back in the file (the revert was undone).".into()),
                ),
                _ => (row.state, row.message.clone()),
            }
        }
        (ReviewChangeKind::Created, ReviewState::Pending) if matches!(current, Current::Missing) => {
            (ReviewState::Undone, Some("The file the agent created no longer exists.".into()))
        }
        _ => (row.state, row.message.clone()),
    }
}

/// Review entry for one file. `current` is `None` when the file's content
/// cannot be consulted (e.g. the task's folder is not the open one); the
/// stored state is shown then.
pub fn file_review(row: &TaskFileRow, documents: &DocumentService, current: Option<&Current>) -> FileReview {
    let current_revision =
        documents.find_by_path(&row.path).and_then(|id| documents.info(&id).ok()).map(|i| i.revision);
    let (state, message) = match current {
        Some(current) => effective_state(row, current),
        None => (row.state, row.message.clone()),
    };
    FileReview {
        path: row.path.clone(),
        kind: row.kind,
        state,
        change_count: row.change_count,
        entangled: row.entangled,
        tip_revision: row.tip_revision,
        current_revision,
        message,
    }
}

pub fn file_diff(row: &TaskFileRow) -> FileDiff {
    FileDiff {
        path: row.path.clone(),
        language: workspace::language_for(&row.path).to_owned(),
        original: row.baseline_text.clone(),
        modified: row.tip_text.clone(),
        checkpoint: row.checkpoint_text.clone(),
    }
}

pub fn accept_file(
    store: &Store,
    task: &TaskId,
    mut row: TaskFileRow,
    current: &Current,
) -> StoreResult<ReviewActionResult> {
    let refused = |message: &str| ReviewActionResult { path: row.path.clone(), ok: false, message: message.into() };
    match effective_state(&row, current).0 {
        ReviewState::Reverted => return Ok(refused("These changes were already reverted.")),
        ReviewState::Undone => {
            return Ok(refused("The agent's changes are no longer in the file, so there is nothing to keep."));
        }
        ReviewState::Pending | ReviewState::Accepted => {}
    }
    row.state = ReviewState::Accepted;
    row.message = Some("Kept.".into());
    store.upsert_task_file(task, &row)?;
    store.insert_change(task, &marker(&row, "accept"))?;
    Ok(ReviewActionResult { path: row.path, ok: true, message: "Kept.".into() })
}

fn marker(row: &TaskFileRow, kind: &str) -> ChangeRow {
    ChangeRow {
        id: 0,
        tool_use_id: None,
        path: row.path.clone(),
        document: row.document.clone(),
        kind: kind.into(),
        previous_revision: None,
        revision: None,
        patches: Vec::new(),
        inverse: Vec::new(),
        created_at_ms: now_ms(),
    }
}

/// Revert one file's changes for a task.
pub fn revert_file(
    store: &Store,
    task: &TaskId,
    documents: &DocumentService,
    workspace: Option<&WorkspaceService>,
    files: &dyn ProjectFiles,
    row: TaskFileRow,
) -> StoreResult<ReviewActionResult> {
    let now = current(&row.path, documents, files);
    let outcome = match effective_state(&row, &now).0 {
        ReviewState::Reverted => Ok("Already reverted.".to_owned()),
        // Already gone from the content: nothing is applied and the stored
        // state stays, so a redo makes the change revertible again.
        ReviewState::Undone => {
            let message = match row.kind {
                ReviewChangeKind::Edited => "Nothing to revert: the agent's changes were already undone.",
                ReviewChangeKind::Created => "Nothing to revert: the file the agent created no longer exists.",
            };
            return Ok(ReviewActionResult { path: row.path, ok: true, message: message.into() });
        }
        _ if row.entangled => Err("Edits made while the agent was working overlap its changes, so they cannot be             separated automatically. Compare the versions and fix the text by hand, or keep the changes."
            .to_owned()),
        ReviewState::Pending | ReviewState::Accepted => match row.kind {
            ReviewChangeKind::Created => revert_created(store, task, documents, workspace, files, &row),
            ReviewChangeKind::Edited => revert_edited(store, task, documents, workspace, files, &row),
        },
    };
    let mut row = row;
    let result = match outcome {
        Ok(message) => {
            row.state = ReviewState::Reverted;
            row.message = Some(message.clone());
            ReviewActionResult { path: row.path.clone(), ok: true, message }
        }
        Err(message) => {
            row.message = Some(message.clone());
            ReviewActionResult { path: row.path.clone(), ok: false, message }
        }
    };
    store.upsert_task_file(task, &row)?;
    Ok(result)
}

fn transaction(
    task: &TaskId,
    document: &DocumentId,
    base: u64,
    patches: Vec<TextPatch>,
    conflict: ConflictPolicy,
) -> TransactionRequest {
    TransactionRequest {
        id: TransactionId(format!("agent-revert-{}", uuid::Uuid::new_v4())),
        origin: EditOrigin::Agent { task_id: task.0.clone() },
        edits: vec![DocumentEdit { document: document.clone(), base_revision: base, patches }],
        undo: UndoBehavior::NewGroup,
        conflict,
        label: Some("Revert agent changes".into()),
        selection: None,
    }
}

fn revert_edited(
    store: &Store,
    task: &TaskId,
    documents: &DocumentService,
    workspace: Option<&WorkspaceService>,
    files: &dyn ProjectFiles,
    row: &TaskFileRow,
) -> Result<String, String> {
    let inverse = patch::diff(&row.tip_text, &row.baseline_text);
    if inverse.is_empty() {
        return Ok("Nothing to revert: the task's changes cancel out.".into());
    }
    const OVERLAP: &str = "Later edits overlap the agent's changes, so the revert was refused to protect them. \
        Edit the text by hand or keep the changes.";

    // Preferred: let the document service rebase the inverse over every
    // revision since the tip.
    if let (Some(id), Some(tip)) = (documents.find_by_path(&row.path), row.tip_revision)
        && row.document.as_deref() == Some(id.0.as_str())
        && documents.info(&id).map(|i| i.revision >= tip).unwrap_or(false)
    {
        let before = documents.snapshot(&id).ok();
        match documents.apply(transaction(task, &id, tip, inverse.clone(), ConflictPolicy::RebaseDisjoint)) {
            Ok(outcome) => {
                record_revert(store, task, row, &id, &outcome.applied[0], before.as_ref());
                return Ok(revert_message(outcome.applied[0].rebased));
            }
            // Some revision since the tip touched the agent's text: a later
            // edit, or an undo and redo that restored it. Decide from the text.
            Err(TransactionError::Conflict { .. } | TransactionError::HistoryUnavailable { .. }) => {}
            Err(e) => return Err(format!("The revert was refused: {e}")),
        }
    }

    // Otherwise rebase against the current text ourselves (the history touched
    // the agent's text, the document was reopened, or its history no longer
    // reaches the tip). Overlap in the text itself refuses.
    let id = workspace::open_document_in(workspace, documents, files, &row.path).map_err(|e| match e {
        FileError::NotFound(_) => "The file no longer exists, so there is nothing to revert.".to_owned(),
        other => format!("The file could not be opened: {other}"),
    })?;
    for _ in 0..3 {
        let snapshot = documents.snapshot(&id).map_err(|e| e.to_string())?;
        let foreign = patch::diff(&row.tip_text, &snapshot.text);
        let rebased = patch::rebase(&inverse, &foreign).ok_or_else(|| OVERLAP.to_owned())?;
        match documents.apply(transaction(task, &id, snapshot.revision, rebased, ConflictPolicy::Reject)) {
            Ok(outcome) => {
                record_revert(store, task, row, &id, &outcome.applied[0], Some(&snapshot));
                return Ok(revert_message(!foreign.is_empty()));
            }
            Err(TransactionError::StaleRevision { .. }) => continue,
            Err(e) => return Err(format!("The revert was refused: {e}")),
        }
    }
    Err("The document kept changing; try reverting again.".into())
}

fn revert_message(rebased: bool) -> String {
    if rebased {
        "Reverted in the open document (not saved); later edits were kept.".into()
    } else {
        "Reverted in the open document (not saved).".into()
    }
}

fn record_revert(
    store: &Store,
    task: &TaskId,
    row: &TaskFileRow,
    id: &DocumentId,
    applied: &exposition_core::document::AppliedEdit,
    before: Option<&exposition_workspace::DocumentSnapshot>,
) {
    let inverse = before
        .filter(|s| s.revision == applied.previous_revision)
        .and_then(|s| patch::invert(&s.text, &applied.patches).ok())
        .unwrap_or_default();
    let change = ChangeRow {
        id: 0,
        tool_use_id: None,
        path: row.path.clone(),
        document: Some(id.0.clone()),
        kind: "revert".into(),
        previous_revision: Some(applied.previous_revision),
        revision: Some(applied.revision),
        patches: applied.patches.clone(),
        inverse,
        created_at_ms: now_ms(),
    };
    if let Err(e) = store.insert_change(task, &change) {
        tracing::error!("failed to record revert evidence: {e}");
    }
}

/// Every version of a created file this task produced: the created content
/// and the text after each of its edits (the user may have saved any of them,
/// or undone back to one).
fn agent_texts(store: &Store, task: &TaskId, row: &TaskFileRow) -> Vec<String> {
    let mut texts = vec![row.tip_text.clone()];
    let Ok(changes) = store.changes(task) else { return texts };
    let mut text: Option<String> = None;
    for change in changes.iter().filter(|c| c.path == row.path) {
        text = match change.kind.as_str() {
            "create" => patch::apply("", &change.patches).ok(),
            "edit" => text.as_deref().and_then(|t| patch::apply(t, &change.patches).ok()),
            _ => continue,
        };
        if let Some(t) = &text
            && !texts.contains(t)
        {
            texts.push(t.clone());
        }
    }
    texts
}

/// Delete a file the task created, when nothing in it (open document or file
/// on disk) is anyone else's work.
fn revert_created(
    store: &Store,
    task: &TaskId,
    documents: &DocumentService,
    workspace: Option<&WorkspaceService>,
    files: &dyn ProjectFiles,
    row: &TaskFileRow,
) -> Result<String, String> {
    if !row.baseline_text.is_empty() {
        return Err("Edits made while the agent was working are in this file, so it was not deleted. Remove the             agent's text by hand, or keep the file."
            .into());
    }
    let ours = agent_texts(store, task, row);
    let is_ours = |text: &str| ours.iter().any(|t| t == text);
    if let Some(id) = documents.find_by_path(&row.path) {
        let snapshot = documents.snapshot(&id).map_err(|e| e.to_string())?;
        if !is_ours(&snapshot.text) {
            return Err("The file was edited after the agent's changes, so it was not deleted. Delete it yourself                 if you no longer want it."
                .into());
        }
    }
    match files.read_text(&row.path) {
        Ok(disk) if is_ours(&disk.text) => {}
        Ok(_) => {
            return Err("The saved file contains text the agent did not write, so it was not deleted. Delete it                 yourself if you no longer want it."
                .into());
        }
        Err(FileError::NotFound(_)) => {
            if let Some(id) = documents.find_by_path(&row.path) {
                let _ = workspace::close_document_in(workspace, documents, &id);
            }
            return Ok("The file was already removed.".into());
        }
        Err(e) => return Err(format!("The file could not be checked: {e}")),
    }
    if let Some(id) = documents.find_by_path(&row.path) {
        workspace::close_document_in(workspace, documents, &id)?;
    }
    files.remove_file(&row.path).map_err(|e| format!("The file could not be deleted: {e}"))?;
    store.insert_change(task, &marker(row, "delete")).map_err(|e| e.to_string())?;
    Ok("Deleted the file the agent created.".into())
}
