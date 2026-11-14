//! P1 project operations on research run records (`.exposition/runs`):
//! app-managed recoverable deletion (the manifest moves to
//! `.exposition/runs/deleted/`, hidden from the history), restore that never
//! overwrites, permanent removal of the record (manifest + transcript), and —
//! only as a separate, explicit scope — removal of the run's generated output
//! files that nothing keeps (retained or attached outputs are kept).
//!
//! An active run (an interactive run that has not finished, or a workflow
//! batch executing now) is never removed: it is canceled first.

use std::path::{Path, PathBuf};

use exposition_core::path::WorkspacePath;

use crate::error::ResearchError;
use crate::service::ResearchService;
use crate::store::{self, read_manifest, valid_run_id};
use crate::types::RunManifest;

const RUNS: &str = ".exposition/runs";
const DELETED: &str = "deleted";
const ARTIFACTS: &str = ".exposition/artifacts";
const TRANSCRIPT: &str = "transcript.json";

/// A research run record as Storage & History lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRecordInfo {
    pub id: String,
    /// A short description (the code's first line or the file it ran).
    pub label: String,
    pub started_at_ms: u64,
    /// Bytes of the record (manifest + transcript).
    pub record_bytes: u64,
    /// Generated output files of the run (not the transcript).
    pub outputs: Vec<OutputFile>,
    /// Running now (canceled before anything is removed).
    pub active: bool,
    /// When it was recoverably deleted.
    pub deleted_at_ms: Option<u64>,
    /// Project files the run read (its declared inputs and code file).
    pub inputs: Vec<WorkspacePath>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputFile {
    pub path: WorkspacePath,
    pub bytes: u64,
}

/// What removing outputs did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputsRemoved {
    pub removed: Vec<WorkspacePath>,
    pub bytes: u64,
    /// Outputs kept because something keeps or uses them.
    pub kept: Vec<WorkspacePath>,
}

fn join(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

fn manifest_path(root: &Path, id: &str) -> PathBuf {
    join(root, RUNS).join(format!("{id}.json"))
}

fn deleted_path(root: &Path, id: &str) -> PathBuf {
    join(root, RUNS).join(DELETED).join(format!("{id}.json"))
}

fn artifact_dir(root: &Path, id: &str) -> PathBuf {
    join(root, ARTIFACTS).join(id)
}

fn size(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0)
}

/// A regular directory inside `root` (never through a link or junction).
fn real_dir_within(root: &Path, dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        && std::fs::canonicalize(dir).is_ok_and(|c| c.starts_with(root))
}

fn label_of(manifest: &RunManifest) -> String {
    let text = manifest.code.text.as_deref().unwrap_or("");
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let label = if first.is_empty() { manifest.record.id.0.clone() } else { first.to_owned() };
    label.chars().take(80).collect()
}

fn read_deleted(root: &Path, id: &str) -> Result<RunManifest, ResearchError> {
    let bytes = std::fs::read(deleted_path(root, id)).map_err(|_| ResearchError::UnknownRun(id.to_owned()))?;
    let manifest: RunManifest =
        serde_json::from_slice(&bytes).map_err(|e| ResearchError::Io(format!("run manifest {id} is invalid: {e}")))?;
    if manifest.record.id.0 != id {
        return Err(ResearchError::Io(format!("run manifest {id} names a different run")));
    }
    Ok(manifest)
}

