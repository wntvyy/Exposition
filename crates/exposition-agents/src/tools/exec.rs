//! Tool execution: gating (scope, permission profile, execution mode,
//! consent, service availability) and the tool implementations.
//!
//! Agent document changes go through the document service as transactions
//! with `EditOrigin::Agent` and `ConflictPolicy::RebaseDisjoint`; reads prefer
//! open buffers over disk. Nothing here bypasses an open buffer.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionError, TransactionId,
    TransactionRequest, UndoBehavior,
};
use exposition_core::job::ExecutionPolicy;
use exposition_core::path::WorkspacePath;
use exposition_core::text::byte_range_to_utf16;
use exposition_workspace::DocumentService;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::services::{
    BuildRequest, CommandRequest, LeanRequest, PythonRequest, ServiceError, ServiceKind, ToolServices,
};
use super::{CreateFileInput, EditDocumentInput, ListFilesInput, ReadFileInput, RunCommandInput, ToolCall};
use crate::model::{ContextConsent, ExecutionMode, PermissionProfile, TaskId, TaskScope, TaskSpec};
use crate::review::{self, EditRecord};
use crate::store::Store;
use crate::workspace::{self, FileError, ProjectFiles};

/// Largest file text sent to the model in one read.
pub const MAX_READ_BYTES: usize = 512 * 1024;
const LIST_LIMIT: usize = 500;

/// Document texts the agent has seen, by (document, revision). `edit_document`
/// resolves `old_text` against the exact revision the agent read.
#[derive(Debug, Default)]
pub struct SeenTexts {
    texts: HashMap<(String, u64), Arc<str>>,
    order: VecDeque<(String, u64)>,
}

impl SeenTexts {
    const CAPACITY: usize = 48;

    pub fn insert(&mut self, document: &DocumentId, revision: u64, text: Arc<str>) {
        let key = (document.0.clone(), revision);
        if self.texts.insert(key.clone(), text).is_none() {
            self.order.push_back(key);
            while self.order.len() > Self::CAPACITY {
                if let Some(old) = self.order.pop_front() {
                    self.texts.remove(&old);
                }
            }
        }
    }

    pub fn get(&self, document: &DocumentId, revision: u64) -> Option<Arc<str>> {
        self.texts.get(&(document.0.clone(), revision)).cloned()
    }
}

pub struct ToolEnv<'a> {
    pub task_id: &'a TaskId,
    pub spec: &'a TaskSpec,
    pub files: &'a dyn ProjectFiles,
    pub documents: &'a DocumentService,
    pub services: &'a dyn ToolServices,
    pub store: &'a Store,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: Value,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: Value) -> Self {
        Self { content, is_error: false }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self { content: json!({"error": message.into()}), is_error: true }
    }

    pub fn error_with(message: impl Into<String>, extra: Value) -> Self {
        let mut content = json!({"error": message.into()});
        if let (Some(obj), Value::Object(more)) = (content.as_object_mut(), extra) {
            obj.extend(more);
        }
        Self { content, is_error: true }
    }

    /// Text for the `tool_result` block.
    pub fn text(&self) -> String {
        serde_json::to_string(&self.content).unwrap_or_else(|_| "{\"error\":\"unserializable result\"}".into())
    }
}

/// Decision before running a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Allowed { policy: Option<ExecutionPolicy> },
    NeedsApproval { title: String, detail: String, policy: Option<ExecutionPolicy> },
    Refused(ToolOutput),
}

fn parse_path(raw: &str) -> Result<WorkspacePath, ToolOutput> {
    WorkspacePath::parse(raw).map_err(|e| ToolOutput::error(format!("invalid path {raw:?}: {e}")))
}

fn parse_dir(raw: &str) -> Result<Option<WorkspacePath>, ToolOutput> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == "./" { Ok(None) } else { parse_path(trimmed).map(Some) }
}

fn out_of_scope(path: &WorkspacePath, scope: &TaskScope) -> ToolOutput {
    ToolOutput::error(format!(
        "{path} is outside this task's scope ({}). The application refused the access; work within the scope.",
        scope.describe()
    ))
}

fn read_only() -> ToolOutput {
    ToolOutput::error("This task is read-only: the user did not allow document changes. Describe the change instead.")
}

