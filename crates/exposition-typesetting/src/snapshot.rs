//! Immutable build input snapshots.
//!
//! A build reads its inputs through one [`WorkspaceSnapshot`]: every open
//! buffer as of the moment the build started (so unsaved changes compile),
//! and other files from disk through the workspace's canonical-root and
//! link-escape checks — including files the compiler discovers while
//! compiling (imports, images). Each file is read at most once per snapshot
//! and then stays fixed for the build. The snapshot identity hashes every file
//! the compiler actually read together with the build configuration.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use exposition_core::build::{SnapshotId, SourceRevision};
use exposition_core::document::DocumentId;
use exposition_core::path::WorkspacePath;
use exposition_workspace::{BuildInput, WorkspaceService, WorkspaceSnapshot};
use sha2::{Digest, Sha256};

/// Why a build input could not be read (message safe to show).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub struct BuildSnapshot {
    workspace: WorkspaceSnapshot,
    /// Files read so far, in path order. Once read, a file never changes
    /// within this snapshot.
    read: Mutex<BTreeMap<WorkspacePath, Result<BuildInput, ReadError>>>,
    /// Unix seconds at capture (the build's notion of "today").
    pub captured_at: u64,
}

impl BuildSnapshot {
    /// Capture the open workspace (all open buffers atomically).
    pub fn capture(workspace: &WorkspaceService) -> Result<Self, ReadError> {
        let snapshot =
            workspace.snapshot().map_err(|e| ReadError { code: e.code.to_owned(), message: e.message.clone() })?;
        Ok(Self::new(snapshot))
    }

    pub fn new(workspace: WorkspaceSnapshot) -> Self {
        let captured_at =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Self { workspace, read: Mutex::new(BTreeMap::new()), captured_at }
    }

    /// Canonical workspace root.
    pub fn root(&self) -> &Path {
        self.workspace.root()
    }

    /// Read a file (open buffer first, then disk) and record the access.
    pub fn read(&self, path: &WorkspacePath) -> Result<BuildInput, ReadError> {
        let mut read = self.read.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = read.get(path) {
            return existing.clone();
        }
        let input =
            self.workspace.read(path).map_err(|e| ReadError { code: e.code.to_owned(), message: e.message.clone() });
        read.insert(path.clone(), input.clone());
        input
    }

    /// The source revisions of every file successfully read, in path order.
    pub fn sources(&self) -> Vec<SourceRevision> {
        let read = self.read.lock().unwrap_or_else(|p| p.into_inner());
        read.iter()
            .filter_map(|(path, input)| {
                let input = input.as_ref().ok()?;
                Some(SourceRevision {
                    path: path.clone(),
                    document: input.document.clone(),
                    revision: input.revision,
                    content_hash: input.content_hash.clone(),
                })
            })
            .collect()
    }

    /// Text of every open document read by the build, by (document, revision).
    pub fn buffer_texts(&self) -> Vec<(DocumentId, u64, WorkspacePath, Arc<str>)> {
        let read = self.read.lock().unwrap_or_else(|p| p.into_inner());
        read.iter()
            .filter_map(|(path, input)| {
                let input = input.as_ref().ok()?;
                Some((input.document.clone()?, input.revision?, path.clone(), Arc::clone(input.text.as_ref()?)))
            })
            .collect()
    }

    /// Content-derived identity: every file read (path + hash, including
    /// failed reads) and the configuration fingerprint.
    pub fn id(&self, configuration: &str) -> SnapshotId {
        let mut hasher = Sha256::new();
        hasher.update(configuration.as_bytes());
        let read = self.read.lock().unwrap_or_else(|p| p.into_inner());
        for (path, input) in read.iter() {
            hasher.update([0u8]);
            hasher.update(path.as_str().as_bytes());
            hasher.update([0u8]);
            match input {
                Ok(input) => hasher.update(input.content_hash.as_bytes()),
                Err(e) => hasher.update(e.code.as_bytes()),
            }
        }
        SnapshotId(hex(&hasher.finalize()[..16]))
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
