//! LaTeX through the shared page-editor core (exposition-page): the TeX
//! service as a PageMapProvider, hit testing and caret geometry on the shared
//! map, and page edits planned by TexInput and applied as page transactions.

mod common;

use std::sync::Arc;
use std::time::Duration;

use exposition_core::build::{BuildStatus, BuildTarget, DocumentFormat, Engine};
use exposition_core::document::{DocumentId, TransactionId};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, utf16_len};
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_page::map::Affinity;
use exposition_page::{
    EditMode, NonEditableReason, PageEditOp, PageEditOutcome, PageEditRequest, PageMap, PageMapProvider,
    apply_page_edit,
};
use exposition_tex::{TexBuildMode, TexBuildSummary, TexEvent, TexInput, TexService, TexServiceConfig};
use exposition_workspace::{DocumentService, WorkspaceService};

struct Env {
    runtime: tokio::runtime::Runtime,
    _data: tempfile::TempDir,
    _project: tempfile::TempDir,
    documents: Arc<DocumentService>,
    service: TexService,
    events: tokio::sync::broadcast::Receiver<TexEvent>,
    engine: Engine,
    main: DocumentId,
}

const MAIN: &str = "main.tex";

impl Env {
    fn new(engine: Engine) -> Option<Self> {
        Self::with_main(engine, None)
    }

    /// The latex-basic project, with `main` as main.tex when given.
    fn with_main(engine: Engine, main: Option<&str>) -> Option<Self> {
        common::toolchain(engine)?;
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let data = tempfile::tempdir().unwrap();
        let project = common::project_from_fixture("latex-basic");
        if let Some(main) = main {
            std::fs::write(project.path().join(MAIN), main).unwrap();
        }
        let mut config = ExecutionConfig::new(data.path());
        config.too_broad_to_trust = Some(Vec::new());
        let exec = ExecutionService::with_config(config).unwrap();
        let documents = Arc::new(DocumentService::new());
        let workspace = WorkspaceService::new(documents.clone(), data.path().join("workspace"));
        workspace.open_workspace(project.path()).unwrap();
        exec.register_workspace_root(project.path()).unwrap();
        exec.grant_trust(&project.path().display().to_string()).unwrap();
        let mut tex_config = TexServiceConfig::new(data.path());
        tex_config.idle_full_delay = None;
        let service = TexService::new(exec, workspace.clone(), runtime.handle().clone(), tex_config).unwrap();
        let events = service.subscribe();
        let main = workspace.open_document(&WorkspacePath::parse(MAIN).unwrap()).unwrap().state.info.id;
        Some(Self { runtime, _data: data, _project: project, documents, service, events, engine, main })
    }

    fn build(&mut self, mode: TexBuildMode) -> TexBuildSummary {
        let target = BuildTarget {
            format: DocumentFormat::Latex,
            engine: self.engine,
            entry: WorkspacePath::parse(MAIN).unwrap(),
        };
        self.service.request_build(Some(target), mode).unwrap();
        loop {
            let event = self
                .runtime
                .block_on(async { tokio::time::timeout(Duration::from_secs(120), self.events.recv()).await })
                .unwrap()
                .unwrap();
            if let TexEvent::Finished { summary, .. } = event {
                assert_eq!(summary.result.status, BuildStatus::Succeeded, "{:?}", summary.result.diagnostics);
                return *summary;
            }
        }
    }

    fn map(&self, summary: &TexBuildSummary) -> Arc<PageMap> {
        PageMapProvider::page_map(&self.service, &summary.result.id.0).expect("page map")
    }

    fn text(&self) -> Arc<str> {
        self.documents.snapshot(&self.main).unwrap().text
    }

    fn edit(&self, base_revision: u64, selection: TextRange, op: PageEditOp) -> PageEditOutcome {
        let request = PageEditRequest {
            id: TransactionId(format!("e{base_revision}-{}", selection.start)),
            document: self.main.clone(),
            base_revision,
            selection,
            op,
            group: 1,
        };
        apply_page_edit(&self.documents, &TexInput, &request, &|d, r| self.service.revision_text(d, r)).unwrap()
    }
}

