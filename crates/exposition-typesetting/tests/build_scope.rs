//! Builds belong to the folder they read (R-sec LOW 6, Typst side): after the
//! user opens another folder, a build id of the previous folder serves no page,
//! page map, or revision text and cannot be exported into the open folder —
//! even before the workspace-closed event resets the service.

mod common;

use std::path::Path;

use common::Fixture;
use exposition_page::PageMapProvider;

fn other_folder(base: &Path) -> std::path::PathBuf {
    let other = base.join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("main.typ"), "Other folder\n").unwrap();
    other
}

#[test]
fn builds_of_a_folder_that_is_no_longer_open_are_unreachable() {
    let fx = Fixture::open("typst-basic", "main.typ");
    let record = fx.build();
    assert!(record.succeeded());
    let id = record.id().to_owned();
    let page = record.result.pages[0].render_ref.clone();
    let revision = fx.revision();
    assert!(fx.service.record(&id).is_some());
    assert!(fx.service.page_svg(&page).is_some(), "the page is served while its folder is open");

    let other_base = tempfile::tempdir().unwrap();
    let other = other_folder(other_base.path());
    fx.workspace.open_workspace(&other).expect("open the other folder");
    // A PDF of the other folder's own, at the default export destination.
    std::fs::write(other.join("main.pdf"), b"%PDF-1.7 the other folder's own export").unwrap();

    assert!(fx.service.record(&id).is_none(), "the build id is unknown in the other folder");
    assert!(fx.service.page_svg(&page).is_none(), "its pages are not served (not even from the SVG cache)");
    assert!(fx.service.page_map(&id).is_none(), "its page map is not served");
    assert!(fx.service.revision_text(&fx.document, revision).is_none(), "its texts are not served");
    assert!(fx.service.last_good(&fx.path).is_none());
    assert!(fx.service.latest(&fx.path).is_none());
    assert!(fx.service.export_pdf(&id, None).is_err(), "it is not exported into the open folder");
    assert_eq!(std::fs::read(other.join("main.pdf")).unwrap(), b"%PDF-1.7 the other folder's own export");

    // Reopening the folder makes its retained builds reachable again.
    fx.workspace.open_workspace(&fx.root).expect("reopen");
    assert!(fx.service.record(&id).is_some());
}
