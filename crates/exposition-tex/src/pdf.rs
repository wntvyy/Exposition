//! PDF page rendering and glyph-level extraction (hayro).
//!
//! Page coordinates everywhere in this crate are PDF points (1/72 in) with
//! the origin at the top-left corner of the page and `y` growing downwards —
//! the same space SyncTeX positions are converted into, and the space a page
//! view overlays carets and selections on (scaled by its zoom factor).

use std::sync::Arc;

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterCache,
    InterpreterSettings, SoftMask, interpret_page,
};
use hayro::hayro_syntax::Pdf;
use hayro::kurbo::{Affine, BezPath, Point, Rect, Shape};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
use serde::{Deserialize, Serialize};

/// An axis-aligned rectangle in page points (top-left origin).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "TexPageRect")]
#[serde(rename_all = "camelCase")]
pub struct PageRect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl PageRect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn center(&self) -> (f64, f64) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    pub fn union(&self, other: &PageRect) -> PageRect {
        PageRect::new(self.x0.min(other.x0), self.y0.min(other.y0), self.x1.max(other.x1), self.y1.max(other.y1))
    }

    /// Squared distance from a point to the rectangle (0 inside).
    pub fn distance_sq(&self, x: f64, y: f64) -> f64 {
        let dx = if x < self.x0 {
            self.x0 - x
        } else if x > self.x1 {
            x - self.x1
        } else {
            0.0
        };
        let dy = if y < self.y0 {
            self.y0 - y
        } else if y > self.y1 {
            y - self.y1
        } else {
            0.0
        };
        dx * dx + dy * dy
    }
}

/// [`PageGlyph::font`] of a rule recorded as a glyph without Unicode: the
/// mapping may pair it with a source character (`\_`), else drops it.
pub const RULE_FONT: u64 = u64::MAX;

/// One glyph drawn on a page, in content-stream (= TeX shipout) order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, rename = "TexPageGlyph")]
#[serde(rename_all = "camelCase")]
pub struct PageGlyph {
    /// Index of the glyph within its page, in drawing order.
    pub index: u32,
    /// Unicode text recovered for the glyph (ToUnicode, then glyph name/AGL);
    /// ligatures yield several characters (e.g. "ffi").
    pub text: Option<String>,
    /// Character code that selected the glyph.
    pub char_code: u32,
    /// Opaque identifier of the font program (stable within one PDF);
    /// [`RULE_FONT`] for a small rule drawn in place of a glyph (pdfLaTeX's
    /// OT1 `\_` is a 0.3 em rule).
    pub font: u64,
    /// Effective font size in points.
    pub size: f64,
    /// Baseline origin of the glyph.
    pub origin_x: f64,
    pub origin_y: f64,
    /// Advance width in points (horizontal writing).
    pub advance: f64,
    /// Layout box: advance width × (0.75 em ascent, 0.25 em descent). Used for
    /// hit testing and caret placement.
    pub bbox: PageRect,
    /// Tight ink bounds from the glyph outline, when it has one.
    pub ink: Option<PageRect>,
}

impl PageGlyph {
    /// Caret x position before (`false`) or after (`true`) the glyph.
    pub fn caret_x(&self, after: bool) -> f64 {
        if after { self.origin_x + self.advance } else { self.origin_x }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("the PDF could not be loaded: {0}")]
    Load(String),
    #[error("page {0} does not exist")]
    NoSuchPage(usize),
    #[error("page rendering failed: {0}")]
    Render(String),
}

/// A rendered page bitmap.
#[derive(Clone, Debug)]
pub struct RenderedPage {
    pub width_px: u32,
    pub height_px: u32,
    /// Pixels per PDF point used for this rendering.
    pub scale: f32,
    /// The page was rendered below the requested scale to stay within
    /// [`exposition_page::MAX_PAGE_IMAGE_PIXELS`].
    pub capped: bool,
    pub png: Vec<u8>,
}

