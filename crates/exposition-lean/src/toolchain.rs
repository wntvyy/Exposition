//! Lean toolchain resolution through elan's on-disk layout.
//!
//! Exposition never runs the elan *shims* (`~/.elan/bin/lean`, `lake`) for a
//! project: a shim downloads whatever toolchain `lean-toolchain` names when it
//! is missing, which would be an implicit installation. Instead the pinned
//! toolchain is resolved to its installed directory
//! (`<ELAN_HOME>/toolchains/<name>`), and its own `lake`/`lean` executables are
//! launched directly. A missing toolchain is reported with guidance; it is
//! installed only by an explicit user action (`elan toolchain install`).

use std::path::{Path, PathBuf};

use crate::project::{PinSource, ToolchainPin, validate_toolchain_name};

pub const DEFAULT_ORIGIN: &str = "leanprover/lean4";
const CHANNELS: &[&str] = &["stable", "beta", "nightly"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedToolchain {
    /// What was requested (`lean-toolchain` content or elan's default).
    pub requested: String,
    /// Exact toolchain used, e.g. `leanprover/lean4:v4.35.0`.
    pub name: String,
    pub dir: PathBuf,
    pub lean: PathBuf,
    pub lake: PathBuf,
    /// Explanation when `name` differs from `requested` (channel resolution).
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Installed(ResolvedToolchain),
    /// Not installed. `install` is the elan toolchain argument that installs it.
    Missing {
        requested: String,
        install: String,
        guidance: String,
    },
    /// The request cannot be satisfied (invalid name, no elan, no pin).
    Invalid {
        requested: Option<String>,
        reason: String,
    },
}

/// elan's home directory: `ELAN_HOME`, else `~/.elan`.
pub fn elan_home(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(home) = var("ELAN_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let profile = if cfg!(windows) { var("USERPROFILE") } else { var("HOME") };
    profile.filter(|v| !v.is_empty()).map(|p| PathBuf::from(p).join(".elan"))
}

/// elan's default toolchain from `settings.toml` (`default_toolchain = "..."`).
pub fn elan_default(elan_home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(elan_home.join("settings.toml")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("default_toolchain") {
            let value = rest.trim_start().strip_prefix('=')?.trim().trim_matches('"').to_owned();
            return validate_toolchain_name(&value).ok().map(|_| value);
        }
        if line.starts_with('[') {
            break; // tables follow the top-level keys
        }
    }
    None
}

/// The requested toolchain for a project: its pin, else elan's default.
pub fn effective_pin(pin: Option<ToolchainPin>, elan_home: Option<&Path>) -> Option<ToolchainPin> {
    pin.or_else(|| {
        elan_home.and_then(elan_default).map(|requested| ToolchainPin { requested, source: PinSource::ElanDefault })
    })
}

/// Fully qualified toolchain name as elan stores it.
pub fn qualify(requested: &str) -> String {
    if requested.contains(':') {
        return requested.to_owned();
    }
    let looks_like_version = requested.strip_prefix('v').unwrap_or(requested).starts_with(|c: char| c.is_ascii_digit());
    if CHANNELS.contains(&requested) {
        format!("{DEFAULT_ORIGIN}:{requested}")
    } else if looks_like_version {
        let version = if requested.starts_with('v') { requested.to_owned() } else { format!("v{requested}") };
        format!("{DEFAULT_ORIGIN}:{version}")
    } else {
        requested.to_owned() // a custom linked toolchain name
    }
}

/// elan's directory name for a qualified toolchain
/// (`leanprover/lean4:v4.35.0` → `leanprover--lean4---v4.35.0`).
pub fn dir_name(qualified: &str) -> String {
    match qualified.split_once(':') {
        Some((origin, release)) => format!("{}---{release}", origin.replace('/', "--")),
        None => qualified.to_owned(),
    }
}

fn exe(dir: &Path, name: &str) -> PathBuf {
    dir.join("bin").join(if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() })
}

fn installed(dir: PathBuf, requested: &str, name: String, note: Option<String>) -> Option<ResolvedToolchain> {
    let lean = exe(&dir, "lean");
    let lake = exe(&dir, "lake");
    (lean.is_file() && lake.is_file()).then(|| ResolvedToolchain {
        requested: requested.to_owned(),
        name,
        dir,
        lean,
        lake,
        note,
    })
}

/// Parse `vX.Y.Z` / `vX.Y.Z-rcN` into a sortable key; `None` for other names.
fn release_key(release: &str) -> Option<(u64, u64, u64, u64)> {
    let rest = release.strip_prefix('v')?;
    let (core, rc) = match rest.split_once("-rc") {
        Some((core, rc)) => (core, rc.parse::<u64>().ok()?),
        None => (rest, u64::MAX), // a final release sorts after its release candidates
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let key = (parts.next()??, parts.next()??, parts.next()??, rc);
    parts.next().is_none().then_some(key)
}

/// Resolve a requested toolchain against the installed toolchains.
pub fn resolve(elan_home: Option<&Path>, requested: &str) -> Resolution {
    if let Err(reason) = validate_toolchain_name(requested) {
        return Resolution::Invalid { requested: Some(requested.to_owned()), reason };
    }
    let qualified = qualify(requested);
    let Some(home) = elan_home else {
        return Resolution::Missing {
            requested: requested.to_owned(),
            install: qualified.clone(),
            guidance: format!(
                "elan, the Lean toolchain manager, was not found. Install it (https://lean-lang.org/install/; on Windows run elan-init.ps1 from https://github.com/leanprover/elan), then install the toolchain {qualified}."
            ),
        };
    };
    let toolchains = home.join("toolchains");
    let (origin, release) = qualified.split_once(':').unwrap_or((qualified.as_str(), ""));
    if CHANNELS.contains(&release) {
        // A channel is not a pin: elan resolves it online. Use the newest
        // installed matching release and say so; never download implicitly.
        let prefix = format!("{}---", origin.replace('/', "--"));
        let mut candidates: Vec<(String, PathBuf)> = std::fs::read_dir(&toolchains)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let rel = name.strip_prefix(&prefix)?.to_owned();
                Some((rel, e.path()))
            })
            .filter(|(rel, _)| match release {
                "stable" => release_key(rel).is_some_and(|k| k.3 == u64::MAX),
                "beta" => release_key(rel).is_some(),
                _ => rel.starts_with("nightly-"),
            })
            .collect();
        candidates.sort_by(|a, b| match (release_key(&a.0), release_key(&b.0)) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => a.0.cmp(&b.0),
        });
        for (rel, dir) in candidates.into_iter().rev() {
            let name = format!("{origin}:{rel}");
            let note = format!(
                "`{requested}` is a channel, not a pinned version; using the newest installed matching toolchain {name}. Pin an exact version in lean-toolchain for reproducible checks."
            );
            if let Some(t) = installed(dir, requested, name, Some(note)) {
                return Resolution::Installed(t);
            }
        }
        return Resolution::Missing {
            requested: requested.to_owned(),
            install: qualified.clone(),
            guidance: format!(
                "No installed Lean toolchain matches the channel `{requested}`. Install one with elan (`elan toolchain install {qualified}`), or pin an exact version in lean-toolchain. Installing downloads Lean from the internet."
            ),
        };
    }
    match installed(toolchains.join(dir_name(&qualified)), requested, qualified.clone(), None) {
        Some(t) => Resolution::Installed(t),
        None => Resolution::Missing {
            requested: requested.to_owned(),
            install: qualified.clone(),
            guidance: format!(
                "Lean toolchain {qualified} is not installed. Install it with Install toolchain (runs `elan toolchain install {qualified}`, which downloads Lean from the internet), then the project is checked with exactly that version."
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_toolchain(home: &Path, dir: &str) {
        let bin = home.join("toolchains").join(dir).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for n in ["lean", "lake"] {
            std::fs::write(exe(&home.join("toolchains").join(dir), n), b"").unwrap();
        }
    }

    #[test]
    fn names_and_directories_follow_elan() {
        assert_eq!(qualify("leanprover/lean4:v4.35.0"), "leanprover/lean4:v4.35.0");
        assert_eq!(qualify("v4.9.0"), "leanprover/lean4:v4.9.0");
        assert_eq!(qualify("4.9.0"), "leanprover/lean4:v4.9.0");
        assert_eq!(qualify("stable"), "leanprover/lean4:stable");
        assert_eq!(qualify("my-local"), "my-local");
        assert_eq!(dir_name("leanprover/lean4:v4.35.0"), "leanprover--lean4---v4.35.0");
        assert_eq!(
            dir_name("leanprover/lean4-pr-releases:pr-release-12"),
            "leanprover--lean4-pr-releases---pr-release-12"
        );
        assert!(release_key("v4.10.0") > release_key("v4.9.0"));
        assert!(release_key("v4.10.0") > release_key("v4.10.0-rc2"));
    }

    #[test]
    fn resolves_installed_missing_and_channels_without_installing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        fake_toolchain(home, "leanprover--lean4---v4.9.0");
        fake_toolchain(home, "leanprover--lean4---v4.10.0");
        fake_toolchain(home, "leanprover--lean4---v4.11.0-rc1");
        std::fs::write(home.join("settings.toml"), "default_toolchain = \"stable\"\ntelemetry = false\n").unwrap();

        match resolve(Some(home), "leanprover/lean4:v4.9.0") {
            Resolution::Installed(t) => {
                assert_eq!(t.name, "leanprover/lean4:v4.9.0");
                assert!(t.note.is_none());
                assert!(t.lake.ends_with(if cfg!(windows) { "lake.exe" } else { "lake" }));
            }
            other => panic!("{other:?}"),
        }
        match resolve(Some(home), "leanprover/lean4:v4.1.0") {
            Resolution::Missing { install, guidance, .. } => {
                assert_eq!(install, "leanprover/lean4:v4.1.0");
                assert!(guidance.contains("not installed"));
            }
            other => panic!("{other:?}"),
        }
        match resolve(Some(home), "stable") {
            Resolution::Installed(t) => {
                assert_eq!(t.name, "leanprover/lean4:v4.10.0", "release candidates are not stable");
                assert!(t.note.unwrap().contains("channel"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(resolve(Some(home), "nightly"), Resolution::Missing { .. }));
        assert!(matches!(resolve(Some(home), "--force"), Resolution::Invalid { .. }));
        assert!(matches!(resolve(None, "leanprover/lean4:v4.9.0"), Resolution::Missing { .. }));
        assert_eq!(elan_default(home).as_deref(), Some("stable"));
        // Nothing was created by resolution.
        let dirs = std::fs::read_dir(home.join("toolchains")).unwrap().count();
        assert_eq!(dirs, 3);
    }
}
