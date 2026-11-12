//! The page map: rendered glyph clusters linked to source ranges, organised
//! into lines for hit testing, caret placement, and navigation.
//!
//! A format adapter produces [`Cluster`]s for every page of one build (one
//! immutable snapshot); [`PageMap::build`] groups them into lines and indexes
//! their caret stops. All offsets are UTF-16 code units over the snapshot text
//! of the cluster's file, and all geometry is in typographic points with the
//! origin at the top-left corner of the page.

use std::collections::HashMap;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Why rendered content cannot be edited on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum NonEditableReason {
    /// Produced by the compiler or a template (numbering, markers, supplements, hyphens).
    Generated,
    /// Produced by code (`#` expressions, strings, function results).
    Code,
    /// Text changed by a show rule or function (e.g. `upper`) so its glyphs no
    /// longer correspond one-to-one to the source.
    Transformed,
    /// Raw/verbatim text.
    Raw,
    /// Content from outside the workspace (e.g. a package).
    External,
}

/// How typed input is interpreted at a caret position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum EditMode {
    /// Prose/markup.
    Markup,
    /// Mathematics.
    Math,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClusterKind {
    Editable(EditMode),
    NonEditable(NonEditableReason),
}

/// A caret stop inside a cluster: a horizontal position and the source offset
/// a caret there corresponds to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub x: f64,
    pub offset: u32,
    /// Byte offset into the cluster's rendered text at this stop.
    pub text_byte: u32,
}

/// Where a cluster comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClusterSource {
    /// Index into [`PageMap::files`].
    pub file: u32,
    pub range: TextRange,
}

/// The smallest rendered unit with its own source mapping: one or more glyphs
/// (a ligature, a grapheme, an escape sequence, a math identifier, ...).
#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    pub x0: f64,
    pub x1: f64,
    pub top: f64,
    pub bottom: f64,
    pub baseline: f64,
    /// Rendered text (what a reader sees and copies).
    pub text: String,
    pub source: Option<ClusterSource>,
    pub kind: ClusterKind,
    /// Caret stops of editable clusters, ordered by offset (and x, left to
    /// right). At least two (start and end); intermediate stops subdivide a
    /// glyph that covers several characters (ligatures) proportionally.
    pub stops: Vec<Stop>,
    /// Index into [`PageMap::equations`] when the cluster is mathematics.
    pub equation: Option<u32>,
}

impl Cluster {
    pub fn is_editable(&self) -> bool {
        matches!(self.kind, ClusterKind::Editable(_)) && self.stops.len() >= 2 && self.source.is_some()
    }

    pub fn mode(&self) -> Option<EditMode> {
        match self.kind {
            ClusterKind::Editable(mode) => Some(mode),
            ClusterKind::NonEditable(_) => None,
        }
    }

    fn center_y(&self) -> f64 {
        (self.top + self.bottom) / 2.0
    }

    fn height(&self) -> f64 {
        (self.bottom - self.top).max(0.0)
    }
}

/// An equation in the source, used to keep its glyphs together on a line and
/// to outline the active equation while the caret is inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquationInfo {
    pub file: u32,
    /// UTF-16 range of the whole equation including its `$` delimiters.
    pub range: TextRange,
    pub block: bool,
}

/// Which side of a caret stop a position belongs to when several clusters
/// share an offset (e.g. the end of one line and the start of the next).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Affinity {
    /// Belongs to the cluster that ends at the offset (left of the caret).
    Upstream,
    /// Belongs to the cluster that starts at the offset (right of the caret).
    Downstream,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    Inner,
    End,
}

