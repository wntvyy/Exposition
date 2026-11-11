//! Ports to the application services that agent process tools call: the
//! typesetting service (builds), research sessions (Python), Lean, and the
//! execution service (commands). Agent tools call the same services people
//! use; they never launch processes themselves.
//!
//! [`super::execution::ExecutionToolServices`] runs commands and Python on the
//! shared execution service; [`UnavailableServices`] reports every tool as
//! unavailable. Results are never simulated.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use exposition_core::job::ExecutionPolicy;
use exposition_core::path::WorkspacePath;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Typesetting,
    Research,
    Lean,
    Execution,
}

impl ServiceKind {
    pub fn label(self) -> &'static str {
        match self {
            ServiceKind::Typesetting => "document builds (typesetting service)",
            ServiceKind::Research => "Python research sessions",
            ServiceKind::Lean => "Lean checking",
            ServiceKind::Execution => "command execution (execution service)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error("{} is not available in this build of Exposition", .0.label())]
    NotAvailable(ServiceKind),
    #[error("canceled")]
    Canceled,
    #[error("{0}")]
    Failed(String),
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
    pub task_id: String,
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