/// A loaded PDF (one build output).
pub struct PdfDocument {
    pdf: Pdf,
}

impl PdfDocument {
    pub fn open(bytes: Vec<u8>) -> Result<Self, PdfError> {
        let pdf = Pdf::new(bytes).map_err(|e| PdfError::Load(format!("{e:?}")))?;
        Ok(Self { pdf })
    }

    pub fn page_count(&self) -> usize {
        self.pdf.pages().len()
    }

    /// Page size in points (after crop box and rotation).
    pub fn page_size(&self, index: usize) -> Result<(f64, f64), PdfError> {
        let page = self.pdf.pages().get(index).ok_or(PdfError::NoSuchPage(index))?;
        let (w, h) = page.render_dimensions();
        Ok((w as f64, h as f64))
    }

    /// Render one page at `scale` pixels per point (e.g. 2.0 ≈ 144 dpi) onto
    /// white; at a lower scale when the page would exceed
    /// [`exposition_page::MAX_PAGE_IMAGE_PIXELS`] (the document chooses its
    /// page size: a 16383 pt page at 3 px/pt would need ~9.7 GB).
    pub fn render_page(&self, index: usize, scale: f32) -> Result<RenderedPage, PdfError> {
        let page = self.pdf.pages().get(index).ok_or(PdfError::NoSuchPage(index))?;
        let (width_pt, height_pt) = page.render_dimensions();
        let (scale, capped) = exposition_page::capped_raster_scale(f64::from(width_pt), f64::from(height_pt), scale);
        let cache = RenderCache::new();
        let pixmap = render(
            page,
            &cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE },
        );
        let (width_px, height_px) = (pixmap.width() as u32, pixmap.height() as u32);
        let png = pixmap.into_png().map_err(|e| PdfError::Render(format!("{e:?}")))?;
        Ok(RenderedPage { width_px, height_px, scale, capped, png })
    }

    /// Every glyph drawn on a page, in drawing order, with recovered Unicode.
    pub fn extract_glyphs(&self, index: usize) -> Result<Vec<PageGlyph>, PdfError> {
        self.extract(index, false)
    }

    /// [`Self::extract_glyphs`] plus small rules recorded as glyphs without
    /// Unicode ([`RULE_FONT`]) in drawing order, for the mapping: pdfLaTeX's
    /// OT1 `\_` is such a rule.
    pub fn extract_glyphs_and_rules(&self, index: usize) -> Result<Vec<PageGlyph>, PdfError> {
        self.extract(index, true)
    }

    fn extract(&self, index: usize, rules: bool) -> Result<Vec<PageGlyph>, PdfError> {
        let page = self.pdf.pages().get(index).ok_or(PdfError::NoSuchPage(index))?;
        let cache = InterpreterCache::new();
        let (w, h) = page.render_dimensions();
        let mut context = Context::new(
            page.initial_transform(true).to_kurbo(),
            Rect::new(0.0, 0.0, w as f64, h as f64),
            &cache,
            self.pdf.xref(),
            InterpreterSettings { warning_sink: Arc::new(|_| {}), ..InterpreterSettings::default() },
        );
        let mut collector = GlyphCollector { glyphs: Vec::new(), rules };
        interpret_page(page, &mut context, &mut collector);
        Ok(collector.glyphs)
    }
}

struct GlyphCollector {
    glyphs: Vec<PageGlyph>,
    /// Also record small rules (see [`RULE_FONT`]).
    rules: bool,
}

