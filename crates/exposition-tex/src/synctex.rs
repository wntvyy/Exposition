//! SyncTeX (`.synctex.gz` / `.synctex`) parsing and queries.
//!
//! Format (SyncTeX version 1): a preamble (`Input:tag:name`, `Output:`,
//! `Magnification:`, `Unit:`, `X Offset:`, `Y Offset:`), then `Content:` with
//! one sheet per page (`{n` ... `}n`) containing nested boxes and point
//! records:
//!
//! | record                     | meaning                         |
//! |----------------------------|---------------------------------|
//! | `[tag,line:h,v:W,H,D` `]`  | vbox start / end                |
//! | `(tag,line:h,v:W,H,D` `)`  | hbox start / end                |
//! | `vtag,line:h,v:W,H,D`      | void vbox                       |
//! | `htag,line:h,v:W,H,D`      | void hbox                       |
//! | `xtag,line:h,v`            | current position                |
//! | `ktag,line:h,v:W`          | kern (also interword glue once a paragraph is set; `h` is its right end) |
//! | `gtag,line:h,v`            | glue                            |
//! | `$tag,line:h,v`            | math on/off                     |
//! | `rtag,line:h,v:W,H,D`      | rule                            |
//! | `fN:h,v`, `<`…`>`          | form reference / form content   |
//!
//! `tag,line` may carry a third `,column` field. Positions are TeX scaled
//! points (times `Unit`, magnified) measured from the top-left corner of the
//! page; this module converts them to PDF points (top-left origin), which match
//! glyph positions extracted from the PDF to within rounding.
//!
//! SyncTeX records boxes and glue/kern/math nodes with the *line* (never the
//! column) of the input that created them. It does not record glyphs. It is
//! line-level navigation, and the input to — not a substitute for — the
//! character-level mapping in [`crate::mapping`].

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::pdf::PageRect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    VBox,
    HBox,
    VoidVBox,
    VoidHBox,
    Current,
    Kern,
    Glue,
    Math,
    Rule,
    Form,
}

impl NodeKind {
    pub fn is_box(self) -> bool {
        matches!(self, NodeKind::VBox | NodeKind::HBox | NodeKind::VoidVBox | NodeKind::VoidHBox)
    }
}

/// One SyncTeX record, positions in PDF points (top-left origin).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncNode {
    pub kind: NodeKind,
    pub tag: u32,
    pub line: u32,
    pub column: Option<i32>,
    /// Reference point (left baseline for boxes; for kerns, the right end).
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub depth: f64,
    /// Index of the enclosing box record on the same sheet.
    pub parent: Option<u32>,
}

