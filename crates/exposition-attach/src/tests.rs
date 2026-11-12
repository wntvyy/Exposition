use std::path::Path;

use exposition_core::artifact::{ArtifactId, RunId};
use exposition_core::document::{EditOrigin, TextEncoding};
use exposition_core::text::utf16_range_to_bytes;
use exposition_workspace::OpenDocument;

use super::*;

fn artifact(kind: ArtifactKind, path: &str, media_type: &str, hash: &str) -> ArtifactRecord {
    ArtifactRecord {
        id: ArtifactId(format!("run-1/{path}")),
        kind,
        path: Some(WorkspacePath::parse(path).unwrap()),
        scratch_path: None,
        media_type: media_type.into(),
        size_bytes: 10,
        content_hash: hash.into(),
    }
}

fn request(
    placement: AttachmentPlacement,
    revision: u64,
    caption: Option<&str>,
    kind: ArtifactKind,
) -> AttachArtifactRequest {
    AttachArtifactRequest {
        document: DocumentId("doc-1".into()),
        base_revision: revision,
        artifact: artifact(
            kind,
            if kind == ArtifactKind::Table {
                ".exposition/artifacts/run-1/output-2.csv"
            } else {
                ".exposition/artifacts/run-1/output-1.png"
            },
            if kind == ArtifactKind::Table { "text/csv" } else { "image/png" },
            "sha256:3fa9c2d1aa",
        ),
        run: Some(RunId("run-1".into())),
        placement,
        caption: caption.map(str::to_owned),
        attachment_key: None,
        origin: EditOrigin::Research { run_id: "run-1".into() },
    }
}

fn figure() -> AttachContent {
    AttachContent::Figure { path: "../.exposition/artifacts/run-1/output-1.png".into(), media_type: "image/png".into() }
}

fn table() -> AttachContent {
    AttachContent::Table(TableGrid::from_csv(",n,sq\n0,1,1\n1,2,4\n").unwrap())
}

/// Apply a plan's single edit to `text` (the transaction is relative to `text`).
fn apply(text: &str, plan: &AttachPlan) -> String {
    let mut out = text.to_owned();
    for patch in plan.transaction.edits[0].patches.iter().rev() {
        let range = utf16_range_to_bytes(&out, patch.range).unwrap();
        out.replace_range(range, &patch.text);
    }
    out
}

fn plan_for(
    format: Format,
    path: &str,
    text: &str,
    req: &AttachArtifactRequest,
    content: &AttachContent,
) -> Result<AttachPlan, AttachError> {
    let document = DocumentId("doc-1".into());
    let path = WorkspacePath::parse(path).unwrap();
    plan(&PlanInput { format, document: &document, document_path: &path, revision: 1, text }, req, content)
}

#[test]
fn typst_figure_insert_and_update() {
    let text = "= Results\nThe curve:\nMore text.\n";
    let at = "= Results\nThe curve:\n".len() as u32;
    let insert =
        request(AttachmentPlacement::Insert { offset: at }, 1, Some("Squares of $n$ [grid]"), ArtifactKind::Figure);
    let plan = plan_for(Format::Typst, "paper/main.typ", text, &insert, &figure()).unwrap();
    assert_eq!(plan.attachment_key, "fig-3fa9c2d1");
    let inserted = apply(text, &plan);
    assert_eq!(
        inserted,
        "= Results\nThe curve:\n// exposition:begin fig-3fa9c2d1\n#figure(\n  image(\"../.exposition/artifacts/run-1/output-1.png\", width: 80%),\n  caption: [Squares of \\$n\\$ \\[grid\\]],\n) <fig-3fa9c2d1>\n// exposition:end fig-3fa9c2d1\nMore text.\n"
    );
    assert_eq!(plan.transaction.origin, EditOrigin::Research { run_id: "run-1".into() });
    assert_eq!(plan.transaction.conflict, ConflictPolicy::RebaseDisjoint);

    // The user edits around the block and inside its caption.
    let edited = inserted
        .replace("= Results", "= Results (revised)")
        .replace("More text.", "More text, edited.")
        .replace("Squares of \\$n\\$ \\[grid\\]", "My *own* caption [see @eq]");
    let update =
        request(AttachmentPlacement::Update { attachment_key: "fig-3fa9c2d1".into() }, 7, None, ArtifactKind::Figure);
    let new_figure = AttachContent::Figure {
        path: "../.exposition/artifacts/run-2/output-1.png".into(),
        media_type: "image/png".into(),
    };
    let plan = plan_for(Format::Typst, "paper/main.typ", &edited, &update, &new_figure).unwrap();
    let updated = apply(&edited, &plan);
    assert_eq!(
        updated,
        edited.replace("run-1/output-1.png", "run-2/output-1.png"),
        "only the block changes; caption kept"
    );
}

