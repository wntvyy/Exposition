//! Exposition execution service.
//!
//! Every external process the application runs — TeX builds, kernels, Lean,
//! version probes, agent-requested commands — goes through
//! [`ExecutionService`]. It implements the shared job contract
//! (`exposition_core::job`):
//!
//! - **Explicit launch.** `program` is an absolute executable or a discovered
//!   tool id; `args` is an array passed directly to the process (never a shell
//!   string). On Windows, `.bat`/`.cmd` files and `cmd.exe` are refused because
//!   cmd.exe re-parses its command line as shell syntax; only `.exe` programs run.
//! - **Validated working directory.** Absolute, no `..`, canonical (symlinks and
//!   junctions resolved), inside a registered workspace root or
//!   application-owned directory.
//! - **Clean environment.** Built from empty; credential-like names can never be
//!   inherited or set (see [`env`] and [`credentials`]).
//! - **Policy.** `TrustedNative` needs the owning workspace to be trusted
//!   ([`trust`]); application probes (fixed version queries in an app-owned
//!   directory) are exempt. `Restricted { backend: "docker" }` runs in a locked
//!   down container ([`docker`]) and is refused — never downgraded — when the
//!   backend is unavailable.
//! - **Containment.** Windows job objects (kill-on-close, assigned while the
//!   child is suspended) or Unix process groups ([`process`]). Cancel, timeout,
//!   primary-process exit, and application exit kill the whole tree.
//! - **Bounded, redacted output** ([`log`]) with rate-limited events.
//! - **Interactive jobs** for protocol sessions (kernel helper, language
//!   servers): stdin kept open, stdout delivered to the owner as ordered bytes.

pub mod credentials;
pub mod docker;
pub mod env;
pub mod log;
pub mod paths;
pub mod process;
pub mod service;
pub mod settings;
pub mod toolchains;
pub mod trust;

pub use log::{OutputChunk, OutputPage};
pub use service::{
    ExecutionConfig, ExecutionService, InputClosed, InteractiveInput, InteractiveJob, InteractiveOptions,
    InteractiveOutput, JobKind, JobRefusal, JobState, JobSummary, ProtocolLog, RefusalCode,
};
pub use settings::{DockerSettings, ExecutionSettings};
pub use toolchains::{Located, ToolSource, ToolStatus, ToolchainInfo};
pub use trust::{TrustError, TrustStatus};
