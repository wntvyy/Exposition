//! Controlled output registration: what a batch wrote to its output area
//! becomes project artifacts only after validation.
//!
//! Accepted: regular files directly in the output folder, with a safe name
//! (`[A-Za-z0-9._-]{1,100}`, no leading dot, no trailing dot, no reserved
//! Windows device name, an exported extension), at most [`MAX_FILES`] files,
//! each at most [`ARTIFACT_SIZE_LIMIT`] and together at most the batch's
//! output budget, with exactly one name (no hard links), and content that
//! matches the claimed type (magic bytes for PNG/JPEG/GIF/PDF; UTF-8 text for
//! SVG/HTML/CSV/TSV/JSON/TXT). Links, junctions/reparse points, folders and
//! special files are rejected without being followed. Accepted files are
//! copied (never renamed across the boundary) into
//! `.exposition/artifacts/<run>/` with their SHA-256. SVG/HTML are stored as
//! inert text and rendered as images/text by the existing viewers.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use exposition_core::artifact::{ArtifactId, ArtifactKind, ArtifactRecord};

use super::fsx::checked_regular_file;
use crate::batch::{BatchPort, BatchPortOutput, BatchRejected};
use crate::error::ResearchError;
use crate::helper::sha256_hex;
use crate::runs::{ARTIFACT_SIZE_LIMIT, media_type_for};
use crate::store::{artifact_dir_path, ensure_dir_within};

/// Files accepted per batch.
pub const MAX_FILES: usize = 32;
/// Directory entries inspected at most (the rest are rejected unread).
const MAX_ENTRIES: usize = 256;
/// Longest accepted name.
const MAX_NAME: usize = 100;
/// Extensions exported from a batch output area.
pub const EXPORTED_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "svg", "pdf", "csv", "tsv", "json", "txt", "html"];
/// Names a run's artifact folder uses itself.
const RESERVED: &[&str] = &["transcript.json"];

/// Why `name` is not a safe output name, if it is not.
pub(crate) fn unsafe_name(name: &str) -> Option<String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Some(format!("the name must have 1 to {MAX_NAME} characters"));
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) {
        return Some("the name may only contain letters, digits, '.', '_' and '-'".into());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Some("hidden names and names ending in '.' are not exported".into());
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved_device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved_device {
        return Some("a reserved Windows device name".into());
    }
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        return Some("the name is reserved for the run's own records".into());
    }
    match extension(name) {
        Some(ext) if EXPORTED_EXTENSIONS.contains(&ext.as_str()) => None,
        Some(ext) => {
            Some(format!("files of type .{ext} are not exported (allowed: {})", EXPORTED_EXTENSIONS.join(", ")))
        }
        None => Some("the name has no file extension".into()),
    }
}

/// Lowercase extension of a name.
pub(crate) fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Whether `name` could be a batch artifact (safe name, exported type).
pub(crate) fn is_batch_artifact_name(name: &str) -> bool {
    unsafe_name(name).is_none()
}

/// Why `bytes` do not match the type `ext` claims, if they do not.
pub(crate) fn content_mismatch(ext: &str, bytes: &[u8]) -> Option<String> {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    let ok = match ext {
        "png" => starts(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => starts(&[0xFF, 0xD8, 0xFF]),
        "gif" => starts(b"GIF87a") || starts(b"GIF89a"),
        "pdf" => starts(b"%PDF-"),
        "svg" => std::str::from_utf8(bytes).is_ok_and(|text| text.contains("<svg") && !text.contains('\0')),
        "json" => serde_json::from_slice::<serde_json::Value>(bytes).is_ok(),
        "csv" | "tsv" | "txt" | "html" => std::str::from_utf8(bytes).is_ok_and(|text| !text.contains('\0')),
        _ => false,
    };
    (!ok).then(|| match ext {
        "png" | "jpg" | "jpeg" | "gif" | "pdf" => format!("the content is not a {} file", ext.to_ascii_uppercase()),
        "svg" => "the content is not SVG text".into(),
        "json" => "the content is not valid JSON".into(),
        _ => "the content is not UTF-8 text".into(),
    })
}

fn kind_for(ext: &str) -> ArtifactKind {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "pdf" => ArtifactKind::Figure,
        "csv" | "tsv" => ArtifactKind::Table,
        _ => ArtifactKind::Data,
    }
}

