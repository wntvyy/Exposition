//! Path rules for blueprint files and folders.
//!
//! A blueprint path must already be in normalized workspace form: relative,
//! forward slashes, no `.`/`..`/empty components, no drive letters, UNC
//! prefixes, or `:` (alternate data streams), no Windows reserved device
//! names or trailing dots/spaces, and bounded length and depth. It must stay
//! out of version-control, tool, and application-metadata folders (the one
//! exception is the blueprint's own research space under
//! `.exposition/research/<id>/`), and it may not create files that tools run
//! automatically (startup hooks) or Windows executables/shortcuts.

use exposition_core::path::WorkspacePath;

pub const MAX_PATH_BYTES: usize = 240;
pub const MAX_COMPONENT_BYTES: usize = 120;
pub const MAX_DEPTH: usize = 12;

/// Top-level folders a blueprint may never write into.
const RESERVED_ROOTS: &[&str] =
    &[".git", ".hg", ".svn", ".exposition-dev", ".claude", ".agents", ".worktrees", ".vscode", ".idea"];

/// Files that tools load or run automatically (startup hooks, agent
/// instructions, build-tool configuration that executes code).
const HOOK_FILES: &[&str] = &[
    "claude.md",
    "claude.local.md",
    "agents.md",
    "agents.local.md",
    "latexmkrc",
    ".latexmkrc",
    "texmf.cnf",
    "sitecustomize.py",
    "usercustomize.py",
    ".rprofile",
    "rprofile.site",
    ".renviron",
    "lakefile.lean",
    "kernel.json",
    ".envrc",
    ".pythonstartup",
    "desktop.ini",
    "autorun.inf",
];

/// Executable, script-host, and shortcut types Windows runs on open.
const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "exe",
    "dll",
    "com",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "psd1",
    "vbs",
    "vbe",
    "jse",
    "wsf",
    "wsh",
    "msi",
    "msp",
    "scr",
    "lnk",
    "url",
    "hta",
    "cpl",
    "reg",
    "scf",
    "pif",
    "application",
    "appref-ms",
    "sys",
    "pth",
];

/// Validate one blueprint path. `research` is the blueprint's research
/// space id, if it declares one.
pub fn check(raw: &str, research: Option<&str>) -> Result<WorkspacePath, String> {
    if raw.is_empty() {
        return Err("the path is empty".into());
    }
    if raw.len() > MAX_PATH_BYTES {
        return Err(format!("the path is longer than {MAX_PATH_BYTES} bytes"));
    }
    if raw.contains('\\') {
        return Err("use forward slashes; backslashes are not allowed".into());
    }
    if raw.starts_with('/') || raw.starts_with("//") {
        return Err("absolute paths are not allowed; paths are relative to the project folder".into());
    }
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err("drive letters are not allowed; paths are relative to the project folder".into());
    }
    if raw.contains(':') {
        return Err("':' is not allowed (alternate data streams and drive prefixes)".into());
    }
    let components: Vec<&str> = raw.split('/').collect();
    if components.len() > MAX_DEPTH {
        return Err(format!("the path is nested more than {MAX_DEPTH} folders deep"));
    }
    for c in &components {
        match *c {
            ".." => return Err("'..' would leave the project folder".into()),
            "" | "." => return Err("empty and '.' path components are not allowed".into()),
            c if c.len() > MAX_COMPONENT_BYTES => {
                return Err(format!("a path component is longer than {MAX_COMPONENT_BYTES} bytes"));
            }
            _ => {}
        }
    }
    let parsed = WorkspacePath::parse(raw).map_err(|e| match e {
        exposition_core::path::PathError::InvalidComponent(part) => format!(
            "{part:?} is not a valid file name (reserved device names, trailing dots or spaces, and <>:\"|?* are not allowed)"
        ),
        other => other.to_string(),
    })?;
    if parsed.as_str() != raw {
        return Err("the path is not in normalized form".into());
    }
    let lower: Vec<String> = components.iter().map(|c| c.to_ascii_lowercase()).collect();
    if RESERVED_ROOTS.contains(&lower[0].as_str()) {
        return Err(format!("{} is a version-control or tool folder", components[0]));
    }
    if lower[0] == ".exposition" {
        let allowed = match research {
            Some(id) => components.len() >= 4 && lower[1] == "research" && components[2] == id,
            None => false,
        };
        if !allowed {
            return Err(
                ".exposition holds application metadata; only files in the blueprint's own research space (.exposition/research/<id>/) may be created there"
                    .into(),
            );
        }
    }
    if let Some(dir) = components[..components.len() - 1]
        .iter()
        .find(|c| crate::admission::EXCLUDED_DIRS.iter().any(|x| x.eq_ignore_ascii_case(c)))
    {
        return Err(format!("{dir} is a dependency, cache, or tool folder"));
    }
    let name = lower.last().expect("at least one component");
    if HOOK_FILES.contains(&name.as_str()) {
        return Err(format!(
            "{} is loaded or run automatically by tools; blueprints cannot create startup hooks",
            components.last().expect("name")
        ));
    }
    if let Some((_, ext)) = name.rsplit_once('.')
        && EXECUTABLE_EXTENSIONS.contains(&ext)
    {
        return Err(format!(".{ext} files can run when opened; blueprints cannot create them"));
    }
    Ok(parsed)
}

