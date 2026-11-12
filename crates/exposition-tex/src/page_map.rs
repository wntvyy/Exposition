//! TeX page maps for the shared page editor: [`PageMapping`]s of a build
//! adapted to `exposition_page` clusters.
//!
//! * A cluster is a run of consecutive glyphs with the same target: one
//!   glyph, an accent drawn over its base (`\'e` in OT1), or the glyphs of one
//!   atomic unit (`\ldots`, `\sin`).
//! * Geometry, in points from the top-left of the page: prose uses the glyphs'
//!   layout boxes (advance × 0.75 em ascent / 0.25 em descent); mathematics
//!   uses their ink extent (at least a band around the math axis), so that a
//!   raised script or a stacked limit is its own hit target rather than lying
//!   inside its base's box.
//! * Editable clusters carry caret stops at the mapped source boundaries,
//!   spread across the cluster (a ligature `ffi` has four stops; an atomic
//!   macro such as `\alpha` or `--` only its two edges). Mathematics refers to
//!   its enclosing equation (`$…$`, `\[…\]`, `equation`, …), so the shared
//!   line builder keeps an equation's glyphs together in source order.
//! * Unmapped glyphs (numbers, labels, citations, macro output, hyphens,
//!   bibliography, page furniture) are `NonEditable(Generated)`; verbatim text
//!   is `NonEditable(Raw)`.

use std::collections::HashMap;

use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_page::{
    Cluster, ClusterKind, ClusterSource, EditMode, EquationInfo, NonEditableReason, PageMap, RawPage, Stop,
};

use crate::latex::{Construct, Mode};
use crate::mapping::{GlyphTarget, PageMapping};
use crate::pdf::PageRect;

/// Build the shared page map of one build from its page mappings.
/// `pages` pairs each mapping with its page size in points.
pub fn build_page_map(pages: &[(PageMapping, (f64, f64))]) -> PageMap {
    let mut files: Vec<WorkspacePath> = Vec::new();
    let mut equations: Vec<EquationInfo> = Vec::new();
    let mut equation_ids: HashMap<(u32, TextRange), u32> = HashMap::new();
    let mut raw_pages = Vec::with_capacity(pages.len());
    for (mapping, (width, height)) in pages {
        let mut clusters = Vec::new();
        let mut i = 0;
        while i < mapping.glyphs.len() {
            // Consecutive glyphs with an identical target form one cluster.
            let mut j = i + 1;
            while j < mapping.glyphs.len() && mapping.targets[j] == mapping.targets[i] && joinable(&mapping.targets[i])
            {
                j += 1;
            }
            let glyphs = &mapping.glyphs[i..j];
            let math = matches!(&mapping.targets[i], GlyphTarget::Source { mode: Mode::Math, .. });
            let glyph_box = |g: &crate::pdf::PageGlyph| -> PageRect {
                if !math {
                    return g.bbox;
                }
                let band = PageRect::new(g.bbox.x0, g.origin_y - 0.45 * g.size, g.bbox.x1, g.origin_y + 0.05 * g.size);
                match g.ink {
                    Some(ink) => PageRect::new(g.bbox.x0, ink.y0.min(band.y0), g.bbox.x1, ink.y1.max(band.y1)),
                    None => band,
                }
            };
            let mut bbox: PageRect = glyph_box(&glyphs[0]);
            for g in &glyphs[1..] {
                bbox = bbox.union(&glyph_box(g));
            }
            let text: String = glyphs.iter().map(|g| g.text.as_deref().unwrap_or("\u{FFFD}")).collect();
            let baseline = glyphs[0].origin_y;
            let cluster = match &mapping.targets[i] {
                GlyphTarget::Source { path, range, boundaries, mode, construct, equation, .. } => {
                    let file = match files.iter().position(|f| f == path) {
                        Some(index) => index as u32,
                        None => {
                            files.push(path.clone());
                            (files.len() - 1) as u32
                        }
                    };
                    let equation = equation.map(|eq| {
                        *equation_ids.entry((file, eq)).or_insert_with(|| {
                            equations.push(EquationInfo {
                                file,
                                range: eq,
                                block: *construct == Construct::DisplayMath,
                            });
                            (equations.len() - 1) as u32
                        })
                    });
                    let kind = match (construct, mode) {
                        (Construct::Verbatim, _) => ClusterKind::NonEditable(NonEditableReason::Raw),
                        (_, Mode::Math) => ClusterKind::Editable(EditMode::Math),
                        (_, Mode::Text) => ClusterKind::Editable(EditMode::Markup),
                    };
                    Cluster {
                        x0: bbox.x0,
                        x1: bbox.x1,
                        top: bbox.y0,
                        bottom: bbox.y1,
                        baseline,
                        stops: stops(bbox.x0, bbox.x1, boundaries, &text),
                        text,
                        source: Some(ClusterSource { file, range: *range }),
                        kind,
                        equation,
                    }
                }
                GlyphTarget::Unmapped { .. } => Cluster {
                    x0: bbox.x0,
                    x1: bbox.x1,
                    top: bbox.y0,
                    bottom: bbox.y1,
                    baseline,
                    text,
                    source: None,
                    kind: ClusterKind::NonEditable(NonEditableReason::Generated),
                    stops: Vec::new(),
                    equation: None,
                },
            };
            clusters.push(cluster);
            i = j;
        }
        attach_equation_numbers(&mut clusters, &equations);
        raw_pages.push(RawPage { width: *width, height: *height, clusters });
    }
    PageMap::build(files, equations, raw_pages)
}

