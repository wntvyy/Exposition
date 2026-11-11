//! Diagnostic dump of the glyph map of a fixture (run with --nocapture and
//! `--ignored`).
mod common;

use common::Fixture;

#[test]
#[ignore = "diagnostic output only"]
fn dump_typst_basic() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let record = fx.build();
    println!("status {:?} diagnostics {:?}", record.result.status, record.result.diagnostics);
    println!("timings {:?}", record.timings);
    let map = record.map.as_ref().unwrap();
    let text = fx.text();
    for (p, page) in map.pages.iter().enumerate() {
        println!("== page {p} {}x{}", page.width, page.height);
        for (l, line) in page.lines.iter().enumerate() {
            let rendered: String = line.clusters.iter().map(|c| c.text.as_str()).collect();
            println!(
                "line {l}: y {:.1}-{:.1} base {:.1} x {:.1}-{:.1} stops {} | {rendered}",
                line.top,
                line.bottom,
                line.baseline,
                line.x0,
                line.x1,
                line.stops.len()
            );
            for c in &line.clusters {
                if !c.is_editable() || c.equation.is_some() {
                    let src = c.source.map(|s| {
                        let r = exposition_core::text::utf16_range_to_bytes(&text, s.range).unwrap();
                        text[r].to_string()
                    });
                    println!(
                        "    {:?} {:?} eq={:?} src={:?} x {:.1}-{:.1}",
                        c.text, c.kind, c.equation, src, c.x0, c.x1
                    );
                }
            }
        }
    }
}