fn offset_of(text: &str, needle: &str) -> u32 {
    utf16_len(&text[..text.find(needle).unwrap_or_else(|| panic!("{needle}"))]) as u32
}

#[test]
fn latex_pages_are_edited_through_the_shared_core() {
    for engine in [Engine::PdfLatex, Engine::LuaLatex] {
        let Some(mut env) = Env::new(engine) else { continue };
        let first = env.build(TexBuildMode::Full);
        let map = env.map(&first);
        let path = WorkspacePath::parse(MAIN).unwrap();
        let (editable, other) = map.cluster_counts();
        eprintln!("{engine:?}: {} pages, {editable} editable / {other} non-editable clusters", map.pages.len());
        assert!(editable > 500 && other > 10);

        // Page images through the provider (PNG).
        let image = PageMapProvider::page_image(&env.service, &first.result.pages[0].render_ref).unwrap();
        assert_eq!(image.media_type, "image/png");
        assert!(image.bytes.starts_with(b"\x89PNG"));

        // Hit test inside "baseline" → caret between "base" and "line".
        let text = env.text();
        let at = offset_of(&text, "line for official");
        let geometry = map.caret_geometry(&path, at, Affinity::Downstream).expect("caret geometry");
        let hit = map.hit_test(geometry.page, geometry.x + 0.3, (geometry.top + geometry.bottom) / 2.0);
        let caret = hit.caret.expect("caret");
        assert_eq!((caret.offset, hit.mode), (at, Some(EditMode::Markup)));

        // Type on the page (based on the build's revision) → source and new page.
        let base = first.result.sources.iter().find(|s| s.path == path).unwrap().revision.unwrap();
        let outcome = env.edit(base, TextRange::empty(at), PageEditOp::Insert { text: "QZ".into() });
        assert!(matches!(outcome, PageEditOutcome::Applied { caret, .. } if caret == at + 2), "{outcome:?}");
        assert_eq!(&env.text()[..], format!("{}QZ{}", &text[..at as usize], &text[at as usize..]));
        let second = env.build(TexBuildMode::Draft);
        let map2 = env.map(&second);
        let selection = map2.selection(&path, at, at + 2);
        assert_eq!(selection.text, "QZ", "{engine:?}");

        // A section number is generated text: not editable.
        let number = map2
            .pages
            .iter()
            .flat_map(|p| &p.lines)
            .flat_map(|l| &l.clusters)
            .find(|c| c.text == "1" && c.source.is_none())
            .expect("section number cluster");
        assert_eq!(number.kind, exposition_page::ClusterKind::NonEditable(NonEditableReason::Generated));

        // Display math: select the exponent of c^2 and type 3.
        let text = env.text();
        let two = offset_of(&text, "2 + \\sum");
        assert_eq!(map2.caret_geometry(&path, two, Affinity::Downstream).map(|g| g.mode), Some(EditMode::Math));
        // A click on the raised exponent itself lands in the exponent, not on
        // its base (whose layout box would otherwise reach over it).
        let at_two = map2.caret_geometry(&path, two, Affinity::Downstream).unwrap();
        let end_two = map2.caret_geometry(&path, two + 1, Affinity::Upstream).unwrap();
        let x = at_two.x + (end_two.x - at_two.x) * 0.25;
        let hit = map2.hit_test(at_two.page, x, (at_two.top + at_two.bottom) / 2.0);
        assert_eq!(hit.caret.map(|c| c.offset), Some(two), "{engine:?}: exponent hit");
        let revision = second.result.sources.iter().find(|s| s.path == path).unwrap().revision.unwrap();
        let outcome = env.edit(revision, TextRange::new(two, two + 1), PageEditOp::Insert { text: "3".into() });
        assert!(matches!(outcome, PageEditOutcome::Applied { .. }), "{outcome:?}");
        assert!(env.text().contains("m c^3 + \\sum"));
        let third = env.build(TexBuildMode::Draft);
        assert_eq!(env.map(&third).selection(&path, two, two + 1).text, "3");

        // Backspace in a ligature: "affine" → "afine".
        let text = env.text();
        let i = offset_of(&text, "ine, office");
        let revision = third.result.sources.iter().find(|s| s.path == path).unwrap().revision.unwrap();
        let outcome = env.edit(revision, TextRange::empty(i), PageEditOp::DeleteBackward);
        assert!(matches!(outcome, PageEditOutcome::Applied { .. }), "{outcome:?}");
        assert!(env.text().contains("tests: afine, office"));
        let fourth = env.build(TexBuildMode::Draft);
        let fi = env.map(&fourth).selection(&path, i - 2, i);
        assert_eq!(fi.text, "fi", "{engine:?}: the ligature re-formed");

        // Markup cannot be removed from the page.
        let text = env.text();
        let s = offset_of(&text, "\\emph{");
        let revision = fourth.result.sources.iter().find(|s| s.path == path).unwrap().revision.unwrap();
        let outcome = env.edit(revision, TextRange::new(s - 3, s + 10), PageEditOp::DeleteBackward);
        assert!(matches!(outcome, PageEditOutcome::Refused { .. }), "{outcome:?}");
    }
}

