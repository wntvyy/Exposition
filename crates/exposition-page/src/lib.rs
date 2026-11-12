//! Exact-page editing core.
//!
//! - [`map`]: rendered glyph clusters linked to source ranges, grouped into
//!   lines (produced by a format adapter for one build snapshot).
//! - [`geometry`]: hit testing, caret placement, navigation over rendered
//!   characters, selection highlights and rendered text.
//! - [`input`]: format-neutral page edit requests and their application as
//!   document transactions.
//! - [`typst_input`]: Typst escaping and structure-verified source patches.
//! - [`mapping`]: offset mapping across revisions.
//! - [`provider`]: the format-neutral seam to adapters' retained builds.
//!
//! Everything except [`typst_input`] is format-neutral: other formats
//! produce the same [`map::Cluster`]s and implement [`PageInputRules`].

pub mod geometry;
pub mod input;
pub mod map;
pub mod mapping;
pub mod provider;
#[cfg(feature = "typst")]
pub mod typst_input;

pub use geometry::{CaretGeometry, CaretPosition, Motion, NavigationResult, PageHit, PageRect, SelectionGeometry};
pub use input::{EditPlan, EditRefusal, PageEditOp, PageEditOutcome, PageEditRequest, PageInputRules, apply_page_edit};
pub use map::{
    Affinity, Cluster, ClusterKind, ClusterSource, EditMode, EquationInfo, NonEditableReason, PageMap, RawPage, Stop,
};
pub use provider::{PageImage, PageMapProvider};
#[cfg(feature = "typst")]
pub use typst_input::TypstInput;
