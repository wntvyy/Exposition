//! Chat attachments: files and pasted images sent to the model with a message.
//!
//! Content is snapshotted at ingestion into a content-addressed store
//! (`<data>/agents/attachments/<sha256>`), so later edits to a source file
//! never change a queued or sent message. Attaching never imports anything
//! into the project, promotes research artifacts, or grants file access: the
//! agent sees the snapshot only. Files enter through the backend's native file
//! dialog (paths never come from the webview) or as pasted bytes.
//!
//! Supported: UTF-8 text/source files, PNG/JPEG/GIF/WebP images, and PDF
//! documents (when the model accepts them). Anything else is rejected with a
//! reason; nothing is silently omitted, truncated, or executed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

/// Largest text/source attachment (bytes).
pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;
/// Largest image (bytes). Providers limit an inline image to 5 MiB of
/// base64 data, which is 3.75 MiB of image bytes.
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024 / 4 * 3;
/// Largest image side (pixels).
pub const MAX_IMAGE_SIDE: u32 = 8000;
/// Largest PDF (bytes): one always fits a request once encoded
/// (see [`MAX_REQUEST_BYTES`]).
pub const MAX_PDF_BYTES: u64 = 20 * 1024 * 1024;
/// Attachments per message.
pub const MAX_PER_MESSAGE: usize = 20;
/// Encoded attachment bytes in one request, the replayed history included.
pub const MAX_REQUEST_BYTES: u64 = 30 * 1024 * 1024;
/// Anthropic: with more than this many images in one request, each image may
/// be at most [`MANY_IMAGES_MAX_SIDE`] pixels on a side.
pub const MANY_IMAGES: usize = 20;
pub const MANY_IMAGES_MAX_SIDE: u32 = 2000;
/// Longest display name kept.
const MAX_NAME_CHARS: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AttachmentId(pub String);

impl AttachmentId {
    pub fn new() -> Self {
        Self(format!("att-{}", uuid::Uuid::new_v4().simple()))
    }

    pub fn is_valid(&self) -> bool {
        self.0.len() <= 64
            && self.0.starts_with("att-")
            && self.0.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    }
}

impl Default for AttachmentId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentKind {
    Text,
    Image,
    Pdf,
}

impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AttachmentKind::Text => "text",
            AttachmentKind::Image => "image",
            AttachmentKind::Pdf => "pdf",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(AttachmentKind::Text),
            "image" => Some(AttachmentKind::Image),
            "pdf" => Some(AttachmentKind::Pdf),
            _ => None,
        }
    }
}

/// An ingested attachment (an immutable snapshot).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInfo {
    pub id: AttachmentId,
    /// Safe display name (no folders).
    pub name: String,
    pub media_type: String,
    pub kind: AttachmentKind,
    pub size_bytes: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Content digest (hex SHA-256): identical content has the same digest.
    pub sha256: String,
    /// The snapshot's content is missing (deleted outside the app).
    #[serde(default)]
    pub missing: bool,
}

/// Result of ingesting one file or pasted item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum IngestOutcome {
    Added { attachment: AttachmentInfo },
    Rejected { name: String, code: String, message: String },
}

/// What a file's bytes are, after checking them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub kind: AttachmentKind,
    pub media_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub code: &'static str,
    pub message: String,
}

fn reject(code: &'static str, message: impl Into<String>) -> Rejection {
    Rejection { code, message: message.into() }
}

