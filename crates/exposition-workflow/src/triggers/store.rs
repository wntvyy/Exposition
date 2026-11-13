//! App-local trigger records: `<data dir>/workflow-triggers.json`.
//!
//! Triggers are the user's standing approvals, so they live with the
//! application's data — never in the project folder, a workflow definition,
//! a project graph, or a bundle — and are written atomically (a temporary
//! file, then a rename). A record that cannot be read is dropped (logged),
//! and an unreadable file is set aside: nothing fires from data the
//! application cannot verify.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::WorkflowTrigger;
use crate::model::WorkflowRunPlan;

pub const STORE_FILE: &str = "workflow-triggers.json";
const SCHEMA: &str = "exposition.workflowTriggers/1";

/// One triggered run, for the rate limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FireRecord {
    pub at_ms: u64,
    pub model_requests: u32,
}

/// A trigger and what it needs to check later plans (persisted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTrigger {
    /// The durable part of the view (runtime fields are reset on load).
    pub trigger: WorkflowTrigger,
    /// `plan::project_key` of the project root it belongs to.
    pub project_key: String,
    /// The approved plan (code review text removed), for describing changes.
    pub standing_plan: WorkflowRunPlan,
    /// Watched input → SHA-256 when the trigger last looked.
    pub baseline: BTreeMap<String, Option<String>>,
    /// Runs of the last day (rate limits survive re-enabling and restarts).
    pub fires: Vec<FireRecord>,
    /// Consecutive refusals of the run service to start (bounded retries).
    #[serde(default)]
    pub start_refusals: u32,
}

#[derive(Serialize, Deserialize)]
struct File<T> {
    schema: String,
    triggers: Vec<T>,
}

/// Load the records at `path` (none if it does not exist).
pub fn load(path: &Path) -> Vec<StoredTrigger> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("workflow triggers could not be read ({e}); none are active");
            return Vec::new();
        }
    };
    let file = match serde_json::from_slice::<File<serde_json::Value>>(&bytes) {
        Ok(file) if file.schema == SCHEMA => file,
        _ => {
            let aside = path.with_extension(format!("json.bad-{}", crate::now_ms()));
            tracing::warn!("workflow triggers file is not readable; set aside as {}", aside.display());
            let _ = std::fs::rename(path, aside);
            return Vec::new();
        }
    };
    file.triggers
        .into_iter()
        .filter_map(|value| match serde_json::from_value::<StoredTrigger>(value) {
            Ok(stored) => Some(stored),
            Err(e) => {
                tracing::warn!("a workflow trigger record could not be read ({e}); it was dropped");
                None
            }
        })
        .collect()
}

/// Write the records atomically.
pub fn save(path: &Path, triggers: &[&StoredTrigger]) -> Result<(), String> {
    let file = File { schema: SCHEMA.to_owned(), triggers: triggers.to_vec() };
    let bytes = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = PathBuf::from(format!("{}.tmp-{}", path.display(), uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}
