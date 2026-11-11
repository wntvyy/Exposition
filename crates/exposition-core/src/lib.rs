//! Shared Exposition contracts.
//!
//! This crate defines the four application-wide contracts (document transactions,
//! jobs, build results, and artifact/run records) plus the text-position and
//! workspace-path rules every boundary uses. Generated TypeScript bindings
//! (`src/bindings`) are produced from these types by `cargo test` and are
//! authoritative for the frontend.

pub mod artifact;
pub mod build;
pub mod document;
pub mod job;
pub mod path;
pub mod text;

pub use text::{TextError, TextRange};
