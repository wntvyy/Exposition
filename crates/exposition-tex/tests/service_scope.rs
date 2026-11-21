//! What the TeX service does in proportion to a change, with the real engine:
//! page images follow page content (an unchanged page is not rendered again),
//! a change of a document the latest build did not read starts no build, and
//! the builds and workers of a folder are released when another one is open.
//! Skips with a message when no TeX engine is installed.

mod common;

use std::sync::Arc;
use std::time::Duration;

use exposition_core::build::{BuildStatus, BuildTarget, DocumentFormat, Engine};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::utf16_len;
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_page::PageMapProvider;
use exposition_tex::{TexBuildMode, TexBuildSummary, TexEvent, TexService, TexServiceConfig};
use exposition_workspace::{DocumentService, WorkspaceService};
use tokio::sync::broadcast;

const PAGES: usize = 3;

/// Three pages of prose and mathematics; the first page reads `extra.tex`.
fn main_tex() -> String {
    let mut text = String::from("\\documentclass[11pt]{article}\n\\begin{document}\n\\input{extra}\n");
    for i in 0..PAGES {
        text.push_str(&format!(
            "\\section{{Section {i}}}\nLet the map be continuous on a compact set, with $a^2 + b^2 = c^2$ inline. anchor{i} plain prose follows here.\n\\begin{{equation}}\n  \\sum_{{k=0}}^{{n}} k = \\frac{{n (n + 1)}}{{2}}\n\\end{{equation}}\nHence the claim.\n\\newpage\n"
        ));
    }
    text.push_str("\\end{document}\n");
    text
}

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

fn path(text: &str) -> WorkspacePath {
    WorkspacePath::parse(text).unwrap()
}

fn target() -> BuildTarget {
    BuildTarget { format: DocumentFormat::Latex, engine: Engine::PdfLatex, entry: path("main.tex") }
}

