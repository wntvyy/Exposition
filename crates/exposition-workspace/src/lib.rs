//! Workspace and document services.

pub mod diff;
pub mod documents;
pub mod encoding;
pub mod error;
pub mod fsutil;
pub mod recovery;
pub mod root;
pub mod service;
pub mod settings;
pub mod tree;
pub mod types;

pub use documents::{DocumentService, DocumentSnapshot, OpenDocument};
pub use error::{WorkspaceError, WorkspaceResult};
pub use service::{AppSettingsState, BuildInput, ProjectSettingsState, WorkspaceService, WorkspaceSnapshot};
