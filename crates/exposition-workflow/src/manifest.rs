//! Run manifests (M9 reproducibility): a portable JSON account of one run —
//! the exact revision, definition and approval hashes, every input read
//! (project path, SHA-256, size), every validated output (port, digest,
//! provenance), each attempt's environment, budgets and usage, and why the
//! run started.
//!
//! A manifest is a pure function of the stored run record, serialized with
//! sorted keys, so exporting the same run twice gives identical bytes. It
//! carries no logs, no report text, no secrets (every string passes the
//! application's redactor), and no absolute paths: inputs and outputs are
//! project-relative, and free text has the project folder, the home folder,
//! and any other absolute path replaced.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::registry::{Containment, PortType};
use exposition_graph::workflow::{ParamValue, StepOperation};
use exposition_graph::{OperationRef, WorkflowId, WorkflowRunId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::adapters::sha256_hex;
use crate::environment::WorkflowEnvironment;
use crate::model::{
    WorkflowAttemptState, WorkflowOutputProvenance, WorkflowOutputValue, WorkflowRejectedOutput, WorkflowRun,
    WorkflowRunBudgets, WorkflowRunCause, WorkflowRunError, WorkflowRunState, WorkflowRunUsage, WorkflowStepOutput,
    WorkflowStepState,
};
use crate::service::WorkflowRunService;

/// Schema of a run manifest.
pub const MANIFEST_SCHEMA: &str = "exposition.workflowRunManifest/1";
/// An exported manifest is never larger than this.
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowManifestInput {
    pub id: String,
    pub label: String,
    pub port_type: PortType,
    /// Project-relative.
    pub path: WorkspacePath,
    pub sha256: String,
    pub bytes: u64,
    /// The plan pinned this digest (the definition, or a reproduction).
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowManifestExecution {
    pub restricted_backend: Option<String>,
    pub restricted_image: Option<String>,
    pub restricted_image_id: Option<String>,
    pub native_steps: Vec<String>,
    pub model_steps: Vec<String>,
    /// R/Julia runners' own images and identities (absent when none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runner_images: Option<Vec<crate::model::WorkflowRunnerImage>>,
}

/// One validated output (or an output value a step reported).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowManifestOutput {
    pub step: String,
    pub port: String,
    pub port_type: PortType,
    /// `file`, `retained`, `formalEvidence`, `report`, `promotionProposal`,
    /// `associationProposal`.
    pub kind: String,
    /// Project-relative file the value names, if any.
    pub path: Option<WorkspacePath>,
    /// SHA-256 (hex) of the file, or of a report's text.
    pub sha256: Option<String>,
    pub bytes: Option<u64>,
    pub media_type: Option<String>,
    /// Verdict of formal evidence, status of a proposal.
    pub status: Option<String>,
    pub provenance: WorkflowOutputProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowManifestAttempt {
    pub attempt: u32,
    pub state: WorkflowAttemptState,
    pub operation: Option<OperationRef>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub intent_at_ms: u64,
    pub dispatched_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub environment: Option<WorkflowEnvironment>,
    pub outputs: Vec<WorkflowManifestOutput>,
    pub rejected: Vec<WorkflowRejectedOutput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowManifestStep {
    pub step: String,
    pub label: String,
    pub operation: StepOperation,
    pub runner: String,
    pub containment: Containment,
    pub state: WorkflowStepState,
    /// SHA-256 (hex) of the step's code or prompt as approved (the full
    /// text is in the definition revision).
    pub payload_sha256: Option<String>,
    pub parameters: BTreeMap<String, ParamValue>,
    pub time_limit_seconds: u32,
    pub attempts: Vec<WorkflowManifestAttempt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowManifestOutcome {
    pub id: String,
    pub label: String,
    pub optional: bool,
    pub step: String,
    pub port: String,
    pub realized: bool,
    pub sha256: Option<String>,
}

/// The manifest of one run (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunManifest {
    pub schema: String,
    /// `Exposition <version>` that wrote the manifest.
    pub generator: String,
    pub run: WorkflowRunId,
    pub cause: WorkflowRunCause,
    pub workflow: WorkflowId,
    pub title: String,
    /// Display name of the project folder (never its path).
    pub project: String,
    pub revision: u64,
    pub definition_hash: String,
    pub approval_hash: String,
    pub state: WorkflowRunState,
    pub code: Option<String>,
    pub message: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub inputs: Vec<WorkflowManifestInput>,
    pub execution: WorkflowManifestExecution,
    pub budgets: WorkflowRunBudgets,
    pub usage: WorkflowRunUsage,
    pub steps: Vec<WorkflowManifestStep>,
    /// Validated outputs of every step's final successful attempt.
    pub outputs: Vec<WorkflowManifestOutput>,
    pub outcomes: Vec<WorkflowManifestOutcome>,
}

/// Where an exported manifest went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowManifestExport {
    pub path: String,
    /// SHA-256 (hex) of the bytes written.
    pub sha256: String,
    pub bytes: u64,
}

fn proposal_status(status: crate::model::WorkflowProposalStatus) -> String {
    serde_json::to_value(status).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

/// The manifest form of a validated output.
fn output(step: &str, o: &WorkflowStepOutput) -> WorkflowManifestOutput {
    let mut m = WorkflowManifestOutput {
        step: step.to_owned(),
        port: o.port.clone(),
        port_type: o.port_type,
        kind: String::new(),
        path: None,
        sha256: None,
        bytes: None,
        media_type: None,
        status: None,
        provenance: o.provenance.clone(),
    };
    match &o.value {
        WorkflowOutputValue::File { path, sha256, bytes, media_type, .. } => {
            m.kind = "file".into();
            m.path = Some(path.clone());
            m.sha256 = Some(crate::adapters::bare_hash(sha256));
            m.bytes = Some(*bytes);
            m.media_type = Some(media_type.clone());
        }
        WorkflowOutputValue::Retained { path, sha256, .. } => {
            m.kind = "retained".into();
            m.path = Some(path.clone());
            m.sha256 = Some(crate::adapters::bare_hash(sha256));
        }
        WorkflowOutputValue::FormalEvidence { file, file_sha256, verdict, .. } => {
            m.kind = "formalEvidence".into();
            m.path = Some(file.clone());
            m.sha256 = Some(crate::adapters::bare_hash(file_sha256));
            m.status = Some(verdict.clone());
        }
        WorkflowOutputValue::Report { text, .. } => {
            // The digest of the text identifies it; the prose stays out.
            m.kind = "report".into();
            m.sha256 = Some(sha256_hex(text.as_bytes()));
            m.bytes = Some(text.len() as u64);
        }
        WorkflowOutputValue::PromotionProposal { document, status, .. } => {
            m.kind = "promotionProposal".into();
            m.path = Some(document.clone());
            m.status = Some(proposal_status(*status));
        }
        WorkflowOutputValue::AssociationProposal { status, .. } => {
            m.kind = "associationProposal".into();
            m.status = Some(proposal_status(*status));
        }
    }
    m
}

/// The manifest of `run` (unscrubbed; see [`scrub_manifest`]).
pub fn build_manifest(run: &WorkflowRun) -> WorkflowRunManifest {
    let plan = &run.plan;
    let mut outputs = Vec::new();
    let steps = run
        .steps
        .iter()
        .map(|step| {
            let planned = plan.steps.iter().find(|p| p.step == step.step);
            let attempts: Vec<WorkflowManifestAttempt> = step
                .attempts
                .iter()
                .map(|a| WorkflowManifestAttempt {
                    attempt: a.attempt,
                    state: a.state,
                    operation: a.operation.clone(),
                    code: a.code.clone(),
                    message: a.message.clone(),
                    intent_at_ms: a.intent_at_ms,
                    dispatched_at_ms: a.dispatched_at_ms,
                    finished_at_ms: a.finished_at_ms,
                    environment: a.environment.clone(),
                    outputs: a.outputs.iter().map(|o| output(&step.step, o)).collect(),
                    rejected: a.rejected.clone(),
                })
                .collect();
            if let Some(last) = step.attempts.iter().rev().find(|a| a.state == WorkflowAttemptState::Succeeded) {
                outputs.extend(last.outputs.iter().map(|o| output(&step.step, o)));
            }
            WorkflowManifestStep {
                step: step.step.clone(),
                label: step.label.clone(),
                operation: planned
                    .map(|p| p.operation.clone())
                    .unwrap_or(StepOperation { id: String::new(), version: 0 }),
                runner: planned.map(|p| p.runner.clone()).unwrap_or_default(),
                containment: step.containment,
                state: step.state,
                payload_sha256: planned
                    .and_then(|p| p.payload.as_ref())
                    .map(|payload| sha256_hex(&serde_json::to_vec(payload).unwrap_or_default())),
                parameters: planned.map(|p| p.parameters.clone()).unwrap_or_default(),
                time_limit_seconds: planned.map(|p| p.time_limit_seconds).unwrap_or(0),
                attempts,
            }
        })
        .collect();
    let outcomes = run
        .outcomes
        .iter()
        .map(|o| {
            let realized = o.value.as_ref().map(|v| {
                output(
                    &o.step,
                    &WorkflowStepOutput {
                        port: o.port.clone(),
                        port_type: PortType::Dataset,
                        value: v.clone(),
                        provenance: WorkflowOutputProvenance {
                            run: run.id.clone(),
                            step: o.step.clone(),
                            attempt: 0,
                            operation: OperationRef { kind: String::new(), id: String::new() },
                            instance: None,
                        },
                    },
                )
            });
            WorkflowManifestOutcome {
                id: o.id.clone(),
                label: o.label.clone(),
                optional: o.optional,
                step: o.step.clone(),
                port: o.port.clone(),
                realized: realized.is_some(),
                sha256: realized.and_then(|r| r.sha256),
            }
        })
        .collect();
    WorkflowRunManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        generator: format!("Exposition {}", crate::environment::APP_VERSION),
        run: run.id.clone(),
        cause: run.cause.clone(),
        workflow: run.workflow.clone(),
        title: run.title.clone(),
        project: plan.project.clone(),
        revision: run.revision,
        definition_hash: run.definition_hash.clone(),
        approval_hash: plan.approval_hash.clone(),
        state: run.state,
        code: run.code.clone(),
        message: run.message.clone(),
        started_at_ms: run.started_at_ms,
        finished_at_ms: run.finished_at_ms,
        inputs: plan
            .inputs
            .iter()
            .map(|i| WorkflowManifestInput {
                id: i.id.clone(),
                label: i.label.clone(),
                port_type: i.port_type,
                path: i.path.clone(),
                sha256: i.sha256.clone(),
                bytes: i.bytes,
                pinned: i.pinned,
            })
            .collect(),
        execution: WorkflowManifestExecution {
            restricted_backend: plan.execution.restricted_backend.clone(),
            restricted_image: plan.execution.restricted_image.clone(),
            restricted_image_id: plan.execution.restricted_image_id.clone(),
            native_steps: plan.execution.native_steps.clone(),
            model_steps: plan.execution.model_steps.clone(),
            runner_images: plan.execution.runner_images.clone(),
        },
        budgets: run.budgets.clone(),
        usage: run.usage.clone(),
        steps,
        outputs,
        outcomes,
    }
}

// ------------------------------------------------------------ scrubbing ----

/// Folders replaced by a label in free text, plus the redactor.
pub struct ManifestScrub<'a> {
    /// The application's secret redactor (known secrets and token shapes).
    pub redact: &'a dyn Fn(&str) -> String,
    /// `(folder, label)`: e.g. the project root → `<project>`, home → `~`.
    pub folders: Vec<(PathBuf, String)>,
}

fn replace_ignore_case(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    let needle_lower = needle.to_lowercase();
    // Lowercasing may change byte lengths for non-ASCII text: fall back to
    // an exact replacement then.
    if lower.len() != text.len() || needle_lower.len() != needle.len() {
        return text.replace(needle, with);
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(found) = lower[i..].find(&needle_lower) {
        out.push_str(&text[i..i + found]);
        out.push_str(with);
        i += found + needle.len();
    }
    out.push_str(&text[i..]);
    out
}

fn is_path_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|' | '“' | '”' | '‘' | '’' | ',' | ';' | ')' | ']')
}

