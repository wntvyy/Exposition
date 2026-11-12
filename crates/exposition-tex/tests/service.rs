//! The TeX build service with the real execution service and engines.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::build::{BuildStatus, BuildTarget, DocumentFormat, Engine};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::utf16_len;
use exposition_execution::{ExecutionConfig, ExecutionService, RefusalCode};
use exposition_tex::{TexBuildMode, TexBuildSummary, TexEvent, TexService, TexServiceConfig, TexServiceError};
use exposition_workspace::{DocumentService, WorkspaceService};
use tokio::sync::broadcast;

struct Env {
    runtime: tokio::runtime::Runtime,
    _data: tempfile::TempDir,
    project: tempfile::TempDir,
    exec: ExecutionService,
    documents: Arc<DocumentService>,
    workspace: Arc<WorkspaceService>,
    service: TexService,
    events: broadcast::Receiver<TexEvent>,
}

fn target(engine: Engine) -> BuildTarget {
    BuildTarget { format: DocumentFormat::Latex, engine, entry: WorkspacePath::parse("main.tex").unwrap() }
}

impl Env {
    fn new(idle_full: Option<Duration>) -> Option<Self> {
        common::toolchain(Engine::PdfLatex)?;
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let data = tempfile::tempdir().unwrap();
        let project = common::project_from_fixture("latex-basic");
        let mut config = ExecutionConfig::new(data.path());
        config.too_broad_to_trust = Some(Vec::new());
        let exec = ExecutionService::with_config(config).unwrap();
        let documents = Arc::new(DocumentService::new());
        let workspace = WorkspaceService::new(documents.clone(), data.path().join("workspace"));
        // As `workspace_open` does: open the folder and register it for jobs.
        workspace.open_workspace(project.path()).unwrap();
        exec.register_workspace_root(project.path()).unwrap();
        let mut tex_config = TexServiceConfig::new(data.path());
        tex_config.idle_full_delay = idle_full;
        let service = TexService::new(exec.clone(), workspace.clone(), runtime.handle().clone(), tex_config).unwrap();
        let events = service.subscribe();
        Some(Self { runtime, _data: data, project, exec, documents, workspace, service, events })
    }

    fn trust(&self) {
        assert!(self.exec.grant_trust(&self.project.path().display().to_string()).unwrap().trusted);
    }

    fn request(&self, mode: TexBuildMode) {
        self.service.request_build(Some(target(Engine::PdfLatex)), mode).unwrap();
    }

    fn next_event(&mut self) -> TexEvent {
        let deadline = Duration::from_secs(120);
        self.runtime
            .block_on(async { tokio::time::timeout(deadline, self.events.recv()).await })
            .expect("timed out waiting for a TeX event")
            .expect("event channel")
    }

    fn next_finished(&mut self) -> TexBuildSummary {
        loop {
            if let TexEvent::Finished { summary, .. } = self.next_event() {
                return *summary;
            }
        }
    }

    fn open_main(&self) -> DocumentId {
        self.workspace.open_document(&WorkspacePath::parse("main.tex").unwrap()).unwrap().state.info.id
    }

