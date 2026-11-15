//! Files the application, or tools it starts on its own, execute or load
//! without a launch the agent requested — classified in one place for the
//! central gate.
//!
//! - **Auto-executed content** ([`auto_executed`]): Lean sources (the live
//!   Lean server elaborates the text of every open `.lean` document in a
//!   trusted folder, `#eval` included) and Lean/Lake configuration
//!   (`lakefile.lean`, `lakefile.toml`, `lean-toolchain`, `lake-manifest.json`
//!   in any folder: when the Lean server starts for a Lean file there, Lake
//!   reads the nearest configuration, may clone the packages it requires and
//!   elaborate their `lakefile.lean`, and elan may download the toolchain it
//!   names), and R startup files (`.Rprofile`, `.Renviron` in any folder: R
//!   reads them from its working directory at the start of every session —
//!   the user's R research session and native R workflow steps start in the
//!   project, and so does R started any other way there — and runs the
//!   profile's code natively). Putting such text into the project is native
//!   execution: the gate requires native-code authority as well as change
//!   authority (D7), whether or not the folder is trusted yet — trust only
//!   decides whether Exposition runs it now or once the folder is trusted.
//!   Files such a startup file sources (for example `renv/activate.R`) are
//!   ordinary files: their changes are reviewable but not gated here.
//! - **Protected folders** ([`protected`]): `.git/` (hooks and configuration
//!   run in the user's tools), `.lake/` (packages and build products Lake
//!   loads), and the project environments `.venv/` and `venv/` (environment
//!   discovery runs the project interpreter automatically, and Python runs
//!   `site-packages/*.pth` at startup). Agents never write, create, insert,
//!   copy into, or delete anything there, in any folder of the project; reads
//!   stay possible.
//!
//! Names are compared case-insensitively (Windows and macOS file systems
//! ignore case); the gate applies both rules to the requested and the
//! resolved path (links, junctions, case and 8.3 aliases).

use std::collections::VecDeque;
use std::path::Path;

use exposition_core::path::WorkspacePath;

/// How a file's content gets executed without an agent-requested launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AutoExec {
    /// Lean source the Lean server elaborates.
    LeanSource,
    /// Lean/Lake configuration Lake and elan act on (downloads and runs code).
    LakeConfig,
    /// R startup files R reads at the start of every session in the folder.
    RStartup,
}

/// Lean/Lake configuration file names (compared case-insensitively).
pub const LAKE_CONFIG_FILES: [&str; 4] = ["lakefile.lean", "lakefile.toml", "lean-toolchain", "lake-manifest.json"];

/// R startup file names (compared case-insensitively).
pub const R_STARTUP_FILES: [&str; 2] = [".rprofile", ".renviron"];

/// Whether `path` (a file) is auto-executed content, and how.
pub fn auto_executed(path: &WorkspacePath) -> Option<AutoExec> {
    auto_executed_name(path.file_name())
}

fn auto_executed_name(name: &str) -> Option<AutoExec> {
    let lower = name.to_ascii_lowercase();
    if LAKE_CONFIG_FILES.contains(&lower.as_str()) {
        Some(AutoExec::LakeConfig)
    } else if R_STARTUP_FILES.contains(&lower.as_str()) {
        Some(AutoExec::RStartup)
    } else if lower.ends_with(".lean") {
        Some(AutoExec::LeanSource)
    } else {
        None
    }
}