/// A caret stop in navigation order on its line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavStop {
    pub x: f64,
    pub offset: u32,
    pub file: u32,
    /// Index of the cluster in its line.
    pub cluster: u32,
    /// Index of the stop within the cluster.
    pub index: u32,
    pub edge: Edge,
    pub mode: EditMode,
    pub equation: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub top: f64,
    pub bottom: f64,
    pub baseline: f64,
    pub x0: f64,
    pub x1: f64,
    /// Clusters in navigation order (left to right; mathematics of one
    /// equation grouped in source order).
    pub clusters: Vec<Cluster>,
    /// Caret stops of the editable clusters, in navigation order.
    pub stops: Vec<NavStop>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageLayout {
    pub width: f64,
    pub height: f64,
    /// Lines in reading order.
    pub lines: Vec<Line>,
}

/// Location of a stop in the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StopRef {
    pub page: u32,
    pub line: u32,
    pub stop: u32,
}

#[derive(Clone, Debug)]
pub struct PageMap {
    pub files: Vec<WorkspacePath>,
    pub equations: Vec<EquationInfo>,
    pub pages: Vec<PageLayout>,
    /// Per file: every stop sorted by offset.
    by_offset: Vec<Vec<(u32, StopRef)>>,
    /// Lines in global reading order as (page, line).
    order: Vec<(u32, u32)>,
}

/// One page of clusters as produced by an adapter.
#[derive(Clone, Debug, Default)]
pub struct RawPage {
    pub width: f64,
    pub height: f64,
    pub clusters: Vec<Cluster>,
}

impl PageMap {
    /// Group clusters into lines and index caret stops.
    pub fn build(files: Vec<WorkspacePath>, equations: Vec<EquationInfo>, pages: Vec<RawPage>) -> Self {
        let pages: Vec<PageLayout> = pages
            .into_iter()
            .map(|page| PageLayout {
                width: page.width,
                height: page.height,
                lines: build_lines(page.clusters, &equations),
            })
            .collect();
        let mut by_offset = vec![Vec::new(); files.len()];
        let mut order = Vec::new();
        for (p, page) in pages.iter().enumerate() {
            for (l, line) in page.lines.iter().enumerate() {
                order.push((p as u32, l as u32));
                for (s, stop) in line.stops.iter().enumerate() {
                    if let Some(list) = by_offset.get_mut(stop.file as usize) {
                        list.push((stop.offset, StopRef { page: p as u32, line: l as u32, stop: s as u32 }));
                    }
                }
            }
        }
        for list in &mut by_offset {
            list.sort_by(|a, b| {
                a.0.cmp(&b.0).then_with(|| (a.1.page, a.1.line, a.1.stop).cmp(&(b.1.page, b.1.line, b.1.stop)))
            });
        }
        Self { files, equations, pages, by_offset, order }
    }

    pub fn file_index(&self, path: &WorkspacePath) -> Option<u32> {
        self.files.iter().position(|f| f == path).map(|i| i as u32)
    }

    pub fn line(&self, page: u32, line: u32) -> Option<&Line> {
        self.pages.get(page as usize)?.lines.get(line as usize)
    }

    pub fn stop(&self, at: StopRef) -> Option<&NavStop> {
        self.line(at.page, at.line)?.stops.get(at.stop as usize)
    }

    pub fn cluster_of(&self, at: StopRef) -> Option<&Cluster> {
        let line = self.line(at.page, at.line)?;
        line.clusters.get(line.stops.get(at.stop as usize)?.cluster as usize)
    }

