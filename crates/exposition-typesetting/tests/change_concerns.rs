//! Which document changes concern an entry's latest build: the decision behind
//! "rebuild only the entries a change can affect". A wrong "no" would leave
//! pages of an older state presented as current, so every case of doubt must
//! answer "yes".
mod common;

use common::Fixture;
use exposition_core::build::BuildStatus;
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::utf16_len;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in [
        ("main.typ", "= Title\n#include \"chapter.typ\"\nBody text.\n"),
        ("chapter.typ", "Chapter text.\n"),
        ("notes.typ", "Unrelated notes.\n"),
        ("later.typ", "Opened later.\n"),
        ("other.typ", "= Another entry\n"),
    ] {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

fn open(fx: &Fixture, path: &str) -> DocumentId {
    fx.workspace.open_document(&WorkspacePath::parse(path).unwrap()).unwrap().state.info.id
}

/// Append `text` to a document as a source edit; returns the new revision.
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

#[test]
fn a_change_concerns_an_entry_only_when_its_latest_build_can_depend_on_it() {
    let dir = project();
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let entry = fx.path.clone();
    let chapter = open(&fx, "chapter.typ");
    let notes = open(&fx, "notes.typ");

    // No build yet: everything concerns the entry.
    assert!(fx.service.change_concerns(&entry, &notes, 2));
    assert!(fx.service.change_concerns(&entry, &fx.document, 1));

    let first = fx.build();
    assert_eq!(first.result.status, BuildStatus::Succeeded, "{:?}", first.result.diagnostics);
    // The entry and the file it includes were compiled at revision 1.
    assert!(!fx.service.change_concerns(&entry, &fx.document, 1), "that revision is built already");
    assert!(fx.service.change_concerns(&entry, &fx.document, 2));
    assert!(!fx.service.change_concerns(&entry, &chapter, 1));
    assert!(fx.service.change_concerns(&entry, &chapter, 2));
    // Open when the build ran and not read by it: no revision of it matters.
    assert!(!fx.service.change_concerns(&entry, &notes, 2));
    assert!(!fx.service.change_concerns(&entry, &notes, 500));
    // An entry that has no retained build is always concerned.
    assert!(fx.service.change_concerns(&WorkspacePath::parse("other.typ").unwrap(), &notes, 2));

    // Leaving the entry alone for an unread document is sound: building after
    // such a change reads the same inputs and gives the same pages.
    append(&fx, &notes, "More notes.\n");
    let same = fx.build();
    assert_eq!(same.result.snapshot, first.result.snapshot);
    let refs = |record: &exposition_typesetting::BuildRecord| -> Vec<String> {
        record.result.pages.iter().map(|page| page.render_ref.clone()).collect()
    };
    assert_eq!(refs(&same), refs(&first));

    // A document opened after the latest build is not known to it.
    let later = open(&fx, "later.typ");
    assert!(fx.service.change_concerns(&entry, &later, 2));
    fx.build();
    assert!(!fx.service.change_concerns(&entry, &later, 2), "known and unread after one build");

    // A change of the included file is compiled by the next build, and only
    // a newer revision concerns the entry again.
    let revision = append(&fx, &chapter, "More chapter text.\n");
    assert!(fx.service.change_concerns(&entry, &chapter, revision));
    let rebuilt = fx.build();
    assert_ne!(rebuilt.result.snapshot, first.result.snapshot);
    assert!(!fx.service.change_concerns(&entry, &chapter, revision));
    assert!(fx.service.change_concerns(&entry, &chapter, revision + 1));

    // The entry starts to include the notes: from that build on they concern it.
    let revision = append(&fx, &fx.document, "#include \"notes.typ\"\n");
    assert!(fx.service.change_concerns(&entry, &fx.document, revision));
    let including = fx.build();
    assert_eq!(including.result.status, BuildStatus::Succeeded, "{:?}", including.result.diagnostics);
    let notes_revision = fx.documents.info(&notes).unwrap().revision;
    assert!(!fx.service.change_concerns(&entry, &notes, notes_revision));
    assert!(fx.service.change_concerns(&entry, &notes, notes_revision + 1));
}

#[test]
fn a_failed_build_is_judged_by_what_it_read_too() {
    let dir = project();
    std::fs::write(dir.path().join("main.typ"), "= Title\n#include \"chapter.typ\"\n#unknown-function()\n").unwrap();
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let entry = fx.path.clone();
    let chapter = open(&fx, "chapter.typ");
    let notes = open(&fx, "notes.typ");
    let failed = fx.build();
    assert_eq!(failed.result.status, BuildStatus::Failed);
    assert!(fx.service.change_concerns(&entry, &fx.document, 2), "fixing the entry rebuilds it");
    assert!(fx.service.change_concerns(&entry, &chapter, 2));
    assert!(!fx.service.change_concerns(&entry, &notes, 2));
}
