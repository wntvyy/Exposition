//! Tool targets resolved through the project graph.
//!
//! File tools keep their argument shapes; the application resolves each path
//! a call acts on to its graph resource identity:
//! - before the call (in the central gate, and again after an approval), the
//!   target's graph state must be usable: a path whose graph entry waits for
//!   reconciliation (`agents.graphPending`) or whose identity is ambiguous
//!   (`agents.graphAmbiguous`) pauses the call — briefly waiting for
//!   reconciliation first ([`settle`]); unrelated graph changes never do;
//! - while the call runs, qualifying files are admitted as resources (the
//!   inclusion policy decides; excluded files such as logs and run records
//!   stay readable under the existing rules as operational references and are
//!   never admitted) and the task records the identity it acted on;
//! - a created file gets a provisional identity (an operation-journal record)
//!   before it is written and is admitted after it exists; a failed admission
//!   leaves a visible pending-reconciliation record, never a fictional one.
//!
//! The graph's own storage is not a file any agent tool can name, and
//! service-owned records (run records, generated artifacts, application
//! metadata) cannot be written or created by agent file tools.

use std::collections::BTreeMap;
use std::time::Duration;

use exposition_core::path::WorkspacePath;
use exposition_graph::{
    Admission, BindingState, CoverageState, EntityKind, Lifecycle, OperationKind, classify, is_graph_storage,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{GraphEnv, TargetUse, TaskTarget, refusal};
use crate::model::TaskId;
use crate::store::Store;
use crate::tools::exec::{ToolOutput, codes};
use crate::tools::{Effects, PathUse};

/// How long a call waits for reconciliation of its targets before pausing.
pub const DEFAULT_SETTLE: Duration = Duration::from_secs(3);

/// The graph state of one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetState {
    Ready,
    /// The file changed and the graph has not reconciled it yet.
    Pending(String),
    /// The graph cannot tell which resource the path is.
    Ambiguous(String),
}

/// Paths the graph's inclusion policy reserves to services: never written or
/// created by agent file tools (reads stay possible under the existing rules).
fn service_owned(path: &WorkspacePath) -> Option<&'static str> {
    let p = path.as_str();
    let rest = p.strip_prefix(".exposition/")?;
    if rest.starts_with("research/") || p == ".exposition/project.json" {
        return None;
    }
    Some(if rest.starts_with("runs/") {
        "a run record (written only by the research service)"
    } else if rest.starts_with("artifacts/") {
        "a generated research artifact (written only by the research service)"
    } else {
        "application metadata"
    })
}

/// Path rules that hold whatever the graph's state: its storage is never a
/// file tool target, and service-owned records are never written by agents.
pub fn check_path_policy(path: &WorkspacePath, path_use: PathUse) -> Result<(), ToolOutput> {
    if is_graph_storage(path) {
        return Err(refusal(
            codes::GRAPH_STORAGE,
            format!(
                "{path} is the project graph's own storage. File tools never read or change it; use graph_query and \
                 graph_propose."
            ),
        ));
    }
    if matches!(path_use, PathUse::Write | PathUse::Create)
        && let Some(what) = service_owned(path)
    {
        return Err(refusal(
            codes::SERVICE_OWNED,
            format!("{path} is {what}. Agents cannot write or create it; nothing was changed."),
        ));
    }
    Ok(())
}

/// The graph state of a target the call acts on.
pub fn target_state(graph: GraphEnv<'_>, path: &WorkspacePath, path_use: PathUse) -> Result<TargetState, ToolOutput> {
    let state = graph.service.with_store(|store| {
        if let Some(c) = store.coverage(path)?
            && c.state == CoverageState::Pending
        {
            return Ok(TargetState::Pending(format!("{path} changed and the project graph has not reconciled it yet")));
        }
        let resources: Vec<_> = store
            .entities_by_key(EntityKind::Resource, path.as_str())?
            .into_iter()
            .filter(|e| e.lifecycle != Lifecycle::Tombstoned)
            .collect();
        if resources.len() > 1 {
            return Ok(TargetState::Ambiguous(format!("{} resource identities claim {path}", resources.len())));
        }
        if let Some(resource) = resources.first() {
            for b in store.bindings(&resource.id)? {
                match b.state {
                    BindingState::Ambiguous => {
                        return Ok(TargetState::Ambiguous(
                            b.detail.unwrap_or_else(|| format!("the binding of {path} is ambiguous")),
                        ));
                    }
                    BindingState::Missing if path_use != PathUse::Create => {
                        return Ok(TargetState::Pending(format!(
                            "the project graph lists {path} as missing; it has not reconciled the file since"
                        )));
                    }
                    _ => {}
                }
            }
        }
        Ok(TargetState::Ready)
    });
    state.map_err(|e| super::unavailable(&e.message))
}

