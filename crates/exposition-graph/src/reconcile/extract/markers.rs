//! Attachment marker blocks written by the format adapters:
//! `// exposition:begin <key>` … `// exposition:end <key>` (Typst),
//! `% exposition:begin <key>` (LaTeX), `<!-- exposition:begin <key> -->`
//! (Markdown). Each marker is a whole (trimmed) line; the key is one token.
//!
//! Only well-formed, unique pairs become attachments: a key with an unpaired,
//! repeated, or reversed marker yields nothing (the caller cannot know which
//! block is meant) and is reported by [`malformed_keys`].

use std::collections::BTreeMap;

use super::{Attachment, PathBase, Utf16Index, valid_name};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerFormat {
    Typst,
    Latex,
    Markdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Begin,
    End,
}

struct Marker<'t> {
    side: Side,
    key: &'t str,
    /// Byte offset of the start of the marker line.
    line_start: usize,
    /// Byte offset of the end of the marker line (before its line break).
    line_end: usize,
}

/// Well-formed attachment blocks, in source order.
pub fn scan(format: MarkerFormat, text: &str, index: &Utf16Index<'_>) -> Vec<Attachment> {
    let markers = markers(format, text);
    if markers.is_empty() {
        return Vec::new();
    }
    let images = images(format, text);
    let base = match format {
        MarkerFormat::Latex => PathBase::LatexMain,
        MarkerFormat::Typst | MarkerFormat::Markdown => PathBase::File,
    };
    let mut out: Vec<(usize, Attachment)> = Vec::new();
    for (key, group) in group_by_key(&markers) {
        let Some((begin, end)) = well_formed(&group) else { continue };
        let inner = begin.line_end..end.line_start;
        // First image whose syntax starts inside the block.
        let at = images.partition_point(|(pos, _)| *pos < inner.start);
        let image = images.get(at).filter(|(pos, _)| *pos < inner.end).map(|(_, path)| path.clone());
        out.push((
            begin.line_start,
            Attachment { key: key.to_owned(), image, base, range: index.range(begin.line_start..end.line_end) },
        ));
    }
    out.sort_by_key(|(start, _)| *start);
    out.into_iter().map(|(_, a)| a).collect()
}

/// Keys whose markers are unpaired, repeated, or out of order (sorted).
pub fn malformed_keys(format: MarkerFormat, text: &str) -> Vec<String> {
    let markers = markers(format, text);
    group_by_key(&markers)
        .into_iter()
        .filter(|(_, group)| well_formed(group).is_none())
        .map(|(key, _)| key.to_owned())
        .collect()
}

fn group_by_key<'m, 't>(markers: &'m [Marker<'t>]) -> BTreeMap<&'t str, Vec<&'m Marker<'t>>> {
    let mut groups: BTreeMap<&str, Vec<&Marker<'_>>> = BTreeMap::new();
    for m in markers {
        groups.entry(m.key).or_default().push(m);
    }
    groups
}

fn well_formed<'m, 't>(group: &[&'m Marker<'t>]) -> Option<(&'m Marker<'t>, &'m Marker<'t>)> {
    match group {
        [begin, end] if begin.side == Side::Begin && end.side == Side::End && begin.line_end <= end.line_start => {
            Some((begin, end))
        }
        _ => None,
    }
}

fn markers(format: MarkerFormat, text: &str) -> Vec<Marker<'_>> {
    let mut out = Vec::new();
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let line_start = pos;
        pos += line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        // Cheap pre-filter: every marker mentions `exposition:`.
        if !content.contains("exposition:") {
            continue;
        }
        if let Some((side, key)) = parse_marker(format, content.trim()) {
            out.push(Marker { side, key, line_start, line_end: line_start + content.len() });
        }
    }
    out
}

fn parse_marker(format: MarkerFormat, line: &str) -> Option<(Side, &str)> {
    let body = match format {
        MarkerFormat::Typst => line.strip_prefix("//")?,
        MarkerFormat::Latex => line.strip_prefix('%')?.trim_start_matches('%'),
        MarkerFormat::Markdown => line.strip_prefix("<!--")?.strip_suffix("-->")?,
    };
    let body = body.trim();
    let (side, rest) = if let Some(rest) = body.strip_prefix("exposition:begin") {
        (Side::Begin, rest)
    } else if let Some(rest) = body.strip_prefix("exposition:end") {
        (Side::End, rest)
    } else {
        return None;
    };
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let key = rest.trim();
    if key.contains(char::is_whitespace) || !valid_name(key) {
        return None;
    }
    Some((side, key))
}

/// Every image the adapter syntax names, as (byte offset, path), ascending.
/// The syntax is the adapters' own literal form, so the parsers are strict
/// and bounded (each stops at a line break).
fn images(format: MarkerFormat, text: &str) -> Vec<(usize, String)> {
    type Parser = fn(&str, usize) -> Option<String>;
    let (needle, parse): (&str, Parser) = match format {
        MarkerFormat::Typst => ("image(", typst_image),
        MarkerFormat::Latex => ("\\includegraphics", latex_image),
        MarkerFormat::Markdown => ("![", markdown_image),
    };
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(k) = text.get(from..).and_then(|rest| rest.find(needle)) {
        let at = from + k;
        if let Some(path) = parse(text, at) {
            out.push((at, path));
        }
        from = at + needle.len();
    }
    out
}

