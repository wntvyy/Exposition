//! Review: attacks on change-scoped LaTeX rebuilds, burst settling, page
//! images by content, and folder release, with the real engine. A change that
//! starts no build while a build of the new state gives other pages is a
//! stale page shown as current. Skips when no TeX engine is installed.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use exposition_core::build::{BuildStatus, BuildTarget, DocumentFormat, Engine};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_page::PageMapProvider;
use exposition_tex::pdf::PdfDocument;
use exposition_tex::service::PAGE_IMAGE_SCALE;
use exposition_tex::{TexBuildMode, TexBuildSummary, TexEvent, TexService, TexServiceConfig};
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

fn path(text: &str) -> WorkspacePath {
    WorkspacePath::parse(text).unwrap()
}

fn target() -> BuildTarget {
    BuildTarget { format: DocumentFormat::Latex, engine: Engine::PdfLatex, entry: path("main.tex") }
}

fn document(body: &str) -> String {
    format!("\\documentclass{{article}}\n\\begin{{document}}\n{body}\n\\end{{document}}\n")
}

impl Env {
    /// A trusted temporary project with the given files (`/` in a name makes folders).
    fn new(files: &[(&str, &str)], supersede_delay: Option<Duration>) -> Option<Self> {
        Self::configured(files, |config| {
            config.idle_full_delay = None;
            if let Some(delay) = supersede_delay {
                config.supersede_delay = delay;
            }
        })
    }

    /// As [`Self::new`], with the defaults of the service changed by `configure`.
    fn configured(files: &[(&str, &str)], configure: impl FnOnce(&mut TexServiceConfig)) -> Option<Self> {
        common::toolchain(Engine::PdfLatex)?;
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let file = project.path().join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
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
        configure(&mut tex_config);
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
        self.next_result()
    }

