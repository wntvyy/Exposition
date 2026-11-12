//! Per-format block rendering, marker location, and caption extraction.

use std::ops::Range;

use crate::{AttachContent, AttachError, Format};

/// A caption: plain text (escaped for the format) or existing source kept verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caption {
    Plain(String),
    Raw(String),
}

pub(crate) fn line_ending(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    if crlf > 0 && crlf * 2 >= lf { "\r\n" } else { "\n" }
}

fn marker(format: Format, begin: bool, key: &str) -> String {
    let word = if begin { "begin" } else { "end" };
    match format {
        Format::Typst => format!("// exposition:{word} {key}"),
        Format::Latex => format!("% exposition:{word} {key}"),
        Format::Markdown => format!("<!-- exposition:{word} {key} -->"),
    }
}

/// `(begin, key)` when `line` (trimmed) is an attachment marker.
fn parse_marker(format: Format, line: &str) -> Option<(bool, &str)> {
    let line = line.trim();
    let body = match format {
        Format::Typst => line.strip_prefix("// exposition:")?,
        Format::Latex => line.strip_prefix("% exposition:")?,
        Format::Markdown => line.strip_prefix("<!-- exposition:")?.strip_suffix("-->")?.trim_end(),
    };
    let (word, key) = body.split_once(' ')?;
    let key = key.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    match word {
        "begin" => Some((true, key)),
        "end" => Some((false, key)),
        _ => None,
    }
}

/// Lines with their byte range (excluding the terminator).
fn lines(text: &str) -> impl Iterator<Item = (Range<usize>, &str)> {
    let mut start = 0;
    text.split_inclusive('\n').map(move |raw| {
        let content = raw.strip_suffix('\n').unwrap_or(raw);
        let content = content.strip_suffix('\r').unwrap_or(content);
        let range = start..start + content.len();
        start += raw.len();
        (range, content)
    })
}

pub(crate) fn find_markers(format: Format, text: &str, key: &str) -> (usize, usize) {
    let mut counts = (0, 0);
    for (_, line) in lines(text) {
        match parse_marker(format, line) {
            Some((true, k)) if k == key => counts.0 += 1,
            Some((false, k)) if k == key => counts.1 += 1,
            _ => {}
        }
    }
    counts
}

pub(crate) fn label_in_use(format: Format, text: &str, key: &str) -> bool {
    match format {
        Format::Typst => text.contains(&format!("<{key}>")),
        Format::Latex => text.contains(&format!("\\label{{{key}}}")),
        Format::Markdown => false,
    }
}

/// Byte range from the start of the begin marker line to the end of the end
/// marker line (terminator excluded).
pub(crate) fn locate(format: Format, text: &str, key: &str) -> Result<Range<usize>, AttachError> {
    let mut begins = Vec::new();
    let mut ends = Vec::new();
    for (range, line) in lines(text) {
        match parse_marker(format, line) {
            Some((true, k)) if k == key => begins.push(range),
            Some((false, k)) if k == key => ends.push(range),
            _ => {}
        }
    }
    match (begins.as_slice(), ends.as_slice()) {
        ([], []) => Err(AttachError::MissingMarkers(key.to_owned())),
        ([begin], [end]) if end.start > begin.end => Ok(begin.start..end.end),
        _ => Err(AttachError::AmbiguousMarkers { key: key.to_owned(), begin: begins.len(), end: ends.len() }),
    }
}

/// True when byte offset `at` lies strictly inside any attachment block.
pub(crate) fn inside_block(format: Format, text: &str, at: usize) -> bool {
    let mut open: Vec<(String, usize)> = Vec::new();
    for (range, line) in lines(text) {
        match parse_marker(format, line) {
            Some((true, key)) => open.push((key.to_owned(), range.start)),
            Some((false, key)) => {
                if let Some(index) = open.iter().rposition(|(k, _)| k == key) {
                    let (_, start) = open.remove(index);
                    if start < at && at < range.end {
                        return true;
                    }
                }
            }
            None => {}
        }
    }
    false
}

/// Index of `needle` outside LaTeX comments.
fn latex_find(text: &str, needle: &str) -> Option<usize> {
    for (range, line) in lines(text) {
        let code = latex_code_part(line);
        if let Some(i) = code.find(needle) {
            return Some(range.start + i);
        }
    }
    None
}

