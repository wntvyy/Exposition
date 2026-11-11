//! Toolchain discovery.
//!
//! Location is cheap and side-effect free (settings override → well-known
//! install locations → host `PATH`); it is what job program resolution uses.
//! Versions come from *application probes*: fixed version queries run through
//! the execution service with an application-owned working directory, no
//! project content, and a clean environment. Probes are the one class of
//! native execution that does not require workspace trust.
//!
//! Missing tools get actionable guidance; nothing is ever installed.

use std::path::{Path, PathBuf};

use exposition_core::job::JobOutcome;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::env::HostEnv;
use crate::settings::ExecutionSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ToolStatus {
    Available,
    Missing,
    Broken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ToolSource {
    /// Configured in execution settings.
    Override,
    /// A well-known install location.
    KnownLocation,
    /// Found on the application's `PATH`.
    Path,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainInfo {
    /// Stable id; also accepted as `JobSpec.program` (except kernel components).
    pub id: String,
    pub name: String,
    /// `tex`, `python`, `r`, `julia`, `lean`, `container`, `vcs`.
    pub category: String,
    pub path: Option<String>,
    pub source: Option<ToolSource>,
    pub version: Option<String>,
    pub status: ToolStatus,
    /// Extra facts, e.g. why a probe failed.
    pub detail: Option<String>,
    /// What the user can do when the tool is missing or broken.
    pub guidance: Option<String>,
}

/// How a tool's version is queried.
#[derive(Clone, Copy, Debug)]
pub struct ToolDef {
    pub id: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    /// Executable base name (without `.exe`). For kernel components, the id of
    /// the interpreter tool they are checked with.
    pub exe: &'static str,
    /// True for kernel components checked through another tool's interpreter.
    pub component_of: Option<&'static str>,
    pub probe_args: &'static [&'static str],
    pub guidance: &'static str,
}

const TEX_GUIDANCE: &str = "Install a TeX distribution: TinyTeX (https://yihui.org/tinytex/), TeX Live (https://tug.org/texlive/), or MiKTeX (https://miktex.org/), then refresh. Or set its path in execution settings.";

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        id: "pdflatex",
        name: "pdfLaTeX",
        category: "tex",
        exe: "pdflatex",
        component_of: None,
        probe_args: &["--version"],
        guidance: TEX_GUIDANCE,
    },
    ToolDef {
        id: "lualatex",
        name: "LuaLaTeX",
        category: "tex",
        exe: "lualatex",
        component_of: None,
        probe_args: &["--version"],
        guidance: TEX_GUIDANCE,
    },
    ToolDef {
        id: "latexmk",
        name: "latexmk",
        category: "tex",
        exe: "latexmk",
        component_of: None,
        probe_args: &["-v"],
        guidance: "Install latexmk with your TeX distribution (TeX Live/TinyTeX: `tlmgr install latexmk`; MiKTeX: package manager), then refresh.",
    },
    ToolDef {
        id: "biber",
        name: "Biber",
        category: "tex",
        exe: "biber",
        component_of: None,
        probe_args: &["--version"],
        guidance: "Install Biber with your TeX distribution (TeX Live/TinyTeX: `tlmgr install biber`), then refresh.",
    },
    ToolDef {
        id: "bibtex",
        name: "BibTeX",
        category: "tex",
        exe: "bibtex",
        component_of: None,
        probe_args: &["--version"],
        guidance: TEX_GUIDANCE,
    },
    ToolDef {
        id: "synctex",
        name: "SyncTeX CLI",
        category: "tex",
        exe: "synctex",
        component_of: None,
        probe_args: &["help"],
        guidance: "The standalone SyncTeX command-line tool is optional (TeX Live/TinyTeX: `tlmgr install synctex`). PDF/source navigation can still use .synctex.gz files directly.",
    },
    ToolDef {
        id: "python",
        name: "Python",
        category: "python",
        exe: "python",
        component_of: None,
        probe_args: &["-E", "-c", "import sys; print('Python ' + sys.version.split()[0])"],
        guidance: "Install Python 3 from https://www.python.org/downloads/ (or the Python install manager), then refresh. The Microsoft Store `python` alias is not an interpreter.",
    },
    ToolDef {
        id: "python-jupyter",
        name: "Python Jupyter kernel (jupyter_client + ipykernel)",
        category: "python",
        exe: "python",
        component_of: Some("python"),
        probe_args: &[
            "-E",
            "-c",
            "import jupyter_client, ipykernel; print('jupyter_client ' + jupyter_client.__version__ + ', ipykernel ' + ipykernel.__version__)",
        ],
        guidance: "Install the Jupyter kernel packages into the selected Python: `python -m pip install jupyter_client ipykernel`, then refresh.",
    },
    ToolDef {
        id: "rscript",
        name: "Rscript",
        category: "r",
        exe: "Rscript",
        component_of: None,
        probe_args: &["--version"],
        guidance: "Install R from https://cran.r-project.org/ (Windows: R-x.y.z-win.exe), then refresh. Rscript is used to run R code and kernels.",
    },
    ToolDef {
        id: "r",
        name: "R",
        category: "r",
        exe: "R",
        component_of: None,
        probe_args: &["--version"],
        guidance: "Install R from https://cran.r-project.org/, then refresh.",
    },
    ToolDef {
        id: "irkernel",
        name: "R Jupyter kernel (IRkernel)",
        category: "r",
        exe: "Rscript",
        component_of: Some("rscript"),
        probe_args: &[
            "-e",
            "if (!requireNamespace('IRkernel', quietly = TRUE)) quit(status = 3); cat('IRkernel', as.character(packageVersion('IRkernel')))",
        ],
        guidance: "Install R first, then in R run `install.packages('IRkernel')` and `IRkernel::installspec()`, then refresh.",
    },
    ToolDef {
        id: "julia",
        name: "Julia",
        category: "julia",
        exe: "julia",
        component_of: None,
        probe_args: &["--version"],
        guidance: "Install Julia with juliaup (https://julialang.org/downloads/; Windows: `winget install --id Julialang.Juliaup`), then refresh.",
    },
    ToolDef {
        id: "ijulia",
        name: "Julia Jupyter kernel (IJulia)",
        category: "julia",
        exe: "julia",
        component_of: Some("julia"),
        probe_args: &[
            "--startup-file=no",
            "-e",
            "p = Base.find_package(\"IJulia\"); p === nothing && exit(3); print(\"IJulia installed\")",
        ],
        guidance: "Install Julia first, then in Julia run `using Pkg; Pkg.add(\"IJulia\")`, then refresh.",
    },
    ToolDef {
        id: "lean",
        name: "Lean",
        category: "lean",
        exe: "lean",
        component_of: None,
        probe_args: &["--version"],
        guidance: "Install Lean through elan (https://lean-lang.org/install/; Windows: run the elan-init.ps1 installer from https://github.com/leanprover/elan), then refresh. Projects pin their Lean version in lean-toolchain.",
    },
    ToolDef {
        id: "lake",
        name: "Lake",
        category: "lean",
        exe: "lake",
        component_of: None,
        probe_args: &["--version"],
        guidance: "Lake ships with Lean; install Lean through elan (https://lean-lang.org/install/), then refresh.",
    },
    ToolDef {
        id: "elan",
        name: "elan",
        category: "lean",
        exe: "elan",
        component_of: None,
        probe_args: &["--version"],
        guidance: "Install the Lean toolchain manager elan (https://github.com/leanprover/elan), then refresh.",
    },
    ToolDef {
        id: "docker",
        name: "Docker",
        category: "container",
        exe: "docker",
        component_of: None,
        probe_args: &["version", "--format", "client {{.Client.Version}}, engine {{.Server.Version}}"],
        guidance: "Restricted (container) execution needs Docker Desktop (https://docs.docker.com/desktop/) running, plus a locally available image configured in execution settings. Without it, restricted jobs are refused — never run natively instead.",
    },
    ToolDef {
        id: "git",
        name: "Git",
        category: "vcs",
        exe: "git",
        component_of: None,
        probe_args: &["--version"],
        guidance: "Install Git from https://git-scm.com/downloads, then refresh.",
    },
];

