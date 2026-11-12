//! Conversions between the application's UTF-16 offsets and LSP positions,
//! Lean diagnostics and the shared diagnostic contract, and document patches
//! and LSP incremental changes.
//!
//! LSP positions default to UTF-16 code units (Lean uses the default), which
//! matches the application's text positions exactly; only the line/column
//! split differs.

use exposition_core::build::{Diagnostic, Severity, SourceLocation};
use exposition_core::document::TextPatch;
use exposition_core::path::WorkspacePath;
use exposition_core::text::{LineColumn, LineIndex, TextError, TextRange, utf16_range_to_bytes};
use serde_json::{Value, json};

/// Longest diagnostic message kept (Lean messages can embed huge terms).
const MAX_MESSAGE: usize = 20_000;

pub fn position(index: &LineIndex, offset: u32) -> Result<Value, TextError> {
    let lc = index.line_column(offset as usize)?;
    Ok(json!({ "line": lc.line, "character": lc.character }))
}

/// UTF-16 offset of an LSP position, clamped to the text as LSP requires
/// (a character past the line end means the line end; a line past the end
/// means the end of the text).
pub fn offset_of(index: &LineIndex, value: &Value) -> Option<u32> {
    let line = value.get("line")?.as_u64()?;
    let character = value.get("character")?.as_u64()?;
    offset_at(index, line.min(u32::MAX as u64) as u32, character.min(u32::MAX as u64) as u32)
}

/// UTF-16 offset of a zero-based line and UTF-16 column, clamped like [`offset_of`].
pub fn offset_at(index: &LineIndex, line: u32, character: u32) -> Option<u32> {
    if line as usize >= index.line_count() {
        return Some(index.len_utf16() as u32);
    }
    let lc = LineColumn { line, character };
    match index.offset(lc) {
        Ok(o) => Some(o as u32),
        // Past the line content: `len` is that line's content end.
        Err(TextError::OutOfBounds { len, .. }) => Some(len as u32),
        Err(_) => None,
    }
}

pub fn range_of(index: &LineIndex, value: &Value) -> Option<TextRange> {
    let start = offset_of(index, value.get("start")?)?;
    let end = offset_of(index, value.get("end")?)?;
    Some(TextRange::new(start.min(end), end.max(start)))
}

/// `declaration uses 'sorry'` (older Lean) / ``declaration uses `sorry` ``.
pub fn is_sorry_message(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("declaration uses 'sorry'") || m.contains("declaration uses `sorry`")
}

/// A Lean diagnostic in the shared shape. `fullRange` (Lean's extension
/// covering the whole erroneous syntax) is preferred over `range`.
pub fn diagnostic(value: &Value, index: &LineIndex, path: &WorkspacePath) -> Diagnostic {
    let severity = match value.get("severity").and_then(Value::as_u64) {
        Some(2) => Severity::Warning,
        Some(3 | 4) => Severity::Info,
        _ => Severity::Error,
    };
    let mut message: String = value.get("message").and_then(Value::as_str).unwrap_or("").to_owned();
    if message.len() > MAX_MESSAGE {
        let mut cut = MAX_MESSAGE;
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        message.truncate(cut);
        message.push_str("\n… (message truncated)");
    }
    let range = value
        .get("fullRange")
        .and_then(|r| range_of(index, r))
        .or_else(|| value.get("range").and_then(|r| range_of(index, r)));
    let mut hints = Vec::new();
    if is_sorry_message(&message) {
        hints.push("Incomplete proof: this declaration uses sorry.".to_owned());
    }
    let tags: Vec<u64> = value
        .get("leanTags")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    if tags.contains(&1) {
        hints.push("Unsolved goals remain at the end of this proof.".to_owned());
    }
    Diagnostic { severity, message, location: range.map(|range| SourceLocation { path: path.clone(), range }), hints }
}

/// Apply sorted, non-overlapping patches (relative to `text`) and return the new text.
pub fn apply_patches(text: &str, patches: &[TextPatch]) -> Option<String> {
    let mut out = text.to_owned();
    for patch in patches.iter().rev() {
        let bytes = utf16_range_to_bytes(&out, patch.range).ok()?;
        out.replace_range(bytes, &patch.text);
    }
    Some(out)
}

