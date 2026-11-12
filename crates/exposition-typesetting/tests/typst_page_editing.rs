//! P1 evidence: real Typst compiles of `tests/fixtures/typst-basic` through the
//! document service; glyph maps; hit testing in prose and display math; page
//! edits (prose insertion, math token edits, grapheme and escape deletion)
//! that rebuild and remap; reflow; PDF export matching the rendered pages.
mod common;

use common::Fixture;
use exposition_core::build::{BuildStatus, MappingPrecision, Severity};
use exposition_core::text::TextRange;
use exposition_page::map::{Affinity, ClusterKind, EditMode, NonEditableReason};
use exposition_page::mapping::map_offset;
use exposition_page::{Motion, PageEditOp, PageEditOutcome};
use typst::layout::{Frame, FrameItem, Transform};

/// A glyph found by walking the laid-out frames directly (independent of the
/// page map): page coordinates of its advance box and baseline.
#[derive(Debug, Clone)]
struct FrameGlyph {
    text: String,
    x0: f64,
    x1: f64,
    baseline: f64,
}

fn frame_glyphs(frame: &Frame, ts: Transform, out: &mut Vec<FrameGlyph>) {
    for (pos, item) in frame.items() {
        let local = ts.pre_concat(Transform::translate(pos.x, pos.y));
        match item {
            FrameItem::Group(g) => frame_glyphs(&g.frame, local.pre_concat(g.transform), out),
            FrameItem::Text(t) => {
                let mut x = 0.0;
                for glyph in &t.glyphs {
                    let adv = glyph.x_advance.at(t.size).to_pt();
                    let p0 = typst::layout::Point::new(typst::layout::Abs::pt(x), -glyph.y_offset.at(t.size))
                        .transform(local);
                    let p1 = typst::layout::Point::new(typst::layout::Abs::pt(x + adv), -glyph.y_offset.at(t.size))
                        .transform(local);
                    out.push(FrameGlyph {
                        text: t.text[glyph.range()].to_string(),
                        x0: p0.x.to_pt(),
                        x1: p1.x.to_pt(),
                        baseline: p0.y.to_pt(),
                    });
                    x += adv;
                }
            }
            _ => {}
        }
    }
}

fn glyphs_of(record: &exposition_typesetting::BuildRecord, page: usize) -> Vec<FrameGlyph> {
    let mut out = Vec::new();
    let doc = record.document.as_ref().expect("document");
    frame_glyphs(&doc.pages()[page].frame, Transform::identity(), &mut out);
    out
}

/// Find the glyph sequence spelling `word` (one glyph per char) in reading order.
fn find_word(glyphs: &[FrameGlyph], word: &str) -> Vec<FrameGlyph> {
    let chars: Vec<String> = word.chars().map(String::from).collect();
    for start in 0..glyphs.len() {
        if glyphs[start..].iter().take(chars.len()).map(|g| g.text.clone()).collect::<Vec<_>>() == chars {
            return glyphs[start..start + chars.len()].to_vec();
        }
    }
    panic!("word {word:?} not found in frames");
}

#[test]
fn builds_with_glyph_map_and_snapshot_identity() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let record = fx.build();
    assert_eq!(record.result.status, BuildStatus::Succeeded, "{:?}", record.result.diagnostics);
    assert!(record.result.diagnostics.iter().all(|d| d.severity != Severity::Error));
    assert_eq!(record.result.mapping, MappingPrecision::Glyph);
    assert_eq!(record.result.pages.len(), 1);
    let source = &record.result.sources[0];
    assert_eq!(source.path.as_str(), "main.typ");
    assert_eq!(source.document.as_ref(), Some(&fx.document));
    assert_eq!(source.revision, Some(1));

    // Identical inputs give the same snapshot id; an edit changes it.
    let again = fx.build();
    assert_eq!(again.result.snapshot, record.result.snapshot);
    assert_eq!(again.result.pages[0].render_ref, record.result.pages[0].render_ref);
    fx.type_text(fx.offset_of("plain prose", 0), "x");
    assert_ne!(fx.build().result.snapshot, record.result.snapshot);

    let map = record.map.as_ref().unwrap();
    let (editable, other) = map.cluster_counts();
    assert!(editable > 300, "editable clusters: {editable}");
    let lines: Vec<String> =
        map.pages[0].lines.iter().map(|l| l.clusters.iter().map(|c| c.text.as_str()).collect()).collect();
    assert!(lines[0].starts_with("Exposition Basics"));
    assert!(lines.iter().any(|l| l.contains("A gray rectangle standing in for a figure.")));
    assert!(lines.iter().any(|l| l.contains("A footnote with its own text.")));
    // Generated content is present but not editable.
    let generated: Vec<&str> = map.pages[0]
        .lines
        .iter()
        .flat_map(|l| &l.clusters)
        .filter(|c| c.kind == ClusterKind::NonEditable(NonEditableReason::Generated))
        .map(|c| c.text.as_str())
        .collect();
    assert!(generated.iter().any(|t| t.starts_with("Figure")), "{generated:?}");
    assert!(generated.contains(&"1"), "footnote markers: {generated:?}");
    assert!(other >= 3);
}

