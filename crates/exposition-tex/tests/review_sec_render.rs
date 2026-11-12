//! R-sec (M6 security review): resource bounds of TeX page rendering.
//!
//! The page view requests TeX pages through `page_image`, which renders at
//! `PAGE_IMAGE_SCALE` (3 px/pt) with no limit on the page size; the document
//! chooses the page size (`\pdfpagewidth`, `\paperwidth`, geometry). A
//! 16383 pt square page (pdfTeX's `\maxdimen`) would need a 49149 x 49149 RGBA
//! pixmap (~9.7 GB) per render. These tests use a moderate 4000 pt page so they
//! stay safe to run on a shared machine and only check the bound.
//!
//! Run: `cargo test -p exposition-tex --test review_sec_render -- --include-ignored`.

use exposition_tex::pdf::PdfDocument;
use exposition_tex::service::PAGE_IMAGE_SCALE;

/// A one-page PDF with an empty content stream and the given MediaBox size.
fn pdf_with_page(width_pt: u32, height_pt: u32) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width_pt} {height_pt}] /Contents 4 0 R /Resources << >> >>"
        ),
        "<< /Length 0 >>\nstream\n\nendstream".to_owned(),
    ];
    let mut out = b"%PDF-1.5\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
    );
    out
}

/// HELD: an ordinary A4 page renders at the page-view scale.
#[test]
fn a4_page_renders() {
    let pdf = PdfDocument::open(pdf_with_page(595, 842)).unwrap();
    let page = pdf.render_page(0, PAGE_IMAGE_SCALE).unwrap();
    assert_eq!((page.width_px, page.height_px), (1785, 2526));
}

/// R-sec F7: the raster size is bounded only by the document's page size.
/// 64 megapixels (256 MB RGBA) is used here as a generous cap.
#[test]
fn page_rendering_is_bounded() {
    const CAP_PIXELS: u64 = 64 * 1024 * 1024;
    let pdf = PdfDocument::open(pdf_with_page(4000, 4000)).unwrap();
    let rendered = pdf.render_page(0, PAGE_IMAGE_SCALE);
    match rendered {
        Err(e) => eprintln!("refused: {e}"),
        Ok(page) => {
            let pixels = u64::from(page.width_px) * u64::from(page.height_px);
            assert!(
                pixels <= CAP_PIXELS,
                "DEFECT: a 4000 pt page rendered to {}x{} px ({} MB RGBA); a 16383 pt page would need ~9.7 GB",
                page.width_px,
                page.height_px,
                pixels * 4 / 1_048_576
            );
        }
    }
}

/// The cap is reported: a page rendered below the requested scale says so and
/// gives the scale used (the page view shows it at its size in points).
#[test]
fn a_capped_render_reports_its_scale() {
    let pdf = PdfDocument::open(pdf_with_page(4000, 4000)).unwrap();
    let page = pdf.render_page(0, PAGE_IMAGE_SCALE).unwrap();
    assert!(page.capped && page.scale < PAGE_IMAGE_SCALE, "{} {}", page.capped, page.scale);
    assert!(u64::from(page.width_px) * u64::from(page.height_px) <= exposition_page::MAX_PAGE_IMAGE_PIXELS);
    let a4 = pdf_with_page(595, 842);
    let a4 = PdfDocument::open(a4).unwrap().render_page(0, PAGE_IMAGE_SCALE).unwrap();
    assert!(!a4.capped && a4.scale == PAGE_IMAGE_SCALE);
}
