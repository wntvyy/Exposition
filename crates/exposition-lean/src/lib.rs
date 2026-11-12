//! Lean 4 support for Exposition.
//!
//! - [`project`]: Lake project detection and `lean-toolchain` pins.
//! - [`toolchain`]: resolving pins to installed elan toolchains (never
//!   installing implicitly).
//! - [`lsp`]: a small LSP client over an execution-service interactive job.
//! - [`LeanService`]: server lifecycle (trusted launch, crash detection and
//!   restart with backoff), document synchronization from the document service
//!   (unsaved buffers, revisions as versions), per-revision checking status,
//!   goals/hover/definition, toolchain installation as an explicit action, and
//!   the [`LeanService::check`] seam for agents.
//!
//! Statuses are distinct by construction: [`LeanStatus::Checked`] means Lean
//! finished exactly that revision with no errors and no `sorry`;
//! [`LeanStatus::CheckedWithSorry`] marks incomplete proofs; errors, missing
//! toolchains, untrusted workspaces, and crashes are separate states.

pub mod convert;
pub mod lsp;
pub mod project;
mod server;
mod service;
pub mod toolchain;
pub mod types;
pub mod uri;

pub use service::{LeanConfig, LeanService};
pub use types::*;