/// The paths of a call that are targets (not folders), parsed.
pub fn target_paths(effects: &Effects<'_>) -> Vec<(WorkspacePath, PathUse)> {
    if !effects.targets {
        return Vec::new();
    }
    effects
        .paths
        .iter()
        .filter(|(_, u)| *u != PathUse::Folder)
        .filter_map(|(raw, u)| WorkspacePath::parse(raw).ok().map(|p| (p, *u)))
        .collect()
}

/// Gate rule: every target of the call is in a usable graph state.
pub fn check_targets(graph: GraphEnv<'_>, effects: &Effects<'_>) -> Result<(), ToolOutput> {
    for (path, path_use) in target_paths(effects) {
        match target_state(graph, &path, path_use)? {
            TargetState::Ready => {}
            TargetState::Pending(detail) => {
                return Err(refusal(
                    codes::GRAPH_PENDING,
                    format!(
                        "Not executed: {detail}. Acting on it now could use stale identities and dependencies, so the \
                         action is paused; nothing was read or changed. Try again after the graph has caught up."
                    ),
                ));
            }
            TargetState::Ambiguous(detail) => {
                return Err(refusal(
                    codes::GRAPH_AMBIGUOUS,
                    format!(
                        "Not executed: the project graph cannot tell which resource {path} is ({detail}). The action \
                         is paused until the user resolves it; nothing was read or changed."
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Wait (bounded) while any target is pending reconciliation, so a short
/// reconciliation does not pause the call. Returns early on cancellation.
pub async fn settle(graph: GraphEnv<'_>, effects: &Effects<'_>, max: Duration, cancel: &CancellationToken) {
    let targets = target_paths(effects);
    if targets.is_empty() || max.is_zero() {
        return;
    }
    let started = std::time::Instant::now();
    loop {
        let pending = targets.iter().any(|(p, u)| matches!(target_state(graph, p, *u), Ok(TargetState::Pending(_))));
        if !pending || started.elapsed() >= max {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            _ = cancel.cancelled() => return,
        }
    }
}

/// Resolve a target to its resource identity (admitting a qualifying file),
/// record it with the task, and describe it for the tool result.
/// `content_hash` is the saved file version when known (never an unsaved buffer).
pub fn resolve(
    graph: GraphEnv<'_>,
    store: &Store,
    task: &TaskId,
    path: &WorkspacePath,
    target_use: TargetUse,
    content_hash: Option<&str>,
) -> Value {
    let (entity, admitted, note) = match classify(path) {
        Admission::Admit(_) => {
            let before = graph.service.resource(path).ok().flatten();
            match graph.service.ensure_resource(path, content_hash, false, "agents") {
                Ok(entity) => (Some(entity.id), before.is_none(), None),
                Err(e) => (None, false, Some(format!("graph identity pending: {}", e.message))),
            }
        }
        Admission::OnRequest(_) => (
            graph.service.resource(path).ok().flatten().map(|e| e.id),
            false,
            Some("a generated output: an operational reference until it is retained or attached".to_owned()),
        ),
        Admission::Exclude(reason) => {
            (None, false, Some(format!("not a project resource ({reason}); an operational reference only")))
        }
    };
    record_target(store, task, path, entity.clone(), target_use, note.clone(), None);
    let status = graph.service.status();
    let mut out = json!({
        "resource": entity,
        "revision": status.revision,
        "freshness": status.freshness.state,
    });
    if admitted {
        out["admitted"] = json!(true);
    }
    if let Some(note) = note {
        out["note"] = json!(note);
    }
    out
}

/// Record a file the task acted on (with its identity, when it has one).
pub fn record_target(
    store: &Store,
    task: &TaskId,
    path: &WorkspacePath,
    entity: Option<exposition_graph::EntityId>,
    target_use: TargetUse,
    note: Option<String>,
    operation: Option<String>,
) {
    let target = TaskTarget {
        path: path.clone(),
        entity,
        uses: vec![target_use],
        note,
        operation,
        updated_at_ms: crate::store::now_ms(),
    };
    if let Err(e) = store.upsert_target(task, &target) {
        tracing::error!("failed to record a task target: {e}");
    }
}

/// A file creation's provisional identity: an operation-journal record
/// written before the file is.
pub struct Provisional {
    operation: Option<exposition_graph::OperationId>,
    admit: bool,
}

/// Reserve a provisional identity for a file about to be created.
pub fn reserve(graph: GraphEnv<'_>, task: &TaskId, path: &WorkspacePath) -> Provisional {
    let admit = matches!(classify(path), Admission::Admit(_));
    if !admit {
        return Provisional { operation: None, admit };
    }
    let Some(root) = graph.service.root() else { return Provisional { operation: None, admit } };
    let project = graph.service.status().project;
    let operation = graph
        .service
        .journal()
        .begin(
            OperationKind::CrossService,
            &root,
            project,
            &format!("Agent task {task} creates {path}"),
            vec!["create the file".into(), "admit it as a project resource".into()],
        )
        .map(|r| r.id)
        .map_err(|e| tracing::error!("operation journal: {e}"))
        .ok();
    Provisional { operation, admit }
}

/// The creation failed: nothing to reconcile.
pub fn abandon(graph: GraphEnv<'_>, provisional: Provisional, message: &str) {
    if let (Some(op), Some(root)) = (provisional.operation, graph.service.root()) {
        let _ = graph.service.journal().fail(&root, &op, message);
    }
}

/// The file exists: admit it and finish the journal record (or leave it
/// needing reconciliation when the graph could not record it).
pub fn finalize(
    graph: GraphEnv<'_>,
    store: &Store,
    task: &TaskId,
    path: &WorkspacePath,
    provisional: Provisional,
    content_hash: &str,
) -> Value {
    let root = graph.service.root();
    let journal = graph.service.journal();
    let op = provisional.operation.clone();
    if let (Some(op), Some(root)) = (&op, &root) {
        let refs = BTreeMap::from([
            ("path".to_owned(), path.to_string()),
            ("contentHash".to_owned(), content_hash.to_owned()),
        ]);
        let _ = journal.record_step(root, op, "create the file", refs, true, None);
    }
    if !provisional.admit {
        return resolve(graph, store, task, path, TargetUse::Create, Some(content_hash));
    }
    match graph.service.ensure_resource(path, Some(content_hash), false, "agents") {
        Ok(entity) => {
            if let (Some(op), Some(root)) = (&op, &root) {
                let refs = BTreeMap::from([("entity".to_owned(), entity.id.to_string())]);
                let _ = journal.record_step(root, op, "admit it as a project resource", refs, true, None);
                let _ = journal.complete(root, op);
            }
            record_target(
                store,
                task,
                path,
                Some(entity.id.clone()),
                TargetUse::Create,
                None,
                op.map(|o| o.to_string()),
            );
            let status = graph.service.status();
            json!({
                "resource": entity.id,
                "admitted": true,
                "revision": status.revision,
                "freshness": status.freshness.state,
            })
        }
        Err(e) => {
            let message = format!("the file was created but the project graph could not record it: {}", e.message);
            if let (Some(op), Some(root)) = (&op, &root) {
                let _ = journal.needs_reconciliation(root, op, &message);
            }
            record_target(
                store,
                task,
                path,
                None,
                TargetUse::Create,
                Some(message.clone()),
                op.as_ref().map(|o| o.to_string()),
            );
            json!({
                "resource": null,
                "pending": true,
                "code": codes::GRAPH_PENDING,
                "operation": op,
                "note": message,
            })
        }
    }
}
