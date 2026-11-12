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
//! - [`audit`]: the axiom audit of every revision Lean reports without errors.
//!
//! Statuses are distinct by construction: [`LeanStatus::Checked`] means Lean
//! elaborated exactly that revision completely with no errors, and the axiom
//! audit found no `sorry` (including imported or suppressed ones), kernel
//! checking enabled, and only the standard axioms. Incomplete proofs, extra
//! axioms, compiled-code trust, partial (`#exit`) and unverified checks,
//! errors, missing toolchains, untrusted workspaces, and server problems are
//! separate states.

pub mod audit;
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
