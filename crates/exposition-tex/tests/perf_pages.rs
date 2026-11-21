//! Scale measurements of LaTeX builds as the page view uses them: page
//! images requested per build, the first page-map query after a build, and
//! engine starts during a burst of edits. Run with
//! `node scripts/perf/rust-bench.mjs exposition-tex`
//! (`EXPOSITION_PERF_SCALE` = small | large | stress). Skips with a message
//! when no TeX engine is installed.
//!
//! `page_images` counts model a page view that keeps the image of every page
//! reference it has already been given and requests the others: per build that
//! is the number of pages rasterized, PNG-encoded and sent.

mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

fn page_counts() -> Vec<usize> {
    match std::env::var("EXPOSITION_PERF_SCALE").as_deref() {
        Ok("large") => vec![100],
        Ok("stress") => vec![300],
        _ => vec![5, 30],
    }
}

fn perf(bench: &str, scale: &[(&str, f64)], samples: &[f64], counters: &[(&str, f64)]) {
    let object =
        |pairs: &[(&str, f64)]| pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect::<Vec<_>>().join(",");
    let samples = samples.iter().map(|s| format!("{s:.3}")).collect::<Vec<_>>().join(",");
    println!(
        "PERF {{\"bench\":\"{bench}\",\"scale\":{{{}}},\"samples_ms\":[{samples}],\"counters\":{{{}}}}}",
        object(scale),
        object(counters)
    );
}

fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// One section per page: prose with inline mathematics and five numbered
/// equations.
fn document(pages: usize) -> String {
    let mut text = String::from("\\documentclass[11pt]{article}\n\\begin{document}\n");
    for i in 0..pages {
        text.push_str(&format!(
            "\\section{{Section {i}}}\\label{{sec:{i}}}\nLet the map be continuous on a compact set, with $a^2 + b^2 = c^2$ and $\\alpha + \\beta \\leq \\gamma$ inline. anchor{i} plain prose follows here.\n"
        ));
        for j in 0..5 {
            text.push_str(&format!(
                "\\begin{{equation}}\\label{{eq:{i}:{j}}}\n  \\sum_{{k=0}}^{{n}} k^{j} = \\frac{{n (n + 1)}}{{2}} + \\int_0^{{{i}}} x^{j} \\, dx\n\\end{{equation}}\n"
            ));
        }
        text.push_str(
            "Hence the supremum is attained by some point of the domain, which proves the claim.\n\\newpage\n",
        );
    }
    text.push_str("\\end{document}\n");
    text
}

struct Env {
    runtime: tokio::runtime::Runtime,
    _data: tempfile::TempDir,
    _project: tempfile::TempDir,
    documents: Arc<DocumentService>,
    workspace: Arc<WorkspaceService>,
    service: TexService,
    events: broadcast::Receiver<TexEvent>,
}

fn entry() -> WorkspacePath {
    WorkspacePath::parse("main.tex").unwrap()
}

fn target() -> BuildTarget {
    BuildTarget { format: DocumentFormat::Latex, engine: Engine::PdfLatex, entry: entry() }
}

impl Env {
    /// A trusted temporary project with `main.tex` and an unrelated `notes.txt`.
    fn new(text: &str) -> Option<Self> {
        common::toolchain(Engine::PdfLatex)?;
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("main.tex"), text).unwrap();
        std::fs::write(project.path().join("notes.txt"), "notes\n").unwrap();
        let mut config = ExecutionConfig::new(data.path());
        config.too_broad_to_trust = Some(Vec::new());
        let exec = ExecutionService::with_config(config).unwrap();
        let documents = Arc::new(DocumentService::new());
        let workspace = WorkspaceService::new(documents.clone(), data.path().join("workspace"));
        workspace.open_workspace(project.path()).unwrap();
        exec.register_workspace_root(project.path()).unwrap();
        assert!(exec.grant_trust(&project.path().display().to_string()).unwrap().trusted);
        let mut tex_config = TexServiceConfig::new(data.path());
        // The automatic full build is requested explicitly where it is measured.
        tex_config.idle_full_delay = None;
        let service = TexService::new(exec, workspace.clone(), runtime.handle().clone(), tex_config).unwrap();
        let events = service.subscribe();
        Some(Self { runtime, _data: data, _project: project, documents, workspace, service, events })
    }

    fn open(&self, path: &str) -> DocumentId {
        self.workspace.open_document(&WorkspacePath::parse(path).unwrap()).unwrap().state.info.id
    }

    fn next_event(&mut self) -> TexEvent {
        self.runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(600), self.events.recv()).await })
            .expect("timed out waiting for a TeX event")
            .expect("event channel")
    }

    /// The next build that was not canceled.
    fn next_result(&mut self) -> TexBuildSummary {
        loop {
            if let TexEvent::Finished { summary, .. } = self.next_event()
                && summary.result.status != BuildStatus::Canceled
            {
                return *summary;
            }
        }
    }

    fn build(&mut self, mode: TexBuildMode) -> TexBuildSummary {
        self.service.request_build(Some(target()), mode).unwrap();
        let summary = self.next_result();
        assert_eq!(summary.result.status, BuildStatus::Succeeded, "{:?}", summary.result.diagnostics);
        summary
    }

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

    /// Request the image of every page whose reference the view does not hold
    /// yet: (images requested, PNG bytes, elapsed ms).
    fn fetch_new_pages(&self, summary: &TexBuildSummary, held: &mut HashSet<String>) -> (usize, usize, f64) {
        let started = Instant::now();
        let (mut images, mut bytes) = (0, 0);
        for page in &summary.result.pages {
            if held.insert(page.render_ref.clone()) {
                bytes += self.service.page_image(&page.render_ref).expect("page image").bytes.len();
                images += 1;
            }
        }
        (images, bytes, ms(started))
    }
}