/// The part of a LaTeX line before an unescaped `%`.
fn latex_code_part(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'%' => return &line[..i],
            _ => i += 1,
        }
    }
    line
}

pub(crate) fn placement_problem(format: Format, text: &str, at: usize) -> Option<String> {
    if format != Format::Latex {
        return None;
    }
    if let Some(begin) = latex_find(text, "\\begin{document}")
        && at < begin + "\\begin{document}".len()
    {
        return Some("Figures and tables must be inserted after \\begin{document}.".into());
    }
    if let Some(end) = latex_find(text, "\\end{document}")
        && at > end
    {
        return Some("Figures and tables must be inserted before \\end{document}.".into());
    }
    None
}

/// Start of the line holding `\begin{document}`, when it begins that line.
pub(crate) fn preamble_insert_point(text: &str) -> Option<usize> {
    let at = latex_find(text, "\\begin{document}")?;
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    text[line_start..at].trim().is_empty().then_some(line_start)
}

pub(crate) fn missing_latex_packages(text: &str, table: bool) -> Vec<&'static str> {
    let mut loaded = Vec::new();
    for (_, line) in lines(text) {
        let mut code = latex_code_part(line);
        while let Some(i) = code.find("\\usepackage").or_else(|| code.find("\\RequirePackage")) {
            let rest = &code[i + 1..];
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphabetic());
            let rest = rest.trim_start();
            let rest = if let Some(after) = rest.strip_prefix('[') {
                after.split_once(']').map_or("", |(_, r)| r).trim_start()
            } else {
                rest
            };
            if let Some(list) = rest.strip_prefix('{').and_then(|r| r.split_once('}')) {
                loaded.extend(list.0.split(',').map(|p| p.trim().to_owned()));
                code = list.1;
            } else {
                break;
            }
        }
    }
    let wanted: &[&'static str] = if table { &["booktabs"] } else { &["graphicx"] };
    wanted.iter().copied().filter(|p| !loaded.iter().any(|l| l == p)).collect()
}

/// Caption source found inside an existing block.
pub(crate) fn extract_caption(format: Format, block: &str) -> Option<String> {
    match format {
        Format::Typst => balanced(block, "caption: [", '[', ']'),
        Format::Latex => balanced(block, "\\caption{", '{', '}'),
        Format::Markdown => {
            if let Some(line) = block.lines().find_map(|l| l.trim_start().strip_prefix("Table: ")) {
                return Some(line.trim_end().to_owned());
            }
            balanced(block, "![", '[', ']').filter(|c| !c.is_empty())
        }
    }
}

