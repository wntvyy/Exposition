//! Shared helpers for typesetting integration tests (real compiler, real
//! document service, fixtures from `tests/fixtures`).
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use exposition_core::build::{BuildTarget, DocumentFormat, Engine};
use exposition_core::document::{DocumentId, TextEncoding, TransactionId};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, utf16_len};
use exposition_page::{PageEditOp, PageEditOutcome, PageEditRequest, TypstInput, apply_page_edit};
use exposition_typesetting::{BuildRecord, Fonts, TypesettingService};
use exposition_workspace::{DocumentService, OpenDocument};

pub fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

pub struct Fixture {
    pub documents: Arc<DocumentService>,
    pub service: TypesettingService,
    pub document: DocumentId,
    pub path: WorkspacePath,
    pub target: BuildTarget,
}

impl Fixture {
    /// Open `entry` of fixture `name` as an in-memory document (the fixture on
    /// disk is never modified).
    pub fn open(name: &str, entry: &str) -> Self {
        let dir = fixture_dir(name);
        let documents = Arc::new(DocumentService::new());
        let service = TypesettingService::new(Arc::clone(&documents), Fonts::embedded_only);
        service.open_workspace(&dir).expect("fixture dir");
        let path = WorkspacePath::parse(entry).unwrap();
        let text = std::fs::read_to_string(dir.join(entry)).unwrap();
        let info = documents.open(OpenDocument {
            path: Some(path.clone()),
            text,
            language: "typst".into(),
            encoding: TextEncoding::Utf8,
            read_only: None,
        });
        let target = BuildTarget { format: DocumentFormat::Typst, engine: Engine::Typst, entry: path.clone() };
        Self { documents, service, document: info.id, path, target }
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
