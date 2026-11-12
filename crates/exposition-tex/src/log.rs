//! TeX log parsing.
//!
//! Engines run with `-file-line-error` and `max_print_line` raised (see
//! [`crate::compile`]) so file names and messages are not wrapped at 79
//! columns. Errors carry their own `file:line:` prefix; warnings and box
//! messages are attributed to the file on top of the `(file ... )` nesting
//! stack that TeX prints while reading inputs.

use exposition_core::build::Severity;

/// One message extracted from a log. Locations are raw (as printed by TeX);
/// [`crate::compile`] resolves them to workspace paths and UTF-16 ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogMessage {
    pub severity: Severity,
    pub message: String,
    /// File as printed by TeX (e.g. `./main.tex` or an absolute path).
    pub file: Option<String>,
    /// 1-based inclusive line range.
    pub lines: Option<(u32, u32)>,
    /// Extra context (the `l.N ...` source excerpt, package continuation lines).
    pub context: Vec<String>,
    pub kind: LogMessageKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogMessageKind {
    Error,
    UndefinedReference,
    UndefinedCitation,
    Warning,
    BadBox,
    /// `Missing character: There is no X in font F!`: the character is left
    /// out of the page (see [`missing_character`]).
    MissingCharacter,
}

/// The character and font of a [`LogMessageKind::MissingCharacter`] message:
/// `Missing character: There is no 日 (U+65E5) in font [lmroman10-regular]:+tlig;!`
/// (LuaTeX) or `... There is no ^^A in font cmr10!` (pdfTeX).
pub fn missing_character(message: &str) -> Option<(String, String)> {
    let rest = message.strip_prefix("Missing character: There is no ")?;
    let (character, font) = rest.rsplit_once(" in font ")?;
    let character = match character.rsplit_once(" (U+") {
        Some((c, code)) if code.ends_with(')') => c,
        _ => character,
    };
    let font = font.trim_end_matches('!');
    let font = font.split(':').next().unwrap_or(font).trim_matches(|c| c == '[' || c == ']' || c == '"');
    let font = font.rsplit(['/', '\\']).next().unwrap_or(font);
    Some((character.to_owned(), font.to_owned()))
}

/// Parse a complete log.
pub fn parse_log(log: &str) -> Vec<LogMessage> {
    let lines: Vec<&str> = log.lines().collect();
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut messages = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let current = stack.iter().rev().find_map(|f| f.clone());
        if let Some((file, line_no, text)) = split_file_line_error(line) {
            let context = error_context(&lines, i + 1);
            let line_no = context_line(&context).unwrap_or(line_no);
            messages.push(LogMessage {
                severity: Severity::Error,
                message: text.trim().to_owned(),
                file: Some(file.to_owned()),
                lines: Some((line_no, line_no)),
                context,
                kind: LogMessageKind::Error,
            });
        } else if let Some(text) = line.strip_prefix("! ") {
            let context = error_context(&lines, i + 1);
            let line_no = context_line(&context);
            messages.push(LogMessage {
                severity: Severity::Error,
                message: text.trim().to_owned(),
                file: if line_no.is_some() { current.clone() } else { None },
                lines: line_no.map(|l| (l, l)),
                context,
                kind: LogMessageKind::Error,
            });
        } else if let Some((package, rest)) = warning_start(line) {
            // Package warnings continue on lines prefixed with "(package)".
            let mut text = rest.trim().to_owned();
            let mut j = i + 1;
            let continuation = format!("({package})");
            while j < lines.len() && !package.is_empty() && lines[j].starts_with(&continuation) {
                text.push(' ');
                text.push_str(lines[j][continuation.len()..].trim());
                j += 1;
            }
            // LaTeX's own multi-line warnings continue until a blank line.
            if package.is_empty() {
                while j < lines.len() && !lines[j].trim().is_empty() && !text.contains("input line") {
                    text.push(' ');
                    text.push_str(lines[j].trim());
                    j += 1;
                }
            }
            let input_line = input_line_of(&text);
            let kind = if text.starts_with("Reference `") && text.contains("undefined") {
                LogMessageKind::UndefinedReference
            } else if text.starts_with("Citation `") && text.contains("undefined") {
                LogMessageKind::UndefinedCitation
            } else {
                LogMessageKind::Warning
            };
            let message = if package.is_empty() { text } else { format!("{package}: {text}") };
            messages.push(LogMessage {
                severity: Severity::Warning,
                message,
                file: input_line.and(current.clone()),
                lines: input_line.map(|l| (l, l)),
                context: Vec::new(),
                kind,
            });
        } else if line.starts_with("Missing character: There is no ") {
            messages.push(LogMessage {
                severity: Severity::Warning,
                message: line.trim().to_owned(),
                file: None,
                lines: None,
                context: Vec::new(),
                kind: LogMessageKind::MissingCharacter,
            });
        } else if line.starts_with("Overfull \\") || line.starts_with("Underfull \\") {
            let lines_range = box_lines(line);
            messages.push(LogMessage {
                severity: Severity::Info,
                message: line.trim().to_owned(),
                file: lines_range.and(current.clone()),
                lines: lines_range,
                context: lines.get(i + 1).map(|l| vec![l.trim().to_owned()]).unwrap_or_default(),
                kind: LogMessageKind::BadBox,
            });
        }
        track_files(line, &mut stack);
        i += 1;
    }
    messages
}

