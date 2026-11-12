//! YAML front matter: the small subset the print profile reads.
//!
//! Supported: `key: value` lines at the top level whose value is a plain,
//! single-quoted or double-quoted scalar on one line, a flow sequence of such
//! scalars (`[a, b]`), or a block sequence (`- a` lines under an empty key).
//! Comments and blank lines are ignored. Anything else is reported and
//! ignored; the front matter is never rewritten by translation.

use std::ops::Range;

/// How a scalar is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarStyle {
    Plain,
    SingleQuoted,
    DoubleQuoted,
}

/// One scalar value with its location in the Markdown source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scalar {
    /// Decoded value.
    pub value: String,
    /// Byte range of the value's characters (inside the quotes, if any).
    pub range: Range<usize>,
    pub style: ScalarStyle,
    /// Whether `value` is exactly the source text at `range` (no escapes).
    pub linear: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub key_range: Range<usize>,
    pub values: Vec<Scalar>,
    /// Written as a sequence.
    pub list: bool,
    /// Byte range of the whole entry (key line and sequence lines).
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub range: Range<usize>,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrontMatter {
    /// Byte range of the whole block including the `---` fences.
    pub block: Range<usize>,
    pub entries: Vec<Entry>,
    pub problems: Vec<Problem>,
}

impl FrontMatter {
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// The first scalar of `key`.
    pub fn scalar(&self, key: &str) -> Option<&Scalar> {
        self.get(key).and_then(|e| e.values.first())
    }

    /// The scalar whose value range contains byte `p` (boundaries included).
    pub fn scalar_at(&self, p: usize) -> Option<&Scalar> {
        self.entries.iter().flat_map(|e| &e.values).find(|s| s.range.start <= p && p <= s.range.end)
    }
}

/// Parse the YAML `content` located at byte `base` of the Markdown source.
pub fn parse(content: &str, base: usize, block: Range<usize>) -> FrontMatter {
    let mut fm = FrontMatter { block, ..Default::default() };
    let mut offset = 0usize;
    let mut current: Option<Entry> = None;
    for raw_line in content.split_inclusive('\n') {
        let line_start = base + offset;
        offset += raw_line.len();
        let line = raw_line.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent > 0 || trimmed.starts_with("- ") || trimmed == "-" {
            // Block sequence item of the current (empty-valued) key.
            if let (Some(entry), Some(rest)) = (current.as_mut(), trimmed.strip_prefix('-'))
                && (entry.list || entry.values.is_empty())
            {
                let rest_start = line_start + indent + 1;
                let lead = rest.len() - rest.trim_start().len();
                match scalar(rest.trim_start(), rest_start + lead) {
                    Ok(Some(s)) => {
                        entry.values.push(s);
                        entry.list = true;
                        entry.range.end = line_start + line.len();
                    }
                    Ok(None) => {}
                    Err(p) => fm.problems.push(p),
                }
                continue;
            }
            fm.problems.push(Problem {
                range: line_start..line_start + line.len(),
                message: "unsupported front matter line (nested YAML is not read by the print profile)".into(),
            });
            continue;
        }
        if let Some(entry) = current.take() {
            fm.entries.push(entry);
        }
        let Some(colon) = trimmed.find(':') else {
            fm.problems.push(Problem {
                range: line_start..line_start + line.len(),
                message: "expected `key: value` in front matter".into(),
            });
            continue;
        };
        let key = &trimmed[..colon];
        if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            fm.problems.push(Problem {
                range: line_start..line_start + line.len(),
                message: format!("unsupported front matter key `{key}`"),
            });
            continue;
        }
        let after = &trimmed[colon + 1..];
        let lead = after.len() - after.trim_start().len();
        let value_text = after.trim_start();
        let value_start = line_start + colon + 1 + lead;
        let mut entry = Entry {
            key: key.to_owned(),
            key_range: line_start..line_start + colon,
            values: Vec::new(),
            list: false,
            range: line_start..line_start + line.len(),
        };
        if let Some(inner) = value_text.strip_prefix('[') {
            entry.list = true;
            match flow_sequence(inner, value_start + 1) {
                Ok(values) => entry.values = values,
                Err(p) => fm.problems.push(p),
            }
        } else if value_text.starts_with('|') || value_text.starts_with('>') {
            fm.problems.push(Problem {
                range: value_start..value_start + value_text.len(),
                message: "block scalars are not supported in front matter".into(),
            });
        } else {
            match scalar(value_text, value_start) {
                Ok(Some(s)) => entry.values.push(s),
                Ok(None) => {}
                Err(p) => fm.problems.push(p),
            }
        }
        current = Some(entry);
    }
    if let Some(entry) = current.take() {
        fm.entries.push(entry);
    }
    fm
}

