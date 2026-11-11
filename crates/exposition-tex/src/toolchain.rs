//! TeX toolchain discovery.
//!
//! Discovery is explicit and read-only: a configured bin directory, then the
//! host `PATH`, then well-known per-user/system install locations (TinyTeX,
//! TeX Live, MiKTeX). Nothing is ever installed. A missing engine becomes
//! `BuildStatus::ToolUnavailable` with [`guidance`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use exposition_core::build::Engine;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TexTool {
    PdfLatex,
    LuaLatex,
    Bibtex,
    Biber,
    Kpsewhich,
}

impl TexTool {
    pub const ALL: [TexTool; 5] =
        [TexTool::PdfLatex, TexTool::LuaLatex, TexTool::Bibtex, TexTool::Biber, TexTool::Kpsewhich];

    pub fn name(self) -> &'static str {
        match self {
            TexTool::PdfLatex => "pdflatex",
            TexTool::LuaLatex => "lualatex",
            TexTool::Bibtex => "bibtex",
            TexTool::Biber => "biber",
            TexTool::Kpsewhich => "kpsewhich",
        }
    }

    pub fn for_engine(engine: Engine) -> Option<TexTool> {
        match engine {
            Engine::PdfLatex => Some(TexTool::PdfLatex),
            Engine::LuaLatex => Some(TexTool::LuaLatex),
            _ => None,
        }
    }

    fn file_name(self) -> String {
        if cfg!(windows) { format!("{}.exe", self.name()) } else { self.name().to_owned() }
    }
}

/// A discovered TeX distribution (one bin directory).
#[derive(Clone, Debug)]
pub struct TexToolchain {
    pub bin_dir: PathBuf,
    tools: BTreeMap<TexTool, PathBuf>,
}

impl TexToolchain {
    /// Look for a distribution: `explicit_bin_dir`, then `PATH`, then
    /// well-known locations. The first directory containing an engine wins.
    pub fn discover(explicit_bin_dir: Option<&Path>) -> Option<Self> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = explicit_bin_dir {
            candidates.push(dir.to_path_buf());
        }
        if let Some(path) = std::env::var_os("PATH") {
            candidates.extend(std::env::split_paths(&path));
        }
        candidates.extend(well_known_bin_dirs());
        candidates.into_iter().find_map(|dir| Self::in_dir(&dir))
    }

    /// The toolchain in exactly `bin_dir`, if it contains at least one engine.
    pub fn in_dir(bin_dir: &Path) -> Option<Self> {
        let mut tools = BTreeMap::new();
        for tool in TexTool::ALL {
            let path = bin_dir.join(tool.file_name());
            if path.is_file() {
                tools.insert(tool, path);
            }
        }
        let has_engine = tools.contains_key(&TexTool::PdfLatex) || tools.contains_key(&TexTool::LuaLatex);
        has_engine.then(|| Self { bin_dir: bin_dir.to_path_buf(), tools })
    }

    pub fn tool(&self, tool: TexTool) -> Option<&Path> {
        self.tools.get(&tool).map(PathBuf::as_path)
    }
}

/// Actionable setup text for a missing tool.
pub fn guidance(tool: TexTool) -> String {
    let what = match tool {
        TexTool::PdfLatex | TexTool::LuaLatex => "a TeX distribution",
        TexTool::Bibtex | TexTool::Biber => "the bibliography tool",
        TexTool::Kpsewhich => "kpathsea",
    };
    format!(
        "{} was not found. Install {what} (TeX Live, MiKTeX, or TinyTeX) that provides `{}`, then add its bin \
         directory to PATH or set the TeX bin directory in Exposition settings. Exposition does not install \
         toolchains automatically.",
        tool.name(),
        tool.name()
    )
}

fn well_known_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("TinyTeX").join("bin").join("windows"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(&local).join("Programs").join("MiKTeX").join("miktex").join("bin").join("x64"));
        }
        dirs.push(PathBuf::from(r"C:\Program Files\MiKTeX\miktex\bin\x64"));
        for year in (2020..=2030).rev() {
            dirs.push(PathBuf::from(format!(r"C:\texlive\{year}\bin\windows")));
            dirs.push(PathBuf::from(format!(r"C:\texlive\{year}\bin\win64")));
        }
    } else {
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            dirs.push(home.join(".TinyTeX").join("bin").join("x86_64-linux"));
            dirs.push(home.join("Library").join("TinyTeX").join("bin").join("universal-darwin"));
        }
        dirs.push(PathBuf::from("/Library/TeX/texbin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/usr/bin"));
    }
    dirs
}
