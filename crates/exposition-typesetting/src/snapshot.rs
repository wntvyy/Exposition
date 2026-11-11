//! Immutable build input snapshots.
//!
//! A snapshot captures every open document (text and revision) atomically from
//! the document service when the build starts; open buffers override disk, so
//! unsaved changes compile. Other workspace files are read through the
//! resolver at most once per snapshot and then stay fixed for the build. The
//! snapshot identity hashes every file the compiler actually read together
//! with the build configuration.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use exposition_core::build::{SnapshotId, SourceRevision};
use exposition_core::document::DocumentId;
use exposition_core::path::WorkspacePath;
use exposition_workspace::DocumentService;
use sha2::{Digest, Sha256};

use crate::resolver::{ResolveError, SourceResolver};

/// An open document captured into a snapshot.
#[derive(Debug, Clone)]
pub struct Overlay {
    pub document: DocumentId,
    pub revision: u64,
    pub text: Arc<str>,
}

/// Contents of one file as compiled.
#[derive(Debug, Clone)]
pub enum FileContent {
    /// Text of an open document.
    Buffer(Overlay),
    /// Bytes read from disk.
    Disk(Arc<Vec<u8>>),
}

impl FileContent {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Buffer(overlay) => overlay.text.as_bytes(),
            Self::Disk(bytes) => bytes,
        }
    }
}

#[derive(Debug)]
pub struct BuildSnapshot {
    overlays: HashMap<WorkspacePath, Overlay>,
    resolver: Arc<dyn SourceResolver>,
    /// Files read so far, in path order. Once read, a file never changes
    /// within this snapshot.
    read: Mutex<BTreeMap<WorkspacePath, Result<FileContent, ResolveError>>>,
    /// Unix seconds at capture (the build's notion of "today").
    pub captured_at: u64,
}

impl BuildSnapshot {
    /// Capture all open documents with a workspace path.
    pub fn capture(documents: &DocumentService, resolver: Arc<dyn SourceResolver>) -> Self {
        let overlays = documents
            .snapshots()
            .into_iter()
            .filter_map(|s| {
                let path = s.path?;
                Some((path, Overlay { document: s.id, revision: s.revision, text: s.text }))
            })
            .collect();
        let captured_at =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Self { overlays, resolver, read: Mutex::new(BTreeMap::new()), captured_at }
    }

    pub fn resolver(&self) -> &Arc<dyn SourceResolver> {
        &self.resolver
    }

    pub fn overlay(&self, path: &WorkspacePath) -> Option<&Overlay> {
        self.overlays.get(path)
    }

    /// Read a file (open buffer first, then disk) and record the access.
    pub fn read(&self, path: &WorkspacePath) -> Result<FileContent, ResolveError> {
        let mut read = self.read.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = read.get(path) {
            return existing.clone();
        }
        let content = match self.overlays.get(path) {
            Some(overlay) => Ok(FileContent::Buffer(overlay.clone())),
            None => self.resolver.read(path).map(|bytes| FileContent::Disk(Arc::new(bytes))),
        };
        read.insert(path.clone(), content.clone());
        content
    }

    /// The source revisions of every file successfully read, in path order.
    pub fn sources(&self) -> Vec<SourceRevision> {
        let read = self.read.lock().unwrap_or_else(|p| p.into_inner());
        read.iter()
            .filter_map(|(path, content)| {
                let content = content.as_ref().ok()?;
                let (document, revision) = match content {
                    FileContent::Buffer(o) => (Some(o.document.clone()), Some(o.revision)),
                    FileContent::Disk(_) => (None, None),
                };
                Some(SourceRevision {
                    path: path.clone(),
                    document,
                    revision,
                    content_hash: sha256_hex(content.bytes()),
                })
            })
            .collect()
    }

    /// Text of every open document read by the build, by (document, revision).
    pub fn buffer_texts(&self) -> Vec<(DocumentId, u64, WorkspacePath, Arc<str>)> {
        let read = self.read.lock().unwrap_or_else(|p| p.into_inner());
        read.iter()
            .filter_map(|(path, content)| match content {
                Ok(FileContent::Buffer(o)) => Some((o.document.clone(), o.revision, path.clone(), Arc::clone(&o.text))),
                _ => None,
            })
            .collect()
    }

    /// Content-derived identity: every file read (path + hash, including
    /// failed reads) and the configuration fingerprint.
    pub fn id(&self, configuration: &str) -> SnapshotId {
        let mut hasher = Sha256::new();
        hasher.update(configuration.as_bytes());
        let read = self.read.lock().unwrap_or_else(|p| p.into_inner());
        for (path, content) in read.iter() {
            hasher.update([0u8]);
            hasher.update(path.as_str().as_bytes());
            hasher.update([0u8]);
            match content {
                Ok(c) => hasher.update(Sha256::digest(c.bytes())),
                Err(e) => hasher.update(e.to_string().as_bytes()),
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