    /// All stops of `file`, sorted by offset.
    pub fn stops_by_offset(&self, file: u32) -> &[(u32, StopRef)] {
        self.by_offset.get(file as usize).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Lines in global reading order as (page, line).
    pub fn reading_order(&self) -> &[(u32, u32)] {
        &self.order
    }

    pub fn reading_index(&self, page: u32, line: u32) -> Option<usize> {
        self.order.iter().position(|&(p, l)| p == page && l == line)
    }

    /// Count of editable and non-editable clusters (diagnostics/tests).
    pub fn cluster_counts(&self) -> (usize, usize) {
        let mut editable = 0;
        let mut other = 0;
        for line in self.pages.iter().flat_map(|p| &p.lines) {
            for cluster in &line.clusters {
                if cluster.is_editable() {
                    editable += 1;
                } else {
                    other += 1;
                }
            }
        }
        (editable, other)
    }
}

/// Fraction of the smaller box that must overlap a line's band to join it.
const LINE_OVERLAP: f64 = 0.5;

fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

struct LineBuilder {
    /// Band of the line's main (non-math) text.
    top: f64,
    bottom: f64,
    baseline: f64,
    members: Vec<usize>,
}

fn build_lines(clusters: Vec<Cluster>, equations: &[EquationInfo]) -> Vec<Line> {
    // 1. Text lines from non-math clusters, grouped by vertical overlap.
    let mut text: Vec<usize> = (0..clusters.len()).filter(|&i| clusters[i].equation.is_none()).collect();
    text.sort_by(|&a, &b| clusters[a].baseline.total_cmp(&clusters[b].baseline));
    let mut lines: Vec<LineBuilder> = Vec::new();
    for i in text {
        let c = &clusters[i];
        let h = c.height().max(0.01);
        // Lines are produced in baseline order, so only recent lines can match.
        let found = lines.iter_mut().rev().take(8).find(|line| {
            let band = (line.bottom - line.top).max(0.01);
            overlap(c.top, c.bottom, line.top, line.bottom) >= LINE_OVERLAP * h.min(band)
        });
        match found {
            Some(line) => {
                // Superscripts and markers join without widening the band much.
                if h >= (line.bottom - line.top) * 0.8 {
                    line.top = line.top.min(c.top);
                    line.bottom = line.bottom.max(c.bottom);
                }
                line.members.push(i);
            }
            None => lines.push(LineBuilder { top: c.top, bottom: c.bottom, baseline: c.baseline, members: vec![i] }),
        }
    }

    // 2. Attach mathematics: an equation whose glyphs sit on text lines is
    //    inline (each glyph joins the nearest of those lines); otherwise its
    //    glyphs form their own (display) line.
    let mut by_equation: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, c) in clusters.iter().enumerate() {
        if let Some(e) = c.equation {
            by_equation.entry(e).or_default().push(i);
        }
    }
    let mut equation_ids: Vec<u32> = by_equation.keys().copied().collect();
    equation_ids.sort_unstable();
    for e in equation_ids {
        let members = &by_equation[&e];
        let block = equations.get(e as usize).is_some_and(|eq| eq.block);
        let host_of = |i: usize, lines: &[LineBuilder]| -> Option<usize> {
            let c = &clusters[i];
            let cy = c.center_y();
            lines.iter().position(|line| {
                let (x0, x1) = line_x_extent(line, &clusters);
                let margin = (line.bottom - line.top).max(1.0) * 2.0;
                cy >= line.top && cy <= line.bottom && c.x1 >= x0 - margin && c.x0 <= x1 + margin
            })
        };
        let hosts: Vec<usize> = if block {
            Vec::new()
        } else {
            let mut hosts: Vec<usize> = members.iter().filter_map(|&i| host_of(i, &lines)).collect();
            hosts.sort_unstable();
            hosts.dedup();
            hosts
        };
        if hosts.is_empty() {
            let top = members.iter().map(|&i| clusters[i].top).fold(f64::INFINITY, f64::min);
            let bottom = members.iter().map(|&i| clusters[i].bottom).fold(f64::NEG_INFINITY, f64::max);
            // The baseline of a display equation: that of its tallest glyph run.
            let baseline = members
                .iter()
                .max_by(|&&a, &&b| clusters[a].height().total_cmp(&clusters[b].height()))
                .map(|&i| clusters[i].baseline)
                .unwrap_or(bottom);
            lines.push(LineBuilder { top, bottom, baseline, members: members.clone() });
        } else {
            for &i in members {
                let cy = clusters[i].center_y();
                let host = hosts
                    .iter()
                    .copied()
                    .min_by(|&a, &b| {
                        let da = band_distance(cy, &lines[a]);
                        let db = band_distance(cy, &lines[b]);
                        da.total_cmp(&db)
                    })
                    .expect("hosts is non-empty");
                lines[host].members.push(i);
            }
        }
    }

