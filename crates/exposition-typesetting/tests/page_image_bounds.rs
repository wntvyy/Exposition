//! Page images and the document-chosen page size (R-sec LOW 10, Typst side).
//! TeX pages are rasterized and capped at `MAX_PAGE_IMAGE_PIXELS`; Typst pages
//! are served as SVG, which the app never rasterizes, so a page of any size
//! costs the same as a small one.

mod common;

use common::Fixture;
use exposition_page::PageMapProvider;

#[test]
fn a_huge_typst_page_is_a_small_vector_image() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.typ"), "#set page(width: 16383pt, height: 16383pt)\nHuge page.\n").unwrap();
    let fx = Fixture::open_dir(dir.path(), "main.typ");
    let record = fx.build();
    assert!(record.succeeded(), "{:?}", record.result.diagnostics);
    let page = &record.result.pages[0];
    assert!(page.width_pt > 16000.0, "{}", page.width_pt);
    let image = fx.service.page_image(&page.render_ref).expect("page image");
    assert_eq!(image.media_type, "image/svg+xml");
    assert!(image.bytes.len() < 64 * 1024, "{} bytes", image.bytes.len());
}