/// LSP incremental `contentChanges` for patches relative to `base`. Changes
/// apply sequentially, so they are emitted last-first: each range then refers
/// to text that earlier (later-positioned) changes did not touch.
pub fn content_changes(base: &LineIndex, patches: &[TextPatch]) -> Option<Vec<Value>> {
    let mut changes = Vec::with_capacity(patches.len());
    let mut previous_start = u32::MAX;
    for patch in patches.iter().rev() {
        if patch.range.end > previous_start || !patch.range.is_ordered() {
            return None; // unsorted or overlapping: caller falls back to a full sync
        }
        previous_start = patch.range.start;
        changes.push(json!({
            "range": { "start": position(base, patch.range.start).ok()?, "end": position(base, patch.range.end).ok()? },
            "text": patch.text,
        }));
    }
    Some(changes)
}

/// Decode LSP semantic tokens (relative encoding) and return the ranges of
/// tokens with `token_type`.
pub fn tokens_of_type(index: &LineIndex, data: &[u64], token_type: u64) -> Vec<TextRange> {
    let mut out = Vec::new();
    let (mut line, mut start) = (0u64, 0u64);
    for chunk in data.chunks_exact(5) {
        let (delta_line, delta_start, length, kind) = (chunk[0], chunk[1], chunk[2], chunk[3]);
        if delta_line > 0 {
            line += delta_line;
            start = delta_start;
        } else {
            start += delta_start;
        }
        if kind == token_type {
            let from = json!({ "line": line, "character": start });
            let to = json!({ "line": line, "character": start + length });
            if let (Some(a), Some(b)) = (offset_of(index, &from), offset_of(index, &to)) {
                out.push(TextRange::new(a, b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_with_astral_characters_and_clamp() {
        let text = "a😀b\r\nαβ\n";
        let index = LineIndex::new(text);
        assert_eq!(position(&index, 3).unwrap(), json!({"line":0,"character":3}));
        assert_eq!(offset_of(&index, &json!({"line":0,"character":3})), Some(3));
        assert_eq!(position(&index, 7).unwrap(), json!({"line":1,"character":1}));
        // Past the line end clamps to the content end, past the text to its end.
        assert_eq!(offset_of(&index, &json!({"line":0,"character":99})), Some(4));
        assert_eq!(offset_of(&index, &json!({"line":9,"character":0})), Some(index.len_utf16() as u32));
    }

    #[test]
    fn incremental_changes_reproduce_the_edit() {
        let base = "theorem t : 1 = 1 := by\n  skip\n-- αβ\n";
        let patches = vec![TextPatch::replace(26, 30, "rfl"), TextPatch::insert(36, "γ")];
        let expected = apply_patches(base, &patches).unwrap();
        assert_eq!(expected, "theorem t : 1 = 1 := by\n  rfl\n-- αβγ\n");
        // Apply the LSP changes sequentially the way a server does.
        let changes = content_changes(&LineIndex::new(base), &patches).unwrap();
        let mut text = base.to_owned();
        for change in changes {
            let index = LineIndex::new(&text);
            let range = range_of(&index, &change["range"]).unwrap();
            let bytes = utf16_range_to_bytes(&text, range).unwrap();
            text.replace_range(bytes, change["text"].as_str().unwrap());
        }
        assert_eq!(text, expected);
        assert!(
            content_changes(&LineIndex::new(base), &[TextPatch::insert(5, "x"), TextPatch::insert(2, "y")]).is_none()
        );
    }

    #[test]
    fn converts_lean_diagnostics_and_recognizes_sorry() {
        let text = "theorem a : True := by\n  sorry\n";
        let index = LineIndex::new(text);
        let path = WorkspacePath::parse("A.lean").unwrap();
        let sorry = json!({"severity":2,"message":"declaration uses `sorry`","range":{"start":{"line":0,"character":8},"end":{"line":0,"character":9}}});
        let d = diagnostic(&sorry, &index, &path);
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.location.unwrap().range, TextRange::new(8, 9));
        assert!(is_sorry_message(&d.message) && !d.hints.is_empty());
        assert!(is_sorry_message("declaration uses 'sorry'"));
        assert!(!is_sorry_message("unused variable `sorry`"));
        let goals = json!({"severity":1,"leanTags":[1],"message":"unsolved goals\n⊢ True","range":{"start":{"line":0,"character":20},"end":{"line":1,"character":0}},"fullRange":{"start":{"line":0,"character":20},"end":{"line":1,"character":7}}});
        let d = diagnostic(&goals, &index, &path);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.location.unwrap().range, TextRange::new(20, 30), "fullRange is preferred");
        assert_eq!(tokens_of_type(&index, &[1, 2, 5, 23, 0], 23), vec![TextRange::new(25, 30)]);
    }
}
