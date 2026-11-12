//! Portable run storage inside the project.
//!
//! Layout (all inside the workspace root, never outside it):
//!
//! ```text
//! .exposition/runs/<run-id>.json               run manifest (RunManifest, format exposition.run/1)
//! .exposition/artifacts/<run-id>/output-N.png  figures (PNG/JPEG/GIF/SVG) as produced by the kernel
//! .exposition/artifacts/<run-id>/output-N.html rich HTML output, stored as inert text
//! .exposition/artifacts/<run-id>/output-N.csv  table grid derived from the HTML table
//! .exposition/artifacts/<run-id>/transcript.json  retained text output (bounded)
//! ```
//!
//! Documents reference artifacts by paths relative to the document, so a project
//! (including its runs) can be moved or shared as a whole.

use std::path::{Path, PathBuf};

use exposition_core::path::WorkspacePath;

use crate::error::ResearchError;
use crate::types::{OutputEntry, RunManifest};

pub const MANIFEST_FORMAT: &str = "exposition.run/1";
const EXPOSITION_DIR: &str = ".exposition";
const RUNS_DIR: &str = "runs";
const ARTIFACTS_DIR: &str = "artifacts";
const TRANSCRIPT: &str = "transcript.json";
/// Manifests read when listing history.
const HISTORY_LIMIT: usize = 500;

pub fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.starts_with("run-")
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn new_run_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("run-{}", &id[..16])
}

/// Workspace-relative location of a run's artifact directory.
pub fn artifact_dir_path(run_id: &str) -> WorkspacePath {
    WorkspacePath::parse(&format!("{EXPOSITION_DIR}/{ARTIFACTS_DIR}/{run_id}")).expect("valid run id path")
}

/// Create `root/<relative>` and verify the result (after resolving symlinks and
/// junctions) is still inside the canonical `root`.
pub fn ensure_dir_within(root: &Path, relative: &WorkspacePath) -> Result<PathBuf, ResearchError> {
    let target = relative.components().fold(root.to_path_buf(), |path, part| path.join(part));
    std::fs::create_dir_all(&target).map_err(|e| ResearchError::io("creating research directory", e))?;
    let canonical = std::fs::canonicalize(&target).map_err(|e| ResearchError::io("resolving research directory", e))?;
    if !canonical.starts_with(root) {
        return Err(ResearchError::InvalidWorkspace(format!(
            "{} resolves outside the workspace; remove the link and retry",
            relative
        )));
    }
    Ok(canonical)
}

/// Resolve an existing workspace file, rejecting anything that resolves outside `root`.
pub fn resolve_existing(root: &Path, relative: &WorkspacePath) -> Result<PathBuf, ResearchError> {
    let target = relative.components().fold(root.to_path_buf(), |path, part| path.join(part));
    let canonical = std::fs::canonicalize(&target)
        .map_err(|e| ResearchError::Io(format!("{relative} could not be opened: {e}")))?;
    if !canonical.starts_with(root) {
        return Err(ResearchError::InvalidRequest(format!("{relative} resolves outside the workspace")));
    }
    Ok(canonical)
}

pub fn run_dir(root: &Path) -> Result<PathBuf, ResearchError> {
    ensure_dir_within(root, &WorkspacePath::parse(&format!("{EXPOSITION_DIR}/{RUNS_DIR}")).expect("static path"))
}

pub fn write_manifest(root: &Path, manifest: &RunManifest) -> Result<(), ResearchError> {
    let dir = run_dir(root)?;
    let id = &manifest.record.id.0;
    if !valid_run_id(id) {
        return Err(ResearchError::InvalidRequest(format!("invalid run id {id}")));
    }
    let json = serde_json::to_vec_pretty(manifest).map_err(|e| ResearchError::Io(e.to_string()))?;
    write_atomic(&dir.join(format!("{id}.json")), &json)
}

pub fn read_manifest(root: &Path, run_id: &str) -> Result<RunManifest, ResearchError> {
    if !valid_run_id(run_id) {
        return Err(ResearchError::UnknownRun(run_id.to_owned()));
    }
    let path = root.join(EXPOSITION_DIR).join(RUNS_DIR).join(format!("{run_id}.json"));
    let bytes = std::fs::read(&path).map_err(|_| ResearchError::UnknownRun(run_id.to_owned()))?;
    let manifest: RunManifest = serde_json::from_slice(&bytes)
        .map_err(|e| ResearchError::Io(format!("run manifest {run_id} is invalid: {e}")))?;
    if manifest.record.id.0 != run_id {
        return Err(ResearchError::Io(format!("run manifest {run_id} names a different run")));
    }
    Ok(manifest)
}

/// Most recent manifests (by file modification time), newest first.
pub fn list_manifests(root: &Path) -> Vec<RunManifest> {
    let dir = root.join(EXPOSITION_DIR).join(RUNS_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".json")?.to_owned();
            if !valid_run_id(&id) {
                return None;
            }
            let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, id))
        })
        .collect();
    files.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    files.truncate(HISTORY_LIMIT);
    files.into_iter().filter_map(|(_, id)| read_manifest(root, &id).ok()).collect()
}

pub fn write_transcript(root: &Path, run_id: &str, entries: &[OutputEntry]) -> Result<(), ResearchError> {
    if entries.is_empty() {
        return Ok(());
    }
    let dir = ensure_dir_within(root, &artifact_dir_path(run_id))?;
    let json = serde_json::to_vec(entries).map_err(|e| ResearchError::Io(e.to_string()))?;
    write_atomic(&dir.join(TRANSCRIPT), &json)
}

pub fn read_transcript(root: &Path, run_id: &str) -> Vec<OutputEntry> {
    if !valid_run_id(run_id) {
        return Vec::new();
    }
    let path = root.join(EXPOSITION_DIR).join(ARTIFACTS_DIR).join(run_id).join(TRANSCRIPT);
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ResearchError> {
    let partial = path.with_extension(format!("{}.partial", std::process::id()));
    std::fs::write(&partial, bytes).map_err(|e| ResearchError::io("writing run data", e))?;
    std::fs::rename(&partial, path).map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        ResearchError::io("replacing run data", e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_are_strict() {
        let id = new_run_id();
        assert!(valid_run_id(&id), "{id}");
        for bad in ["", "run-", "../x", "run-A", "run-../../x", "run-a/b", "x-123"] {
            if bad == "run-" {
                continue;
            }
            assert!(!valid_run_id(bad), "{bad}");
        }
        assert_eq!(artifact_dir_path("run-abc").as_str(), ".exposition/artifacts/run-abc");
    }

    #[test]
    fn directories_stay_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let created = ensure_dir_within(&root, &artifact_dir_path("run-1")).unwrap();
        assert!(created.starts_with(&root));
        assert!(read_manifest(&root, "run-missing").is_err());
        assert!(read_manifest(&root, "../secret").is_err());
    }
}