#[test]
fn hit_test_in_paragraph_word_maps_to_source_offset() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let record = fx.build();
    let map = record.map.as_ref().unwrap();
    let glyphs = glyphs_of(&record, 0);
    let word = find_word(&glyphs, "explains");
    let base = fx.offset_of("explains", 0);
    for (k, g) in word.iter().enumerate() {
        let w = g.x1 - g.x0;
        let left = map.hit_test(0, g.x0 + 0.25 * w, g.baseline - 3.0);
        assert_eq!(left.caret.as_ref().unwrap().offset, base + k as u32, "left half of {:?}", g.text);
        assert_eq!(left.mode, Some(EditMode::Markup));
        let right = map.hit_test(0, g.x0 + 0.75 * w, g.baseline - 3.0);
        assert_eq!(right.caret.as_ref().unwrap().offset, base + k as u32 + 1, "right half of {:?}", g.text);
    }
}

#[test]
fn hit_test_in_display_equation_maps_to_math_source() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let record = fx.build();
    let map = record.map.as_ref().unwrap();
    let glyphs = glyphs_of(&record, 0);
    // The display equation `sum_(k=1)^n k = (n (n + 1)) / 2` has three `n`s:
    // the sum's limit, then two in the numerator (left to right).
    let mut ns: Vec<FrameGlyph> = glyphs.iter().filter(|g| g.text == "𝑛").cloned().collect();
    ns.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    assert_eq!(ns.len(), 3, "{ns:?}");
    let expected = [fx.offset_of("^n k", 1), fx.offset_of("(n (n", 1), fx.offset_of("(n + 1)", 1)];
    for (g, start) in ns.iter().zip(expected) {
        let w = g.x1 - g.x0;
        let hit = map.hit_test(0, g.x0 + 0.3 * w, g.baseline - 2.0);
        let caret = hit.caret.unwrap();
        assert_eq!(hit.mode, Some(EditMode::Math));
        assert_eq!(caret.offset, start, "n at x {:.1}", g.x0);
        let hit = map.hit_test(0, g.x0 + 0.7 * w, g.baseline - 2.0);
        assert_eq!(hit.caret.unwrap().offset, start + 1);
        // The caret geometry outlines the display equation.
        let geometry = map.caret_geometry(&fx.path, start, Affinity::Downstream).unwrap();
        assert!(geometry.exact);
        assert!(!geometry.equation.is_empty());
    }
    // The 2 in the denominator.
    let two = glyphs.iter().filter(|g| g.text == "2").max_by(|a, b| a.baseline.total_cmp(&b.baseline)).unwrap();
    let hit = map.hit_test(0, two.x0 + 0.3 * (two.x1 - two.x0), two.baseline - 2.0);
    assert_eq!(hit.caret.unwrap().offset, fx.offset_of("/ 2 $", 2));
}

