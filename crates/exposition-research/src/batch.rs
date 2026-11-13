//! Workflow-owned Python batch runs (M8 contract; implemented by the research lane),
//! and R and Julia batches through the same runner (M9, [`BatchLanguage`]).
//!
//! Two accurately labelled modes share one request shape:
//! - [`BatchMode::Restricted`]: the scoped research adapter. Only the declared
//!   inputs and the code are staged (copied, hash-verified) into a
//!   service-owned working copy that is mounted read-only; the code writes to
//!   a dedicated output area. Docker backend with no network, no host
//!   credentials/environment/sockets, a configured local image (never pulled),
//!   non-root user, and the existing process/time limits. Undeclared project
//!   files are simply absent (an undeclared import fails visibly). Never falls
//!   back to native execution.
//! - [`BatchMode::TrustedNative`]: a fresh workflow-owned native Python
//!   session with the user's account privileges (never the user's own
//!   kernel; not a sandbox).
//!
//! Output registration (both modes): only regular files directly produced in
//! the output area, with safe relative names, bounded count and sizes, no
//! links/junctions/reparse points or hard-link tricks, SHA-256 recorded; an
//! output port `p` is realized by the file `p.<ext>` with an extension the
//! port accepts. Accepted files become artifacts of a research run (manifest
//! in `.exposition/runs`, files in `.exposition/artifacts/<run>`), labelled
//! with their actual execution mode, so retention/promotion reuse the existing
//! paths. Active formats (SVG/HTML) are stored and later rendered inertly.

use std::path::Path;
use std::time::Duration;

use exposition_core::artifact::{ArtifactRecord, RunStatus};
use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ResearchError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum BatchMode {
    Restricted,
    TrustedNative,
}

/// The workflow step that owns the batch (sessions and runs are attributed to it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BatchOwner {
    pub run: String,
    pub step: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum BatchCode {
    Inline {
        source: String,
    },
    /// A declared project script, verified against this hash.
    Script {
        path: WorkspacePath,
        sha256: String,
    },
}

/// A declared input: staged at the same project-relative path, verified
/// against `sha256` (a mismatch refuses the batch: `research.inputChanged`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BatchInput {
    pub path: WorkspacePath,
    pub sha256: String,
}

/// An output port and the file extensions that may realize it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BatchPort {
    pub port: String,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct BatchRequest {
    pub mode: BatchMode,
    pub owner: BatchOwner,
    pub code: BatchCode,
    pub inputs: Vec<BatchInput>,
    pub outputs: Vec<BatchPort>,
    pub time_limit: Duration,
    pub max_output_bytes: u64,
    /// Restricted mode: the image identity (`sha256:…`) the batch was approved
    /// for. The batch is refused unless the configured image still has it, and
    /// the container starts from it. `None` runs whatever the configured image
    /// is when the batch starts (its identity is recorded either way).
    pub restricted_image_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BatchPortOutput {
    pub port: String,
    pub artifact: ArtifactRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BatchRejected {
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BatchResult {
    /// The research run holding the manifest and accepted artifacts.
    pub research_run: String,
    pub status: RunStatus,
    pub exit_code: Option<i32>,
    /// Accepted files that realize output ports.
    pub ports: Vec<BatchPortOutput>,
    /// Other accepted files in the output area (kept as run artifacts).
    pub extra: Vec<ArtifactRecord>,
    pub rejected: Vec<BatchRejected>,
    /// Bounded, redacted stdout/stderr tail.
    pub log: String,
    /// Execution environment as recorded (mode, image, interpreter, limits).
    pub environment: Vec<(String, String)>,
}

/// Cancellation and dispatch callbacks of one batch.
pub struct BatchControl<'a> {
    pub canceled: &'a (dyn Fn() -> bool + Sync),
    /// Called once with the research run id when the batch is dispatched
    /// (the runner persists it before waiting for the outcome).
    pub on_dispatched: &'a (dyn Fn(&str) + Sync),
}

/// Runs workflow-owned Python batches (see the module docs), and R and Julia
/// batches through the `*_language` methods (M9). A runner that does not
/// implement a language refuses it: never another language or mode.
pub trait BatchRunner: Send + Sync {
    /// Whether `mode` is usable for `root` now, and why not.
    fn availability(&self, root: &Path, mode: BatchMode) -> Result<(), String>;
    /// Run one batch to completion (or cancellation/time limit).
    fn run(
        &self,
        root: &Path,
        request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError>;

    /// [`Self::availability`] of a `language` batch.
    fn language_availability(&self, root: &Path, language: BatchLanguage, mode: BatchMode) -> Result<(), String> {
        match language {
            BatchLanguage::Python => self.availability(root, mode),
            other => Err(format!("{} batches are not supported by this batch runner.", other.title())),
        }
    }

    /// [`Self::run`] for a `language` batch: the code is `language` code
    /// (inline, or a declared script with the language's extension).
    fn run_language(
        &self,
        root: &Path,
        language: BatchLanguage,
        request: &BatchRequest,
        control: &BatchControl<'_>,
    ) -> Result<BatchResult, ResearchError> {
        match language {
            BatchLanguage::Python => self.run(root, request, control),
            other => Err(ResearchError::coded(
                "research.languageUnsupported",
                format!("{} batches are not supported by this batch runner; nothing was run.", other.title()),
            )),
        }
    }

    /// The image restricted `language` batches run in now: its reference
    /// and identity (`sha256:…`, when the engine reports it). `Err`: why
    /// restricted batches of the language are unavailable. Never pulls.
    fn restricted_image(&self, language: BatchLanguage) -> Result<(String, Option<String>), String> {
        Err(format!("restricted {} batches are not supported by this batch runner.", language.title()))
    }
}

/// The language of a batch's code (M9: R and Julia join Python).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum BatchLanguage {
    Python,
    R,
    Julia,
}

impl BatchLanguage {
    pub const ALL: [BatchLanguage; 3] = [BatchLanguage::Python, BatchLanguage::R, BatchLanguage::Julia];

    pub fn title(self) -> &'static str {
        match self {
            BatchLanguage::Python => "Python",
            BatchLanguage::R => "R",
            BatchLanguage::Julia => "Julia",
        }
    }

    /// Script extensions of the language (lowercase, compared without
    /// regard to case: `.R` and `.r` are both R).
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            BatchLanguage::Python => &["py"],
            BatchLanguage::R => &["r"],
            BatchLanguage::Julia => &["jl"],
        }
    }

    /// `a Python script (.py)`, `an R script (.R)`, `a Julia script (.jl)`.
    pub fn script_description(self) -> &'static str {
        match self {
            BatchLanguage::Python => "a Python script (.py)",
            BatchLanguage::R => "an R script (.R)",
            BatchLanguage::Julia => "a Julia script (.jl)",
        }
    }

    /// Whether `file_name` is a script of this language.
    pub fn is_script(self, file_name: &str) -> bool {
        file_name.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && self.extensions().contains(&ext.to_ascii_lowercase().as_str())
        })
    }
}
