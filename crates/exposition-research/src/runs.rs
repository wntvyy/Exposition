//! In-memory run state: bounded output retention, artifact registration, and
//! manifest/summary construction.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;

use crate::helper::sha256_hex;
use crate::protocol::HelperFile;
use crate::store::artifact_dir_path;
use crate::types::{OutputEntry, OutputItem, RunManifest, RunOutputPage, RunSummary, SessionOwner};

/// Text retained in memory per run (older entries are dropped first).
pub const RETAINED_CHARS: usize = 2 * 1024 * 1024;
/// Entries retained in memory per run.
pub const RETAINED_ENTRIES: usize = 10_000;
/// Largest page returned by one output request.
pub const PAGE_LIMIT: u32 = 500;
/// Code stored in a manifest (larger code is recorded by hash only).
pub const MANIFEST_CODE_LIMIT: usize = 256 * 1024;
/// Largest artifact read back for hashing or display.
pub const ARTIFACT_SIZE_LIMIT: u64 = 64 * 1024 * 1024;

pub(crate) struct RunState {
    pub manifest: RunManifest,
    pub session_id: String,
    pub owner: SessionOwner,
    pub root: PathBuf,
    pub code: String,
    pub queued: bool,
    pub output_dir: Option<PathBuf>,
    pub entries: VecDeque<OutputEntry>,
    pub next_seq: u32,
    pub retained_chars: usize,
    pub interrupt_requested: bool,
    pub cancel_requested: bool,
    pub finished: bool,
}

impl RunState {
    pub fn id(&self) -> &str {
        &self.manifest.record.id.0
    }

    /// Append an output entry, discarding the oldest retained entries beyond the limits.
    pub fn push(&mut self, item: OutputItem) -> OutputEntry {
        self.next_seq += 1;
        let entry = OutputEntry { seq: self.next_seq, item };
        let size = entry_chars(&entry);
        self.retained_chars += size;
        self.entries.push_back(entry.clone());
        self.manifest.output.entries += 1;
        if let OutputItem::Stream { text, .. } = &entry.item {
            self.manifest.output.stream_chars += text.chars().count() as u64;
        }
        while self.entries.len() > 1 && (self.retained_chars > RETAINED_CHARS || self.entries.len() > RETAINED_ENTRIES)
        {
            if let Some(old) = self.entries.pop_front() {
                self.retained_chars -= entry_chars(&old);
                self.manifest.output.truncated = true;
            }
        }
        entry
    }

    pub fn page(&self, after: Option<u32>, limit: u32) -> RunOutputPage {
        page_of(self.id(), self.entries.iter(), after, limit, self.manifest.output.truncated, self.next_seq)
    }

    /// Register a file the helper wrote. The file must be a direct child of this
    /// run's artifact directory; size and hash are recomputed here. Text
    /// artifacts (HTML, SVG, CSV, JSON) are redacted like stream output first.
    pub fn register_file(&mut self, file: &HelperFile, redact: &dyn Fn(&str) -> String) -> Option<ArtifactRecord> {
        let dir = self.output_dir.as_ref()?;
        if !valid_output_name(&file.name) {
            return None;
        }
        let expected = dir.join(&file.name);
        let canonical = std::fs::canonicalize(&expected).ok()?;
        let reported = std::fs::canonicalize(Path::new(&file.path)).ok()?;
        if canonical != reported || canonical.parent() != Some(dir.as_path()) {
            return None;
        }
        let metadata = std::fs::metadata(&canonical).ok()?;
        if !metadata.is_file() || metadata.len() > ARTIFACT_SIZE_LIMIT {
            return None;
        }
        let bytes = redact_text_artifact(&canonical, std::fs::read(&canonical).ok()?, redact).ok()?;
        let kind = match file.role.as_str() {
            "image" => ArtifactKind::Figure,
            "table" => ArtifactKind::Table,
            _ => ArtifactKind::Data,
        };
        let relative = artifact_dir_path(self.id()).join(&file.name).ok()?;
        let record = ArtifactRecord {
            id: ArtifactId(format!("{}/{}", self.id(), file.name)),
            kind,
            path: Some(relative),
            scratch_path: None,
            media_type: media_type_for(&file.name).to_owned(),
            size_bytes: bytes.len() as u64,
            content_hash: format!("sha256:{}", sha256_hex(&bytes)),
        };
        self.manifest.record.outputs.push(record.clone());
        Some(record)
    }

