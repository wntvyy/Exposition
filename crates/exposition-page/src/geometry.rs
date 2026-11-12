//! Geometry queries over a [`PageMap`]: hit testing, caret placement,
//! keyboard navigation over rendered characters, and selection highlights.
//!
//! Every query is relative to the build the map came from: offsets are UTF-16
//! positions in that build's snapshot text. Callers map offsets between
//! document revisions before asking.

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::map::{Affinity, Cluster, Edge, EditMode, NavStop, NonEditableReason, PageMap, StopRef};

/// A rectangle on a page, in points from the page's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PageRect {
    pub page: u32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A caret anchored to source: a UTF-16 offset in a file of the build snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CaretPosition {
    pub path: WorkspacePath,
    pub offset: u32,
    pub affinity: Affinity,
}

/// Result of a click on a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PageHit {
    /// Nearest editable caret position (none when the page has no editable text).
    pub caret: Option<CaretPosition>,
    pub mode: Option<EditMode>,
    /// Set when the point is directly over content that cannot be edited on the page.
    pub non_editable: Option<NonEditableReason>,
}

/// Where to draw a caret.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CaretGeometry {
    pub page: u32,
    pub x: f64,
    pub top: f64,
    pub bottom: f64,
    pub mode: EditMode,
    /// The offset of the stop the caret was placed at (equals the requested
    /// offset when `exact`).
    pub offset: u32,
    pub affinity: Affinity,
    /// False when the offset has no caret stop (inside markup syntax or a
    /// region changed since this build) and the nearest stop was used.
    pub exact: bool,
    /// Outline of the equation containing the caret, if any.
    pub equation: Vec<PageRect>,
    /// Rendered text of the caret's line (for assistive technology).
    pub line_text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct NavigationResult {
    pub caret: CaretPosition,
    /// Horizontal goal to keep across consecutive vertical moves.
    pub goal_x: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SelectionGeometry {
    pub rects: Vec<PageRect>,
    /// Rendered text of the selection (for copy).
    pub text: String,
}

const SAME_X: f64 = 0.01;

fn same_visual(a: &NavStop, b: &NavStop) -> bool {
    (a.x - b.x).abs() < SAME_X && a.mode == b.mode && a.equation == b.equation && a.file == b.file
}

fn affinity_of(stop: &NavStop) -> Affinity {
    match stop.edge {
        Edge::End => Affinity::Upstream,
        Edge::Start | Edge::Inner => Affinity::Downstream,
    }
}

fn box_distance(c: &Cluster, x: f64, y: f64) -> f64 {
    let dx = if x < c.x0 {
        c.x0 - x
    } else if x > c.x1 {
        x - c.x1
    } else {
        0.0
    };
    let dy = if y < c.top {
        c.top - y
    } else if y > c.bottom {
        y - c.bottom
    } else {
        0.0
    };
    (dx * dx + dy * dy).sqrt()
}

impl PageMap {
    fn caret_at(&self, at: StopRef) -> Option<CaretPosition> {
        let stop = self.stop(at)?;
        Some(CaretPosition {
            path: self.files.get(stop.file as usize)?.clone(),
            offset: stop.offset,
            affinity: affinity_of(stop),
        })
    }

    /// Find the stop for a caret. Returns the stop and whether it matches the
    /// offset exactly.
    pub fn locate(&self, path: &WorkspacePath, offset: u32, affinity: Affinity) -> Option<(StopRef, bool)> {
        let file = self.file_index(path)?;
        let list = self.stops_by_offset(file);
        if list.is_empty() {
            return None;
        }
        let lo = list.partition_point(|(o, _)| *o < offset);
        let hi = list.partition_point(|(o, _)| *o <= offset);
        if lo < hi {
            let preference = |r: &StopRef| {
                let stop = self.stop(*r).expect("indexed stop exists");
                match (affinity, stop.edge) {
                    (Affinity::Upstream, Edge::End) | (Affinity::Downstream, Edge::Start) | (_, Edge::Inner) => 0,
                    _ => 1,
                }
            };
            let best = list[lo..hi].iter().min_by_key(|(_, r)| preference(r)).expect("non-empty");
            return Some((best.1, true));
        }
        let before = lo.checked_sub(1).map(|i| list[i]);
        let after = list.get(lo).copied();
        let pick = match (before, after) {
            (Some(b), Some(a)) => {
                if offset - b.0 <= a.0 - offset {
                    b
                } else {
                    a
                }
            }
            (Some(b), None) => b,
            (None, Some(a)) => a,
            (None, None) => return None,
        };
        Some((pick.1, false))
    }

    /// Where to draw the caret for a source position.
    pub fn caret_geometry(&self, path: &WorkspacePath, offset: u32, affinity: Affinity) -> Option<CaretGeometry> {
        let (at, exact) = self.locate(path, offset, affinity)?;
        let line = self.line(at.page, at.line)?;
        let stop = line.stops.get(at.stop as usize)?;
        let cluster = line.clusters.get(stop.cluster as usize)?;
        Some(CaretGeometry {
            page: at.page,
            x: stop.x,
            top: cluster.top,
            bottom: cluster.bottom,
            mode: stop.mode,
            offset: stop.offset,
            affinity: affinity_of(stop),
            exact,
            equation: stop.equation.map(|e| self.equation_outline(e)).unwrap_or_default(),
            line_text: line.clusters.iter().map(|c| c.text.as_str()).collect(),
        })
    }

    /// Bounding rectangles (one per line it occupies) of an equation.
    pub fn equation_outline(&self, equation: u32) -> Vec<PageRect> {
        let mut rects = Vec::new();
        for (p, page) in self.pages.iter().enumerate() {
            for line in &page.lines {
                let mut x0 = f64::INFINITY;
                let mut x1 = f64::NEG_INFINITY;
                let mut y0 = f64::INFINITY;
                let mut y1 = f64::NEG_INFINITY;
                for c in line.clusters.iter().filter(|c| c.equation == Some(equation)) {
                    x0 = x0.min(c.x0);
                    x1 = x1.max(c.x1);
                    y0 = y0.min(c.top);
                    y1 = y1.max(c.bottom);
                }
                if x0.is_finite() {
                    rects.push(PageRect { page: p as u32, x: x0, y: y0, width: x1 - x0, height: y1 - y0 });
                }
            }
        }
        rects
    }

    /// Resolve a point on a page to the nearest editable caret position.
    pub fn hit_test(&self, page: u32, x: f64, y: f64) -> PageHit {
        let none = PageHit { caret: None, mode: None, non_editable: None };
        let Some(layout) = self.pages.get(page as usize) else { return none };
        if layout.lines.is_empty() {
            return none;
        }
        let vertical = |l: usize| {
            let line = &layout.lines[l];
            if y < line.top {
                line.top - y
            } else if y > line.bottom {
                y - line.bottom
            } else {
                0.0
            }
        };
        // The line under (or nearest to) the point.
        let line_index = (0..layout.lines.len())
            .min_by(|&a, &b| {
                vertical(a).total_cmp(&vertical(b)).then_with(|| {
                    let ca = (layout.lines[a].top + layout.lines[a].bottom) / 2.0;
                    let cb = (layout.lines[b].top + layout.lines[b].bottom) / 2.0;
                    (ca - y).abs().total_cmp(&(cb - y).abs())
                })
            })
            .expect("non-empty");
        let line = &layout.lines[line_index];

        // The cluster under (or nearest to) the point.
        let direct = line
            .clusters
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| box_distance(a, x, y).total_cmp(&box_distance(b, x, y)));
        let mut non_editable = None;
        let mut target: Option<StopRef> = None;
        if let Some((ci, cluster)) = direct {
            let inside = box_distance(cluster, x, y) == 0.0;
            if !cluster.is_editable() {
                if inside && let crate::map::ClusterKind::NonEditable(reason) = cluster.kind {
                    non_editable = Some(reason);
                }
            } else {
                target = line
                    .stops
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.cluster == ci as u32)
                    .min_by(|(_, a), (_, b)| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
                    .map(|(si, _)| StopRef { page, line: line_index as u32, stop: si as u32 });
            }
        }
        if target.is_none() {
            target = nearest_stop_in_line(line, x).map(|si| StopRef { page, line: line_index as u32, stop: si });
        }
        if target.is_none() {
            // Nearest line on this page that has editable text.
            target = (0..layout.lines.len())
                .filter(|&l| !layout.lines[l].stops.is_empty())
                .min_by(|&a, &b| vertical(a).total_cmp(&vertical(b)))
                .and_then(|l| {
                    nearest_stop_in_line(&layout.lines[l], x).map(|si| StopRef { page, line: l as u32, stop: si })
                });
        }
        let caret = target.and_then(|at| self.caret_at(at));
        let mode = target.and_then(|at| self.stop(at)).map(|s| s.mode);
        PageHit { caret, mode, non_editable }
    }

    /// Move a caret over rendered characters.
    pub fn navigate(
        &self,
        path: &WorkspacePath,
        offset: u32,
        affinity: Affinity,
        motion: Motion,
        goal_x: Option<f64>,
    ) -> Option<NavigationResult> {
        let (at, exact) = self.locate(path, offset, affinity)?;
        let current = *self.stop(at)?;
        let result = |to: StopRef, goal: f64| self.caret_at(to).map(|caret| NavigationResult { caret, goal_x: goal });
        if !exact && matches!(motion, Motion::Left | Motion::Right) {
            // The caret is between stops (e.g. a stale offset): the located
            // stop already lies in the requested direction or at the caret.
            let wanted = match motion {
                Motion::Right => current.offset >= offset,
                _ => current.offset <= offset,
            };
            if wanted {
                return result(at, current.x);
            }
        }
        let order = self.reading_order();
        let here = self.reading_index(at.page, at.line)?;
        match motion {
            Motion::Right => {
                let target = self.step_right(at, &current, order, here)?;
                result(target, self.stop(target)?.x)
            }
            Motion::Left => {
                let target = self.step_left(at, &current, order, here)?;
                result(target, self.stop(target)?.x)
            }
            Motion::WordRight => {
                let mut pos = at;
                for _ in 0..512 {
                    let Some(next) =
                        self.step_right(pos, self.stop(pos)?, order, self.reading_index(pos.page, pos.line)?)
                    else {
                        break;
                    };
                    pos = next;
                    if self.is_word_boundary(pos) {
                        break;
                    }
                }
                result(pos, self.stop(pos)?.x)
            }
            Motion::WordLeft => {
                let mut pos = at;
                for _ in 0..512 {
                    let Some(prev) =
                        self.step_left(pos, self.stop(pos)?, order, self.reading_index(pos.page, pos.line)?)
                    else {
                        break;
                    };
                    pos = prev;
                    if self.is_word_boundary(pos) {
                        break;
                    }
                }
                result(pos, self.stop(pos)?.x)
            }
            Motion::Up | Motion::Down => {
                let goal = goal_x.unwrap_or(current.x);
                let candidates: Box<dyn Iterator<Item = &(u32, u32)>> = if motion == Motion::Up {
                    Box::new(order[..here].iter().rev())
                } else {
                    Box::new(order[here + 1..].iter())
                };
                for &(p, l) in candidates {
                    let line = self.line(p, l)?;
                    if let Some(si) = nearest_stop_in_line(line, goal) {
                        return result(StopRef { page: p, line: l, stop: si }, goal);
                    }
                }
                // No further line: go to the start/end of this line.
                let line = self.line(at.page, at.line)?;
                let si = if motion == Motion::Up { 0 } else { line.stops.len() as u32 - 1 };
                result(StopRef { stop: si, ..at }, goal)
            }
            Motion::LineStart => result(StopRef { stop: 0, ..at }, 0.0).map(|mut r| {
                r.goal_x = self.stop(StopRef { stop: 0, ..at }).map_or(0.0, |s| s.x);
                r
            }),
            Motion::LineEnd => {
                let line = self.line(at.page, at.line)?;
                let to = StopRef { stop: line.stops.len() as u32 - 1, ..at };
                result(to, self.stop(to)?.x)
            }
            Motion::DocumentStart => {
                let file = self.file_index(path)?;
                let &(p, l) = order
                    .iter()
                    .find(|&&(p, l)| self.line(p, l).is_some_and(|ln| ln.stops.iter().any(|s| s.file == file)))?;
                let si = self.line(p, l)?.stops.iter().position(|s| s.file == file)? as u32;
                let to = StopRef { page: p, line: l, stop: si };
                result(to, self.stop(to)?.x)
            }
            Motion::DocumentEnd => {
                let file = self.file_index(path)?;
                let &(p, l) = order
                    .iter()
                    .rev()
                    .find(|&&(p, l)| self.line(p, l).is_some_and(|ln| ln.stops.iter().any(|s| s.file == file)))?;
                let si = self.line(p, l)?.stops.iter().rposition(|s| s.file == file)? as u32;
                let to = StopRef { page: p, line: l, stop: si };
                result(to, self.stop(to)?.x)
            }
        }
    }

    fn step_right(&self, at: StopRef, current: &NavStop, order: &[(u32, u32)], here: usize) -> Option<StopRef> {
        let line = self.line(at.page, at.line)?;
        if let Some(j) = (at.stop as usize + 1..line.stops.len()).find(|&j| !same_visual(&line.stops[j], current)) {
            return Some(StopRef { stop: j as u32, ..at });
        }
        for &(p, l) in &order[here + 1..] {
            let next = self.line(p, l)?;
            if next.stops.is_empty() {
                continue;
            }
            // A stop at the current source offset on the next line is the
            // same logical position (e.g. after a hyphenation point); skip it.
            let j = (0..next.stops.len()).find(|&j| next.stops[j].offset != current.offset).unwrap_or(0);
            return Some(StopRef { page: p, line: l, stop: j as u32 });
        }
        None
    }

    fn step_left(&self, at: StopRef, current: &NavStop, order: &[(u32, u32)], here: usize) -> Option<StopRef> {
        let line = self.line(at.page, at.line)?;
        if let Some(j) = (0..at.stop as usize).rev().find(|&j| !same_visual(&line.stops[j], current)) {
            return Some(StopRef { stop: j as u32, ..at });
        }
        for &(p, l) in order[..here].iter().rev() {
            let prev = self.line(p, l)?;
            let n = prev.stops.len();
            if n == 0 {
                continue;
            }
            // The end of the previous line can be the same logical position as
            // the start of this one (a wrapped space); skip that offset.
            let j = (0..n).rev().find(|&j| prev.stops[j].offset != current.offset).unwrap_or(n - 1);
            return Some(StopRef { page: p, line: l, stop: j as u32 });
        }
        None
    }

    /// Whether a stop sits at a word start (left of it whitespace or line start,
    /// right of it a non-space character) or at a line end.
    fn is_word_boundary(&self, at: StopRef) -> bool {
        let Some(line) = self.line(at.page, at.line) else { return true };
        let Some(stop) = line.stops.get(at.stop as usize) else { return true };
        let cluster = &line.clusters[stop.cluster as usize];
        let (left, right): (Option<char>, Option<char>) = match stop.edge {
            Edge::Inner => {
                let byte = cluster.stops[stop.index as usize].text_byte as usize;
                (cluster.text[..byte].chars().next_back(), cluster.text[byte..].chars().next())
            }
            Edge::Start => (
                (stop.cluster > 0).then(|| line.clusters[stop.cluster as usize - 1].text.chars().next_back()).flatten(),
                cluster.text.chars().next(),
            ),
            Edge::End => (
                cluster.text.chars().next_back(),
                line.clusters.get(stop.cluster as usize + 1).and_then(|c| c.text.chars().next()),
            ),
        };
        match (left, right) {
            (_, None) => true,
            (None, Some(r)) => !r.is_whitespace(),
            (Some(l), Some(r)) => l.is_whitespace() && !r.is_whitespace(),
        }
    }

    /// Highlight rectangles and rendered text for a source range.
    pub fn selection(&self, path: &WorkspacePath, start: u32, end: u32) -> SelectionGeometry {
        let (start, end) = (start.min(end), start.max(end));
        let mut rects = Vec::new();
        let mut text = String::new();
        let Some(file) = self.file_index(path) else { return SelectionGeometry { rects, text } };
        let mut last_end: Option<u32> = None;
        for &(p, l) in self.reading_order() {
            let Some(line) = self.line(p, l) else { continue };
            let mut x0 = f64::INFINITY;
            let mut x1 = f64::NEG_INFINITY;
            let mut line_text = String::new();
            let mut first_start: Option<u32> = None;
            let mut line_end: Option<u32> = None;
            for cluster in &line.clusters {
                let Some(source) = cluster.source.filter(|s| s.file == file) else { continue };
                let range = source.range;
                if cluster.is_editable() {
                    if range.end <= start || range.start >= end {
                        continue;
                    }
                    let inside: Vec<_> =
                        cluster.stops.iter().filter(|s| s.offset >= start && s.offset <= end).collect();
                    let (Some(a), Some(b)) = (inside.first(), inside.last()) else { continue };
                    if a.offset == b.offset {
                        continue;
                    }
                    x0 = x0.min(a.x.min(b.x));
                    x1 = x1.max(a.x.max(b.x));
                    line_text.push_str(&cluster.text[a.text_byte as usize..b.text_byte as usize]);
                    first_start.get_or_insert(a.offset);
                    line_end = Some(b.offset);
                } else if range.start >= start && range.end <= end && !range.is_empty() {
                    x0 = x0.min(cluster.x0);
                    x1 = x1.max(cluster.x1);
                    line_text.push_str(&cluster.text);
                    first_start.get_or_insert(range.start);
                    line_end = Some(range.end);
                }
            }
            if x0.is_finite() {
                rects.push(PageRect {
                    page: p,
                    x: x0,
                    y: line.top,
                    width: (x1 - x0).max(0.5),
                    height: line.bottom - line.top,
                });
                if let (Some(prev), Some(first)) = (last_end, first_start)
                    && prev != first
                {
                    text.push('\n');
                }
                text.push_str(&line_text);
                last_end = line_end;
            }
        }
        SelectionGeometry { rects, text }
    }
}

