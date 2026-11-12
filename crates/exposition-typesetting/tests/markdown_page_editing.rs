//! Markdown through the typesetting service: real Typst builds of
//! `tests/fixtures/markdown-basic` from the workspace (open buffer), page maps
//! in Markdown coordinates, hit testing and selection, page edits through the
//! document service with the Markdown input rules, shared undo, diagnostics
//! mapped to Markdown, and PDF export from the same document.
mod common;

use common::Fixture;
use exposition_core::build::{BuildStatus, BuildTarget, DocumentFormat, Engine, MappingPrecision, Severity};
use exposition_core::document::{DocumentEdit, EditOrigin, TransactionId, TransactionRequest, UndoBehavior};
use exposition_core::text::{TextRange, utf16_range_to_bytes};
use exposition_page::map::{Affinity, ClusterKind, EditMode};
use exposition_page::{PageEditOp, PageEditOutcome, PageEditRequest, PageInputRules, apply_page_edit};
use exposition_typesetting::markdown::PROFILE_ID;
use exposition_typesetting::{BuildRecord, FormatAdapter, MarkdownAdapter, MarkdownInput};

fn open(fixture: &str) -> Fixture {
    let mut fx = Fixture::open(fixture, "main.md");
    fx.target = BuildTarget { format: DocumentFormat::Markdown, engine: Engine::MarkdownPrint, entry: fx.path.clone() };
    fx
}

fn edit(fx: &Fixture, selection: TextRange, op: PageEditOp, group: u32) -> PageEditOutcome {
    let request = PageEditRequest {
        id: TransactionId(format!("t-{}", fx.revision())),
        document: fx.document.clone(),
        base_revision: fx.revision(),
        selection,
        op,
        group,
    };
    apply_page_edit(&fx.documents, &MarkdownInput, &request, &|d, r| fx.service.revision_text(d, r))
        .expect("transaction")
}

fn slice(text: &str, range: TextRange) -> &str {
    &text[utf16_range_to_bytes(text, range).unwrap()]
}

/// Every cluster of a build, with the Markdown it maps to.
fn clusters(record: &BuildRecord) -> Vec<exposition_page::Cluster> {
    let map = record.map.as_ref().expect("page map");
    map.pages.iter().flat_map(|p| &p.lines).flat_map(|l| l.clusters.iter().cloned()).collect()
}

#[test]
fn markdown_builds_with_the_profile_and_maps_to_markdown() {
    let fx = open("markdown-basic");
    assert!(MarkdownAdapter.handles(&fx.path));
    assert_eq!((MarkdownAdapter.format(), MarkdownAdapter.engine()), (DocumentFormat::Markdown, Engine::MarkdownPrint));
    let record = fx.build();
    assert!(matches!(record.result.status, BuildStatus::Succeeded), "{:#?}", record.result.diagnostics);
    assert!(record.result.diagnostics.is_empty(), "{:#?}", record.result.diagnostics);
    let config = record.result.configuration.as_ref().unwrap();
    assert_eq!((config.engine, config.profile.as_deref()), (Engine::MarkdownPrint, Some(PROFILE_ID)));
    assert_eq!(record.result.mapping, MappingPrecision::Glyph);
    assert!(record.id().starts_with("markdown-"));
    // The generated Typst is never written to the workspace.
    let files: Vec<_> = std::fs::read_dir(&fx.root).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(files.len(), 2, "{files:?}");

    let text = fx.text();
    let map = record.map.as_ref().unwrap();
    assert_eq!(map.files, vec![fx.path.clone()]);
    let mut editable = 0;
    let mut math = 0;
    for c in clusters(&record) {
        let Some(source) = c.source else { continue };
        assert!(source.range.end as usize <= text.len(), "Markdown range {:?} for {:?}", source.range, c.text);
        if let ClusterKind::Editable(mode) = c.kind {
            editable += 1;
            let md = slice(&text, source.range);
            assert!(!md.is_empty(), "{c:?}");
            if mode == EditMode::Math {
                math += 1;
                assert!(c.equation.is_some(), "math clusters belong to an equation: {c:?}");
            } else if md.chars().count() == 1 && c.text.chars().count() == 1 && md.chars().all(char::is_alphanumeric) {
                assert_eq!(md, c.text, "prose cluster text equals its Markdown");
            }
            // Stops are Markdown offsets inside the cluster's range.
            assert_eq!(c.stops.first().unwrap().offset, source.range.start);
            assert_eq!(c.stops.last().unwrap().offset, source.range.end);
        }
    }
    let (e, other) = map.cluster_counts();
    println!("markdown-basic page map: {e} editable clusters ({math} math), {other} non-editable");
    assert!(editable > 400 && math > 20);
    // Equations carry their Markdown ranges (with `$` delimiters).
    for eq in &map.equations {
        let md = slice(&text, eq.range);
        assert!(md.starts_with('$') && md.ends_with('$'), "{md:?}");
    }

    // Hit testing and caret geometry in Markdown offsets.
    let at = fx.offset_of("cites a footnote", 0);
    let caret = map.caret_geometry(&fx.path, at, Affinity::Downstream).expect("caret in prose");
    let hit = map.hit_test(caret.page, caret.x + 0.5, (caret.top + caret.bottom) / 2.0);
    assert_eq!((hit.caret.map(|c| c.offset), hit.mode), (Some(at), Some(EditMode::Markup)));
    let two = fx.offset_of("\\alpha^2", 7);
    let caret = map.caret_geometry(&fx.path, two, Affinity::Downstream).expect("caret in math");
    let hit = map.hit_test(caret.page, caret.x + 0.3, (caret.top + caret.bottom) / 2.0);
    assert_eq!(hit.mode, Some(EditMode::Math));
    // Selections return the rendered text of a Markdown range.
    let words = fx.offset_of("strong text", 0);
    assert_eq!(map.selection(&fx.path, words, words + 11).text, "strong text");
    let quotes = fx.offset_of("\"quotes\"", 0);
    assert_eq!(map.selection(&fx.path, quotes, quotes + 8).text, "“quotes”");
}

