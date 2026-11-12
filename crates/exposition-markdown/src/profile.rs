//! The fixed print template of the profile and its front matter options.

use crate::front_matter::FrontMatter;

/// Identifier of the declared Markdown print profile. It changes whenever the
/// template or the translation rules change the layout of existing documents.
pub const PROFILE_ID: &str = "exposition-markdown-print/1";

/// Paper sizes accepted in `papersize:` (front matter value → Typst paper).
const PAPERS: &[(&str, &str)] = &[
    ("a3", "a3"),
    ("a4", "a4"),
    ("a5", "a5"),
    ("b5", "iso-b5"),
    ("letter", "us-letter"),
    ("us-letter", "us-letter"),
    ("legal", "us-legal"),
    ("us-legal", "us-legal"),
];

/// Layout settings of one document (defaults overridden by front matter).
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Typst paper name.
    pub paper: &'static str,
    /// Body font size in points.
    pub font_size_pt: f64,
    /// Page margin as a Typst length literal (validated).
    pub margin: String,
    /// Text language (two or three ASCII letters).
    pub lang: String,
}

impl Default for Layout {
    fn default() -> Self {
        Self { paper: "a4", font_size_pt: 11.0, margin: "2.5cm".into(), lang: "en".into() }
    }
}

/// A front matter value the profile could not use.
pub(crate) struct LayoutProblem {
    pub range: std::ops::Range<usize>,
    pub message: String,
}

/// Recognised front matter keys.
pub const KEYS: &[&str] = &["title", "author", "date", "papersize", "fontsize", "margin", "lang"];

pub(crate) fn layout(fm: &FrontMatter) -> (Layout, Vec<LayoutProblem>) {
    let mut layout = Layout::default();
    let mut problems = Vec::new();
    if let Some(s) = fm.scalar("papersize") {
        let key = s.value.trim().to_ascii_lowercase();
        match PAPERS.iter().find(|(k, _)| *k == key) {
            Some((_, paper)) => layout.paper = paper,
            None => problems.push(LayoutProblem {
                range: s.range.clone(),
                message: format!(
                    "unsupported papersize `{}` (use one of {}); using a4",
                    s.value,
                    PAPERS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
                ),
            }),
        }
    }
    if let Some(s) = fm.scalar("fontsize") {
        let v = s.value.trim();
        let number = v.strip_suffix("pt").unwrap_or(v).trim();
        match number.parse::<f64>() {
            Ok(pt) if (6.0..=24.0).contains(&pt) => layout.font_size_pt = pt,
            _ => problems.push(LayoutProblem {
                range: s.range.clone(),
                message: format!("unsupported fontsize `{}` (use 6pt to 24pt); using 11pt", s.value),
            }),
        }
    }
    if let Some(s) = fm.scalar("margin") {
        match length(&s.value) {
            Some(len) => layout.margin = len,
            None => problems.push(LayoutProblem {
                range: s.range.clone(),
                message: format!("unsupported margin `{}` (use e.g. 2cm, 20mm, 1in, 72pt); using 2.5cm", s.value),
            }),
        }
    }
    if let Some(s) = fm.scalar("lang") {
        let v = s.value.trim();
        if (2..=3).contains(&v.len()) && v.chars().all(|c| c.is_ascii_alphabetic()) {
            layout.lang = v.to_ascii_lowercase();
        } else {
            problems.push(LayoutProblem {
                range: s.range.clone(),
                message: format!("unsupported lang `{}` (use an ISO 639 code); using en", s.value),
            });
        }
    }
    (layout, problems)
}

/// A validated Typst length literal (`2cm`, `20mm`, `1in`, `72pt`) within
/// sensible page-margin bounds.
fn length(value: &str) -> Option<String> {
    let v = value.trim();
    let unit = ["cm", "mm", "in", "pt"].into_iter().find(|u| v.ends_with(u))?;
    let number: f64 = v[..v.len() - unit.len()].trim().parse().ok()?;
    let pt = match unit {
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "in" => number * 72.0,
        _ => number,
    };
    (0.0..=216.0).contains(&pt).then(|| format!("{number}{unit}"))
}