#[test]
fn typst_table_insert() {
    let text = "Intro.\n";
    let insert = request(AttachmentPlacement::Insert { offset: 7 }, 1, Some("Squares"), ArtifactKind::Table);
    let plan = plan_for(Format::Typst, "main.typ", text, &insert, &table()).unwrap();
    assert_eq!(
        apply(text, &plan),
        "Intro.\n// exposition:begin tab-3fa9c2d1\n#figure(\n  table(\n    columns: 3,\n    align: (right, right, right),\n    table.header(\"\", \"n\", \"sq\"),\n    \"0\", \"1\", \"1\",\n    \"1\", \"2\", \"4\",\n  ),\n  caption: [Squares],\n) <tab-3fa9c2d1>\n// exposition:end tab-3fa9c2d1\n"
    );
}

#[test]
fn latex_figure_and_table_with_packages() {
    let text = "\\documentclass{article}\n\\usepackage{amsmath} % graphicx is not loaded\n\\begin{document}\nHello.\n\\end{document}\n";
    let at = text.find("\\end{document}").unwrap() as u32;
    let insert =
        request(AttachmentPlacement::Insert { offset: at }, 1, Some("50% of $x_1$ & more"), ArtifactKind::Figure);
    let plan = plan_for(Format::Latex, "main.tex", text, &insert, &figure()).unwrap();
    let inserted = apply(text, &plan);
    assert_eq!(
        inserted,
        "\\documentclass{article}\n\\usepackage{amsmath} % graphicx is not loaded\n\\usepackage{graphicx}\n\\begin{document}\nHello.\n% exposition:begin fig-3fa9c2d1\n\\begin{figure}[htbp]\n  \\centering\n  \\includegraphics[width=0.8\\linewidth]{../.exposition/artifacts/run-1/output-1.png}\n  \\caption{50\\% of \\$x\\_1\\$ \\& more}\n  \\label{fig-3fa9c2d1}\n\\end{figure}\n% exposition:end fig-3fa9c2d1\n\\end{document}\n"
    );
    let at = inserted.find("\\end{document}").unwrap() as u32;
    let insert = request(AttachmentPlacement::Insert { offset: at }, 1, None, ArtifactKind::Table);
    let plan = plan_for(Format::Latex, "main.tex", &inserted, &insert, &table()).unwrap();
    let with_table = apply(&inserted, &plan);
    assert!(with_table.contains("\\usepackage{booktabs}\n\\begin{document}"));
    assert!(with_table.contains(
        "% exposition:begin tab-3fa9c2d1\n\\begin{table}[htbp]\n  \\centering\n  \\label{tab-3fa9c2d1}\n  \\begin{tabular}{rrr}\n    \\toprule\n     & n & sq \\\\\n    \\midrule\n    0 & 1 & 1 \\\\\n    1 & 2 & 4 \\\\\n    \\bottomrule\n  \\end{tabular}\n\\end{table}\n% exposition:end tab-3fa9c2d1\n"
    ));
    // Already-loaded packages are not added again (option lists and package lists both count).
    let loaded = "\\usepackage[final]{graphicx,booktabs}\n\\begin{document}\n\\end{document}\n";
    let at = loaded.find("\\end{document}").unwrap() as u32;
    let plan = plan_for(
        Format::Latex,
        "main.tex",
        loaded,
        &request(AttachmentPlacement::Insert { offset: at }, 1, None, ArtifactKind::Figure),
        &figure(),
    )
    .unwrap();
    assert_eq!(plan.transaction.edits[0].patches.len(), 1);
}

#[test]
fn latex_refuses_preamble_and_svg() {
    let text = "\\documentclass{article}\n\\begin{document}\n\\end{document}\n";
    let insert = request(AttachmentPlacement::Insert { offset: 3 }, 1, None, ArtifactKind::Figure);
    assert!(matches!(
        plan_for(Format::Latex, "main.tex", text, &insert, &figure()),
        Err(AttachError::InvalidPlacement(_))
    ));
    let after = request(AttachmentPlacement::Insert { offset: text.len() as u32 }, 1, None, ArtifactKind::Figure);
    assert!(matches!(
        plan_for(Format::Latex, "main.tex", text, &after, &figure()),
        Err(AttachError::InvalidPlacement(_))
    ));
}

