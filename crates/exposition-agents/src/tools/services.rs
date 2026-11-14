//! Ports to the application services that agent process tools call: the
//! typesetting service (builds), research sessions (Python), Lean, and the
//! execution service (commands). Agent tools call the same services people
//! use; they never launch processes themselves.
//!
//! [`super::execution::AppToolServices`] connects the application's services
//! (execution, typesetting, TeX builds, research); [`UnavailableServices`]
//! reports every tool as unavailable. Results are never simulated.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use exposition_core::artifact::ArtifactRecord;
use exposition_core::job::ExecutionPolicy;
use exposition_core::ops::{OpError, OpPlan, OpRequest, OpResult};
use exposition_core::path::WorkspacePath;
use exposition_research::project::types::{AttachmentNotice, AttachmentRecorded};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::model::TaskScope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Typesetting,
    Research,
    Lean,
    Execution,
    /// The application's project operations (P1: delete, copy, restore).
    ProjectOps,
}

impl ServiceKind {
    pub fn label(self) -> &'static str {
        match self {
            ServiceKind::Typesetting => "document builds (typesetting service)",
            ServiceKind::Research => "Python research sessions",
            ServiceKind::Lean => "Lean checking",
            ServiceKind::Execution => "command execution (execution service)",
            ServiceKind::ProjectOps => "project file operations (delete and copy)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error("{} is not available in this build of Exposition", .0.label())]
    NotAvailable(ServiceKind),
    /// A specific capability (e.g. Markdown builds) is not wired in this build.
    #[error("{0} is not available in this build of Exposition")]
    Unsupported(String),
    #[error("canceled")]
    Canceled,
    #[error("{0}")]
    Failed(String),
    /// The service ran and reports a genuine failure (build errors, an
    /// exception, a refusal); the details go to the model as an error result.
    #[error("{0}")]
    Outcome(Value),
}

/// Engine for a LaTeX entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatexEngine {
    /// The project's configured engine for this entry, else pdfLaTeX.
    Auto,
    PdfLatex,
    LuaLatex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    pub program: String,
    pub args: Vec<String>,
    /// Absolute working directory inside the project root.
    pub working_directory: PathBuf,
    /// Canonical project root of the task.
    pub root: PathBuf,
    /// Never `TrustedNative` unless the user approved this launch.
    pub policy: ExecutionPolicy,
    /// Agent task that requested the launch (agent runs use separate sessions).
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonRequest {
    pub code: String,
    /// Agent task that requested the run (agent-owned research sessions).
    pub task_id: String,
    /// Project root of the task (also the working directory).
    pub root: PathBuf,
    pub working_directory: PathBuf,
    pub policy: ExecutionPolicy,
    /// Agent runs use a session separate from the user's.
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeanRequest {
    pub path: WorkspacePath,
    pub policy: ExecutionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRequest {
    pub entry: WorkspacePath,
    pub engine: LatexEngine,
    pub task_id: String,
    /// Canonical project root of the task.
    pub root: PathBuf,
    /// The task's scope: an in-process build reads only files in it.
    pub scope: TaskScope,
    /// Execution policy for builds that launch processes (LaTeX): set only by
    /// the gate, `TrustedNative` only after the user approved the launch.
    /// `None` for in-process builds; a LaTeX build without it is refused.
    pub policy: Option<ExecutionPolicy>,
}

#[async_trait]
pub trait ToolServices: Send + Sync {
    /// Whether the service behind a tool exists in this build.
    fn available(&self, kind: ServiceKind) -> bool;

    /// Name of the tested restricted (container) execution backend, if one is
    /// available. Unattended agent execution requires it.
    fn restricted_backend(&self) -> Option<String>;

    /// Why trusted native execution is not possible in `root` (e.g. the folder
    /// is not trusted), checked before the user is asked to approve a launch.
    fn native_refusal(&self, _root: &Path) -> Option<String> {
        None
    }

    async fn build_document(&self, request: BuildRequest, cancel: CancellationToken) -> Result<Value, ServiceError>;
    async fn run_python(&self, request: PythonRequest, cancel: CancellationToken) -> Result<Value, ServiceError>;
    async fn check_lean(&self, request: LeanRequest, cancel: CancellationToken) -> Result<Value, ServiceError>;
    async fn run_command(&self, request: CommandRequest, cancel: CancellationToken) -> Result<Value, ServiceError>;

    /// The record of a research run's artifact (for attaching it to a document).
    fn research_artifact(&self, _run_id: &str, _artifact_id: &str) -> Result<ArtifactRecord, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Research))
    }

    /// Record an attachment the agent inserted with the format-owned
    /// transform, as a reviewed promotion records its own: the research
    /// project retains the artifact (one resource identity, computational
    /// evidence, provenance), records `manuscript usesAsset artifact`,
    /// journals the operation, and follows it through undo/redo. A failed
    /// graph write is reported as `PendingReconciliation`, never as recorded.
    fn record_attachment(&self, _notice: &AttachmentNotice) -> Result<AttachmentRecorded, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Research))
    }

    /// A task run ended: release what the services hold for it (e.g. the
    /// agent's research sessions).
    async fn task_finished(&self, _task_id: &str) {}

    /// The application's project-operations orchestrator (the same one the
    /// interface uses), when present.
    fn project_operations(&self) -> Option<Arc<dyn ProjectOperations>> {
        None
    }
}

/// The application's project operations (P1) as agent tools use them: the
/// same orchestrator as the interface, with the agent task as the actor.
/// Plans list concrete consequences; applying re-validates the plan and
/// reports every target's actual outcome. Nothing here widens a grant: the
/// central gate has already checked every path against the task's scope and
/// path policy, and the orchestrator refuses what file operations may never
/// touch (and every non-file target for agents).
pub trait ProjectOperations: Send + Sync {
    /// Plan `request` (its actor is the agent task).
    fn plan(&self, request: OpRequest) -> Result<OpPlan, OpError>;
    /// Apply a plan with each choice's default (never a destructive one).
    fn apply(&self, plan: &OpPlan) -> Result<OpResult, OpError>;
    /// Restore a trashed item by its restore token (reverting an agent's
    /// deletion from the review).
    fn restore(&self, token: &str) -> Result<OpResult, OpError>;
}

/// A research-side refusal (with its stable code) as a service outcome.
pub fn research_outcome(code: &str, message: impl Into<String>) -> ServiceError {
    ServiceError::Outcome(json!({"code": code, "error": message.into()}))
}

/// No process services: every call fails with `NotAvailable`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableServices;

#[async_trait]
impl ToolServices for UnavailableServices {
    fn available(&self, _kind: ServiceKind) -> bool {
        false
    }

    fn restricted_backend(&self) -> Option<String> {
        None
    }

    async fn build_document(&self, _: BuildRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Typesetting))
    }

    async fn run_python(&self, _: PythonRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Research))
    }

    async fn check_lean(&self, _: LeanRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Lean))
    }

    async fn run_command(&self, _: CommandRequest, _: CancellationToken) -> Result<Value, ServiceError> {
        Err(ServiceError::NotAvailable(ServiceKind::Execution))
    }
}