    /// Insert `text` at the start of the document; returns the new revision.
    fn prepend(&self, doc: &DocumentId, text: &str) -> u64 {
        let current = self.documents.snapshot(doc).unwrap();
        let outcome = self
            .documents
            .apply(TransactionRequest {
                id: TransactionId(format!("t{}-{}", doc.0, current.revision)),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: doc.clone(),
                    base_revision: current.revision,
                    patches: vec![TextPatch::insert(0, text)],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        outcome.applied[0].revision
    }

    /// Insert `text` before `\end{document}` of the entry; returns the new revision.
    fn insert_body(&self, doc: &DocumentId, text: &str) -> u64 {
        let current = self.documents.snapshot(doc).unwrap();
        let before = &current.text[..current.text.find("\\end{document}").unwrap()];
        let at = exposition_core::text::utf16_len(before) as u32;
        let outcome = self
            .documents
            .apply(TransactionRequest {
                id: TransactionId(format!("b{}-{}", doc.0, current.revision)),
                origin: EditOrigin::Source,
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
        while let Some(event) = self.event_within(Duration::from_millis(if started { 60_000 } else { 600 })) {
            match event {
                TexEvent::Queued { .. } | TexEvent::Started { .. } => started = true,
                TexEvent::Finished { summary, .. } if summary.result.status != BuildStatus::Canceled => break,
                TexEvent::Finished { .. } => {}
            }
        }
        started
    }

    fn drain(&mut self, quiet: Duration) {
        while self.event_within(quiet).is_some() {}
    }
}

fn refs(summary: &TexBuildSummary) -> Vec<String> {
    summary.result.pages.iter().map(|page| page.render_ref.clone()).collect()
}

/// The soundness check of a skipped rebuild. The entry is watched and built
/// with `edited` open; then `edited` gets `text` prepended. Returns whether the
/// change started a build, and whether the pages shown afterwards are stale:
/// no build started, and a build of the new state has other pages.
fn skip_probe(files: &[(&str, &str)], edited: &str, text: &str) -> Option<(bool, bool)> {
    let mut env = Env::new(files, None)?;
    let _main = env.open("main.tex");
    let doc = env.open(edited);
    env.service.watch(&path("main.tex")).unwrap();
    let first = env.next_result();
    assert_eq!(
        first.result.status,
        BuildStatus::Succeeded,
        "the probe document must build: {:?}",
        first.result.diagnostics
    );
    env.drain(Duration::from_millis(300));
    env.prepend(&doc, text);
    let started = env.changed(&doc, edited);
    let shown = env.service.status(Some(&target())).unwrap().last.unwrap();
    let forced = env.build(TexBuildMode::Draft);
    let stale = !started && (refs(&forced) != refs(&shown) || forced.result.status != shown.result.status);
    eprintln!("{edited}: build started = {started}; stale pages shown = {stale}");
    Some((started, stale))
}

fn assert_sound(outcome: Option<(bool, bool)>) {
    let Some((_started, stale)) = outcome else { return };
    assert!(!stale, "no build started although the pages of the new state differ: stale pages shown");
}

/// The probe itself must be able to tell: a file that is plainly read rebuilds
/// and changes the page, a file that is not read does neither.
#[test]
fn a_plain_input_rebuilds_and_an_unread_file_does_not() {
    let files = [
        ("main.tex", document("\\input{extra}")),
        ("extra.tex", "Extra text.\n".to_owned()),
        ("notes.txt", "notes\n".to_owned()),
    ];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let Some(read) = skip_probe(&files, "extra.tex", "More ") else { return };
    assert_eq!(read, (true, false), "a read file rebuilds");
    let unread = skip_probe(&files, "notes.txt", "x").unwrap();
    assert_eq!(unread, (false, false), "an unread file starts no build and changes no page");
}

#[test]
fn an_input_through_a_dotted_path_and_a_subfolder_rebuilds() {
    let files = [
        ("main.tex", document("\\input{./sub/../dotted}\n\\input{sub/nested}")),
        ("dotted.tex", "Dotted text.\n".to_owned()),
        ("sub/nested.tex", "Nested text.\n".to_owned()),
    ];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let Some(dotted) = skip_probe(&files, "dotted.tex", "More ") else { return };
    assert_eq!(dotted, (true, false));
    assert_eq!(skip_probe(&files, "sub/nested.tex", "More ").unwrap(), (true, false));
}

#[test]
fn an_input_from_a_parent_folder_of_the_entry_rebuilds() {
    // The entry is in a subfolder and reads a file above it.
    let Some(mut env) = Env::new(
        &[("paper/main.tex", &*document("\\input{../shared/macros}")), ("shared/macros.tex", "Shared text.\n")],
        None,
    ) else {
        return;
    };
    let entry = path("paper/main.tex");
    let _main = env.open("paper/main.tex");
    let shared = env.open("shared/macros.tex");
    env.service.watch(&entry).unwrap();
    let first = env.next_result();
    if first.result.status != BuildStatus::Succeeded {
        eprintln!("SKIPPED: the engine refuses to read above the entry folder: {:?}", first.result.diagnostics);
        return;
    }
    env.drain(Duration::from_millis(300));
    env.prepend(&shared, "More ");
    assert!(env.changed(&shared, "shared/macros.tex"), "a file read from above the entry folder must rebuild");
}

#[cfg(windows)]
#[test]
fn an_input_named_in_another_letter_case_rebuilds() {
    let files = [("main.tex", document("\\input{UPPER}")), ("upper.tex", "Upper text.\n".to_owned())];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    assert_sound(skip_probe(&files, "upper.tex", "More "));
}

#[test]
fn an_input_with_a_non_ascii_name_and_a_space_rebuilds() {
    let files = [
        ("main.tex", document("\\input{\"kapitel \u{fc}ber\"}")),
        ("kapitel \u{fc}ber.tex", "Kapitel text.\n".to_owned()),
    ];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let Some(mut env) = Env::new(&files, None) else { return };
    let _main = env.open("main.tex");
    let doc = env.open("kapitel \u{fc}ber.tex");
    env.service.watch(&path("main.tex")).unwrap();
    let first = env.next_result();
    if first.result.status != BuildStatus::Succeeded {
        eprintln!("SKIPPED: this engine does not read the non-ASCII file name: {:?}", first.result.diagnostics);
        return;
    }
    env.drain(Duration::from_millis(300));
    env.prepend(&doc, "More ");
    let started = env.changed(&doc, "kapitel \u{fc}ber.tex");
    let shown = env.service.status(Some(&target())).unwrap().last.unwrap();
    let forced = env.build(TexBuildMode::Draft);
    assert!(started || refs(&forced) == refs(&shown), "a non-ASCII input changed, no build started, pages differ");
}

#[test]
fn a_file_read_with_openin_rebuilds() {
    let files = [
        (
            "main.tex",
            document(
                "\\newread\\reader\\openin\\reader=readin.txt \\read\\reader to\\firstline \\closein\\reader \\firstline",
            ),
        ),
        ("readin.txt", "line one\n".to_owned()),
    ];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    assert_sound(skip_probe(&files, "readin.txt", "More "));
}

#[test]
fn a_file_whose_size_is_typeset_rebuilds() {
    let files = [("main.tex", document("Size \\pdffilesize{sized.txt}.")), ("sized.txt", "12345\n".to_owned())];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    assert_sound(skip_probe(&files, "sized.txt", "67890"));
}

#[test]
fn a_file_whose_checksum_is_typeset_rebuilds() {
    let files =
        [("main.tex", document("Sum \\pdfmdfivesum file {summed.txt}.")), ("summed.txt", "content\n".to_owned())];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    assert_sound(skip_probe(&files, "summed.txt", "x"));
}

#[test]
fn a_file_dumped_into_the_page_rebuilds() {
    let files =
        [("main.tex", document("Dump \\pdffiledump length 4 {dumped.txt}.")), ("dumped.txt", "abcdefgh\n".to_owned())];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    assert_sound(skip_probe(&files, "dumped.txt", "x"));
}

/// A file that the label of the previous run decides to read: the first draft
/// does not read it (the label is not known yet), the next run does.
#[test]
#[ignore = "review: defect - a file read only once the .aux file knows a label is skipped after a single draft pass; the automatic full build bounds it in the application (next test)"]
fn a_file_read_depending_on_the_aux_file_rebuilds() {
    let body = "\\makeatletter\\@ifundefined{r@flag}{}{\\input{late.tex}}\\makeatother\nText.\\label{flag}";
    let files = [("main.tex", document(body)), ("late.tex", "Late text.\n".to_owned())];
    let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
    assert_sound(skip_probe(&files, "late.tex", "More "));
}

/// The same document with the defaults of the service (as the application
/// runs it): the edit made before the automatic full build is skipped, the
/// full build then reads the file, and later edits rebuild.
#[test]
fn with_the_automatic_full_build_an_aux_dependent_file_is_caught_up() {
    let body = "\\makeatletter\\@ifundefined{r@flag}{}{\\input{late.tex}}\\makeatother\nText.\\label{flag}";
    let Some(mut env) = Env::configured(&[("main.tex", &*document(body)), ("late.tex", "Late text.\n")], |_| {}) else {
        return;
    };
    let _main = env.open("main.tex");
    let late = env.open("late.tex");
    env.service.watch(&path("main.tex")).unwrap();
    let draft = env.next_result();
    assert_eq!(draft.result.status, BuildStatus::Succeeded, "{:?}", draft.result.diagnostics);
    assert!(matches!(draft.mode, TexBuildMode::Draft));
    // Within the 1.5 s before the automatic full build.
    let revision = env.prepend(&late, "More ");
    env.service.rebuild_for_change(&late, Some(&path("late.tex")), revision);
    let started = Instant::now();
    let full = loop {
        let summary = env.next_result();
        if matches!(summary.mode, TexBuildMode::Full) {
            break summary;
        }
    };
    eprintln!("pages without the edit of late.tex were shown as current for {:?}", started.elapsed());
    assert_eq!(full.result.status, BuildStatus::Succeeded, "{:?}", full.result.diagnostics);
    let compiled = full.result.sources.iter().find(|s| s.path.as_str() == "late.tex").and_then(|s| s.revision);
    assert_eq!(compiled, Some(revision), "the full build read the file at its current revision");
    env.drain(Duration::from_millis(300));
    env.prepend(&late, "Again ");
    assert!(env.changed(&late, "late.tex"), "known to be read now");
}

#[test]
fn after_a_failed_build_a_read_file_rebuilds_and_a_missing_input_that_appears_is_built() {
    let Some(mut env) = Env::new(
        &[
            ("main.tex", &*document("\\input{extra}\n\\input{missing}\n\\undefinedcommand")),
            ("extra.tex", "Extra text.\n"),
            ("notes.txt", "notes\n"),
        ],
        None,
    ) else {
        return;
    };
    let _main = env.open("main.tex");
    let extra = env.open("extra.tex");
    env.service.watch(&path("main.tex")).unwrap();
    let first = env.next_result();
    assert_ne!(first.result.status, BuildStatus::Succeeded, "missing input and an undefined command");
    env.drain(Duration::from_millis(300));
    // A file the failed build read.
    env.prepend(&extra, "More ");
    assert!(env.changed(&extra, "extra.tex"), "a read file changed after a failed build");
    // The missing file is created and opened: the build did not know it.
    std::fs::write(env.project.path().join("missing.tex"), "Now here.\n").unwrap();
    let missing = env.open("missing.tex");
    env.prepend(&missing, "More ");
    assert!(env.changed(&missing, "missing.tex"), "a document the latest build did not know must rebuild");
}

#[test]
fn the_same_document_under_a_path_the_build_did_not_know_is_not_skipped() {
    let Some(mut env) = Env::new(&[("main.tex", &*document("Text.")), ("notes.txt", "notes\n")], None) else {
        return;
    };
    let _main = env.open("main.tex");
    let notes = env.open("notes.txt");
    env.service.watch(&path("main.tex")).unwrap();
    env.next_result();
    env.drain(Duration::from_millis(300));
    env.prepend(&notes, "x");
    assert!(!env.changed(&notes, "notes.txt"), "known and unread");
    // The same document under a path the build did not have it at.
    env.prepend(&notes, "x");
    let revision = env.revision(&notes);
    env.service.rebuild_for_change(&notes, Some(&path("renamed.tex")), revision);
    let mut started = false;
    while let Some(event) = env.event_within(Duration::from_millis(if started { 60_000 } else { 600 })) {
        match event {
            TexEvent::Finished { summary, .. } if summary.result.status != BuildStatus::Canceled => break,
            _ => started = true,
        }
    }
    assert!(started, "a path the latest build did not know rebuilds");
}

// ---- burst settling -------------------------------------------------------

#[test]
fn an_idle_target_starts_at_once_and_a_superseding_request_waits_for_quiet() {
    // A long settle time makes the two cases unmistakable.
    let delay = Duration::from_secs(20);
    let Some(mut env) = Env::new(&[("main.tex", &*document("Text."))], Some(delay)) else { return };
    let main = env.open("main.tex");
    // Idle: the build runs and finishes long before the settle time.
    let started = Instant::now();
    let first = env.build(TexBuildMode::Draft);
    assert_eq!(first.result.status, BuildStatus::Succeeded);
    assert!(started.elapsed() < delay, "an idle target must not wait: {:?}", started.elapsed());
    // Idle again after a result: again at once.
    let started = Instant::now();
    env.build(TexBuildMode::Draft);
    assert!(started.elapsed() < delay, "idle after a result: {:?}", started.elapsed());

    // A request while a build runs waits; canceling during the wait starts nothing.
    env.insert_body(&main, "A ");
    env.service.request_build(Some(target()), TexBuildMode::Draft).unwrap();
    while !matches!(env.event_within(Duration::from_secs(60)).expect("the build starts"), TexEvent::Started { .. }) {}
    env.insert_body(&main, "B ");
    env.service.request_build(Some(target()), TexBuildMode::Draft).unwrap();
    // The running build is canceled; the superseding one must not have started 2 s later.
    let mut restarted = false;
    let until = Instant::now() + Duration::from_secs(2);
    while let Some(event) = env.event_within(until.saturating_duration_since(Instant::now())) {
        if matches!(event, TexEvent::Started { .. }) {
            restarted = true;
        }
    }
    assert!(!restarted, "a superseding request starts only after the settle time");
    let status = env.service.status(Some(&target())).unwrap();
    assert!(status.pending.is_some(), "the superseding request is pending");
    assert!(status.stale, "pending work marks the shown build stale");
    assert!(env.service.cancel(Some(&target())), "there was something to cancel");
    env.drain(Duration::from_secs(1));
    let status = env.service.status(Some(&target())).unwrap();
    assert!(status.pending.is_none() && status.running.is_none());
    // After the cancel the target is idle: a request starts at once and
    // builds the latest text.
    let started = Instant::now();
    let last = env.build(TexBuildMode::Draft);
    assert!(started.elapsed() < delay, "after a cancel the target is idle: {:?}", started.elapsed());
    assert_eq!(last.result.status, BuildStatus::Succeeded, "{:?}", last.result.diagnostics);
    let compiled = last.result.sources.iter().find(|s| s.path.as_str() == "main.tex").and_then(|s| s.revision);
    assert_eq!(compiled, Some(env.revision(&main)));
}

#[test]
fn the_last_edit_of_a_burst_is_always_built() {
    let Some(mut env) = Env::new(&[("main.tex", &*document("Text."))], None) else { return };
    let main = env.open("main.tex");
    env.service.watch(&path("main.tex")).unwrap();
    env.next_result();
    env.drain(Duration::from_millis(300));
    // Edits at different spacings around the settle time.
    for (round, spacing) in [10u64, 60, 140].into_iter().enumerate() {
        for _ in 0..12 {
            let revision = env.insert_body(&main, "x");
            env.service.rebuild_for_change(&main, Some(&path("main.tex")), revision);
            std::thread::sleep(Duration::from_millis(spacing));
        }
        let wanted = env.revision(&main);
        // Results arrive until one has the last revision; nothing may be left unbuilt.
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let status = env.service.status(Some(&target())).unwrap();
            let compiled = status
                .last
                .as_ref()
                .and_then(|l| l.result.sources.iter().find(|s| s.path.as_str() == "main.tex").and_then(|s| s.revision));
            if compiled == Some(wanted) && status.pending.is_none() && status.running.is_none() {
                assert_eq!(status.last.as_ref().unwrap().result.status, BuildStatus::Succeeded);
                assert!(!status.stale, "the built burst is not stale");
                break;
            }
            assert!(Instant::now() < deadline, "round {round}: revision {wanted} was never built ({compiled:?})");
            env.event_within(Duration::from_millis(200));
        }
        env.drain(Duration::from_millis(300));
    }
}

// ---- page images ----------------------------------------------------------

/// Across builds of different documents: whenever two pages have the same
/// reference, the PDFs render them to the same pixels (rendered here from each
/// build's own PDF, not through the service cache).
#[test]
fn equal_references_across_real_builds_mean_equal_pixels() {
    let variants = [
        "First page.\\newpage Second page.\\newpage Third page.",
        "First page.\\newpage Second page!\\newpage Third page.",
        "First page.\\newpage {\\pdfliteral{1 0 0 rg}Second page.}\\newpage Third page.",
        "First page.\\newpage {\\pdfliteral{0 0 1 rg}Second page.}\\newpage Third page.",
        "First page.\\newpage \\pdfpageattr{/Rotate 90}Second page.\\newpage \\pdfpageattr{}Third page.",
        "First page.\\newpage Second page.\\rule{10pt}{10pt}\\newpage Third page.",
        "First page.\\newpage Second page.\\rule{10pt}{11pt}\\newpage Third page.",
        "\\pdfpagewidth=500pt First page.\\newpage Second page.\\newpage Third page.",
        "First page.\\newpage \\pdfcompresslevel=0 Second page.\\newpage Third page.",
        "\\pdfobjcompresslevel=0 First page.\\newpage Second page.\\newpage Third page.",
        "First page.\\newpage Second page.\\newpage Third page.\\newpage Fourth page.",
        "{\\itshape First page.}\\newpage Second page.\\newpage Third page.",
    ];
    let Some(mut env) = Env::new(&[("main.tex", &*document(variants[0]))], None) else { return };
    let main = env.open("main.tex");
    let mut seen: HashMap<String, (usize, Vec<u8>)> = HashMap::new();
    let mut shared = 0;
    let mut content_refs = 0;
    for (index, body) in variants.iter().enumerate() {
        // Replace the whole text.
        let current = env.documents.snapshot(&main).unwrap();
        let length = exposition_core::text::utf16_len(&current.text) as u32;
        env.documents
            .apply(TransactionRequest {
                id: TransactionId(format!("replace-{index}")),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: main.clone(),
                    base_revision: current.revision,
                    patches: vec![TextPatch {
                        range: exposition_core::text::TextRange { start: 0, end: length },
                        text: document(body),
                    }],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        let summary = env.build(TexBuildMode::Draft);
        assert_eq!(summary.result.status, BuildStatus::Succeeded, "variant {index}: {:?}", summary.result.diagnostics);
        let stored = env.service.build(&summary.result.id).expect("retained");
        let pdf_path = stored.build.output.as_ref().expect("output").pdf.clone();
        let pdf = PdfDocument::open(std::fs::read(pdf_path).unwrap()).unwrap();
        for page in &summary.result.pages {
            let png = pdf.render_page(page.index as usize, PAGE_IMAGE_SCALE).unwrap().png;
            if page.render_ref.starts_with("tex-c-") {
                content_refs += 1;
            }
            // What the service serves for this reference is this page's pixels.
            let served = env.service.page_image(&page.render_ref).expect("served").bytes;
            assert_eq!(
                &*served,
                png.as_slice(),
                "variant {index} page {}: the served image is another page",
                page.index
            );
            match seen.get(&page.render_ref) {
                Some((other, pixels)) => {
                    shared += 1;
                    assert_eq!(pixels, &png, "variant {other} and {index} share {} with other pixels", page.render_ref);
                }
                None => {
                    seen.insert(page.render_ref.clone(), (index, png));
                }
            }
        }
    }
    eprintln!("{content_refs} content references, {shared} shared between builds, {} distinct", seen.len());
    assert!(content_refs > 0 && shared > 0, "the engine's output must give content references that repeat");
}

#[test]
fn the_scale_is_part_of_the_image_key_and_an_unknown_reference_is_not_served() {
    let Some(mut env) = Env::new(&[("main.tex", &*document("Text."))], None) else { return };
    let build = env.build(TexBuildMode::Draft);
    let id = &build.result.id;
    let small = env.service.page_png(id, 0, 1.0).unwrap();
    let large = env.service.page_png(id, 0, 3.0).unwrap();
    assert!(large.len() > small.len() && small != large);
    assert_eq!(env.service.page_png(id, 0, 1.0).unwrap(), small, "the small image again, not the large one");
    assert!(Arc::ptr_eq(&env.service.page_png(id, 0, 3.0).unwrap(), &large));
    assert!(env.service.page_png(id, 0, 0.1).is_err() && env.service.page_png(id, 0, f32::NAN).is_err());
    assert!(env.service.page_png(id, 7, 1.0).is_err());
    for reference in ["tex-c-", "tex-c-zz", "tex-", "tex-c-0000000000000000000000000000000000000000", "tex-1-p0"] {
        assert!(env.service.page_image(reference).is_none(), "{reference}");
    }
}

// ---- folders --------------------------------------------------------------

/// Without any release call: a page of a folder that is not open is not
/// served, by build or by content reference, even though its image is cached.
#[test]
fn a_cached_image_of_another_folder_is_not_served_by_its_reference() {
    let Some(mut env) = Env::new(&[("main.tex", &*document("Secret text."))], None) else { return };
    let first = env.build(TexBuildMode::Draft);
    let reference = first.result.pages[0].render_ref.clone();
    assert!(reference.starts_with("tex-c-"));
    assert!(env.service.page_image(&reference).is_some(), "cached now");
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("main.tex"), document("Other text.")).unwrap();
    env.workspace.open_workspace(other.path()).unwrap();
    // The application releases on the workspace event; before it does:
    assert!(env.service.page_image(&reference).is_none(), "by content reference");
    assert!(env.service.page_png(&first.result.id, 0, PAGE_IMAGE_SCALE).is_err(), "by build");
    assert!(env.service.build(&first.result.id).is_none());
    assert!(PageMapProvider::page_map(&env.service, &first.result.id.0).is_none());
    assert!(env.service.page_map(&first.result.id, 0).is_err());
    env.service.release_closed_folders();
    assert_eq!(env.service.retained(), (0, 0));
    assert!(env.service.page_image(&reference).is_none());
}

#[test]
fn a_build_in_flight_when_the_folder_changes_is_not_kept_and_the_new_folder_builds() {
    let Some(mut env) = Env::new(&[("main.tex", &*document("Folder A."))], None) else { return };
    let warm = env.build(TexBuildMode::Draft);
    assert_eq!(warm.result.status, BuildStatus::Succeeded);
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("main.tex"), document("Folder B.")).unwrap();
    env.exec.register_workspace_root(other.path()).unwrap();
    assert!(env.exec.grant_trust(&other.path().display().to_string()).unwrap().trusted);

    for wait_ms in [0u64, 150, 400, 900] {
        // A build of folder A is running (or just finishing) when B opens.
        env.workspace.open_workspace(env.project.path()).unwrap();
        env.service.release_closed_folders();
        env.service.request_build(Some(target()), TexBuildMode::Draft).unwrap();
        std::thread::sleep(Duration::from_millis(wait_ms));
        env.workspace.open_workspace(other.path()).unwrap();
        env.service.release_closed_folders();
        // Whatever folder A's worker still reports, nothing of it is kept.
        let mut reported = Vec::new();
        while let Some(event) = env.event_within(Duration::from_secs(3)) {
            if let TexEvent::Finished { summary, .. } = event {
                reported.push(summary);
            }
        }
        assert_eq!(env.service.retained(), (0, 0), "wait {wait_ms} ms: nothing of folder A survives");
        for summary in &reported {
            assert!(env.service.build(&summary.result.id).is_none());
            for page in &summary.result.pages {
                assert!(
                    env.service.page_image(&page.render_ref).is_none(),
                    "wait {wait_ms} ms: a page of folder A is served"
                );
            }
        }
        eprintln!(
            "wait {wait_ms} ms: folder A reported {:?}",
            reported.iter().map(|s| format!("{:?}", s.result.status)).collect::<Vec<_>>()
        );
        // Folder B builds its own text.
        let built = env.build(TexBuildMode::Draft);
        assert_eq!(built.result.status, BuildStatus::Succeeded, "{:?}", built.result.diagnostics);
        let status = env.service.status(Some(&target())).unwrap();
        assert_eq!(status.last.as_ref().map(|l| &l.result.id), Some(&built.result.id));
        assert_eq!(env.service.retained().0, 1);
        env.drain(Duration::from_millis(300));
    }
}