/// The template preamble. `document` holds the `#set document(..)` metadata
/// arguments (already rendered as Typst string literals).
pub(crate) fn preamble(layout: &Layout, document_args: &str) -> String {
    let mut s = String::with_capacity(4096);
    s.push_str("// ");
    s.push_str(PROFILE_ID);
    s.push_str(" — generated from Markdown; edit the Markdown source, not this file.\n");
    if !document_args.is_empty() {
        s.push_str("#set document(");
        s.push_str(document_args);
        s.push_str(")\n");
    }
    s.push_str(&format!(
        "#set page(paper: \"{paper}\", margin: {margin}, numbering: \"1\")\n\
         #set text(font: \"New Computer Modern\", size: {size}pt, lang: \"{lang}\")\n",
        paper = layout.paper,
        margin = layout.margin,
        size = layout.font_size_pt,
        lang = layout.lang,
    ));
    s.push_str(TEMPLATE);
    s
}

/// Fixed styles and helper functions of `exposition-markdown-print/1`.
const TEMPLATE: &str = r##"#set par(justify: true)
#show math.equation: set text(font: "New Computer Modern Math")
#show raw: set text(font: "DejaVu Sans Mono", size: 0.85em)
#show raw.where(block: true): it => block(fill: luma(244), inset: 7pt, radius: 2pt, width: 100%, it)
#show raw.where(block: false): it => box(fill: luma(240), inset: (x: 2pt), outset: (y: 2pt), radius: 1pt, it)
#show heading: set block(above: 1.5em, below: 0.9em)
#show heading.where(level: 1): set text(size: 1.45em)
#show heading.where(level: 2): set text(size: 1.25em)
#show heading.where(level: 3): set text(size: 1.1em)
#show link: set text(fill: rgb("#1f4e8c"))
#set table(stroke: 0.5pt + luma(150), inset: 5pt)
#set figure(numbering: none)
#show figure.caption: set text(size: 0.9em)
#let exmd-title(title: none, authors: (), date: none) = {
  set align(center)
  if title != none { block(below: 0.9em, text(size: 1.7em, weight: "bold", title)) }
  if authors.len() > 0 { block(below: 0.5em, text(size: 1.1em, authors.join(", ", last: " and "))) }
  if date != none { block(below: 0.5em, date) }
  v(1.2em)
}
#let exmd-quote(body) = block(inset: (left: 10pt, y: 3pt), stroke: (left: 2pt + luma(170)), body)
#let exmd-rule() = align(center, line(length: 100%, stroke: 0.5pt + luma(140)))
#let exmd-task(done) = box(width: 0.72em, height: 0.72em, stroke: 0.6pt, inset: 0.14em, baseline: 0.06em,
  if done { box(width: 100%, height: 100%, fill: black) })
#let exmd-alert-fill = rgb("#fbe9e7")
#let exmd-alert-ink = rgb("#b3261e")
#let exmd-unsupported(source) = box(fill: exmd-alert-fill, stroke: 0.5pt + exmd-alert-ink, inset: (x: 2pt),
  outset: (y: 2pt), radius: 1pt, text(font: "DejaVu Sans Mono", size: 7.5pt, fill: exmd-alert-ink,
  "unsupported: " + source))
#let exmd-html(source, block: false) = {
  let body = text(font: "DejaVu Sans Mono", size: 0.8em, fill: luma(90), source)
  if block { std.block(stroke: (left: 1pt + luma(170)), inset: (left: 6pt, y: 2pt), body) } else { body }
}
#let exmd-image(path, alt: "") = image(path, alt: alt)
#let exmd-inline-image(path, alt: "") = box(image(path, alt: alt, height: 1.2em))
#let exmd-missing-image(path) = box(fill: exmd-alert-fill, stroke: 0.5pt + exmd-alert-ink, inset: 3pt, radius: 1pt,
  text(font: "DejaVu Sans Mono", size: 7.5pt, fill: exmd-alert-ink, "image not available: " + path))
"##;
