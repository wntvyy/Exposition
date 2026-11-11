//! Research environments connected to the paper.
//!
//! * [`discovery`] finds Python interpreters (with jupyter_client/ipykernel),
//!   R + IRkernel, and Julia + IJulia, with setup guidance when missing.
//! * [`ResearchService`] supervises one kernel host helper per session (the
//!   bundled `workers/kernel-host` Python helper around jupyter_client), queues
//!   runs, streams bounded output, interrupts/cancels/restarts, and detects
//!   crashes of the kernel or the helper.
//! * Every run is recorded as a portable [`types::RunManifest`] inside the
//!   project (see [`store`] for the layout); figures and tables are artifacts
//!   referenced by path, never copied repeatedly through IPC.
//!
//! Processes are launched only through [`launcher::KernelProcessLauncher`].
//! Research never constructs document source text: inserting artifacts into
//! documents is the format-owned `exposition-attach` transformation.

pub mod discovery;
pub mod error;
pub mod helper;
pub mod launcher;
pub mod protocol;
mod runs;
mod service;
pub mod store;
pub mod types;

pub use error::ResearchError;
pub use runs::media_type_for;
pub use service::{ResearchConfig, ResearchService, StartSessionRequest};