/// Text between `opener` (which ends with `open`) and its matching `close`.
fn balanced(text: &str, opener: &str, open: char, close: char) -> Option<String> {
    let start = text.find(opener)? + opener.len();
    let mut depth = 1;
    let mut chars = text[start..].char_indices();
    while let Some((i, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            }
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..start + i].to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn render(
    format: Format,
    key: &str,
    content: &AttachContent,
    caption: Option<&Caption>,
    newline: &str,
) -> String {
    let caption = caption.map(|c| match c {
        Caption::Raw(raw) => raw.clone(),
        Caption::Plain(plain) => match format {
            Format::Typst => escape_typst_markup(plain),
            Format::Latex => escape_latex(plain),
            Format::Markdown => escape_markdown(plain),
        },
    });
    let mut out = vec![marker(format, true, key)];
    match (format, content) {
        (Format::Typst, AttachContent::Figure { path, .. }) => {
            out.push("#figure(".into());
            out.push(format!("  image({}, width: 80%),", typst_string(path)));
            if let Some(caption) = &caption {
                out.push(format!("  caption: [{caption}],"));
            }
            out.push(format!(") <{key}>"));
        }
        (Format::Typst, AttachContent::Table(grid)) => {
            let aligns: Vec<&str> = grid.numeric_columns().iter().map(|n| if *n { "right" } else { "left" }).collect();
            let aligns =
                if aligns.len() == 1 { format!("({},)", aligns[0]) } else { format!("({})", aligns.join(", ")) };
            out.push("#figure(".into());
            out.push("  table(".into());
            out.push(format!("    columns: {},", grid.columns()));
            out.push(format!("    align: {aligns},"));
            out.push(format!("    table.header({}),", typst_cells(&grid.header)));
            for row in &grid.rows {
                out.push(format!("    {},", typst_cells(row)));
            }
            out.push("  ),".into());
            if let Some(caption) = &caption {
                out.push(format!("  caption: [{caption}],"));
            }
            out.push(format!(") <{key}>"));
        }
        (Format::Latex, AttachContent::Figure { path, .. }) => {
            out.push("\\begin{figure}[htbp]".into());
            out.push("  \\centering".into());
            out.push(format!("  \\includegraphics[width=0.8\\linewidth]{{{path}}}"));
            if let Some(caption) = &caption {
                out.push(format!("  \\caption{{{caption}}}"));
            }
            out.push(format!("  \\label{{{key}}}"));
            out.push("\\end{figure}".into());
        }
        (Format::Latex, AttachContent::Table(grid)) => {
            let spec: String = grid.numeric_columns().iter().map(|n| if *n { 'r' } else { 'l' }).collect();
            out.push("\\begin{table}[htbp]".into());
            out.push("  \\centering".into());
            if let Some(caption) = &caption {
                out.push(format!("  \\caption{{{caption}}}"));
            }
            out.push(format!("  \\label{{{key}}}"));
            out.push(format!("  \\begin{{tabular}}{{{spec}}}"));
            out.push("    \\toprule".into());
            out.push(format!("    {} \\\\", latex_row(&grid.header)));
            out.push("    \\midrule".into());
            for row in &grid.rows {
                out.push(format!("    {} \\\\", latex_row(row)));
            }
            out.push("    \\bottomrule".into());
            out.push("  \\end{tabular}".into());
            out.push("\\end{table}".into());
        }
        (Format::Markdown, AttachContent::Figure { path, .. }) => {
            let target = if path.contains([' ', '(', ')', '<', '>']) { format!("<{path}>") } else { path.clone() };
            out.push(format!("![{}]({target})", caption.clone().unwrap_or_default()));
        }
        (Format::Markdown, AttachContent::Table(grid)) => {
            let numeric = grid.numeric_columns();
            out.push(markdown_row(&grid.header));
            out.push(format!(
                "|{}|",
                numeric.iter().map(|n| if *n { " ---: " } else { " --- " }).collect::<Vec<_>>().join("|")
            ));
            for row in &grid.rows {
                out.push(markdown_row(row));
            }
            if let Some(caption) = &caption {
                out.push(String::new());
                out.push(format!("Table: {caption}"));
            }
        }
    }
    out.push(marker(format, false, key));
    out.join(newline)
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

fn typst_string(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in single_line(text).chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn typst_cells(cells: &[String]) -> String {
    cells.iter().map(|c| typst_string(c.trim())).collect::<Vec<_>>().join(", ")
}

/// Escape plain text for Typst markup (inside a content block).
fn escape_typst_markup(text: &str) -> String {
    let text = single_line(text);
    let text = text.trim();
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        let leading_marker = i == 0 && matches!(ch, '-' | '+' | '=');
        if leading_marker || matches!(ch, '\\' | '#' | '[' | ']' | '$' | '*' | '_' | '`' | '<' | '>' | '@' | '~' | '/')
        {
            out.push('\\');
        }
        out.push(ch);
    }
    // "1. text" at the start would become a numbered list.
    let digits = out.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && out[digits..].starts_with('.') {
        out.insert(digits, '\\');
    }
    out
}

fn escape_latex(text: &str) -> String {
    let mut out = String::new();
    for ch in single_line(text).trim().chars() {
        match ch {
            '\\' => out.push_str("\\textbackslash{}"),
            '&' | '%' | '$' | '#' | '_' | '{' | '}' => {
                out.push('\\');
                out.push(ch);
            }
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            '<' => out.push_str("\\textless{}"),
            '>' => out.push_str("\\textgreater{}"),
            '|' => out.push_str("\\textbar{}"),
            other => out.push(other),
        }
    }
    out
}

fn latex_row(cells: &[String]) -> String {
    cells.iter().map(|c| escape_latex(c)).collect::<Vec<_>>().join(" & ")
}

fn escape_markdown(text: &str) -> String {
    let mut out = String::new();
    for ch in single_line(text).trim().chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn markdown_row(cells: &[String]) -> String {
    format!("|{}|", cells.iter().map(|c| format!(" {} ", escape_markdown(c))).collect::<Vec<_>>().join("|"))
}