/// A validated output file, read into memory (bounded).
#[derive(Debug)]
pub(crate) struct Validated {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Inspect the output area (never following links) and validate each entry.
pub(crate) fn validate(out_dir: &Path, max_total: u64) -> (Vec<Validated>, Vec<BatchRejected>) {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let reject = |rejected: &mut Vec<BatchRejected>, name: &str, reason: String| {
        rejected.push(BatchRejected { name: name.chars().take(200).collect(), reason });
    };
    let Ok(entries) = std::fs::read_dir(out_dir) else { return (accepted, rejected) };
    let mut names: Vec<(String, std::path::PathBuf, bool)> = Vec::new();
    for (i, entry) in entries.flatten().enumerate() {
        let raw = entry.file_name();
        if i >= MAX_ENTRIES {
            reject(&mut rejected, &raw.to_string_lossy(), format!("more than {MAX_ENTRIES} entries; not inspected"));
            continue;
        }
        match raw.to_str() {
            Some(name) => names.push((name.to_owned(), entry.path(), true)),
            None => names.push((raw.to_string_lossy().into_owned(), entry.path(), false)),
        }
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    let mut total: u64 = 0;
    for (name, path, utf8) in names {
        if !utf8 {
            reject(&mut rejected, &name, "the name is not valid Unicode".into());
            continue;
        }
        // The kind of entry first (a link is reported as a link, whatever its name).
        let (mut file, len) = match checked_regular_file(&path) {
            Ok(v) => v,
            Err(reason) => {
                reject(&mut rejected, &name, reason);
                continue;
            }
        };
        if let Some(reason) = unsafe_name(&name) {
            reject(&mut rejected, &name, reason);
            continue;
        }
        if accepted.len() >= MAX_FILES {
            reject(&mut rejected, &name, format!("more than {MAX_FILES} output files"));
            continue;
        }
        if len > ARTIFACT_SIZE_LIMIT {
            reject(&mut rejected, &name, format!("larger than {} MiB", ARTIFACT_SIZE_LIMIT / (1024 * 1024)));
            continue;
        }
        if total + len > max_total {
            reject(&mut rejected, &name, format!("the outputs exceed the batch's output budget ({max_total} bytes)"));
            continue;
        }
        let mut bytes = Vec::with_capacity(len as usize);
        let limit = ARTIFACT_SIZE_LIMIT.min(max_total - total);
        if let Err(e) = (&mut file).take(limit + 1).read_to_end(&mut bytes) {
            reject(&mut rejected, &name, format!("could not be read ({e})"));
            continue;
        }
        if bytes.len() as u64 > limit {
            reject(&mut rejected, &name, "grew beyond the size limit while it was read".into());
            continue;
        }
        let ext = extension(&name).unwrap_or_default();
        if let Some(reason) = content_mismatch(&ext, &bytes) {
            reject(&mut rejected, &name, reason);
            continue;
        }
        total += bytes.len() as u64;
        accepted.push(Validated { name, bytes });
    }
    (accepted, rejected)
}

/// Copy validated files into the run's artifact folder. Text artifacts are
/// redacted like stream output first. Returns their records.
pub(crate) fn register(
    root: &Path,
    run_id: &str,
    files: Vec<Validated>,
    redact: &dyn Fn(&str) -> String,
) -> Result<Vec<ArtifactRecord>, ResearchError> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let dir = ensure_dir_within(root, &artifact_dir_path(run_id))?;
    let mut records = Vec::with_capacity(files.len());
    for file in files {
        let media_type = media_type_for(&file.name).to_owned();
        let bytes =
            if media_type.starts_with("text/") || media_type == "image/svg+xml" || media_type == "application/json" {
                match String::from_utf8(file.bytes) {
                    Ok(text) => redact(&text).into_bytes(),
                    Err(e) => e.into_bytes(),
                }
            } else {
                file.bytes
            };
        let target = dir.join(&file.name);
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| ResearchError::io(&format!("storing output {}", file.name), e))?;
        out.write_all(&bytes)
            .and_then(|_| out.flush())
            .map_err(|e| ResearchError::io(&format!("storing output {}", file.name), e))?;
        let ext = extension(&file.name).unwrap_or_default();
        let path = artifact_dir_path(run_id)
            .join(&file.name)
            .map_err(|e| ResearchError::InvalidRequest(format!("output {}: {e}", file.name)))?;
        records.push(ArtifactRecord {
            id: ArtifactId(format!("{run_id}/{}", file.name)),
            kind: kind_for(&ext),
            path: Some(path),
            scratch_path: None,
            media_type,
            size_bytes: bytes.len() as u64,
            content_hash: format!("sha256:{}", sha256_hex(&bytes)),
        });
    }
    Ok(records)
}

