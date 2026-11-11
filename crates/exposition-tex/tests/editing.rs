//! Direct page editing proof with the real compilers.
//!
//! Each scenario starts from a page point, resolves it to a source offset
//! through the glyph mapping, applies a `DocumentService` transaction with
//! `EditOrigin::Page`, rebuilds the snapshot (unsaved buffer), re-extracts and
//! re-maps the page, and checks that the edit appears at the corresponding
//! page position while all untouched source is byte-identical.

mod common;

use std::sync::Arc;
use std::time::Instant;

use exposition_core::build::{BuildStatus, Engine};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, DocumentId, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{TextRange, utf16_to_byte};
use exposition_tex::latex::{Construct, Mode};
use exposition_tex::mapping::{GlyphTarget, PageMapping};
use exposition_tex::{BuildMode, TexBuild, TexPageSession};

const ENGINES: [Engine; 2] = [Engine::PdfLatex, Engine::LuaLatex];

fn find_word(mapping: &PageMapping, word: &str) -> Vec<u32> {
    let mut text = String::new();
    let mut owners = Vec::new();
    for (i, g) in mapping.glyphs.iter().enumerate() {
        for c in g.text.as_deref().unwrap_or("\u{FFFD}").chars() {
            text.push(c);
            owners.push(i as u32);
        }
    }
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = word.chars().collect();
    let start = chars.windows(needle.len()).position(|w| w == needle.as_slice()).unwrap_or_else(|| panic!("{word}"));
    let mut glyphs: Vec<u32> = owners[start..start + needle.len()].to_vec();
    glyphs.dedup();
    glyphs
}

struct Timing {
    apply_us: u128,
    build_ms: u128,
    map_ms: u128,
    render_ms: u128,
}

impl Timing {
    fn total_ms(&self) -> u128 {
        self.apply_us / 1000 + self.build_ms + self.map_ms + self.render_ms
    }
}

struct Editor {
    h: common::Harness,
    main: DocumentId,
    results: DocumentId,
    timings: Vec<(String, Timing)>,
}

impl Editor {
    fn new(engine: Engine) -> Option<(Self, TexBuild, PageMapping, u128)> {
        let h = common::Harness::new("latex-basic", engine)?;
        let main = h.open("main.tex");
        let results = h.open("sections/results.tex");
        let started = Instant::now();
        let build = h.builder.build(&h.request(BuildMode::Full));
        let session = TexPageSession::open(&build).unwrap();
        let mapping = session.map_page(0).unwrap();
        let _ = session.render_page(0, 2.0).unwrap();
        let cold = started.elapsed().as_millis();
        assert_eq!(build.result.status, BuildStatus::Succeeded);
        Some((Self { h, main, results, timings: Vec::new() }, build, mapping, cold))
    }

    fn doc_for(&self, path: &WorkspacePath) -> DocumentId {
        if path.as_str() == "main.tex" { self.main.clone() } else { self.results.clone() }
    }

