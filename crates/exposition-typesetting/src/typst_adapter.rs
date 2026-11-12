//! The Typst compiler adapter.
//!
//! A [`TypstCompiler`] is a long-lived per-workspace compiler context: it
//! keeps the standard library, the shared font set, and parsed sources (edited
//! incrementally with `Source::replace`), so comemo memoization makes rebuilds
//! after small edits incremental. Each compile runs over one immutable
//! [`BuildSnapshot`] through a short-lived [`World`] implementation.
//!
//! Packages are only read from an existing local Typst package directory; a
//! missing package is a diagnostic, never a download.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use exposition_core::build::{Diagnostic, Severity, SourceLocation};
use exposition_core::path::WorkspacePath;
use exposition_page::PageMap;
use typst::diag::{FileError, FileResult, PackageError, SourceDiagnostic, Warned};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::package::PackageSpec;
use typst::syntax::{FileId, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World, WorldExt};
use typst_layout::PagedDocument;

use crate::fonts::Fonts;
use crate::glyphs;
use crate::snapshot::{BuildSnapshot, ReadError};

pub const COMPILER: &str = "typst 0.15.1";

/// Long-lived Typst compiler context for one workspace.
pub struct TypstCompiler {
    library: LazyHash<Library>,
    fonts: Arc<Fonts>,
    /// Parsed sources by file, reparsed incrementally between builds.
    sources: Mutex<HashMap<FileId, Source>>,
    /// Local package directories (data first, then cache).
    package_dirs: Vec<PathBuf>,
    /// Serializes compiles in this context.
    compile_lock: Mutex<()>,
}

impl std::fmt::Debug for TypstCompiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypstCompiler").field("fonts", &self.fonts).finish_non_exhaustive()
    }
}

/// Output of one compile.
pub struct TypstOutput {
    pub document: Option<Arc<PagedDocument>>,
    pub diagnostics: Vec<Diagnostic>,
    pub map: Option<Arc<PageMap>>,
    pub compile_ms: f64,
    pub map_ms: f64,
}

impl TypstCompiler {
    pub fn new(fonts: Arc<Fonts>) -> Self {
        let package_dirs = [dirs::data_dir(), dirs::cache_dir()]
            .into_iter()
            .flatten()
            .map(|d| d.join("typst").join("packages"))
            .collect();
        Self {
            library: LazyHash::new(Library::default()),
            fonts,
            sources: Mutex::new(HashMap::new()),
            package_dirs,
            compile_lock: Mutex::new(()),
        }
    }

    pub fn fonts(&self) -> &Arc<Fonts> {
        &self.fonts
    }

    /// Compile `entry` from `snapshot`, producing the paged document, its
    /// diagnostics, and its glyph map.
    pub fn compile(&self, entry: &WorkspacePath, snapshot: &BuildSnapshot) -> TypstOutput {
        self.compile_with(entry, snapshot, None, &|raw| PageMap::build(raw.files, raw.equations, raw.pages))
    }

    /// Compile `entry` with `main_text` (when given) as its Typst source in
    /// place of the file's own text — the translation of another format, e.g.
    /// Markdown — and build the page map with `finish`. Diagnostics and page
    /// map ranges in `entry` are then in `main_text` coordinates; the caller
    /// maps them back to the file's text. Nothing is written to the workspace.
    pub fn compile_with(
        &self,
        entry: &WorkspacePath,
        snapshot: &BuildSnapshot,
        main_text: Option<Arc<str>>,
        finish: &dyn Fn(glyphs::RawExtraction) -> PageMap,
    ) -> TypstOutput {
        let _guard = self.compile_lock.lock().unwrap_or_else(|p| p.into_inner());
        let main = file_id(entry);
        let world =
            SnapshotWorld { compiler: self, snapshot, main, main_text, today: civil_date(snapshot.captured_at) };
        let started = Instant::now();
        let Warned { output, warnings } = typst::compile::<PagedDocument>(&world);
        let compile_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut diagnostics = Vec::new();
        let (document, errors) = match output {
            Ok(document) => (Some(Arc::new(document)), Vec::new()),
            Err(errors) => (None, errors.into_iter().collect::<Vec<_>>()),
        };
        for diag in errors.iter().chain(warnings.iter()) {
            diagnostics.push(convert_diagnostic(&world, diag));
        }
        let started = Instant::now();
        let map = document.as_ref().map(|doc| Arc::new(finish(glyphs::extract_raw(&world, doc))));
        let map_ms = started.elapsed().as_secs_f64() * 1000.0;
        // Keep memoized results of recent compiles only.
        comemo::evict(10);
        TypstOutput { document, diagnostics, map, compile_ms, map_ms }
    }