/// Replace absolute paths (`C:\…`, `\\server\…`, `/home/…`, `/Users/…`,
/// `/root/…`) with `<path>`. Container paths (`/workspace`, `/out`, `/tmp`,
/// `/usr`) are not user paths and stay.
pub fn scrub_absolute_paths(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let prev_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
        let drive = prev_ok
            && chars[i].is_ascii_alphabetic()
            && chars.get(i + 1) == Some(&':')
            && matches!(chars.get(i + 2), Some('\\') | Some('/'));
        let unc = prev_ok && chars[i] == '\\' && chars.get(i + 1) == Some(&'\\');
        let rest: String = chars[i..chars.len().min(i + 8)].iter().collect();
        let posix_home = (i == 0 || chars[i - 1].is_whitespace() || matches!(chars[i - 1], '"' | '\'' | '(' | '='))
            && ["/home/", "/Users/", "/root/"].iter().any(|p| rest.starts_with(p));
        if drive || unc || posix_home {
            let mut j = i;
            loop {
                while j < chars.len() && !is_path_end(chars[j]) {
                    j += 1;
                }
                // A space inside a folder name: continue when the next word
                // is still part of the path (it contains a separator).
                if j < chars.len() && chars[j] == ' ' {
                    let mut k = j + 1;
                    while k < chars.len() && !is_path_end(chars[k]) {
                        k += 1;
                    }
                    if chars[j + 1..k].iter().any(|c| *c == '\\' || *c == '/') {
                        j = k;
                        continue;
                    }
                }
                break;
            }
            out.push_str("<path>");
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

impl ManifestScrub<'_> {
    pub fn text(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (folder, label) in &self.folders {
            let display = folder.display().to_string();
            for form in [display.clone(), display.strip_prefix(r"\\?\").unwrap_or(&display).to_owned()] {
                if form.len() >= 3 {
                    out = replace_ignore_case(&out, &form, label);
                    let forward = form.replace('\\', "/");
                    if forward != form {
                        out = replace_ignore_case(&out, &forward, label);
                    }
                }
            }
        }
        (self.redact)(&scrub_absolute_paths(&out))
    }
}

fn scrub_value(value: &mut serde_json::Value, scrub: &ManifestScrub<'_>) {
    match value {
        serde_json::Value::String(s) => *s = scrub.text(s),
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| scrub_value(v, scrub)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|v| scrub_value(v, scrub)),
        _ => {}
    }
}

/// The manifest as a JSON value with every string scrubbed (redacted,
/// folders and absolute paths replaced). Digests, ids, and project-relative
/// paths are unchanged.
pub fn scrubbed_value(manifest: &WorkflowRunManifest, scrub: &ManifestScrub<'_>) -> serde_json::Value {
    let mut value = serde_json::to_value(manifest).unwrap_or(serde_json::Value::Null);
    scrub_value(&mut value, scrub);
    value
}

/// [`scrubbed_value`] read back as a manifest. Never falls back to the
/// unscrubbed manifest: a scrubbed value that no longer reads is an error.
pub fn scrub_manifest(
    manifest: &WorkflowRunManifest,
    scrub: &ManifestScrub<'_>,
) -> Result<WorkflowRunManifest, WorkflowRunError> {
    serde_json::from_value(scrubbed_value(manifest, scrub)).map_err(|e| {
        WorkflowRunError::new("workflow.manifestUnavailable", format!("The run manifest could not be prepared: {e}"))
    })
}

fn sorted(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            serde_json::Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// Canonical bytes of a JSON value: keys sorted at every level, two-space
/// indentation, LF line ends, a final newline.
pub fn canonical_json(value: serde_json::Value) -> String {
    let mut text = serde_json::to_string_pretty(&sorted(value)).unwrap_or_default();
    text.push('\n');
    text
}

/// Canonical bytes of a manifest (see [`canonical_json`]).
pub fn manifest_json(manifest: &WorkflowRunManifest) -> String {
    canonical_json(serde_json::to_value(manifest).unwrap_or(serde_json::Value::Null))
}

/// Whether `bytes` are an Exposition run manifest (an export may replace one).
fn is_manifest(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("schema").and_then(|s| s.as_str()).map(|s| s.starts_with("exposition.workflowRunManifest/"))
        })
        .unwrap_or(false)
}