#[test]
fn markdown_figure_and_table() {
    let text = "# Notes\n\nSee below.";
    let insert = request(
        AttachmentPlacement::Insert { offset: text.len() as u32 },
        1,
        Some("Squares | *bold*"),
        ArtifactKind::Figure,
    );
    let plan = plan_for(Format::Markdown, "notes/notes.md", text, &insert, &figure()).unwrap();
    let inserted = apply(text, &plan);
    assert_eq!(
        inserted,
        "# Notes\n\nSee below.\n<!-- exposition:begin fig-3fa9c2d1 -->\n![Squares \\| \\*bold\\*](../.exposition/artifacts/run-1/output-1.png)\n<!-- exposition:end fig-3fa9c2d1 -->\n"
    );
    let insert =
        request(AttachmentPlacement::Insert { offset: inserted.len() as u32 }, 1, Some("Values"), ArtifactKind::Table);
    let plan = plan_for(Format::Markdown, "notes/notes.md", &inserted, &insert, &table()).unwrap();
    let with_table = apply(&inserted, &plan);
    assert!(with_table.ends_with(
        "<!-- exposition:begin tab-3fa9c2d1 -->\n|  | n | sq |\n| ---: | ---: | ---: |\n| 0 | 1 | 1 |\n| 1 | 2 | 4 |\n\nTable: Values\n<!-- exposition:end tab-3fa9c2d1 -->\n"
    ));
    // Update the table after edits elsewhere, keeping the edited caption.
    let edited = with_table.replace("# Notes", "# Notes v2").replace("Table: Values", "Table: Values (user)");
    let update =
        request(AttachmentPlacement::Update { attachment_key: "tab-3fa9c2d1".into() }, 3, None, ArtifactKind::Table);
    let bigger = AttachContent::Table(TableGrid::from_csv(",n,sq\n0,1,1\n1,2,4\n2,3,9\n").unwrap());
    let plan = plan_for(Format::Markdown, "notes/notes.md", &edited, &update, &bigger).unwrap();
    let updated = apply(&edited, &plan);
    assert_eq!(updated, edited.replace("| 1 | 2 | 4 |\n", "| 1 | 2 | 4 |\n| 2 | 3 | 9 |\n"));
}

#[test]
fn update_refuses_missing_or_ambiguous_markers() {
    let update = request(AttachmentPlacement::Update { attachment_key: "fig-1".into() }, 1, None, ArtifactKind::Figure);
    let missing = plan_for(Format::Typst, "main.typ", "no markers here\n", &update, &figure());
    assert_eq!(missing.unwrap_err(), AttachError::MissingMarkers("fig-1".into()));
    let duplicated = "// exposition:begin fig-1\na\n// exposition:end fig-1\n// exposition:begin fig-1\nb\n// exposition:end fig-1\n";
    assert!(matches!(
        plan_for(Format::Typst, "main.typ", duplicated, &update, &figure()),
        Err(AttachError::AmbiguousMarkers { begin: 2, end: 2, .. })
    ));
    let end_only = "// exposition:end fig-1\n";
    assert!(matches!(
        plan_for(Format::Typst, "main.typ", end_only, &update, &figure()),
        Err(AttachError::AmbiguousMarkers { .. })
    ));
    let reversed = "// exposition:end fig-1\n// exposition:begin fig-1\n";
    assert!(matches!(
        plan_for(Format::Typst, "main.typ", reversed, &update, &figure()),
        Err(AttachError::AmbiguousMarkers { .. })
    ));
    // Markers of another format do not count.
    let latex_markers = "% exposition:begin fig-1\n% exposition:end fig-1\n";
    assert!(matches!(
        plan_for(Format::Typst, "main.typ", latex_markers, &update, &figure()),
        Err(AttachError::MissingMarkers(_))
    ));
}

#[test]
fn insert_refusals() {
    let text = "a\n// exposition:begin fig-x\n#figure(image(\"x.png\")) <fig-x>\n// exposition:end fig-x\nb\n";
    let stale = request(AttachmentPlacement::Insert { offset: 0 }, 9, None, ArtifactKind::Figure);
    assert_eq!(
        plan_for(Format::Typst, "main.typ", text, &stale, &figure()).unwrap_err(),
        AttachError::Stale { base: 9, current: 1 }
    );
    let inside = request(
        AttachmentPlacement::Insert { offset: text.find("#figure").unwrap() as u32 },
        1,
        None,
        ArtifactKind::Figure,
    );
    assert!(matches!(
        plan_for(Format::Typst, "main.typ", text, &inside, &figure()),
        Err(AttachError::InvalidPlacement(_))
    ));
    let mut duplicate = request(AttachmentPlacement::Insert { offset: 0 }, 1, None, ArtifactKind::Figure);
    duplicate.attachment_key = Some("fig-x".into());
    assert_eq!(
        plan_for(Format::Typst, "main.typ", text, &duplicate, &figure()).unwrap_err(),
        AttachError::DuplicateKey("fig-x".into())
    );
    for bad in ["1abc", "a b", "a--b", "", "x}"] {
        let mut req = request(AttachmentPlacement::Insert { offset: 0 }, 1, None, ArtifactKind::Figure);
        req.attachment_key = Some(bad.into());
        assert!(
            matches!(plan_for(Format::Typst, "main.typ", text, &req, &figure()), Err(AttachError::InvalidKey(_))),
            "{bad}"
        );
    }
    let mid_char = request(AttachmentPlacement::Insert { offset: 1 }, 1, None, ArtifactKind::Figure);
    assert!(matches!(
        plan_for(Format::Typst, "main.typ", "😀", &mid_char, &figure()),
        Err(AttachError::InvalidPlacement(_))
    ));
}

