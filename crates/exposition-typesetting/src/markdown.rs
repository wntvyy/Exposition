//! The Markdown adapter: Markdown documents typeset through the declared
//! print profile (`exposition-markdown-print/1`).
//!
//! A build reads the entry's Markdown from the snapshot (open buffer first),
//! translates it to Typst with a source map, and compiles the generated
//! source *as the entry's own file* — the generated Typst exists only in
//! memory and is never written to the workspace. The glyph map, equations and
//! diagnostics the Typst compiler reports in generated coordinates are then
//! mapped back to the Markdown through the source map: a rendered cluster is
//! editable only when the source map gives it an edit mode (the map's mode
//! wins over the Typst view of the generated markup), and everything else
//! keeps its Markdown range for navigation only.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use exposition_core::build::{Diagnostic, DocumentFormat, Engine, Severity, SourceLocation};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_markdown::{SegmentKind, TranslateOptions, Translation};
use exposition_page::PageInputRules;
use exposition_page::map::{ClusterKind, ClusterSource, EditMode, EquationInfo, NonEditableReason, PageMap};

pub use exposition_markdown::{MarkdownInput, PROFILE_ID};

use crate::FormatAdapter;
use crate::glyphs::RawExtraction;
use crate::snapshot::BuildSnapshot;
use crate::typst_adapter::{TypstCompiler, TypstOutput};

/// The Markdown adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct MarkdownAdapter;

impl FormatAdapter for MarkdownAdapter {
    fn format(&self) -> DocumentFormat {
        DocumentFormat::Markdown
    }

    fn engine(&self) -> Engine {
        Engine::MarkdownPrint
    }

    fn input_rules(&self) -> &dyn PageInputRules {
        &MarkdownInput
    }

    fn handles(&self, path: &WorkspacePath) -> bool {
        is_markdown(path)
    }
}

/// Whether a workspace file is a Markdown document (`.md`, `.markdown`).
pub fn is_markdown(path: &WorkspacePath) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

fn error_output(message: String, entry: &WorkspacePath) -> TypstOutput {
    TypstOutput {
        document: None,
        diagnostics: vec![Diagnostic {
            severity: Severity::Error,
            message,
            location: Some(SourceLocation { path: entry.clone(), range: TextRange::empty(0) }),
            hints: Vec::new(),
        }],
        map: None,
        compile_ms: 0.0,
        map_ms: 0.0,
    }
}

/// Translate and compile a Markdown entry. `compile_ms` includes the
/// translation.
pub(crate) fn compile(compiler: &TypstCompiler, entry: &WorkspacePath, snapshot: &BuildSnapshot) -> TypstOutput {
    let input = match snapshot.read(entry) {
        Ok(input) => input,
        Err(e) => return error_output(e.message, entry),
    };
    let text: Arc<str> = match input.text {
        Some(text) => text,
        None => {
            let bytes = input.bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&input.bytes);
            match std::str::from_utf8(bytes) {
                Ok(text) => Arc::from(text),
                Err(_) => return error_output("the Markdown file is not valid UTF-8".into(), entry),
            }
        }
    };
    let started = Instant::now();
    let translation = exposition_markdown::translate(&text, &TranslateOptions { document: Some(entry.clone()) });
    let translate_ms = started.elapsed().as_secs_f64() * 1000.0;
    let generated: Arc<str> = Arc::from(translation.typst.as_str());
    let mut output =
        compiler.compile_with(entry, snapshot, Some(generated), &|raw| remap_page_map(raw, entry, &translation));
    // Compiler diagnostics in the entry are in generated coordinates.
    let map = &translation.map;
    let trace = format!("({entry}:");
    for diagnostic in &mut output.diagnostics {
        if let Some(location) = diagnostic.location.as_mut()
            && &location.path == entry
        {
            location.range =
                map.generated_range_to_markdown(location.range).map_or(TextRange::empty(0), |m| m.markdown);
        }
        // Trace hints "... (entry:offset)" name generated offsets too.
        for hint in &mut diagnostic.hints {
            if let Some(at) = hint.rfind(&trace)
                && let Some(offset) = hint[at + trace.len()..].strip_suffix(')').and_then(|n| n.parse::<u32>().ok())
                && let Some(m) = map.generated_to_markdown(offset)
            {
                hint.replace_range(at.., &format!("{trace}{})", m.markdown));
            }
        }
    }
    let mut diagnostics: Vec<Diagnostic> = translation.diagnostics.iter().map(|d| d.to_diagnostic(entry)).collect();
    diagnostics.append(&mut output.diagnostics);
    output.diagnostics = diagnostics;
    output.compile_ms += translate_ms;
    output
}