/// Whether listing `dir` can show anything inside the scope.
fn dir_visible(dir: Option<&WorkspacePath>, scope: &TaskScope) -> bool {
    let Some(dir) = dir else { return true };
    if scope.allows(dir) {
        return true;
    }
    scoped_paths(scope).iter().any(|p| p.as_str().starts_with(&format!("{}/", dir.as_str())))
}

fn scoped_paths(scope: &TaskScope) -> Vec<WorkspacePath> {
    match scope {
        TaskScope::Workspace => Vec::new(),
        TaskScope::Files { paths } => paths.clone(),
        TaskScope::Document { path } | TaskScope::Selection { path, .. } => vec![path.clone()],
    }
}

fn entry_visible(path: &str, scope: &TaskScope) -> bool {
    let Ok(parsed) = WorkspacePath::parse(path) else { return false };
    scope.allows(&parsed) || scoped_paths(scope).iter().any(|p| p.as_str().starts_with(&format!("{path}/")))
}

fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} more bytes)", &text[..end], text.len() - end)
}

/// Decide whether `call` may run, and whether it needs the user's approval.
pub fn gate(call: &ToolCall, env: &ToolEnv<'_>) -> Gate {
    let spec = env.spec;
    match call {
        ToolCall::ListFiles(input) => match parse_dir(&input.path) {
            Ok(dir) if dir_visible(dir.as_ref(), &spec.scope) => Gate::Allowed { policy: None },
            Ok(Some(dir)) => Gate::Refused(out_of_scope(&dir, &spec.scope)),
            Ok(None) => Gate::Allowed { policy: None },
            Err(e) => Gate::Refused(e),
        },
        ToolCall::ReadFile(input) => {
            let path = match parse_path(&input.path) {
                Ok(p) => p,
                Err(e) => return Gate::Refused(e),
            };
            if !spec.scope.allows(&path) {
                return Gate::Refused(out_of_scope(&path, &spec.scope));
            }
            if spec.consent == ContextConsent::MetadataOnly {
                return Gate::Refused(ToolOutput::error(
                    "The user's context consent for this task allows only file names and metadata to be sent to \
                     the model provider, so file content cannot be read.",
                ));
            }
            Gate::Allowed { policy: None }
        }
        ToolCall::EditDocument(input) => {
            if spec.permission == PermissionProfile::ReadOnly {
                return Gate::Refused(read_only());
            }
            let path = match parse_path(&input.path) {
                Ok(p) => p,
                Err(e) => return Gate::Refused(e),
            };
            if !spec.scope.allows(&path) {
                return Gate::Refused(out_of_scope(&path, &spec.scope));
            }
            if spec.permission == PermissionProfile::ReviewEdits {
                let mut detail = format!("Edit {path} (based on revision {}):\n", input.base_revision);
                for (i, edit) in input.edits.iter().enumerate() {
                    detail.push_str(&format!(
                        "\n#{}\n- {}\n+ {}\n",
                        i + 1,
                        truncate(&edit.old_text, 1500).replace('\n', "\n  "),
                        truncate(&edit.new_text, 1500).replace('\n', "\n  ")
                    ));
                }
                return Gate::NeedsApproval { title: format!("Apply edits to {path}"), detail, policy: None };
            }
            Gate::Allowed { policy: None }
        }
        ToolCall::CreateFile(input) => {
            if spec.permission == PermissionProfile::ReadOnly {
                return Gate::Refused(read_only());
            }
            let path = match parse_path(&input.path) {
                Ok(p) => p,
                Err(e) => return Gate::Refused(e),
            };
            if !spec.scope.allows(&path) {
                return Gate::Refused(out_of_scope(&path, &spec.scope));
            }
            if spec.permission == PermissionProfile::ReviewEdits {
                return Gate::NeedsApproval {
                    title: format!("Create {path}"),
                    detail: format!("Create {path} with content:\n\n{}", truncate(&input.content, 4000)),
                    policy: None,
                };
            }
            Gate::Allowed { policy: None }
        }
        ToolCall::BuildDocument(input) => {
            let path = match parse_path(&input.entry) {
                Ok(p) => p,
                Err(e) => return Gate::Refused(e),
            };
            if !spec.scope.allows(&path) {
                return Gate::Refused(out_of_scope(&path, &spec.scope));
            }
            if !env.services.available(ServiceKind::Typesetting) {
                return Gate::Refused(ToolOutput::error(
                    ServiceError::NotAvailable(ServiceKind::Typesetting).to_string(),
                ));
            }
            if spec.execution == ExecutionMode::Disabled {
                return Gate::Refused(execution_disabled());
            }
            // The typesetting service applies its own (shell-escape-free) policy.
            Gate::Allowed { policy: None }
        }
        ToolCall::RunPython(input) => process_gate(env, ServiceKind::Research, || {
            (
                "Run Python code".into(),
                format!("Run in this task's separate research session:\n\n{}", truncate(&input.code, 4000)),
            )
        }),
        ToolCall::CheckLean(input) => {
            let path = match parse_path(&input.path) {
                Ok(p) => p,
                Err(e) => return Gate::Refused(e),
            };
            if !spec.scope.allows(&path) {
                return Gate::Refused(out_of_scope(&path, &spec.scope));
            }
            process_gate(env, ServiceKind::Lean, || {
                (format!("Check {path} with Lean"), format!("lake/lean check of {path}"))
            })
        }
        ToolCall::RunCommand(input) => {
            if spec.scope != TaskScope::Workspace {
                return Gate::Refused(ToolOutput::error(
                    "Commands can affect any file, so they are only available for tasks scoped to the whole project.",
                ));
            }
            if let Err(e) = parse_dir(&input.working_directory) {
                return Gate::Refused(e);
            }
            process_gate(env, ServiceKind::Execution, || {
                let wd = if input.working_directory.is_empty() { "." } else { input.working_directory.as_str() };
                (
                    format!("Run {}", input.program),
                    format!("Program: {}\nArguments: {:?}\nWorking folder: {wd}", input.program, input.args),
                )
            })
        }
    }
}

