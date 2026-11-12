//! App-owned journal of cross-service operations.
//!
//! Source files and the graph are not one transaction. Before an operation
//! that changes both (promotion of a research result into a manuscript,
//! project creation from a blueprint), its intent is written here; each step
//! then records what actually happened (document transaction ids, attachment
//! keys, graph revisions). The journal lives in application data, so it
//! survives a damaged or missing project graph. After an interruption,
//! incomplete operations are listed for reconciliation against the recovered
//! documents and artifacts — they are never replayed automatically.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::ids::{OperationId, ProjectId};
use crate::store::now_ms;

/// Records kept per project folder (oldest completed ones are pruned).
const MAX_RECORDS: usize = 200;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OperationKind {
    /// Reviewed promotion of a retained result into a manuscript.
    Promotion,
    /// Project creation from a preset or an agent-proposed blueprint.
    ProjectCreation,
    /// Any other change spanning documents and the graph.
    CrossService,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OperationState {
    /// Intent recorded; steps may or may not have happened.
    Pending,
    Completed,
    /// Failed with nothing left to reconcile.
    Failed {
        message: String,
    },
    /// Interrupted (found pending at startup); needs reconciliation by a
    /// person or the owning service against the actual documents/graph.
    NeedsReconciliation {
        message: String,
    },
    /// A person or service resolved it after reconciliation.
    Resolved {
        resolution: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OperationStep {
    pub name: String,
    /// Identifiers of what actually happened (transaction id, attachment key,
    /// graph revision, created path, ...).
    pub refs: BTreeMap<String, String>,
    pub completed: bool,
    pub message: Option<String>,
    pub at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct OperationRecord {
    pub id: OperationId,
    pub kind: OperationKind,
    /// Absolute project folder.
    pub project_root: String,
    pub project: Option<ProjectId>,
    pub description: String,
    /// What is intended, in order (names of steps).
    pub plan: Vec<String>,
    pub steps: Vec<OperationStep>,
    pub state: OperationState,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("operation journal: {0}")]
    Io(#[from] std::io::Error),
    #[error("operation journal record is unreadable: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("unknown operation {0}")]
    Unknown(String),
}

/// The operation journal (one folder per project root under application data).
#[derive(Clone, Debug)]
pub struct OperationJournal {
    dir: PathBuf,
}

impl OperationJournal {
    pub fn new(data_dir: &Path) -> Self {
        Self { dir: data_dir.join("graph-operations") }
    }

    fn project_dir(&self, root: &Path) -> PathBuf {
        let digest = Sha256::digest(root.to_string_lossy().to_lowercase().as_bytes());
        let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
        self.dir.join(hex)
    }

    fn file(&self, root: &Path, id: &OperationId) -> PathBuf {
        self.project_dir(root).join(format!("{id}.json"))
    }

    fn write(&self, record: &OperationRecord) -> Result<(), JournalError> {
        let root = PathBuf::from(&record.project_root);
        let dir = self.project_dir(&root);
        fs::create_dir_all(&dir)?;
        let path = self.file(&root, &record.id);
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(record)?)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Record intent before any mutation.
    pub fn begin(
        &self,
        kind: OperationKind,
        project_root: &Path,
        project: Option<ProjectId>,
        description: &str,
        plan: Vec<String>,
    ) -> Result<OperationRecord, JournalError> {
        let now = now_ms();
        let record = OperationRecord {
            id: OperationId::mint(),
            kind,
            project_root: project_root.to_string_lossy().into_owned(),
            project,
            description: crate::store::truncate(description, 1000),
            plan,
            steps: Vec::new(),
            state: OperationState::Pending,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.write(&record)?;
        self.prune(project_root);
        Ok(record)
    }

    pub fn get(&self, project_root: &Path, id: &OperationId) -> Result<OperationRecord, JournalError> {
        let path = self.file(project_root, id);
        let meta = fs::metadata(&path).map_err(|_| JournalError::Unknown(id.to_string()))?;
        if meta.len() > MAX_RECORD_BYTES {
            return Err(JournalError::Unknown(id.to_string()));
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    fn update(
        &self,
        project_root: &Path,
        id: &OperationId,
        f: impl FnOnce(&mut OperationRecord),
    ) -> Result<OperationRecord, JournalError> {
        let mut record = self.get(project_root, id)?;
        f(&mut record);
        record.updated_at_ms = now_ms();
        self.write(&record)?;
        Ok(record)
    }

    /// Record a step's actual outcome.
    pub fn record_step(
        &self,
        project_root: &Path,
        id: &OperationId,
        name: &str,
        refs: BTreeMap<String, String>,
        completed: bool,
        message: Option<String>,
    ) -> Result<OperationRecord, JournalError> {
        self.update(project_root, id, |r| {
            r.steps.push(OperationStep { name: name.to_owned(), refs, completed, message, at_ms: now_ms() })
        })
    }

    pub fn complete(&self, project_root: &Path, id: &OperationId) -> Result<OperationRecord, JournalError> {
        self.update(project_root, id, |r| r.state = OperationState::Completed)
    }

    pub fn fail(&self, project_root: &Path, id: &OperationId, message: &str) -> Result<OperationRecord, JournalError> {
        self.update(project_root, id, |r| r.state = OperationState::Failed { message: message.to_owned() })
    }

    /// Mark an operation as needing reconciliation (e.g. its graph step failed
    /// after the document changed).
    pub fn needs_reconciliation(
        &self,
        project_root: &Path,
        id: &OperationId,
        message: &str,
    ) -> Result<OperationRecord, JournalError> {
        self.update(project_root, id, |r| r.state = OperationState::NeedsReconciliation { message: message.to_owned() })
    }

    pub fn resolve(
        &self,
        project_root: &Path,
        id: &OperationId,
        resolution: &str,
    ) -> Result<OperationRecord, JournalError> {
        self.update(project_root, id, |r| r.state = OperationState::Resolved { resolution: resolution.to_owned() })
    }

    /// Every record of a project folder, newest first.
    pub fn list(&self, project_root: &Path) -> Vec<OperationRecord> {
        let mut out: Vec<OperationRecord> = fs::read_dir(self.project_dir(project_root))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter(|e| e.metadata().map(|m| m.len() <= MAX_RECORD_BYTES).unwrap_or(false))
            .filter_map(|e| fs::read(e.path()).ok())
            .filter_map(|bytes| serde_json::from_slice::<OperationRecord>(&bytes).ok())
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
        out
    }

    /// Operations left pending or needing reconciliation. Pending records
    /// found here at startup were interrupted: they are re-labelled
    /// `NeedsReconciliation` (never re-run).
    pub fn unresolved(&self, project_root: &Path, mark_interrupted: bool) -> Vec<OperationRecord> {
        let mut out = Vec::new();
        for mut record in self.list(project_root) {
            match record.state {
                OperationState::Pending if mark_interrupted => {
                    if let Ok(r) = self.needs_reconciliation(
                        project_root,
                        &record.id,
                        "Exposition stopped before this operation finished; check the result before continuing.",
                    ) {
                        record = r;
                    }
                    out.push(record);
                }
                OperationState::Pending | OperationState::NeedsReconciliation { .. } => out.push(record),
                _ => {}
            }
        }
        out
    }

    fn prune(&self, project_root: &Path) {
        let records = self.list(project_root);
        if records.len() <= MAX_RECORDS {
            return;
        }
        for r in records.iter().skip(MAX_RECORDS) {
            if matches!(
                r.state,
                OperationState::Completed | OperationState::Failed { .. } | OperationState::Resolved { .. }
            ) {
                let _ = fs::remove_file(self.file(project_root, &r.id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_steps_and_interruption() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let journal = OperationJournal::new(data.path());
        let op = journal
            .begin(OperationKind::Promotion, project.path(), None, "Promote fig", vec!["attach".into(), "graph".into()])
            .unwrap();
        let mut refs = BTreeMap::new();
        refs.insert("transaction".into(), "t-1".into());
        journal.record_step(project.path(), &op.id, "attach", refs, true, None).unwrap();
        // Simulate a crash before completion: the next start lists it as needing reconciliation.
        let unresolved = journal.unresolved(project.path(), true);
        assert_eq!(unresolved.len(), 1);
        assert!(matches!(unresolved[0].state, OperationState::NeedsReconciliation { .. }));
        assert_eq!(unresolved[0].steps[0].refs["transaction"], "t-1");
        journal.resolve(project.path(), &op.id, "attachment verified").unwrap();
        assert!(journal.unresolved(project.path(), true).is_empty());
    }
}
