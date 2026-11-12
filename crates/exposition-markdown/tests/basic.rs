mod common;

use common::*;
use exposition_core::path::WorkspacePath;
use exposition_markdown::{TranslateOptions, translate};

#[test]
fn dump_basic() {
    let md = read_fixture("markdown-basic");
    let tr = translate(&md, &TranslateOptions { document: Some(WorkspacePath::parse("main.md").unwrap()) });
    std::fs::write(std::env::temp_dir().join("exmd-basic.typ"), &tr.typst).unwrap();
    for d in &tr.diagnostics {
        println!("diag {:?} {} {:?}", d.severity, d.message, md_slice(&md, d.range));
    }
    let compiler = Compiler::new(fixture_dir("markdown-basic"));
    let out = compiler.compile(&tr.typst);
    for e in &out.errors {
        println!("ERROR {e}");
    }
    for w in &out.warnings {
        println!("warning {w}");
    }
    let doc = out.document.expect("compiled");
    let glyphs = glyphs(&doc, &compiler.source());
    let cov = coverage(&md, &tr, &glyphs);
    println!("{cov:#?}");
    for t in page_texts(&glyphs) {
        println!("PAGE: {t}");
    }
}
