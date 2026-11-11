//! Development aid: print the glyphs of a PDF page with recovered Unicode.
fn main() {
    let path = std::env::args().nth(1).expect("pdf path");
    let page: usize = std::env::args().nth(2).map(|p| p.parse().unwrap()).unwrap_or(0);
    let doc = exposition_tex::pdf::PdfDocument::open(std::fs::read(path).unwrap()).unwrap();
    let start = std::time::Instant::now();
    let glyphs = doc.extract_glyphs(page).unwrap();
    eprintln!("{} glyphs in {:?}", glyphs.len(), start.elapsed());
    let mut line = String::new();
    let mut last_y = f64::NAN;
    for g in &glyphs {
        if last_y.is_nan() || (g.origin_y - last_y).abs() > 2.0 {
            if !line.is_empty() {
                println!("{last_y:7.2}: {line}");
            }
            line.clear();
            last_y = g.origin_y;
        }
        line.push_str(g.text.as_deref().unwrap_or("\u{FFFD}"));
    }
    println!("{last_y:7.2}: {line}");
}
