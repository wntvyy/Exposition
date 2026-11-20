//! Read-only project PDF views (`exposition_tex::pdf_view`): reading through
//! the project scope, parsing and rendering single- and multi-page files,
//! damaged / encrypted / empty files, size, page, pixel and view bounds, project
//! binding, and inert handling of active content.
//!
//! Fixtures are generated here (small hand-written PDFs with a classic xref).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_tex::pdf_view::{
    MAX_OPEN_PDF_VIEWS, MAX_PDF_VIEW_PAGE_PIXELS, MAX_PDF_VIEW_PAGES, PdfPageSize, PdfViews, bounded_view_scale,
};
use exposition_workspace::media::MAX_PDF_BYTES;
use exposition_workspace::{DocumentService, WorkspaceService};

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

/// A PDF from numbered object bodies (object 1 is the catalog) and extra trailer entries.
fn pdf_from_objects(objects: &[String], trailer_extra: &str) -> Vec<u8> {
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
        format!("trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1)
            .as_bytes(),
    );
    out
}

/// A PDF with one page per `(width, height)` in points; page `i` draws a
/// filled rectangle whose size depends on `i`. `catalog_extra` is added to the
/// catalog dictionary.
fn pdf_with_pages(sizes: &[(u32, u32)], catalog_extra: &str) -> Vec<u8> {
    let n = sizes.len();
    let kids = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect::<Vec<_>>().join(" ");
    let mut objects = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>"),
    ];
    for (i, (w, h)) in sizes.iter().enumerate() {
        let content = format!("0.8 0.1 0.1 rg 10 10 {} {} re f", 20 + 10 * (i % 50), 30 + 5 * (i % 50));
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R /Resources << >> >>",
            4 + 2 * i
        ));
        objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
    }
    pdf_from_objects(&objects, "")
}

/// Width and height of a PNG from its IHDR.
fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "a PNG");
    let u32_at = |i: usize| u32::from_be_bytes(png[i..i + 4].try_into().unwrap());
    (u32_at(16), u32_at(20))
}

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Arc<WorkspaceService>,
    views: PdfViews,
}

impl Fixture {
    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let target = self.project().join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    fn open(&self, name: &str) -> Result<exposition_tex::pdf_view::PdfViewInfo, &'static str> {
        self.views.open(&self.workspace, &wp(name)).map_err(|e| e.code)
    }
    fn page(&self, token: u32, page: u32, scale: f32) -> Result<Vec<u8>, &'static str> {
        self.views.page_png(&self.workspace, token, page, scale).map_err(|e| e.code)
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(dir.path().join("outside")).unwrap();
    let workspace = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    workspace.open_workspace(&project).unwrap();
    Fixture { dir, workspace, views: PdfViews::new() }
}

#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(unix)]
fn junction(link: &Path, target: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn a_single_page_pdf_opens_and_renders() {
    let f = fixture();
    let bytes = pdf_with_pages(&[(595, 842)], "");
    f.write("paper.pdf", &bytes);
    let info = f.open("paper.pdf").unwrap();
    assert_eq!(info.page_count, 1);
    assert_eq!(info.pages, vec![PdfPageSize { width: 595.0, height: 842.0 }]);
    assert_eq!(info.bytes as usize, bytes.len());
    let png = f.page(info.token, 0, 2.0).unwrap();
    assert_eq!(png_size(&png), (1190, 1684));
    assert_eq!(f.page(info.token, 1, 2.0).unwrap_err(), "pdf.noSuchPage");
    // Viewing never changes the file.
    assert_eq!(fs::read(f.project().join("paper.pdf")).unwrap(), bytes);
}

#[test]
fn a_multi_page_pdf_lists_and_renders_every_page() {
    let f = fixture();
    f.write("out/export one.PDF", &pdf_with_pages(&[(595, 842), (842, 595), (300, 400)], ""));
    let info = f.open("out/export one.PDF").unwrap();
    assert_eq!(info.page_count, 3);
    assert_eq!(
        info.pages,
        vec![
            PdfPageSize { width: 595.0, height: 842.0 },
            PdfPageSize { width: 842.0, height: 595.0 },
            PdfPageSize { width: 300.0, height: 400.0 },
        ]
    );
    let pages: Vec<Vec<u8>> = (0..3).map(|i| f.page(info.token, i, 1.0).unwrap()).collect();
    assert_eq!(png_size(&pages[0]), (595, 842));
    assert_eq!(png_size(&pages[1]), (842, 595));
    assert_eq!(png_size(&pages[2]), (300, 400));
    // The page content is drawn: the same page without its rectangle differs.
    f.write(
        "blank.pdf",
        &pdf_from_objects(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R /Resources << >> >>".to_owned(),
                "<< /Length 0 >>\nstream\n\nendstream".to_owned(),
            ],
            "",
        ),
    );
    let blank = f.open("blank.pdf").unwrap();
    let blank_png = f.page(blank.token, 0, 1.0).unwrap();
    assert_eq!(png_size(&blank_png), (300, 400));
    assert_ne!(blank_png, pages[2]);
    assert_eq!(f.page(info.token, 3, 1.0).unwrap_err(), "pdf.noSuchPage");
}