/// What the application does for a change of an open document: rebuild the
/// watched entries the change can concern (at the base revision: all of them).
fn on_document_changed(env: &Env, document: &DocumentId) {
    let info = env.documents.info(document).unwrap();
    env.service.rebuild_for_change(document, info.path.as_ref(), info.revision);
}

#[test]
#[ignore = "perf"]
fn tex_page_images_and_page_map() {
    for pages in page_counts() {
        let text = document(pages);
        let Some(mut env) = Env::new(&text) else { return };
        let scale = [("pages", pages as f64), ("source_bytes", text.len() as f64)];
        let main = env.open("main.tex");
        let middle = pages / 2;

        let started = Instant::now();
        let first = env.build(TexBuildMode::Full);
        let cold = ms(started);
        assert_eq!(first.result.pages.len(), pages, "one page per section");
        let mut held = HashSet::new();
        let (images, bytes, elapsed) = env.fetch_new_pages(&first, &mut held);
        perf("tex.first_build_full", &scale, &[cold], &[("engine_passes", first.passes.len() as f64)]);
        perf(
            "tex.page_images_first_build",
            &scale,
            &[elapsed],
            &[("page_images", images as f64), ("png_bytes", bytes as f64)],
        );

        // A rebuild of unchanged sources (what the automatic full build after
        // a draft is when the draft already settled the references).
        let (mut build_ms, mut fetch_ms) = (Vec::new(), Vec::new());
        let (mut images, mut bytes) = (0, 0);
        for _ in 0..5 {
            let started = Instant::now();
            let summary = env.build(TexBuildMode::Full);
            build_ms.push(ms(started));
            let (i, b, elapsed) = env.fetch_new_pages(&summary, &mut held);
            fetch_ms.push(elapsed);
            (images, bytes) = (i, b);
        }
        perf("tex.rebuild_unchanged", &scale, &build_ms, &[]);
        perf(
            "tex.page_images_unchanged_rebuild",
            &scale,
            &fetch_ms,
            &[("page_images", images as f64), ("png_bytes", bytes as f64)],
        );

        // A draft after a one-character edit on the middle page, then the
        // first page-map query (the caret) on that build.
        let (mut build_ms, mut fetch_ms, mut map_first, mut map_again) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut images, mut bytes) = (0, 0);
        for _ in 0..5 {
            env.insert(&main, &format!("anchor{middle} "), "x");
            let started = Instant::now();
            let summary = env.build(TexBuildMode::Draft);
            build_ms.push(ms(started));
            let (i, b, elapsed) = env.fetch_new_pages(&summary, &mut held);
            fetch_ms.push(elapsed);
            (images, bytes) = (i, b);
            let started = Instant::now();
            assert!(PageMapProvider::page_map(&env.service, &summary.result.id.0).is_some());
            map_first.push(ms(started));
            let started = Instant::now();
            assert!(PageMapProvider::page_map(&env.service, &summary.result.id.0).is_some());
            map_again.push(ms(started));
        }
        perf("tex.rebuild_draft_one_char", &scale, &build_ms, &[]);
        perf(
            "tex.page_images_after_edit",
            &scale,
            &fetch_ms,
            &[("page_images", images as f64), ("png_bytes", bytes as f64)],
        );
        perf("tex.first_page_map", &scale, &map_first, &[]);
        perf("tex.page_map_again", &scale, &map_again, &[]);

        // Where the first page-map query spends its time: glyph extraction
        // from the PDF, mapping glyphs to source, and building the shared map.
        let latest = env.build(TexBuildMode::Draft);
        let stored = env.service.build(&latest.result.id).expect("retained build");
        let session = exposition_tex::TexPageSession::open(&stored.build).expect("page session");
        let (mut total, mut extract, mut map, mut assemble) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for _ in 0..5 {
            let all = Instant::now();
            let (mut extract_ms, mut map_ms) = (0.0, 0.0);
            let mut mapped = Vec::new();
            for page in 0..session.page_count() {
                let started = Instant::now();
                let glyphs = session.pdf.extract_glyphs_and_rules(page).unwrap();
                extract_ms += ms(started);
                let started = Instant::now();
                let mapping =
                    exposition_tex::mapping::map_page(page as u32, glyphs, &session.synctex, &session.sources);
                map_ms += ms(started);
                mapped.push((mapping, session.pdf.page_size(page).unwrap()));
            }
            let started = Instant::now();
            let built = exposition_tex::page_map::build_page_map(&mapped);
            assemble.push(ms(started));
            assert_eq!(built.pages.len(), pages);
            extract.push(extract_ms);
            map.push(map_ms);
            total.push(ms(all));
        }
        let median = |values: &[f64]| {
            let mut sorted = values.to_vec();
            sorted.sort_by(f64::total_cmp);
            sorted[sorted.len() / 2]
        };
        perf(
            "tex.page_map_breakdown",
            &scale,
            &total,
            &[
                ("extract_glyphs_ms_median", median(&extract)),
                ("map_to_source_ms_median", median(&map)),
                ("build_map_ms_median", median(&assemble)),
            ],
        );
    }
}

