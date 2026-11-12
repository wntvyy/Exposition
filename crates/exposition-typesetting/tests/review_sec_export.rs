//! R-sec (M6 security review): Typst/Markdown PDF export destinations.
//!
//! `typesetting_export_pdf` takes a caller-supplied `destination` straight from
//! IPC. R-m23 F6 made the TeX export refuse to replace a file that is not a
//! PDF, an input of the build, or an open document
//! (`exposition-tex/tests/service.rs::exports_never_replace_sources_or_other_files`).
//! These tests check the same rules for the Typst/Markdown export
//! (`TypesettingService::export_pdf`).
//!
//! Failing checks are `#[ignore = "R-sec Fn: ..."]`; run them with
//! `cargo test -p exposition-typesetting --test review_sec_export -- --include-ignored`.

mod common;

use std::path::PathBuf;

use common::Fixture;

fn fixture() -> Fixture {
    Fixture::open("typst-basic", "main.typ")
}

/// HELD: relative paths, non-.pdf names, and missing folders are refused.
#[test]
fn export_refuses_relative_non_pdf_and_missing_folders() {
    let fx = fixture();
    let record = fx.build();
    let outside = tempfile::tempdir().unwrap();
    for destination in [
        PathBuf::from("relative.pdf"),
        outside.path().join("notes.txt"),
        outside.path().join("missing-folder").join("out.pdf"),
        outside.path().to_path_buf(),
    ] {
        let outcome = fx.service.export_pdf(record.id(), Some(destination.clone()));
        assert!(outcome.is_err(), "{} accepted: {outcome:?}", destination.display());
    }
}

/// R-sec F-export-a: an existing file named `*.pdf` that is NOT a PDF is
/// silently replaced (the TeX export refuses this since R-m23 F6).
#[test]
#[ignore = "R-sec F4: Typst export replaces an existing non-PDF file named *.pdf (TeX refuses)"]
fn export_never_replaces_a_file_that_is_not_a_pdf() {
    let fx = fixture();
    let record = fx.build();
    let outside = tempfile::tempdir().unwrap();
    let notes = outside.path().join("notes.pdf");
    std::fs::write(&notes, "my notes, not a PDF").unwrap();
    let outcome = fx.service.export_pdf(record.id(), Some(notes.clone()));
    let after = std::fs::read(&notes).unwrap();
    assert!(
        outcome.is_err() && after == b"my notes, not a PDF",
        "DEFECT: export over a non-PDF file was accepted ({outcome:?}); file now starts with {:?}",
        String::from_utf8_lossy(&after[..after.len().min(8)])
    );
}

/// R-sec F-export-b: an open document of the workspace (a text file that
/// happens to be named `*.pdf`) is replaced behind the document service.
#[test]
#[ignore = "R-sec F4: Typst export replaces an open document's file on disk"]
fn export_never_replaces_an_open_document() {
    let fx = fixture();
    let record = fx.build();
    let notes = fx.root.join("notes.pdf");
    std::fs::write(&notes, "plain text notes\n").unwrap();
    let path = exposition_core::path::WorkspacePath::parse("notes.pdf").unwrap();
    // Opening succeeds only if the workspace treats it as text.
    if fx.workspace.open_document(&path).is_err() {
        eprintln!("notes.pdf did not open as a text document; check not applicable");
        return;
    }
    let outcome = fx.service.export_pdf(record.id(), Some(notes.clone()));
    let after = std::fs::read(&notes).unwrap();
    assert!(
        outcome.is_err() && after == b"plain text notes\n",
        "DEFECT: export over an open document was accepted ({outcome:?})"
    );
}

/// R-sec F-export-c: a PDF the document itself includes (an input of the
/// build) is replaced by the export.
#[test]
#[ignore = "R-sec F4: Typst export replaces a PDF the document includes (a build input)"]
fn export_never_replaces_an_input_of_the_build() {
    let fx = fixture();
    let first = fx.build();
    // Make a real PDF figure inside the project from the first build.
    let figure = fx.root.join("fig.pdf");
    fx.service.export_pdf(first.id(), Some(figure.clone())).unwrap();
    // Include it in the document (append at the end of the buffer).
    // A source edit (page typing would escape the markup).
    use exposition_core::document::{
        ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
    };
    let text = fx.text();
    let end = exposition_core::text::utf16_len(&text) as u32;
    fx.documents
        .apply(TransactionRequest {
            id: TransactionId("review-sec-figure".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: fx.document.clone(),
                base_revision: fx.revision(),
                patches: vec![TextPatch {
                    range: exposition_core::text::TextRange { start: end, end },
                    text: "\n\n#image(\"fig.pdf\", width: 2cm)\n".into(),
                }],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    let with_figure = fx.build();
    let uses_figure = with_figure.result.sources.iter().any(|s| s.path.as_str() == "fig.pdf");
    if !uses_figure {
        eprintln!(
            "the build does not list fig.pdf as a source (diagnostics: {:?}); check not applicable",
            with_figure.result.diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>()
        );
        return;
    }
    let before = std::fs::read(&figure).unwrap();
    let outcome = fx.service.export_pdf(with_figure.id(), Some(figure.clone()));
    let after = std::fs::read(&figure).unwrap();
    assert!(
        outcome.is_err() && after == before,
        "DEFECT: export over an input of the build was accepted ({outcome:?}); fig.pdf changed: {}",
        after != before
    );
}

/// R-sec: the default destination (`<entry stem>.pdf` next to the entry)
/// must also never replace a non-PDF file.
#[test]
#[ignore = "R-sec F4: Typst default export replaces an existing non-PDF <entry>.pdf"]
fn default_export_never_replaces_a_non_pdf() {
    let fx = fixture();
    let record = fx.build();
    let default = fx.root.join("main.pdf");
    std::fs::write(&default, "hand-written main.pdf that is not a PDF").unwrap();
    let outcome = fx.service.export_pdf(record.id(), None);
    let after = std::fs::read(&default).unwrap();
    assert!(
        outcome.is_err() && after == b"hand-written main.pdf that is not a PDF",
        "DEFECT: default export over a non-PDF main.pdf was accepted ({outcome:?})"
    );
}

/// Windows: a destination naming an alternate data stream of an existing
/// source file (`main.typ:x.pdf`) must not write into that file.
#[cfg(windows)]
#[test]
fn export_to_an_alternate_data_stream_of_a_source_is_refused_or_harmless() {
    let fx = fixture();
    let record = fx.build();
    let main = fx.root.join("main.typ");
    let before = std::fs::read(&main).unwrap();
    let stream = PathBuf::from(format!("{}:x.pdf", main.display()));
    let outcome = fx.service.export_pdf(record.id(), Some(stream.clone()));
    let stream_written = std::fs::read(&stream).map(|b| b.starts_with(b"%PDF-")).unwrap_or(false);
    let leftovers: Vec<String> = std::fs::read_dir(&fx.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert_eq!(std::fs::read(&main).unwrap(), before, "main.typ's content changed");
    assert!(!stream_written, "DEFECT: export wrote a PDF into the alternate data stream main.typ:x.pdf ({outcome:?})");
    assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
}