/// Whether the log asks for another engine pass.
pub fn rerun_requested(log: &str) -> bool {
    log.contains("Rerun to get")
        || log.contains("Label(s) may have changed")
        || log.contains("Please rerun LaTeX")
        || log.contains("Rerun LaTeX")
}

/// `file:line: message` as printed with `-file-line-error`.
fn split_file_line_error(line: &str) -> Option<(&str, u32, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' && i > 0 {
            let digits_end = bytes[i + 1..].iter().position(|b| !b.is_ascii_digit()).map(|p| i + 1 + p);
            if let Some(end) = digits_end
                && end > i + 1
                && bytes.get(end) == Some(&b':')
                && bytes.get(end + 1) == Some(&b' ')
            {
                let file = &line[..i];
                if looks_like_path(file) {
                    let number = line[i + 1..end].parse().ok()?;
                    return Some((file, number, &line[end + 2..]));
                }
            }
        }
        i += 1;
    }
    None
}

fn looks_like_path(text: &str) -> bool {
    !text.contains(' ') && (text.contains('/') || text.contains('\\') || text.contains('.'))
}

fn error_context(lines: &[&str], from: usize) -> Vec<String> {
    let mut context = Vec::new();
    for line in lines.iter().skip(from).take(12) {
        if line.starts_with("l.") {
            context.push(line.trim_end().to_owned());
            break;
        }
    }
    context
}

fn context_line(context: &[String]) -> Option<u32> {
    context.iter().find_map(|l| {
        let rest = l.strip_prefix("l.")?;
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        rest[..end].parse().ok()
    })
}

/// `LaTeX Warning: ...`, `Package foo Warning: ...`, `Class foo Warning: ...`.
fn warning_start(line: &str) -> Option<(String, &str)> {
    if let Some(rest) = line.strip_prefix("LaTeX Warning:") {
        return Some((String::new(), rest));
    }
    for prefix in ["Package ", "Class ", "LaTeX "] {
        if let Some(rest) = line.strip_prefix(prefix)
            && let Some(pos) = rest.find(" Warning:")
        {
            let name = &rest[..pos];
            if !name.contains(' ') {
                return Some((name.to_owned(), &rest[pos + " Warning:".len()..]));
            }
        }
    }
    None
}

fn input_line_of(text: &str) -> Option<u32> {
    let pos = text.find("input line ")?;
    let rest = &text[pos + "input line ".len()..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `... at lines 10--12` or `... detected at line 12`.
fn box_lines(line: &str) -> Option<(u32, u32)> {
    if let Some(pos) = line.find("at lines ") {
        let rest = &line[pos + "at lines ".len()..];
        let mut parts = rest.split("--");
        let a: u32 = parts.next()?.trim().parse().ok()?;
        let b_text = parts.next()?;
        let end = b_text.find(|c: char| !c.is_ascii_digit()).unwrap_or(b_text.len());
        let b: u32 = b_text[..end].parse().ok()?;
        return Some((a.min(b), a.max(b)));
    }
    if let Some(pos) = line.find("at line ") {
        let rest = &line[pos + "at line ".len()..];
        let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let n: u32 = rest[..end].parse().ok()?;
        return Some((n, n));
    }
    None
}

/// Maintain the stack of open input files from `(path` / `)` in a log line.
fn track_files(line: &str, stack: &mut Vec<Option<String>>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '(' => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && !matches!(chars[end], ' ' | '(' | ')' | '[' | '{' | '<') {
                    end += 1;
                }
                let token: String = chars[start..end].iter().collect();
                let is_file = !token.is_empty()
                    && (token.starts_with("./")
                        || token.starts_with('/')
                        || token.starts_with("../")
                        || (token.len() > 2 && token.as_bytes()[1] == b':')
                        || token.ends_with(".tex")
                        || token.ends_with(".sty")
                        || token.ends_with(".cls")
                        || token.ends_with(".aux")
                        || token.ends_with(".bbl"));
                stack.push(is_file.then_some(token));
                i = end;
                continue;
            }
            ')' => {
                stack.pop();
            }
            _ => {}
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R-m23 F10: characters the font cannot show are reported.
    #[test]
    fn missing_characters_are_parsed() {
        let log = "Missing character: There is no 日 (U+65E5) in font [lmroman10-regular]:+tlig;!\nMissing character: There is no ^^A in font cmr10!\n";
        let messages = parse_log(log);
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|m| m.kind == LogMessageKind::MissingCharacter && m.severity == Severity::Warning));
        assert_eq!(missing_character(&messages[0].message), Some(("日".into(), "lmroman10-regular".into())));
        assert_eq!(missing_character(&messages[1].message), Some(("^^A".into(), "cmr10".into())));
    }

    const SAMPLE: &str = r"This is pdfTeX, Version 3.141592653-2.6-1.40.29 (TeX Live 2026)