/// A display name from a file name or a paste: last path component only,
/// control characters removed, bounded length; empty becomes a fallback.
pub fn safe_name(raw: &str, fallback: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let name = if cleaned.is_empty() { fallback } else { cleaned };
    if name.chars().count() <= MAX_NAME_CHARS {
        return name.to_owned();
    }
    // Keep the extension when shortening.
    let (stem, ext) = match name.rfind('.') {
        Some(i) if name.len() - i <= 12 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let keep = MAX_NAME_CHARS - ext.chars().count() - 1;
    format!("{}…{}", stem.chars().take(keep).collect::<String>(), ext)
}

/// Check bytes and decide what they are. `declared` is a media type the
/// webview reported for pasted data; content decides, never the declaration.
pub fn check(bytes: &[u8], name: &str, declared: Option<&str>) -> Result<Checked, Rejection> {
    if bytes.is_empty() {
        return Err(reject("attachments.empty", "The file is empty."));
    }
    if let Some((media_type, width, height)) = image_info(bytes) {
        let size = bytes.len() as u64;
        if size > MAX_IMAGE_BYTES {
            return Err(reject(
                "attachments.imageTooLarge",
                format!("The image is {}; images can be at most {}.", human_size(size), human_size(MAX_IMAGE_BYTES)),
            ));
        }
        let (Some(w), Some(h)) = (width, height) else {
            return Err(reject(
                "attachments.imageUnreadable",
                "The image's size could not be read; it may be damaged.",
            ));
        };
        if w == 0 || h == 0 || w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
            return Err(reject(
                "attachments.imageDimensions",
                format!("The image is {w}×{h} pixels; each side can be at most {MAX_IMAGE_SIDE}."),
            ));
        }
        return Ok(Checked { kind: AttachmentKind::Image, media_type: media_type.into(), width, height });
    }
    if bytes.starts_with(b"%PDF-") {
        let size = bytes.len() as u64;
        if size > MAX_PDF_BYTES {
            return Err(reject(
                "attachments.pdfTooLarge",
                format!("The PDF is {}; PDFs can be at most {}.", human_size(size), human_size(MAX_PDF_BYTES)),
            ));
        }
        return Ok(Checked {
            kind: AttachmentKind::Pdf,
            media_type: "application/pdf".into(),
            width: None,
            height: None,
        });
    }
    if declared.is_some_and(|d| d.starts_with("image/")) {
        return Err(reject(
            "attachments.unsupportedImage",
            "This image format is not supported; use PNG, JPEG, GIF, or WebP.",
        ));
    }
    if is_text(bytes) {
        let size = bytes.len() as u64;
        if size > MAX_TEXT_BYTES {
            return Err(reject(
                "attachments.textTooLarge",
                format!(
                    "The text file is {}; text files can be at most {}.",
                    human_size(size),
                    human_size(MAX_TEXT_BYTES)
                ),
            ));
        }
        return Ok(Checked {
            kind: AttachmentKind::Text,
            media_type: text_media_type(name).into(),
            width: None,
            height: None,
        });
    }
    Err(reject(
        "attachments.unsupported",
        "This kind of file cannot be attached. Exposition attaches text and source files, PNG/JPEG/GIF/WebP images, and PDF documents.",
    ))
}

fn text_media_type(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("md" | "markdown") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        _ => "text/plain",
    }
}

/// Valid UTF-8 (BOM allowed) without NUL bytes or many other control characters.
fn is_text(bytes: &[u8]) -> bool {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let Ok(text) = std::str::from_utf8(body) else { return false };
    let controls = text.chars().filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}')).count();
    !text.contains('\0') && controls * 100 <= text.chars().count().max(1)
}

pub fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// Image media type and dimensions from the header, for PNG/JPEG/GIF/WebP.
/// `None` when the bytes are none of these; dimensions `None` when unreadable.
pub fn image_info(b: &[u8]) -> Option<(&'static str, Option<u32>, Option<u32>)> {
    let be32 = |i: usize| b.get(i..i + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]));
    let le16 = |i: usize| b.get(i..i + 2).map(|s| u32::from(u16::from_le_bytes([s[0], s[1]])));
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let ok = b.get(12..16) == Some(b"IHDR");
        return Some(("image/png", be32(16).filter(|_| ok), be32(20).filter(|_| ok)));
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some(("image/gif", le16(6), le16(8)));
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        let dims = match b.get(12..16) {
            Some(b"VP8 ") => b.get(26..30).map(|s| {
                (
                    u32::from(u16::from_le_bytes([s[0], s[1]]) & 0x3fff),
                    u32::from(u16::from_le_bytes([s[2], s[3]]) & 0x3fff),
                )
            }),
            Some(b"VP8L") => b.get(21..25).map(|s| {
                let v = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                ((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1)
            }),
            Some(b"VP8X") => b.get(24..30).map(|s| {
                (
                    (u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16) + 1,
                    (u32::from(s[3]) | u32::from(s[4]) << 8 | u32::from(s[5]) << 16) + 1,
                )
            }),
            _ => None,
        };
        return Some(("image/webp", dims.map(|d| d.0), dims.map(|d| d.1)));
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
                i += 2;
                continue;
            }
            let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
            // SOF0..SOF15 except DHT (C4), JPG (C8), DAC (CC).
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = u32::from(u16::from_be_bytes([b[i + 5], b[i + 6]]));
                let w = u32::from(u16::from_be_bytes([b[i + 7], b[i + 8]]));
                return Some(("image/jpeg", Some(w), Some(h)));
            }
            if len < 2 {
                break;
            }
            i += 2 + len;
        }
        return Some(("image/jpeg", None, None));
    }
    None
}

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- base64

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Decode standard base64 (padding optional; whitespace and a `data:…;base64,`
/// prefix are tolerated). `None` on invalid input.
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let input = match input.find(";base64,") {
        Some(i) if input.starts_with("data:") => &input[i + 8..],
        _ => input,
    };
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut padding = false;
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding = true;
                continue;
            }
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        if padding {
            return None;
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 6 {
        return None;
    }
    Some(out)
}

