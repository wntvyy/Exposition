//! Re-anchoring explicit region bindings (`SourceSelector::Region`).
//!
//! A region chosen by a person or agent is identified by its exact text, not
//! by its range: its fingerprint records the text's byte length, a rolling
//! hash, and a SHA-256 prefix. After the file changes the region is searched
//! for: found exactly once → resolved at its new range; not found → missing;
//! found several times → ambiguous. Nothing is guessed from the old range.

use exposition_core::text::{TextRange, utf16_range_to_bytes};
use sha2::{Digest, Sha256};

use super::extract::Utf16Index;

const PREFIX: &str = "region:v1:";
const BASE: u64 = 1_099_511_628_211;

/// Result of locating a region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    Found(TextRange),
    Missing,
    Ambiguous(usize),
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().take(16).map(|b| format!("{b:02x}")).collect()
}

fn rolling(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |h, &b| h.wrapping_mul(BASE).wrapping_add(u64::from(b) + 1))
}

/// Fingerprint of the region `range` of `text` (UTF-16), or `None` when the
/// range is invalid or empty.
pub fn region_fingerprint(text: &str, range: TextRange) -> Option<String> {
    let bytes = utf16_range_to_bytes(text, range).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let slice = &text.as_bytes()[bytes];
    Some(format!("{PREFIX}{}:{:016x}:{}", slice.len(), rolling(slice), sha(slice)))
}

fn parse(fingerprint: &str) -> Option<(usize, u64, &str)> {
    let rest = fingerprint.strip_prefix(PREFIX)?;
    let mut parts = rest.splitn(3, ':');
    let len: usize = parts.next()?.parse().ok()?;
    let roll = u64::from_str_radix(parts.next()?, 16).ok()?;
    let digest = parts.next()?;
    if len == 0 || digest.len() != 32 {
        return None;
    }
    Some((len, roll, digest))
}

/// Locate a region by its fingerprint in `text` (linear time).
pub fn locate(text: &str, fingerprint: &str) -> Located {
    let Some((len, target, digest)) = parse(fingerprint) else { return Located::Missing };
    let bytes = text.as_bytes();
    if len > bytes.len() {
        return Located::Missing;
    }
    // BASE^(len-1) for removing the outgoing byte.
    let mut high = 1u64;
    for _ in 1..len {
        high = high.wrapping_mul(BASE);
    }
    let mut h = rolling(&bytes[..len]);
    let mut found: Vec<usize> = Vec::new();
    let mut start = 0usize;
    loop {
        if h == target
            && text.is_char_boundary(start)
            && text.is_char_boundary(start + len)
            && sha(&bytes[start..start + len]) == digest
        {
            found.push(start);
            if found.len() > 16 {
                break;
            }
        }
        if start + len >= bytes.len() {
            break;
        }
        let out = u64::from(bytes[start]) + 1;
        let inn = u64::from(bytes[start + len]) + 1;
        h = h.wrapping_sub(out.wrapping_mul(high)).wrapping_mul(BASE).wrapping_add(inn);
        start += 1;
    }
    match found.len() {
        0 => Located::Missing,
        1 => {
            let index = Utf16Index::new(text);
            Located::Found(index.range(found[0]..found[0] + len))
        }
        n => Located::Ambiguous(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_follow_their_text_and_never_guess() {
        let text = "intro é\nThe key claim holds.\nmore text\n";
        let start = exposition_core::text::byte_to_utf16(text, text.find("The key").unwrap()).unwrap() as u32;
        let range = TextRange::new(start, start + 20);
        let fp = region_fingerprint(text, range).unwrap();
        assert_eq!(locate(text, &fp), Located::Found(range));
        // Text inserted before: found at the shifted range.
        let moved = format!("A new paragraph 😀.\n{text}");
        let Located::Found(r) = locate(&moved, &fp) else { panic!() };
        let bytes = utf16_range_to_bytes(&moved, r).unwrap();
        assert_eq!(&moved[bytes], "The key claim holds.");
        // Edited inside: missing, not re-anchored to the old range.
        assert_eq!(locate(&text.replace("key", "main"), &fp), Located::Missing);
        // Duplicated: ambiguous.
        assert_eq!(locate(&format!("{text}{text}"), &fp), Located::Ambiguous(2));
        assert_eq!(locate(text, "garbage"), Located::Missing);
        assert!(region_fingerprint(text, TextRange::new(5, 2)).is_none());
        assert!(region_fingerprint(text, TextRange::new(0, 999)).is_none());
    }
}