/// Case-insensitive identity of a path (Windows file names ignore case).
pub fn fold(path: &WorkspacePath) -> String {
    path.as_str().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_project_paths() {
        for ok in ["main.typ", "paper/main.tex", "data/obs.csv", "Investigation.lean", "lakefile.toml", ".gitignore"] {
            assert_eq!(check(ok, None).unwrap().as_str(), ok);
        }
        assert!(check(".exposition/research/rs-ab12/sources/a.py", Some("rs-ab12")).is_ok());
    }

    #[test]
    fn refuses_hostile_paths() {
        let cases: &[(&str, &str)] = &[
            ("../outside.txt", "'..'"),
            ("a/../../x", "'..'"),
            ("/etc/passwd", "absolute"),
            ("//server/share/x", "absolute"),
            ("C:/Windows/x.txt", "drive"),
            ("c:relative.txt", "drive"),
            ("a\\b.txt", "backslash"),
            ("notes.txt:hidden", "':'"),
            ("main.typ::$DATA", "':'"),
            ("CON", "not a valid file name"),
            ("docs/nul.txt", "not a valid file name"),
            ("COM1.tex", "not a valid file name"),
            ("LPT\u{b9}", "not a valid file name"),
            ("trailing.", "not a valid file name"),
            ("space /a.txt", "not a valid file name"),
            ("a/./b.txt", "'.'"),
            ("a//b.txt", "empty"),
            (".git/hooks/post-checkout", "version-control"),
            (".GIT/config", "version-control"),
            (".vscode/tasks.json", "tool folder"),
            (".exposition/graph.sqlite3", "application metadata"),
            (".exposition/project.json", "application metadata"),
            (".exposition/research/rs-other/a.py", "application metadata"),
            ("node_modules/x/index.js", "dependency"),
            ("CLAUDE.md", "startup hooks"),
            ("sub/AGENTS.md", "startup hooks"),
            (".latexmkrc", "startup hooks"),
            ("lib/sitecustomize.py", "startup hooks"),
            ("lakefile.lean", "startup hooks"),
            ("evil.pth", "can run"),
            ("run.BAT", "can run"),
            ("shortcut.lnk", "can run"),
            ("bad\u{0}name", "not a valid file name"),
            ("", "empty"),
        ];
        for (raw, expect) in cases {
            let err = check(raw, Some("rs-ab12")).expect_err(raw);
            assert!(err.contains(expect), "{raw:?}: {err}");
        }
        assert!(check(&format!("{}.txt", "a".repeat(250)), None).is_err());
        assert!(check(&vec!["d"; 13].join("/"), None).unwrap_err().contains("nested"));
    }
}
