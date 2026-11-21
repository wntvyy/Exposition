//! Scale measurement of the Markdown print profile's translation to Typst
//! (run once per build of a Markdown document). Run with
//! `node scripts/perf/rust-bench.mjs exposition-markdown`.

use std::time::Instant;

use exposition_markdown::{TranslateOptions, translate};

fn sizes() -> Vec<usize> {
    match std::env::var("EXPOSITION_PERF_SCALE").as_deref() {
        Ok("large") => vec![1 << 20],
        Ok("stress") => vec![10 << 20],
        _ => vec![20_000, 200_000],
    }
}

/// Sections with prose, inline and display mathematics, a list and a table.
fn document(bytes: usize) -> String {
    let mut text = String::from("---\ntitle: Scale document\n---\n\n");
    let mut i = 0;
    while text.len() < bytes {
        text.push_str(&format!(
            "## Section {i}\n\nLet the map be *continuous* on a compact set, with $a_{{{i}}}^2 + b^2 = c^2$ and \
             $\\alpha + \\beta \\leq \\gamma$ inline, and a [link](https://example.org/{i}).\n\n\
             $$\n\\sum_{{k=0}}^{{n}} k = \\frac{{n (n + 1)}}{{2}}\n$$\n\n\
             - first item {i}\n- second item with `code`\n- third item\n\n\
             | a | b |\n|---|---|\n| {i} | $x^2$ |\n\n\
             Hence the supremum is attained by some point of the domain, which proves the claim.\n\n"
        ));
        i += 1;
    }
    text
}

#[test]
#[ignore = "perf"]
fn markdown_translate() {
    for bytes in sizes() {
        let text = document(bytes);
        let options = TranslateOptions::default();
        let mut samples = Vec::new();
        let mut out = None;
        for sample in 0..6 {
            let started = Instant::now();
            let translation = translate(&text, &options);
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            if sample > 0 {
                samples.push(elapsed);
            }
            out = Some(translation);
        }
        let out = out.unwrap();
        let samples = samples.iter().map(|s| format!("{s:.3}")).collect::<Vec<_>>().join(",");
        println!(
            "PERF {{\"bench\":\"markdown.translate\",\"scale\":{{\"source_bytes\":{}}},\"samples_ms\":[{samples}],\"counters\":{{\"typst_bytes\":{},\"equations\":{},\"diagnostics\":{}}}}}",
            text.len(),
            out.typst.len(),
            out.equations.len(),
            out.diagnostics.len()
        );
    }
}
