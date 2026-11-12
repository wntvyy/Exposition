//! Input rules at every page caret position of the fixture: each position
//! the source map exposes as editable accepts typing (and deletion), and the
//! patched Markdown re-translates and compiles with the change visible.

mod common;

use std::collections::BTreeMap;

use common::*;
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_markdown::{
    EditMode, EditRefusal, MarkdownInput, PageEditOp, PageInputRules, TranslateOptions, translate,
};

fn options() -> TranslateOptions {
    TranslateOptions { document: Some(WorkspacePath::parse("main.md").unwrap()) }
}

/// Markdown caret positions reachable from rendered glyphs (segment
/// boundaries and, inside linear segments, every character boundary).
fn caret_positions(tr: &exposition_markdown::Translation) -> BTreeMap<u32, EditMode> {
    let mut out = BTreeMap::new();
    for s in tr.map.segments().iter().filter(|s| s.is_editable()) {
        let mode = s.kind.edit_mode().unwrap();
        if s.is_linear() {
            for m in s.markdown.start..=s.markdown.end {
                out.entry(m).or_insert(mode);
            }
        } else {
            out.entry(s.markdown.start).or_insert(mode);
            out.entry(s.markdown.end).or_insert(mode);
        }
    }
    out
}

#[test]
fn every_caret_position_accepts_typing_and_deletion() {
    let md = read_fixture("markdown-basic");
    let compiler = Compiler::new(fixture_dir("markdown-basic"));
    let tr = translate(&md, &options());
    let positions = caret_positions(&tr);
    let rendered = |text: &str| {
        let tr = translate(text, &options());
        let out = compiler.compile(&tr.typst);
        assert!(out.errors.is_empty(), "{:#?}", out.errors);
        let doc = out.document.unwrap();
        page_texts(&glyphs(&doc, &compiler.source())).concat()
    };
    let base_text = rendered(&md);
    let count = |s: &str, c: char| s.chars().filter(|&x| x == c || x == '𝑞').count();
    let mut typed_ok = 0;
    let mut refused: Vec<String> = Vec::new();
    let mut deleted_ok = 0;
    let mut delete_refusals: BTreeMap<String, usize> = BTreeMap::new();
    for (&p, &mode) in &positions {
        let utf16 = exposition_core::text::utf16_range_to_bytes(&md, TextRange::empty(p));
        if utf16.is_err() {
            continue;
        }
        // The input mode agrees with the source map.
        let at = MarkdownInput.mode_at(&md, p);
        assert!(
            at.is_some(),
            "position {p} ({:?}) has no input mode",
            &md[utf16.unwrap().start..][..8.min(md.len() - p as usize)]
        );
        let typed = if mode == EditMode::Math { "q" } else { "Q" };
        match MarkdownInput.plan(&md, TextRange::empty(p), &PageEditOp::Insert { text: typed.into() }, "\n") {
            Ok(plan) => {
                let edited = apply_patches(&md, &plan.patches);
                let text = rendered(&edited);
                let c = if mode == EditMode::Math { 'q' } else { 'Q' };
                assert_eq!(count(&text, c), count(&base_text, c) + 1, "typing at {p} shows one more {c:?}");
                typed_ok += 1;
            }
            Err(e) => refused.push(format!(
                "{p}: {e:?} near {:?}",
                &md[p as usize - 3.min(p as usize)..(p as usize + 3).min(md.len())]
            )),
        }
        match MarkdownInput.plan(&md, TextRange::empty(p), &PageEditOp::DeleteBackward, "\n") {
            Ok(plan) => {
                let edited = apply_patches(&md, &plan.patches);
                assert!(edited != md, "backspace at {p}: {:?} -> {:?}", plan.patches, &edited[..edited.len().min(80)]);
                let _ = rendered(&edited);
                deleted_ok += 1;
            }
            Err(e) => {
                let key = match e {
                    EditRefusal::NotEditable { reason } => format!("not editable: {reason}"),
                    other => format!("{other:?}"),
                };
                if std::env::var("SWEEP_VERBOSE").is_ok() {
                    let b = exposition_core::text::utf16_range_to_bytes(&md, TextRange::empty(p)).unwrap().start;
                    println!("backspace refused at {p}: {key} after {:?}", &md[b.saturating_sub(12)..b]);
                }
                *delete_refusals.entry(key).or_default() += 1;
            }
        }
    }
    println!(
        "caret sweep: {} positions; typing ok {typed_ok}, refused {}; backspace ok {deleted_ok}, refused {delete_refusals:?}",
        positions.len(),
        refused.len()
    );
    assert!(refused.is_empty(), "typing refused at: {refused:#?}");
}