fn execution_disabled() -> ToolOutput {
    ToolOutput::error("Process execution is disabled for this task by the user.")
}

fn process_gate(env: &ToolEnv<'_>, kind: ServiceKind, describe: impl FnOnce() -> (String, String)) -> Gate {
    let spec = env.spec;
    if spec.permission == PermissionProfile::ReadOnly || spec.execution == ExecutionMode::Disabled {
        return Gate::Refused(execution_disabled());
    }
    if !env.services.available(kind) {
        return Gate::Refused(ToolOutput::error(ServiceError::NotAvailable(kind).to_string()));
    }
    match spec.execution {
        ExecutionMode::Disabled => Gate::Refused(execution_disabled()),
        ExecutionMode::Restricted => match env.services.restricted_backend() {
            Some(backend) => Gate::Allowed { policy: Some(ExecutionPolicy::Restricted { backend }) },
            None => Gate::Refused(ToolOutput::error(
                "This task only allows unattended execution inside the restricted (container) backend, which is not \
                 available. Exposition never falls back to running it natively; the user can choose per-command \
                 approval instead.",
            )),
        },
        ExecutionMode::ApproveEach => {
            let (title, detail) = describe();
            Gate::NeedsApproval {
                title,
                detail: format!("{detail}\n\nRuns natively with your account's privileges."),
                policy: Some(ExecutionPolicy::TrustedNative),
            }
        }
    }
}

/// Run an allowed (and, if needed, approved) call.
pub async fn run(
    call: ToolCall,
    env: &ToolEnv<'_>,
    seen: &mut SeenTexts,
    tool_use_id: &str,
    policy: Option<ExecutionPolicy>,
    cancel: CancellationToken,
) -> ToolOutput {
    let result = match call {
        ToolCall::ListFiles(input) => list_files(env, input),
        ToolCall::ReadFile(input) => read_file(env, seen, input),
        ToolCall::EditDocument(input) => edit_document(env, seen, tool_use_id, input),
        ToolCall::CreateFile(input) => create_file(env, seen, tool_use_id, input),
        ToolCall::BuildDocument(input) => match parse_path(&input.entry) {
            Ok(entry) => service_result(
                env.services.build_document(BuildRequest { entry, task_id: env.task_id.0.clone() }, cancel).await,
            ),
            Err(e) => Err(e),
        },
        ToolCall::RunPython(input) => {
            let Some(policy) = policy else { return ToolOutput::error("missing execution policy") };
            service_result(
                env.services
                    .run_python(
                        PythonRequest { code: input.code, policy, session: format!("agent-{}", env.task_id.0) },
                        cancel,
                    )
                    .await,
            )
        }
        ToolCall::CheckLean(input) => {
            let Some(policy) = policy else { return ToolOutput::error("missing execution policy") };
            match parse_path(&input.path) {
                Ok(path) => service_result(env.services.check_lean(LeanRequest { path, policy }, cancel).await),
                Err(e) => Err(e),
            }
        }
        ToolCall::RunCommand(input) => run_command(env, input, policy, cancel).await,
    };
    result.unwrap_or_else(|e| e)
}