    fn package_root(&self, spec: &PackageSpec) -> Option<PathBuf> {
        self.package_dirs.iter().find_map(|base| {
            let dir = base.join(spec.namespace.as_str()).join(spec.name.as_str()).join(spec.version.to_string());
            dir.is_dir().then(|| dunce_canonicalize(&dir).ok()).flatten()
        })
    }

    fn package_file(&self, spec: &PackageSpec, vpath: &VirtualPath) -> FileResult<Vec<u8>> {
        let root = self.package_root(spec).ok_or_else(|| FileError::Package(PackageError::NotFound(spec.clone())))?;
        let full = vpath.realize(&root).map_err(FileError::Realize)?;
        let canonical =
            dunce_canonicalize(&full).map_err(|e| FileError::from_io(e, Path::new(vpath.get_with_slash())))?;
        if !canonical.starts_with(&root) {
            return Err(FileError::AccessDenied);
        }
        std::fs::read(&canonical).map_err(|e| FileError::from_io(e, Path::new(vpath.get_with_slash())))
    }
}

/// The file id of a workspace path in the project root.
pub fn file_id(path: &WorkspacePath) -> FileId {
    let vpath = VirtualPath::new(path.as_str()).expect("workspace paths are valid virtual paths");
    typst::syntax::RootedPath::new(VirtualRoot::Project, vpath).intern()
}

fn workspace_path(id: FileId) -> FileResult<WorkspacePath> {
    WorkspacePath::parse(id.vpath().get_without_slash()).map_err(|_| FileError::AccessDenied)
}

fn file_error(path: &WorkspacePath, error: ReadError) -> FileError {
    match error.code.as_str() {
        // Report workspace-relative paths only.
        "path.notFound" | "path.brokenLink" => FileError::NotFound(PathBuf::from(path.as_str())),
        "path.outsideWorkspace" | "file.permissionDenied" | crate::snapshot::OUTSIDE_SCOPE => FileError::AccessDenied,
        "file.isDirectory" => FileError::IsDirectory,
        _ => FileError::Other(Some(error.message.into())),
    }
}

/// Canonicalize without the Windows verbatim prefix when the plain form is
/// equivalent, so prefix checks use ordinary paths.
fn dunce_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let text = canonical.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\")
            && rest.len() < 260
        {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(canonical)
}

struct SharedBytes(Arc<[u8]>);

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

struct SharedText(Arc<str>);

impl AsRef<str> for SharedText {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

struct SnapshotWorld<'a> {
    compiler: &'a TypstCompiler,
    snapshot: &'a BuildSnapshot,
    main: FileId,
    /// Typst source standing in for the main file (a translated format).
    main_text: Option<Arc<str>>,
    today: (i32, u8, u8),
}

impl SnapshotWorld<'_> {
    fn text(&self, id: FileId) -> FileResult<Arc<str>> {
        if id == self.main
            && let Some(text) = &self.main_text
        {
            return Ok(Arc::clone(text));
        }
        match id.root() {
            VirtualRoot::Project => {
                let path = workspace_path(id)?;
                let input = self.snapshot.read(&path).map_err(|e| file_error(&path, e))?;
                match input.text {
                    // Decoded per the file's encoding (buffers and BOM/UTF-16 files).
                    Some(text) => Ok(text),
                    None => decode(&input.bytes),
                }
            }
            VirtualRoot::Package(spec) => decode(&self.compiler.package_file(spec, id.vpath())?),
        }
    }
}

fn decode(bytes: &[u8]) -> FileResult<Arc<str>> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    std::str::from_utf8(bytes).map(Arc::from).map_err(|_| FileError::InvalidUtf8)
}

