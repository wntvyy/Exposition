//! Where LaTeX resolves `\includegraphics` paths for a document.
//!
//! LaTeX resolves graphics (and `\input`) paths against the directory of the
//! build's main file, not the included file. A figure attached to
//! `chapters/ch1.tex` that `book.tex` includes must therefore be written
//! relative to `book.tex`'s directory.

use std::path::Path;

use exposition_core::build::DocumentFormat;
use exposition_core::path::WorkspacePath;
use exposition_workspace::settings::ProjectSettings;

/// Files scanned when looking for the main file that includes a document.
const SCAN_LIMIT: usize = 500;
const SCAN_DEPTH: usize = 6;
const FILE_LIMIT: u64 = 2 * 1024 * 1024;

/// The main LaTeX file that builds `document` (itself when it has
/// `\documentclass`), when one can be determined.
pub(crate) fn main_file(root: &Path, document: &WorkspacePath, text: Option<&str>) -> Option<WorkspacePath> {
    let own_text = text.map(str::to_owned).or_else(|| read(root, document));
    if own_text.as_deref().is_some_and(is_main_file) {
        return Some(document.clone());
    }
    let entries = project_latex_entries(root);
    if let Some(entry) = entries.iter().find(|e| *e != document && includes(root, e, document)) {
        return Some(entry.clone());
    }
    scan_for_includer(root, document)
}

/// Packages the main file loads (for preamble advice on included files).
pub(crate) fn loads_package(root: &Path, main: &WorkspacePath, package: &str) -> bool {
    read(root, main).is_some_and(|text| {
        code_lines(&text).any(|line| {
            (line.contains("\\usepackage") || line.contains("\\RequirePackage"))
                && line.split(['{', '}', ',']).any(|part| part.trim() == package)
        })
    })
}

/// The base directory (`None` = workspace root) graphics paths are relative to,
/// plus a warning when it could not be determined with confidence.
pub(crate) fn graphics_base(
    root: &Path,
    document: &WorkspacePath,
    text: Option<&str>,
) -> (Option<WorkspacePath>, Option<String>) {
    let own_dir = document.parent();
    let own_text = text.map(str::to_owned).or_else(|| read(root, document));
    // A document with \documentclass is its own build entry.
    if own_text.as_deref().is_some_and(is_main_file) {
        return (own_dir, None);
    }
    // Project targets: the entry that includes this file, else the default LaTeX target.
    let entries = project_latex_entries(root);
    if let Some(entry) = entries.iter().find(|e| *e != document && includes(root, e, document)) {
        return (entry.parent(), None);
    }
    if let Some(entry) = entries.iter().find(|e| *e != document) {
        return (
            entry.parent(),
            Some(format!(
                "{document} is not a main LaTeX file; the figure path is relative to the project's LaTeX entry {entry}."
            )),
        );
    }
    // No configured target: look for a main file in the project that includes it.
    if let Some(entry) = scan_for_includer(root, document) {
        return (entry.parent(), None);
    }
    (
        own_dir,
        Some(format!(
            "No main LaTeX file including {document} was found; the figure path is relative to {document}'s folder. If another file \\input{{}}s it, adjust the path."
        )),
    )
}

fn read(root: &Path, path: &WorkspacePath) -> Option<String> {
    let file = path.components().fold(root.to_path_buf(), |p, c| p.join(c));
    let canonical = std::fs::canonicalize(&file).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    if !canonical.starts_with(&root) || std::fs::metadata(&canonical).ok()?.len() > FILE_LIMIT {
        return None;
    }
    std::fs::read_to_string(canonical).ok()
}

/// LaTeX source without comments (an unescaped `%` starts a comment).
fn code_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(|line| {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'%' => return &line[..i],
                _ => i += 1,
            }
        }
        line
    })
}

fn is_main_file(text: &str) -> bool {
    code_lines(text).any(|line| line.contains("\\documentclass"))
}

