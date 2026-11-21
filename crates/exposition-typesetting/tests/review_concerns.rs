//! Review: attacks on "a change concerns an entry" for Typst builds. A wrong
//! "no" leaves pages of an older state presented as current.
mod common;

use common::Fixture;
use exposition_core::build::BuildStatus;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::utf16_len;

fn path(text: &str) -> WorkspacePath {
    WorkspacePath::parse(text).unwrap()
}

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        let file = dir.path().join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    dir
}

fn open(fx: &Fixture, name: &str) -> DocumentId {
    fx.workspace.open_document(&path(name)).unwrap().state.info.id
}

/// Append `text` to a document; returns the new revision.
fn append(fx: &Fixture, document: &DocumentId, text: &str) -> u64 {
    let state = fx.documents.state(document).unwrap();
    let at = utf16_len(&state.text) as u32;
    let outcome = fx
        .documents
        .apply(TransactionRequest {
            id: TransactionId(format!("t-{}-{}", document.0, state.info.revision)),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: document.clone(),
                base_revision: state.info.revision,
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

fn refs(record: &exposition_typesetting::BuildRecord) -> Vec<String> {
    record.result.pages.iter().map(|page| page.render_ref.clone()).collect()
}

/// Every way a Typst document reads another file: an edit of that file (open
/// as a document) must concern the entry, and building it must give other
/// pages (so the case means something).
#[test]
fn files_read_indirectly_concern_the_entry() {
    let main = "#import \"lib/defs.typ\": greeting\n#include \"parts/outer.typ\"\n#greeting\n\
                #read(\"data/plain.txt\")\n#json(\"data/values.json\").at(\"name\")\n\
                #csv(\"data/table.csv\").at(0).at(0)\n#yaml(\"data/config.yaml\").at(\"key\")\n\
                #include \"./parts/../dotted.typ\"\n";
    let files = [
        ("main.typ", main),
        ("lib/defs.typ", "#let greeting = [Hello]\n"),
        ("parts/outer.typ", "Outer.\n#include \"inner.typ\"\n"),
        ("parts/inner.typ", "Inner.\n"),
        ("data/plain.txt", "plain"),
        ("data/values.json", "{\"name\": \"json\"}"),
        ("data/table.csv", "cell,other\n"),
        ("data/config.yaml", "key: yaml\n"),
        ("dotted.typ", "Dotted.\n"),
        ("unread.typ", "Unread.\n"),
    ];
    let dir = project(&files);
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let entry = fx.path.clone();
    let edits: [(&str, &str); 8] = [
        ("lib/defs.typ", "#let greeting = [Changed]\n"),
        ("parts/outer.typ", "More outer.\n"),
        ("parts/inner.typ", "More inner.\n"),
        ("data/plain.txt", " more"),
        ("data/values.json", " "),
        ("data/table.csv", "x,y\n"),
        ("data/config.yaml", "other: 1\n"),
        ("dotted.typ", "More dotted.\n"),
    ];
    let documents: Vec<DocumentId> = edits.iter().map(|(name, _)| open(&fx, name)).collect();
    let unread = open(&fx, "unread.typ");
    let mut shown = fx.build();
    assert_eq!(shown.result.status, BuildStatus::Succeeded, "{:?}", shown.result.diagnostics);
    for ((name, text), document) in edits.iter().zip(&documents) {
        let revision = append(&fx, document, text);
        assert!(fx.service.change_concerns(&entry, document, revision), "{name}: a file the entry reads changed");
        let rebuilt = fx.build();
        assert_eq!(rebuilt.result.status, BuildStatus::Succeeded, "{name}: {:?}", rebuilt.result.diagnostics);
        assert_ne!(rebuilt.result.snapshot, shown.result.snapshot, "{name}: the build read the change");
        assert!(!fx.service.change_concerns(&entry, document, revision), "{name}: built");
        shown = rebuilt;
    }
    // The control: an open file nothing reads.
    let revision = append(&fx, &unread, "More.\n");
    assert!(!fx.service.change_concerns(&entry, &unread, revision));
    assert_eq!(refs(&fx.build()), refs(&shown));
}

/// The include names the file in another letter case than the open document.
#[cfg(windows)]
#[test]
fn a_file_included_under_another_letter_case_concerns_the_entry() {
    let dir = project(&[("main.typ", "#include \"Chapter.TYP\"\n"), ("chapter.typ", "Chapter.\n")]);
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let chapter = open(&fx, "chapter.typ");
    let shown = fx.build();
    assert_eq!(shown.result.status, BuildStatus::Succeeded, "{:?}", shown.result.diagnostics);
    let revision = append(&fx, &chapter, "More.\n");
    let concerns = fx.service.change_concerns(&fx.path, &chapter, revision);
    let rebuilt = fx.build();
    assert!(concerns || refs(&rebuilt) == refs(&shown), "skipped, but a build of the new state has other pages");
}

/// A missing include that appears as a newly opened document, and a build
/// that is no longer retained.
#[test]
fn a_missing_file_that_appears_and_an_evicted_build_concern_the_entry() {
    let dir = project(&[
        ("main.typ", "= Title\n#include \"chapter.typ\"\n"),
        ("notes.typ", "Notes.\n"),
        ("other.typ", "Other entry.\n"),
    ]);
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let entry = fx.path.clone();
    let notes = open(&fx, "notes.typ");
    let failed = fx.build();
    assert_eq!(failed.result.status, BuildStatus::Failed, "the include is missing");
    // A build with a read that failed is concerned by every open document it
    // did not compile: the missing file may be one of them under a new name
    // (review S2).
    assert!(fx.service.change_concerns(&entry, &notes, 2), "open and unread, but an include was missing");
    // The file is created and opened.
    std::fs::write(fx.root.join("chapter.typ"), "Chapter.\n").unwrap();
    let chapter = open(&fx, "chapter.typ");
    assert!(fx.service.change_concerns(&entry, &chapter, 1), "not known to the failed build");
    let built = fx.build();
    assert_eq!(built.result.status, BuildStatus::Succeeded, "{:?}", built.result.diagnostics);

    // Other builds push the entry's latest build out of the retained ones.
    let other = exposition_core::build::BuildTarget {
        format: exposition_core::build::DocumentFormat::Typst,
        engine: exposition_core::build::Engine::Typst,
        entry: path("other.typ"),
    };
    for _ in 0..12 {
        fx.service.build(&other).unwrap();
    }
    assert!(fx.service.latest(&entry).is_none(), "the entry's latest build is no longer retained");
    assert!(fx.service.change_concerns(&entry, &notes, 2), "nothing is known about an evicted build");
}

/// An open, unread document is renamed onto the path the entry includes (the
/// include was missing). The document keeps its identity, so the service
/// still answers "open at that build and not read": only the file watcher's
/// change event rebuilds the entry.
#[test]
fn a_document_renamed_onto_a_missing_include_concerns_the_entry() {
    let dir = project(&[("main.typ", "= Title\n#include \"chapter.typ\"\n"), ("draft.typ", "Draft.\n")]);
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let entry = fx.path.clone();
    let draft = open(&fx, "draft.typ");
    let failed = fx.build();
    assert_eq!(failed.result.status, BuildStatus::Failed);
    let renamed = fx.workspace.rename_entry(&path("draft.typ"), "chapter.typ").unwrap();
    assert_eq!(renamed.documents.len(), 1);
    assert_eq!(renamed.documents[0].id, draft, "the same document under its new path");
    let revision = append(&fx, &draft, "More.\n");
    let concerns = fx.service.change_concerns(&entry, &draft, revision);
    let rebuilt = fx.build();
    assert_eq!(rebuilt.result.status, BuildStatus::Succeeded, "a build now finds the include");
    assert!(concerns, "the entry's pages change with this document, but its change does not concern the entry");
}