fn service_result(result: Result<Value, ServiceError>) -> Result<ToolOutput, ToolOutput> {
    match result {
        Ok(value) => Ok(ToolOutput::ok(value)),
        Err(e) => Err(ToolOutput::error(e.to_string())),
    }
}

async fn run_command(
    env: &ToolEnv<'_>,
    input: RunCommandInput,
    policy: Option<ExecutionPolicy>,
    cancel: CancellationToken,
) -> Result<ToolOutput, ToolOutput> {
    let Some(policy) = policy else { return Err(ToolOutput::error("missing execution policy")) };
    let mut working_directory = env.files.root().to_path_buf();
    if let Some(dir) = parse_dir(&input.working_directory)? {
        for part in dir.components() {
            working_directory.push(part);
        }
    }
    service_result(
        env.services
            .run_command(
                CommandRequest {
                    program: input.program,
                    args: input.args,
                    working_directory,
                    policy,
                    task_id: env.task_id.0.clone(),
                },
                cancel,
            )
            .await,
    )
}

fn file_error(e: FileError) -> ToolOutput {
    ToolOutput::error(e.to_string())
}

fn list_files(env: &ToolEnv<'_>, input: ListFilesInput) -> Result<ToolOutput, ToolOutput> {
    let dir = parse_dir(&input.path)?;
    let listing = env.files.list(dir.as_ref(), input.recursive, LIST_LIMIT).map_err(file_error)?;
    let open: HashMap<String, (u64, bool)> = env
        .documents
        .list()
        .into_iter()
        .filter_map(|info| {
            let unsaved = info.saved_revision != Some(info.revision);
            info.path.map(|p| (p.as_str().to_owned(), (info.revision, unsaved)))
        })
        .collect();
    let entries: Vec<Value> = listing
        .entries
        .into_iter()
        .filter(|e| entry_visible(&e.path, &env.spec.scope))
        .map(|e| {
            let mut value = serde_json::to_value(&e).unwrap_or(Value::Null);
            if let Some((revision, unsaved)) = open.get(&e.path) {
                value["openRevision"] = json!(revision);
                value["unsavedChanges"] = json!(unsaved);
            }
            value
        })
        .collect();
    Ok(ToolOutput::ok(json!({"entries": entries, "truncated": listing.truncated})))
}

fn read_file(env: &ToolEnv<'_>, seen: &mut SeenTexts, input: ReadFileInput) -> Result<ToolOutput, ToolOutput> {
    let path = parse_path(&input.path)?;
    let (id, _) = workspace::open_document(env.documents, env.files, &path).map_err(file_error)?;
    let snapshot = env.documents.snapshot(&id).map_err(|e| ToolOutput::error(e.to_string()))?;
    let info = env.documents.info(&id).map_err(|e| ToolOutput::error(e.to_string()))?;
    if snapshot.text.len() > MAX_READ_BYTES {
        return Err(ToolOutput::error(format!(
            "{path} is {} bytes, more than the {MAX_READ_BYTES}-byte limit for sending a file to the model.",
            snapshot.text.len()
        )));
    }
    seen.insert(&id, snapshot.revision, Arc::clone(&snapshot.text));
    Ok(ToolOutput::ok(json!({
        "path": path.as_str(),
        "revision": snapshot.revision,
        "unsavedChanges": info.saved_revision != Some(info.revision),
        "text": &*snapshot.text,
    })))
}