pub fn tool(id: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.id == id)
}

/// Result of locating a tool executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    Found {
        path: PathBuf,
        source: ToolSource,
    },
    /// A settings override points at something that is not an executable.
    BadOverride {
        path: String,
    },
    NotFound,
}

/// Finds tool executables without running anything.
pub struct Locator<'a> {
    pub host: &'a HostEnv,
    pub settings: &'a ExecutionSettings,
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) { format!("{base}.exe") } else { base.to_owned() }
}

/// Numeric components of a name, for "highest version first" ordering.
fn version_key(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

/// Subdirectories of `parent` whose names start with `prefix`, highest version first.
fn versioned_dirs(parent: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()))
        .map(|e| e.path())
        .collect();
    dirs.sort_by(|a, b| {
        let ka = version_key(&a.file_name().unwrap_or_default().to_string_lossy());
        let kb = version_key(&b.file_name().unwrap_or_default().to_string_lossy());
        kb.cmp(&ka)
    });
    dirs
}

/// True for an existing regular file (or app-execution alias) path.
pub fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
        || std::fs::symlink_metadata(path).map(|m| !m.is_dir()).unwrap_or(false)
}

impl Locator<'_> {
    fn env_dir(&self, var: &str) -> Option<PathBuf> {
        self.host.get(var).filter(|v| !v.is_empty()).map(PathBuf::from)
    }

    /// Directories to search for `exe` before `PATH`, most preferred first.
    fn known_dirs(&self, exe: &str) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        #[cfg(windows)]
        {
            let appdata = self.env_dir("APPDATA");
            let local = self.env_dir("LOCALAPPDATA");
            let profile = self.env_dir("USERPROFILE");
            let program_files = self.env_dir("ProgramFiles").unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
            match exe {
                "pdflatex" | "lualatex" | "latexmk" | "biber" | "bibtex" | "synctex" => {
                    if let Some(appdata) = &appdata {
                        for arch in ["windows", "win64", "win32"] {
                            dirs.push(appdata.join("TinyTeX").join("bin").join(arch));
                        }
                    }
                    for year in versioned_dirs(Path::new(r"C:\texlive"), "20") {
                        dirs.push(year.join("bin").join("windows"));
                        dirs.push(year.join("bin").join("win64"));
                        dirs.push(year.join("bin").join("win32"));
                    }
                    if let Some(local) = &local {
                        dirs.push(local.join("Programs").join("MiKTeX").join("miktex").join("bin").join("x64"));
                    }
                    dirs.push(program_files.join("MiKTeX").join("miktex").join("bin").join("x64"));
                }
                "python" => {
                    if let Some(local) = &local {
                        dirs.extend(versioned_dirs(&local.join("Python"), "pythoncore-"));
                        dirs.extend(versioned_dirs(&local.join("Programs").join("Python"), "Python3"));
                    }
                    dirs.extend(versioned_dirs(&program_files, "Python3"));
                    if let Some(profile) = &profile {
                        for conda in ["miniforge3", "miniconda3", "anaconda3"] {
                            dirs.push(profile.join(conda));
                        }
                    }
                }
                "Rscript" | "R" => {
                    for base in [Some(program_files.join("R")), local.as_ref().map(|l| l.join("Programs").join("R"))]
                        .into_iter()
                        .flatten()
                    {
                        for version in versioned_dirs(&base, "R-") {
                            dirs.push(version.join("bin"));
                            dirs.push(version.join("bin").join("x64"));
                        }
                    }
                }
                "julia" => {
                    if let Some(local) = &local {
                        for version in versioned_dirs(&local.join("Programs"), "Julia") {
                            dirs.push(version.join("bin"));
                        }
                        // juliaup installs an app-execution alias here.
                        dirs.push(local.join("Microsoft").join("WindowsApps"));
                    }
                }
                "lean" | "lake" | "elan" => {
                    if let Some(profile) = &profile {
                        dirs.push(profile.join(".elan").join("bin"));
                    }
                }
                "docker" => {
                    dirs.push(program_files.join("Docker").join("Docker").join("resources").join("bin"));
                    if let Some(local) = &local {
                        dirs.push(local.join("Programs").join("DockerDesktop").join("resources").join("bin"));
                    }
                }
                "git" => {
                    dirs.push(program_files.join("Git").join("cmd"));
                    if let Some(local) = &local {
                        dirs.push(local.join("Programs").join("Git").join("cmd"));
                    }
                }
                _ => {}
            }
        }
        #[cfg(not(windows))]
        {
            if let Some(home) = self.env_dir("HOME") {
                dirs.push(home.join(".elan").join("bin"));
                dirs.push(home.join(".juliaup").join("bin"));
            }
            dirs.push(PathBuf::from("/Library/TeX/texbin"));
            dirs.push(PathBuf::from("/opt/homebrew/bin"));
            dirs.push(PathBuf::from("/usr/local/bin"));
            dirs.push(PathBuf::from("/usr/bin"));
            let _ = exe;
        }
        dirs
    }

    /// Host PATH directories, skipping Windows Store python stubs.
    fn path_dirs(&self, exe: &str) -> Vec<PathBuf> {
        let sep = if cfg!(windows) { ';' } else { ':' };
        self.host
            .get("PATH")
            .unwrap_or_default()
            .split(sep)
            .filter(|d| !d.trim().is_empty())
            .map(|d| PathBuf::from(d.trim()))
            .filter(|d| d.is_absolute())
            .filter(|d| !(exe == "python" && d.to_string_lossy().to_ascii_lowercase().contains("windowsapps")))
            .collect()
    }

    /// Locate a tool executable by tool id.
    pub fn locate(&self, id: &str) -> Located {
        let Some(def) = tool(id) else { return Located::NotFound };
        let base_id = def.component_of.unwrap_or(def.id);
        if let Some(raw) = self.settings.tool_paths.get(base_id) {
            let path = PathBuf::from(raw);
            return if path.is_absolute() && is_executable_file(&path) {
                Located::Found { path, source: ToolSource::Override }
            } else {
                Located::BadOverride { path: raw.clone() }
            };
        }
        let name = exe_name(def.exe);
        for dir in self.known_dirs(def.exe) {
            let candidate = dir.join(&name);
            if is_executable_file(&candidate) {
                return Located::Found { path: candidate, source: ToolSource::KnownLocation };
            }
        }
        for dir in self.path_dirs(def.exe) {
            let candidate = dir.join(&name);
            if is_executable_file(&candidate) {
                return Located::Found { path: candidate, source: ToolSource::Path };
            }
        }
        Located::NotFound
    }

    /// Directories of every located tool (plus Python's `Scripts`), in catalog
    /// order: the toolchain part of a constructed child `PATH`.
    pub fn toolchain_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for def in TOOLS.iter().filter(|t| t.component_of.is_none()) {
            if let Located::Found { path, .. } = self.locate(def.id)
                && let Some(dir) = path.parent()
            {
                let mut add = vec![dir.to_path_buf()];
                if def.id == "python" {
                    add.push(dir.join(if cfg!(windows) { "Scripts" } else { "bin" }));
                }
                for d in add {
                    if !dirs.iter().any(|x| crate::paths::same_path(x, &d)) {
                        dirs.push(d);
                    }
                }
            }
        }
        dirs
    }
}