    // 3. Split lines at large horizontal gaps (columns, side-by-side boxes).
    let mut split: Vec<LineBuilder> = Vec::new();
    for mut line in lines {
        line.members.sort_by(|&a, &b| clusters[a].x0.total_cmp(&clusters[b].x0));
        let gap_limit = 2.0 * (line.bottom - line.top).max(1.0);
        let mut current: Vec<usize> = Vec::new();
        let mut right = f64::NEG_INFINITY;
        for i in line.members.drain(..) {
            if !current.is_empty() && clusters[i].x0 - right > gap_limit {
                split.push(LineBuilder {
                    top: line.top,
                    bottom: line.bottom,
                    baseline: line.baseline,
                    members: std::mem::take(&mut current),
                });
                right = f64::NEG_INFINITY;
            }
            right = right.max(clusters[i].x1);
            current.push(i);
        }
        if !current.is_empty() {
            split.push(LineBuilder { top: line.top, bottom: line.bottom, baseline: line.baseline, members: current });
        }
    }
    let mut lines = split;

    // 4. Reading order: top to bottom; lines sharing a visual row (overlapping
    //    bands, e.g. after a column split) left to right.
    lines.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut ordered: Vec<LineBuilder> = Vec::with_capacity(lines.len());
    let mut row: Vec<LineBuilder> = Vec::new();
    let flush = |row: &mut Vec<LineBuilder>, ordered: &mut Vec<LineBuilder>| {
        row.sort_by(|a, b| line_x_extent(a, &clusters).0.total_cmp(&line_x_extent(b, &clusters).0));
        ordered.append(row);
    };
    for line in lines {
        if let Some(first) = row.first() {
            let same_row = overlap(first.top, first.bottom, line.top, line.bottom)
                >= LINE_OVERLAP * (first.bottom - first.top).min(line.bottom - line.top).max(0.01);
            if !same_row {
                flush(&mut row, &mut ordered);
            }
        }
        row.push(line);
    }
    flush(&mut row, &mut ordered);

    let mut slots: Vec<Option<Cluster>> = clusters.into_iter().map(Some).collect();
    ordered
        .into_iter()
        .map(|builder| {
            let mut members: Vec<Cluster> = builder.members.iter().filter_map(|&i| slots[i].take()).collect();
            order_clusters(&mut members);
            finish_line(members, builder.baseline)
        })
        .filter(|line| !line.clusters.is_empty())
        .collect()
}

fn band_distance(y: f64, line: &LineBuilder) -> f64 {
    if y < line.top {
        line.top - y
    } else if y > line.bottom {
        y - line.bottom
    } else {
        0.0
    }
}

fn line_x_extent(line: &LineBuilder, clusters: &[Cluster]) -> (f64, f64) {
    let x0 = line.members.iter().map(|&i| clusters[i].x0).fold(f64::INFINITY, f64::min);
    let x1 = line.members.iter().map(|&i| clusters[i].x1).fold(f64::NEG_INFINITY, f64::max);
    (x0, x1)
}

/// Navigation order: left to right, except that the clusters of one equation
/// stay together in source order, placed at the equation's leftmost position.
fn order_clusters(members: &mut [Cluster]) {
    let mut equation_x: HashMap<u32, f64> = HashMap::new();
    for c in members.iter() {
        if let Some(e) = c.equation {
            let x = equation_x.entry(e).or_insert(f64::INFINITY);
            *x = x.min(c.x0);
        }
    }
    members.sort_by(|a, b| {
        let key = |c: &Cluster| match c.equation {
            Some(e) => (equation_x[&e], c.source.map_or(u32::MAX, |s| s.range.start), c.x0),
            None => (c.x0, 0, c.x0),
        };
        let (ax, asrc, ax0) = key(a);
        let (bx, bsrc, bx0) = key(b);
        ax.total_cmp(&bx).then(asrc.cmp(&bsrc)).then(ax0.total_cmp(&bx0))
    });
}