// ---------------------------------------------------------------- content store

/// Content-addressed snapshot files. In memory for tests.
pub struct ContentStore {
    dir: Option<PathBuf>,
    memory: Mutex<HashMap<String, Vec<u8>>>,
}

impl ContentStore {
    pub fn on_disk(dir: PathBuf) -> Self {
        Self { dir: Some(dir), memory: Mutex::new(HashMap::new()) }
    }

    pub fn in_memory() -> Self {
        Self { dir: None, memory: Mutex::new(HashMap::new()) }
    }

    fn path(&self, sha256: &str) -> Option<PathBuf> {
        let valid = sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit());
        self.dir.as_ref().filter(|_| valid).map(|d| d.join(sha256))
    }

    /// Store bytes under their digest (idempotent; written atomically).
    pub fn put(&self, bytes: &[u8]) -> std::io::Result<String> {
        let digest = sha256_hex(bytes);
        match self.path(&digest) {
            Some(path) => {
                if !path.exists() {
                    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
                    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
                    std::fs::write(&tmp, bytes)?;
                    if let Err(e) = std::fs::rename(&tmp, &path) {
                        let _ = std::fs::remove_file(&tmp);
                        if !path.exists() {
                            return Err(e);
                        }
                    }
                }
            }
            None => {
                self.memory.lock().unwrap_or_else(|p| p.into_inner()).insert(digest.clone(), bytes.to_vec());
            }
        }
        Ok(digest)
    }

    pub fn get(&self, sha256: &str) -> Option<Vec<u8>> {
        match self.path(sha256) {
            Some(path) => std::fs::read(path).ok().filter(|b| sha256_hex(b) == sha256),
            None if self.dir.is_none() => self.memory.lock().unwrap_or_else(|p| p.into_inner()).get(sha256).cloned(),
            None => None,
        }
    }

    pub fn exists(&self, sha256: &str) -> bool {
        match self.path(sha256) {
            Some(path) => path.is_file(),
            None => self.memory.lock().unwrap_or_else(|p| p.into_inner()).contains_key(sha256),
        }
    }

    /// Remove snapshots whose digest is not in `keep`.
    pub fn retain(&self, keep: &std::collections::HashSet<String>) {
        match &self.dir {
            Some(dir) => {
                let Ok(entries) = std::fs::read_dir(dir) else { return };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let digest = name.split('.').next().unwrap_or("");
                    let stale_tmp = name.contains(".tmp-");
                    if stale_tmp || (digest.len() == 64 && !keep.contains(digest)) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            None => self.memory.lock().unwrap_or_else(|p| p.into_inner()).retain(|k, _| keep.contains(k)),
        }
    }
}