fn nearest_stop_in_line(line: &crate::map::Line, x: f64) -> Option<u32> {
    line.stops
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
        .map(|(i, _)| i as u32)
}

#[cfg(test)]
mod tests {
    use exposition_core::text::TextRange;

    use super::*;
    use crate::map::tests::text_cluster;
    use crate::map::{EquationInfo, RawPage};

    fn sample() -> (PageMap, WorkspacePath) {
        let path = WorkspacePath::parse("main.typ").unwrap();
        // Line 1: "ab " (0..3) then wrapped "cd" (3..5) on line 2.
        let mut space = text_cluster(20.0, 20.0, " ", 2, None);
        space.x1 = 20.0;
        space.stops[1].x = 20.0; // trimmed end-of-line space has no width
        let clusters = vec![text_cluster(10.0, 20.0, "ab", 0, None), space, text_cluster(10.0, 34.0, "cd", 3, None)];
        let map = PageMap::build(
            vec![path.clone()],
            Vec::<EquationInfo>::new(),
            vec![RawPage { width: 100.0, height: 100.0, clusters }],
        );
        (map, path)
    }

    #[test]
    fn hit_test_maps_points_to_offsets() {
        let (map, path) = sample();
        let hit = map.hit_test(0, 16.0, 18.0);
        assert_eq!(hit.caret.unwrap().offset, 1);
        let hit = map.hit_test(0, 90.0, 18.0);
        assert_eq!(hit.caret.unwrap().offset, 2, "right of the line resolves to the line end");
        let hit = map.hit_test(0, 21.0, 33.0);
        let caret = hit.caret.unwrap();
        assert_eq!((caret.path, caret.offset), (path, 5));
    }

