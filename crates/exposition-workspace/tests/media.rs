//! Project images for view-only display (`exposition_workspace::media`):
//! scope, links/junctions, extension allow-list, size cap, and project binding.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use exposition_core::path::WorkspacePath;
use exposition_workspace::media::{ImageLocation, MAX_IMAGE_BYTES};
use exposition_workspace::{DocumentService, WorkspaceService};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";

fn wp(s: &str) -> WorkspacePath {
    WorkspacePath::parse(s).unwrap()
}

struct Fixture {
    dir: tempfile::TempDir,
    service: Arc<WorkspaceService>,
}

impl Fixture {
    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }
    fn file(&self, p: &str) -> PathBuf {
        self.project().join(p)
    }
    fn outside(&self) -> PathBuf {
        self.dir.path().join("outside")
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("figs")).unwrap();
    fs::create_dir_all(dir.path().join("outside")).unwrap();
    fs::write(project.join("figs/plot one.png"), PNG).unwrap();
    fs::write(project.join("figs/vector.svg"), b"<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
    fs::write(project.join("figs/scan.tiff"), b"II*\0").unwrap();
    fs::write(project.join("figs/diagram.pdf"), b"%PDF-1.7").unwrap();
    fs::write(project.join("secret.env"), b"TOKEN=x").unwrap();
    fs::write(dir.path().join("outside/escape.png"), PNG).unwrap();
    let service = WorkspaceService::new(Arc::new(DocumentService::new()), dir.path().join("data"));
    service.open_workspace(&project).unwrap();
    Fixture { dir, service }
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
fn reads_project_images_exactly() {
    let f = fixture();
    assert_eq!(f.service.read_image(&wp("figs/plot one.png")).unwrap(), PNG);
    assert!(f.service.read_image(&wp("figs/vector.svg")).unwrap().starts_with(b"<svg"));
    // Reading never writes: the file is unchanged and no document is opened.
    assert_eq!(fs::read(f.file("figs/plot one.png")).unwrap(), PNG);
    assert!(f.service.documents().list().is_empty());
}

#[test]
fn refuses_files_that_are_not_images() {
    let f = fixture();
    assert_eq!(f.service.read_image(&wp("secret.env")).unwrap_err().code, "image.notImage");
    assert_eq!(f.service.read_image(&wp("figs/scan.tiff")).unwrap_err().code, "image.unsupported");
    assert_eq!(f.service.read_image(&wp("figs/diagram.pdf")).unwrap_err().code, "image.unsupported");
    assert_eq!(f.service.read_image(&wp("figs/missing.png")).unwrap_err().code, "path.notFound");
    fs::create_dir_all(f.file("dir.png")).unwrap();
    assert_eq!(f.service.read_image(&wp("dir.png")).unwrap_err().code, "file.isDirectory");
}

#[test]
fn refuses_images_over_the_cap_unread() {
    let f = fixture();
    let big = fs::File::create(f.file("big.png")).unwrap();
    big.set_len(MAX_IMAGE_BYTES + 1).unwrap();
    drop(big);
    let error = f.service.read_image(&wp("big.png")).unwrap_err();
    assert_eq!(error.code, "image.tooLarge");
    assert!(error.message.contains("32 MB"), "{}", error.message);
    // Exactly at the cap is read.
    let edge = fs::File::create(f.file("edge.png")).unwrap();
    edge.set_len(MAX_IMAGE_BYTES).unwrap();
    drop(edge);
    assert_eq!(f.service.read_image(&wp("edge.png")).unwrap().len() as u64, MAX_IMAGE_BYTES);
}

#[test]
fn links_and_junctions_out_of_the_folder_are_refused() {
    let f = fixture();
    junction(&f.file("out-link"), &f.outside());
    let error = f.service.read_image(&wp("out-link/escape.png")).unwrap_err();
    assert_eq!(error.code, "path.outsideWorkspace");
    assert_eq!(f.service.locate_image(&[wp("out-link/escape.png")]).unwrap(), ImageLocation::Blocked);
    // A junction inside the folder is fine and reports the canonical path.
    junction(&f.file("figs-link"), &f.file("figs"));
    assert_eq!(f.service.read_image(&wp("figs-link/plot one.png")).unwrap(), PNG);
    assert_eq!(
        f.service.locate_image(&[wp("figs-link/plot one.png")]).unwrap(),
        ImageLocation::Found { path: wp("figs/plot one.png") }
    );
}

#[test]
fn locates_the_first_existing_candidate() {
    let f = fixture();
    let found = f.service.locate_image(&[wp("figs/plot one.pdf"), wp("figs/plot one.png")]).unwrap();
    assert_eq!(found, ImageLocation::Found { path: wp("figs/plot one.png") });
    assert_eq!(
        f.service.locate_image(&[wp("figs/diagram.pdf"), wp("figs/diagram.png")]).unwrap(),
        ImageLocation::Unsupported { path: wp("figs/diagram.pdf") }
    );
    assert_eq!(f.service.locate_image(&[wp("nope.png")]).unwrap(), ImageLocation::Missing);
    assert_eq!(f.service.locate_image(&[wp("figs")]).unwrap(), ImageLocation::Missing, "folders are not images");
}

#[test]
fn bound_to_the_open_project() {
    let f = fixture();
    f.service.close_workspace();
    assert_eq!(f.service.read_image(&wp("figs/plot one.png")).unwrap_err().code, "workspace.notOpen");
    assert_eq!(f.service.locate_image(&[wp("figs/plot one.png")]).unwrap_err().code, "workspace.notOpen");
    // Another project: the same relative path is answered from that project only.
    let other = f.dir.path().join("other");
    fs::create_dir_all(other.join("figs")).unwrap();
    f.service.open_workspace(&other).unwrap();
    assert_eq!(f.service.read_image(&wp("figs/plot one.png")).unwrap_err().code, "path.notFound");
}

#[test]
fn reads_project_pdf_files_only_within_scope_and_size() {
    use exposition_workspace::media::MAX_PDF_BYTES;
    let f = fixture();
    let (root, bytes) = f.service.read_pdf(&wp("figs/diagram.pdf")).unwrap();
    assert_eq!(bytes, b"%PDF-1.7");
    assert_eq!(Some(root), f.service.open_root());
    fs::write(f.file("figs/UPPER.PDF"), b"%PDF-1.4").unwrap();
    assert_eq!(f.service.read_pdf(&wp("figs/UPPER.PDF")).unwrap().1, b"%PDF-1.4");
    // Not a way to read other project files as bytes.
    assert_eq!(f.service.read_pdf(&wp("secret.env")).unwrap_err().code, "pdf.notPdf");
    assert_eq!(f.service.read_pdf(&wp("figs/plot one.png")).unwrap_err().code, "pdf.notPdf");
    assert_eq!(f.service.read_pdf(&wp("figs/missing.pdf")).unwrap_err().code, "path.notFound");
    fs::create_dir_all(f.file("dir.pdf")).unwrap();
    assert_eq!(f.service.read_pdf(&wp("dir.pdf")).unwrap_err().code, "file.isDirectory");
    // Links and junctions out of the folder are refused.
    fs::write(f.outside().join("out.pdf"), b"%PDF-1.7").unwrap();
    junction(&f.file("out-link"), &f.outside());
    assert_eq!(f.service.read_pdf(&wp("out-link/out.pdf")).unwrap_err().code, "path.outsideWorkspace");
    // Size cap: refused unread above it, read at it.
    let big = fs::File::create(f.file("big.pdf")).unwrap();
    big.set_len(MAX_PDF_BYTES + 1).unwrap();
    drop(big);
    assert_eq!(f.service.read_pdf(&wp("big.pdf")).unwrap_err().code, "pdf.tooLarge");
    // Bound to the open project.
    f.service.close_workspace();
    assert_eq!(f.service.read_pdf(&wp("figs/diagram.pdf")).unwrap_err().code, "workspace.notOpen");
}
