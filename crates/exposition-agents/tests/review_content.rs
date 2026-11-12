//! Review state and revert follow the file's actual content.
//!
//! Undo history is shared by every edit origin, so the user's Ctrl+Z can
//! remove an agent change, a redo brings it back, and undoing a revert
//! restores it. Review must say what is in the file, and Revert must neither
//! report a false overlap nor touch text that is not the agent's. Created
//! files are deleted only when every version of them (open document, file on
//! disk) is the agent's own text. The scripted provider (development only)
//! stands in for the model; the document service is real.
#![cfg(debug_assertions)]

use std::sync::Arc;
use std::time::Duration;

use exposition_agents::credentials::MemoryStore;
use exposition_agents::tools::services::UnavailableServices;
use exposition_agents::*;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tempfile::TempDir;

const MAIN: &str = "= Notes\nThe cat sat on the mat.\n";
const EDITED: &str = "= Notes\nThe dog sat on the mat.\n";

struct Harness {
    service: AgentService,
    documents: Arc<DocumentService>,
    project: TempDir,
    _data: TempDir,
}

fn harness() -> Harness {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.typ"), MAIN).unwrap();
    let data = tempfile::tempdir().unwrap();
    let documents = Arc::new(DocumentService::new());
    let service = AgentService::new(AgentConfig {
        database: Some(data.path().join("agents.sqlite3")),
        documents: Arc::clone(&documents),
        workspace: None,
        credentials: Arc::new(MemoryStore::default()),
        services: Arc::new(UnavailableServices),
        runtime: tokio::runtime::Handle::current(),
        allow_scripted: true,
        anthropic_base_url: None,
        retry: None,
        graph: None,
    })
    .unwrap();
    service
        .update_settings(ProviderSettings { provider: ProviderKind::Scripted, ..ProviderSettings::default() })
        .unwrap();
    Harness { service, documents, project, _data: data }
}

fn p(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

async fn run(h: &Harness, script: Value) -> TaskId {
    h.service.load_script(script).unwrap();
    let task = h
        .service
        .create_task(CreateTaskRequest {
            prompt: "Improve the notes".into(),
            workspace_root: h.project.path().display().to_string(),
            scope: TaskScope::Workspace,
            permission: PermissionProfile::AutoApply,
            execution: ExecutionMode::Disabled,
            consent: None,
            budgets: None,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), h.service.join(&task.id)).await.expect("task finished");
    assert_eq!(h.service.task(&task.id).unwrap().status, TaskStatus::Completed);
    task.id
}

/// The agent replaces "cat" with "dog" in main.typ.
async fn agent_edit(h: &Harness) -> TaskId {
    run(
        h,
        json!({"turns": [
            {"steps": [{"toolUse": {"id": "r", "name": "read_file", "input": {"path": "main.typ"}}}]},
            {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {"path": "main.typ",
                "base_revision": "$ref:r/revision", "edits": [{"old_text": "cat", "new_text": "dog"}]}}}]},
            {"steps": [{"text": "done"}]}
        ]}),
    )
    .await
}

fn doc(h: &Harness, path: &str) -> DocumentId {
    h.documents.find_by_path(&p(path)).expect("document open")
}

fn text(h: &Harness, path: &str) -> String {
    h.documents.state(&doc(h, path)).unwrap().text
}