impl GlyphCollector {
    fn push(&mut self, transform: Affine, glyph: &Glyph<'_>) {
        let (advance_units, ink_units, font) = match glyph {
            Glyph::Outline(outline) => {
                let path = outline.outline();
                let ink = (!path.elements().is_empty()).then(|| path.bounding_box());
                let font = (outline.identifier().cache_key() & u128::from(u64::MAX)) as u64;
                (outline.advance_width().unwrap_or(0.0) as f64, ink, font)
            }
            Glyph::Type3(t3) => (t3.advance_width().unwrap_or(0.0) as f64, None, 0),
        };
        let origin = transform * Point::ZERO;
        let end = transform * Point::new(advance_units, 0.0);
        let em_point = transform * Point::new(0.0, 1000.0);
        let size = ((em_point.x - origin.x).powi(2) + (em_point.y - origin.y).powi(2)).sqrt();
        let advance = ((end.x - origin.x).powi(2) + (end.y - origin.y).powi(2)).sqrt();
        let bbox = PageRect::new(origin.x, origin.y - 0.75 * size, origin.x + advance, origin.y + 0.25 * size);
        let ink = ink_units.map(|r| {
            let tr = transform.transform_rect_bbox(r);
            PageRect::new(tr.x0, tr.y0, tr.x1, tr.y1)
        });
        let text = glyph.as_unicode().map(|s| match s {
            BfString::Char(c) => c.to_string(),
            BfString::String(s) => s,
        });
        self.glyphs.push(PageGlyph {
            index: self.glyphs.len() as u32,
            text,
            char_code: glyph.char_code(),
            font,
            size,
            origin_x: origin.x,
            origin_y: origin.y,
            advance,
            bbox,
            ink,
        });
    }
}

impl GlyphCollector {
    /// A rule the size of a typeset underscore (OT1 `\_`: 0.3 em wide,
    /// 0.4 pt high, on the baseline), drawn as a filled rectangle or, as
    /// pdfTeX writes thin rules, a horizontal line stroked at the rule's
    /// height (`stroke_width`).
    fn push_rule(&mut self, path: &BezPath, transform: Affine, stroke_width: Option<f64>) {
        use hayro::kurbo::PathEl;
        let elements = path.elements();
        let [_, b, c, d] = transform.as_coeffs()[..4] else { return };
        if elements.len() > 6
            || elements.iter().any(|e| matches!(e, PathEl::QuadTo(..) | PathEl::CurveTo(..)))
            || b.abs() > 1e-9
            || c.abs() > 1e-9
        {
            return;
        }
        let r = transform.transform_rect_bbox(path.bounding_box());
        let r = match stroke_width {
            // Only a horizontal line, stroked.
            Some(w) if elements.len() == 2 && r.height() < 1e-6 => {
                let half = w * d.abs() / 2.0;
                Rect::new(r.x0, r.y0 - half, r.x1, r.y1 + half)
            }
            Some(_) => return,
            None => r,
        };
        let (width, height) = (r.width(), r.height());
        if !(1.2..=8.0).contains(&width) || height > 1.0 || height <= 0.0 {
            return;
        }
        let rect = PageRect::new(r.x0, r.y0, r.x1, r.y1);
        // The same rule painted twice is one rule.
        if self.glyphs.last().is_some_and(|g| g.font == RULE_FONT && g.ink == Some(rect)) {
            return;
        }
        let size = width / 0.3;
        self.glyphs.push(PageGlyph {
            index: self.glyphs.len() as u32,
            text: None,
            char_code: u32::from(b'_'),
            font: RULE_FONT,
            size,
            origin_x: rect.x0,
            origin_y: rect.y1,
            advance: width,
            bbox: PageRect::new(rect.x0, rect.y1 - 0.75 * size, rect.x1, rect.y1 + 0.25 * size),
            ink: Some(rect),
        });
    }
}

impl<'a> Device<'a> for GlyphCollector {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, mode: &DrawMode) {
        if !self.rules {
            return;
        }
        match mode {
            DrawMode::Fill(_) => self.push_rule(path, props.transform, None),
            DrawMode::Stroke(stroke) => self.push_rule(path, props.transform, Some(f64::from(stroke.line_width))),
            _ => {}
        }
    }
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, glyph_run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for glyph in glyph_run.glyphs() {
            self.push(props.transform * glyph.transform(), glyph);
        }
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}
