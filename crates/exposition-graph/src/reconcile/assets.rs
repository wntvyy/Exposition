//! Image references of a live buffer, for view-only previews in source.
//!
//! The text is read with the file's own extractor (the one that indexes the
//! saved file), so comments, verbatim/code, and non-literal arguments are
//! treated exactly as indexing treats them; written paths become the same
//! candidates the reconciler tries. Nothing is stored, no graph event is
//! emitted, and no overlay is installed: previews are presentation only, and
//! the buffer's references reach the graph through the normal overlay and
//! save/reconcile paths, whether previews are shown or not.

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::extract::{self, LinkTarget, MAX_EXTRACT_BYTES, PathBase};
use super::resolve::path_candidates;
use crate::service::{GraphError, GraphResult, GraphService};
use crate::vocabulary::RelationKind;

/// References kept per text.
pub const MAX_ASSET_REFERENCES: usize = 500;

/// One literal asset reference (`\includegraphics{…}`, `![…](…)`, `<img src>`,
/// Typst `image("…")`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AssetReference {
    /// UTF-16 range of the referencing syntax in the text that was read.
    pub range: TextRange,
    /// The path as written.
    pub written: String,
    /// Project paths to try, in order. Empty when the written path leaves the
    /// project (it is never resolved outside it).
    pub candidates: Vec<WorkspacePath>,
}

/// Asset references in `text`, the current text of the file at `path`.
/// LaTeX paths are also tried from the folders of the project's main files
/// when a project graph is attached (as indexing does).
pub fn asset_references(graph: &GraphService, path: &WorkspacePath, text: &str) -> GraphResult<Vec<AssetReference>> {
    if text.len() > MAX_EXTRACT_BYTES {
        return Err(GraphError::new("graph.tooLarge", "The text is too large to look for image references."));
    }
    let Some(extraction) = extract::extract(path, text) else { return Ok(Vec::new()) };
    let assets: Vec<_> = extraction
        .links
        .into_iter()
        .filter(|l| l.kind == RelationKind::UsesAsset)
        .filter_map(|l| match l.target {
            LinkTarget::Path { written, base, extensions } => Some((l.range, written, base, extensions)),
            _ => None,
        })
        .take(MAX_ASSET_REFERENCES)
        .collect();
    let mains = if assets.iter().any(|(_, _, base, _)| *base == PathBase::LatexMain) {
        graph.latex_mains()
    } else {
        Default::default()
    };
    Ok(assets
        .into_iter()
        .map(|(range, written, base, extensions)| AssetReference {
            candidates: path_candidates(path, &written, base, &extensions, &mains),
            range,
            written,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::GraphConfig;

    fn wp(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    fn refs(path: &str, text: &str) -> Vec<(String, Vec<String>)> {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphService::new(GraphConfig { data_dir: dir.path().join("data") });
        asset_references(&graph, &wp(path), text)
            .unwrap()
            .into_iter()
            .map(|r| (r.written, r.candidates.iter().map(|c| c.as_str().to_owned()).collect()))
            .collect()
    }

    #[test]
    fn latex_skips_comments_and_verbatim() {
        let tex = "\\documentclass{article}\n\\begin{document}\n% \\includegraphics{commented.png}\n\\begin{verbatim}\n\\includegraphics{verb.png}\n\\end{verbatim}\n\\verb|\\includegraphics{inline.png}|\n\\includegraphics[width=2cm]{figs/my plot}\n\\includegraphics{\\figdir/x.png}\n\\end{document}\n";
        let found = refs("paper/main.tex", tex);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "figs/my plot");
        // The file's folder first, then the project root, each with the graphics extensions.
        assert_eq!(found[0].1[0], "paper/figs/my plot.pdf");
        assert!(found[0].1.contains(&"paper/figs/my plot.png".to_owned()));
        assert!(found[0].1.contains(&"figs/my plot.png".to_owned()));
    }

    #[test]
    fn markdown_skips_code_and_remote() {
        let md = "# T\n\n![a](img/a%20b.png)\n\n```\n![code](no.png)\n```\n\n`![span](no2.png)`\n\n<!-- ![c](no3.png) -->\n\n![remote](https://example.com/x.png)\n\n<img src=\"b.svg\">\n";
        let found = refs("docs/notes.md", md);
        let written: Vec<&str> = found.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(written, ["img/a b.png", "b.svg"]);
        assert_eq!(found[0].1, ["docs/img/a b.png"]);
    }

    #[test]
    fn typst_skips_comments_and_raw_and_dynamic() {
        let typ = "// #image(\"c.png\")\n/* #image(\"d.png\") */\n`#image(\"raw.png\")`\n#image(\"figs/a.png\", width: 50%)\n#image(path)\n#figure(image(\"/top.svg\"))\n";
        let found = refs("ch/one.typ", typ);
        let written: Vec<&str> = found.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(written, ["figs/a.png", "/top.svg"]);
        assert_eq!(found[0].1, ["ch/figs/a.png"]);
        assert_eq!(found[1].1, ["top.svg"], "a leading slash is the project root in Typst");
    }

    #[test]
    fn escaping_paths_have_no_candidates() {
        let found = refs("a.md", "![x](../../outside.png)\n");
        assert_eq!(found, [("../../outside.png".to_owned(), Vec::<String>::new())]);
    }

    #[test]
    fn other_files_have_none_and_ranges_are_utf16() {
        assert!(refs("a.py", "x = 'a.png'\n").is_empty());
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphService::new(GraphConfig { data_dir: dir.path().join("data") });
        let text = "é𝔸 ![x](p.png)\n";
        let r = asset_references(&graph, &wp("a.md"), text).unwrap();
        let start = text.encode_utf16().count() as u32 - "![x](p.png)\n".len() as u32;
        assert_eq!(r[0].range.start, start);
        assert_eq!(r[0].range.end, start + "![x](p.png)".len() as u32);
    }
}