/// Locate each `old_text` exactly once in `base` and build sorted patches.
fn resolve_edits(base: &str, input: &EditDocumentInput) -> Result<Vec<TextPatch>, ToolOutput> {
    let mut ranges: Vec<(usize, usize, &str, usize)> = Vec::new();
    for (i, edit) in input.edits.iter().enumerate() {
        let n = i + 1;
        if edit.old_text.is_empty() {
            if base.is_empty() && input.edits.len() == 1 {
                ranges.push((0, 0, &edit.new_text, n));
                continue;
            }
            return Err(ToolOutput::error(format!(
                "edit #{n}: old_text is empty; it may only be empty to fill an empty file. Include the neighbouring \
                 text you want to keep in both old_text and new_text."
            )));
        }
        let mut matches = base.match_indices(edit.old_text.as_str());
        let Some((start, _)) = matches.next() else {
            return Err(ToolOutput::error(format!(
                "edit #{n}: old_text was not found in revision {} of {}. Copy it exactly from read_file output \
                 (including whitespace and line breaks).",
                input.base_revision, input.path
            )));
        };
        let extra = matches.count();
        if extra > 0 {
            return Err(ToolOutput::error(format!(
                "edit #{n}: old_text occurs {} times; include more surrounding text so it is unique.",
                extra + 1
            )));
        }
        ranges.push((start, start + edit.old_text.len(), &edit.new_text, n));
    }
    ranges.sort_by_key(|r| (r.0, r.1));
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(ToolOutput::error(format!("edits #{} and #{} overlap; combine them.", pair[0].3, pair[1].3)));
        }
    }
    ranges
        .into_iter()
        .map(|(start, end, text, _)| {
            let range = byte_range_to_utf16(base, start..end).map_err(|e| ToolOutput::error(e.to_string()))?;
            Ok(TextPatch { range, text: text.to_owned() })
        })
        .collect()
}

fn edit_document(
    env: &ToolEnv<'_>,
    seen: &mut SeenTexts,
    tool_use_id: &str,
    input: EditDocumentInput,
) -> Result<ToolOutput, ToolOutput> {
    let path = parse_path(&input.path)?;
    let Some(id) = env.documents.find_by_path(&path) else {
        return Err(ToolOutput::error(format!(
            "{path} is not open. Call read_file first and edit the revision it returns."
        )));
    };
    let Some(base) = seen.get(&id, input.base_revision) else {
        let current = env.documents.info(&id).map(|i| i.revision).unwrap_or_default();
        return Err(ToolOutput::error_with(
            format!(
                "Revision {} of {path} is not a revision you have read. Call read_file and edit the revision it \
                 returns.",
                input.base_revision
            ),
            json!({"currentRevision": current}),
        ));
    };
    let patches = resolve_edits(&base, &input)?;
    if patches.iter().all(|p| {
        exposition_core::text::utf16_range_to_bytes(&base, p.range).map(|b| base[b] == p.text).unwrap_or(false)
    }) {
        return Err(ToolOutput::error("The edits do not change anything."));
    }
    let before_apply = env.documents.snapshot(&id).map_err(|e| ToolOutput::error(e.to_string()))?;
    let request = TransactionRequest {
        id: TransactionId(format!("agent-{}-{tool_use_id}", env.task_id.0)),
        origin: EditOrigin::Agent { task_id: env.task_id.0.clone() },
        edits: vec![DocumentEdit { document: id.clone(), base_revision: input.base_revision, patches }],
        undo: UndoBehavior::NewGroup,
        conflict: ConflictPolicy::RebaseDisjoint,
        label: Some(format!("Agent edit to {path}")),
        selection: None,
    };
    let outcome = match env.documents.apply(request) {
        Ok(outcome) => outcome,
        Err(e) => {
            let current = env.documents.info(&id).map(|i| i.revision).unwrap_or_default();
            let message = match e {
                TransactionError::Conflict { .. } | TransactionError::HistoryUnavailable { .. } => format!(
                    "Your edit overlaps changes made to {path} after revision {} (it is now at revision {current}), \
                     most likely by the user. Nothing was changed. Read the file again and redo the edit on the new \
                     text; do not overwrite the user's work.",
                    input.base_revision
                ),
                TransactionError::ReadOnly { reason, .. } => format!("{path} is read-only: {reason}"),
                other => format!("The edit was rejected: {other}"),
            };
            return Err(ToolOutput::error_with(message, json!({"currentRevision": current})));
        }
    };
    let applied = &outcome.applied[0];
    let before = (before_apply.revision == applied.previous_revision).then(|| Arc::clone(&before_apply.text));
    let after: Option<Arc<str>> = match &before {
        Some(text) => crate::patch::apply(text, &applied.patches).ok().map(Arc::from),
        None => env.documents.snapshot(&id).ok().filter(|s| s.revision == applied.revision).map(|s| s.text),
    };
    if let Some(after) = &after {
        seen.insert(&id, applied.revision, Arc::clone(after));
    }
    review::record_edit(
        env.store,
        env.task_id,
        &EditRecord {
            tool_use_id,
            path: &path,
            document: &id,
            previous_revision: applied.previous_revision,
            revision: applied.revision,
            patches: &applied.patches,
            before: before.as_deref(),
            after: after.as_deref(),
        },
    )
    .map_err(|e| ToolOutput::error(format!("The edit was applied but could not be recorded for review: {e}")))?;
    let mut result = json!({
        "path": path.as_str(),
        "revision": applied.revision,
        "previousRevision": applied.previous_revision,
        "note": "Applied to the open document (not saved). The user reviews agent changes.",
    });
    if applied.rebased {
        result["rebased"] = json!(true);
        result["note"] = json!(
            "Applied over concurrent edits made since your base revision (they did not overlap). Read the file \
             again before further edits."
        );
    }
    Ok(ToolOutput::ok(result))
}

