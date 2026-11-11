//! Minimal text patches between two versions of a document.
//!
//! Used for reloads after external changes and for crash recovery, so the
//! document service receives a small transaction (keeping unrelated cursor
//! positions and undo history meaningful) instead of a whole-text replacement.

use exposition_core::document::TextPatch;
use exposition_core::text::{TextRange, utf16_len};

/// Largest line-comparison table computed before falling back to one hunk.
const MAX_TABLE_CELLS: usize = 4_000_000;

/// Sorted, non-overlapping patches (relative to `old`) that turn `old` into `new`.
pub fn diff_patches(old: &str, new: &str) -> Vec<TextPatch> {
    if old == new {
        return Vec::new();
    }
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();

    let prefix = old_lines.iter().zip(&new_lines).take_while(|(a, b)| a == b).count();
    let max_suffix = old_lines.len().min(new_lines.len()) - prefix;
    let suffix =
        old_lines.iter().rev().zip(new_lines.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let old_mid = &old_lines[prefix..old_lines.len() - suffix];
    let new_mid = &new_lines[prefix..new_lines.len() - suffix];

    // Hunks as (old line range, new line range) relative to the middle slices.
    let hunks = if old_mid.len().saturating_mul(new_mid.len()) <= MAX_TABLE_CELLS {
        line_hunks(old_mid, new_mid)
    } else {
        vec![((0, old_mid.len()), (0, new_mid.len()))]
    };

    // UTF-16 offset at the start of each old line, and byte offsets of each line.
    let mut old_starts = Vec::with_capacity(old_lines.len() + 1);
    let mut units = 0usize;
    for line in &old_lines {
        old_starts.push(units);
        units += utf16_len(line);
    }
    old_starts.push(units);

    let mut patches = Vec::with_capacity(hunks.len());
    for ((os, oe), (ns, ne)) in hunks {
        let (os, oe, ns, ne) = (os + prefix, oe + prefix, ns + prefix, ne + prefix);
        let removed: String = old_lines[os..oe].concat();
        let inserted: String = new_lines[ns..ne].concat();
        // Trim common characters at both ends of the hunk.
        let head: usize =
            removed.chars().zip(inserted.chars()).take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum();
        let tail: usize = removed[head..]
            .chars()
            .rev()
            .zip(inserted[head..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(c, _)| c.len_utf8())
            .sum();
        let removed_core = &removed[head..removed.len() - tail];
        let inserted_core = &inserted[head..inserted.len() - tail];
        let start = old_starts[os] + utf16_len(&removed[..head]);
        let end = start + utf16_len(removed_core);
        patches.push(TextPatch { range: TextRange::new(to_u32(start), to_u32(end)), text: inserted_core.to_owned() });
    }
    patches
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).expect("document text exceeds u32 UTF-16 code units")
}

type Hunk = ((usize, usize), (usize, usize));

/// Line hunks from a longest-common-subsequence table.
fn line_hunks(old: &[&str], new: &[&str]) -> Vec<Hunk> {
    let (n, m) = (old.len(), new.len());
    if n == 0 || m == 0 {
        return if n == 0 && m == 0 { Vec::new() } else { vec![((0, n), (0, m))] };
    }
    // lcs[i * (m + 1) + j] = LCS length of old[i..] and new[j..].
    let width = m + 1;
    let mut lcs = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if old[i] == new[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let mut hunks = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut open: Option<(usize, usize)> = None;
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            if let Some((hi, hj)) = open.take() {
                hunks.push(((hi, i), (hj, j)));
            }
            i += 1;
            j += 1;
        } else {
            open.get_or_insert((i, j));
            if j < m && (i == n || lcs[i * width + j + 1] >= lcs[(i + 1) * width + j]) {
                j += 1;
            } else {
                i += 1;
            }
        }
    }
    if let Some((hi, hj)) = open {
        hunks.push(((hi, n), (hj, m)));
    }
    hunks
}

/// Apply sorted patches to `text` (for tests and verification).
pub fn apply_patches(text: &str, patches: &[TextPatch]) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for patch in patches {
        let range = exposition_core::text::utf16_range_to_bytes(text, patch.range).ok()?;
        if range.start < cursor {
            return None;
        }
        out.push_str(&text[cursor..range.start]);
        out.push_str(&patch.text);
        cursor = range.end;
    }
    out.push_str(&text[cursor..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(old: &str, new: &str) -> Vec<TextPatch> {
        let patches = diff_patches(old, new);
        assert_eq!(apply_patches(old, &patches).as_deref(), Some(new), "patches {patches:?}");
        for pair in patches.windows(2) {
            assert!(pair[0].range.end <= pair[1].range.start, "sorted and disjoint: {patches:?}");
        }
        patches
    }

    #[test]
    fn produces_small_patches_for_separate_changes() {
        assert!(check("same", "same").is_empty());
        let patches = check("a\nb\nc\nd\ne\n", "a\nB\nc\nd\nE\n");
        assert_eq!(patches, vec![TextPatch::replace(2, 3, "B"), TextPatch::replace(8, 9, "E")]);
        let patches = check("line one\nline two\n", "line one\nline 2\n");
        assert_eq!(patches, vec![TextPatch::replace(14, 17, "2")]);
    }

    #[test]
    fn handles_insertions_deletions_crlf_and_astral_text() {
        check("", "new\n");
        check("old\n", "");
        check("x\r\ny\r\n", "x\r\ninserted\r\ny\r\n");
        check("😀 a\n😀 b\n", "😀 a\n😀 c\n😀 b\n");
        let patches = check("α😀β", "α😀γβ");
        assert_eq!(patches, vec![TextPatch::insert(3, "γ")]);
        check("no newline", "no newline\nnow two");
        check("a\nb\nc", "c\nb\na");
    }
}