impl Env {
    /// A trusted temporary project: `main.tex`, the `extra.tex` it reads, and
    /// files it does not read.
    fn new() -> Option<Self> {
        common::toolchain(Engine::PdfLatex)?;
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("main.tex", main_tex()),
            ("extra.tex", "Extra text read by the entry.\n".to_owned()),
            ("notes.txt", "notes\n".to_owned()),
            ("later.txt", "later\n".to_owned()),
            ("refs.bib", "@misc{key, title = {Title}}\n".to_owned()),
        ] {
            std::fs::write(project.path().join(name), text).unwrap();
        }
        let mut config = ExecutionConfig::new(data.path());
        config.too_broad_to_trust = Some(Vec::new());
        let exec = ExecutionService::with_config(config).unwrap();
        let documents = Arc::new(DocumentService::new());
        let workspace = WorkspaceService::new(documents.clone(), data.path().join("workspace"));
        workspace.open_workspace(project.path()).unwrap();
        exec.register_workspace_root(project.path()).unwrap();
        assert!(exec.grant_trust(&project.path().display().to_string()).unwrap().trusted);
        let mut tex_config = TexServiceConfig::new(data.path());
        tex_config.idle_full_delay = None;
        let service = TexService::new(exec.clone(), workspace.clone(), runtime.handle().clone(), tex_config).unwrap();
        let events = service.subscribe();
        Some(Self { runtime, _data: data, project, exec, documents, workspace, service, events })
    }

    fn open(&self, name: &str) -> DocumentId {
        self.workspace.open_document(&path(name)).unwrap().state.info.id
    }

    fn revision(&self, doc: &DocumentId) -> u64 {
        self.documents.info(doc).unwrap().revision
    }

    /// The next event within `wait`, if any.
    fn event_within(&mut self, wait: Duration) -> Option<TexEvent> {
        self.runtime
            .block_on(async { tokio::time::timeout(wait, self.events.recv()).await })
            .ok()
            .map(|event| event.expect("event channel"))
    }

    /// The next build that was not canceled.
    fn next_result(&mut self) -> TexBuildSummary {
        loop {
            match self.event_within(Duration::from_secs(180)).expect("timed out waiting for a TeX build") {
                TexEvent::Finished { summary, .. } if summary.result.status != BuildStatus::Canceled => {
                    return *summary;
                }
                _ => {}
            }
        }
    }

    fn build(&mut self, mode: TexBuildMode) -> TexBuildSummary {
        self.service.request_build(Some(target()), mode).unwrap();
        let summary = self.next_result();
        assert_eq!(summary.result.status, BuildStatus::Succeeded, "{:?}", summary.result.diagnostics);
        summary
    }

    /// Insert `text` before `before`; returns the new revision.
    fn insert(&self, doc: &DocumentId, before: &str, text: &str) -> u64 {
        let current = self.documents.snapshot(doc).unwrap();
        let at = utf16_len(&current.text[..current.text.find(before).unwrap()]) as u32;
        let outcome = self
            .documents
            .apply(TransactionRequest {
                id: TransactionId(format!("t{}-{}", doc.0, current.revision)),
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

    /// Forward a change of `doc` as the application does; whether a build started.
    fn changed(&mut self, doc: &DocumentId, name: &str) -> bool {
        let revision = self.revision(doc);
        self.service.rebuild_for_change(doc, Some(&path(name)), revision);
        let mut started = false;
        // Until the target is quiet again.
        while let Some(event) = self.event_within(Duration::from_millis(if started { 30_000 } else { 400 })) {
            match event {
                TexEvent::Queued { .. } | TexEvent::Started { .. } => started = true,
                TexEvent::Finished { summary, .. } if summary.result.status != BuildStatus::Canceled => break,
                TexEvent::Finished { .. } => {}
            }
        }
        started
    }

    fn images(&self, summary: &TexBuildSummary) -> Vec<Arc<[u8]>> {
        summary
            .result
            .pages
            .iter()
            .map(|page| self.service.page_image(&page.render_ref).expect("page image").bytes)
            .collect()
    }
}

#[test]
fn unchanged_pages_keep_their_reference_and_are_not_rendered_again() {
    let Some(mut env) = Env::new() else { return };
    let main = env.open("main.tex");
    let first = env.build(TexBuildMode::Full);
    let refs = |summary: &TexBuildSummary| -> Vec<String> {
        summary.result.pages.iter().map(|page| page.render_ref.clone()).collect()
    };
    let first_refs = refs(&first);
    assert_eq!(first_refs.len(), PAGES);
    // Content references (not build-unique ones), one per distinct page.
    assert!(first_refs.iter().all(|r| r.starts_with("tex-c-")), "{first_refs:?}");
    assert!(first_refs[0] != first_refs[1] && first_refs[1] != first_refs[2]);
    let first_images = env.images(&first);
    assert_eq!(env.service.pages_rendered(), PAGES as u64);

    // The same sources built again (the automatic full build after a draft):
    // the same pages, nothing rendered.
    let again = env.build(TexBuildMode::Full);
    assert_ne!(again.result.id, first.result.id);
    assert_eq!(refs(&again), first_refs);
    let again_images = env.images(&again);
    assert!(first_images.iter().zip(&again_images).all(|(a, b)| Arc::ptr_eq(a, b)));
    assert_eq!(env.service.pages_rendered(), PAGES as u64);

    // A word added on the second page: only that page is a new page.
    env.insert(&main, "anchor1 ", "claim ");
    let edited = env.build(TexBuildMode::Draft);
    let edited_refs = refs(&edited);
    assert_eq!(edited_refs[0], first_refs[0]);
    assert_ne!(edited_refs[1], first_refs[1]);
    assert_eq!(edited_refs[2], first_refs[2]);
    let edited_images = env.images(&edited);
    assert_eq!(env.service.pages_rendered(), PAGES as u64 + 1, "one page rendered for one changed page");
    assert!(Arc::ptr_eq(&edited_images[0], &first_images[0]));
    assert!(edited_images[1] != first_images[1], "the changed page shows other pixels");
    // The earlier build still serves its own second page, by build and by reference.
    assert_eq!(
        env.service.page_png(&first.result.id, 1, exposition_tex::service::PAGE_IMAGE_SCALE).unwrap(),
        first_images[1]
    );
    assert_eq!(env.service.page_image(&first_refs[1]).expect("retained page").bytes, first_images[1]);
    // A reference no retained build has is not served.
    assert!(env.service.page_image("tex-c-0000000000000000000000000000000000000000").is_none());
}

#[test]
fn a_change_of_a_document_the_latest_build_did_not_read_starts_no_build() {
    let Some(mut env) = Env::new() else { return };
    let main = env.open("main.tex");
    let extra = env.open("extra.tex");
    let notes = env.open("notes.txt");
    let bib = env.open("refs.bib");
    env.service.watch(&path("main.tex")).unwrap();
    let first = env.next_result();
    assert_eq!(first.result.status, BuildStatus::Succeeded, "{:?}", first.result.diagnostics);
    // A draft is one pass: what it did not read, the next pass may. Until a
    // full build has settled the read set, every change builds.
    env.insert(&notes, "notes", "x");
    assert!(env.changed(&notes, "notes.txt"), "a draft is no evidence that a file is not read");
    env.build(TexBuildMode::Full);
    let jobs = env.exec.list().len();

    // An open file the engine did not read in the full build: no build, no
    // process, however often.
    for _ in 0..5 {
        env.insert(&notes, "notes", "x");
        assert!(!env.changed(&notes, "notes.txt"), "an unrelated edit must not build");
    }
    assert_eq!(env.exec.list().len(), jobs, "no engine process for unrelated edits");

    // The entry and a file it reads: each change builds, and the result has it.
    let revision = env.insert(&main, "anchor0 ", "x");
    assert!(env.changed(&main, "main.tex"));
    let status = env.service.status(Some(&target())).unwrap();
    let compiled = |status: &exposition_tex::TexTargetStatus, name: &str| {
        status.last.as_ref().unwrap().result.sources.iter().find(|s| s.path.as_str() == name).and_then(|s| s.revision)
    };
    assert_eq!(compiled(&status, "main.tex"), Some(revision));
    // The revision the latest build compiled is not built a second time.
    assert!(!env.changed(&main, "main.tex"));
    let revision = env.insert(&extra, "Extra", "More ");
    assert!(env.changed(&extra, "extra.tex"));
    assert_eq!(compiled(&env.service.status(Some(&target())).unwrap(), "extra.tex"), Some(revision));

    // A bibliography database is read by tools that leave no record: always built.
    env.insert(&bib, "@misc", "% comment\n");
    assert!(env.changed(&bib, "refs.bib"));

    // A document opened after the latest build is not known to it: built once,
    // then known not to be read.
    let later = env.open("later.txt");
    env.insert(&later, "later", "x");
    assert!(env.changed(&later, "later.txt"));
    env.build(TexBuildMode::Full);
    env.insert(&later, "later", "x");
    assert!(!env.changed(&later, "later.txt"));

    // While a build is pending or running, nothing is skipped: the change may
    // be one the running build has not seen.
    env.insert(&main, "anchor0 ", "y");
    env.service.request_build(Some(target()), TexBuildMode::Draft).unwrap();
    env.insert(&notes, "notes", "x");
    assert!(env.changed(&notes, "notes.txt"));
    // A canceled request leaves the latest result behind the sources: the
    // next change builds whatever it is.
    env.insert(&main, "anchor0 ", "z");
    env.service.request_build(Some(target()), TexBuildMode::Draft).unwrap();
    env.service.cancel(Some(&target()));
    while env.event_within(Duration::from_secs(2)).is_some() {}
    env.insert(&notes, "notes", "x");
    assert!(env.changed(&notes, "notes.txt"));
    let status = env.service.status(Some(&target())).unwrap();
    assert_eq!(compiled(&status, "main.tex"), Some(env.revision(&main)), "the build caught up with the entry");

    // A document without a path is read by no build.
    let before = env.exec.list().len();
    env.service.rebuild_for_change(&notes, None, 99);
    assert!(env.event_within(Duration::from_millis(400)).is_none());
    assert_eq!(env.exec.list().len(), before);
}

#[test]
fn another_open_folder_releases_the_previous_folders_builds_and_workers() {
    let Some(mut env) = Env::new() else { return };
    let first = env.build(TexBuildMode::Draft);
    let reference = first.result.pages[0].render_ref.clone();
    assert!(env.service.page_image(&reference).is_some());
    let (targets, builds) = env.service.retained();
    assert_eq!(targets, 1);
    assert!(builds >= 1);
    // Releasing with the same folder open keeps everything.
    env.service.release_closed_folders();
    assert_eq!(env.service.retained(), (targets, builds));
    assert!(env.service.build(&first.result.id).is_some());

    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("main.tex"), "\\documentclass{article}\\begin{document}B\\end{document}\n")
        .unwrap();
    env.workspace.open_workspace(other.path()).unwrap();
    env.service.release_closed_folders();
    assert_eq!(env.service.retained(), (0, 0), "nothing of the closed folder is kept");

    // The first folder again: it builds anew and serves its pages.
    env.workspace.open_workspace(env.project.path()).unwrap();
    assert!(env.service.build(&first.result.id).is_none());
    assert!(env.service.page_image(&reference).is_none(), "its earlier build is gone");
    let rebuilt = env.build(TexBuildMode::Draft);
    assert_eq!(rebuilt.result.pages[0].render_ref, reference, "the same page content");
    assert!(env.service.page_image(&reference).is_some());
    assert_eq!(env.service.retained().0, 1);
}

/// Under a continuous stream of superseding requests a build still starts
/// within the fixed maximum, however long the configured settle time is.
#[test]
fn a_continuous_stream_of_requests_still_starts_a_build_within_the_maximum() {
    use exposition_tex::service::MAX_SUPERSEDE_WAIT;
    let Some(mut env) = Env::new() else { return };
    let main = env.open("main.tex");
    env.build(TexBuildMode::Full);
    // Requests every 50 ms for four times the maximum. Each one supersedes
    // the running build or extends the wait of the pending one.
    let stream = MAX_SUPERSEDE_WAIT * 4;
    let begun = std::time::Instant::now();
    let mut starts = Vec::new();
    while begun.elapsed() < stream {
        env.insert(&main, "anchor0 ", "x");
        env.service.request_build(Some(target()), TexBuildMode::Draft).unwrap();
        while let Some(event) = env.event_within(Duration::from_millis(50)) {
            if matches!(event, TexEvent::Started { .. }) {
                starts.push(begun.elapsed());
            }
        }
    }
    // The first request starts at once; without a bound nothing else would
    // start before the stream ends (every request restarts the wait).
    assert!(starts.len() >= 3, "builds started during the stream: {starts:?}");
    for pair in starts.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(gap <= MAX_SUPERSEDE_WAIT + Duration::from_millis(900), "no build started for {gap:?}: {starts:?}");
    }
    // The stream's last state is built.
    let wanted = env.revision(&main);
    loop {
        let summary = env.next_result();
        let compiled = summary.result.sources.iter().find(|s| s.path.as_str() == "main.tex").and_then(|s| s.revision);
        if compiled == Some(wanted) {
            break;
        }
    }
}