#[test]
fn damaged_and_disguised_files_are_refused() {
    let f = fixture();
    f.write("garbage.pdf", b"%PDF-1.7\nthis is not a pdf at all\n");
    f.write("text.pdf", b"TOKEN=not-a-pdf\n");
    f.write("binary.pdf", &[0x89, b'P', b'N', b'G', 0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    let mut noise = b"%PDF-1.4\n".to_vec();
    noise.extend((0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
    f.write("noise.pdf", &noise);
    for name in ["garbage.pdf", "text.pdf", "binary.pdf", "noise.pdf"] {
        assert_eq!(f.open(name).unwrap_err(), "pdf.damaged", "{name}");
    }
    assert_eq!(f.views.open_count(), 0, "a refused file keeps nothing");
}

#[test]
fn a_truncated_pdf_fails_or_renders_without_panicking() {
    let f = fixture();
    let whole = pdf_with_pages(&[(595, 842), (595, 842)], "");
    for cut in [whole.len() / 4, whole.len() / 2, whole.len() - 30] {
        f.write("cut.pdf", &whole[..cut]);
        match f.open("cut.pdf") {
            Err(code) => assert!(code == "pdf.damaged" || code == "pdf.empty", "{cut}: {code}"),
            // The parser may rebuild the file from the objects that are left.
            Ok(info) => {
                for page in 0..info.pages.len() as u32 {
                    let _ = f.page(info.token, page, 1.0);
                }
            }
        }
    }
}

#[test]
fn an_encrypted_pdf_is_refused_as_encrypted() {
    let f = fixture();
    let content = "0 0 0 rg 0 0 10 10 re f";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << >> >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        // A security handler that is not the standard one: cannot be opened.
        "<< /Filter /Custom.Handler /V 4 /R 4 /Length 128 >>".to_owned(),
    ];
    f.write(
        "locked.pdf",
        &pdf_from_objects(
            &objects,
            "/Encrypt 5 0 R /ID [<00112233445566778899aabbccddeeff> <00112233445566778899aabbccddeeff>]",
        ),
    );
    assert_eq!(f.open("locked.pdf").unwrap_err(), "pdf.encrypted");
    // The standard handler with a user password that is not empty.
    let mut standard = objects;
    standard[4] = format!("<< /Filter /Standard /V 1 /R 2 /P -44 /O <{}> /U <{}> >>", "11".repeat(32), "22".repeat(32));
    f.write(
        "password.pdf",
        &pdf_from_objects(
            &standard,
            "/Encrypt 5 0 R /ID [<00112233445566778899aabbccddeeff> <00112233445566778899aabbccddeeff>]",
        ),
    );
    assert_eq!(f.open("password.pdf").unwrap_err(), "pdf.encrypted");
    assert_eq!(f.views.open_count(), 0);
}

#[test]
fn empty_files_and_files_without_pages_are_refused_as_empty() {
    let f = fixture();
    f.write("zero.pdf", b"");
    assert_eq!(f.open("zero.pdf").unwrap_err(), "pdf.empty");
    f.write(
        "nopages.pdf",
        &pdf_from_objects(
            &["<< /Type /Catalog /Pages 2 0 R >>".to_owned(), "<< /Type /Pages /Kids [] /Count 0 >>".to_owned()],
            "",
        ),
    );
    let code = f.open("nopages.pdf").unwrap_err();
    assert!(code == "pdf.empty" || code == "pdf.damaged", "{code}");
}

#[test]
fn files_over_the_size_limit_are_refused_unread() {
    let f = fixture();
    let big = fs::File::create(f.project().join("big.pdf")).unwrap();
    big.set_len(MAX_PDF_BYTES + 1).unwrap();
    drop(big);
    let error = f.views.open(&f.workspace, &wp("big.pdf")).unwrap_err();
    assert_eq!(error.code, "pdf.tooLarge");
    assert!(error.message.contains("64 MiB"), "{}", error.message);
}

#[test]
fn only_the_first_pages_of_a_very_long_pdf_are_shown() {
    let f = fixture();
    let sizes = vec![(200u32, 100u32); MAX_PDF_VIEW_PAGES + 5];
    f.write("long.pdf", &pdf_with_pages(&sizes, ""));
    let info = f.open("long.pdf").unwrap();
    assert_eq!(info.page_count as usize, MAX_PDF_VIEW_PAGES + 5);
    assert_eq!(info.pages.len(), MAX_PDF_VIEW_PAGES);
    assert_eq!(png_size(&f.page(info.token, MAX_PDF_VIEW_PAGES as u32 - 1, 1.0).unwrap()), (200, 100));
    assert_eq!(f.page(info.token, MAX_PDF_VIEW_PAGES as u32, 1.0).unwrap_err(), "pdf.noSuchPage");
}

#[test]
fn page_rasters_and_scales_are_bounded() {
    let f = fixture();
    f.write("poster.pdf", &pdf_with_pages(&[(4000, 4000)], ""));
    let info = f.open("poster.pdf").unwrap();
    // 4000 pt at 8 px/pt would be 1024 megapixels.
    let (w, h) = png_size(&f.page(info.token, 0, 8.0).unwrap());
    assert!(u64::from(w) * u64::from(h) <= MAX_PDF_VIEW_PAGE_PIXELS, "{w}x{h}");
    assert!(u64::from(w) * u64::from(h) > MAX_PDF_VIEW_PAGE_PIXELS * 9 / 10, "{w}x{h}: close to the limit");
    for scale in [0.0, 0.1, 8.5, f32::NAN, f32::INFINITY, -1.0] {
        assert_eq!(f.page(info.token, 0, scale).unwrap_err(), "pdf.invalidScale", "{scale}");
    }
    assert_eq!(bounded_view_scale(595.0, 842.0, 3.0), 3.0);
    for (w, h) in [(16383.0, 16383.0), (20000.0, 10.0), (1.0, 1.0), (14400.0, 200.0)] {
        let s = bounded_view_scale(w, h, 8.0);
        let px = ((w * f64::from(s)).floor().max(1.0) * (h * f64::from(s)).floor().max(1.0)) as u64;
        assert!(s > 0.0 && px <= MAX_PDF_VIEW_PAGE_PIXELS, "{w}x{h}: {s} -> {px}");
    }
}

#[test]
fn open_views_are_bounded_and_closing_releases_them() {
    let f = fixture();
    f.write("a.pdf", &pdf_with_pages(&[(100, 100)], ""));
    let tokens: Vec<u32> = (0..MAX_OPEN_PDF_VIEWS + 2).map(|_| f.open("a.pdf").unwrap().token).collect();
    assert_eq!(f.views.open_count(), MAX_OPEN_PDF_VIEWS);
    assert_eq!(f.page(tokens[0], 0, 1.0).unwrap_err(), "pdf.closed", "the least recently used view was dropped");
    assert_eq!(f.page(tokens[1], 0, 1.0).unwrap_err(), "pdf.closed");
    let last = *tokens.last().unwrap();
    assert!(f.page(last, 0, 1.0).is_ok());
    // A view that is used is kept over one that is not.
    assert!(f.page(tokens[2], 0, 1.0).is_ok());
    let newer = f.open("a.pdf").unwrap().token;
    assert!(f.page(tokens[2], 0, 1.0).is_ok());
    assert_eq!(f.page(tokens[3], 0, 1.0).unwrap_err(), "pdf.closed");
    f.views.close(last);
    assert_eq!(f.page(last, 0, 1.0).unwrap_err(), "pdf.closed");
    f.views.close(last);
    f.views.close(987_654);
    assert!(f.page(newer, 0, 1.0).is_ok());
    f.views.clear();
    assert_eq!(f.views.open_count(), 0);
    assert_eq!(f.page(newer, 0, 1.0).unwrap_err(), "pdf.closed");
}

#[test]
fn only_pdf_files_inside_the_project_are_read() {
    let f = fixture();
    let pdf = pdf_with_pages(&[(100, 100)], "");
    f.write("docs/in.pdf", &pdf);
    f.write("secret.env", b"TOKEN=x");
    // PDF bytes under another extension are not read through this path either.
    f.write("renamed.txt", &pdf);
    fs::write(f.dir.path().join("outside/out.pdf"), &pdf).unwrap();
    fs::create_dir_all(f.project().join("folder.pdf")).unwrap();

    assert_eq!(f.open("secret.env").unwrap_err(), "pdf.notPdf");
    assert_eq!(f.open("renamed.txt").unwrap_err(), "pdf.notPdf");
    assert_eq!(f.open("missing.pdf").unwrap_err(), "path.notFound");
    assert_eq!(f.open("folder.pdf").unwrap_err(), "file.isDirectory");
    // A path cannot name a file above the project.
    assert!(WorkspacePath::parse("../outside/out.pdf").is_err());
    assert!(WorkspacePath::parse(&f.dir.path().join("outside/out.pdf").to_string_lossy()).is_err());
    // A link or junction that leads out of the project is refused, not followed.
    junction(&f.project().join("out-link"), &f.dir.path().join("outside"));
    assert_eq!(f.open("out-link/out.pdf").unwrap_err(), "path.outsideWorkspace");
    // One that stays inside the project is fine.
    junction(&f.project().join("docs-link"), &f.project().join("docs"));
    assert_eq!(f.open("docs-link/in.pdf").unwrap().page_count, 1);
    assert_eq!(f.views.open_count(), 1);
}

#[test]
fn a_view_belongs_to_the_project_it_was_opened_in() {
    let f = fixture();
    f.write("paper.pdf", &pdf_with_pages(&[(100, 100)], ""));
    let info = f.open("paper.pdf").unwrap();
    assert!(f.page(info.token, 0, 1.0).is_ok());

    let other = f.dir.path().join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("paper.pdf"), pdf_with_pages(&[(300, 300), (300, 300)], "")).unwrap();
    f.workspace.open_workspace(&other).unwrap();
    // The old view is never served in the other project, and is dropped.
    assert_eq!(f.page(info.token, 0, 1.0).unwrap_err(), "workspace.changed");
    assert_eq!(f.views.open_count(), 0);
    assert_eq!(f.page(info.token, 0, 1.0).unwrap_err(), "pdf.closed");
    // The same relative path is answered from the open project only.
    let there = f.open("paper.pdf").unwrap();
    assert_eq!(there.page_count, 2);
    assert_ne!(there.token, info.token);

    f.workspace.close_workspace();
    assert_eq!(f.open("paper.pdf").unwrap_err(), "workspace.notOpen");
    assert_eq!(f.page(there.token, 0, 1.0).unwrap_err(), "workspace.changed");
}

/// Active content is data to the parser and the rasterizer: a document with an
/// open action that launches a program, document JavaScript, a link, and a
/// reference to an external file opens and renders, and nothing is run or read.
#[test]
fn active_content_is_inert() {
    let f = fixture();
    let marker = f.dir.path().join("launched.txt");
    let secret = f.dir.path().join("outside/secret.txt");
    fs::write(&secret, b"TOKEN=outside").unwrap();
    let marker_arg = marker.to_string_lossy().replace('\\', "\\\\");
    let secret_arg = secret.to_string_lossy().replace('\\', "/");
    let content = "0.1 0.5 0.1 rg 10 10 50 50 re f /Ext Do";
    let objects = vec![
        format!(
            "<< /Type /Catalog /Pages 2 0 R /OpenAction 5 0 R /Names << /JavaScript << /Names [(a) 6 0 R] >> >> \
             /AA << /WC 6 0 R >> >>"
        ),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots [7 0 R] \
         /AA << /O 5 0 R >> /Resources << /XObject << /Ext 8 0 R >> >> >>"
            .to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        format!(
            "<< /S /Launch /F (cmd.exe) /Win << /F (cmd.exe) /P (/c echo launched > {marker_arg}) >> /Next 6 0 R >>"
        ),
        "<< /S /JavaScript /JS (app.launchURL\\('https://example.invalid/', true\\); app.alert\\('x'\\);) >>"
            .to_owned(),
        "<< /Type /Annot /Subtype /Link /Rect [0 0 200 200] /A << /S /URI /URI (https://example.invalid/track) >> >>"
            .to_owned(),
        // An image whose data is said to live in an external file.
        format!(
            "<< /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 \
             /F << /Type /Filespec /F ({secret_arg}) >> /Length 0 >>\nstream\n\nendstream"
        ),
    ];
    f.write("active.pdf", &pdf_from_objects(&objects, ""));
    let info = f.open("active.pdf").unwrap();
    assert_eq!(info.page_count, 1);
    let png = f.page(info.token, 0, 2.0).unwrap();
    assert_eq!(png_size(&png), (400, 400));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!marker.exists(), "the launch action was not run");
}
