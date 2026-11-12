//! Lazy directory listing for the file tree.

use std::fs;

use exposition_core::path::WorkspacePath;

use crate::error::{WorkspaceError, WorkspaceResult};
use crate::fsutil::TEMP_MARKER;
use crate::root::WorkspaceRoot;
use crate::types::{EntryKind, FileEntry};

/// Directory names never shown in the tree or reported by the watcher.
pub const HIDDEN_NAMES: &[&str] = &[".git", ".lake", "node_modules", "target"];

/// True if a workspace path is inside a hidden directory, is a save temp
/// file, or is the project graph's own storage (`.exposition/graph.sqlite3*`,
/// its backups and set-aside copies): graph writes are not source changes.
pub fn is_ignored(path: &WorkspacePath) -> bool {
    path.components().any(|c| HIDDEN_NAMES.contains(&c))
        || path.file_name().contains(TEMP_MARKER)
        || is_graph_storage(path)
}

/// The project graph's own files under `.exposition/`.
pub fn is_graph_storage(path: &WorkspacePath) -> bool {
    let Some(rest) = path.as_str().strip_prefix(".exposition/") else { return false };
    rest.starts_with("graph.sqlite3") || rest.starts_with("graph-backups/") || rest.starts_with("graph.damaged")
}

/// List one directory (`None` = the root): directories first, then files, each
/// sorted case-insensitively. Hidden directories, temporary save files, and
/// names that are not valid workspace paths are omitted. Links are followed
/// only when their target stays inside the workspace.
pub fn list_dir(root: &WorkspaceRoot, dir: Option<&WorkspacePath>) -> WorkspaceResult<Vec<FileEntry>> {
    let absolute = match dir {
        None => root.path().to_path_buf(),
        Some(dir) => root.resolve_existing(dir)?,
    };
    let shown = dir.map(|d| d.to_string()).unwrap_or_else(|| root.name());
    let meta = fs::metadata(&absolute).map_err(|e| WorkspaceError::io(&shown, &e))?;
    if !meta.is_dir() {
        return Err(WorkspaceError::new("path.notDirectory", format!("{shown} is not a folder.")));
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&absolute).map_err(|e| WorkspaceError::io(&shown, &e))? {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if HIDDEN_NAMES.contains(&name.as_str()) || name.contains(TEMP_MARKER) {
            continue;
        }
        let path = match dir {
            None => WorkspacePath::parse(&name),
            Some(dir) => dir.join(&name),
        };
        let Ok(path) = path else { continue };
        let Ok(file_type) = entry.file_type() else { continue };
        let mut item = FileEntry { name, path, kind: EntryKind::Other, size: None, link: false, unavailable: None };
        if file_type.is_symlink() {
            item.link = true;
            match fs::canonicalize(entry.path()) {
                Ok(target) if root.contains(&target) => match fs::metadata(&target) {
                    Ok(m) if m.is_dir() => item.kind = EntryKind::Directory,
                    Ok(m) => {
                        item.kind = EntryKind::File;
                        item.size = Some(m.len());
                    }
                    Err(_) => item.unavailable = Some("The link target cannot be read.".into()),
                },
                Ok(_) => item.unavailable = Some("This link points outside the open folder.".into()),
                Err(_) => item.unavailable = Some("This link's target does not exist.".into()),
            }
        } else if file_type.is_dir() {
            item.kind = EntryKind::Directory;
        } else if file_type.is_file() {
            item.kind = EntryKind::File;
            item.size = entry.metadata().ok().map(|m| m.len());
        }
        entries.push(item);
    }
    entries.sort_by(|a, b| {
        let rank = |e: &FileEntry| if e.kind == EntryKind::Directory { 0 } else { 1 };
        rank(a).cmp(&rank(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then(a.name.cmp(&b.name))
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
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
    fn lists_directories_first_and_hides_build_and_vcs_folders() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        for d in ["b-dir", "A-dir", ".git", ".lake", "node_modules", "target", "A-dir/nested"] {
            fs::create_dir_all(project.join(d)).unwrap();
        }
        for f in ["z.typ", "a.tex", "B.md", "A-dir/inner.txt", ".a.tex.exposition-save-1.tmp"] {
            fs::write(project.join(f), "x").unwrap();
        }
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        junction(&project.join("escape"), &outside);

        let root = WorkspaceRoot::open(&project).unwrap();
        let top = list_dir(&root, None).unwrap();
        let names: Vec<_> = top.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A-dir", "b-dir", "a.tex", "B.md", "escape", "z.typ"]);
        let escape = top.iter().find(|e| e.name == "escape").unwrap();
        assert!(escape.link && escape.unavailable.is_some());
        assert_eq!(top[2].size, Some(1));

        let inner = list_dir(&root, Some(&WorkspacePath::parse("A-dir").unwrap())).unwrap();
        let names: Vec<_> = inner.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, ["A-dir/nested", "A-dir/inner.txt"]);

        let err = list_dir(&root, Some(&WorkspacePath::parse("escape").unwrap())).unwrap_err();
        assert_eq!(err.code, "path.outsideWorkspace");
        let err = list_dir(&root, Some(&WorkspacePath::parse("a.tex").unwrap())).unwrap_err();
        assert_eq!(err.code, "path.notDirectory");
    }

    #[test]
    fn ignores_hidden_and_temporary_paths() {
        assert!(is_ignored(&WorkspacePath::parse(".git/index").unwrap()));
        assert!(is_ignored(&WorkspacePath::parse(".lake/build/lib/A.olean").unwrap()));
        assert!(is_ignored(&WorkspacePath::parse("app/node_modules/x/y.js").unwrap()));
        assert!(is_ignored(&WorkspacePath::parse("dir/.main.typ.exposition-save-12.tmp").unwrap()));
        assert!(is_ignored(&WorkspacePath::parse(".exposition/graph.sqlite3").unwrap()));
        assert!(is_ignored(&WorkspacePath::parse(".exposition/graph.sqlite3-journal").unwrap()));
        assert!(is_ignored(&WorkspacePath::parse(".exposition/graph-backups/b1.sqlite3").unwrap()));
        assert!(!is_ignored(&WorkspacePath::parse(".exposition/project.json").unwrap()));
        assert!(!is_ignored(&WorkspacePath::parse(".exposition/research/r/fit.py").unwrap()));
        assert!(!is_ignored(&WorkspacePath::parse("chapters/target.typ").unwrap()));
    }
}