fn project_latex_entries(root: &Path) -> Vec<WorkspacePath> {
    // Read-only parse: never rewrite or move the project's settings file.
    let file = root.join(".exposition").join("project.json");
    let Ok(bytes) = std::fs::read(file) else { return Vec::new() };
    let Ok(settings) = serde_json::from_slice::<ProjectSettings>(&bytes) else { return Vec::new() };
    let mut entries = Vec::new();
    let default = settings.default_target.clone();
    let mut targets: Vec<_> = settings.targets.iter().filter(|t| t.format == DocumentFormat::Latex).collect();
    targets.sort_by_key(|t| Some(&t.name) != default.as_ref());
    for target in targets {
        if !entries.contains(&target.entry) {
            entries.push(target.entry.clone());
        }
    }
    if let Some(entry) = settings.entry
        && entry.extension().is_some_and(|e| e.eq_ignore_ascii_case("tex"))
        && !entries.contains(&entry)
    {
        entries.push(entry);
    }
    entries
}

/// Whether `entry` includes `document` with \input, \include, or \subfile
/// (paths resolve against the entry's directory, as LaTeX does).
fn includes(root: &Path, entry: &WorkspacePath, document: &WorkspacePath) -> bool {
    let Some(text) = read(root, entry) else { return false };
    let base = entry.parent();
    for line in code_lines(&text) {
        for command in ["\\input{", "\\include{", "\\subfile{", "\\includeonly{"] {
            let mut rest = line;
            while let Some(at) = rest.find(command) {
                rest = &rest[at + command.len()..];
                let Some(end) = rest.find('}') else { break };
                let name = rest[..end].trim();
                rest = &rest[end..];
                for candidate in [name.to_owned(), format!("{name}.tex")] {
                    let joined = match &base {
                        Some(dir) => dir.join(&candidate),
                        None => WorkspacePath::parse(&candidate),
                    };
                    if joined.as_ref().is_ok_and(|p| p == document) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn scan_for_includer(root: &Path, document: &WorkspacePath) -> Option<WorkspacePath> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    let mut scanned = 0;
    while let Some((dir, prefix, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else { continue };
            let relative = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if kind.is_dir() {
                if depth < SCAN_DEPTH && !name.starts_with('.') && !matches!(name.as_str(), "node_modules" | "target") {
                    stack.push((entry.path(), relative, depth + 1));
                }
            } else if kind.is_file() && name.to_ascii_lowercase().ends_with(".tex") {
                scanned += 1;
                if scanned > SCAN_LIMIT {
                    return None;
                }
                let Ok(path) = WorkspacePath::parse(&relative) else { continue };
                if &path != document
                    && read(root, &path).is_some_and(|t| is_main_file(&t))
                    && includes(root, &path, document)
                {
                    found.push(path);
                }
            }
        }
    }
    found.sort();
    found.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    #[test]
    fn included_chapters_resolve_against_the_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "book.tex", "\\documentclass{book}\n\\begin{document}\n\\input{chapters/ch1}\n\\end{document}\n");
        write(root, "chapters/ch1.tex", "\\section{One}\n");
        write(root, "chapters/ch2.tex", "% \\documentclass{x} commented\n\\section{Two}\n");
        let ch1 = WorkspacePath::parse("chapters/ch1.tex").unwrap();
        assert_eq!(graphics_base(root, &ch1, None), (None, None), "book.tex at the root includes ch1");
        let main = WorkspacePath::parse("chapters/standalone.tex").unwrap();
        let (base, warning) = graphics_base(root, &main, Some("\\documentclass{article}\n"));
        assert_eq!(base.unwrap().as_str(), "chapters");
        assert!(warning.is_none());
        // Not included anywhere: own folder, with a warning.
        let ch2 = WorkspacePath::parse("chapters/ch2.tex").unwrap();
        let (base, warning) = graphics_base(root, &ch2, None);
        assert_eq!(base.unwrap().as_str(), "chapters");
        assert!(warning.is_some());
    }

    #[test]
    fn project_targets_take_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "thesis/main.tex",
            "\\documentclass{report}\n\\begin{document}\n\\include{parts/a}\n\\end{document}\n",
        );
        write(root, "thesis/parts/a.tex", "Text.\n");
        write(
            root,
            ".exposition/project.json",
            r#"{"version":1,"targets":[{"name":"thesis","format":"latex","engine":"pdfLatex","entry":"thesis/main.tex"}],"defaultTarget":"thesis"}"#,
        );
        let a = WorkspacePath::parse("thesis/parts/a.tex").unwrap();
        assert_eq!(graphics_base(root, &a, None).0.unwrap().as_str(), "thesis");
    }
}
