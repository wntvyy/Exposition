//! Directories too broad to be a workspace or trust root.
//!
//! Trusting or opening a directory covers everything beneath it, so a root
//! must be a specific project folder. [`too_broad_reason`] refuses a directory
//! that:
//!
//! 1. is a drive, share, or filesystem root (it has no parent);
//! 2. equals, or is an ANCESTOR of, a broad personal or temporary location:
//!    the users directory, the user profile, `AppData`, `AppData\Local`,
//!    `AppData\Roaming`, `TEMP`/`TMP`, and the profile's `Desktop`,
//!    `Documents`, `Downloads` (and their OneDrive equivalents); on Unix
//!    `HOME`, its parent, `/tmp`, `/var/tmp`, `TMPDIR`, and the same folders
//!    under `HOME`;
//! 3. equals, contains, or lies INSIDE a system directory: the Windows
//!    directory, `Program Files`, `Program Files (x86)`, `ProgramData`; on Unix
//!    `/usr`, `/etc`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/boot`, `/dev`,
//!    `/proc`, `/sys`, `/System`, `/Library`.
//!
//! Strict descendants of the rule-2 locations stay allowed: a project in
//! `Documents\paper` or `%TEMP%\checkout` is a normal workspace.
//!
//! Comparison is component-wise and ASCII case-insensitive on Windows, and
//! ignores the `\\?\` verbatim prefix. Locations come from the environment and
//! are also compared in canonical form when they exist (8.3 short names,
//! junctions). Pure `std`; callers should pass canonical paths.

use std::path::{Component, Path, PathBuf};

/// A refused location and how to describe it to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub description: &'static str,
}

/// The locations used by [`too_broad_reason_in`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BroadDirs {
    /// Rule 2: refused as the root itself or as anything above it.
    pub personal: Vec<Location>,
    /// Rule 3: refused as the root, anything above, or anything inside.
    pub system: Vec<Location>,
}

impl BroadDirs {
    /// Locations for the current process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from))
    }

    /// Locations from an environment lookup (for tests and other hosts).
    pub fn from_lookup(get: impl Fn(&str) -> Option<PathBuf>) -> Self {
        let mut dirs = BroadDirs::default();
        let mut personal = |path: Option<PathBuf>, description: &'static str| {
            if let Some(path) = path.filter(|p| p.is_absolute()) {
                dirs.personal.push(Location { path, description });
            }
        };
        if cfg!(windows) {
            let profile = get("USERPROFILE");
            personal(profile.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)), "the users folder");
            personal(
                get("SystemDrive").map(|d| PathBuf::from(format!("{}\\", d.display())).join("Users")),
                "the users folder",
            );
            personal(get("PUBLIC").and_then(|p| p.parent().map(Path::to_path_buf)), "the users folder");
            personal(profile.clone(), "your user profile folder");
            personal(profile.as_ref().map(|p| p.join("AppData")), "the AppData folder");
            personal(get("LOCALAPPDATA"), "the local application data folder");
            personal(get("APPDATA"), "the roaming application data folder");
            personal(get("TEMP"), "the temporary files folder");
            personal(get("TMP"), "the temporary files folder");
            for base in [profile, get("OneDrive")].into_iter().flatten() {
                personal(Some(base.join("Desktop")), "your Desktop folder");
                personal(Some(base.join("Documents")), "your Documents folder");
                personal(Some(base.join("Downloads")), "your Downloads folder");
            }
            personal(get("OneDrive"), "your OneDrive folder");
        } else {
            let home = get("HOME");
            personal(home.as_ref().and_then(|h| h.parent().map(Path::to_path_buf)), "the users folder");
            personal(home.clone(), "your home folder");
            personal(Some(PathBuf::from("/tmp")), "the temporary files folder");
            personal(Some(PathBuf::from("/var/tmp")), "the temporary files folder");
            personal(get("TMPDIR"), "the temporary files folder");
            if let Some(home) = home {
                personal(Some(home.join("Desktop")), "your Desktop folder");
                personal(Some(home.join("Documents")), "your Documents folder");
                personal(Some(home.join("Downloads")), "your Downloads folder");
            }
        }

        let mut system = |path: Option<PathBuf>, description: &'static str| {
            if let Some(path) = path.filter(|p| p.is_absolute()) {
                dirs.system.push(Location { path, description });
            }
        };
        if cfg!(windows) {
            system(get("SystemRoot").or_else(|| get("windir")), "the Windows folder");
            system(get("ProgramFiles"), "the Program Files folder");
            system(get("ProgramFiles(x86)"), "the Program Files (x86) folder");
            system(get("ProgramW6432"), "the Program Files folder");
            system(get("ProgramData").or_else(|| get("ALLUSERSPROFILE")), "the ProgramData folder");
        } else {
            for dir in [
                "/usr", "/etc", "/bin", "/sbin", "/lib", "/lib64", "/boot", "/dev", "/proc", "/sys", "/System",
                "/Library",
            ] {
                system(Some(PathBuf::from(dir)), "a system folder");
            }
        }

        // Also compare canonical forms (short names, junctions, symlinks).
        for list in [&mut dirs.personal, &mut dirs.system] {
            let extra: Vec<Location> = list
                .iter()
                .filter_map(|l| {
                    let canonical = std::fs::canonicalize(&l.path).ok()?;
                    (!same(&canonical, &l.path)).then_some(Location { path: canonical, description: l.description })
                })
                .collect();
            list.extend(extra);
        }
        dirs
    }
}