/// Engine starts for bursts of edits at ten per second, as the application
/// forwards them: in the entry itself, and in an open file the entry does not
/// read.
#[test]
#[ignore = "perf"]
fn tex_edit_bursts() {
    let pages = page_counts()[0];
    let text = document(pages);
    let Some(mut env) = Env::new(&text) else { return };
    let scale = [("pages", pages as f64), ("edits", 20.0)];
    let main = env.open("main.tex");
    let notes = env.open("notes.txt");
    env.service.watch(&entry()).unwrap();
    let first = env.next_result();
    assert_eq!(first.result.status, BuildStatus::Succeeded, "{:?}", first.result.diagnostics);

    // (builds started, engine processes, builds canceled, results) until the
    // target has been quiet for two seconds.
    fn drain(env: &mut Env) -> (usize, usize, usize, usize) {
        let (mut started, mut passes, mut canceled, mut results) = (0, 0, 0, 0);
        loop {
            let next =
                env.runtime.block_on(async { tokio::time::timeout(Duration::from_secs(2), env.events.recv()).await });
            match next {
                Ok(Ok(TexEvent::Started { .. })) => started += 1,
                Ok(Ok(TexEvent::Finished { summary, .. })) => {
                    passes += summary.passes.len();
                    if summary.result.status == BuildStatus::Canceled {
                        canceled += 1;
                    } else {
                        results += 1;
                    }
                }
                Ok(Ok(TexEvent::Queued { .. })) => {}
                Ok(Err(e)) => panic!("event channel: {e}"),
                Err(_) => return (started, passes, canceled, results),
            }
        }
    }

    for (bench, edited, needle) in
        [("tex.burst_in_entry", &main, "anchor0 "), ("tex.burst_in_unrelated_file", &notes, "notes")]
    {
        let mut samples = Vec::new();
        let mut counters = (0, 0, 0, 0);
        for _ in 0..5 {
            let started = Instant::now();
            for _ in 0..20 {
                env.insert(edited, needle, "x");
                on_document_changed(&env, edited);
                std::thread::sleep(Duration::from_millis(100));
            }
            counters = drain(&mut env);
            // Without the two quiet seconds that end the drain.
            samples.push(ms(started) - 2000.0);
        }
        perf(
            bench,
            &scale,
            &samples,
            &[
                ("builds_started", counters.0 as f64),
                ("engine_processes", counters.1 as f64),
                ("builds_canceled", counters.2 as f64),
                ("results", counters.3 as f64),
            ],
        );
    }
}
