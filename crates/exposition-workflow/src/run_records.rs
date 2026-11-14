//! P1 project operations on workflow run records: app-managed recoverable
//! deletion (hidden, restorable), permanent removal, the outputs a run
//! produced (a separate, explicit scope), active-run coordination, and
//! compaction of the history database so reported space is real.
//!
//! An active run is never deleted: it is stopped first (through its own
//! cancellation) and only an ended run is removed.

use exposition_core::path::WorkspacePath;

use super::{WorkflowRunService, project_key, store, unknown_run};
use crate::model::{WorkflowOutputValue, WorkflowRun, WorkflowRunError, WorkflowRunState, WorkflowRunSummary};
use exposition_graph::WorkflowRunId;
use std::time::{Duration, Instant};

/// A run record of the open project as Storage & History lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecordInfo {
    pub summary: WorkflowRunSummary,
    /// Size of the stored record.
    pub bytes: u64,
    /// Running now (it is stopped before anything is removed).
    pub active: bool,
    /// The research runs (batch outputs) the run produced.
    pub research_runs: Vec<String>,
    /// Project files the run produced or retained.
    pub outputs: Vec<WorkspacePath>,
}

/// An active run of the open project and the project files it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveRunInputs {
    pub run: WorkflowRunId,
    pub title: String,
    pub inputs: Vec<WorkspacePath>,
}

/// The research runs and project files a run produced (from every attempt).
pub fn run_outputs(run: &WorkflowRun) -> (Vec<String>, Vec<WorkspacePath>) {
    let mut research = Vec::new();
    let mut files = Vec::new();
    for step in &run.steps {
        let attempts = step.attempts.iter().chain(step.instances.iter().flat_map(|i| i.attempts.iter()));
        for attempt in attempts {
            for output in &attempt.outputs {
                let (path, research_run) = match &output.value {
                    WorkflowOutputValue::File { path, research_run, .. }
                    | WorkflowOutputValue::Retained { path, research_run, .. } => (path, research_run),
                    _ => continue,
                };
                if let Some(r) = research_run
                    && !research.contains(r)
                {
                    research.push(r.clone());
                }
                if !files.contains(path) {
                    files.push(path.clone());
                }
            }
        }
    }
    (research, files)
}

impl WorkflowRunService {
    /// Whether `id` is running now.
    pub fn is_active(&self, id: &WorkflowRunId) -> bool {
        self.inner.active.lock().unwrap_or_else(|p| p.into_inner()).contains_key(id)
    }

    /// Active runs of the open project with the project files their plans read.
    pub fn active_run_inputs(&self) -> Vec<ActiveRunInputs> {
        let Ok(root) = self.root() else { return Vec::new() };
        let key = project_key(&root);
        let ids: Vec<WorkflowRunId> = self
            .inner
            .active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, a)| project_key(&a.root) == key)
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| self.load(&id).ok())
            .map(|(_, run)| ActiveRunInputs {
                run: run.id.clone(),
                title: run.title.clone(),
                inputs: run.plan.inputs.iter().map(|i| i.path.clone()).collect(),
            })
            .collect()
    }

    /// Run records of the open project (not deleted), newest first.
    pub fn run_records(&self) -> Result<Vec<RunRecordInfo>, WorkflowRunError> {
        let root = self.root()?;
        let key = project_key(&root);
        let store = self.inner.store.lock().unwrap_or_else(|p| p.into_inner());
        let sizes: std::collections::HashMap<String, u64> = store.record_bytes(&key)?.into_iter().collect();
        let summaries = store.list(&key, None, store::MAX_LIST)?;
        let mut out = Vec::new();
        for summary in summaries {
            let (research_runs, outputs) = match store.get(&summary.id) {
                Ok(Some((_, run))) => run_outputs(&run),
                _ => (Vec::new(), Vec::new()),
            };
            out.push(RunRecordInfo {
                bytes: sizes.get(summary.id.as_str()).copied().unwrap_or(0),
                active: self.is_active(&summary.id),
                summary,
                research_runs,
                outputs,
            });
        }
        Ok(out)
    }

    /// Recoverably deleted runs of the open project.
    pub fn deleted_runs(&self) -> Result<Vec<store::DeletedRun>, WorkflowRunError> {
        let root = self.root()?;
        self.inner.store.lock().unwrap_or_else(|p| p.into_inner()).deleted(&project_key(&root))
    }

    /// A run of the open project, deleted or not (`deleted_at_ms` when deleted).
    pub fn run_any(&self, id: &WorkflowRunId) -> Result<(WorkflowRun, Option<u64>), WorkflowRunError> {
        let root = self.root()?;
        let found = self.inner.store.lock().unwrap_or_else(|p| p.into_inner()).get_any(id)?;
        match found {
            Some((project, run, deleted)) if project == project_key(&root) => Ok((run, deleted)),
            _ => Err(unknown_run()),
        }
    }

    /// Delete a run record of the open project: recoverably (hidden from the
    /// history, restorable) or `permanent`ly. Its outputs are not touched
    /// here (a separate scope). An active run is refused: stop it first.
    pub fn delete_run(&self, id: &WorkflowRunId, permanent: bool) -> Result<(), WorkflowRunError> {
        let _edit = self.inner.edits.lock().unwrap_or_else(|p| p.into_inner());
        let (run, deleted) = self.run_any(id)?;
        if self.is_active(id) || run.state == WorkflowRunState::Running {
            return Err(WorkflowRunError::new("workflow.runActive", "The run is still active; stop it first."));
        }
        let store = self.inner.store.lock().unwrap_or_else(|p| p.into_inner());
        if permanent {
            store.remove(id)?;
        } else if deleted.is_some() {
            return Err(WorkflowRunError::new("workflow.alreadyDeleted", "The run is already deleted."));
        } else {
            store.set_deleted(id, Some(crate::now_ms()))?;
        }
        Ok(())
    }

    /// Bring a recoverably deleted run back into the history. Nothing is
    /// re-run or resumed.
    pub fn restore_run(&self, id: &WorkflowRunId) -> Result<WorkflowRunSummary, WorkflowRunError> {
        let _edit = self.inner.edits.lock().unwrap_or_else(|p| p.into_inner());
        let (run, deleted) = self.run_any(id)?;
        if deleted.is_none() {
            return Err(WorkflowRunError::new("workflow.notDeleted", "The run is not deleted."));
        }
        self.inner.store.lock().unwrap_or_else(|p| p.into_inner()).set_deleted(id, None)?;
        Ok(store::summary(&run))
    }

    /// Stop an active run through its own cancellation and wait (bounded)
    /// until it has ended. Returns whether it is no longer active.
    pub fn stop_and_wait(&self, id: &WorkflowRunId, timeout: Duration) -> bool {
        if !self.is_active(id) {
            return true;
        }
        let _ = self.cancel(id);
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_active(id) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        !self.is_active(id)
    }

    /// Bytes the run history occupies on disk (database + log), if on disk.
    pub fn history_bytes(&self) -> Option<u64> {
        self.inner.store.lock().unwrap_or_else(|p| p.into_inner()).file_bytes()
    }

    /// Compact the run history after removals; (before, after) on-disk sizes.
    pub fn compact_history(&self) -> Result<(Option<u64>, Option<u64>), WorkflowRunError> {
        self.inner.store.lock().unwrap_or_else(|p| p.into_inner()).compact()
    }
}
