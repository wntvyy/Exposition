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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use exposition_core::build::{
    BuildConfiguration, BuildId, BuildResult, BuildStatus, BuildTarget, DocumentFormat, Engine, ExportArtifact,
    ExportKind, MappingPrecision, PageArtifact, Severity,
};
use exposition_core::document::DocumentId;
use exposition_core::path::WorkspacePath;
use exposition_page::{PageImage, PageMap, PageMapProvider};
use exposition_workspace::WorkspaceService;
use typst_layout::PagedDocument;

use crate::fonts::Fonts;
use crate::snapshot::{BuildSnapshot, ReadFilter, sha256_hex};
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

/// Compiler context of one workspace root (kept while the root stays open,
/// so rebuilds are incremental).
struct CompilerContext {
    root: PathBuf,
    compiler: TypstCompiler,
}

#[derive(Default)]
struct Store {
    recent: VecDeque<Arc<BuildRecord>>,
    last_good: HashMap<WorkspacePath, Arc<BuildRecord>>,
    svgs: VecDeque<(String, Arc<String>)>,
}

pub struct TypesettingService {
    workspace: Arc<WorkspaceService>,
    /// Loaded on first use (system font discovery can take a moment).
    fonts: OnceLock<Arc<Fonts>>,
    load_fonts: fn() -> Arc<Fonts>,
    context: Mutex<Option<Arc<CompilerContext>>>,
    store: Mutex<Store>,
    counter: AtomicU64,
}

impl TypesettingService {
    /// Builds read the open workspace of `workspace` (open buffers first, then
    /// disk through its path checks). `load_fonts` provides the font set on
    /// first use, e.g. [`Fonts::with_system`] or [`Fonts::embedded_only`].
    pub fn new(workspace: Arc<WorkspaceService>, load_fonts: fn() -> Arc<Fonts>) -> Self {
        Self {
            workspace,
            fonts: OnceLock::new(),
            load_fonts,
            context: Mutex::new(None),
            store: Mutex::new(Store::default()),
            counter: AtomicU64::new(0),
        }
    }

    pub fn fonts(&self) -> &Arc<Fonts> {
        self.fonts.get_or_init(self.load_fonts)
    }

    pub fn workspace(&self) -> &Arc<WorkspaceService> {
        &self.workspace
    }

    /// The compiler context for `root`; a different root starts a new one
    /// and forgets the previous workspace's builds.
    fn context_for(&self, root: &Path) -> Arc<CompilerContext> {
        let mut context = self.context.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = context.as_ref().filter(|c| c.root == root) {
            return Arc::clone(existing);
        }
        let fresh = Arc::new(CompilerContext {
            root: root.to_path_buf(),
            compiler: TypstCompiler::new(Arc::clone(self.fonts())),
        });
        *context = Some(Arc::clone(&fresh));
        *self.store.lock().unwrap_or_else(|p| p.into_inner()) = Store::default();
        fresh
    }

    /// Forget compiler state and builds (e.g. when the workspace closes).
    pub fn reset(&self) {
        *self.context.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.store.lock().unwrap_or_else(|p| p.into_inner()) = Store::default();
    }

    /// Compile `entry` now from a fresh snapshot (open buffers override disk).
    pub fn build(&self, target: &BuildTarget) -> Result<Arc<BuildRecord>, TypesettingError> {
        self.build_with(target, None)
    }

    /// Compile `entry` reading only the files `filter` allows (an agent task's
    /// scope): any other file reads as "access denied", so neither the output
    /// nor the diagnostics can carry its content. The record is returned but
    /// not retained for page views, which always build the whole project.
    pub fn build_scoped(&self, target: &BuildTarget, filter: ReadFilter) -> Result<Arc<BuildRecord>, TypesettingError> {
        self.build_with(target, Some(filter))
    }

    fn build_with(
        &self,
        target: &BuildTarget,
        filter: Option<ReadFilter>,
    ) -> Result<Arc<BuildRecord>, TypesettingError> {
        // Typst, and Markdown through its print profile (translated to Typst).
        let markdown = match (target.format, target.engine) {
            (DocumentFormat::Typst, Engine::Typst) => false,
            (DocumentFormat::Markdown, Engine::MarkdownPrint) => true,
            _ => return Err(TypesettingError::Unsupported(format!("{:?}/{:?}", target.format, target.engine))),
        };
        let started = Instant::now();
        let snapshot = BuildSnapshot::capture(&self.workspace).map_err(|_| TypesettingError::NoWorkspace)?;
        let retain = filter.is_none();
        let snapshot = match filter {
            Some(filter) => snapshot.with_filter(filter),
            None => snapshot,
        };
        let context = self.context_for(snapshot.root());
        let snapshot_ms = started.elapsed().as_secs_f64() * 1000.0;
        let output = if markdown {
            crate::markdown::compile(&context.compiler, &target.entry, &snapshot)
        } else {
            context.compiler.compile(&target.entry, &snapshot)
        };
        let configuration = BuildConfiguration {
            engine: target.engine,
            compiler: COMPILER.into(),
            font_fingerprint: self.fonts().fingerprint().to_owned(),
            profile: markdown.then(|| crate::markdown::PROFILE_ID.to_owned()),
        };
        let fingerprint = format!(
            "{}|{}|{}|{}",
            configuration.compiler,
            configuration.font_fingerprint,
            configuration.profile.as_deref().unwrap_or(""),
            target.entry
        );
        // Build ids are unique across formats' providers.
        let prefix = if markdown { "markdown" } else { "typst" };
        let id = BuildId(format!("{prefix}-{}", self.counter.fetch_add(1, Ordering::Relaxed) + 1));
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
        if !retain {
            return Ok(record);
        }
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
                // The display root (no verbatim prefix) of the open folder.
                let root = PathBuf::from(self.workspace.current().ok_or(TypesettingError::NoWorkspace)?.root);
                let mut path = root;
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

impl PageMapProvider for TypesettingService {
    fn page_map(&self, build: &str) -> Option<Arc<PageMap>> {
        self.record(build)?.map.clone()
    }

    fn page_image(&self, render_ref: &str) -> Option<PageImage> {
        let svg = self.page_svg(render_ref)?;
        Some(PageImage { media_type: "image/svg+xml", bytes: Arc::from(svg.as_bytes()) })
    }

    fn revision_text(&self, document: &DocumentId, revision: u64) -> Option<Arc<str>> {
        TypesettingService::revision_text(self, document, revision)
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("pdf.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}
