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

use std::collections::BTreeMap;
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
    /// Given to the batch as [`SEED_VAR`] (decimal) and recorded with its
    /// environment. The workflow runner derives it from the run, step, and
    /// instance; it never carries anything secret.
    pub seed: Option<u64>,
    /// The instance index of a repeated (forEach/until) step, given to the
    /// batch as [`ITERATION_VAR`].
    pub iteration: Option<u32>,
    /// The step's declared parameters as text (a forEach value already
    /// applied), each given to the batch as `EXPOSITION_PARAM_<NAME>` (see
    /// [`parameter_variable`]). Names must be identifiers; values one line,
    /// at most [`MAX_PARAMETER_BYTES`], and never a secret.
    pub parameters: BTreeMap<String, String>,
}

/// Environment variable carrying [`BatchRequest::seed`].
pub const SEED_VAR: &str = "EXPOSITION_SEED";
/// Environment variable carrying [`BatchRequest::iteration`].
pub const ITERATION_VAR: &str = "EXPOSITION_ITERATION";
/// Prefix of the variables carrying [`BatchRequest::parameters`].
pub const PARAMETER_PREFIX: &str = "EXPOSITION_PARAM_";
/// Longest parameter value given to a batch.
pub const MAX_PARAMETER_BYTES: usize = 4096;

/// `EXPOSITION_PARAM_<UPPER_SNAKE_NAME>` for a parameter name (`timeoutSeconds`
/// → `EXPOSITION_PARAM_TIMEOUT_SECONDS`, `value` → `EXPOSITION_PARAM_VALUE`),
/// or `None` when the name is not an identifier of at most 64 characters.
pub fn parameter_variable(name: &str) -> Option<String> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return None;
    }
    let mut out = String::from(PARAMETER_PREFIX);
    let mut previous_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() && previous_lower {
            out.push('_');
        }
        previous_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        out.push(c.to_ascii_uppercase());
    }
    Some(out)
}

/// Keys of [`BatchResult::environment`] that workflow environment capture
/// reads (other keys describe limits and containment for display).
pub mod environment_keys {
    /// Configured image reference and its identity (`sha256:…`).
    pub const IMAGE: &str = "image";
    pub const IMAGE_ID: &str = "imageId";
    /// Operating system and CPU architecture the code ran on (the image's
    /// platform for restricted batches).
    pub const OS: &str = "os";
    pub const ARCH: &str = "arch";
    /// Interpreter implementation (`cpython`, …) and version.
    pub const IMPLEMENTATION: &str = "implementation";
    pub const PYTHON_VERSION: &str = "pythonVersion";
    /// The interpreter of any batch language (`python`, `R`, `julia`) and its
    /// version when known (R and Julia batches report only these).
    pub const INTERPRETER_NAME: &str = "interpreterName";
    pub const INTERPRETER_VERSION: &str = "interpreterVersion";
    /// One entry per installed package: `name==version`.
    pub const PACKAGE: &str = "package";
    /// Where the package list came from (shown with it).
    pub const PACKAGES_SOURCE: &str = "packagesSource";
    /// The package list was cut at its bound.
    pub const PACKAGES_TRUNCATED: &str = "packagesTruncated";
    /// [`super::SEED_VAR`] as given to the batch.
    pub const SEED: &str = "seed";
    /// Anything else the capture needs to say (e.g. why packages are unknown).
    pub const NOTE: &str = "note";
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
    /// The environment a batch of `mode` would get now, in the
    /// [`environment_keys`] form, from what is already known (configured
    /// image identity, cached image package lists, the cached discovery
    /// report). Never runs anything to find out; unknown parts are omitted.
    fn current_environment(&self, _root: &Path, _mode: BatchMode) -> Vec<(String, String)> {
        Vec::new()
    }

    /// [`Self::current_environment`] of a `language` batch.
    fn current_language_environment(
        &self,
        root: &Path,
        language: BatchLanguage,
        mode: BatchMode,
    ) -> Vec<(String, String)> {
        match language {
            BatchLanguage::Python => self.current_environment(root, mode),
            _ => Vec::new(),
        }
    }

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
