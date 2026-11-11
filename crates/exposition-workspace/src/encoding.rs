//! Text decoding and encoding for project files.
//!
//! Supported encodings: UTF-8 (with or without BOM) and UTF-16 LE/BE with a
//! BOM. Decoding is strict: a file that is binary or not valid in its detected
//! encoding is never opened as text, so invalid bytes are never replaced.
//! `encode(decode(bytes))` reproduces `bytes` exactly for every accepted file.

use exposition_core::document::TextEncoding;

/// Largest file opened as a text document.
pub const MAX_TEXT_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The content looks binary (NUL bytes/characters or an unsupported BOM).
    Binary,
    /// The bytes are not valid in the detected encoding.
    Undecodable { encoding: TextEncoding },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub text: String,
    pub encoding: TextEncoding,
}

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16LE_BOM: &[u8] = &[0xFF, 0xFE];
const UTF16BE_BOM: &[u8] = &[0xFE, 0xFF];
const UTF32LE_BOM: &[u8] = &[0xFF, 0xFE, 0x00, 0x00];
const UTF32BE_BOM: &[u8] = &[0x00, 0x00, 0xFE, 0xFF];

pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    if bytes.starts_with(UTF32LE_BOM) || bytes.starts_with(UTF32BE_BOM) {
        return Err(DecodeError::Binary);
    }
    let decoded = if let Some(rest) = bytes.strip_prefix(UTF8_BOM) {
        let text =
            std::str::from_utf8(rest).map_err(|_| DecodeError::Undecodable { encoding: TextEncoding::Utf8Bom })?;
        Decoded { text: text.to_owned(), encoding: TextEncoding::Utf8Bom }
    } else if let Some(rest) = bytes.strip_prefix(UTF16LE_BOM) {
        Decoded { text: decode_utf16(rest, TextEncoding::Utf16Le)?, encoding: TextEncoding::Utf16Le }
    } else if let Some(rest) = bytes.strip_prefix(UTF16BE_BOM) {
        Decoded { text: decode_utf16(rest, TextEncoding::Utf16Be)?, encoding: TextEncoding::Utf16Be }
    } else {
        if bytes.contains(&0) {
            return Err(DecodeError::Binary);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| DecodeError::Undecodable { encoding: TextEncoding::Utf8 })?;
        Decoded { text: text.to_owned(), encoding: TextEncoding::Utf8 }
    };
    if decoded.text.contains('\0') {
        return Err(DecodeError::Binary);
    }
    Ok(decoded)
}

fn decode_utf16(bytes: &[u8], encoding: TextEncoding) -> Result<String, DecodeError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(DecodeError::Undecodable { encoding });
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| match encoding {
            TextEncoding::Utf16Be => u16::from_be_bytes([pair[0], pair[1]]),
            _ => u16::from_le_bytes([pair[0], pair[1]]),
        })
        .collect();
    String::from_utf16(&units).map_err(|_| DecodeError::Undecodable { encoding })
}

/// Encode `text` for saving, including the BOM the encoding requires.
pub fn encode(text: &str, encoding: TextEncoding) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 => text.as_bytes().to_vec(),
        TextEncoding::Utf8Bom => {
            let mut out = Vec::with_capacity(text.len() + 3);
            out.extend_from_slice(UTF8_BOM);
            out.extend_from_slice(text.as_bytes());
            out
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let big = encoding == TextEncoding::Utf16Be;
            let mut out = Vec::with_capacity(2 + text.len() * 2);
            out.extend_from_slice(if big { UTF16BE_BOM } else { UTF16LE_BOM });
            for unit in text.encode_utf16() {
                out.extend_from_slice(&if big { unit.to_be_bytes() } else { unit.to_le_bytes() });
            }
            out
        }
    }
}

pub fn encoding_label(encoding: TextEncoding) -> &'static str {
    match encoding {
        TextEncoding::Utf8 => "UTF-8",
        TextEncoding::Utf8Bom => "UTF-8 with BOM",
        TextEncoding::Utf16Le => "UTF-16 LE",
        TextEncoding::Utf16Be => "UTF-16 BE",
    }
}