/// The canonical history block for an attachment (resolved to provider
/// content when a request is built).
pub fn reference_block(info: &AttachmentInfo) -> serde_json::Value {
    serde_json::json!({
        "type": "exposition_attachment",
        "attachment_id": info.id.0,
        "name": info.name,
        "media_type": info.media_type,
        "kind": info.kind.as_str(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0]);
        b
    }

    #[test]
    fn base64_round_trips_and_rejects_garbage() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 % 256) as u8).collect();
            let enc = base64_encode(&bytes);
            assert_eq!(base64_decode(&enc).unwrap(), bytes, "len {len}");
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_decode("data:image/png;base64,TWFu").unwrap(), b"Man");
        assert_eq!(base64_decode("TW\nFu").unwrap(), b"Man");
        assert!(base64_decode("TW!u").is_none());
        assert!(base64_decode("TW=u").is_none());
    }

    #[test]
    fn sniffs_images_and_reads_dimensions() {
        assert_eq!(image_info(&png(640, 480)), Some(("image/png", Some(640), Some(480))));
        let gif = [b"GIF89a".as_slice(), &[0x20, 0x03, 0x58, 0x02]].concat();
        assert_eq!(image_info(&gif), Some(("image/gif", Some(800), Some(600))));
        // JPEG: SOI, APP0 (len 16), SOF0 with height 300, width 400.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpeg.extend([0u8; 14]);
        jpeg.extend([0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0x2C, 0x01, 0x90, 0x03]);
        jpeg.extend([0u8; 12]);
        assert_eq!(image_info(&jpeg), Some(("image/jpeg", Some(400), Some(300))));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend([0u8; 8]);
        webp.extend([0x3F, 0x01, 0x00, 0xEF, 0x00, 0x00]); // 320 x 240
        assert_eq!(image_info(&webp), Some(("image/webp", Some(320), Some(240))));
        assert_eq!(image_info(b"%PDF-1.7"), None);
    }

    #[test]
    fn checks_kinds_sizes_and_dimensions() {
        let ok = check(&png(100, 50), "shot.png", Some("image/png")).unwrap();
        assert_eq!((ok.kind, ok.width, ok.height), (AttachmentKind::Image, Some(100), Some(50)));
        assert_eq!(check(&png(9000, 10), "big.png", None).unwrap_err().code, "attachments.imageDimensions");
        let mut huge = png(10, 10);
        huge.resize(MAX_IMAGE_BYTES as usize + 1, 0);
        assert_eq!(check(&huge, "huge.png", None).unwrap_err().code, "attachments.imageTooLarge");
        assert_eq!(check(b"%PDF-1.4\n...", "paper.pdf", None).unwrap().kind, AttachmentKind::Pdf);
        let text = check("\u{feff}α + β\n".as_bytes(), "notes.md", None).unwrap();
        assert_eq!((text.kind, text.media_type.as_str()), (AttachmentKind::Text, "text/markdown"));
        assert_eq!(check(&[0, 1, 2, 3, 0xff], "data.bin", None).unwrap_err().code, "attachments.unsupported");
        assert_eq!(check(b"BM\0\0", "x.bmp", Some("image/bmp")).unwrap_err().code, "attachments.unsupportedImage");
        assert_eq!(check(b"", "empty.txt", None).unwrap_err().code, "attachments.empty");
        // Content decides, not the declared type or name.
        assert_eq!(check(&png(1, 1), "notes.txt", Some("text/plain")).unwrap().kind, AttachmentKind::Image);
    }

    #[test]
    fn names_are_safe_and_bounded() {
        assert_eq!(safe_name("C:\\Users\\me\\data\\plot.png", "x"), "plot.png");
        assert_eq!(safe_name("../../etc/passwd", "x"), "passwd");
        assert_eq!(safe_name("  \u{7}  ", "Pasted image.png"), "Pasted image.png");
        let long = format!("{}.tex", "a".repeat(300));
        let short = safe_name(&long, "x");
        assert!(short.chars().count() <= MAX_NAME_CHARS && short.ends_with(".tex"), "{short}");
    }

    #[test]
    fn content_store_is_content_addressed_and_retains() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::on_disk(dir.path().join("attachments"));
        let a = store.put(b"alpha").unwrap();
        assert_eq!(store.put(b"alpha").unwrap(), a);
        let b = store.put(b"beta").unwrap();
        assert_eq!(store.get(&a).unwrap(), b"alpha");
        store.retain(&[a.clone()].into_iter().collect());
        assert!(store.exists(&a) && !store.exists(&b));
        assert!(store.get("../escape").is_none());
        // A tampered snapshot is not returned.
        std::fs::write(dir.path().join("attachments").join(&a), b"changed").unwrap();
        assert!(store.get(&a).is_none());
    }
}