/// A build with a biber control file reads data sources that no record
/// names: nothing is skipped for it.
#[test]
fn an_entry_with_a_biber_control_file_is_rebuilt_for_every_open_document() {
    let Some(mut env) = Env::new() else { return };
    let main = env.open("main.tex");
    let notes = env.open("notes.txt");
    env.service.watch(&path("main.tex")).unwrap();
    env.next_result();
    // Without the control file: skipped after a full build.
    env.build(TexBuildMode::Full);
    env.insert(&main, "anchor0 ", "x");
    assert!(env.changed(&main, "main.tex"), "the entry is watched");
    env.build(TexBuildMode::Full);
    env.insert(&notes, "notes", "x");
    assert!(!env.changed(&notes, "notes.txt"));
    // The document writes `main.bcf` (as biblatex does).
    env.insert(
        &main,
        "\\section{Section 0}",
        "\\newwrite\\control\\immediate\\openout\\control=\\jobname.bcf \\immediate\\write\\control{x}\\immediate\\closeout\\control\n",
    );
    env.service.request_build(Some(target()), TexBuildMode::Full).unwrap();
    env.next_result();
    env.insert(&notes, "notes", "x");
    assert!(env.changed(&notes, "notes.txt"), "biber may read any file its control file names");
}

/// Events of a target are for the folder it was built in.
#[test]
fn a_target_of_a_folder_that_is_no_longer_open_is_told_apart() {
    let Some(mut env) = Env::new() else { return };
    let reference = env.service.request_build(Some(target()), TexBuildMode::Draft).unwrap();
    env.next_result();
    assert!(env.service.is_open_folder(&reference));
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("main.tex"), "\\documentclass{article}\\begin{document}B\\end{document}\n")
        .unwrap();
    env.workspace.open_workspace(other.path()).unwrap();
    assert!(!env.service.is_open_folder(&reference), "the same entry path in another folder is another target");
    env.workspace.close_workspace();
    assert!(!env.service.is_open_folder(&reference));
    env.workspace.open_workspace(env.project.path()).unwrap();
    assert!(env.service.is_open_folder(&reference));
}