impl AutoExec {
    /// What happens to such text, for refusals and approval cards; in a
    /// folder that is not trusted yet, nothing runs now but it runs once the
    /// folder is trusted.
    pub fn consequence(self, trusted: bool) -> &'static str {
        match (self, trusted) {
            (AutoExec::LeanSource, true) => {
                "Lean elaborates the text of an open Lean file natively as soon as it changes: code in it (for \
                 example #eval) runs on this computer with your account's privileges."
            }
            (AutoExec::LeanSource, false) => {
                "Lean elaborates the text of Lean files natively. This folder is not trusted yet, so nothing runs \
                 now; once it is trusted, code in this text (for example #eval) runs on this computer with your \
                 account's privileges."
            }
            (AutoExec::LakeConfig, true) => {
                "Lake reads Lean and Lake configuration natively when the Lean server starts for a Lean file in that \
                 folder: it may download the packages it names and run their build code, and elan may download the \
                 toolchain it names, with your account's privileges."
            }
            (AutoExec::LakeConfig, false) => {
                "Lake reads Lean and Lake configuration natively. This folder is not trusted yet, so nothing runs \
                 now; once it is trusted, Lake may download the packages it names and run their build code, and \
                 elan may download the toolchain it names, with your account's privileges."
            }
            (AutoExec::RStartup, true) => {
                "R runs this file at the start of every R session in this folder (also Rscript), natively with your \
                 account's privileges."
            }
            (AutoExec::RStartup, false) => {
                "R runs this file at the start of every R session in this folder (also Rscript), natively with your \
                 account's privileges. This folder is not trusted yet, so Exposition starts no R session here now; \
                 R started here any other way still runs it."
            }
        }
    }

    /// The short reason an approval states.
    pub fn reason(self, trusted: bool) -> &'static str {
        match (self, trusted) {
            (AutoExec::LeanSource, true) => "Lean elaborates this file's text natively as soon as it changes.",
            (AutoExec::LeanSource, false) => {
                "Lean will elaborate this file's text natively once the folder is trusted."
            }
            (AutoExec::LakeConfig, true) => {
                "Lake acts on this configuration natively (it may download and run packages and toolchains)."
            }
            (AutoExec::LakeConfig, false) => {
                "Lake will act on this configuration natively once the folder is trusted (it may download and run \
                 packages and toolchains)."
            }
            (AutoExec::RStartup, true) => "R runs this file natively at the start of every R session in this folder.",
            (AutoExec::RStartup, false) => {
                "R runs this file natively at the start of R sessions in this folder (Exposition's once the folder \
                 is trusted)."
            }
        }
    }
}

/// A folder agents never change, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protected {
    /// The folder name as protected (`.git`, `.lake`, `.venv`, `venv`).
    pub folder: &'static str,
    /// What it is and what runs it.
    pub what: &'static str,
    /// What the user can do instead.
    pub instead: &'static str,
}

const PROTECTED: [Protected; 4] = [
    Protected {
        folder: ".git",
        what: "Git's repository data (its hooks and configuration run in your tools)",
        instead: "The user can make this change themselves.",
    },
    Protected {
        folder: ".lake",
        what: "Lake's build folder (packages and build products Lake loads and runs)",
        instead: "The user can make this change themselves; Lake manages this folder.",
    },
    Protected {
        folder: ".venv",
        what: "a project Python environment (Exposition runs its interpreter automatically, and Python runs code \
               from its site-packages at startup)",
        instead: "Use install_packages to add packages, or ask the user to change it.",
    },
    Protected {
        folder: "venv",
        what: "a project Python environment (Exposition runs its interpreter automatically, and Python runs code \
               from its site-packages at startup)",
        instead: "Use install_packages to add packages, or ask the user to change it.",
    },
];

impl Protected {
    /// The refusal of an agent change at `path`.
    pub fn refused(&self, path: &WorkspacePath) -> String {
        format!(
            "{path} is inside {}: {}. Agents never write, create, copy into, or delete anything there; nothing was \
             changed. {}",
            self.folder, self.what, self.instead
        )
    }
}

/// The protected folder `path` is (or lies inside), at any depth.
pub fn protected(path: &WorkspacePath) -> Option<Protected> {
    path.components().find_map(protected_name)
}

fn protected_name(component: &str) -> Option<Protected> {
    PROTECTED.iter().copied().find(|p| component.eq_ignore_ascii_case(p.folder))
}

/// What copying a source tree would create, inspected before the copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeScan {
    /// The strongest auto-executed content in the tree.
    pub auto: Option<AutoExec>,
    /// A protected folder the copy would create (its path inside the source).
    pub protected: Option<(String, Protected)>,
    /// The tree was larger than the inspection bound (treated as auto-executed).
    pub truncated: bool,
}

/// Entries inspected per copy call before the scan gives up (conservatively).
pub const MAX_SCAN_ENTRIES: usize = 20_000;

