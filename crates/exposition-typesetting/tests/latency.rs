//! Edit-to-layout latency: a one-character page edit, then an incremental
//! compile + glyph map + SVG render of the changed page. Run with
//! `cargo test [--release] -p exposition-typesetting --test latency -- --ignored --nocapture`.
mod common;

use std::sync::Arc;
use std::time::Instant;

use common::Fixture;
use exposition_core::build::{BuildStatus, BuildTarget, DocumentFormat, Engine};
use exposition_core::document::TextEncoding;
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_page::{PageEditOp, PageEditOutcome};
use exposition_typesetting::{Fonts, TypesettingService};
use exposition_workspace::{DocumentService, OpenDocument};

struct Stats(Vec<f64>);

impl Stats {
    fn report(&mut self, label: &str) {
        self.0.sort_by(f64::total_cmp);
        let n = self.0.len();
        let pct = |p: f64| self.0[((n as f64 - 1.0) * p).round() as usize];
        println!(
            "{label:<34} n={n:<3} median {:>7.2} ms  p95 {:>7.2} ms  max {:>7.2} ms",
            pct(0.5),
            pct(0.95),
            self.0[n - 1]
        );
    }
}

fn measure(fx: &Fixture, at_needle: &str, iterations: usize) {
    let cold = Instant::now();
    let record = fx.build();
    let cold_build = cold.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(record.result.status, BuildStatus::Succeeded);
    let t = Instant::now();
    for page in &record.result.pages {
        fx.service.page_svg(&page.render_ref).unwrap();
    }
    println!(
        "pages {}  cold build {cold_build:.1} ms (compile {:.1}, map {:.1})  cold render all pages {:.1} ms",
        record.result.pages.len(),
        record.timings.compile_ms,
        record.timings.map_ms,
        t.elapsed().as_secs_f64() * 1000.0
    );
    let mut edit = Stats(Vec::new());
    let mut compile = Stats(Vec::new());
    let mut map = Stats(Vec::new());
    let mut render = Stats(Vec::new());
    let mut total = Stats(Vec::new());
    let mut changed_pages = Stats(Vec::new());
    let mut caret = fx.offset_of(at_needle, 0);
    let mut previous = record;
    for i in 0..iterations {
        let started = Instant::now();
        let ch = if i % 2 == 0 { "e" } else { "x" };
        match fx.edit(TextRange::empty(caret), PageEditOp::Insert { text: ch.into() }, 1) {
            PageEditOutcome::Applied { caret: c, .. } => caret = c,
            other => panic!("{other:?}"),
        }
        let edited = started.elapsed().as_secs_f64() * 1000.0;
        let record = fx.build();
        let built = started.elapsed().as_secs_f64() * 1000.0;
        // Render only pages whose content changed.
        let mut changed = 0;
        for (page, old) in
            record.result.pages.iter().zip(previous.result.pages.iter().map(Some).chain(std::iter::repeat(None)))
        {
            if old.is_none_or(|o| o.render_ref != page.render_ref) {
                fx.service.page_svg(&page.render_ref).unwrap();
                changed += 1;
            }
        }
        let done = started.elapsed().as_secs_f64() * 1000.0;
        edit.0.push(edited);
        compile.0.push(record.timings.compile_ms);
        map.0.push(record.timings.map_ms);
        render.0.push(done - built);
        total.0.push(done);
        changed_pages.0.push(changed as f64);
        previous = record;
    }
    edit.report("page edit (plan+verify+apply)");
    compile.report("incremental compile");
    map.report("glyph map");
    render.report("SVG render of changed pages");
    total.report("edit -> compile -> map -> render");
    changed_pages.report("changed pages per edit (count)");
}

#[test]
#[ignore = "measurement; prints timings"]
fn warm_incremental_latency_fixture() {
    println!(
        "== typst-basic (1 page), profile: {}",
        if cfg!(debug_assertions) { "dev (deps opt-level 2)" } else { "release" }
    );
    let fx = Fixture::open("typst-basic", "main.typ");
    measure(&fx, "plain prose", 40);
}

#[test]
#[ignore = "measurement; prints timings"]
fn warm_incremental_latency_long_document() {
    // A synthetic ~20-page document built from the fixture's constructs.
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::read_to_string(common::fixture_dir("typst-basic").join("main.typ")).unwrap();
    let body = base.split_once("= Exposition Basics").unwrap().1;
    let mut text = String::from(
        "#set page(width: 14cm, height: 18cm, margin: 1.5cm)\n#set par(justify: true)\n#set heading(numbering: \"1.\")\n\n",
    );
    for i in 0..40 {
        text.push_str(&format!("= Section {i}\n{body}\n"));
    }
    std::fs::write(dir.path().join("main.typ"), &text).unwrap();
    let documents = Arc::new(DocumentService::new());
    let service = TypesettingService::new(Arc::clone(&documents), Fonts::embedded_only);
    service.open_workspace(dir.path()).unwrap();
    let path = WorkspacePath::parse("main.typ").unwrap();
    let info = documents.open(OpenDocument {
        path: Some(path.clone()),
        text,
        language: "typst".into(),
        encoding: TextEncoding::Utf8,
        read_only: None,
    });
    let fx = Fixture {
        documents,
        service,
        document: info.id,
        path: path.clone(),
        target: BuildTarget { format: DocumentFormat::Typst, engine: Engine::Typst, entry: path },
    };
    println!(
        "== synthetic long document, profile: {}",
        if cfg!(debug_assertions) { "dev (deps opt-level 2)" } else { "release" }
    );
    // Edit near the middle of the document.
    measure(&fx, "Section 20\n", 30);
}