(./main.tex
LaTeX2e <2026-06-01>
(c:/texlive/texmf-dist/tex/latex/base/article.cls
Document Class: article 2025/01/22 v1.4n Standard LaTeX document class
(c:/texlive/texmf-dist/tex/latex/base/size10.clo
File: size10.clo 2025/01/22 v1.4n Standard LaTeX file (size option)
)
\c@part=\count276
) (./main.aux)
./main.tex:8: Undefined control sequence.
l.8 This line has an \undefinedmacro
                                    {} in it.
The control sequence at the end of the top line

LaTeX Warning: Reference `sec:nowhere' on page 1 undefined on input line 9.


Package natbib Warning: Citation `x' on page 1 undefined on input line 9.

(./sections/part.tex
./sections/part.tex:1: Undefined control sequence.
l.1 An included file with a second error: $x^2 \badmathcmd
                                                           + 1$.

Underfull \hbox (badness 10000) in paragraph at lines 3--5
[]\OT1/cmr/m/n/10 word
)
Overfull \hbox (135.8172pt too wide) detected at line 12
\OT1/cmr/m/n/10 averyveryverylongwordthatcannotpossiblyfit
 []

LaTeX Warning: There were undefined references.

LaTeX Warning: Label(s) may have changed. Rerun to get cross-references right.

 )
";

    #[test]
    fn parses_errors_warnings_and_boxes_with_files() {
        let messages = parse_log(SAMPLE);
        let summary: Vec<_> =
            messages.iter().map(|m| (m.kind, m.file.as_deref(), m.lines, m.message.as_str())).collect();
        assert_eq!(
            summary,
            vec![
                (LogMessageKind::Error, Some("./main.tex"), Some((8, 8)), "Undefined control sequence."),
                (
                    LogMessageKind::UndefinedReference,
                    Some("./main.tex"),
                    Some((9, 9)),
                    "Reference `sec:nowhere' on page 1 undefined on input line 9."
                ),
                (
                    LogMessageKind::UndefinedCitation,
                    Some("./main.tex"),
                    Some((9, 9)),
                    "natbib: Citation `x' on page 1 undefined on input line 9."
                ),
                (LogMessageKind::Error, Some("./sections/part.tex"), Some((1, 1)), "Undefined control sequence."),
                (
                    LogMessageKind::BadBox,
                    Some("./sections/part.tex"),
                    Some((3, 5)),
                    "Underfull \\hbox (badness 10000) in paragraph at lines 3--5"
                ),
                (
                    LogMessageKind::BadBox,
                    Some("./main.tex"),
                    Some((12, 12)),
                    "Overfull \\hbox (135.8172pt too wide) detected at line 12"
                ),
                (LogMessageKind::Warning, None, None, "There were undefined references."),
                (
                    LogMessageKind::Warning,
                    None,
                    None,
                    "Label(s) may have changed. Rerun to get cross-references right."
                ),
            ]
        );
        assert_eq!(messages[0].context[0], r"l.8 This line has an \undefinedmacro");
        assert!(rerun_requested(SAMPLE));
    }

    #[test]
    fn windows_paths_with_drive_letters_split_correctly() {
        assert_eq!(split_file_line_error("c:/proj/ch 1.tex:4: x"), None, "paths with spaces are not file-line errors");
        assert_eq!(
            split_file_line_error("C:/Users/a/proj/main.tex:12: Missing $ inserted."),
            Some(("C:/Users/a/proj/main.tex", 12, "Missing $ inserted."))
        );
        assert_eq!(split_file_line_error("Package: amsmath 2026/05/19 v2.18d"), None);
    }
}
