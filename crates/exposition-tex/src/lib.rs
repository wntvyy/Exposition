//! LaTeX and LuaLaTeX typesetting for Exposition.
//!
//! * [`compile`] — snapshot builds (unsaved buffers included) with pdfLaTeX or
//!   LuaLaTeX, bibliography and reruns, diagnostics, immutable outputs.
//! * [`runner`] — the [`runner::JobRunner`] seam to the shared execution
//!   service, plus a temporary local runner.
//! * [`synctex`] — SyncTeX parsing, forward and backward search.
//! * [`pdf`] — page rendering and glyph extraction (hayro).
//! * [`latex`], [`normalize`], [`mapping`] — character-level page ↔ source
//!   mapping for the declared construct set.
//! * [`session`] — one build's pages: render, extract, map, hit-test.

pub mod compile;
pub mod latex;
pub mod log;
pub mod mapping;
pub mod normalize;
pub mod pdf;
pub mod runner;
pub mod session;
pub mod synctex;
pub mod toolchain;

pub use compile::{BuildMode, BuildSnapshot, TexBuild, TexBuildRequest, TexBuilder, TexOutput};
pub use mapping::{CaretHit, GlyphTarget, PageMapping, UnmappedReason};
pub use runner::{JobRunner, LocalProcessRunner, ProcessOutput};
pub use session::TexPageSession;
pub use toolchain::{TexTool, TexToolchain};
