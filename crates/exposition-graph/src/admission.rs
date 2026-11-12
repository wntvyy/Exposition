//! The inclusion policy for project files (a backend rule).
//!
//! A file qualifies as a resource when it belongs to the project, has a
//! stable referent, and its type supports a concrete operation. Caches,
//! dependency installations, generated outputs, the graph's own storage, run
//! records, and developer-planning files are excluded from normal scanning.
//! Managed research sources under `.exposition/research/` are an explicit
//! exception: they are real project sources. Generated research artifacts
//! (`.exposition/artifacts/`) are admitted only explicitly, when retained or
//! attached (`Admission::OnRequest`).

use exposition_core::path::WorkspacePath;

use crate::vocabulary::ResourceRole;

/// How the inclusion policy treats a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Scanned and admitted as a resource with these roles.
    Admit(Vec<ResourceRole>),
    /// Not scanned, but may be admitted explicitly (retained/attached output).
    OnRequest(Vec<ResourceRole>),
    /// Never a resource; the reason is shown when someone asks.
    Exclude(&'static str),
}

/// Directory names never scanned anywhere in the project.
pub const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".lake",
    "lake-packages",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".ipynb_checkpoints",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    ".cache",
    "_minted",
    ".claude",
    ".agents",
    ".exposition-dev",
    ".worktrees",
    ".vscode",
    ".idea",
];

/// Developer-planning and agent-instruction files (never project resources).
const EXCLUDED_FILES: &[&str] = &["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md", "AGENTS.local.md"];

/// Generated TeX/build byproducts (not authored resources).
const GENERATED_EXTENSIONS: &[&str] = &[
    "aux",
    "log",
    "out",
    "toc",
    "lof",
    "lot",
    "fls",
    "fdb_latexmk",
    "synctex",
    "gz",
    "bbl",
    "blg",
    "bcf",
    "run.xml",
    "nav",
    "snm",
    "vrb",
    "xdv",
    "dvi",
    "olean",
    "ilean",
    "c",
    "o",
    "obj",
    "exe",
    "dll",
    "pyc",
    "tmp",
    "swp",
];

/// Roles a file has from its name, if it is a recognised project resource.
pub fn roles_for(path: &WorkspacePath) -> Option<Vec<ResourceRole>> {
    let name = path.file_name();
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "lakefile.lean" | "lakefile.toml" | "lean-toolchain" | "lake-manifest.json") {
        return Some(vec![ResourceRole::Configuration]);
    }
    if path.as_str() == ".exposition/project.json" {
        return Some(vec![ResourceRole::Configuration]);
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e.to_owned()).unwrap_or_default();
    let roles = match ext.as_str() {
        "tex" | "typ" => vec![ResourceRole::Manuscript],
        "md" | "markdown" => vec![ResourceRole::Manuscript],
        "txt" | "rst" | "org" => vec![ResourceRole::Note],
        "bib" => vec![ResourceRole::Bibliography],
        "py" | "ipy" | "pyw" | "r" | "jl" => vec![ResourceRole::Script],
        "ipynb" => vec![ResourceRole::Script],
        "lean" => vec![ResourceRole::FormalSource],
        "csv" | "tsv" | "json" | "jsonl" | "parquet" | "feather" | "xlsx" | "xls" | "h5" | "hdf5" | "nc" | "npz"
        | "npy" | "mat" | "rds" | "rdata" | "arrow" => vec![ResourceRole::Dataset],
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "pdf" | "eps" | "webp" => vec![ResourceRole::Asset],
        "sty" | "cls" | "bst" | "toml" | "yaml" | "yml" | "cfg" | "ini" => vec![ResourceRole::Configuration],
        _ => return None,
    };
    Some(roles)
}