/// Why `path` is too broad to be a workspace/trust root, or `None` if it is a
/// specific enough folder. Uses the current environment.
pub fn too_broad_reason(path: &Path) -> Option<String> {
    too_broad_reason_in(path, &BroadDirs::from_env())
}

/// [`too_broad_reason`] against explicit locations.
pub fn too_broad_reason_in(path: &Path, dirs: &BroadDirs) -> Option<String> {
    let shown = strip_verbatim(path).display().to_string();
    if !path.is_absolute() {
        return Some(format!("{shown} is not an absolute path"));
    }
    if normal_components(path) == 0 {
        return Some(format!("{shown} is a drive or filesystem root"));
    }
    for loc in &dirs.system {
        if within(path, &loc.path) {
            return Some(format!(
                "{shown} is inside {} ({}); system folders cannot be a workspace",
                loc.description,
                loc.path.display()
            ));
        }
        if within(&loc.path, path) {
            return Some(format!("{shown} contains {} ({})", loc.description, loc.path.display()));
        }
    }
    for loc in &dirs.personal {
        if within(&loc.path, path) {
            return Some(if same(&loc.path, path) {
                format!("{shown} is {}", loc.description)
            } else {
                format!("{shown} contains {} ({})", loc.description, loc.path.display())
            });
        }
    }
    None
}