/// Parse one scalar at the start of `text` (located at byte `at`). Returns
/// `None` for an empty value.
fn scalar(text: &str, at: usize) -> Result<Option<Scalar>, Problem> {
    let text = strip_comment(text);
    if text.is_empty() {
        return Ok(None);
    }
    let problem = |message: &str| Problem { range: at..at + text.len(), message: message.into() };
    if let Some(body) = text.strip_prefix('"') {
        let (value, end, linear) = double_quoted(body).ok_or_else(|| problem("unterminated double-quoted string"))?;
        if !trailing_ok(&body[end + 1..]) {
            return Err(problem("unexpected text after a quoted value"));
        }
        return Ok(Some(Scalar { value, range: at + 1..at + 1 + end, style: ScalarStyle::DoubleQuoted, linear }));
    }
    if let Some(body) = text.strip_prefix('\'') {
        let (value, end, linear) = single_quoted(body).ok_or_else(|| problem("unterminated single-quoted string"))?;
        if !trailing_ok(&body[end + 1..]) {
            return Err(problem("unexpected text after a quoted value"));
        }
        return Ok(Some(Scalar { value, range: at + 1..at + 1 + end, style: ScalarStyle::SingleQuoted, linear }));
    }
    let value = text.trim_end();
    Ok(Some(Scalar { value: value.to_owned(), range: at..at + value.len(), style: ScalarStyle::Plain, linear: true }))
}

/// After a quoted value only whitespace and a comment may follow.
fn trailing_ok(rest: &str) -> bool {
    let rest = rest.trim();
    rest.is_empty() || rest.starts_with('#')
}

fn strip_comment(text: &str) -> &str {
    if text.starts_with(['"', '\'']) {
        return text;
    }
    match text.find(" #") {
        Some(i) => text[..i].trim_end(),
        None => text.trim_end(),
    }
}

/// Decode a double-quoted body; returns (value, byte index of the closing
/// quote, no escapes used).
fn double_quoted(body: &str) -> Option<(String, usize, bool)> {
    let mut value = String::new();
    let mut chars = body.char_indices();
    let mut linear = true;
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((value, i, linear)),
            '\\' => {
                linear = false;
                let (_, e) = chars.next()?;
                value.push(match e {
                    'n' => '\n',
                    't' => '\t',
                    '0' => '\0',
                    other => other,
                });
            }
            c => value.push(c),
        }
    }
    None
}

fn single_quoted(body: &str) -> Option<(String, usize, bool)> {
    let mut value = String::new();
    let mut linear = true;
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < body.len() {
        let c = body[i..].chars().next()?;
        if c == '\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                value.push('\'');
                linear = false;
                i += 2;
                continue;
            }
            return Some((value, i, linear));
        }
        value.push(c);
        i += c.len_utf8();
    }
    None
}

/// Parse `a, "b", c]` (after the opening bracket).
fn flow_sequence(inner: &str, at: usize) -> Result<Vec<Scalar>, Problem> {
    let Some(close) = inner.rfind(']') else {
        return Err(Problem { range: at..at + inner.len(), message: "unterminated flow sequence".into() });
    };
    let mut values = Vec::new();
    let body = &inner[..close];
    let mut start = 0usize;
    let mut in_quote: Option<char> = None;
    let mut items: Vec<(usize, usize)> = Vec::new();
    for (i, c) in body.char_indices() {
        match (in_quote, c) {
            (None, '"' | '\'') => in_quote = Some(c),
            (Some(q), c) if c == q => in_quote = None,
            (None, ',') => {
                items.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push((start, body.len()));
    for (a, b) in items {
        let item = &body[a..b];
        let lead = item.len() - item.trim_start().len();
        if let Some(s) = scalar(item.trim_start().trim_end(), at + a + lead)? {
            values.push(s);
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_scalars_sequences_and_ranges() {
        let src = "title: Hello *World*\nauthor:\n  - Ada\n  - 'O''Neil'\ndate: \"2026-11-11\" # c\nkeywords: [a, \"b c\"]\nnested:\n  x: 1\n";
        let fm = parse(src, 4, 0..src.len() + 8);
        let title = fm.scalar("title").unwrap();
        assert_eq!(title.value, "Hello *World*");
        assert_eq!(&src[title.range.start - 4..title.range.end - 4], "Hello *World*");
        let authors = fm.get("author").unwrap();
        assert!(authors.list);
        assert_eq!(authors.values.iter().map(|s| s.value.as_str()).collect::<Vec<_>>(), ["Ada", "O'Neil"]);
        assert!(!authors.values[1].linear);
        let date = fm.scalar("date").unwrap();
        assert_eq!((date.value.as_str(), date.style), ("2026-11-11", ScalarStyle::DoubleQuoted));
        assert_eq!(&src[date.range.start - 4..date.range.end - 4], "2026-11-11");
        let kw = fm.get("keywords").unwrap();
        assert_eq!(kw.values.len(), 2);
        assert_eq!(kw.values[1].value, "b c");
        assert_eq!(fm.problems.len(), 1, "{:?}", fm.problems);
    }
}