    fn insert(&self, doc: &DocumentId, after: &str, text: &str) -> u64 {
        let current = self.documents.snapshot(doc).unwrap();
        let at = utf16_len(&current.text[..current.text.find(after).unwrap() + after.len()]) as u32;
        let outcome = self
            .documents
            .apply(TransactionRequest {
                id: TransactionId(format!("t{}", current.revision)),
                origin: EditOrigin::Page,
                edits: vec![DocumentEdit {
                    document: doc.clone(),
                    base_revision: current.revision,
                    patches: vec![TextPatch::insert(at, text)],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        outcome.applied[0].revision
    }
}

fn page_text(service: &TexService, summary: &TexBuildSummary) -> String {
    let map = service.page_map(&summary.result.id, 0).unwrap();
    map.glyphs.iter().filter_map(|g| g.text.clone()).collect()
}

#[test]
fn untrusted_workspaces_are_refused_before_anything_runs() {
    let Some(mut env) = Env::new(None) else { return };
    env.request(TexBuildMode::Draft);
    let summary = env.next_finished();
    let refusal = summary.refusal.as_ref().expect("refusal");
    assert_eq!(refusal.code, RefusalCode::UntrustedWorkspace);
    assert!(matches!(summary.result.status, BuildStatus::Error { .. }));
    assert!(summary.passes.is_empty() && !summary.has_output);
    assert!(env.exec.list().is_empty(), "no job may be launched: {:?}", env.exec.list());
    // Nothing was materialized for the untrusted workspace.
    let tex_dir = env._data.path().join("tex");
    let written: Vec<_> = walk(&tex_dir).into_iter().filter(|p| p.is_file()).collect();
    assert!(written.is_empty(), "{written:?}");
    // Unsupported targets are refused at the request.
    let mut typst = target(Engine::Typst);
    typst.format = DocumentFormat::Typst;
    assert_eq!(env.service.request_build(Some(typst), TexBuildMode::Draft), Err(TexServiceError::UnsupportedTarget));
    // Builds only ever target the open workspace.
    env.workspace.close_workspace();
    assert_eq!(
        env.service.request_build(Some(target(Engine::PdfLatex)), TexBuildMode::Draft),
        Err(TexServiceError::WorkspaceNotOpen)
    );
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            }
            out.push(path);
        }
    }
    out
}

#[test]
fn trusted_builds_run_through_the_execution_service_and_serve_pages() {
    let Some(mut env) = Env::new(None) else { return };
    env.trust();
    env.request(TexBuildMode::Draft);
    assert!(matches!(env.next_event(), TexEvent::Queued { mode: TexBuildMode::Draft, .. }));
    assert!(matches!(env.next_event(), TexEvent::Started { automatic: false, .. }));
    let summary = env.next_finished();
    assert_eq!(summary.result.status, BuildStatus::Succeeded, "{:?}", summary.result.diagnostics);
    assert!(summary.has_output && summary.refusal.is_none());
    // The engine ran as a supervised job.
    let jobs = env.exec.list();
    assert!(jobs.iter().any(|j| j.info.label.starts_with("pdflatex main.tex")), "{jobs:?}");
    // Pages: PNG bytes and the glyph mapping, by build id.
    let id = &summary.result.id;
    let png = env.service.page_png(id, 0, 1.5).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    assert!(Arc::ptr_eq(&png, &env.service.page_png(id, 0, 1.5).unwrap()), "rendered pages are cached");
    let map = env.service.page_map(id, 0).unwrap();
    assert!(map.stats.mapped > 600);
    assert!(matches!(env.service.page_png(id, 9, 1.0), Err(TexServiceError::NoSuchPage(9))));
    assert!(matches!(env.service.page_png(id, 0, 50.0), Err(TexServiceError::InvalidScale(_))));
    let status = env.service.status(Some(&target(Engine::PdfLatex))).unwrap();
    assert_eq!(status.last_good.as_ref().map(|s| &s.result.id), Some(id));
    assert!(!status.stale);
}

#[test]
fn rapid_edits_coalesce_and_supersede_running_builds() {
    let Some(mut env) = Env::new(None) else { return };
    env.trust();
    let main = env.open_main();
    let requests = 6;
    let mut last_revision = 0;
    for _ in 0..requests {
        last_revision = env.insert(&main, "The first", "Q");
        env.request(TexBuildMode::Draft);
        std::thread::sleep(Duration::from_millis(40));
    }
    let (mut started, mut superseded) = (0, 0);
    let summary = loop {
        match env.next_event() {
            TexEvent::Started { .. } => started += 1,
            TexEvent::Finished { summary, .. } => {
                let summary = *summary;
                superseded += usize::from(summary.superseded);
                let revision =
                    summary.result.sources.iter().find(|s| s.path.as_str() == "main.tex").and_then(|s| s.revision);
                if summary.result.status == BuildStatus::Succeeded && revision == Some(last_revision) {
                    break summary;
                }
            }
            TexEvent::Queued { .. } => {}
        }
    };
    eprintln!("{requests} requests → {started} builds started, {superseded} superseded");
    assert!(started < requests || superseded > 0, "requests were neither coalesced nor superseded");
    // Every coalesced edit is in the final pages ("fi" is one ligature glyph).
    assert!(page_text(&env.service, &summary).contains("ThefirstQQQQQQparagraph"));
}

#[test]
fn an_idle_full_build_follows_drafts() {
    let Some(mut env) = Env::new(Some(Duration::from_millis(300))) else { return };
    env.trust();
    env.request(TexBuildMode::Draft);
    let draft = env.next_finished();
    assert_eq!((draft.mode, draft.automatic), (TexBuildMode::Draft, false));
    assert!(page_text(&env.service, &draft).contains("[?]"), "a cold draft has no bibliography yet");
    let started = Instant::now();
    let full = env.next_finished();
    assert_eq!((full.mode, full.automatic), (TexBuildMode::Full, true));
    assert_eq!(full.result.status, BuildStatus::Succeeded);
    assert!(full.passes.iter().any(|p| p.tool == "bibtex"), "{:?}", full.passes);
    assert!(page_text(&env.service, &full).contains("[1]"));
    eprintln!("automatic full build finished {} ms after the draft", started.elapsed().as_millis());
    // Nothing further is scheduled once the full build is current.
    let quiet = env.runtime.block_on(async { tokio::time::timeout(Duration::from_secs(1), env.events.recv()).await });
    assert!(quiet.is_err(), "unexpected event {quiet:?}");
}

#[test]
fn last_good_output_is_kept_and_marked_stale() {
    let Some(mut env) = Env::new(None) else { return };
    env.trust();
    let main = env.open_main();
    env.request(TexBuildMode::Draft);
    let good = env.next_finished();
    assert!(good.has_output);
    // A fatal error: no pages at all.
    env.insert(&main, "\\maketitle\n", "\\input{does-not-exist}\n");
    env.request(TexBuildMode::Draft);
    let failed = env.next_finished();
    assert_eq!(failed.result.status, BuildStatus::Failed);
    assert!(!failed.has_output);
    assert!(failed.result.diagnostics.iter().any(|d| d.message.contains("does-not-exist")));
    let status = env.service.status(Some(&target(Engine::PdfLatex))).unwrap();
    assert_eq!(status.last.as_ref().unwrap().result.id, failed.result.id);
    assert_eq!(status.last_good.as_ref().unwrap().result.id, good.result.id);
    assert!(status.stale, "the last good pages predate the current text");
    // Its pages are still served.
    assert!(env.service.page_png(&good.result.id, 0, 1.0).unwrap().starts_with(b"\x89PNG"));
    assert!(matches!(env.service.page_png(&failed.result.id, 0, 1.0), Err(TexServiceError::NoOutput)));
    // An explicit cancel ends a running build (not marked superseded).
    env.request(TexBuildMode::Full);
    loop {
        if let TexEvent::Started { .. } = env.next_event() {
            break;
        }
    }
    assert!(env.service.cancel(Some(&target(Engine::PdfLatex))));
    let canceled = env.next_finished();
    assert_eq!(canceled.result.status, BuildStatus::Canceled);
    assert!(!canceled.superseded);
}

#[test]
fn the_default_target_comes_from_project_settings() {
    let Some(mut env) = Env::new(None) else { return };
    assert_eq!(env.service.default_target(), Err(TexServiceError::NoTarget));
    std::fs::create_dir_all(env.project.path().join(".exposition")).unwrap();
    std::fs::write(
        env.project.path().join(".exposition/project.json"),
        r#"{"version":1,"entry":"main.tex","targets":[
            {"name":"pdf","format":"latex","engine":"pdfLatex","entry":"main.tex"},
            {"name":"lua","format":"latex","engine":"luaLatex","entry":"main.tex"}],
           "defaultTarget":"lua"}"#,
    )
    .unwrap();
    let default = env.service.default_target().unwrap();
    assert_eq!((default.engine, default.entry.as_str()), (Engine::LuaLatex, "main.tex"));
    env.trust();
    let reference = env.service.request_build(None, TexBuildMode::Draft).unwrap();
    assert_eq!(reference.engine, Engine::LuaLatex);
    let summary = env.next_finished();
    assert_eq!(summary.result.status, BuildStatus::Succeeded, "{:?}", summary.result.diagnostics);
    assert!(summary.result.configuration.as_ref().unwrap().compiler.contains("LuaHBTeX"));
    // Buffers are compiled as they would be saved; disk inputs are recorded
    // through the workspace snapshot.
    let sources: Vec<&str> = summary.result.sources.iter().map(|s| s.path.as_str()).collect();
    assert!(sources.contains(&"sections/results.tex") && sources.contains(&"figures/box.png"), "{sources:?}");
    assert!(env.service.status(None).is_some_and(|s| s.target == reference));
}