/// Map the clusters and equations of a compiled translation back to the
/// Markdown source and build the page map.
pub fn remap_page_map(raw: RawExtraction, entry: &WorkspacePath, translation: &Translation) -> PageMap {
    let RawExtraction { files, mut equations, mut pages } = raw;
    let Some(file) = files.iter().position(|f| f == entry).map(|i| i as u32) else {
        return PageMap::build(files, equations, pages);
    };
    let map = &translation.map;
    // Equations: the Markdown equation (with its `$` delimiters) whose
    // generated equation contains the compiled one.
    let markdown_equation = |generated: TextRange| {
        translation
            .equations
            .iter()
            .position(|e| e.generated.start <= generated.start && generated.end <= e.generated.end)
    };
    let mut index_of: HashMap<usize, u32> = HashMap::new();
    for (i, eq) in equations.iter_mut().enumerate() {
        if eq.file != file {
            continue;
        }
        if let Some(t) = markdown_equation(eq.range) {
            index_of.entry(t).or_insert(i as u32);
            eq.range = translation.equations[t].markdown;
            eq.block = translation.equations[t].block;
        } else {
            eq.range = TextRange::empty(0);
        }
    }
    for page in &mut pages {
        for cluster in &mut page.clusters {
            let Some(source) = cluster.source.filter(|s| s.file == file) else { continue };
            let generated = source.range;
            let Some(mapped) = map.generated_range_to_markdown(generated) else {
                cluster.source = None;
                cluster.kind = ClusterKind::NonEditable(NonEditableReason::Generated);
                cluster.stops.clear();
                cluster.equation = None;
                continue;
            };
            cluster.source = Some(ClusterSource { file, range: mapped.markdown });
            match (cluster.kind, mapped.mode) {
                (ClusterKind::Editable(_), Some(mode)) => {
                    cluster.kind = ClusterKind::Editable(mode);
                    let md = mapped.markdown;
                    for stop in &mut cluster.stops {
                        stop.offset = if stop.offset <= generated.start {
                            md.start
                        } else if stop.offset >= generated.end {
                            md.end
                        } else {
                            map.generated_to_markdown(stop.offset)
                                .map_or(md.start, |m| m.markdown)
                                .clamp(md.start, md.end)
                        };
                    }
                    if mode == EditMode::Math {
                        // `\text{..}` is Typst markup inside the equation:
                        // attach it to its equation for line grouping.
                        if cluster.equation.is_none()
                            && let Some(t) = markdown_equation(generated)
                        {
                            let index = *index_of.entry(t).or_insert_with(|| {
                                equations.push(EquationInfo {
                                    file,
                                    range: translation.equations[t].markdown,
                                    block: translation.equations[t].block,
                                });
                                (equations.len() - 1) as u32
                            });
                            cluster.equation = Some(index);
                        }
                    } else {
                        cluster.equation = None;
                    }
                }
                (ClusterKind::Editable(_), None) => {
                    let reason = match mapped.kind {
                        SegmentKind::Raw => NonEditableReason::Raw,
                        _ => NonEditableReason::Generated,
                    };
                    cluster.kind = ClusterKind::NonEditable(reason);
                    cluster.stops.clear();
                }
                (ClusterKind::NonEditable(_), _) => cluster.stops.clear(),
            }
        }
    }
    PageMap::build(files, equations, pages)
}
