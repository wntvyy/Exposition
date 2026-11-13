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
//! * [`sandbox`] runs workflow-owned Python batches ([`batch::BatchRunner`]):
//!   restricted (Docker, scoped working copy) or trusted native, with
//!   controlled output registration.
//!
//! Processes are launched only through [`launcher::KernelProcessLauncher`]
//! (kernels) or the execution service (batches).
//! Research never constructs document source text: inserting artifacts into
//! documents is the format-owned `exposition-attach` transformation.

pub mod batch;
pub mod discovery;
pub mod error;
pub mod helper;
pub mod launcher;
pub mod project;
pub mod protocol;
mod runs;
pub mod sandbox;
mod service;
pub mod store;
pub mod types;

pub use error::ResearchError;
pub use project::ResearchProject;
pub use runs::media_type_for;
pub use service::{ResearchConfig, ResearchService, ResolvedSource, SourceReader, StartSessionRequest, hash_file};