#[test]
fn insert_into_paragraph_rebuilds_with_text_at_caret() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let before = fx.build();
    let at = fx.offset_of("long enough", 0);
    let caret = fx.type_text(at, "very ");
    assert_eq!(caret, at + 5);
    assert!(fx.text().contains("It is very long enough to wrap"));
    let after = fx.build();
    assert_eq!(after.result.status, BuildStatus::Succeeded);
    assert_eq!(after.revision_of(&fx.document), Some(fx.revision()));
    let map = after.map.as_ref().unwrap();
    // The inserted text is rendered exactly where the source has it.
    let selection = map.selection(&fx.path, at, at + 5);
    assert_eq!(selection.text, "very ");
    let caret_geometry = map.caret_geometry(&fx.path, caret, Affinity::Downstream).unwrap();
    assert!(caret_geometry.exact);
    let start_geometry = map.caret_geometry(&fx.path, at, Affinity::Downstream).unwrap();
    assert!(caret_geometry.x > start_geometry.x || caret_geometry.top > start_geometry.top);
    // Typing merged into one undo step; undo restores the original text.
    let original = before.text_of(&fx.document).unwrap().to_string();
    fx.documents.undo(&fx.document, exposition_core::document::EditOrigin::Page).unwrap();
    assert_eq!(fx.text(), original);
}

#[test]
fn math_token_insert_and_modify() {
    let fx = Fixture::open("typst-basic", "main.typ");
    fx.build();
    // Insert a token after `c` in the inline equation: `c d^2` (implicit product).
    let after_c = fx.offset_of("c^2$", 1);
    let caret = fx.type_text(after_c, "d");
    assert!(fx.text().contains("$a^2 + b^2 = c d^2$"), "{}", fx.text());
    assert_eq!(caret, after_c + 2);
    // Replace the exponent of `b` (select `2`, type `3`).
    let two = fx.offset_of("b^2", 2);
    let out = fx.edit(TextRange::new(two, two + 1), PageEditOp::Insert { text: "3".into() }, 2);
    assert!(matches!(out, PageEditOutcome::Applied { mode: EditMode::Math, .. }), "{out:?}");
    assert!(fx.text().contains("$a^2 + b^3 = c d^2$"));
    let record = fx.build();
    assert_eq!(record.result.status, BuildStatus::Succeeded, "{:?}", record.result.diagnostics);
    let map = record.map.as_ref().unwrap();
    let three = fx.offset_of("b^3", 2);
    let g = map.caret_geometry(&fx.path, three, Affinity::Downstream).unwrap();
    assert!(g.exact && g.mode == EditMode::Math);
    assert_eq!(map.selection(&fx.path, three, three + 1).text, "3");
    let d = fx.offset_of("c d^2", 2);
    assert_eq!(map.selection(&fx.path, d, d + 1).text, "𝑑");
}

#[test]
fn delete_graphemes_and_escapes() {
    let fx = Fixture::open("typst-basic", "main.typ");
    fx.build();
    // Backspace after the rendered `#` removes the whole `\#` escape.
    let after_hash = fx.offset_of("\\#tag", 2);
    let out = fx.edit(TextRange::empty(after_hash), PageEditOp::DeleteBackward, 1);
    assert!(matches!(out, PageEditOutcome::Applied { caret, .. } if caret == after_hash - 2), "{out:?}");
    assert!(fx.text().contains("with the tag marker."));
    // Delete forward a plain grapheme.
    let at = fx.offset_of("tag marker", 0);
    fx.edit(TextRange::empty(at), PageEditOp::DeleteForward, 2);
    assert!(fx.text().contains("with the ag marker."));
    let record = fx.build();
    let map = record.map.as_ref().unwrap();
    let line: String = map.pages[0]
        .lines
        .iter()
        .map(|l| l.clusters.iter().map(|c| c.text.as_str()).collect::<String>())
        .find(|l| l.contains("marker"))
        .unwrap();
    assert!(line.contains("ag marker.") && !line.contains('#'), "{line}");
}

#[test]
fn ligature_gets_proportional_caret_stops() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let record = fx.build();
    let map = record.map.as_ref().unwrap();
    // "Offices" is set with an `ffi` ligature in Libertinus Serif.
    let start = fx.offset_of("Offices", 0);
    let cluster = map.pages[0]
        .lines
        .iter()
        .flat_map(|l| &l.clusters)
        .find(|c| c.source.is_some_and(|s| s.range.start == start + 1))
        .expect("cluster after O");
    assert_eq!(cluster.text, "ffi", "expected a ligature glyph for ffi");
    assert_eq!(cluster.stops.len(), 4);
    assert!(cluster.stops.windows(2).all(|w| w[0].x < w[1].x && w[0].offset + 1 == w[1].offset));
    // Arrows step through the ligature one character at a time.
    for offset in start + 1..start + 4 {
        let next = map.navigate(&fx.path, offset, Affinity::Downstream, Motion::Right, None).unwrap();
        assert_eq!(next.caret.offset, offset + 1);
    }
}