/// First meaningful line of probe output.
fn first_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(200).collect())
}

/// Interpret a probe's outcome.
pub fn interpret_probe(
    def: &ToolDef,
    outcome: &JobOutcome,
    stdout: &str,
    stderr: &str,
) -> (ToolStatus, Option<String>, Option<String>) {
    let line = first_line(stdout).or_else(|| first_line(stderr));
    match outcome {
        JobOutcome::Exited { code: 0 } => {
            let version = match def.id {
                // latexmk prints a blank line and its banner.
                "latexmk" => stdout.lines().map(str::trim).find(|l| l.contains("Version")).map(str::to_owned).or(line),
                _ => line,
            };
            (ToolStatus::Available, version, None)
        }
        JobOutcome::Exited { code } if def.component_of.is_some() => {
            (ToolStatus::Missing, None, Some(format!("not installed (check exited with code {code})")))
        }
        JobOutcome::Exited { code } if def.id == "docker" => (
            ToolStatus::Broken,
            None,
            Some(format!("Docker client found but the engine is not reachable (exit {code}); start Docker Desktop.")),
        ),
        JobOutcome::Exited { code } => (
            ToolStatus::Broken,
            None,
            Some(format!("version check exited with code {code}: {}", line.unwrap_or_default())),
        ),
        JobOutcome::TimedOut => (ToolStatus::Broken, None, Some("version check timed out".into())),
        JobOutcome::Canceled => (ToolStatus::Broken, None, Some("version check was canceled".into())),
        JobOutcome::Terminated { description } => {
            (ToolStatus::Broken, None, Some(format!("version check {description}")))
        }
        JobOutcome::FailedToStart { reason } => (ToolStatus::Broken, None, Some(reason.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering_is_numeric() {
        let tmp = tempfile::tempdir().unwrap();
        for d in ["pythoncore-3.9-64", "pythoncore-3.14-64", "pythoncore-3.12-64", "other"] {
            std::fs::create_dir(tmp.path().join(d)).unwrap();
        }
        let names: Vec<String> = versioned_dirs(tmp.path(), "pythoncore-")
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["pythoncore-3.14-64", "pythoncore-3.12-64", "pythoncore-3.9-64"]);
    }

    #[test]
    fn override_wins_and_bad_override_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join(exe_name("mytex"));
        std::fs::write(&exe, "x").unwrap();
        let host = HostEnv::from_pairs([("PATH", "")]);
        let mut settings = ExecutionSettings::default();
        settings.tool_paths.insert("pdflatex".into(), exe.display().to_string());
        settings.tool_paths.insert("python".into(), tmp.path().join("nope.exe").display().to_string());
        let locator = Locator { host: &host, settings: &settings };
        assert_eq!(locator.locate("pdflatex"), Located::Found { path: exe, source: ToolSource::Override });
        assert!(matches!(locator.locate("python"), Located::BadOverride { .. }));
        assert!(matches!(locator.locate("python-jupyter"), Located::BadOverride { .. }), "components follow base");
        assert_eq!(locator.locate("not-a-tool"), Located::NotFound);
    }

    #[test]
    fn path_search_skips_store_python_stub() {
        let tmp = tempfile::tempdir().unwrap();
        let stub_dir = tmp.path().join("Microsoft").join("WindowsApps");
        std::fs::create_dir_all(&stub_dir).unwrap();
        std::fs::write(stub_dir.join(exe_name("python")), "stub").unwrap();
        std::fs::write(stub_dir.join(exe_name("git")), "x").unwrap();
        let host = HostEnv::from_pairs([("PATH", stub_dir.display().to_string())]);
        let settings = ExecutionSettings::default();
        let locator = Locator { host: &host, settings: &settings };
        assert!(locator.path_dirs("python").is_empty());
        assert_eq!(locator.path_dirs("git").len(), 1);
    }

    #[test]
    fn probe_interpretation() {
        let def = tool("pdflatex").unwrap();
        let (status, version, _) = interpret_probe(
            def,
            &JobOutcome::Exited { code: 0 },
            "pdfTeX 3.141592653-2.6-1.40.29 (TeX Live 2026)\nkpathsea version 6.4.2\n",
            "",
        );
        assert_eq!(status, ToolStatus::Available);
        assert_eq!(version.as_deref(), Some("pdfTeX 3.141592653-2.6-1.40.29 (TeX Live 2026)"));
        let jupyter = tool("python-jupyter").unwrap();
        let (status, _, detail) = interpret_probe(jupyter, &JobOutcome::Exited { code: 1 }, "", "ModuleNotFoundError");
        assert_eq!(status, ToolStatus::Missing);
        assert!(detail.is_some());
        let (status, _, _) = interpret_probe(def, &JobOutcome::TimedOut, "", "");
        assert_eq!(status, ToolStatus::Broken);
    }
}
