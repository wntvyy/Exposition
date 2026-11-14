//! App-local trigger records: `<data dir>/workflow-triggers.json`.
//!
//! Triggers are the user's standing approvals, so they live with the
//! application's data — never in the project folder, a workflow definition,
//! a project graph, or a bundle — and are written atomically (a temporary
//! file, then a rename). Nothing fires from data the application cannot
//! verify, and nothing it cannot read is overwritten: an unreadable record is
//! kept as it is, a file in an unknown format is set aside, and a store that
//! cannot be read at all is not written until it can be.

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

/// What [`load`] read.
#[derive(Debug, Default)]
pub struct Loaded {
    pub triggers: Vec<StoredTrigger>,
    /// Records this version cannot read: kept as they are and written back
    /// unchanged (never fired from, never dropped).
    pub unreadable: Vec<serde_json::Value>,
}

/// Load the records at `path` (none if it does not exist). `Err`: the store
/// exists but cannot be read now (and could not be set aside): it must not
/// be written until it can be read.
pub fn load(path: &Path) -> Result<Loaded, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded::default()),
        Err(e) => {
            return Err(format!(
                "The trigger store ({}) cannot be read ({e}); triggers cannot be changed until it can be, and it is never overwritten.",
                path.display()
            ));
        }
    };
    let file = match serde_json::from_slice::<File<serde_json::Value>>(&bytes) {
        Ok(file) if file.schema == SCHEMA => file,
        _ => {
            // Not a trigger store this version understands: kept aside, never
            // overwritten, and nothing fires from it.
            let aside = path.with_extension(format!("json.bad-{}", crate::now_ms()));
            return match std::fs::rename(path, &aside) {
                Ok(()) => {
                    tracing::warn!("workflow triggers file is not readable; set aside as {}", aside.display());
                    Ok(Loaded::default())
                }
                Err(e) => Err(format!(
                    "The trigger store ({}) is not readable and could not be set aside ({e}); triggers cannot be changed until it is, and it is never overwritten.",
                    path.display()
                )),
            };
        }
    };
    let mut loaded = Loaded::default();
    for value in file.triggers {
        match serde_json::from_value::<StoredTrigger>(value.clone()) {
            Ok(stored) => loaded.triggers.push(stored),
            Err(e) => {
                tracing::warn!("a workflow trigger record could not be read ({e}); it is kept but never fires");
                loaded.unreadable.push(value);
            }
        }
    }
    Ok(loaded)
}

/// Write the records (and the unreadable ones, unchanged) atomically.
pub fn save(path: &Path, triggers: &[&StoredTrigger], unreadable: &[serde_json::Value]) -> Result<(), String> {
    let mut records = Vec::with_capacity(triggers.len() + unreadable.len());
    for stored in triggers {
        records.push(serde_json::to_value(stored).map_err(|e| e.to_string())?);
    }
    records.extend(unreadable.iter().cloned());
    let file = File { schema: SCHEMA.to_owned(), triggers: records };
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