/// Language identifier for a file name (by extension).
pub fn language_for(file_name: &str) -> &'static str {
    let ext = file_name.rfind('.').filter(|&i| i > 0).map(|i| file_name[i + 1..].to_ascii_lowercase());
    match ext.as_deref() {
        Some("typ") => "typst",
        Some("tex" | "sty" | "cls" | "ltx" | "dtx" | "ins" | "bbx" | "cbx") => "latex",
        Some("bib") => "bibtex",
        Some("md" | "markdown") => "markdown",
        Some("py" | "pyw") => "python",
        Some("r") => "r",
        Some("jl") => "julia",
        Some("lean") => "lean",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        _ => "plaintext",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(bytes: &[u8], expected: TextEncoding) -> String {
        let decoded = decode(bytes).unwrap();
        assert_eq!(decoded.encoding, expected);
        assert_eq!(encode(&decoded.text, decoded.encoding), bytes, "round trip must be byte-exact");
        decoded.text
    }

    #[test]
    fn decodes_and_reencodes_supported_encodings_exactly() {
        assert_eq!(round_trip(b"a\r\nb\r\n", TextEncoding::Utf8), "a\r\nb\r\n");
        assert_eq!(round_trip("\u{feff}α\n".as_bytes(), TextEncoding::Utf8Bom), "α\n");
        // A second BOM-like character after the BOM is content and is kept.
        assert_eq!(round_trip("\u{feff}\u{feff}x".as_bytes(), TextEncoding::Utf8Bom), "\u{feff}x");
        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain("h😀\r\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(round_trip(&le, TextEncoding::Utf16Le), "h😀\r\n");
        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain("h😀\n".encode_utf16().flat_map(u16::to_be_bytes)).collect();
        assert_eq!(round_trip(&be, TextEncoding::Utf16Be), "h😀\n");
        assert_eq!(round_trip(b"", TextEncoding::Utf8), "");
    }

    #[test]
    fn refuses_binary_and_invalid_bytes() {
        assert_eq!(decode(b"PNG\0\x01\x02"), Err(DecodeError::Binary));
        assert_eq!(decode(b"caf\xe9"), Err(DecodeError::Undecodable { encoding: TextEncoding::Utf8 }));
        assert_eq!(decode(b"\xEF\xBB\xBFok\xff"), Err(DecodeError::Undecodable { encoding: TextEncoding::Utf8Bom }));
        // Odd byte count and an unpaired surrogate in UTF-16.
        assert_eq!(decode(&[0xFF, 0xFE, 0x41]), Err(DecodeError::Undecodable { encoding: TextEncoding::Utf16Le }));
        assert_eq!(
            decode(&[0xFF, 0xFE, 0x00, 0xD8, 0x41, 0x00]),
            Err(DecodeError::Undecodable { encoding: TextEncoding::Utf16Le })
        );
        assert_eq!(decode(&[0xFF, 0xFE, 0x00, 0x00, 0x41, 0x00, 0x00, 0x00]), Err(DecodeError::Binary));
        assert_eq!(decode(&[0xFE, 0xFF, 0x00, 0x00]), Err(DecodeError::Binary));
    }

    #[test]
    fn maps_extensions_to_languages() {
        for (name, lang) in [
            ("main.typ", "typst"),
            ("paper.TEX", "latex"),
            ("refs.bib", "bibtex"),
            ("README.md", "markdown"),
            ("analysis.py", "python"),
            ("model.R", "r"),
            ("sim.jl", "julia"),
            ("Proof.lean", "lean"),
            ("data.json", "json"),
            ("Cargo.toml", "toml"),
            ("ci.yml", "yaml"),
            ("notes.txt", "plaintext"),
            (".gitignore", "plaintext"),
        ] {
            assert_eq!(language_for(name), lang, "{name}");
        }
    }
}
