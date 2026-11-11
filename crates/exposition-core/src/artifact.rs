//! Artifact/run contract and the artifact attachment command shape.
//!
//! Research runs, builds, and agent tasks produce artifacts (figures, tables,
//! data). A [`RunRecord`] captures provenance. Large outputs are referenced by
//! path, never copied repeatedly through IPC.
//!
//! Inserting or updating an artifact in a document is a format-owned source
//! transformation: research supplies an [`AttachArtifactRequest`]; the
//! document's format adapter turns it into a document transaction. Research
//! never constructs LaTeX/Typst/Markdown insertion text itself.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::document::{DocumentId, EditOrigin};
use crate::path::WorkspacePath;
use crate::text::TextRange;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArtifactId(pub String);

/// What produced a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Producer {
    /// Interactive or scripted execution in a research kernel session.
    KernelSession { session_id: String, runtime: String },
    /// A supervised process job.
    Job { job_id: String },
    /// A typesetting build.
    Build { build_id: String },
    /// An agent task (which may itself have used the producers above).
    AgentTask { task_id: String },
}

/// An input that a run consumed, pinned by revision or content hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunInput {
    pub path: WorkspacePath,
    pub revision: Option<u64>,
    pub content_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ToolVersion {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactKind {
    Figure,
    Table,
    Data,
    Document,
    Log,
}

/// A produced file, referenced by location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRecord {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    /// Workspace-relative location (artifacts that belong to the project) or
    /// `None` for application-scratch artifacts referenced by `scratch_path`.
    pub path: Option<WorkspacePath>,
    pub scratch_path: Option<String>,
    pub media_type: String,
    pub size_bytes: u64,
    pub content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed { message: String },
    Canceled,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: RunId,
    pub producer: Producer,
    pub inputs: Vec<RunInput>,
    /// Runtime, interpreter, and tool versions the run actually used.
    pub environment: Vec<ToolVersion>,
    pub outputs: Vec<ArtifactRecord>,
    pub status: RunStatus,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
}

/// Where an attachment goes in the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AttachmentPlacement {
    /// Insert a new attachment at a UTF-16 offset of `base_revision`.
    Insert { offset: u32 },
    /// Update an attachment previously inserted by the format adapter, located
    /// by its stable attachment key (e.g. a label) in the current source.
    Update { attachment_key: String },
}

/// Format-neutral request to insert or update an artifact in a document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AttachArtifactRequest {
    pub document: DocumentId,
    pub base_revision: u64,
    pub artifact: ArtifactRecord,
    pub run: Option<RunId>,
    pub placement: AttachmentPlacement,
    pub caption: Option<String>,
    /// Stable key/label; the adapter derives one when absent.
    pub attachment_key: Option<String>,
    pub origin: EditOrigin,
}

/// Result of a format-owned attachment transformation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AttachArtifactOutcome {
    pub document: DocumentId,
    pub revision: u64,
    pub attachment_key: String,
    /// Source range of the inserted/updated attachment in the new revision.
    pub range: TextRange,
}
