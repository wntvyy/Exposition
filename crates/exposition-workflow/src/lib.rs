//! Workflow runs (M8): reviewable plans, the bounded DAG runner, durable
//! attempts, output validation, and step adapters. See [`model`] for the
//! run contract shared with the UI.

pub mod adapters;
pub mod environment;
pub mod manifest;
pub mod model;
pub mod plan;
pub mod reproduce;
pub mod runner;
pub mod service;
pub mod store;

pub use model::*;
pub use service::{PlanOptions, WorkflowHost, WorkflowRunConfig, WorkflowRunService};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod repro_tests;
#[cfg(test)]
mod tests;
