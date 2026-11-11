//! The typesetting service: per-workspace compiler contexts, build records
//! (result + layout + glyph map + snapshot texts), page rendering, and export.
//!
//! Build records are immutable and identified by build id. Recent records and
//! the last successful record per entry are retained, so a page view keeps
//! the last good output (marked stale) while a build fails, and page edits
//! based on the revision a map was built from can still be planned.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use exposition_core::build::{
    BuildConfiguration, BuildId, BuildResult, BuildStatus, BuildTarget, DocumentFormat, Engine, ExportArtifact,
    ExportKind, MappingPrecision, PageArtifact, Severity,
};
use exposition_core::document::DocumentId;
use exposition_core::path::WorkspacePath;
use exposition_page::PageMap;
use exposition_workspace::DocumentService;
use typst_layout::PagedDocument;

use crate::fonts::Fonts;
use crate::resolver::{DiskResolver, SourceResolver};
use crate::snapshot::{BuildSnapshot, sha256_hex};
use crate::typst_adapter::{self, COMPILER, TypstCompiler};

/// Recent build records retained (beyond the last good one per entry).
const RECENT_BUILDS: usize = 8;
/// Rendered SVG pages retained.
const SVG_CACHE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TypesettingError {
    #[error("no workspace is open")]
    NoWorkspace,
    #[error("unsupported build target: {0}")]
    Unsupported(String),
    #[error("build {0} is not available")]
    UnknownBuild(String),
    #[error("build {0} has no output")]
    NoOutput(String),
    #[error("export failed: {0}")]
    Export(String),
    #[error("invalid destination: {0}")]
    InvalidDestination(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct BuildTimings {
    pub snapshot_ms: f64,
    pub compile_ms: f64,
    pub map_ms: f64,
    pub total_ms: f64,
}

/// One immutable build: the contract result plus layout and mapping.
pub struct BuildRecord {
    pub result: BuildResult,
    pub entry: WorkspacePath,
    pub document: Option<Arc<PagedDocument>>,
    pub map: Option<Arc<PageMap>>,
    /// Texts of the open documents compiled, by (document, revision).
    pub texts: Vec<(DocumentId, u64, WorkspacePath, Arc<str>)>,
    pub timings: BuildTimings,
}

impl BuildRecord {
    pub fn id(&self) -> &str {
        &self.result.id.0
    }

    pub fn succeeded(&self) -> bool {
        matches!(self.result.status, BuildStatus::Succeeded)
    }

    /// Revision of `document` this build compiled, if it was an open buffer.
    pub fn revision_of(&self, document: &DocumentId) -> Option<u64> {
        self.texts.iter().find(|(d, ..)| d == document).map(|(_, r, ..)| *r)
    }

    pub fn text_of(&self, document: &DocumentId) -> Option<&Arc<str>> {
        self.texts.iter().find(|(d, ..)| d == document).map(|(.., t)| t)
    }
}

struct WorkspaceContext {
    root: PathBuf,
    resolver: Arc<dyn SourceResolver>,
    compiler: TypstCompiler,
}

#[derive(Default)]
struct Store {
    recent: VecDeque<Arc<BuildRecord>>,
    last_good: HashMap<WorkspacePath, Arc<BuildRecord>>,
    svgs: VecDeque<(String, Arc<String>)>,
}

pub struct TypesettingService {
    documents: Arc<DocumentService>,
    fonts: Arc<Fonts>,
    workspace: RwLock<Option<Arc<WorkspaceContext>>>,
    store: Mutex<Store>,
    counter: AtomicU64,
}

impl TypesettingService {
    pub fn new(documents: Arc<DocumentService>, fonts: Arc<Fonts>) -> Self {
        Self {
            documents,
            fonts,
            workspace: RwLock::new(None),
            store: Mutex::new(Store::default()),
            counter: AtomicU64::new(0),
        }
    }

    pub fn documents(&self) -> &Arc<DocumentService> {
        &self.documents
    }

    /// Use the workspace at `root` (canonicalized). Reopening the same root
    /// keeps the compiler context and its caches.
    pub fn open_workspace(&self, root: &Path) -> std::io::Result<PathBuf> {
        let resolver = DiskResolver::new(root)?;
        let canonical = resolver.root().to_path_buf();
        if self.workspace_root().as_deref() == Some(canonical.as_path()) {
            return Ok(canonical);
        }
        self.open_workspace_with(Arc::new(resolver));
        Ok(canonical)
    }

    /// Use a workspace provided by another resolver (e.g. the workspace service).
    pub fn open_workspace_with(&self, resolver: Arc<dyn SourceResolver>) {
        let context = WorkspaceContext {
            root: resolver.root().to_path_buf(),
            resolver,
            compiler: TypstCompiler::new(Arc::clone(&self.fonts)),
        };
        *self.workspace.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(context));
        *self.store.lock().unwrap_or_else(|p| p.into_inner()) = Store::default();
    }

    pub fn workspace_root(&self) -> Option<PathBuf> {
        self.workspace.read().unwrap_or_else(|p| p.into_inner()).as_ref().map(|w| w.root.clone())
    }

    fn context(&self) -> Result<Arc<WorkspaceContext>, TypesettingError> {
        self.workspace.read().unwrap_or_else(|p| p.into_inner()).clone().ok_or(TypesettingError::NoWorkspace)
    }

    /// Compile `entry` now from a fresh snapshot (open buffers override disk).
    pub fn build(&self, target: &BuildTarget) -> Result<Arc<BuildRecord>, TypesettingError> {
        if target.format != DocumentFormat::Typst || target.engine != Engine::Typst {
            return Err(TypesettingError::Unsupported(format!("{:?}/{:?}", target.format, target.engine)));
        }
        let context = self.context()?;
        let started = Instant::now();
        let snapshot = BuildSnapshot::capture(&self.documents, Arc::clone(&context.resolver));
        let snapshot_ms = started.elapsed().as_secs_f64() * 1000.0;
        let output = context.compiler.compile(&target.entry, &snapshot);
        let configuration = BuildConfiguration {
            engine: Engine::Typst,
            compiler: COMPILER.into(),
            font_fingerprint: self.fonts.fingerprint().to_owned(),
            profile: None,
        };
        let fingerprint = format!("{}|{}|{}", configuration.compiler, configuration.font_fingerprint, target.entry);
        let id = BuildId(format!("b{}", self.counter.fetch_add(1, Ordering::Relaxed) + 1));
        let failed = output.document.is_none() || output.diagnostics.iter().any(|d| d.severity == Severity::Error);
        let pages = output
            .document
            .as_ref()
            .map(|doc| {
                doc.pages()
                    .iter()
                    .enumerate()
                    .map(|(i, page)| PageArtifact {
                        index: i as u32,
                        width_pt: page.frame.width().to_pt(),
                        height_pt: page.frame.height().to_pt(),
                        render_ref: typst_adapter::page_hash(doc, i).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let total_ms = started.elapsed().as_secs_f64() * 1000.0;
        let result = BuildResult {
            id,
            target: target.clone(),
            snapshot: snapshot.id(&fingerprint),
            sources: snapshot.sources(),
            configuration: Some(configuration),
            status: if failed { BuildStatus::Failed } else { BuildStatus::Succeeded },
            diagnostics: output.diagnostics,
            pages,
            mapping: if output.map.is_some() { MappingPrecision::Glyph } else { MappingPrecision::None },
            exports: Vec::new(),
            duration_ms: total_ms.round() as u64,
        };
        let record = Arc::new(BuildRecord {
            result,
            entry: target.entry.clone(),
            document: if failed { None } else { output.document },
            map: if failed { None } else { output.map },
            texts: snapshot.buffer_texts().into_iter().collect(),
            timings: BuildTimings { snapshot_ms, compile_ms: output.compile_ms, map_ms: output.map_ms, total_ms },
        });
        let mut store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        store.recent.push_back(Arc::clone(&record));
        while store.recent.len() > RECENT_BUILDS {
            store.recent.pop_front();
        }
        if record.succeeded() {
            store.last_good.insert(target.entry.clone(), Arc::clone(&record));
        }
        Ok(record)
    }

    /// A retained build by id.
    pub fn record(&self, id: &str) -> Option<Arc<BuildRecord>> {
        let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        store.recent.iter().rev().chain(store.last_good.values()).find(|r| r.id() == id).cloned()
    }

    /// The last successful build of an entry.
    pub fn last_good(&self, entry: &WorkspacePath) -> Option<Arc<BuildRecord>> {
        self.store.lock().unwrap_or_else(|p| p.into_inner()).last_good.get(entry).cloned()
    }

    /// The most recent build of an entry (successful or not).
    pub fn latest(&self, entry: &WorkspacePath) -> Option<Arc<BuildRecord>> {
        let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        store.recent.iter().rev().find(|r| &r.entry == entry).cloned()
    }

    /// Text of a document revision compiled by a retained build.
    pub fn revision_text(&self, document: &DocumentId, revision: u64) -> Option<Arc<str>> {
        let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        store
            .recent
            .iter()
            .rev()
            .chain(store.last_good.values())
            .flat_map(|r| r.texts.iter())
            .find(|(d, rev, ..)| d == document && *rev == revision)
            .map(|(.., text)| Arc::clone(text))
    }

    /// SVG for a page render reference (page content hash) of a retained build.
    pub fn page_svg(&self, render_ref: &str) -> Option<Arc<String>> {
        let source = {
            let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((_, svg)) = store.svgs.iter().find(|(r, _)| r == render_ref) {
                return Some(Arc::clone(svg));
            }
            store.recent.iter().rev().chain(store.last_good.values()).find_map(|record| {
                let index = record.result.pages.iter().position(|p| p.render_ref == render_ref)?;
                Some((Arc::clone(record.document.as_ref()?), index))
            })
        };
        let (document, index) = source?;
        let svg = Arc::new(typst_adapter::render_svg(&document, index)?);
        let mut store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        store.svgs.push_back((render_ref.to_owned(), Arc::clone(&svg)));
        while store.svgs.len() > SVG_CACHE {
            store.svgs.pop_front();
        }
        Some(svg)
    }

    /// Export a build's document to PDF from the same layout used for its pages.
    /// Without a destination, writes `<entry stem>.pdf` next to the entry in
    /// the workspace. The file is written atomically (temp file + rename).
    pub fn export_pdf(&self, build: &str, destination: Option<PathBuf>) -> Result<ExportArtifact, TypesettingError> {
        let record = self.record(build).ok_or_else(|| TypesettingError::UnknownBuild(build.into()))?;
        let document = record.document.as_ref().ok_or_else(|| TypesettingError::NoOutput(build.into()))?;
        let bytes = typst_adapter::export_pdf(document).map_err(TypesettingError::Export)?;
        let path = match destination {
            Some(path) => {
                if !path.is_absolute()
                    || path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() != Some("pdf")
                {
                    return Err(TypesettingError::InvalidDestination("an absolute .pdf path is required".into()));
                }
                let parent = path.parent().filter(|p| p.is_dir()).ok_or_else(|| {
                    TypesettingError::InvalidDestination("the destination folder does not exist".into())
                })?;
                parent.join(path.file_name().expect("has extension"))
            }
            None => {
                let context = self.context()?;
                let mut path = context.root.clone();
                for component in record.entry.components() {
                    path.push(component);
                }
                path.set_extension("pdf");
                path
            }
        };
        write_atomically(&path, &bytes).map_err(|e| TypesettingError::Export(e.to_string()))?;
        Ok(ExportArtifact { kind: ExportKind::Pdf, path: path.display().to_string(), content_hash: sha256_hex(&bytes) })
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("pdf.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}