    pub fn summary(&self) -> RunSummary {
        summary_of(&self.manifest, self.owner.clone(), self.queued)
    }
}

pub(crate) fn page_of<'a>(
    run_id: &str,
    entries: impl Iterator<Item = &'a OutputEntry>,
    after: Option<u32>,
    limit: u32,
    truncated: bool,
    last_seq: u32,
) -> RunOutputPage {
    let limit = limit.clamp(1, PAGE_LIMIT) as usize;
    let all: Vec<&OutputEntry> = entries.collect();
    let first_retained_seq = all.first().map(|e| e.seq).unwrap_or(last_seq + 1);
    let entries = all.into_iter().filter(|e| after.is_none_or(|a| e.seq > a)).take(limit).cloned().collect();
    RunOutputPage {
        run_id: exposition_core::artifact::RunId(run_id.to_owned()),
        entries,
        first_retained_seq,
        last_seq,
        truncated,
    }
}

pub(crate) fn summary_of(manifest: &RunManifest, owner: SessionOwner, queued: bool) -> RunSummary {
    let code_preview = manifest
        .code
        .text
        .as_deref()
        .map(|text| {
            let first: String = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(120).collect();
            first
        })
        .unwrap_or_else(|| format!("sha256:{}", &manifest.code.sha256[..12.min(manifest.code.sha256.len())]));
    RunSummary {
        id: manifest.record.id.clone(),
        session_id: manifest.session.session_id.clone(),
        owner,
        status: manifest.record.status.clone(),
        queued,
        origin: manifest.code.origin.clone(),
        code_preview,
        execution_count: manifest.execution_count,
        started_at_ms: manifest.record.started_at_ms,
        finished_at_ms: manifest.record.finished_at_ms,
        outputs: manifest.record.outputs.clone(),
        error: manifest.error.clone(),
    }
}

/// A run read from disk that claims to be running but has no live state.
pub(crate) fn settle_orphan(mut manifest: RunManifest) -> RunManifest {
    if manifest.record.status == RunStatus::Running {
        manifest.record.status =
            RunStatus::Failed { message: "Exposition stopped before this run finished.".to_owned() };
    }
    manifest
}

fn entry_chars(entry: &OutputEntry) -> usize {
    match &entry.item {
        OutputItem::Stream { text, .. } => text.len(),
        OutputItem::Display { text, .. } => text.as_ref().map_or(0, String::len) + 64,
        OutputItem::Error { evalue, traceback, .. } => evalue.len() + traceback.iter().map(String::len).sum::<usize>(),
        OutputItem::ClearOutput { .. } => 8,
        OutputItem::Notice { message } => message.len(),
    }
}

fn valid_output_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("output-") else { return false };
    let Some((number, extension)) = rest.split_once('.') else { return false };
    !number.is_empty()
        && number.len() <= 6
        && number.bytes().all(|b| b.is_ascii_digit())
        && matches!(extension, "png" | "jpg" | "gif" | "svg" | "html" | "csv")
}

/// Media types whose content is text a secret can be written into verbatim.
fn is_text_media(media_type: &str) -> bool {
    media_type.starts_with("text/") || matches!(media_type, "image/svg+xml" | "application/json")
}

/// Kernel output written to a text artifact (an HTML display, an SVG figure, a
/// CSV table) is untrusted text like stream output: secrets are replaced
/// before it is registered, stored, or shown. The replacement is plain text
/// ("[REDACTED]"), so markup around it renders unchanged. The file is
/// rewritten (through a temporary file in the same folder) only when
/// something was redacted. Returns the bytes the artifact now holds.
pub(crate) fn redact_text_artifact(
    path: &Path,
    bytes: Vec<u8>,
    redact: &dyn Fn(&str) -> String,
) -> std::io::Result<Vec<u8>> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if !is_text_media(media_type_for(name)) {
        return Ok(bytes);
    }
    let Ok(text) = std::str::from_utf8(&bytes) else { return Ok(bytes) };
    let redacted = redact(text);
    if redacted == text {
        return Ok(bytes);
    }
    let temp = path.with_file_name(format!(".{name}.redacting"));
    std::fs::write(&temp, redacted.as_bytes())?;
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(redacted.into_bytes())
}