fn line_rest(text: &str, at: usize) -> &str {
    let rest = text.get(at..).unwrap_or("");
    rest.split('\n').next().unwrap_or("")
}

fn typst_image(text: &str, at: usize) -> Option<String> {
    // Not a longer identifier ending in `image` (`myimage(`).
    if text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let rest = line_rest(text, at + "image(".len()).trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut path = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return (!path.is_empty()).then_some(path),
            '\\' => path.push(chars.next()?),
            c => path.push(c),
        }
    }
    None
}

fn latex_image(text: &str, at: usize) -> Option<String> {
    let mut rest = line_rest(text, at + "\\includegraphics".len());
    if let Some(r) = rest.strip_prefix('*') {
        rest = r;
    }
    rest = rest.trim_start();
    if let Some(r) = rest.strip_prefix('[') {
        rest = &r[r.find(']')? + 1..];
        rest = rest.trim_start();
    }
    let r = rest.strip_prefix('{')?;
    let path = r[..r.find('}')?].trim();
    (!path.is_empty() && !path.contains(['\\', '#', '{'])).then(|| path.to_owned())
}

fn markdown_image(text: &str, at: usize) -> Option<String> {
    let rest = line_rest(text, at + 2);
    let close = rest.find(']')?;
    if rest[..close].contains('[') {
        return None;
    }
    let rest = rest[close + 1..].strip_prefix('(')?.trim_start();
    let path = if let Some(r) = rest.strip_prefix('<') {
        &r[..r.find('>')?]
    } else {
        let end = rest.find(|c: char| c.is_whitespace() || c == ')')?;
        &rest[..end]
    };
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice<'a>(text: &'a str, a: &Attachment) -> &'a str {
        let r = exposition_core::text::utf16_range_to_bytes(text, a.range).unwrap();
        &text[r]
    }

    #[test]
    fn typst_blocks_pair_and_find_their_image() {
        let text = "intro\n// exposition:begin fig-1\n#figure(image(\"plots/a.png\", width: 50%))\n// exposition:end fig-1\nafter\n";
        let index = Utf16Index::new(text);
        let found = scan(MarkerFormat::Typst, text, &index);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "fig-1");
        assert_eq!(found[0].image.as_deref(), Some("plots/a.png"));
        assert_eq!(found[0].base, PathBase::File);
        assert!(slice(text, &found[0]).starts_with("// exposition:begin fig-1"));
        assert!(slice(text, &found[0]).ends_with("// exposition:end fig-1"));
        assert!(malformed_keys(MarkerFormat::Typst, text).is_empty());
    }

    #[test]
    fn latex_and_markdown_markers() {
        let tex = "  % exposition:begin k\n\\includegraphics[width=3cm]{figs/b.pdf}\n%exposition:end k\n";
        let found = scan(MarkerFormat::Latex, tex, &Utf16Index::new(tex));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].image.as_deref(), Some("figs/b.pdf"));
        assert_eq!(found[0].base, PathBase::LatexMain);
        assert!(slice(tex, &found[0]).starts_with("  % exposition:begin k"));

        let md = "<!-- exposition:begin m1 -->\n![A plot](out/plot.png \"t\")\n<!-- exposition:end m1 -->\n";
        let found = scan(MarkerFormat::Markdown, md, &Utf16Index::new(md));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].image.as_deref(), Some("out/plot.png"));

        // A block without an image is still an attachment.
        let md = "<!-- exposition:begin t -->\n| a |\n<!-- exposition:end t -->";
        let found = scan(MarkerFormat::Markdown, md, &Utf16Index::new(md));
        assert_eq!(found[0].image, None);
        assert_eq!(slice(md, &found[0]), md);
    }

    #[test]
    fn unpaired_and_duplicated_keys_are_malformed() {
        let text = "// exposition:begin a\n// exposition:end a\n// exposition:begin a\n// exposition:end a\n\
                    // exposition:begin b\n// exposition:end c\n// exposition:end d\n// exposition:begin d\n\
                    // exposition:begin ok\nimage(\"x.png\")\n// exposition:end ok\n";
        let found = scan(MarkerFormat::Typst, text, &Utf16Index::new(text));
        assert_eq!(found.iter().map(|a| a.key.as_str()).collect::<Vec<_>>(), ["ok"]);
        assert_eq!(malformed_keys(MarkerFormat::Typst, text), ["a", "b", "c", "d"]);
    }

    #[test]
    fn nested_blocks_and_non_markers() {
        let text = "// exposition:begin outer\n// exposition:begin inner\nimage(\"in.png\")\n// exposition:end inner\n// exposition:end outer\n\
                    // exposition:beginx y\n// exposition:begin two words\n% exposition:begin wrong-format\n";
        let found = scan(MarkerFormat::Typst, text, &Utf16Index::new(text));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].key, "outer");
        assert_eq!(found[0].image.as_deref(), Some("in.png"));
        assert_eq!(found[1].key, "inner");
        assert!(malformed_keys(MarkerFormat::Typst, text).is_empty());
    }

    #[test]
    fn hostile_text_does_not_panic() {
        for text in ["", "// exposition:begin", "image(\"", "\\includegraphics[{", "![é](", "// exposition:begin é\n"]
        {
            let index = Utf16Index::new(text);
            for f in [MarkerFormat::Typst, MarkerFormat::Latex, MarkerFormat::Markdown] {
                let _ = scan(f, text, &index);
                let _ = malformed_keys(f, text);
            }
        }
    }
}
