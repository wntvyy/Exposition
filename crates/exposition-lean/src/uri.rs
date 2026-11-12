//! `file:` URIs for the language server.
//!
//! The Lean server echoes the URIs we send for open documents, but reports
//! other files (definition targets) in VS Code form, e.g.
//! `file:///c%3A/Users/x/Defs.lean` (lower-case drive, encoded colon). Both
//! forms decode to the same path; comparisons go through [`same_path`].

use std::path::{Path, PathBuf};

/// Encode an absolute path as a `file:` URI (`C:\a b\x.lean` →
/// `file:///C:/a%20b/x.lean`; UNC `\\host\share\x` → `file://host/share/x`).
pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    let (authority, rest) = {
        let unified = text.replace('\\', "/");
        if let Some(unc) = unified.strip_prefix("//") {
            let (host, rest) = unc.split_once('/').unwrap_or((unc, ""));
            (host.to_owned(), format!("/{rest}"))
        } else {
            (String::new(), format!("/{unified}"))
        }
    };
    #[cfg(not(windows))]
    let (authority, rest) = (String::new(), text.to_string());
    let mut uri = format!("file://{authority}");
    let bytes = rest.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/')
            // Keep the drive colon readable (`/C:/`), as most clients do.
            || (b == b':' && i == 2 && bytes.first() == Some(&b'/') && bytes[1].is_ascii_alphabetic());
        if keep {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// Decode a `file:` URI into a local path. Returns `None` for other schemes,
/// invalid percent-encoding, or non-UTF-8 paths.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let scheme_end = uri.find(':')?;
    if !uri[..scheme_end].eq_ignore_ascii_case("file") {
        return None;
    }
    let rest = uri[scheme_end + 1..].strip_prefix("//")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let decoded = percent_decode(path)?;
    let authority = percent_decode(authority)?;
    #[cfg(windows)]
    {
        if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
            return Some(PathBuf::from(format!(r"\\{authority}{}", decoded.replace('/', "\\"))));
        }
        let mut local = decoded.strip_prefix('/').unwrap_or(&decoded).to_owned();
        let bytes = local.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let drive = (bytes[0] as char).to_ascii_uppercase();
            local.replace_range(..1, &drive.to_string());
        } else {
            return None;
        }
        Some(PathBuf::from(local.replace('/', "\\")))
    }
    #[cfg(not(windows))]
    {
        if !authority.is_empty() && authority != "localhost" {
            return None;
        }
        Some(PathBuf::from(decoded))
    }
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Compare two absolute paths the way the host file system does (case and
/// separator insensitive on Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
        norm(a) == norm(b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_paths_round_trip_and_match_vscode_form() {
        let path = Path::new(r"C:\Users\x y\Lean Proj\Défs#1.lean");
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///C:/Users/x%20y/Lean%20Proj/D%C3%A9fs%231.lean");
        assert_eq!(uri_to_path(&uri).unwrap(), path);
        // The form the Lean server uses for files it reports itself.
        let vscode = uri_to_path("file:///c%3A/Users/x%20y/Lean%20Proj/D%C3%A9fs%231.lean").unwrap();
        assert!(same_path(&vscode, path));
        assert_eq!(vscode, path, "drive letter is normalized to upper case");
        let unc = Path::new(r"\\server\share\a.lean");
        assert_eq!(path_to_uri(unc), "file://server/share/a.lean");
        assert_eq!(uri_to_path("file://server/share/a.lean").unwrap(), unc);
    }

    #[test]
    fn rejects_other_schemes_and_bad_encoding() {
        assert!(uri_to_path("untitled:Untitled-1").is_none());
        assert!(uri_to_path("https://example.com/a.lean").is_none());
        assert!(uri_to_path("file:///C:/a%zz.lean").is_none());
        assert!(uri_to_path("file:///C:/a%ff.lean").is_none(), "not UTF-8");
    }
}
