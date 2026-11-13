//! Job contract.
//!
//! A job is one supervised process launch: an explicit executable plus an
//! argument array (never an interpolated shell string), a deliberately
//! constructed environment, a validated working directory, and an execution
//! policy. The execution service owns launch, lifecycle events, cancellation,
//! bounded/redacted logs, and process-tree cleanup.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JobId(pub String);

/// Where and how a process may run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ExecutionPolicy {
    /// Runs natively with the user's account privileges. Requires explicit trust
    /// of the workspace (or an explicit per-launch user action).
    TrustedNative,
    /// Runs inside a tested OS/container isolation backend. If the backend is
    /// unavailable the job is refused; it is never downgraded to native.
    Restricted {
        backend: String,
        /// Pin the job to this exact image identity (`sha256:…`). The job is
        /// refused unless the configured image currently has this identity,
        /// and the container is started from the identity, not the tag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        image_id: Option<String>,
    },
}

/// The environment a child receives. Nothing is inherited implicitly: only the
/// named variables are copied from the host, then `set` is applied. Provider
/// credentials are never eligible for inheritance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSpec {
    pub inherit: Vec<String>,
    pub set: BTreeMap<String, String>,
}

/// A request to launch a process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct JobSpec {
    /// Human-readable label for job lists.
    pub label: String,
    /// Absolute path of a discovered/configured executable, or a bare tool
    /// name the execution service resolves through toolchain discovery.
    pub program: String,
    pub args: Vec<String>,
    pub environment: EnvironmentSpec,
    /// Absolute working directory; must lie inside an open workspace root or an
    /// application-owned scratch/build directory.
    pub working_directory: String,
    pub policy: ExecutionPolicy,
    /// Wall-clock limit; `None` uses the service default.
    pub timeout_ms: Option<u64>,
    /// Retained output limit per stream; older output is dropped first.
    pub log_limit_bytes: Option<u64>,
    /// Optional data written to the child's stdin, then closed.
    pub stdin: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// Terminal state of a job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobOutcome {
    /// The process exited on its own; `code` may be non-zero.
    Exited {
        code: i32,
    },
    /// Terminated by a signal or abnormal termination without an exit code.
    Terminated {
        description: String,
    },
    Canceled,
    TimedOut,
    /// The job could not start (missing executable, policy refusal, invalid
    /// working directory, unavailable isolation backend, ...).
    FailedToStart {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct JobResult {
    pub id: JobId,
    pub outcome: JobOutcome,
    pub duration_ms: u64,
    /// True when retained output was truncated to the log limit.
    pub output_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobEvent {
    Queued {
        id: JobId,
        label: String,
    },
    Started {
        id: JobId,
        pid: Option<u32>,
    },
    /// A bounded, redacted chunk of output.
    Output {
        id: JobId,
        stream: OutputStream,
        text: String,
    },
    CancelRequested {
        id: JobId,
    },
    Finished {
        result: JobResult,
    },
}

/// Snapshot of a job for listings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: JobId,
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
    pub policy: ExecutionPolicy,
    pub started_at_ms: Option<u64>,
    pub result: Option<JobResult>,
}