#[test]
fn caret_mapping_survives_reflow() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let before = fx.build();
    let map = before.map.as_ref().unwrap();
    let target = fx.offset_of("Offices", 0);
    let old = map.caret_geometry(&fx.path, target, Affinity::Downstream).unwrap();
    let figure_before = map.selection(&fx.path, fx.offset_of("A gray", 0), fx.offset_of("A gray", 6)).rects[0];
    let footnote_before =
        map.selection(&fx.path, fx.offset_of("A footnote", 0), fx.offset_of("A footnote", 10)).rects[0];

    // Insert a long phrase at the start of the first paragraph.
    let at = fx.offset_of("The first paragraph", 0);
    let revision = fx.revision();
    let out = fx.edit(
        TextRange::empty(at),
        PageEditOp::Insert {
            text: "Before anything else, this inserted sentence pushes the rest of the paragraph. ".into(),
        },
        1,
    );
    let PageEditOutcome::Applied { revision: new_revision, .. } = out else { panic!("{out:?}") };
    assert_eq!(new_revision, revision + 1);
    let inserted = fx.text().len() - before.text_of(&fx.document).unwrap().len();
    let mapped = map_offset(
        target,
        &[exposition_core::document::TextPatch::insert(at, "x".repeat(inserted))],
        Affinity::Downstream,
    );
    assert_eq!(mapped, fx.offset_of("Offices", 0));

    let after = fx.build();
    let map = after.map.as_ref().unwrap();
    let new = map.caret_geometry(&fx.path, mapped, Affinity::Downstream).unwrap();
    assert!(new.exact);
    assert!(new.top > old.top + 5.0, "Offices moved down a line: {:.1} -> {:.1}", old.top, new.top);
    assert!(new.line_text.contains("Offices"));
    // Clicking at the new position maps back to the same source offset.
    let hit = map.hit_test(new.page, new.x + 0.5, (new.top + new.bottom) / 2.0);
    assert_eq!(hit.caret.unwrap().offset, mapped);
    // Floats below move too.
    let figure_after = map.selection(&fx.path, fx.offset_of("A gray", 0), fx.offset_of("A gray", 6)).rects[0];
    assert!(figure_after.y > figure_before.y, "figure caption moved down");
    let footnote_after =
        map.selection(&fx.path, fx.offset_of("A footnote", 0), fx.offset_of("A footnote", 10)).rects[0];
    assert!((footnote_after.y - footnote_before.y).abs() < 1.0, "footnote stays at the page bottom");
}