#[test]
fn page_edits_rebuild_and_undo_through_the_document_service() {
    let fx = open("markdown-basic");
    let original = fx.text();
    let first = fx.build();
    // Type characters that need Markdown escapes.
    let at = fx.offset_of("cites a footnote", 0);
    let mut caret = at;
    for ch in "a*b $5 ".chars() {
        match edit(&fx, TextRange::empty(caret), PageEditOp::Insert { text: ch.to_string() }, 1) {
            PageEditOutcome::Applied { caret: c, mode, .. } => {
                assert_eq!(mode, EditMode::Markup);
                caret = c;
            }
            other => panic!("typing {ch:?}: {other:?}"),
        }
    }
    let typed = fx.text();
    let start = utf16_range_to_bytes(&original, TextRange::empty(at)).unwrap().start;
    assert_eq!(typed, format!("{}a*b \\$5 {}", &original[..start], &original[start..]));
    let record = fx.build();
    let map = record.map.as_ref().unwrap();
    // The page renders exactly what was typed where it was typed.
    assert_eq!(map.selection(&fx.path, at, caret).text, "a*b $5 ");

    // Change the exponent of `\alpha^2` to 3 (select the glyph, type).
    let two = fx.offset_of("\\alpha^2", 7);
    match edit(&fx, TextRange::new(two, two + 1), PageEditOp::Insert { text: "3".into() }, 2) {
        PageEditOutcome::Applied { mode, caret, .. } => assert_eq!((mode, caret), (EditMode::Math, two + 1)),
        other => panic!("{other:?}"),
    }
    assert!(fx.text().contains("$\\alpha^3 + \\beta_1"));
    let record = fx.build();
    assert_eq!(record.map.as_ref().unwrap().selection(&fx.path, two, two + 1).text, "3");

    // Shared undo: the source side undoes the math edit, then the typing.
    fx.documents.undo(&fx.document, EditOrigin::Source).unwrap();
    assert_eq!(fx.text(), typed);
    fx.documents.undo(&fx.document, EditOrigin::Source).unwrap();
    assert_eq!(fx.text(), original);
    let restored = fx.build();
    // Identical rendering (page hashes also cover source spans, which an
    // incremental reparse renumbers).
    let svg = |r: &BuildRecord| fx.service.page_svg(&r.result.pages[0].render_ref).unwrap();
    assert_eq!(svg(&restored), svg(&first), "identical page after undo");
}

