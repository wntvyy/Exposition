//! Workflow runs (M8): reviewable plans, the bounded DAG runner, durable
//! attempts, output validation, and step adapters. See [`model`] for the
//! run contract shared with the UI.

pub mod model;

pub use model::*;