#[test]
fn stale_edits_rebase_or_resync() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let record = fx.build();
    let built = record.revision_of(&fx.document).unwrap();
    // A source-editor change lands elsewhere after the build.
    fx.documents
        .apply(exposition_core::document::TransactionRequest {
            id: exposition_core::document::TransactionId("src".into()),
            origin: exposition_core::document::EditOrigin::Source,
            edits: vec![exposition_core::document::DocumentEdit {
                document: fx.document.clone(),
                base_revision: built,
                patches: vec![exposition_core::document::TextPatch::insert(fx.offset_of("Offices", 0), "Busy ")],
            }],
            undo: exposition_core::document::UndoBehavior::NewGroup,
            conflict: exposition_core::document::ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    // A page edit based on the built revision (whose text the build retains)
    // is planned against that text and rebased over the disjoint change.
    let at = record.text_of(&fx.document).unwrap().find("plain prose").unwrap() as u32;
    let request = exposition_page::PageEditRequest {
        id: exposition_core::document::TransactionId("p".into()),
        document: fx.document.clone(),
        base_revision: built,
        selection: TextRange::empty(at),
        op: PageEditOp::Insert { text: "X".into() },
        group: 1,
    };
    let out = exposition_page::apply_page_edit(&fx.documents, &exposition_page::TypstInput, &request, &|d, r| {
        fx.service.revision_text(d, r)
    })
    .unwrap();
    assert!(matches!(out, PageEditOutcome::Applied { rebased: true, caret, .. } if caret == at + 1), "{out:?}");
    assert!(fx.text().contains("Xplain prose") && fx.text().contains("Busy Offices"));
    // A revision whose text is unknown cannot be planned: resync instead.
    let unknown = exposition_page::PageEditRequest { base_revision: built + 1, ..request };
    let out =
        exposition_page::apply_page_edit(&fx.documents, &exposition_page::TypstInput, &unknown, &|_, _| None).unwrap();
    assert!(matches!(out, PageEditOutcome::Stale { .. }), "{out:?}");
}

#[test]
fn pdf_export_matches_rendered_pages() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let record = fx.build();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.pdf");
    let artifact = fx.service.export_pdf(record.id(), Some(dest.clone())).unwrap();
    let bytes = std::fs::read(&dest).unwrap();
    assert_eq!(artifact.content_hash, exposition_typesetting::snapshot::sha256_hex(&bytes));
    let text = String::from_utf8_lossy(&bytes);
    let pages = text.match_indices("/Type/Page").filter(|(i, _)| !text[i + 10..].starts_with('s')).count();
    assert_eq!(pages, record.result.pages.len());
    let boxes: Vec<(f64, f64)> = text
        .match_indices("/MediaBox[")
        .map(|(i, m)| {
            let rest = &text[i + m.len()..];
            let nums: Vec<f64> =
                rest[..rest.find(']').unwrap()].split_whitespace().map(|n| n.parse().unwrap()).collect();
            (nums[2] - nums[0], nums[3] - nums[1])
        })
        .collect();
    assert_eq!(boxes.len(), record.result.pages.len());
    for (b, p) in boxes.iter().zip(&record.result.pages) {
        assert!((b.0 - p.width_pt).abs() < 0.01 && (b.1 - p.height_pt).abs() < 0.01, "{b:?} vs {p:?}");
    }
    // The SVG of the same build has the same page size.
    let svg = fx.service.page_svg(&record.result.pages[0].render_ref).unwrap();
    assert!(svg.starts_with("<svg") && !svg.contains("<script"));
    let view_box = svg.split("viewBox=\"").nth(1).unwrap().split('"').next().unwrap();
    let dims: Vec<f64> = view_box.split_whitespace().map(|n| n.parse().unwrap()).collect();
    assert!((dims[2] - record.result.pages[0].width_pt).abs() < 0.01);
    assert!((dims[3] - record.result.pages[0].height_pt).abs() < 0.01);
}

#[test]
fn failed_build_keeps_last_good_output_and_reports_utf16_range() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let good = fx.build();
    // An unknown variable in an equation is a compile error.
    let at = fx.offset_of("c^2$", 0);
    fx.documents
        .apply(exposition_core::document::TransactionRequest {
            id: exposition_core::document::TransactionId("e".into()),
            origin: exposition_core::document::EditOrigin::Source,
            edits: vec![exposition_core::document::DocumentEdit {
                document: fx.document.clone(),
                base_revision: fx.revision(),
                patches: vec![exposition_core::document::TextPatch::insert(at, "😀 #nosuchthing ")],
            }],
            undo: exposition_core::document::UndoBehavior::NewGroup,
            conflict: exposition_core::document::ConflictPolicy::Reject,
            label: None,
            selection: None,
        })
        .unwrap();
    let failed = fx.build();
    assert_eq!(failed.result.status, BuildStatus::Failed);
    assert!(failed.map.is_none() && failed.result.pages.is_empty());
    let error = failed.result.diagnostics.iter().find(|d| d.severity == Severity::Error).unwrap();
    let location = error.location.as_ref().unwrap();
    // UTF-16 range: the emoji before the error counts as two units.
    let expected = fx.offset_of("nosuchthing", 0);
    assert_eq!(location.range, TextRange::new(expected, expected + 11), "{error:?}");
    // The last good output stays available.
    assert_eq!(fx.service.last_good(&fx.path).unwrap().id(), good.id());
    assert!(fx.service.page_svg(&good.result.pages[0].render_ref).is_some());
}