fn finish_line(clusters: Vec<Cluster>, baseline: f64) -> Line {
    let top = clusters.iter().map(|c| c.top).fold(f64::INFINITY, f64::min);
    let bottom = clusters.iter().map(|c| c.bottom).fold(f64::NEG_INFINITY, f64::max);
    let x0 = clusters.iter().map(|c| c.x0).fold(f64::INFINITY, f64::min);
    let x1 = clusters.iter().map(|c| c.x1).fold(f64::NEG_INFINITY, f64::max);
    let mut stops = Vec::new();
    for (ci, cluster) in clusters.iter().enumerate() {
        let (Some(source), Some(mode)) = (cluster.source, cluster.mode()) else { continue };
        if !cluster.is_editable() {
            continue;
        }
        let last = cluster.stops.len() - 1;
        for (si, stop) in cluster.stops.iter().enumerate() {
            let edge = if si == 0 {
                Edge::Start
            } else if si == last {
                Edge::End
            } else {
                Edge::Inner
            };
            stops.push(NavStop {
                x: stop.x,
                offset: stop.offset,
                file: source.file,
                cluster: ci as u32,
                index: si as u32,
                edge,
                mode,
                equation: cluster.equation,
            });
        }
    }
    Line { top, bottom, baseline, x0, x1, clusters, stops }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A simple editable cluster of `text` at x with one stop per char.
    pub fn text_cluster(x: f64, baseline: f64, text: &str, start: u32, equation: Option<u32>) -> Cluster {
        let advance = 5.0;
        let mut stops = Vec::new();
        let mut byte = 0u32;
        for (i, ch) in text.chars().enumerate() {
            stops.push(Stop { x: x + advance * i as f64, offset: start + i as u32, text_byte: byte });
            byte += ch.len_utf8() as u32;
        }
        let n = text.chars().count();
        stops.push(Stop { x: x + advance * n as f64, offset: start + n as u32, text_byte: byte });
        Cluster {
            x0: x,
            x1: x + advance * n as f64,
            top: baseline - 8.0,
            bottom: baseline + 2.0,
            baseline,
            text: text.to_owned(),
            source: Some(ClusterSource { file: 0, range: TextRange::new(start, start + n as u32) }),
            kind: ClusterKind::Editable(if equation.is_some() { EditMode::Math } else { EditMode::Markup }),
            stops,
            equation,
        }
    }

    #[test]
    fn groups_lines_and_attaches_inline_math() {
        let path = WorkspacePath::parse("main.typ").unwrap();
        let equations = vec![
            EquationInfo { file: 0, range: TextRange::new(6, 9), block: false },
            EquationInfo { file: 0, range: TextRange::new(30, 40), block: true },
        ];
        let mut raised = text_cluster(40.0, 17.0, "2", 7, Some(0));
        raised.top = 9.0;
        raised.bottom = 16.0;
        let clusters = vec![
            text_cluster(10.0, 20.0, "hello ", 0, None),
            text_cluster(35.0, 20.0, "x", 6, Some(0)),
            raised,
            text_cluster(10.0, 34.0, "next", 12, None),
            text_cluster(30.0, 60.0, "y", 31, Some(1)),
        ];
        let map = PageMap::build(vec![path], equations, vec![RawPage { width: 100.0, height: 100.0, clusters }]);
        let lines = &map.pages[0].lines;
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].clusters.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["hello ", "x", "2"]);
        assert_eq!(lines[1].clusters[0].text, "next");
        assert_eq!(lines[2].clusters[0].text, "y");
        assert_eq!(map.stops_by_offset(0).first().unwrap().0, 0);
    }
}