/// Generated text on a display equation's baseline (its number, `(1)`) is
/// given the display's full vertical band. Its own (narrower) line would
/// otherwise overlap the display with a centre closer to clicks on raised or
/// lowered mathematics and capture them; with equal bands the display line,
/// first in reading order, wins.
fn attach_equation_numbers(clusters: &mut [Cluster], equations: &[EquationInfo]) {
    let mut bands: HashMap<u32, (f64, f64, Vec<f64>)> = HashMap::new();
    for c in clusters.iter() {
        if let Some(e) = c.equation
            && equations.get(e as usize).is_some_and(|eq| eq.block)
        {
            let band = bands.entry(e).or_insert((f64::INFINITY, f64::NEG_INFINITY, Vec::new()));
            band.0 = band.0.min(c.top);
            band.1 = band.1.max(c.bottom);
            band.2.push(c.baseline);
        }
    }
    for cluster in clusters.iter_mut().filter(|c| c.source.is_none() && c.equation.is_none()) {
        let display =
            bands.values().find(|(_, _, baselines)| baselines.iter().any(|b| (b - cluster.baseline).abs() < 0.5));
        if let Some((top, bottom, _)) = display {
            cluster.top = *top;
            cluster.bottom = *bottom;
        }
    }
}

/// Unmapped glyphs stay separate clusters (each is its own hit target).
fn joinable(target: &GlyphTarget) -> bool {
    target.is_mapped()
}

/// Caret stops spread evenly across the cluster, one per source boundary.
fn stops(x0: f64, x1: f64, boundaries: &[u32], text: &str) -> Vec<Stop> {
    let steps = boundaries.len().saturating_sub(1).max(1);
    let chars: Vec<usize> = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
    let char_count = chars.len() - 1;
    boundaries
        .iter()
        .enumerate()
        .map(|(i, &offset)| {
            let fraction = i as f64 / steps as f64;
            let char_index = ((fraction * char_count as f64).round() as usize).min(char_count);
            Stop { x: x0 + (x1 - x0) * fraction, offset, text_byte: chars[char_index] as u32 }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_spread_over_ligatures_and_atomic_units() {
        let ligature = stops(10.0, 16.0, &[5, 6, 7, 8], "ffi");
        assert_eq!(ligature.iter().map(|s| s.offset).collect::<Vec<_>>(), [5, 6, 7, 8]);
        assert_eq!(ligature.iter().map(|s| s.text_byte).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert!((ligature[1].x - 12.0).abs() < 1e-9);
        let atomic = stops(0.0, 4.0, &[10, 17], "α");
        assert_eq!(atomic.iter().map(|s| (s.offset, s.text_byte)).collect::<Vec<_>>(), [(10, 0), (17, 2)]);
    }
}
