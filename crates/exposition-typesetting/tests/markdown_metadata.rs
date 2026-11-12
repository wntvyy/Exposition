//! The Markdown print profile's PDF metadata: title, authors, and a calendar
//! `date` from the front matter reach the exported PDF.

mod common;

use common::Fixture;
use exposition_core::build::{BuildTarget, DocumentFormat, Engine};

#[test]
fn front_matter_metadata_reaches_the_pdf() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.md"),
        "---\ntitle: Metadata Check\nauthor: Ada Lovelace\ndate: 1999-12-31\n---\n\nBody text.\n",
    )
    .unwrap();
    let fx = Fixture::open_dir(dir.path(), "main.md");
    let target =
        BuildTarget { format: DocumentFormat::Markdown, engine: Engine::MarkdownPrint, entry: fx.path.clone() };
    let record = fx.service.build(&target).expect("build runs");
    assert!(record.succeeded(), "{:?}", record.result.diagnostics);
    let out = tempfile::tempdir().unwrap();
    let pdf = out.path().join("out.pdf");
    fx.service.export_pdf(record.id(), Some(pdf.clone())).unwrap();
    let bytes = String::from_utf8_lossy(&std::fs::read(&pdf).unwrap()).into_owned();
    assert!(bytes.contains("Metadata Check"), "title in the PDF metadata");
    assert!(bytes.contains("Ada Lovelace"), "author in the PDF metadata");
    assert!(bytes.contains("D:19991231") || bytes.contains("1999-12-31"), "date in the PDF metadata");
}