/// Inspect the entry `source` (a project path) under `root` as a copy would
/// take it: the entry itself and, for a folder, everything inside it (links
/// are not followed, as the copy does not copy them). Bounded by `budget`.
pub fn scan_tree(root: &Path, source: &WorkspacePath, budget: &mut usize) -> TreeScan {
    let mut scan = TreeScan::default();
    let mut absolute = root.to_path_buf();
    for part in source.components() {
        absolute.push(part);
    }
    let name = source.file_name().to_owned();
    let mut queue: VecDeque<(std::path::PathBuf, String)> = VecDeque::from([(absolute, name)]);
    while let Some((path, relative)) = queue.pop_front() {
        if *budget == 0 {
            scan.truncated = true;
            break;
        }
        *budget -= 1;
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        let leaf = relative.rsplit('/').next().unwrap_or(&relative);
        if scan.protected.is_none()
            && let Some(p) = protected_name(leaf)
        {
            scan.protected = Some((relative.clone(), p));
        }
        if meta.is_dir() {
            let Ok(entries) = std::fs::read_dir(&path) else { continue };
            for entry in entries.flatten() {
                let child = entry.file_name().to_string_lossy().into_owned();
                queue.push_back((entry.path(), format!("{relative}/{child}")));
            }
        } else if let Some(kind) = auto_executed_name(leaf) {
            scan.auto = scan.auto.max(Some(kind));
        }
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> WorkspacePath {
        WorkspacePath::parse(s).unwrap()
    }

    #[test]
    fn classifies_lean_sources_and_lake_configuration_in_any_folder_and_case() {
        assert_eq!(auto_executed(&p("Main.lean")), Some(AutoExec::LeanSource));
        assert_eq!(auto_executed(&p("a/B.LEAN")), Some(AutoExec::LeanSource));
        for name in ["lakefile.lean", "LakeFile.toml", "a/b/lean-toolchain", "x/LAKE-MANIFEST.JSON"] {
            assert_eq!(auto_executed(&p(name)), Some(AutoExec::LakeConfig), "{name}");
        }
        for name in [".Rprofile", "sub/.RENVIRON", "a/b/.rprofile"] {
            assert_eq!(auto_executed(&p(name)), Some(AutoExec::RStartup), "{name}");
        }
        for name in [
            "main.typ",
            "lean-toolchain.txt",
            "lakefile.toml.bak",
            "notes/lean",
            "Rprofile",
            ".Rprofile.bak",
            "x.Rprofile",
        ] {
            assert_eq!(auto_executed(&p(name)), None, "{name}");
        }
    }

    #[test]
    fn protects_git_lake_and_project_environments_at_any_depth() {
        for path in [
            ".git/hooks/pre-commit",
            ".GIT/config",
            "sub/.lake/build/x.olean",
            ".venv/Lib/site-packages/x.pth",
            "venv",
            "a/VENV/b",
        ] {
            assert!(protected(&p(path)).is_some(), "{path}");
        }
        for path in ["git/x", "notes/.gitignore", "my.venv/x", "venvs/x", "lake/x"] {
            assert!(protected(&p(path)).is_none(), "{path}");
        }
    }

    #[test]
    fn scans_copied_trees_for_auto_executed_files_and_protected_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pkg/src")).unwrap();
        std::fs::write(root.join("pkg/src/A.lean"), "").unwrap();
        std::fs::create_dir_all(root.join("plain")).unwrap();
        std::fs::write(root.join("plain/a.typ"), "").unwrap();
        std::fs::create_dir_all(root.join("lake/sub")).unwrap();
        std::fs::write(root.join("lake/sub/LAKEFILE.TOML"), "").unwrap();
        std::fs::create_dir_all(root.join("rproj/analysis")).unwrap();
        std::fs::write(root.join("rproj/analysis/.Rprofile"), "").unwrap();
        std::fs::create_dir_all(root.join("env/.venv/Lib")).unwrap();
        std::fs::write(root.join("env/.venv/Lib/x.pth"), "").unwrap();
        let mut budget = MAX_SCAN_ENTRIES;
        assert_eq!(scan_tree(root, &p("pkg"), &mut budget).auto, Some(AutoExec::LeanSource));
        assert_eq!(scan_tree(root, &p("plain"), &mut budget), TreeScan::default());
        assert_eq!(scan_tree(root, &p("lake"), &mut budget).auto, Some(AutoExec::LakeConfig));
        assert_eq!(scan_tree(root, &p("rproj"), &mut budget).auto, Some(AutoExec::RStartup));
        let env = scan_tree(root, &p("env"), &mut budget);
        assert_eq!(env.protected.map(|(at, p)| (at, p.folder)), Some(("env/.venv".to_owned(), ".venv")));
        // Over the bound: reported, so the gate treats it as auto-executed.
        let mut none = 0;
        assert!(scan_tree(root, &p("plain"), &mut none).truncated);
    }
}