#[test]
fn derived_keys_avoid_collisions_and_crlf_is_preserved() {
    let text = "x\r\n// exposition:begin fig-3fa9c2d1\r\n// exposition:end fig-3fa9c2d1\r\n";
    let insert = request(AttachmentPlacement::Insert { offset: 3 }, 1, None, ArtifactKind::Figure);
    let plan = plan_for(Format::Typst, "main.typ", text, &insert, &figure()).unwrap();
    assert_eq!(plan.attachment_key, "fig-3fa9c2d1-2");
    let block = &plan.transaction.edits[0].patches[0].text;
    assert!(block.contains("\r\n") && !block.replace("\r\n", "").contains('\n'));
}

#[test]
fn relative_paths_from_document_directory() {
    let artifact = WorkspacePath::parse(".exposition/artifacts/run-1/output-1.png").unwrap();
    assert_eq!(
        relative_path(&WorkspacePath::parse("main.typ").unwrap(), &artifact),
        ".exposition/artifacts/run-1/output-1.png"
    );
    assert_eq!(
        relative_path(&WorkspacePath::parse("paper/sec/a.tex").unwrap(), &artifact),
        "../../.exposition/artifacts/run-1/output-1.png"
    );
    let sibling = WorkspacePath::parse("paper/figs/f.png").unwrap();
    assert_eq!(relative_path(&WorkspacePath::parse("paper/main.md").unwrap(), &sibling), "figs/f.png");
}

#[test]
fn attach_through_document_service() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let run_dir = root.join(".exposition").join("artifacts").join("run-1");
    std::fs::create_dir_all(&run_dir).unwrap();
    let png = b"\x89PNG\r\n\x1a\nfake";
    std::fs::write(run_dir.join("output-1.png"), png).unwrap();
    let hash = format!("sha256:{}", hex(&Sha256::digest(png)));
    let documents = DocumentService::new();
    let info = documents.open(OpenDocument {
        path: Some(WorkspacePath::parse("main.md").unwrap()),
        text: "Intro\n\nOutro\n".into(),
        language: "markdown".into(),
        encoding: TextEncoding::Utf8,
        read_only: None,
    });
    let mut req =
        request(AttachmentPlacement::Insert { offset: 7 }, info.revision, Some("A figure"), ArtifactKind::Figure);
    req.document = info.id.clone();
    req.artifact.content_hash = hash.clone();
    let (outcome, warnings) = attach_artifact(&documents, root, &req).unwrap();
    assert!(warnings.is_empty());
    let state = documents.state(&info.id).unwrap();
    assert_eq!(outcome.revision, state.info.revision);
    let block = &state.text[utf16_range_to_bytes(&state.text, outcome.range).unwrap()];
    assert!(block.starts_with("<!-- exposition:begin fig-") && block.ends_with(" -->"), "{block}");
    assert!(state.text.starts_with("Intro\n\n<!-- exposition:begin") && state.text.ends_with("-->\nOutro\n"));
    // The insert is one undoable step.
    documents.undo(&info.id, EditOrigin::Source).unwrap();
    assert_eq!(documents.state(&info.id).unwrap().text, "Intro\n\nOutro\n");
    documents.redo(&info.id, EditOrigin::Source).unwrap();

    // A modified artifact is refused.
    std::fs::write(run_dir.join("output-1.png"), b"tampered").unwrap();
    let mut update = request(
        AttachmentPlacement::Update { attachment_key: outcome.attachment_key.clone() },
        0,
        None,
        ArtifactKind::Figure,
    );
    update.document = info.id.clone();
    update.artifact.content_hash = hash;
    assert!(matches!(attach_artifact(&documents, root, &update), Err(AttachError::ArtifactChanged { .. })));
    // An artifact path outside the workspace cannot be expressed (WorkspacePath), and a missing file is refused.
    update.artifact.path = Some(WorkspacePath::parse(".exposition/artifacts/run-1/output-9.png").unwrap());
    assert!(matches!(attach_artifact(&documents, root, &update), Err(AttachError::Artifact(_))));
    let _ = Path::new("unused");
}