fn create_file(
    env: &ToolEnv<'_>,
    seen: &mut SeenTexts,
    tool_use_id: &str,
    input: CreateFileInput,
) -> Result<ToolOutput, ToolOutput> {
    let path = parse_path(&input.path)?;
    if env.documents.find_by_path(&path).is_some() || env.files.exists(&path).map_err(file_error)? {
        return Err(ToolOutput::error(format!("{path} already exists; use read_file and edit_document to change it.")));
    }
    env.files.create_text(&path, &input.content).map_err(file_error)?;
    let opened = workspace::open_document(env.documents, env.files, &path).ok();
    let document = opened.as_ref().and_then(|(id, _)| env.documents.snapshot(id).ok().map(|s| (id.clone(), s)));
    if let Some((id, snapshot)) = &document {
        seen.insert(id, snapshot.revision, Arc::clone(&snapshot.text));
    }
    review::record_create(
        env.store,
        env.task_id,
        tool_use_id,
        &path,
        &input.content,
        document.as_ref().map(|(id, s)| (id, s.revision)),
    )
    .map_err(|e| ToolOutput::error(format!("The file was created but could not be recorded for review: {e}")))?;
    let mut result = json!({"path": path.as_str(), "created": true, "bytes": input.content.len()});
    if let Some((_, snapshot)) = &document {
        result["revision"] = json!(snapshot.revision);
    }
    Ok(ToolOutput::ok(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::EditInput;

    fn input(edits: &[(&str, &str)]) -> EditDocumentInput {
        EditDocumentInput {
            path: "main.typ".into(),
            base_revision: 1,
            edits: edits.iter().map(|(o, n)| EditInput { old_text: (*o).into(), new_text: (*n).into() }).collect(),
        }
    }

    #[test]
    fn resolves_unique_snippets_in_utf16() {
        let base = "α = 1\nβ = 2\n";
        let patches = resolve_edits(base, &input(&[("β = 2", "β = 3"), ("α", "a")])).unwrap();
        assert_eq!(patches, vec![TextPatch::replace(0, 1, "a"), TextPatch::replace(6, 11, "β = 3")]);
        assert!(resolve_edits("x x", &input(&[("x", "y")])).unwrap_err().text().contains("occurs 2 times"));
        assert!(resolve_edits("abc", &input(&[("zzz", "y")])).unwrap_err().text().contains("not found"));
        assert!(
            resolve_edits("abcdef", &input(&[("abcd", "1"), ("cdef", "2")])).unwrap_err().text().contains("overlap")
        );
        assert_eq!(resolve_edits("", &input(&[("", "hello")])).unwrap(), vec![TextPatch::insert(0, "hello")]);
        assert!(resolve_edits("abc", &input(&[("", "hello")])).is_err());
    }
}