/// Classify a workspace path under the inclusion policy.
pub fn classify(path: &WorkspacePath) -> Admission {
    let p = path.as_str();
    let components: Vec<&str> = path.components().collect();
    if components.iter().any(|c| EXCLUDED_DIRS.contains(c)) {
        return Admission::Exclude("inside a cache, dependency, version-control, or tooling folder");
    }
    if EXCLUDED_FILES.contains(&path.file_name()) {
        return Admission::Exclude("a development-planning or agent-instruction file");
    }
    if path.file_name().contains(".exposition-save-") {
        return Admission::Exclude("a temporary save file");
    }
    if let Some(rest) = p.strip_prefix(".exposition/") {
        if rest.starts_with("graph.sqlite3") || rest.starts_with("graph-backups/") || rest.starts_with("graph.damaged")
        {
            return Admission::Exclude("the project graph's own storage");
        }
        if rest.starts_with("runs/") {
            return Admission::Exclude("a run record (inspect it through the run's provenance)");
        }
        if rest.starts_with("artifacts/") {
            // Generated outputs: admitted when retained or attached.
            return match roles_for(path) {
                Some(mut roles) => {
                    roles.retain(|r| *r != ResourceRole::Asset);
                    roles.push(ResourceRole::Artifact);
                    Admission::OnRequest(roles)
                }
                None => Admission::OnRequest(vec![ResourceRole::Artifact]),
            };
        }
        if rest.starts_with("research/") {
            return match roles_for(path) {
                Some(roles) => Admission::Admit(roles),
                None => Admission::Exclude("not a recognised research source type"),
            };
        }
        if p == ".exposition/project.json" {
            return Admission::Admit(vec![ResourceRole::Configuration]);
        }
        return Admission::Exclude("application metadata");
    }
    let lower = path.file_name().to_ascii_lowercase();
    if let Some((_, ext)) = lower.rsplit_once('.')
        && GENERATED_EXTENSIONS.contains(&ext)
    {
        return Admission::Exclude("a generated build or cache file");
    }
    match roles_for(path) {
        Some(roles) => Admission::Admit(roles),
        None => Admission::Exclude("not a recognised project resource type"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(p: &str) -> Admission {
        classify(&WorkspacePath::parse(p).unwrap())
    }

    #[test]
    fn authored_sources_are_admitted_with_roles() {
        assert_eq!(c("paper/main.tex"), Admission::Admit(vec![ResourceRole::Manuscript]));
        assert_eq!(c("refs.bib"), Admission::Admit(vec![ResourceRole::Bibliography]));
        assert_eq!(c("analysis/fit.py"), Admission::Admit(vec![ResourceRole::Script]));
        assert_eq!(c("Proofs/Main.lean"), Admission::Admit(vec![ResourceRole::FormalSource]));
        assert_eq!(c("data/obs.csv"), Admission::Admit(vec![ResourceRole::Dataset]));
        assert_eq!(c("figures/plot.png"), Admission::Admit(vec![ResourceRole::Asset]));
        assert_eq!(c(".exposition/research/rs-1/sources/sim.py"), Admission::Admit(vec![ResourceRole::Script]));
        assert_eq!(c(".exposition/project.json"), Admission::Admit(vec![ResourceRole::Configuration]));
    }

    #[test]
    fn caches_logs_graph_storage_runs_and_planning_files_are_excluded() {
        for p in [
            ".git/config",
            "node_modules/x/index.js",
            ".lake/build/lib/A.olean",
            ".venv/lib/site.py",
            "src/__pycache__/m.pyc",
            "main.aux",
            "main.log",
            "main.synctex.gz",
            ".exposition/graph.sqlite3",
            ".exposition/graph.sqlite3-journal",
            ".exposition/graph-backups/g.sqlite3",
            ".exposition/runs/run-1.json",
            "CLAUDE.md",
            ".exposition-dev/MASTER.md",
            ".claude/settings.json",
            "unknown.xyz",
        ] {
            assert!(matches!(c(p), Admission::Exclude(_)), "{p}");
        }
    }

    #[test]
    fn generated_artifacts_are_admitted_only_on_request() {
        assert_eq!(c(".exposition/artifacts/run-1/output-0.png"), Admission::OnRequest(vec![ResourceRole::Artifact]));
        assert_eq!(
            c(".exposition/artifacts/run-1/output-1.csv"),
            Admission::OnRequest(vec![ResourceRole::Dataset, ResourceRole::Artifact])
        );
    }
}
