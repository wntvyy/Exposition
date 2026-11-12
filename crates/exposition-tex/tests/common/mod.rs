//! Shared helpers for tests that run the real TeX engines.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::build::{BuildTarget, DocumentFormat, Engine};
use exposition_core::document::TextEncoding;
use exposition_core::path::WorkspacePath;
use exposition_tex::{BuildMode, LocalProcessRunner, TexBuildRequest, TexBuilder, TexTool, TexToolchain};
use exposition_workspace::{DocumentService, OpenDocument};

/// The toolchain under test. Missing engines skip the calling test with an
/// explicit message, unless `EXPOSITION_REQUIRE_TEX=1` makes that a failure.
pub fn toolchain(engine: Engine) -> Option<TexToolchain> {
    let explicit = std::env::var_os("EXPOSITION_TEX_BIN").map(PathBuf::from);
    let found = TexToolchain::discover(explicit.as_deref());
    let tool = TexTool::for_engine(engine).expect("TeX engine");
    match found {
        Some(t) if t.tool(tool).is_some() => Some(t),
        _ => {
            let message = format!("SKIPPED: {} not found (set EXPOSITION_TEX_BIN or install TeX)", tool.name());
            if std::env::var("EXPOSITION_REQUIRE_TEX").as_deref() == Ok("1") {
                panic!("{message}");
            }
            eprintln!("{message}");
            None
        }
    }
}

pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

/// Copy a fixture into a fresh project directory.
pub fn project_from_fixture(name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    copy_dir(&fixture_dir(name), dir.path());
    dir
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

pub struct Harness {
    pub project: tempfile::TempDir,
    pub build_root: tempfile::TempDir,
    pub builder: TexBuilder<LocalProcessRunner>,
    pub documents: DocumentService,
    pub engine: Engine,
}

impl Harness {
    pub fn new(fixture: &str, engine: Engine) -> Option<Self> {
        let toolchain = toolchain(engine)?;
        let project = project_from_fixture(fixture);
        let build_root = tempfile::tempdir().unwrap();
        let runner = LocalProcessRunner::new(vec![build_root.path().to_path_buf()]);
        let builder = TexBuilder::new(runner, Some(toolchain), build_root.path().to_path_buf()).unwrap();
        Some(Self { project, build_root, builder, documents: DocumentService::new(), engine })
    }

    /// Open a project file as a document (as the editor would).
    pub fn open(&self, path: &str) -> exposition_core::document::DocumentId {
        let text = std::fs::read_to_string(self.project.path().join(path)).unwrap();
        self.documents
            .open(OpenDocument {
                path: Some(WorkspacePath::parse(path).unwrap()),
                text,
                language: "latex".into(),
                encoding: TextEncoding::Utf8,
                read_only: None,
            })
            .id
    }

    pub fn request(&self, mode: BuildMode) -> TexBuildRequest {
        TexBuildRequest {
            target: BuildTarget {
                format: DocumentFormat::Latex,
                engine: self.engine,
                entry: WorkspacePath::parse("main.tex").unwrap(),
            },
            project_root: self.project.path().to_path_buf(),
            buffers: self.documents.snapshots(),
            mode,
            tounicode: false,
            workspace: None,
        }
    }

    pub fn text(&self, id: &exposition_core::document::DocumentId) -> Arc<str> {
        self.documents.snapshot(id).unwrap().text
    }
}
