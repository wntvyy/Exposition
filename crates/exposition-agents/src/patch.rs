//! UTF-16 text patch utilities for change review: application, inversion,
//! diffing, and rebasing. Rebasing follows the document service's rules
//! exactly: a concurrent insertion at the start of a range lands before it, one
//! at the end of a non-empty range lands after it, and anything overlapping
//! conflicts.

use std::time::Duration;

use exposition_core::document::TextPatch;
use exposition_core::text::{TextRange, utf16_len, utf16_range_to_bytes};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("patch range {0:?} is invalid for the text")]
pub struct PatchError(pub TextRange);

/// Apply sorted, non-overlapping patches to `text`.
pub fn apply(text: &str, patches: &[TextPatch]) -> Result<String, PatchError> {
    let mut out = String::with_capacity(text.len() + patches.iter().map(|p| p.text.len()).sum::<usize>());
    let mut cursor = 0usize;
    for patch in patches {
        let bytes = utf16_range_to_bytes(text, patch.range).map_err(|_| PatchError(patch.range))?;
        if bytes.start < cursor {
            return Err(PatchError(patch.range));
        }
        out.push_str(&text[cursor..bytes.start]);
        out.push_str(&patch.text);
        cursor = bytes.end;
    }
    out.push_str(&text[cursor..]);
    Ok(out)
}

/// Patches that undo `patches` (sorted, relative to `old`), relative to the new text.
pub fn invert(old: &str, patches: &[TextPatch]) -> Result<Vec<TextPatch>, PatchError> {
    let mut shift: i64 = 0;
    let mut out = Vec::with_capacity(patches.len());
    for patch in patches {
        let start = u32::try_from(i64::from(patch.range.start) + shift).map_err(|_| PatchError(patch.range))?;
        let inserted = to_u32(utf16_len(&patch.text));
        let bytes = utf16_range_to_bytes(old, patch.range).map_err(|_| PatchError(patch.range))?;
        shift += i64::from(inserted) - i64::from(patch.range.len());
        out.push(TextPatch { range: TextRange::new(start, start + inserted), text: old[bytes].to_owned() });
    }
    Ok(out)
}

/// Minimal-ish patches turning `from` into `to` (sorted, relative to `from`).
///
/// Common prefix/suffix are trimmed first; the middle is diffed per character
/// with a time limit, after which the diff degrades to coarser hunks (still
/// correct, just larger).
pub fn diff(from: &str, to: &str) -> Vec<TextPatch> {
    if from == to {
        return Vec::new();
    }
    let prefix = common_prefix(from, to);
    let suffix = common_suffix(&from[prefix..], &to[prefix..]);
    let from_mid = &from[prefix..from.len() - suffix];
    let to_mid = &to[prefix..to.len() - suffix];
    let base = to_u32(utf16_len(&from[..prefix]));

    let diff = similar::TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .timeout(Duration::from_millis(500))
        .diff_chars(from_mid, to_mid);
    let old_chars: Vec<&str> = diff.old_slices().to_vec();
    let new_chars: Vec<&str> = diff.new_slices().to_vec();
    // UTF-16 offset of each old token boundary.
    let mut old_offsets = Vec::with_capacity(old_chars.len() + 1);
    let mut acc = 0u32;
    old_offsets.push(0);
    for c in &old_chars {
        acc += to_u32(utf16_len(c));
        old_offsets.push(acc);
    }

    let mut patches: Vec<TextPatch> = Vec::new();
    for op in diff.ops() {
        let (old_range, new_range) = (op.old_range(), op.new_range());
        if matches!(op.tag(), similar::DiffTag::Equal) {
            continue;
        }
        let start = base + old_offsets[old_range.start];
        let end = base + old_offsets[old_range.end];
        let text: String = new_chars[new_range].concat();
        match patches.last_mut() {
            Some(last) if last.range.end == start => {
                last.range.end = end;
                last.text.push_str(&text);
            }
            _ => patches.push(TextPatch { range: TextRange::new(start, end), text }),
        }
    }
    patches
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .take_while(|((_, x), y)| x == y)
        .last()
        .map(|((i, c), _)| i + c.len_utf8())
        .unwrap_or(0)
}

fn common_suffix(a: &str, b: &str) -> usize {
    a.chars().rev().zip(b.chars().rev()).take_while(|(x, y)| x == y).map(|(c, _)| c.len_utf8()).sum()
}

/// Map `range` through one revision's patches; `None` when they overlap.
pub fn map_range(range: TextRange, concurrent: &[TextPatch]) -> Option<TextRange> {
    let mut shift: i64 = 0;
    for q in concurrent {
        let delta = utf16_len(&q.text) as i64 - i64::from(q.range.len());
        if q.range.is_empty() {
            let at = q.range.start;
            if at <= range.start {
                shift += delta;
            } else if at < range.end {
                return None;
            }
        } else if q.range.end <= range.start {
            shift += delta;
        } else if q.range.start >= range.end {
            // After the range.
        } else {
            return None;
        }
    }
    let start = i64::from(range.start) + shift;
    let end = i64::from(range.end) + shift;
    Some(TextRange::new(u32::try_from(start).ok()?, u32::try_from(end).ok()?))
}

/// Rebase `patches` over one set of concurrent patches (both relative to the
/// same text). `None` when any patch overlaps a concurrent change.
pub fn rebase(patches: &[TextPatch], over: &[TextPatch]) -> Option<Vec<TextPatch>> {
    patches.iter().map(|p| map_range(p.range, over).map(|range| TextPatch { range, text: p.text.clone() })).collect()
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).expect("document text exceeds u32 UTF-16 code units")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(from: &str, to: &str) {
        let patches = diff(from, to);
        assert_eq!(apply(from, &patches).unwrap(), to, "diff({from:?}, {to:?}) = {patches:?}");
        let inverse = invert(from, &patches).unwrap();
        assert_eq!(apply(to, &inverse).unwrap(), from);
    }

    #[test]
    fn diff_round_trips_including_non_bmp_text() {
        roundtrip("", "");
        roundtrip("", "hello");
        roundtrip("hello", "");
        roundtrip("The cat sat.", "The big cat sat down.");
        roundtrip("α + β = γ", "α + δ = γ'");
        roundtrip("emoji 😀 here", "emoji 😃 there");
        roundtrip("line1\r\nline2\r\n", "line1\r\nline 2\r\nline3\r\n");
    }

    #[test]
    fn diff_produces_separate_hunks() {
        let patches = diff("aaaa XXX bbbb YYY cccc", "aaaa xxx bbbb yyy cccc");
        assert_eq!(patches.len(), 2, "{patches:?}");
    }

    #[test]
    fn rebase_matches_document_service_rules() {
        // Disjoint: shifted.
        let ours = vec![TextPatch::replace(8, 11, "lay")];
        let theirs = vec![TextPatch::insert(4, "big ")];
        assert_eq!(rebase(&ours, &theirs).unwrap(), vec![TextPatch::replace(12, 15, "lay")]);
        // Overlap: conflict.
        let theirs = vec![TextPatch::replace(9, 10, "o")];
        assert!(rebase(&ours, &theirs).is_none());
        // Insertion at the end of a non-empty range lands after it.
        let theirs = vec![TextPatch::insert(11, "!")];
        assert_eq!(rebase(&ours, &theirs).unwrap(), vec![TextPatch::replace(8, 11, "lay")]);
    }
}