/// `\\?\C:\x` → `C:\x`, `\\?\UNC\s\h` → `\\s\h`; other paths unchanged.
fn strip_verbatim(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else { return path.to_path_buf() };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn key(component: Component<'_>) -> Option<String> {
    match component {
        Component::Prefix(p) => {
            let text = p.as_os_str().to_string_lossy().into_owned();
            let text = text.strip_prefix(r"\\?\UNC\").map(|r| format!(r"\\{r}")).unwrap_or(text);
            let text = text.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(text);
            Some(if cfg!(windows) { text.to_lowercase() } else { text })
        }
        Component::RootDir => Some("/".into()),
        Component::CurDir => None,
        Component::ParentDir => Some("..".into()),
        Component::Normal(name) => {
            let text = name.to_string_lossy().into_owned();
            Some(if cfg!(windows) { text.to_lowercase() } else { text })
        }
    }
}

fn keys(path: &Path) -> Vec<String> {
    strip_verbatim(path).components().filter_map(key).collect()
}

fn normal_components(path: &Path) -> usize {
    strip_verbatim(path).components().filter(|c| matches!(c, Component::Normal(_))).count()
}

/// `path` equals `ancestor` or lies beneath it.
fn within(path: &Path, ancestor: &Path) -> bool {
    let (p, a) = (keys(path), keys(ancestor));
    a.len() <= p.len() && p[..a.len()] == a[..]
}

fn same(a: &Path, b: &Path) -> bool {
    keys(a) == keys(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn windows_dirs() -> BroadDirs {
        let env = |name: &str| -> Option<PathBuf> {
            let value = match name {
                "USERPROFILE" => r"C:\Users\alice",
                "SystemDrive" => "C:",
                "PUBLIC" => r"C:\Users\Public",
                "LOCALAPPDATA" => r"C:\Users\alice\AppData\Local",
                "APPDATA" => r"C:\Users\alice\AppData\Roaming",
                "TEMP" | "TMP" => r"C:\Users\alice\AppData\Local\Temp",
                "SystemRoot" => r"C:\Windows",
                "ProgramFiles" => r"C:\Program Files",
                "ProgramFiles(x86)" => r"C:\Program Files (x86)",
                "ProgramData" => r"C:\ProgramData",
                _ => return None,
            };
            Some(PathBuf::from(value))
        };
        BroadDirs::from_lookup(env)
    }

    #[cfg(windows)]
    #[test]
    fn windows_broad_roots_and_their_ancestors_are_refused() {
        let dirs = windows_dirs();
        for refused in [
            r"C:\",
            r"D:\",
            r"\\?\C:\",
            r"\\server\share\",
            r"C:\Users",
            r"c:\users",
            r"C:\Users\alice",
            r"C:\Users\alice\AppData",
            r"C:\Users\alice\AppData\Local",
            r"C:\Users\alice\AppData\Roaming",
            r"C:\Users\alice\AppData\Local\Temp",
            r"C:\Users\alice\Desktop",
            r"C:\Users\alice\Documents",
            r"C:\Users\alice\Downloads",
            r"C:\Windows",
            r"C:\Windows\System32",
            r"C:\WINDOWS\system32\drivers",
            r"C:\Program Files",
            r"C:\Program Files\Some App",
            r"C:\Program Files (x86)\Tool",
            r"C:\ProgramData",
            r"C:\ProgramData\cache\x",
            r"\\?\C:\Users\alice",
        ] {
            assert!(too_broad_reason_in(Path::new(refused), &dirs).is_some(), "{refused} should be refused");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_specific_project_folders_are_allowed() {
        let dirs = windows_dirs();
        for allowed in [
            r"C:\Users\alice\Documents\paper",
            r"C:\Users\alice\Desktop\thesis",
            r"C:\Users\alice\AppData\Local\Temp\checkout",
            r"C:\Users\alice\src\exposition",
            r"C:\Users\alice\Downloads\project-1",
            r"D:\projects\paper",
            r"\\?\C:\work\paper",
            r"C:\Users\bob\paper",
        ] {
            assert_eq!(too_broad_reason_in(Path::new(allowed), &dirs), None, "{allowed} should be allowed");
        }
        let reason = too_broad_reason_in(Path::new(r"C:\Users"), &dirs).unwrap();
        assert!(reason.contains("users folder"), "{reason}");
        let reason = too_broad_reason_in(Path::new(r"C:\Windows\System32"), &dirs).unwrap();
        assert!(reason.contains("inside the Windows folder"), "{reason}");
    }

    #[cfg(unix)]
    #[test]
    fn unix_broad_roots_are_refused_and_projects_allowed() {
        let dirs = BroadDirs::from_lookup(|name| (name == "HOME").then(|| PathBuf::from("/home/alice")));
        for refused in ["/", "/home", "/home/alice", "/tmp", "/home/alice/Documents", "/usr/local/src", "/etc"] {
            assert!(too_broad_reason_in(Path::new(refused), &dirs).is_some(), "{refused}");
        }
        for allowed in ["/home/alice/paper", "/tmp/checkout", "/home/alice/Documents/thesis", "/srv/projects/x"] {
            assert_eq!(too_broad_reason_in(Path::new(allowed), &dirs), None, "{allowed}");
        }
    }

    #[test]
    fn relative_paths_are_refused() {
        assert!(too_broad_reason_in(Path::new("relative/dir"), &BroadDirs::default()).is_some());
    }

    #[test]
    fn the_real_environment_refuses_the_profile_and_its_parent() {
        let dirs = BroadDirs::from_env();
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        if let Some(home) = home {
            assert!(too_broad_reason_in(&home, &dirs).is_some());
            if let Some(parent) = home.parent() {
                assert!(too_broad_reason_in(parent, &dirs).is_some());
            }
            assert_eq!(too_broad_reason_in(&home.join("some-project-folder"), &dirs), None);
        }
    }
}
