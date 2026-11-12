//! Builds read through the workspace snapshot: unopened files come from disk,
//! open buffers (unsaved) win, and paths escaping the folder are refused.
mod common;

use common::Fixture;
use exposition_core::build::{BuildStatus, Severity};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    dir
}

#[test]
fn included_files_come_from_disk_and_open_buffers_win() {
    let dir = project(&[
        ("main.typ", "= Main\n\nBefore.\n\n#include \"chapters/one.typ\"\n\nAfter.\n"),
        ("chapters/one.typ", "Chapter text on disk.\n"),
    ]);
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let record = fx.build();
    assert_eq!(record.result.status, BuildStatus::Succeeded, "{:?}", record.result.diagnostics);
    let paths: Vec<&str> = record.result.sources.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["chapters/one.typ", "main.typ"]);
    let chapter = &record.result.sources[0];
    assert!(chapter.document.is_none() && chapter.revision.is_none(), "read from disk");
    let map = record.map.as_ref().unwrap();
    assert!(map.files.iter().any(|f| f.as_str() == "chapters/one.typ"), "glyphs map into the included file");

    // Opening the included file and editing it (unsaved) changes the build.
    let one = WorkspacePath::parse("chapters/one.typ").unwrap();
    let opened = fx.workspace.open_document(&one).unwrap();
    fx.documents
        .apply(TransactionRequest {
            id: TransactionId("t".into()),
            origin: EditOrigin::Source,
            edits: vec![DocumentEdit {
                document: opened.state.info.id.clone(),
                base_revision: opened.state.info.revision,
                patches: vec![TextPatch::insert(0, "Edited. ")],
            }],
            undo: UndoBehavior::NewGroup,
            conflict: ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    let rebuilt = fx.build();
    let chapter = rebuilt.result.sources.iter().find(|s| s.path == one).unwrap();
    assert_eq!(chapter.document.as_ref(), Some(&opened.state.info.id), "the open buffer is compiled");
    let line: String = rebuilt.map.as_ref().unwrap().pages[0]
        .lines
        .iter()
        .flat_map(|l| l.clusters.iter().map(|c| c.text.as_str()))
        .collect();
    assert!(line.contains("Edited. Chapter text on disk."), "{line}");
    assert!(
        std::fs::read_to_string(fx.root.join("chapters/one.typ")).unwrap().starts_with("Chapter"),
        "disk untouched"
    );
}

#[cfg(windows)]
#[test]
fn links_escaping_the_folder_are_refused() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.typ"), "secret").unwrap();
    let dir = project(&[("main.typ", "#include \"escape/secret.typ\"\n")]);
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(fx.root.join("escape"))
        .arg(outside.path())
        .output()
        .unwrap();
    if !status.status.success() {
        eprintln!("mklink /J unavailable; skipping");
        return;
    }
    let record = fx.build();
    assert_eq!(record.result.status, BuildStatus::Failed);
    let error = record.result.diagnostics.iter().find(|d| d.severity == Severity::Error).unwrap();
    assert!(!error.message.contains("secret"), "{error:?}");
    assert!(record.result.sources.iter().all(|s| s.path.as_str() != "escape/secret.typ"));
}
