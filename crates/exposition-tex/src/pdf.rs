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
    /// Opaque identifier of the font program (stable within one PDF).
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

    /// Render one page at `scale` pixels per point (e.g. 2.0 ≈ 144 dpi) onto white.
    pub fn render_page(&self, index: usize, scale: f32) -> Result<RenderedPage, PdfError> {
        let page = self.pdf.pages().get(index).ok_or(PdfError::NoSuchPage(index))?;
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
        Ok(RenderedPage { width_px, height_px, scale, png })
    }

    /// Every glyph drawn on a page, in drawing order, with recovered Unicode.
    pub fn extract_glyphs(&self, index: usize) -> Result<Vec<PageGlyph>, PdfError> {
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
        let mut collector = GlyphCollector { glyphs: Vec::new() };
        interpret_page(page, &mut context, &mut collector);
        Ok(collector.glyphs)
    }
}

struct GlyphCollector {
    glyphs: Vec<PageGlyph>,
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

impl<'a> Device<'a> for GlyphCollector {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
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
