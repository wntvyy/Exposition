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
        common::toolchain(engine)?;
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let data = tempfile::tempdir().unwrap();
        let project = common::project_from_fixture("latex-basic");
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
