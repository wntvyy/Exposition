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
//! happened later and refuses on overlap. Every revert is itself recorded.

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionError, TransactionId,
    TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_workspace::DocumentService;

use crate::model::{FileChangeKind, FileDiff, FileReview, ReviewActionResult, ReviewState, TaskId};
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
                kind: FileChangeKind::Edited,
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
        kind: FileChangeKind::Created,
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

pub fn file_review(row: &TaskFileRow, documents: &DocumentService) -> FileReview {
    let current_revision =
        documents.find_by_path(&row.path).and_then(|id| documents.info(&id).ok()).map(|i| i.revision);
    FileReview {
        path: row.path.clone(),
        kind: row.kind,
        state: row.state,
        change_count: row.change_count,
        entangled: row.entangled,
        tip_revision: row.tip_revision,
        current_revision,
        message: row.message.clone(),
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

pub fn accept_file(store: &Store, task: &TaskId, mut row: TaskFileRow) -> StoreResult<ReviewActionResult> {
    if row.state == ReviewState::Reverted {
        return Ok(ReviewActionResult {
            path: row.path,
            ok: false,
            message: "These changes were already reverted.".into(),
        });
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
    files: &dyn ProjectFiles,
    row: TaskFileRow,
) -> StoreResult<ReviewActionResult> {
    let outcome = match row.state {
        ReviewState::Reverted => Ok("Already reverted.".to_owned()),
        _ if row.entangled => Err("Edits made while the agent was working overlap its changes, so they cannot be \
            separated automatically. Compare the versions and fix the text by hand, or keep the changes."
            .to_owned()),
        _ => match row.kind {
            FileChangeKind::Created => revert_created(store, task, documents, files, &row),
            FileChangeKind::Edited => revert_edited(store, task, documents, files, &row),
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
            Err(TransactionError::Conflict { .. }) => return Err(OVERLAP.into()),
            Err(TransactionError::HistoryUnavailable { .. }) => {}
            Err(e) => return Err(format!("The revert was refused: {e}")),
        }
    }

    // Otherwise rebase against the current text ourselves (the document was
    // reopened, or its history no longer reaches the tip).
    let (id, _) = workspace::open_document(documents, files, &row.path).map_err(|e| match e {
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

fn revert_created(
    store: &Store,
    task: &TaskId,
    documents: &DocumentService,
    files: &dyn ProjectFiles,
    row: &TaskFileRow,
) -> Result<String, String> {
    const CHANGED: &str = "The file changed after the agent created it, so it was not deleted. Delete it yourself \
        if you no longer want it.";
    if let Some(id) = documents.find_by_path(&row.path) {
        let snapshot = documents.snapshot(&id).map_err(|e| e.to_string())?;
        if *snapshot.text != *row.tip_text {
            return Err(CHANGED.into());
        }
    }
    match files.read_text(&row.path) {
        Ok(disk) if disk.text == row.tip_text => {}
        Ok(_) => return Err(CHANGED.into()),
        Err(FileError::NotFound(_)) => {
            if let Some(id) = documents.find_by_path(&row.path) {
                let _ = documents.close(&id);
            }
            return Ok("The file was already removed.".into());
        }
        Err(e) => return Err(format!("The file could not be checked: {e}")),
    }
    if let Some(id) = documents.find_by_path(&row.path) {
        documents.close(&id).map_err(|e| e.to_string())?;
    }
    files.remove_file(&row.path).map_err(|e| format!("The file could not be deleted: {e}"))?;
    store.insert_change(task, &marker(row, "delete")).map_err(|e| e.to_string())?;
    Ok("Deleted the file the agent created.".into())
}