/// R-m23 F8, F10, F12: what is typed on the page shows and copies as typed.
/// `<` `>` `|` `"` `$` typed on a pdfLaTeX (OT1) page are inserted in forms
/// that typeset as typed and are mapped; accented letters copy as one
/// character; with LuaLaTeX a base letter with a combining accent is mapped
/// and characters the font lacks are reported (and still copy).
#[test]
fn typed_characters_show_and_copy_as_typed() {
    for engine in [Engine::PdfLatex, Engine::LuaLatex] {
        let lua = engine == Engine::LuaLatex;
        let extra =
            if lua { "Comb e\u{301} x \u{65E5}\u{672C} \u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{1D538} end.\n" } else { "" };
        let main = format!(
            "\\documentclass{{article}}\n\\begin{{document}}\nTyped: X end.\n\nCaf\u{e9} na\\\"ive r\\'ole done.\n\n{extra}\\end{{document}}\n"
        );
        let Some(mut env) = Env::with_main(engine, Some(&main)) else { continue };
        let path = WorkspacePath::parse(MAIN).unwrap();
        let first = env.build(TexBuildMode::Draft);
        let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();

        // Accented letters (UTF-8 and accent macros) copy as one character each.
        let text = env.text();
        let (s, e) = (offset_of(&text, "Caf"), offset_of(&text, " done"));
        assert_eq!(squash(&env.map(&first).selection(&path, s, e).text), "Cafénaïveróle", "{engine:?}");

        // Type the specials on the page, through the shared page-edit core.
        let at = offset_of(&text, "X end");
        let base = first.result.sources.iter().find(|s| s.path == path).unwrap().revision.unwrap();
        let typed = "a<b>c|d \"q\" e$f g_h%#&{} x~y 2^3 a\\b ";
        let outcome = env.edit(base, TextRange::empty(at), PageEditOp::Insert { text: typed.into() });
        let PageEditOutcome::Applied { caret, .. } = outcome else { panic!("{outcome:?}") };
        let second = env.build(TexBuildMode::Draft);
        let shown = env.map(&second).selection(&path, at, caret).text;
        // As typed, with LaTeX's curly quotes for `"`.
        let expected = squash(typed).replace("\"q\"", "“q”");
        assert_eq!(squash(&shown), expected, "{engine:?}: {:?}", &env.text()[at as usize..caret as usize]);

        if lua {
            let text = env.text();
            let combining = offset_of(&text, "e\u{301}");
            let map = env.map(&second);
            assert_eq!(map.selection(&path, combining, combining + 2).text, "é");
            let cjk = offset_of(&text, "\u{65E5}");
            assert_eq!(map.selection(&path, cjk, cjk + 2).text, "\u{65E5}\u{672C}");
            let warning = second
                .result
                .diagnostics
                .iter()
                .find(|d| d.message.starts_with("Missing characters"))
                .unwrap_or_else(|| panic!("no missing-character warning: {:?}", second.result.diagnostics));
            assert_eq!(warning.severity, exposition_core::build::Severity::Warning);
            assert!(
                warning.message.contains('\u{65E5}') && warning.message.contains('\u{1D538}'),
                "{}",
                warning.message
            );
            let location = warning.location.as_ref().expect("located");
            assert_eq!((location.path.as_str(), location.range.start), (MAIN, cjk));
        }
    }
}
