//! Incremental reconciliation of the project graph with the project's files.
//!
//! - [`scan`]: bounded scanning under the inclusion policy (no links followed).
//! - [`extract`]: versioned, literal extractors (LaTeX, Typst, Markdown, Lean,
//!   BibTeX, Python/R/Julia imports, attachment markers).
//! - [`matching`]: declaration identity across edits (names, fingerprints,
//!   context; never ranges alone).
//! - [`anchor`]: re-anchoring explicit region bindings by their text.
//! - [`overlay`]: unsaved buffers, extracted and matched like saved files,
//!   never written (revision-labelled).
//! - [`driver`]: the background thread subscribed to workspace, document,
//!   and graph events.
//! - [`engine`]: one pass (scan → read/extract without the lock → plan and
//!   write in one transaction); [`resolve`] and [`structure`] are its link
//!   resolution and folder/containment steps.

pub mod anchor;
pub mod driver;
pub mod engine;
pub mod extract;
pub mod matching;
pub mod overlay;
mod resolve;
pub mod scan;
mod structure;

pub use driver::{ReconcilerHandle, spawn};
pub use engine::{ReconcileOptions, ReconcileReport, Scope};
pub use overlay::OverlayEntry;
