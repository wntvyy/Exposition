//! Readable, collision-free copy names: `report.pdf` → `report (2).pdf`,
//! `report (2).pdf` → `report (3).pdf`, `data` (folder) → `data (2)`.
//!
//! These functions only propose candidates; a name is taken by creating it
//! with create-new semantics (see `fsops`), so two operations racing for the
//! same name never both get it.

use exposition_core::path::WorkspacePath;

/// Largest counter tried before giving up (`ops.nameUnavailable`).
pub const MAX_ATTEMPTS: u32 = 10_000;

/// Split a name into (stem, extension-with-dot). Folders keep their whole
/// name as the stem; a leading dot is not an extension separator.
pub fn split_name(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// `stem (k)` → (`stem`, k) when the name already carries a copy counter.
fn strip_counter(stem: &str) -> (&str, u32) {
    if let Some(open) = stem.rfind(" (")
        && let Some(inner) = stem[open + 2..].strip_suffix(')')
        && !inner.is_empty()
        && inner.len() <= 6
        && inner.bytes().all(|b| b.is_ascii_digit())
        && let Ok(k) = inner.parse::<u32>()
        && k >= 2
    {
        return (&stem[..open], k);
    }
    (stem, 1)
}

/// Candidate names for a copy of `name`, in order. With `keep_original` the
/// unchanged name comes first (pasting into another folder); otherwise the
/// first candidate is the next counter (duplicating next to the original).
pub fn candidates(name: &str, is_dir: bool, keep_original: bool) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = split_name(name, is_dir);
    let (base, counter) = strip_counter(stem);
    let original = keep_original.then(|| name.to_owned());
    let start = counter + 1;
    original.into_iter().chain((start..start.saturating_add(MAX_ATTEMPTS)).map(move |k| format!("{base} ({k}){ext}")))
}

/// The first candidate that is a valid workspace name under `parent` and is
/// not `taken` (used for planning; the real allocation happens on disk).
pub fn first_free(
    parent: Option<&WorkspacePath>,
    name: &str,
    is_dir: bool,
    keep_original: bool,
    mut taken: impl FnMut(&WorkspacePath) -> bool,
) -> Option<WorkspacePath> {
    for candidate in candidates(name, is_dir, keep_original) {
        let Ok(path) = join(parent, &candidate) else { continue };
        if !taken(&path) {
            return Some(path);
        }
    }
    None
}

/// `parent/name` (or `name` at the project root).
pub fn join(parent: Option<&WorkspacePath>, name: &str) -> Result<WorkspacePath, exposition_core::path::PathError> {
    match parent {
        Some(parent) => parent.join(name),
        None => WorkspacePath::parse(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(name: &str, is_dir: bool, keep: bool, n: usize) -> Vec<String> {
        candidates(name, is_dir, keep).take(n).collect()
    }

    #[test]
    fn copies_keep_extensions_and_count_up() {
        assert_eq!(first("report.pdf", false, false, 3), ["report (2).pdf", "report (3).pdf", "report (4).pdf"]);
        assert_eq!(first("report.pdf", false, true, 2), ["report.pdf", "report (2).pdf"]);
        assert_eq!(first("archive.tar.gz", false, false, 1), ["archive.tar (2).gz"]);
        assert_eq!(first(".gitignore", false, false, 1), [".gitignore (2)"]);
        assert_eq!(first("Makefile", false, false, 1), ["Makefile (2)"]);
        // Folders never split at a dot.
        assert_eq!(first("v1.2", true, false, 1), ["v1.2 (2)"]);
    }

    #[test]
    fn an_existing_counter_continues_instead_of_nesting() {
        assert_eq!(first("report (2).pdf", false, false, 2), ["report (3).pdf", "report (4).pdf"]);
        assert_eq!(first("data (7)", true, false, 1), ["data (8)"]);
        // Not a counter: kept as part of the name.
        assert_eq!(first("eq (1).typ", false, false, 1), ["eq (1) (2).typ"]);
        assert_eq!(first("notes (draft).md", false, false, 1), ["notes (draft) (2).md"]);
    }

    #[test]
    fn first_free_skips_taken_and_invalid_names() {
        let parent = WorkspacePath::parse("figs").unwrap();
        let taken = ["figs/plot (2).png", "figs/plot (3).png"];
        let free = first_free(Some(&parent), "plot.png", false, false, |p| taken.contains(&p.as_str())).unwrap();
        assert_eq!(free.as_str(), "figs/plot (4).png");
        let at_root = first_free(None, "plot.png", false, true, |_| false).unwrap();
        assert_eq!(at_root.as_str(), "plot.png");
    }
}