    /// Apply a page-origin patch based on the revision the page was built
    /// from, rebuild in draft mode, and map + render the page again.
    fn edit(
        &mut self,
        label: &str,
        build: &TexBuild,
        path: &WorkspacePath,
        patch: TextPatch,
    ) -> (TexBuild, PageMapping) {
        let doc = self.doc_for(path);
        let base = build.result.sources.iter().find(|s| &s.path == path).unwrap().revision.unwrap();
        let started = Instant::now();
        self.h
            .documents
            .apply(TransactionRequest {
                id: TransactionId(label.into()),
                origin: EditOrigin::Page,
                edits: vec![DocumentEdit { document: doc, base_revision: base, patches: vec![patch] }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::RebaseDisjoint,
                label: Some(label.into()),
                selection: None,
            })
            .unwrap();
        let apply_us = started.elapsed().as_micros();
        let started = Instant::now();
        let rebuilt = self.h.builder.build(&self.h.request(BuildMode::Draft));
        let build_ms = started.elapsed().as_millis();
        assert_eq!(rebuilt.result.status, BuildStatus::Succeeded, "{label}: {:?}", rebuilt.result.diagnostics);
        let started = Instant::now();
        let session = TexPageSession::open(&rebuilt).unwrap();
        let mapping = session.map_page(0).unwrap();
        let map_ms = started.elapsed().as_millis();
        let started = Instant::now();
        let _ = session.render_page(0, 2.0).unwrap();
        let render_ms = started.elapsed().as_millis();
        self.timings.push((label.into(), Timing { apply_us, build_ms, map_ms, render_ms }));
        (rebuilt, mapping)
    }
}

fn utf16_slice(text: &str, range: TextRange) -> &str {
    &text[utf16_to_byte(text, range.start as usize).unwrap()..utf16_to_byte(text, range.end as usize).unwrap()]
}

/// `after` equals `before` with exactly `patch` applied (untouched text is
/// byte-identical).
fn assert_only_patch(before: &Arc<str>, after: &Arc<str>, patch: &TextPatch) {
    let s = utf16_to_byte(before, patch.range.start as usize).unwrap();
    let e = utf16_to_byte(before, patch.range.end as usize).unwrap();
    let expected = format!("{}{}{}", &before[..s], patch.text, &before[e..]);
    assert_eq!(after.as_bytes(), expected.as_bytes());
}

/// Glyphs whose mapped source range lies inside `range` of `path`, in page order.
fn glyphs_in(mapping: &PageMapping, path: &WorkspacePath, range: TextRange) -> Vec<u32> {
    mapping
        .targets
        .iter()
        .enumerate()
        .filter_map(|(i, t)| match t {
            GlyphTarget::Source { path: p, range: r, .. }
                if p == path && r.start >= range.start && r.end <= range.end =>
            {
                Some(i as u32)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn page_edits_round_trip_through_the_real_compilers() {
    for engine in ENGINES {
        let Some((mut ed, build, mapping, cold_ms)) = Editor::new(engine) else { continue };
        let main_path = WorkspacePath::parse("main.tex").unwrap();
        let original_main = ed.h.text(&ed.main);
        let original_results = ed.h.text(&ed.results);
        let disk_before = std::fs::read(ed.h.project.path().join("main.tex")).unwrap();

        // 1. Prose insertion: click inside "baseline" between "base" and "line".
        let word = find_word(&mapping, "baseline");
        let e = &mapping.glyphs[word[3] as usize];
        let (_, y) = e.bbox.center();
        let caret = mapping.hit_test(e.origin_x + 0.8 * e.advance, y, 1.0).unwrap();
        assert_eq!(caret.path, main_path);
        let text = ed.h.text(&ed.main);
        let at = utf16_to_byte(&text, caret.offset as usize).unwrap();
        assert!(text[..at].ends_with("base") && text[at..].starts_with("line"), "{engine:?} caret");
        let patch = TextPatch::insert(caret.offset, "QZ");
        let (build1, map1) = ed.edit("insert-prose", &build, &main_path, patch.clone());
        let after = ed.h.text(&ed.main);
        assert_only_patch(&original_main, &after, &patch);
        let inserted = glyphs_in(&map1, &main_path, TextRange::new(caret.offset, caret.offset + 2));
        let texts: Vec<_> = inserted.iter().map(|&g| map1.glyphs[g as usize].text.clone().unwrap()).collect();
        assert_eq!(texts, ["Q", "Z"], "{engine:?}");
        // At the corresponding page position: right after "base", before "line".
        let new_word = find_word(&map1, "baseQZline");
        assert_eq!(&new_word[4..6], inserted.as_slice());
        let q = &map1.glyphs[inserted[0] as usize];
        let old_e = &mapping.glyphs[word[3] as usize];
        let new_e = &map1.glyphs[new_word[3] as usize];
        assert!((q.origin_y - old_e.origin_y).abs() < 0.01, "same baseline as before the edit");
        assert!((q.origin_x - (new_e.origin_x + new_e.advance)).abs() < 0.5, "{engine:?} directly after the e");

        // 2. Equation edit: change the exponent of c^2 in the display to 3.
        let display = find_word(&map1, "E=mc2+");
        let two = display[4];
        let GlyphTarget::Source { range, mode: Mode::Math, construct: Construct::DisplayMath, .. } =
            map1.targets[two as usize].clone()
        else {
            panic!("{engine:?}: exponent not mapped as display math")
        };
        let before = ed.h.text(&ed.main);
        assert_eq!(utf16_slice(&before, range), "2");
        let patch = TextPatch::replace(range.start, range.end, "3");
        let (build2, map2) = ed.edit("exponent", &build1, &main_path, patch.clone());
        assert_only_patch(&before, &ed.h.text(&ed.main), &patch);
        let three = glyphs_in(&map2, &main_path, range);
        assert_eq!(three.len(), 1);
        let g3 = &map2.glyphs[three[0] as usize];
        let c = &map2.glyphs[find_word(&map2, "E=mc3")[3] as usize];
        assert_eq!(g3.text.as_deref(), Some("3"));
        assert!(g3.origin_y < c.origin_y - 2.0, "{engine:?}: still a superscript");
        assert!(g3.size < c.size, "{engine:?}: script size");

        // 3. Insert "+ 1" inside display math after x_i.
        let xi = find_word(&map2, "xi(1)");
        let GlyphTarget::Source { range: i_range, .. } = map2.targets[xi[1] as usize].clone() else { panic!() };
        let before = ed.h.text(&ed.main);
        assert_eq!(utf16_slice(&before, i_range), "i");
        let patch = TextPatch::insert(i_range.end, " + 1");
        let (build3, map3) = ed.edit("display-insert", &build2, &main_path, patch.clone());
        assert_only_patch(&before, &ed.h.text(&ed.main), &patch);
        let added = glyphs_in(&map3, &main_path, TextRange::new(i_range.end, i_range.end + 4));
        let texts: Vec<_> = added.iter().map(|&g| map3.glyphs[g as usize].text.clone().unwrap()).collect();
        assert_eq!(texts, ["+", "1"], "{engine:?}");
        let x = &map3.glyphs[find_word(&map3, "xi+1(1)")[0] as usize];
        let plus = &map3.glyphs[added[0] as usize];
        assert!((plus.origin_y - x.origin_y).abs() < 0.01 && plus.origin_x > x.origin_x, "{engine:?}");
        for &g in &added {
            assert!(matches!(map3.targets[g as usize], GlyphTarget::Source { mode: Mode::Math, .. }));
        }

        // 4. Deletion next to a ligature: caret inside the "ffi" of "affine"
        //    before the "i", then backspace → "afine" typesets an "fi" ligature.
        let affine = find_word(&map3, "affine");
        let ffi = &map3.glyphs[affine[1] as usize];
        assert_eq!(ffi.text.as_deref(), Some("ffi"));
        let (_, y) = ffi.bbox.center();
        let caret = map3.hit_test(ffi.origin_x + 0.8 * ffi.advance, y, 1.0).unwrap();
        let before = ed.h.text(&ed.main);
        let at = utf16_to_byte(&before, caret.offset as usize).unwrap();
        assert!(before[..at].ends_with("aff") && before[at..].starts_with("ine"), "{engine:?} caret in ligature");
        let patch = TextPatch::delete(caret.offset - 1, caret.offset);
        let (_build4, map4) = ed.edit("ligature-delete", &build3, &main_path, patch.clone());
        assert_only_patch(&before, &ed.h.text(&ed.main), &patch);
        let afine = find_word(&map4, "afine,");
        let fi = &map4.glyphs[afine[1] as usize];
        assert_eq!(fi.text.as_deref(), Some("fi"), "{engine:?}: ligature re-formed");
        let GlyphTarget::Source { range, boundaries, .. } = &map4.targets[afine[1] as usize] else { panic!() };
        assert_eq!(utf16_slice(&ed.h.text(&ed.main), *range), "fi");
        assert_eq!(boundaries.len(), 3);

        // Untouched files are byte-identical; nothing was saved to disk.
        assert_eq!(ed.h.text(&ed.results).as_bytes(), original_results.as_bytes());
        assert_eq!(std::fs::read(ed.h.project.path().join("main.tex")).unwrap(), disk_before);
        // Shared undo restores the original buffer exactly.
        for _ in 0..4 {
            ed.h.documents.undo(&ed.main, EditOrigin::Page).unwrap();
        }
        assert_eq!(ed.h.text(&ed.main).as_bytes(), original_main.as_bytes());

        eprintln!("{engine:?}: cold full build + map + render: {cold_ms} ms");
        for (label, t) in &ed.timings {
            eprintln!(
                "{engine:?}: {label}: edit-to-new-page {} ms (apply {} us, draft build {} ms, extract+map {} ms, \
                 render 2x {} ms)",
                t.total_ms(),
                t.apply_us,
                t.build_ms,
                t.map_ms,
                t.render_ms
            );
            assert!(t.total_ms() < 30_000);
        }
    }
}

/// A failed rebuild leaves the previous build's immutable output in place, so
/// the page view can keep showing the last good page.
#[test]
fn last_good_output_survives_a_failed_rebuild() {
    for engine in ENGINES {
        let Some((ed, build, _, _)) = Editor::new(engine) else { continue };
        let good = build.output.clone().unwrap();
        let text = ed.h.text(&ed.main);
        let at = text.find("\\end{document}").unwrap();
        let at16 = exposition_core::text::utf16_len(&text[..at]) as u32;
        ed.h.documents
            .apply(TransactionRequest {
                id: TransactionId("break".into()),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: ed.main.clone(),
                    base_revision: 1,
                    patches: vec![TextPatch::insert(at16, "\\input{does-not-exist}\n")],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        let failed = ed.h.builder.build(&ed.h.request(BuildMode::Draft));
        assert_eq!(failed.result.status, BuildStatus::Failed, "{engine:?}");
        assert!(failed.result.diagnostics.iter().any(|d| d.message.contains("does-not-exist")));
        // The previous output is untouched and still maps.
        assert!(good.pdf.is_file() && good.synctex.as_ref().unwrap().is_file());
        let session = TexPageSession::open(&build).unwrap();
        assert!(session.map_page(0).unwrap().stats.mapped > 600);
    }
}

/// A page computed from an older build cannot apply its offsets blindly:
/// disjoint concurrent edits are rebased, overlapping ones rejected.
#[test]
fn stale_page_edits_are_rebased_or_rejected() {
    use exposition_core::document::TransactionError;
    let Some((ed, build, mapping, _)) = Editor::new(Engine::PdfLatex) else { return };
    let base = build.result.sources.iter().find(|s| s.path.as_str() == "main.tex").unwrap().revision.unwrap();
    let word = find_word(&mapping, "baseline");
    let e = &mapping.glyphs[word[3] as usize];
    let caret = mapping.hit_test(e.origin_x + 0.8 * e.advance, e.bbox.center().1, 1.0).unwrap();
    let request = |id: &str, origin: EditOrigin, base_revision: u64, patch: TextPatch, conflict: ConflictPolicy| {
        TransactionRequest {
            id: TransactionId(id.into()),
            origin,
            edits: vec![DocumentEdit { document: ed.main.clone(), base_revision, patches: vec![patch] }],
            undo: UndoBehavior::NewGroup,
            conflict,
            label: None,
            selection: None,
        }
    };
    // The source editor changes the document after the page was built.
    let header = "% edited in the source editor\n";
    ed.h.documents
        .apply(request("src", EditOrigin::Source, base, TextPatch::insert(0, header), ConflictPolicy::Reject))
        .unwrap();
    // Strict policy: the stale page edit is refused.
    let stale = ed.h.documents.apply(request(
        "page-strict",
        EditOrigin::Page,
        base,
        TextPatch::insert(caret.offset, "QZ"),
        ConflictPolicy::Reject,
    ));
    assert!(matches!(stale, Err(TransactionError::StaleRevision { .. })));
    // Rebase policy: disjoint, so it lands at the shifted position.
    let outcome =
        ed.h.documents
            .apply(request(
                "page",
                EditOrigin::Page,
                base,
                TextPatch::insert(caret.offset, "QZ"),
                ConflictPolicy::RebaseDisjoint,
            ))
            .unwrap();
    assert!(outcome.applied[0].rebased);
    let text = ed.h.text(&ed.main);
    assert!(text.starts_with(header) && text.contains("baseQZline"));
    // A source edit replacing the word, then a page edit inside it from the
    // older page: overlapping, so rejected.
    let now = ed.h.documents.info(&ed.main).unwrap().revision;
    let start = exposition_core::text::utf16_len(&text[..text.find("baseQZline").unwrap()]) as u32;
    ed.h.documents
        .apply(request(
            "src2",
            EditOrigin::Source,
            now,
            TextPatch::replace(start, start + 10, "foundation"),
            ConflictPolicy::Reject,
        ))
        .unwrap();
    let overlapping = ed.h.documents.apply(request(
        "page2",
        EditOrigin::Page,
        now,
        TextPatch::insert(start + 4, "X"),
        ConflictPolicy::RebaseDisjoint,
    ));
    assert!(matches!(overlapping, Err(TransactionError::Conflict { .. })), "{overlapping:?}");
    // The rebuilt page shows the accepted state.
    let rebuilt = ed.h.builder.build(&ed.h.request(BuildMode::Draft));
    let map = TexPageSession::open(&rebuilt).unwrap().map_page(0).unwrap();
    let found = find_word(&map, "foundation");
    assert!(found.iter().all(|&g| map.targets[g as usize].is_mapped()));
}
