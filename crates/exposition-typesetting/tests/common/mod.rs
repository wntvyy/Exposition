//! Shared helpers for typesetting integration tests (real compiler, real
//! workspace and document services, fixtures from `tests/fixtures` copied to a
//! temporary project so they are never modified).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::build::{BuildTarget, DocumentFormat, Engine};
use exposition_core::document::{DocumentId, TransactionId};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, utf16_len};
use exposition_page::{PageEditOp, PageEditOutcome, PageEditRequest, TypstInput, apply_page_edit};
use exposition_typesetting::{BuildRecord, Fonts, TypesettingService};
use exposition_workspace::{DocumentService, WorkspaceService};

pub fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

pub struct Fixture {
    pub documents: Arc<DocumentService>,
    pub workspace: Arc<WorkspaceService>,
    pub service: TypesettingService,
    pub document: DocumentId,
    pub path: WorkspacePath,
    pub target: BuildTarget,
    pub root: PathBuf,
    _temp: tempfile::TempDir,
}

impl Fixture {
    /// Open a temporary copy of fixture `name` as the workspace and `entry`
    /// as a document through the workspace service.
    pub fn open(name: &str, entry: &str) -> Self {
        Self::open_dir(&fixture_dir(name), entry)
    }

    pub fn open_dir(dir: &Path, entry: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        copy_dir(dir, &root);
        let data = temp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let documents = Arc::new(DocumentService::new());
        let workspace = WorkspaceService::new(Arc::clone(&documents), data);
        workspace.open_workspace(&root).expect("open workspace");
        let service = TypesettingService::new(Arc::clone(&workspace), Fonts::embedded_only);
        let path = WorkspacePath::parse(entry).unwrap();
        let opened = workspace.open_document(&path).expect("open document");
        let target = BuildTarget { format: DocumentFormat::Typst, engine: Engine::Typst, entry: path.clone() };
        Self { documents, workspace, service, document: opened.state.info.id, path, target, root, _temp: temp }
    }

    pub fn build(&self) -> Arc<BuildRecord> {
        self.service.build(&self.target).expect("build runs")
    }

    pub fn text(&self) -> String {
        self.documents.state(&self.document).unwrap().text
    }

    pub fn revision(&self) -> u64 {
        self.documents.info(&self.document).unwrap().revision
    }

    /// UTF-16 offset of the first occurrence of `needle` (plus `delta` units).
    pub fn offset_of(&self, needle: &str, delta: u32) -> u32 {
        let text = self.text();
        let byte = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in source"));
        utf16_len(&text[..byte]) as u32 + delta
    }

    pub fn edit(&self, selection: TextRange, op: PageEditOp, group: u32) -> PageEditOutcome {
        let request = PageEditRequest {
            id: TransactionId(format!("t-{}", self.revision())),
            document: self.document.clone(),
            base_revision: self.revision(),
            selection,
            op,
            group,
        };
        apply_page_edit(&self.documents, &TypstInput, &request, &|d, r| self.service.revision_text(d, r))
            .expect("transaction")
    }

    pub fn type_text(&self, at: u32, text: &str) -> u32 {
        let mut caret = at;
        for ch in text.chars() {
            match self.edit(TextRange::empty(caret), PageEditOp::Insert { text: ch.to_string() }, 1) {
                PageEditOutcome::Applied { caret: c, .. } | PageEditOutcome::Unchanged { caret: c, .. } => caret = c,
                other => panic!("typing {ch:?} at {caret}: {other:?}"),
            }
        }
        caret
    }
}
