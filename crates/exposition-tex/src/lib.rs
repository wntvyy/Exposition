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

#[cfg(test)]
mod thread_safety {
    use super::*;

    fn send_sync<T: Send + Sync>() {}

    /// Builds and page sessions can move to and be shared across worker threads.
    #[test]
    fn builder_session_and_mappings_are_send_and_sync() {
        send_sync::<TexBuilder<LocalProcessRunner>>();
        send_sync::<TexPageSession>();
        send_sync::<PageMapping>();
        send_sync::<TexBuild>();
    }
}