/// Write `json` to the user-chosen `destination`: an absolute `.json` path
/// in an existing folder. An existing file is replaced only when it is a
/// regular file holding an earlier run manifest (never a link, a folder, or
/// any other file). The write is atomic.
pub fn write_manifest_file(destination: &Path, json: &str) -> Result<WorkflowManifestExport, WorkflowRunError> {
    let invalid = |m: &str| WorkflowRunError::new("workflow.invalidDestination", m);
    if json.len() > MAX_MANIFEST_BYTES {
        return Err(WorkflowRunError::new("workflow.tooLarge", "The manifest is too large to export."));
    }
    if !destination.is_absolute()
        || destination.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() != Some("json")
    {
        return Err(invalid("Choose an absolute path ending in .json."));
    }
    let parent =
        destination.parent().filter(|p| p.is_dir()).ok_or_else(|| invalid("The destination folder does not exist."))?;
    let name = destination.file_name().ok_or_else(|| invalid("The destination has no file name."))?;
    let target = parent.join(name);
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if !meta.is_file() {
            return Err(invalid("The destination exists and is not a regular file."));
        }
        let existing = std::fs::read(&target).map_err(|e| invalid(&format!("The destination cannot be read: {e}")))?;
        if !is_manifest(&existing) {
            return Err(invalid("Refusing to replace a file that is not an Exposition run manifest."));
        }
    }
    exposition_workspace::fsutil::write_atomic(&target, json.as_bytes()).map_err(|e| {
        WorkflowRunError::new("workflow.exportFailed", format!("The manifest could not be written: {e}"))
    })?;
    Ok(WorkflowManifestExport {
        path: target.display().to_string(),
        sha256: sha256_hex(json.as_bytes()),
        bytes: json.len() as u64,
    })
}