/// A user edit typed in the editor (its own undo group).
fn type_text(h: &Harness, path: &str, patch: TextPatch) {
    let id = doc(h, path);
    let base = h.documents.info(&id).unwrap().revision;
    h.documents
        .apply(TransactionRequest {
            id: TransactionId(format!("user-{}", uuid::Uuid::new_v4())),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit { document: id, base_revision: base, patches: vec![patch] }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
}

fn undo(h: &Harness, path: &str) {
    h.documents.undo(&doc(h, path), EditOrigin::Source).unwrap();
}

fn redo(h: &Harness, path: &str) {
    h.documents.redo(&doc(h, path), EditOrigin::Source).unwrap();
}

fn state(h: &Harness, task: &TaskId, path: &str) -> (ReviewState, Option<String>) {
    let review = h.service.review(task).unwrap();
    let file = review.files.into_iter().find(|f| f.path == p(path)).expect("file in review");
    (file.state, file.message)
}

fn pending(h: &Harness, task: &TaskId) -> u32 {
    h.service.task(task).unwrap().pending_review
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undone_change_is_not_pending_and_revert_reports_nothing_to_do() {
    let h = harness();
    let task = agent_edit(&h).await;
    assert_eq!((state(&h, &task, "main.typ").0, pending(&h, &task)), (ReviewState::Pending, 1));

    undo(&h, "main.typ");
    assert_eq!(text(&h, "main.typ"), MAIN);
    let (s, message) = state(&h, &task, "main.typ");
    assert_eq!(s, ReviewState::Undone);
    assert!(message.unwrap().contains("no longer in the file"));
    assert_eq!(pending(&h, &task), 0, "an undone change does not count as pending review");
    // Keep has nothing to keep; Revert has nothing to do and changes nothing.
    let kept = h.service.accept(&task, Some(&p("main.typ"))).unwrap();
    assert!(!kept[0].ok && kept[0].message.contains("nothing to keep"), "{kept:?}");
    assert!(h.service.accept(&task, None).unwrap().is_empty(), "Keep all skips undone files");
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(reverted[0].ok && reverted[0].message.contains("already undone"), "{reverted:?}");
    assert_eq!(text(&h, "main.typ"), MAIN);

    // Redo brings the change back: pending again, and Revert removes it even
    // though the history since the agent's edit (undo, redo) touches its text.
    redo(&h, "main.typ");
    assert_eq!(text(&h, "main.typ"), EDITED);
    assert_eq!((state(&h, &task, "main.typ").0, pending(&h, &task)), (ReviewState::Pending, 1));
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(reverted[0].ok, "{reverted:?}");
    assert_eq!(text(&h, "main.typ"), MAIN);
    assert_eq!(state(&h, &task, "main.typ").0, ReviewState::Reverted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undo_plus_edits_elsewhere_is_undone_and_revert_keeps_the_users_text() {
    let h = harness();
    let task = agent_edit(&h).await;
    undo(&h, "main.typ");
    type_text(&h, "main.typ", TextPatch::replace(2, 7, "Diary"));
    let expected = "= Diary\nThe cat sat on the mat.\n";
    assert_eq!(text(&h, "main.typ"), expected);
    assert_eq!(state(&h, &task, "main.typ").0, ReviewState::Undone);
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(reverted[0].ok && !reverted[0].message.contains("overlap"), "{reverted:?}");
    assert_eq!(text(&h, "main.typ"), expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_elsewhere_keep_the_change_pending_and_revert_keeps_them() {
    let h = harness();
    let task = agent_edit(&h).await;
    type_text(&h, "main.typ", TextPatch::replace(27, 30, "rug"));
    assert_eq!(state(&h, &task, "main.typ").0, ReviewState::Pending);
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(reverted[0].ok && reverted[0].message.contains("later edits were kept"), "{reverted:?}");
    assert_eq!(text(&h, "main.typ"), "= Notes\nThe cat sat on the rug.\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edits_on_the_same_text_refuse_unless_they_restore_it() {
    let h = harness();
    let task = agent_edit(&h).await;
    // The user rewrites the agent's word: a real overlap, refused, text untouched.
    type_text(&h, "main.typ", TextPatch::replace(12, 15, "fox"));
    assert_eq!(state(&h, &task, "main.typ").0, ReviewState::Pending);
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(!reverted[0].ok && reverted[0].message.contains("overlap"), "{reverted:?}");
    assert_eq!(text(&h, "main.typ"), "= Notes\nThe fox sat on the mat.\n");
    // Typing the original word back by hand removes the agent's change.
    type_text(&h, "main.typ", TextPatch::replace(12, 15, "cat"));
    assert_eq!(state(&h, &task, "main.typ").0, ReviewState::Undone);
    assert!(h.service.revert(&task, None).unwrap()[0].ok);
    assert_eq!(text(&h, "main.typ"), MAIN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undoing_a_revert_makes_the_change_pending_again() {
    let h = harness();
    let task = agent_edit(&h).await;
    assert!(h.service.revert(&task, None).unwrap()[0].ok);
    assert_eq!(state(&h, &task, "main.typ").0, ReviewState::Reverted);
    undo(&h, "main.typ"); // the user's Ctrl+Z undoes the revert
    assert_eq!(text(&h, "main.typ"), EDITED);
    let (s, message) = state(&h, &task, "main.typ");
    assert_eq!(s, ReviewState::Pending);
    assert!(message.unwrap().contains("revert was undone"));
    assert_eq!(pending(&h, &task), 1);
    assert!(h.service.revert(&task, None).unwrap()[0].ok);
    assert_eq!(text(&h, "main.typ"), MAIN);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discarding_the_unsaved_change_counts_as_undone() {
    let h = harness();
    let task = agent_edit(&h).await;
    // Close without saving: the agent's edit only lived in the buffer.
    h.documents.close(&doc(&h, "main.typ")).unwrap();
    assert_eq!(state(&h, &task, "main.typ").0, ReviewState::Undone);
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(reverted[0].ok && reverted[0].message.contains("already undone"), "{reverted:?}");
    assert_eq!(std::fs::read_to_string(h.project.path().join("main.typ")).unwrap(), MAIN);
}

/// The agent creates notes.typ and edits it ("Alpha" → "Beta").
async fn agent_create_and_edit(h: &Harness) -> TaskId {
    run(
        h,
        json!({"turns": [
            {"steps": [{"toolUse": {"id": "c", "name": "create_file",
                "input": {"path": "notes.typ", "content": "= Draft\nAlpha\n"}}}]},
            {"steps": [{"toolUse": {"id": "r", "name": "read_file", "input": {"path": "notes.typ"}}}]},
            {"steps": [{"toolUse": {"id": "e", "name": "edit_document", "input": {"path": "notes.typ",
                "base_revision": "$ref:r/revision", "edits": [{"old_text": "Alpha", "new_text": "Beta"}]}}}]},
            {"steps": [{"text": "done"}]}
        ]}),
    )
    .await
}

fn exists(h: &Harness, path: &str) -> bool {
    h.project.path().join(path).exists()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn created_file_is_deleted_whichever_agent_version_is_open_or_saved() {
    // Unsaved edit (disk has the created content): deleted.
    let h = harness();
    let task = agent_create_and_edit(&h).await;
    assert!(h.service.revert(&task, None).unwrap().iter().all(|r| r.ok));
    assert!(!exists(&h, "notes.typ"));
    assert!(h.documents.find_by_path(&p("notes.typ")).is_none(), "its document is closed");

    // The user undid the agent's edit (buffer = created content): deleted.
    let h = harness();
    let task = agent_create_and_edit(&h).await;
    undo(&h, "notes.typ");
    assert_eq!(text(&h, "notes.typ"), "= Draft\nAlpha\n");
    assert!(h.service.revert(&task, None).unwrap().iter().all(|r| r.ok));
    assert!(!exists(&h, "notes.typ"));

    // The user saved the agent's latest version: deleted.
    let h = harness();
    let task = agent_create_and_edit(&h).await;
    std::fs::write(h.project.path().join("notes.typ"), "= Draft\nBeta\n").unwrap();
    assert!(h.service.revert(&task, None).unwrap().iter().all(|r| r.ok));
    assert!(!exists(&h, "notes.typ"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn created_file_with_someone_elses_text_is_kept() {
    // Typed into the open document after the agent: kept.
    let h = harness();
    let task = agent_create_and_edit(&h).await;
    type_text(&h, "notes.typ", TextPatch::insert(0, "% mine\n"));
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(!reverted[0].ok && reverted[0].message.contains("edited after the agent's changes"), "{reverted:?}");
    assert!(exists(&h, "notes.typ"));
    assert_eq!(state(&h, &task, "notes.typ").0, ReviewState::Pending);

    // Saved text the agent never wrote (buffer is the agent's): kept.
    let h = harness();
    let task = agent_create_and_edit(&h).await;
    std::fs::write(h.project.path().join("notes.typ"), "= Draft\nMy own text\n").unwrap();
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(!reverted[0].ok && reverted[0].message.contains("did not write"), "{reverted:?}");
    assert_eq!(std::fs::read_to_string(h.project.path().join("notes.typ")).unwrap(), "= Draft\nMy own text\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn created_file_removed_by_the_user_is_undone() {
    let h = harness();
    let task = agent_create_and_edit(&h).await;
    h.documents.close(&doc(&h, "notes.typ")).unwrap();
    std::fs::remove_file(h.project.path().join("notes.typ")).unwrap();
    assert_eq!(state(&h, &task, "notes.typ").0, ReviewState::Undone);
    assert_eq!(pending(&h, &task), 0);
    let reverted = h.service.revert(&task, None).unwrap();
    assert!(reverted[0].ok && reverted[0].message.contains("no longer exists"), "{reverted:?}");
}

#[test]
fn presence_is_decided_from_content() {
    use exposition_agents::review::{Presence, presence};
    let (base, tip) = ("one two three four", "one TWO three four");
    assert_eq!(presence(base, tip, tip), Presence::Present);
    assert_eq!(presence(base, tip, base), Presence::Absent);
    assert_eq!(presence(base, tip, "one TWO three FOUR"), Presence::Present, "change kept, edit elsewhere");
    assert_eq!(presence(base, tip, "one two three FOUR"), Presence::Absent, "change gone, edit elsewhere");
    assert_eq!(presence(base, tip, "one Two three four"), Presence::Overlapped, "same word rewritten");
}