    #[test]
    fn arrows_cross_lines_once() {
        let (map, path) = sample();
        let right = |o, a| map.navigate(&path, o, a, Motion::Right, None).unwrap().caret;
        let c = right(1, Affinity::Downstream);
        assert_eq!(c.offset, 2);
        // From the end of line 1 the next press lands at the start of line 2.
        let c = right(c.offset, c.affinity);
        assert_eq!((c.offset, c.affinity), (3, Affinity::Downstream));
        let g = map.caret_geometry(&path, 3, Affinity::Downstream).unwrap();
        assert!(g.top > 20.0, "caret drawn on the second line");
        let c = map.navigate(&path, 3, Affinity::Downstream, Motion::Left, None).unwrap().caret;
        assert_eq!(c.offset, 2);
        let up = map.navigate(&path, 4, Affinity::Downstream, Motion::Up, None).unwrap();
        assert_eq!(up.caret.offset, 1);
    }

    #[test]
    fn selection_spans_lines() {
        let (map, path) = sample();
        let sel = map.selection(&path, 1, 4);
        assert_eq!(sel.rects.len(), 2);
        assert_eq!(sel.text, "b c");
        assert_eq!(map.selection(&path, 0, 5).text, "ab cd");
        let _ = TextRange::new(0, 0);
    }
}
