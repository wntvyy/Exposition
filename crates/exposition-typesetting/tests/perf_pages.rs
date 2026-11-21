//! Scale measurements of Typst builds, page images and page-map queries.
//! Run with `node scripts/perf/rust-bench.mjs exposition-typesetting`
//! (`EXPOSITION_PERF_SCALE` = small | large | stress).
//!
//! The documents have one section per page with several display equations.
//! Counters model what a page view asks for: `fetch_all` is a view that
//! requests every page of a build, `fetch_viewport` one that requests three.
mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use common::Fixture;
use exposition_core::build::BuildStatus;
use exposition_core::text::TextRange;
use exposition_page::{Affinity, Motion, PageEditOp, PageEditOutcome, PageMapProvider};

fn page_counts() -> Vec<usize> {
    match std::env::var("EXPOSITION_PERF_SCALE").as_deref() {
        Ok("large") => vec![200],
        Ok("stress") => vec![500],
        _ => vec![5, 50],
    }
}

fn perf(bench: &str, scale: &[(&str, f64)], samples: &[f64], counters: &[(&str, f64)]) {
    let object = |pairs: &[(&str, f64)]| {
        pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect::<Vec<_>>().join(",")
    };
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

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

/// A document of exactly `pages` pages: per page a heading, prose with inline
/// mathematics, and five numbered display equations.
fn document(pages: usize) -> String {
    let mut text = String::from(
        "#set page(width: 14cm, height: 18cm, margin: 1.5cm)\n#set par(justify: true)\n#set heading(numbering: \"1.\")\n#set math.equation(numbering: \"(1)\")\n\n",
    );
    for i in 0..pages {
        text.push_str(&format!(
            "= Section {i} <sec-{i}>\nLet the map be continuous on a compact set, with $a_{i}^2 + b_{i}^2 = c_{i}^2$ and $alpha + beta <= gamma$ inline. anchor{i} plain prose follows here.\n\n"
        ));
        for j in 0..5 {
            text.push_str(&format!(
                "$ sum_(k = 0)^n k^{j} = (n (n + 1)) / 2 + integral_0^{i} x^{j} dif x $ <eq-{i}-{j}>\n\n"
            ));
        }
        text.push_str("Hence the supremum is attained by some point of the domain, which proves the claim.\n");
        if i + 1 < pages {
            text.push_str("#pagebreak()\n\n");
        }
    }
    text
}

fn open(text: &str) -> (Fixture, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.typ"), text).unwrap();
    (Fixture::open_dir(dir.path(), "main.typ"), dir)
}

#[test]
#[ignore = "perf"]
fn typst_build_and_page_images() {
    for pages in page_counts() {
        let text = document(pages);
        let scale = [("pages", pages as f64), ("source_bytes", text.len() as f64)];
        let middle = pages / 2;

        // First build and first page requests, each on a fresh service
        // (one warm-up, then five samples).
        let (mut first, mut compile, mut map) = (Vec::new(), Vec::new(), Vec::new());
        let (mut viewport, mut all) = (Vec::new(), Vec::new());
        let (mut bytes_total, mut bytes_max, mut hits) = (0usize, 0usize, 0usize);
        let mut last = None;
        for sample in 0..6 {
            let (fx, dir) = open(&text);
            let started = Instant::now();
            let record = fx.build();
            let built = ms(started);
            assert_eq!(record.result.status, BuildStatus::Succeeded);
            assert_eq!(record.result.pages.len(), pages, "one page per section");
            // A view that requests the three pages around the middle page.
            let started = Instant::now();
            for page in record.result.pages.iter().skip(middle.saturating_sub(1)).take(3) {
                fx.service.page_image(&page.render_ref).expect("page image");
            }
            let window = ms(started);
            // A view that requests every page.
            let started = Instant::now();
            let mut images = Vec::new();
            for page in &record.result.pages {
                images.push(fx.service.page_svg(&page.render_ref).expect("page svg"));
            }
            let everything = ms(started);
            if sample > 0 {
                first.push(built);
                compile.push(record.timings.compile_ms);
                map.push(record.timings.map_ms);
                viewport.push(window);
                all.push(everything);
            }
            bytes_total = images.iter().map(|svg| svg.len()).sum();
            bytes_max = images.iter().map(|svg| svg.len()).max().unwrap_or(0);
            // A second view of the same build: a page still cached comes back
            // as the same allocation; any other was rendered again.
            hits = record
                .result
                .pages
                .iter()
                .zip(&images)
                .filter(|(page, svg)| Arc::ptr_eq(&fx.service.page_svg(&page.render_ref).unwrap(), svg))
                .count();
            last = Some((fx, dir, record));
        }
        perf(
            "typst.first_build",
            &scale,
            &first,
            &[("compile_ms_median", median(&compile)), ("map_ms_median", median(&map))],
        );
        perf("typst.fetch_viewport_pages", &scale, &viewport, &[("page_images", 3.0f64.min(pages as f64))]);
        perf(
            "typst.fetch_all_pages",
            &scale,
            &all,
            &[
                ("page_images", pages as f64),
                ("bytes_total", bytes_total as f64),
                ("bytes_max_page", bytes_max as f64),
                ("second_view_cache_hits", hits as f64),
                ("second_view_renders", (pages - hits) as f64),
            ],
        );

        // Rebuild after a one-character page edit on the middle page; the view
        // keeps the images whose reference it already has.
        let (fx, _dir, mut previous) = last.unwrap();
        let mut held: HashSet<String> = previous.result.pages.iter().map(|p| p.render_ref.clone()).collect();
        let mut caret = fx.offset_of(&format!("anchor{middle} "), 0);
        let (mut edit, mut rebuild, mut compile, mut map, mut fetch) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut changed_pages, mut fetched_bytes) = (0usize, 0usize);
        for sample in 0..6 {
            let started = Instant::now();
            match fx.edit(TextRange::empty(caret), PageEditOp::Insert { text: "x".into() }, 1) {
                PageEditOutcome::Applied { caret: c, .. } => caret = c,
                other => panic!("{other:?}"),
            }
            let edited = ms(started);
            let started = Instant::now();
            let record = fx.build();
            let built = ms(started);
            assert_eq!(record.result.status, BuildStatus::Succeeded);
            let started = Instant::now();
            let (mut changed, mut bytes) = (0, 0);
            for page in &record.result.pages {
                if held.insert(page.render_ref.clone()) {
                    bytes += fx.service.page_image(&page.render_ref).expect("page image").bytes.len();
                    changed += 1;
                }
            }
            let fetched = ms(started);
            if sample > 0 {
                edit.push(edited);
                rebuild.push(built);
                compile.push(record.timings.compile_ms);
                map.push(record.timings.map_ms);
                fetch.push(fetched);
            }
            changed_pages = changed;
            fetched_bytes = bytes;
            previous = record;
        }
        perf("page.edit_one_char", &scale, &edit, &[]);
        perf(
            "typst.rebuild_one_char",
            &scale,
            &rebuild,
            &[("compile_ms_median", median(&compile)), ("map_ms_median", median(&map))],
        );
        perf(
            "typst.fetch_changed_pages",
            &scale,
            &fetch,
            &[("page_images", changed_pages as f64), ("bytes", fetched_bytes as f64)],
        );

        // Page-map queries on the last page of the shown build.
        let map = previous.map.clone().expect("page map");
        let last_page = pages - 1;
        let at = fx.offset_of(&format!("anchor{last_page} "), 0);
        let batch = |run: &dyn Fn()| -> Vec<f64> {
            (0..6)
                .map(|_| {
                    let started = Instant::now();
                    for _ in 0..50 {
                        run();
                    }
                    ms(started) / 50.0
                })
                .skip(1)
                .collect()
        };
        let lookup = batch(&|| {
            assert!(fx.service.page_map(previous.id()).is_some());
        });
        perf("page.map_lookup", &scale, &lookup, &[]);
        let caret_samples = batch(&|| {
            assert!(map.caret_geometry(&fx.path, at, Affinity::Downstream).is_some());
        });
        perf("page.caret", &scale, &caret_samples, &[]);
        let word = batch(&|| {
            map.selection(&fx.path, at, at + 6);
        });
        let one_word = map.selection(&fx.path, at, at + 6);
        perf("page.selection_word", &scale, &word, &[("rects", one_word.rects.len() as f64)]);
        let whole_len = exposition_core::text::utf16_len(&fx.text()) as u32;
        let whole = batch(&|| {
            map.selection(&fx.path, 0, whole_len);
        });
        let everything = map.selection(&fx.path, 0, whole_len);
        perf(
            "page.selection_all",
            &scale,
            &whole,
            &[("rects", everything.rects.len() as f64), ("text_bytes", everything.text.len() as f64)],
        );
        let hit = batch(&|| {
            map.hit_test(last_page as u32, 120.0, 200.0);
        });
        perf("page.hit_test", &scale, &hit, &[]);
        let down = batch(&|| {
            map.navigate(&fx.path, at, Affinity::Downstream, Motion::Down, None);
        });
        perf("page.navigate_down", &scale, &down, &[]);
    }
}
