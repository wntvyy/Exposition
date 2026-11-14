//! Which project paths file operations may touch.
//!
//! The project's own application data under `.exposition/` belongs to its
//! owning services (the graph database and its backups, run records,
//! generated artifacts, the research space structure). File operations never
//! delete, duplicate into, or paste into it (`ops.protected`); the research
//! space's group folders (`sources`, `data`, `notes`) accept files, and
//! `project.json` is an ordinary settings file. Generated artifacts may be
//! COPIED out (pasting an artifact into manuscript resources copies the asset).
//! Compared case-insensitively: `.EXPOSITION/Runs` is `.exposition/runs`.

use exposition_core::path::WorkspacePath;

/// The area a project path lies in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    /// The project graph's own storage (database, journal, backups, set-aside copies).
    GraphStorage,
    /// A generated research artifact (or its folder): service-owned, readable.
    Artifacts,
    /// Other application metadata or research-space structure.
    Metadata(&'static str),
    /// A research space group folder (`.exposition/research/<space>/<group>`).
    ResearchGroup,
    /// Ordinary project content (including the research groups' contents and `project.json`).
    User,
}

const GROUPS: &[&str] = &["sources", "data", "notes"];

pub fn classify(path: &WorkspacePath) -> Area {
    let folded = path.as_str().to_lowercase();
    let parts: Vec<&str> = folded.split('/').collect();
    if parts[0] != ".exposition" {
        return Area::User;
    }
    let Some(second) = parts.get(1) else { return Area::Metadata("the project's application data folder") };
    if second.starts_with("graph.sqlite3") || *second == "graph-backups" || second.starts_with("graph.damaged") {
        return Area::GraphStorage;
    }
    match *second {
        "project.json" if parts.len() == 2 => Area::User,
        "artifacts" => Area::Artifacts,
        "runs" => Area::Metadata("a research run record (kept by the research service)"),
        "research" => match parts.len() {
            2 => Area::Metadata("the research folder (managed by the research space)"),
            3 => Area::Metadata("a research space (managed by the research space)"),
            4 if GROUPS.contains(&parts[3]) => Area::ResearchGroup,
            4 => Area::Metadata("research space structure"),
            _ if GROUPS.contains(&parts[3]) => Area::User,
            _ => Area::Metadata("research space structure"),
        },
        _ => Area::Metadata("application metadata"),
    }
}

/// Why a path may not be deleted or duplicated (`None`: it may).
pub fn refuse_change(path: &WorkspacePath) -> Option<String> {
    match classify(path) {
        Area::User => None,
        Area::GraphStorage => {
            Some(format!("{path} is the project graph's own storage; it is managed by the project graph only."))
        }
        Area::Artifacts => Some(format!(
            "{path} is a generated research artifact kept by the research service; delete it from Storage & History or the run that produced it."
        )),
        Area::ResearchGroup => {
            Some(format!("{path} is a folder of the research space; delete the files inside it instead."))
        }
        Area::Metadata(what) => Some(format!("{path} is {what}; it is managed by Exposition, not as a project file.")),
    }
}

/// Why a path may not be read as a copy source (`None`: it may).
pub fn refuse_source(path: &WorkspacePath) -> Option<String> {
    match classify(path) {
        Area::User | Area::Artifacts | Area::ResearchGroup => None,
        Area::GraphStorage => Some(format!("{path} is the project graph's own storage and is not copied as a file.")),
        Area::Metadata(what) => Some(format!("{path} is {what} and is not copied as a file.")),
    }
}

/// Why a folder may not receive pasted copies (`None`: it may).
pub fn refuse_destination(folder: &WorkspacePath) -> Option<String> {
    match classify(folder) {
        Area::User | Area::ResearchGroup => None,
        Area::GraphStorage => Some(format!("{folder} is the project graph's own storage.")),
        Area::Artifacts => {
            Some(format!("{folder} holds generated research artifacts; only the research service writes there."))
        }
        Area::Metadata(what) => Some(format!("{folder} is {what}; files cannot be pasted there.")),
    }
}

/// Folders whose contents are disposable caches: deleting them permanently
/// needs no recovery-loss decision.
const CACHE_DIRS: &[&str] =
    &["__pycache__", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".ipynb_checkpoints", "node_modules", ".lake"];

/// True when everything at `path` is a disposable cache (a cache folder, a
/// file inside one, or a compiled Python file).
pub fn disposable_cache(path: &WorkspacePath) -> bool {
    path.components().any(|c| CACHE_DIRS.iter().any(|d| d.eq_ignore_ascii_case(c)))
        || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pyc"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wp(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    #[test]
    fn application_data_is_protected_in_any_spelling() {
        for p in [
            ".exposition",
            ".exposition/graph.sqlite3",
            ".EXPOSITION/Graph.sqlite3-journal",
            ".exposition/graph-backups/g.sqlite3",
            ".exposition/runs",
            ".Exposition/Runs/r1.json",
            ".exposition/research",
            ".exposition/research/rs-1",
            ".exposition/research/rs-1/data",
            ".exposition/research/rs-1/other",
            ".exposition/workflows.json",
            ".exposition/artifacts/r1/output-1.png",
        ] {
            assert!(refuse_change(&wp(p)).is_some(), "{p}");
        }
        for p in [
            "main.typ",
            ".exposition/project.json",
            ".exposition/research/rs-1/data/a.csv",
            ".Exposition/Research/RS-1/Sources/fit.py",
            "nested/.exposition/graph.sqlite3",
            ".git/config",
        ] {
            assert!(refuse_change(&wp(p)).is_none(), "{p}");
        }
    }

    #[test]
    fn artifacts_can_be_copied_out_but_not_into() {
        assert!(refuse_source(&wp(".exposition/artifacts/r1/output-1.png")).is_none());
        assert!(refuse_source(&wp(".exposition/graph.sqlite3")).is_some());
        assert!(refuse_source(&wp(".exposition/runs/r1.json")).is_some());
        assert!(refuse_source(&wp(".exposition")).is_some());
        assert!(refuse_destination(&wp(".exposition/artifacts/r1")).is_some());
        assert!(refuse_destination(&wp(".exposition")).is_some());
        assert!(refuse_destination(&wp(".exposition/research/rs-1")).is_some());
        assert!(refuse_destination(&wp(".exposition/research/rs-1/data")).is_none());
        assert!(refuse_destination(&wp(".exposition/research/rs-1/data/sub")).is_none());
        assert!(refuse_destination(&wp("figures")).is_none());
    }

    #[test]
    fn caches_are_disposable() {
        assert!(disposable_cache(&wp("src/__pycache__")));
        assert!(disposable_cache(&wp("src/__pycache__/m.cpython-314.pyc")));
        assert!(disposable_cache(&wp("lib/m.pyc")));
        assert!(!disposable_cache(&wp("paper/main.tex")));
        assert!(!disposable_cache(&wp("build")));
    }
}