impl World for SnapshotWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.compiler.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.compiler.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        let text = self.text(id)?;
        let mut sources = self.compiler.sources.lock().unwrap_or_else(|p| p.into_inner());
        match sources.get_mut(&id) {
            Some(source) => {
                if source.text() != &*text {
                    source.replace(&text);
                }
                Ok(source.clone())
            }
            None => {
                let source = Source::new(id, text.to_string());
                sources.insert(id, source.clone());
                Ok(source)
            }
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main
            && let Some(text) = &self.main_text
        {
            return Ok(Bytes::from_string(SharedText(Arc::clone(text))));
        }
        match id.root() {
            VirtualRoot::Project => {
                let path = workspace_path(id)?;
                let input = self.snapshot.read(&path).map_err(|e| file_error(&path, e))?;
                match (input.document.is_some(), input.text) {
                    // Open buffers: the text as compiled (not its saved encoding).
                    (true, Some(text)) => Ok(Bytes::from_string(SharedText(text))),
                    _ => Ok(Bytes::new(SharedBytes(input.bytes))),
                }
            }
            VirtualRoot::Package(spec) => Ok(Bytes::new(self.compiler.package_file(spec, id.vpath())?)),
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.compiler.fonts.font(index)
    }

    fn today(&self, offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        let (y, m, d) = match offset {
            None => self.today,
            Some(offset) => {
                let shift = offset.seconds() as i64;
                civil_date((self.snapshot.captured_at as i64 + shift).max(0) as u64)
            }
        };
        Datetime::from_ymd(y, m, d)
    }
}

/// UTC calendar date of a Unix timestamp (days-from-civil inverse).
fn civil_date(unix_seconds: u64) -> (i32, u8, u8) {
    let days = (unix_seconds / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

fn location(world: &SnapshotWorld, span: typst::syntax::DiagSpan) -> Option<SourceLocation> {
    let id = span.id()?;
    if !matches!(id.root(), VirtualRoot::Project) {
        return None;
    }
    let path = workspace_path(id).ok()?;
    let range = world.range(span)?;
    let text = world.text(id).ok()?;
    let range = exposition_core::text::byte_range_to_utf16(&text, range).ok()?;
    Some(SourceLocation { path, range })
}

fn convert_diagnostic(world: &SnapshotWorld, diag: &SourceDiagnostic) -> Diagnostic {
    let severity = match diag.severity {
        typst::diag::Severity::Error => Severity::Error,
        typst::diag::Severity::Warning => Severity::Warning,
    };
    let mut hints: Vec<String> = diag.hints.iter().map(|h| h.v.to_string()).collect();
    if diag.message.contains("package") && diag.message.contains("not found") {
        hints.push(
            "Exposition does not download packages. Install the package into the local Typst package directory \
             (e.g. by compiling once with the Typst CLI) and rebuild."
                .into(),
        );
    }
    for point in diag.trace.iter() {
        if let Some(loc) = location(world, point.span.into()) {
            hints.push(format!("{} ({}:{})", point.v, loc.path, loc.range.start));
        }
    }
    Diagnostic { severity, message: diag.message.to_string(), location: location(world, diag.span), hints }
}

/// Render one page as standalone SVG (text as outlines; no scripts).
pub fn render_svg(document: &PagedDocument, page: usize) -> Option<String> {
    let page = document.pages().get(page)?;
    Some(typst_svg::svg(page, &typst_svg::SvgOptions::default()))
}

/// Export the whole document as PDF from the same layout used for pages.
pub fn export_pdf(document: &PagedDocument) -> Result<Vec<u8>, String> {
    typst_pdf::pdf(document, &typst_pdf::PdfOptions::default())
        .map_err(|errors| errors.iter().map(|e| e.message.to_string()).collect::<Vec<_>>().join("; "))
}

/// Stable content hash of a page (identifies its rendering).
pub fn page_hash(document: &PagedDocument, page: usize) -> Option<String> {
    let page = document.pages().get(page)?;
    Some(format!("{:032x}", typst::utils::hash128(page)))
}

#[cfg(test)]
mod tests {
    use super::civil_date;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(951_782_400), (2000, 2, 29));
        assert_eq!(civil_date(1_794_355_200), (2026, 11, 11));
    }
}
