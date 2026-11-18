//! Agent tasks for Exposition.
//!
//! One user request becomes a persistent task run by a small in-app loop: the
//! model provider adapter (Anthropic Claude, raw HTTPS from Rust), structured
//! tools that call the same application services people use (the document
//! service for every change), permission profiles and budgets enforced in
//! code, cancellation, persistence in SQLite, and change review with
//! checkpoints and conflict-aware revert.

pub mod attachments;
pub mod catalog;
pub mod credentials;
pub mod graph;
pub mod model;
pub mod patch;
pub mod permissions;
pub mod prompt;
pub mod provider;
pub mod questions;
pub mod review;
pub mod service;
pub mod store;
pub mod tools;
#[cfg(windows)]
pub mod wincred;
pub mod workspace;

pub use model::*;
pub use questions::{Question, QuestionAnswer, QuestionOption, QuestionRequest, QuestionRequestState, QuestionValue};
pub use service::{AgentConfig, AgentError, AgentService};