impl WorkflowRunService {
    /// The manifest of a run of the open project, scrubbed.
    pub fn run_manifest(
        &self,
        id: &WorkflowRunId,
        scrub: &ManifestScrub<'_>,
    ) -> Result<WorkflowRunManifest, WorkflowRunError> {
        let run = self.get(id)?;
        scrub_manifest(&build_manifest(&run), scrub)
    }

    /// The canonical JSON of a run's scrubbed manifest (the exported bytes).
    pub fn run_manifest_json(&self, id: &WorkflowRunId, scrub: &ManifestScrub<'_>) -> Result<String, WorkflowRunError> {
        let run = self.get(id)?;
        Ok(canonical_json(scrubbed_value(&build_manifest(&run), scrub)))
    }

    /// Export a run's manifest to `destination` (see [`write_manifest_file`]).
    pub fn export_run_manifest(
        &self,
        id: &WorkflowRunId,
        destination: &Path,
        scrub: &ManifestScrub<'_>,
    ) -> Result<WorkflowManifestExport, WorkflowRunError> {
        write_manifest_file(destination, &self.run_manifest_json(id, scrub)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_paths_are_replaced_and_container_paths_kept() {
        let text = r#"File "C:\Users\Jane Doe\AppData\x.py", line 3; also D:/data/set.csv and \\server\share\f and /home/jane/p.txt; but /workspace/a.py and /out stay"#;
        let out = scrub_absolute_paths(text);
        assert!(!out.contains("Jane") && !out.contains("jane") && !out.contains("server"), "{out}");
        assert!(out.contains("/workspace/a.py") && out.contains("/out stay"), "{out}");
        assert_eq!(out.matches("<path>").count(), 4, "{out}");
        assert_eq!(scrub_absolute_paths("ratio 3:2 and a:b"), "ratio 3:2 and a:b");
    }

    #[test]
    fn folders_are_labelled_case_insensitively() {
        let scrub = ManifestScrub {
            redact: &|t: &str| t.replace("sk-ant-SECRET", "[redacted]"),
            folders: vec![(PathBuf::from(r"C:\Proj\Paper"), "<project>".into())],
        };
        assert_eq!(scrub.text(r"c:\proj\paper\data\x.csv changed"), r"<project>\data\x.csv changed");
        assert_eq!(scrub.text("key sk-ant-SECRET"), "key [redacted]");
    }

    #[test]
    fn exports_refuse_relative_paths_other_extensions_and_foreign_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let json = "{\"schema\": \"exposition.workflowRunManifest/1\"}\n";
        assert!(write_manifest_file(Path::new("m.json"), json).is_err());
        assert!(write_manifest_file(&dir.join("m.txt"), json).is_err());
        assert!(write_manifest_file(&dir.join("missing").join("m.json"), json).is_err());
        let other = dir.join("package.json");
        std::fs::write(&other, "{\"name\": \"x\"}").unwrap();
        let e = write_manifest_file(&other, json).unwrap_err();
        assert!(e.message.contains("not an Exposition run manifest"), "{e:?}");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "{\"name\": \"x\"}", "untouched");
        std::fs::create_dir(dir.join("d.json")).unwrap();
        assert!(write_manifest_file(&dir.join("d.json"), json).is_err());

        let target = dir.join("run.json");
        let first = write_manifest_file(&target, json).unwrap();
        assert_eq!(first.sha256, sha256_hex(json.as_bytes()));
        let again = write_manifest_file(&target, json).expect("an earlier manifest may be replaced");
        assert_eq!(again.bytes, json.len() as u64);
    }
}