impl SyncNode {
    /// Extent of a box record (or the point/segment of other records).
    pub fn rect(&self) -> PageRect {
        match self.kind {
            NodeKind::Kern => PageRect::new(self.x - self.width, self.y, self.x, self.y),
            _ => PageRect::new(self.x, self.y - self.height, self.x + self.width, self.y + self.depth),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSheet {
    /// 1-based page number.
    pub page: u32,
    pub nodes: Vec<SyncNode>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncTex {
    pub version: u32,
    /// Input tag → file name as recorded by the engine.
    pub inputs: BTreeMap<u32, String>,
    pub magnification: f64,
    pub unit: f64,
    pub x_offset: f64,
    pub y_offset: f64,
    pub sheets: Vec<SyncSheet>,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncTexError {
    #[error("could not read SyncTeX data: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed SyncTeX record at line {line}: {text}")]
    Malformed { line: usize, text: String },
    #[error("not a SyncTeX file")]
    NotSyncTex,
}

/// A forward-search hit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardHit {
    pub page: u32,
    pub rect: PageRect,
}

/// A backward-search hit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackwardHit {
    pub tag: u32,
    pub line: u32,
}

/// TeX points → PDF points.
const TEX_PT_TO_BP: f64 = 72.0 / 72.27;

impl SyncTex {
    /// Read `.synctex.gz` (gzip) or plain `.synctex`.
    pub fn read(path: &Path) -> Result<Self, SyncTexError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SyncTexError> {
        let text = if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut decoded = String::new();
            flate2::read::MultiGzDecoder::new(bytes).read_to_string(&mut decoded)?;
            decoded
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        };
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, SyncTexError> {
        let mut out = SyncTex { magnification: 1000.0, unit: 1.0, ..Default::default() };
        let mut lines = text.lines().enumerate();
        let mut seen_header = false;
        // Preamble.
        for (_, line) in lines.by_ref() {
            if let Some(v) = line.strip_prefix("SyncTeX Version:") {
                out.version = v.trim().parse().unwrap_or(1);
                seen_header = true;
            } else if let Some(rest) = line.strip_prefix("Input:") {
                out.add_input(rest);
            } else if let Some(v) = line.strip_prefix("Magnification:") {
                out.magnification = v.trim().parse().unwrap_or(1000.0);
            } else if let Some(v) = line.strip_prefix("Unit:") {
                out.unit = v.trim().parse().unwrap_or(1.0);
            } else if let Some(v) = line.strip_prefix("X Offset:") {
                out.x_offset = v.trim().parse().unwrap_or(0.0);
            } else if let Some(v) = line.strip_prefix("Y Offset:") {
                out.y_offset = v.trim().parse().unwrap_or(0.0);
            } else if line.starts_with("Content:") {
                break;
            }
        }
        if !seen_header {
            return Err(SyncTexError::NotSyncTex);
        }
        let scale = out.unit * out.magnification / 1000.0 / 65536.0 * TEX_PT_TO_BP;
        let mut sheet: Option<SyncSheet> = None;
        let mut stack: Vec<u32> = Vec::new();
        for (number, line) in lines {
            let Some(first) = line.chars().next() else { continue };
            match first {
                '!' => {}
                '{' => {
                    let page = line[1..].trim().parse().unwrap_or(out.sheets.len() as u32 + 1);
                    sheet = Some(SyncSheet { page, nodes: Vec::new() });
                    stack.clear();
                }
                '}' => {
                    if let Some(done) = sheet.take() {
                        out.sheets.push(done);
                    }
                    stack.clear();
                }
                ']' | ')' => {
                    stack.pop();
                }
                '<' | '>' => {}
                '[' | '(' | 'v' | 'h' | 'x' | 'k' | 'g' | '$' | 'r' | 'f' => {
                    let Some(current) = sheet.as_mut() else { continue };
                    let kind = match first {
                        '[' => NodeKind::VBox,
                        '(' => NodeKind::HBox,
                        'v' => NodeKind::VoidVBox,
                        'h' => NodeKind::VoidHBox,
                        'x' => NodeKind::Current,
                        'k' => NodeKind::Kern,
                        'g' => NodeKind::Glue,
                        '$' => NodeKind::Math,
                        'r' => NodeKind::Rule,
                        _ => NodeKind::Form,
                    };
                    let mut node = parse_record(kind, &line[1..], scale, out.x_offset, out.y_offset)
                        .ok_or_else(|| SyncTexError::Malformed { line: number + 1, text: line.to_owned() })?;
                    node.parent = stack.last().copied();
                    let index = current.nodes.len() as u32;
                    current.nodes.push(node);
                    if matches!(kind, NodeKind::VBox | NodeKind::HBox) {
                        stack.push(index);
                    }
                }
                'I' => {
                    if let Some(rest) = line.strip_prefix("Input:") {
                        out.add_input(rest);
                    }
                }
                'P' if line.starts_with("Postamble:") => break,
                _ => {}
            }
        }
        if let Some(done) = sheet.take() {
            out.sheets.push(done);
        }
        Ok(out)
    }

    fn add_input(&mut self, rest: &str) {
        if let Some((tag, name)) = rest.split_once(':')
            && let Ok(tag) = tag.trim().parse()
        {
            self.inputs.insert(tag, name.to_owned());
        }
    }

    pub fn sheet(&self, page: u32) -> Option<&SyncSheet> {
        self.sheets.iter().find(|s| s.page == page)
    }

    /// Tags whose recorded file name satisfies `matches`.
    pub fn tags_where(&self, mut matches: impl FnMut(&str) -> bool) -> Vec<u32> {
        self.inputs.iter().filter(|(_, name)| matches(name)).map(|(tag, _)| *tag).collect()
    }

    /// Forward search: page regions produced by `line` of input `tag`.
    ///
    /// Box records of that line contribute their extent; point records
    /// (glue/kern/math/current) contribute horizontal spans within their
    /// enclosing box, so a source line inside a paragraph yields one rectangle
    /// per typeset line it contributes to.
    pub fn forward(&self, tag: u32, line: u32) -> Vec<ForwardHit> {
        let mut hits = Vec::new();
        for sheet in &self.sheets {
            let mut spans: BTreeMap<Option<u32>, (f64, f64)> = BTreeMap::new();
            for node in &sheet.nodes {
                if node.tag != tag || node.line != line {
                    continue;
                }
                if node.kind == NodeKind::HBox && node.width > 0.0 {
                    hits.push(ForwardHit { page: sheet.page, rect: node.rect() });
                } else if !node.kind.is_box() && node.kind != NodeKind::Form {
                    let r = node.rect();
                    let entry = spans.entry(node.parent).or_insert((r.x0, r.x1));
                    entry.0 = entry.0.min(r.x0);
                    entry.1 = entry.1.max(r.x1);
                }
            }
            for (parent, (x0, x1)) in spans {
                let Some(parent) = parent.and_then(|p| sheet.nodes.get(p as usize)) else { continue };
                let pr = parent.rect();
                hits.push(ForwardHit { page: sheet.page, rect: PageRect::new(x0, pr.y0, x1.max(x0 + 1.0), pr.y1) });
            }
        }
        // Drop boxes that contain other hits on the same page (keep the most specific).
        let specific: Vec<ForwardHit> = hits
            .iter()
            .filter(|h| {
                !hits.iter().any(|o| {
                    o.page == h.page
                        && o != *h
                        && h.rect.contains(o.rect.x0, o.rect.y0)
                        && h.rect.contains(o.rect.x1, o.rect.y1)
                        && o.rect.height() < h.rect.height()
                })
            })
            .cloned()
            .collect();
        if specific.is_empty() { hits } else { specific }
    }

    /// Backward search: the input line that produced the content at a page
    /// point (PDF points, top-left origin).
    pub fn backward(&self, page: u32, x: f64, y: f64) -> Option<BackwardHit> {
        let sheet = self.sheet(page)?;
        // Innermost hbox containing the point.
        let mut best: Option<(usize, f64)> = None;
        for (i, node) in sheet.nodes.iter().enumerate() {
            if node.kind == NodeKind::HBox && node.width > 0.0 && node.rect().contains(x, y) {
                let area = node.width * (node.height + node.depth).max(0.1);
                if best.is_none_or(|(_, a)| area < a) {
                    best = Some((i, area));
                }
            }
        }
        let (container, _) = best?;
        // Within it, the nearest point record at or right of x (a word belongs
        // to the line of the space that ends it), else the nearest overall.
        let mut right: Option<(f64, &SyncNode)> = None;
        let mut any: Option<(f64, &SyncNode)> = None;
        for node in sheet.nodes.iter().filter(|n| self.is_within(sheet, n, container as u32)) {
            if node.kind.is_box() || node.kind == NodeKind::Form {
                continue;
            }
            let r = node.rect();
            let dx = r.x1 - x;
            if dx >= 0.0 && right.is_none_or(|(d, _)| dx < d) {
                right = Some((dx, node));
            }
            let d = dx.abs();
            if any.is_none_or(|(b, _)| d < b) {
                any = Some((d, node));
            }
        }
        let node = right.or(any).map(|(_, n)| n).unwrap_or(&sheet.nodes[container]);
        Some(BackwardHit { tag: node.tag, line: node.line })
    }

    fn is_within(&self, sheet: &SyncSheet, node: &SyncNode, ancestor: u32) -> bool {
        let mut parent = node.parent;
        while let Some(p) = parent {
            if p == ancestor {
                return true;
            }
            parent = sheet.nodes.get(p as usize).and_then(|n| n.parent);
        }
        false
    }
}

fn parse_record(kind: NodeKind, body: &str, scale: f64, x_offset: f64, y_offset: f64) -> Option<SyncNode> {
    let mut parts = body.split(':');
    let link = parts.next()?;
    let mut link_parts = link.split(',');
    let (tag, line, column) = if kind == NodeKind::Form {
        (link_parts.next()?.trim().parse().ok()?, 0, None)
    } else {
        let tag = link_parts.next()?.trim().parse().ok()?;
        let line = link_parts.next()?.trim().parse::<i64>().ok()?.max(0) as u32;
        let column = link_parts.next().and_then(|c| c.trim().parse().ok());
        (tag, line, column)
    };
    let point = parts.next()?;
    let (h, v) = point.split_once(',')?;
    let h: f64 = h.trim().parse().ok()?;
    let v: f64 = v.trim().parse().ok()?;
    let mut size = [0.0f64; 3];
    if let Some(dims) = parts.next() {
        for (slot, value) in size.iter_mut().zip(dims.split(',')) {
            *slot = value.trim().parse().ok()?;
        }
    }
    Some(SyncNode {
        kind,
        tag,
        line,
        column,
        x: (h + x_offset) * scale,
        y: (v + y_offset) * scale,
        width: size[0] * scale,
        height: size[1] * scale,
        depth: size[2] * scale,
        parent: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "SyncTeX Version:1
Input:1:C:/proj/./main.tex
Input:2:C:/proj/sections/a.tex
Output:pdf
Magnification:1000
Unit:1
X Offset:0
Y Offset:0
Content:
!100
{1
[1,30:4736286,45984660:27131904,41248374,0
(1,23:8275230,18413965:23592960,498346,139537
x1,23:9511131,18413965
k1,14:9681765,18413965:170634
x1,14:10923647,18413965
k1,15:11094282,18413965:170635
k1,15:31868190,18413965:0
)
(2,7:8275230,19305255:23592960,498346,139537
g2,7:9917783,19305255
$2,7:10211423,19305255
)
h1,40:8275230,30000000:100000,0,0
]
}1
Input:3:C:/proj/late.tex
{2
(3,1:8275230,18413965:23592960,498346,139537
)
}2
Postamble:
Count:12
";

    #[test]
    fn parses_preamble_sheets_and_nested_records() {
        let s = SyncTex::parse(SAMPLE).unwrap();
        assert_eq!(s.inputs.len(), 3);
        assert_eq!(s.inputs[&2], "C:/proj/sections/a.tex");
        assert_eq!(s.sheets.len(), 2);
        let sheet = &s.sheets[0];
        assert_eq!(sheet.page, 1);
        assert_eq!(sheet.nodes.len(), 11);
        assert_eq!(sheet.nodes[0].kind, NodeKind::VBox);
        assert_eq!(sheet.nodes[1].parent, Some(0));
        assert_eq!(sheet.nodes[3].kind, NodeKind::Kern);
        assert_eq!(sheet.nodes[3].parent, Some(1));
        assert_eq!(sheet.nodes[10].kind, NodeKind::VoidHBox);
        assert_eq!(sheet.nodes[10].parent, Some(0));
        // 1in = 4736286sp = 72bp.
        assert!((sheet.nodes[0].x - 72.0).abs() < 1e-4);
        // Kern rect spans to the left of its recorded (right-end) position.
        let k = sheet.nodes[3].rect();
        assert!(k.x1 > k.x0);
        assert!((k.x1 - 9681765.0 / 65536.0 * TEX_PT_TO_BP).abs() < 1e-9);
    }

    #[test]
    fn forward_and_backward_queries() {
        let s = SyncTex::parse(SAMPLE).unwrap();
        // Line 14 contributes a span inside the first paragraph line box.
        let hits = s.forward(1, 14);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page, 1);
        let parent = s.sheets[0].nodes[1].rect();
        assert!((hits[0].rect.y0 - parent.y0).abs() < 1e-9 && (hits[0].rect.y1 - parent.y1).abs() < 1e-9);
        // A whole box line.
        let hits = s.forward(3, 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].page, 2);
        // Backward: a point inside the first line, left of the first kern → line 14.
        let y = s.sheets[0].nodes[1].y - 2.0;
        let x = 9600000.0 / 65536.0 * TEX_PT_TO_BP;
        assert_eq!(s.backward(1, x, y), Some(BackwardHit { tag: 1, line: 14 }));
        // Right of the second kern → line 15 (the trailing zero-width kern).
        let x = 20000000.0 / 65536.0 * TEX_PT_TO_BP;
        assert_eq!(s.backward(1, x, y), Some(BackwardHit { tag: 1, line: 15 }));
        // Second box belongs to input 2.
        let y2 = s.sheets[0].nodes[7].y - 1.0;
        assert_eq!(s.backward(1, 130.0, y2).map(|h| h.tag), Some(2));
        assert_eq!(s.backward(1, 10.0, 10.0), None);
    }

    #[test]
    fn rejects_non_synctex_input() {
        assert!(matches!(SyncTex::parse("hello"), Err(SyncTexError::NotSyncTex)));
        assert!(matches!(
            SyncTex::parse("SyncTeX Version:1\nContent:\n{1\n(1,a:b\n}1\n"),
            Err(SyncTexError::Malformed { .. })
        ));
    }
}