#[test]
fn unsupported_math_and_compiler_errors_point_into_the_markdown() {
    let fx = open("markdown-basic");
    let at = fx.offset_of("Final paragraph", 0);
    let insert = |text: &str, at: u32| {
        fx.documents
            .apply(TransactionRequest {
                id: TransactionId(format!("src-{}", fx.revision())),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: fx.document.clone(),
                    base_revision: fx.revision(),
                    patches: vec![exposition_core::document::TextPatch::insert(at, text)],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: exposition_core::document::ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
    };
    insert("Bad $\\color{red}{x} + y$ math. ", at);
    let record = fx.build();
    assert!(matches!(record.result.status, BuildStatus::Succeeded));
    let warning = record.result.diagnostics.iter().find(|d| d.message.contains("\\color")).expect("reported");
    assert_eq!(warning.severity, Severity::Warning);
    let location = warning.location.as_ref().unwrap();
    assert_eq!(location.path, fx.path);
    let text = fx.text();
    assert_eq!(slice(&text, location.range), "\\color{red}{x}");
    // The marker is on the page, not editable, and selectable by its source.
    let map = record.map.as_ref().unwrap();
    assert_eq!(map.selection(&fx.path, location.range.start, location.range.end).text, "\\color{red}{x}");
    let marker: Vec<_> =
        clusters(&record).into_iter().filter(|c| c.source.is_some_and(|s| s.range == location.range)).collect();
    assert!(!marker.is_empty() && marker.iter().all(|c| !c.is_editable()));
    assert_eq!(MarkdownInput.mode_at(&text, location.range.start + 2), Some(EditMode::Math));

    // A missing image is a compiler error located at the image in Markdown.
    let good = record.id().to_owned();
    insert("![missing](nothere.png)\n\n", at);
    let failed = fx.build();
    assert!(matches!(failed.result.status, BuildStatus::Failed));
    let error = failed.result.diagnostics.iter().find(|d| d.severity == Severity::Error).expect("error");
    let range = error.location.as_ref().unwrap().range;
    assert_eq!(slice(&fx.text(), range), "![missing](nothere.png)", "{error:?}");
    assert_eq!(fx.service.last_good(&fx.path).unwrap().id(), good, "last good pages stay available");
}

#[test]
fn pdf_export_comes_from_the_markdown_build() {
    let fx = open("markdown-basic");
    let record = fx.build();
    let out = fx.root.join("export");
    std::fs::create_dir_all(&out).unwrap();
    let artifact = fx.service.export_pdf(record.id(), Some(out.join("main.pdf"))).unwrap();
    let pdf = std::fs::read(&artifact.path).unwrap();
    let doc = exposition_typesetting::typst_adapter::export_pdf(record.document.as_ref().unwrap()).unwrap();
    assert_eq!(pdf, doc, "exported bytes come from the build's own document");
    assert!(pdf.starts_with(b"%PDF"));
    assert_eq!(record.result.pages.len(), record.document.as_ref().unwrap().pages().len());
}

/// R-m23 F13: code, footnote reference numbers and task boxes are content
/// the page cannot edit; clicking them must report why (the page editor's
/// "not editable" affordance) instead of silently placing the caret.
#[test]
fn non_editable_markdown_constructs_report_their_reason() {
    use exposition_page::map::NonEditableReason;
    let fx = open("markdown-basic");
    let record = fx.build();
    let map = record.map.as_ref().unwrap();
    let text = fx.text();
    let hit_center = |c: &exposition_page::Cluster, page: u32| {
        map.hit_test(page, (c.x0 + c.x1) / 2.0, (c.top + c.bottom) / 2.0).non_editable
    };
    let find = |pred: &dyn Fn(&exposition_page::Cluster) -> bool| {
        map.pages.iter().enumerate().find_map(|(p, page)| {
            page.lines.iter().flat_map(|l| &l.clusters).find(|c| pred(c)).map(|c| (p as u32, c.clone()))
        })
    };
    // Code block and inline code: raw/verbatim, navigating to their source.
    let (page, code) = find(&|c| c.text == "println!").expect("code block glyphs");
    assert_eq!(hit_center(&code, page), Some(NonEditableReason::Raw));
    assert!(slice(&text, code.source.unwrap().range).starts_with("```rust"));
    let (page, inline) = find(&|c| c.text == "code" && !c.is_editable()).expect("inline code glyphs");
    assert_eq!(hit_center(&inline, page), Some(NonEditableReason::Raw));
    assert_eq!(slice(&text, inline.source.unwrap().range), "`code`");
    // Task box: a glyph mapped to `[x]`, generated.
    let (page, task) = find(&|c| c.text == "☑").expect("task box glyph");
    assert_eq!(slice(&text, task.source.unwrap().range), "[x]");
    assert_eq!(hit_center(&task, page), Some(NonEditableReason::Generated));
    // Footnote reference number: on the text line it belongs to, so a click
    // just right of the referencing word lands on it (the reviewer's probe).
    let at = fx.offset_of("[^note]", 0);
    let caret = map.caret_geometry(&fx.path, at, Affinity::Downstream).unwrap();
    let hit = map.hit_test(caret.page, caret.x + 1.0, (caret.top + caret.bottom) / 2.0);
    assert_eq!(hit.non_editable, Some(NonEditableReason::Generated));
    // Every piece of an inline equation (the radical of `\sqrt`) groups with
    // its equation, so it does not split the text line.
    let (_, radical) = find(&|c| c.text == "√").expect("radical");
    assert!(radical.equation.is_some(), "{radical:?}");
}