/// Generated output files of a run (regular files directly in its artifact
/// folder, the transcript excluded).
fn output_files(root: &Path, id: &str) -> Vec<OutputFile> {
    let dir = artifact_dir(root, id);
    if !real_dir_within(root, &dir) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if name == TRANSCRIPT || !meta.is_file() || entry.file_type().is_ok_and(|t| t.is_symlink()) {
            continue;
        }
        if let Ok(path) = WorkspacePath::parse(&format!("{ARTIFACTS}/{id}/{name}")) {
            out.push(OutputFile { path, bytes: meta.len() });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn info(root: &Path, manifest: &RunManifest, active: bool, deleted_at_ms: Option<u64>) -> RunRecordInfo {
    let id = &manifest.record.id.0;
    let record = if deleted_at_ms.is_some() { deleted_path(root, id) } else { manifest_path(root, id) };
    RunRecordInfo {
        id: id.clone(),
        label: label_of(manifest),
        started_at_ms: manifest.record.started_at_ms,
        record_bytes: size(&record) + size(&artifact_dir(root, id).join(TRANSCRIPT)),
        outputs: output_files(root, id),
        active,
        deleted_at_ms,
        inputs: manifest.record.inputs.iter().map(|i| i.path.clone()).collect(),
    }
}

impl ResearchService {
    /// The open project's root (canonical).
    fn records_root(&self) -> Result<PathBuf, ResearchError> {
        self.workspace_root().ok_or(ResearchError::NoWorkspace)
    }

    /// Run records of the open project (not deleted), newest first.
    pub fn run_records(&self) -> Result<Vec<RunRecordInfo>, ResearchError> {
        let root = self.records_root()?;
        let mut out: Vec<RunRecordInfo> = store::list_manifests(&root)
            .iter()
            .map(|m| info(&root, m, self.run_active(&m.record.id.0), None))
            .collect();
        // Active runs whose manifest is not written yet are listed too.
        for id in self.active_run_ids() {
            if !out.iter().any(|r| r.id == id)
                && let Ok(m) = self.run_manifest(&id)
            {
                out.push(info(&root, &m, true, None));
            }
        }
        out.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms).then_with(|| b.id.cmp(&a.id)));
        Ok(out)
    }

    /// Recoverably deleted run records of the open project.
    pub fn deleted_run_records(&self) -> Result<Vec<RunRecordInfo>, ResearchError> {
        let root = self.records_root()?;
        let dir = join(&root, RUNS).join(DELETED);
        let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json").filter(|id| valid_run_id(id)) else { continue };
            let Ok(manifest) = read_deleted(&root, id) else { continue };
            let at = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            out.push(info(&root, &manifest, false, Some(at)));
        }
        out.sort_by(|a, b| b.deleted_at_ms.cmp(&a.deleted_at_ms).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    /// One record, deleted or not.
    pub fn run_record(&self, id: &str) -> Result<RunRecordInfo, ResearchError> {
        if !valid_run_id(id) {
            return Err(ResearchError::UnknownRun(id.to_owned()));
        }
        let root = self.records_root()?;
        if let Ok(m) = read_manifest(&root, id) {
            return Ok(info(&root, &m, self.run_active(id), None));
        }
        if self.run_active(id)
            && let Ok(m) = self.run_manifest(id)
        {
            return Ok(info(&root, &m, true, None));
        }
        let m = read_deleted(&root, id)?;
        let at = std::fs::metadata(deleted_path(&root, id))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Ok(info(&root, &m, false, Some(at)))
    }

    /// Delete a run's record: recoverably (hidden from the history,
    /// restorable) or `permanent`ly (manifest and transcript). Output files
    /// are a separate scope ([`Self::remove_run_outputs`]). An active run is
    /// refused (`research.runActive`): cancel it first.
    pub fn delete_run_record(&self, id: &str, permanent: bool) -> Result<(), ResearchError> {
        if !valid_run_id(id) {
            return Err(ResearchError::UnknownRun(id.to_owned()));
        }
        if self.run_active(id) {
            return Err(ResearchError::coded("research.runActive", "The run is still active; cancel it first."));
        }
        let root = self.records_root()?;
        let live = manifest_path(&root, id);
        let deleted = deleted_path(&root, id);
        let runs = join(&root, RUNS);
        if !real_dir_within(&root, &runs) {
            return Err(ResearchError::UnknownRun(id.to_owned()));
        }
        if permanent {
            let mut found = false;
            for path in [&live, &deleted] {
                if path.is_file() {
                    std::fs::remove_file(path).map_err(|e| ResearchError::io("removing the run record", e))?;
                    found = true;
                }
            }
            if !found {
                return Err(ResearchError::UnknownRun(id.to_owned()));
            }
            let transcript = artifact_dir(&root, id).join(TRANSCRIPT);
            if real_dir_within(&root, &artifact_dir(&root, id)) && transcript.is_file() {
                std::fs::remove_file(&transcript).map_err(|e| ResearchError::io("removing the run transcript", e))?;
            }
            remove_if_empty(&root, &artifact_dir(&root, id));
            return Ok(());
        }
        if !live.is_file() {
            return Err(ResearchError::UnknownRun(id.to_owned()));
        }
        let dir = runs.join(DELETED);
        std::fs::create_dir_all(&dir).map_err(|e| ResearchError::io("creating the deleted-runs folder", e))?;
        if !real_dir_within(&root, &dir) {
            return Err(ResearchError::InvalidWorkspace(format!("{RUNS}/{DELETED} is a link; remove it and retry")));
        }
        std::fs::rename(&live, &deleted).map_err(|e| ResearchError::io("moving the run record", e))
    }

    /// Bring a recoverably deleted run record back into the history (never
    /// over a record with the same id).
    pub fn restore_run_record(&self, id: &str) -> Result<RunRecordInfo, ResearchError> {
        if !valid_run_id(id) {
            return Err(ResearchError::UnknownRun(id.to_owned()));
        }
        let root = self.records_root()?;
        let (live, deleted) = (manifest_path(&root, id), deleted_path(&root, id));
        if !deleted.is_file() {
            return Err(ResearchError::UnknownRun(id.to_owned()));
        }
        if live.exists() {
            return Err(ResearchError::InvalidRequest(format!("a run record {id} exists again; nothing was replaced")));
        }
        std::fs::rename(&deleted, &live).map_err(|e| ResearchError::io("restoring the run record", e))?;
        self.run_record(id)
    }

    /// Remove a run's generated output files, except those `keep` says are
    /// retained or used (kept by default). The record is not touched.
    pub fn remove_run_outputs(
        &self,
        id: &str,
        keep: &dyn Fn(&WorkspacePath) -> bool,
    ) -> Result<OutputsRemoved, ResearchError> {
        if !valid_run_id(id) {
            return Err(ResearchError::UnknownRun(id.to_owned()));
        }
        if self.run_active(id) {
            return Err(ResearchError::coded("research.runActive", "The run is still active; cancel it first."));
        }
        let root = self.records_root()?;
        let mut done = OutputsRemoved::default();
        for file in output_files(&root, id) {
            if keep(&file.path) {
                done.kept.push(file.path);
                continue;
            }
            let abs = join(&root, file.path.as_str());
            std::fs::remove_file(&abs).map_err(|e| ResearchError::io("removing a run output", e))?;
            done.bytes += file.bytes;
            done.removed.push(file.path);
        }
        remove_if_empty(&root, &artifact_dir(&root, id));
        Ok(done)
    }
}

fn remove_if_empty(root: &Path, dir: &Path) {
    if real_dir_within(root, dir) && std::fs::read_dir(dir).is_ok_and(|mut e| e.next().is_none()) {
        let _ = std::fs::remove_dir(dir);
    }
}