/// The media type of an artifact by its extension (compared case-insensitively,
/// as output names are accepted: `FIGURE.SVG` is an SVG and is treated as one).
pub fn media_type_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "html" => "text/html",
        "csv" => "text/csv",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "tsv" => "text/tab-separated-values",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Validate an artifact path claimed by a caller against the run that produced it
/// (kernel outputs `output-N.<ext>`, or a batch output with a safe exported name).
pub fn artifact_belongs_to_run(path: &WorkspacePath, run_id: &str) -> bool {
    let prefix = artifact_dir_path(run_id);
    path.parent().as_ref() == Some(&prefix)
        && (valid_output_name(path.file_name()) || crate::sandbox::is_batch_artifact_name(path.file_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_ignore_the_extension_case() {
        assert_eq!(media_type_for("upper.TXT"), "text/plain");
        assert_eq!(media_type_for("figure.SVG"), "image/svg+xml");
        assert_eq!(media_type_for("table.Csv"), "text/csv");
        assert_eq!(media_type_for("photo.JPEG"), "image/jpeg");
        assert_eq!(media_type_for("noext"), "application/octet-stream");
    }

    #[test]
    fn text_artifacts_are_redacted_and_binary_ones_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let redact = |text: &str| text.replace("sk-ant-api03-SECRET", "[REDACTED]");
        let html = "<table><tr><td><b>sk-ant-api03-SECRET</b></td></tr></table>";
        let path = dir.path().join("output-1.html");
        std::fs::write(&path, html).unwrap();
        let bytes = redact_text_artifact(&path, html.as_bytes().to_vec(), &redact).unwrap();
        let expected = "<table><tr><td><b>[REDACTED]</b></td></tr></table>";
        assert_eq!(bytes, expected.as_bytes());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected, "the stored file is redacted");
        let svg = "<svg><text>sk-ant-api03-SECRET</text></svg>";
        let svg_path = dir.path().join("output-2.svg");
        std::fs::write(&svg_path, svg).unwrap();
        redact_text_artifact(&svg_path, svg.as_bytes().to_vec(), &redact).unwrap();
        assert!(!std::fs::read_to_string(&svg_path).unwrap().contains("SECRET"));
        // Binary images are not text: untouched, even if the bytes happen to match.
        let png = b"\x89PNGsk-ant-api03-SECRET".to_vec();
        let png_path = dir.path().join("output-3.png");
        std::fs::write(&png_path, &png).unwrap();
        assert_eq!(redact_text_artifact(&png_path, png.clone(), &redact).unwrap(), png);
        // Nothing to redact: the file is not rewritten.
        let clean = "<p>fine</p>";
        let clean_path = dir.path().join("output-4.html");
        std::fs::write(&clean_path, clean).unwrap();
        assert_eq!(redact_text_artifact(&clean_path, clean.as_bytes().to_vec(), &redact).unwrap(), clean.as_bytes());
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 4, "no temporary file left: {names:?}");
    }

    #[test]
    fn output_names_are_restricted() {
        assert!(valid_output_name("output-1.png"));
        assert!(valid_output_name("output-12.csv"));
        for bad in ["output-.png", "output-1.exe", "../output-1.png", "output-1.png.partial", "x.png"] {
            assert!(!valid_output_name(bad), "{bad}");
        }
        let path = WorkspacePath::parse(".exposition/artifacts/run-ab/output-2.svg").unwrap();
        assert!(artifact_belongs_to_run(&path, "run-ab"));
        assert!(!artifact_belongs_to_run(&path, "run-cd"));
    }
}