/// Map accepted artifacts to output ports: port `p` is realized by `p.<ext>`
/// with an extension the port accepts (the port's first listed extension
/// wins when several exist). Everything else is `extra`.
pub(crate) fn map_ports(
    ports: &[BatchPort],
    records: Vec<ArtifactRecord>,
) -> (Vec<BatchPortOutput>, Vec<ArtifactRecord>) {
    let mut remaining: Vec<Option<ArtifactRecord>> = records.into_iter().map(Some).collect();
    let mut mapped = Vec::new();
    for port in ports {
        let mut chosen: Option<usize> = None;
        for wanted in &port.extensions {
            let wanted = wanted.trim_start_matches('.').to_ascii_lowercase();
            let found = remaining.iter().position(|r| {
                r.as_ref().is_some_and(|r| {
                    let name = r.id.0.rsplit('/').next().unwrap_or("");
                    name.rsplit_once('.')
                        .is_some_and(|(stem, ext)| stem == port.port && ext.eq_ignore_ascii_case(&wanted))
                })
            });
            if found.is_some() {
                chosen = found;
                break;
            }
        }
        if let Some(index) = chosen
            && let Some(artifact) = remaining[index].take()
        {
            mapped.push(BatchPortOutput { port: port.port.clone(), artifact });
        }
    }
    (mapped, remaining.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn out() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("out");
        std::fs::create_dir_all(&dir).unwrap();
        (tmp, dir)
    }

    fn reason<'a>(rejected: &'a [BatchRejected], name: &str) -> &'a str {
        &rejected.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("{name} not rejected: {rejected:?}")).reason
    }

    #[test]
    fn names_are_restricted() {
        for ok in ["table.csv", "figure.svg", "fig-1_a.PNG", "data.json", "notes.txt", "a.b.csv"] {
            assert_eq!(unsafe_name(ok), None, "{ok}");
        }
        for bad in [
            "",
            ".hidden.csv",
            "a.csv.",
            "../x.csv",
            "a/b.csv",
            "a b.csv",
            "con.csv",
            "NUL.txt",
            "com1.png",
            "lpt9.csv",
            "transcript.json",
            "run.exe",
            "noext",
            "naïve.csv",
            &format!("{}.csv", "a".repeat(100)),
        ] {
            assert!(unsafe_name(bad).is_some(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn content_must_match_the_claimed_type() {
        assert_eq!(content_mismatch("png", PNG), None);
        assert!(content_mismatch("png", b"<svg/>").is_some());
        assert!(content_mismatch("jpg", PNG).is_some());
        assert_eq!(content_mismatch("jpg", &[0xFF, 0xD8, 0xFF, 0xE0]), None);
        assert_eq!(content_mismatch("gif", b"GIF89a...."), None);
        assert_eq!(content_mismatch("pdf", b"%PDF-1.7\n"), None);
        assert!(content_mismatch("pdf", b"MZ\x90\0").is_some());
        assert_eq!(content_mismatch("svg", b"<?xml version='1.0'?><svg xmlns='http://www.w3.org/2000/svg'/>"), None);
        assert!(content_mismatch("svg", PNG).is_some());
        assert!(content_mismatch("csv", b"a,b\n\xff\xfe").is_some());
        assert_eq!(content_mismatch("json", b"{\"a\": 1}"), None);
        assert!(content_mismatch("json", b"{oops").is_some());
    }

    #[test]
    fn regular_files_are_accepted_and_everything_else_rejected() {
        let (_tmp, dir) = out();
        std::fs::write(dir.join("table.csv"), "a,b\n1,2\n").unwrap();
        std::fs::write(dir.join("figure.svg"), "<svg xmlns='http://www.w3.org/2000/svg'></svg>").unwrap();
        std::fs::write(dir.join("plot.png"), PNG).unwrap();
        std::fs::write(dir.join("fake.png"), "<html><script>alert(1)</script></html>").unwrap();
        std::fs::write(dir.join("big.csv"), vec![b'1'; 2048]).unwrap();
        std::fs::write(dir.join("run.exe"), "MZ").unwrap();
        std::fs::write(dir.join(".hidden.csv"), "x").unwrap();
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested").join("inner.csv"), "x").unwrap();
        std::fs::write(dir.join("original.csv"), "1").unwrap();
        std::fs::hard_link(dir.join("original.csv"), dir.join("second.csv")).unwrap();
        let (accepted, rejected) = validate(&dir, 1024);
        let names: Vec<&str> = accepted.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["figure.svg", "plot.png", "table.csv"], "{rejected:?}");
        assert!(reason(&rejected, "fake.png").contains("not a PNG"));
        assert!(reason(&rejected, "big.csv").contains("budget"));
        assert!(reason(&rejected, "run.exe").contains("not exported"));
        assert!(reason(&rejected, ".hidden.csv").contains("hidden"));
        assert!(reason(&rejected, "nested").contains("directory"));
        assert!(reason(&rejected, "original.csv").contains("hard link"));
        assert!(reason(&rejected, "second.csv").contains("hard link"));
    }

    #[test]
    fn counts_and_sizes_are_bounded() {
        let (_tmp, dir) = out();
        for i in 0..(MAX_FILES + 3) {
            std::fs::write(dir.join(format!("f{i:02}.txt")), "x").unwrap();
        }
        let (accepted, rejected) = validate(&dir, 1 << 20);
        assert_eq!(accepted.len(), MAX_FILES);
        assert_eq!(rejected.len(), 3);
        assert!(rejected.iter().all(|r| r.reason.contains("more than")));
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_rejected_without_being_followed() {
        let (tmp, dir) = out();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.csv"), "s").unwrap();
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.join("escape.csv"))
            .arg(&outside)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J failed");
        let (accepted, rejected) = validate(&dir, 1 << 20);
        assert!(accepted.is_empty());
        assert!(reason(&rejected, "escape.csv").contains("junction"), "{rejected:?}");
    }

    #[test]
    fn symlinks_are_rejected_when_links_can_be_made() {
        let (tmp, dir) = out();
        let outside = tmp.path().join("secret.csv");
        std::fs::write(&outside, "s").unwrap();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&outside, dir.join("table.csv"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, dir.join("table.csv"));
        if let Err(e) = made {
            eprintln!("SKIPPED symlink case: creating a symbolic link is not permitted here ({e})");
            return;
        }
        let (accepted, rejected) = validate(&dir, 1 << 20);
        assert!(accepted.is_empty());
        assert!(reason(&rejected, "table.csv").contains("link"), "{rejected:?}");
    }

    #[test]
    fn registration_copies_hashes_and_maps_ports() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let files = vec![
            Validated { name: "figure.png".into(), bytes: PNG.to_vec() },
            Validated { name: "figure.svg".into(), bytes: b"<svg>secret-token</svg>".to_vec() },
            Validated { name: "table.csv".into(), bytes: b"a\n1\n".to_vec() },
            Validated { name: "notes.txt".into(), bytes: b"n".to_vec() },
        ];
        let redact = |t: &str| t.replace("secret-token", "[REDACTED]");
        let records = register(&root, "run-abc", files, &redact).unwrap();
        let svg = records.iter().find(|r| r.id.0 == "run-abc/figure.svg").unwrap();
        let stored = std::fs::read(root.join(".exposition/artifacts/run-abc/figure.svg")).unwrap();
        assert_eq!(stored, b"<svg>[REDACTED]</svg>");
        assert_eq!(svg.content_hash, format!("sha256:{}", sha256_hex(&stored)));
        assert_eq!(svg.media_type, "image/svg+xml");
        assert_eq!(svg.kind, ArtifactKind::Figure);
        let ports = vec![
            BatchPort { port: "figure".into(), extensions: vec!["svg".into(), "png".into()] },
            BatchPort { port: "table".into(), extensions: vec![".csv".into()] },
            BatchPort { port: "missing".into(), extensions: vec!["csv".into()] },
        ];
        let (mapped, extra) = map_ports(&ports, records);
        let pairs: Vec<(&str, &str)> = mapped.iter().map(|m| (m.port.as_str(), m.artifact.id.0.as_str())).collect();
        assert_eq!(pairs, [("figure", "run-abc/figure.svg"), ("table", "run-abc/table.csv")]);
        let extra: Vec<&str> = extra.iter().map(|r| r.id.0.as_str()).collect();
        assert_eq!(extra, ["run-abc/figure.png", "run-abc/notes.txt"]);
        // Registered artifacts are reachable through the run-artifact check.
        let path = exposition_core::path::WorkspacePath::parse(".exposition/artifacts/run-abc/table.csv").unwrap();
        assert!(crate::runs::artifact_belongs_to_run(&path, "run-abc"));
    }
}
