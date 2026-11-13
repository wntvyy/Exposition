//! Research runtime discovery.
//!
//! Finds real Python interpreters (and whether jupyter_client/ipykernel are
//! present), R with an IRkernel kernelspec, and Julia with an IJulia kernelspec.
//! Every probe is a short process run through the launcher with a fixed
//! argument array, a neutral working directory, and the research environment.
//! Missing runtimes produce `Unavailable` entries with exact setup guidance;
//! nothing is ever installed.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use exposition_core::artifact::ToolVersion;
use exposition_core::job::{ExecutionPolicy, JobSpec};
use serde::Deserialize;

use crate::launcher::{KernelProcessLauncher, looks_secret, research_environment};
use crate::types::{Availability, EnvironmentReport, KernelChoice, RuntimeEnvironment, RuntimeLanguage};

const PROBE_MARKER: &str = "EXPOSITION-PROBE";

/// Python probe: versions from package metadata and module presence; kernelspec
/// listing imports jupyter_client (proving it imports) when it is present.
const PYTHON_PROBE: &str = r#"
import json, sys
from importlib import metadata, util
def version(name):
    try:
        return metadata.version(name)
    except Exception:
        return None
names = ["jupyter_client", "ipykernel", "pyzmq", "numpy", "pandas", "matplotlib", "scipy", "sympy", "polars"]
out = {
    "version": ".".join(str(p) for p in sys.version_info[:3]),
    "implementation": sys.implementation.name,
    "executable": sys.executable,
    "packages": {n: version(n) for n in names},
    "importable": {n: util.find_spec(n) is not None for n in ("jupyter_client", "ipykernel")},
    "kernelspecs": [],
}
if out["importable"]["jupyter_client"]:
    try:
        from jupyter_client.kernelspec import KernelSpecManager
        for name, entry in sorted(KernelSpecManager().get_all_specs().items()):
            spec = entry.get("spec", {})
            argv = spec.get("argv") or []
            out["kernelspecs"].append({"name": name, "language": spec.get("language", ""),
                "displayName": spec.get("display_name", name), "argv0": argv[0] if argv else None})
    except Exception as error:
        out["importable"]["jupyter_client"] = False
        out["kernelspecError"] = f"{type(error).__name__}: {error}"
print("EXPOSITION-PROBE" + json.dumps(out))
"#;

pub const PYTHON_MISSING_GUIDANCE: &str = "Install Python 3.10 or newer (https://www.python.org/downloads/), then install the Jupyter kernel packages into it with: python -m pip install ipykernel jupyter_client — then refresh environments. Exposition never installs runtimes or packages itself.";

pub const R_MISSING_GUIDANCE: &str = "Install R 4.x from https://cran.r-project.org/, then in R run: install.packages('IRkernel'); IRkernel::installspec() — then refresh environments. Exposition never installs runtimes or packages itself.";

pub const IRKERNEL_SPEC_GUIDANCE: &str =
    "In R, run: IRkernel::installspec() — then refresh environments. Exposition never registers kernels itself.";

pub const IJULIA_SPEC_GUIDANCE: &str = "In Julia, run: using IJulia; installkernel(\"Julia\") — then refresh environments. Exposition never registers kernels itself.";

pub const IRKERNEL_MISSING_GUIDANCE: &str = "In R, run: install.packages('IRkernel'); IRkernel::installspec() — then refresh environments. Exposition never installs packages itself.";

pub const JULIA_MISSING_GUIDANCE: &str = "Install Julia from https://julialang.org/downloads/ (juliaup is recommended), then in Julia run: using Pkg; Pkg.add(\"IJulia\") — then refresh environments. Exposition never installs runtimes or packages itself.";

pub const IJULIA_MISSING_GUIDANCE: &str = "In Julia, run: using Pkg; Pkg.add(\"IJulia\") — then refresh environments. Exposition never installs packages itself.";

pub const HOST_MISSING_GUIDANCE: &str = "R and Julia kernels run through Exposition's Jupyter helper, which needs a Python interpreter with jupyter_client: python -m pip install jupyter_client — then refresh environments. Exposition never installs packages itself.";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PythonProbe {
    version: String,
    implementation: String,
    executable: String,
    packages: BTreeMap<String, Option<String>>,
    importable: BTreeMap<String, bool>,
    #[serde(default)]
    kernelspecs: Vec<ProbedKernelspec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbedKernelspec {
    name: String,
    language: String,
    display_name: String,
}

#[derive(Debug, Clone)]
struct ProbedPython {
    path: PathBuf,
    probe: PythonProbe,
}

impl ProbedPython {
    fn has(&self, module: &str) -> bool {
        self.probe.importable.get(module).copied().unwrap_or(false)
    }

    fn kernel_ready(&self) -> bool {
        self.has("jupyter_client") && self.has("ipykernel")
    }
}

/// Discovery inputs.
pub struct DiscoveryOptions<'a> {
    pub launcher: &'a dyn KernelProcessLauncher,
    /// Neutral working directory for probes (application-owned).
    pub probe_dir: &'a Path,
    /// The open project. Interpreters inside it (e.g. `.venv`) are project
    /// content: they are probed only when the project is trusted, as trusted
    /// project jobs, and never as application probes.
    pub project: Option<ProjectScope>,
}

#[derive(Debug, Clone)]
pub struct ProjectScope {
    /// Canonical project root.
    pub root: PathBuf,
    pub trusted: bool,
}

/// Interpreter locations inside a project that are offered when it is trusted.
fn project_candidates(root: &Path) -> Vec<PathBuf> {
    let (dir, exe) = if cfg!(windows) { ("Scripts", "python.exe") } else { ("bin", "python") };
    [".venv", "venv"].iter().map(|venv| root.join(venv).join(dir).join(exe)).collect()
}

fn inside(path: &Path, root: &Path) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical.starts_with(root) || path.starts_with(root)
}

pub fn discover(options: &DiscoveryOptions<'_>) -> EnvironmentReport {
    let project_root = options.project.as_ref().map(|p| p.root.clone());
    // System interpreters only: anything that resolves into the project is project content.
    let system: Vec<PathBuf> = dedupe(python_candidates(options))
        .into_iter()
        .filter(|p| project_root.as_ref().is_none_or(|root| !inside(p, root)))
        .collect();
    let project: Vec<PathBuf> = match &options.project {
        Some(scope) if scope.trusted => dedupe(project_candidates(&scope.root)),
        _ => Vec::new(),
    };
    let candidates: Vec<(PathBuf, bool)> =
        system.into_iter().map(|p| (p, false)).chain(project.into_iter().map(|p| (p, true))).collect();

    let probed: Vec<Option<ProbedPython>> = std::thread::scope(|scope| {
        let handles: Vec<_> = candidates
            .iter()
            .map(|(path, in_project)| {
                scope.spawn(move || {
                    let probe =
                        if *in_project { probe_project_python(options, path) } else { probe_python(options, path) };
                    probe.map(|probe| ProbedPython { path: path.clone(), probe })
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    let mut pythons: Vec<ProbedPython> = probed.into_iter().flatten().collect();
    // Dedupe interpreters that resolve to the same executable (py launcher + PATH).
    let mut seen = HashSet::new();
    pythons.retain(|p| seen.insert(normalize(Path::new(&p.probe.executable))));
    pythons.sort_by(|a, b| {
        b.kernel_ready()
            .cmp(&a.kernel_ready())
            .then_with(|| version_key(&b.probe.version).cmp(&version_key(&a.probe.version)))
    });

    let mut runtimes: Vec<RuntimeEnvironment> = pythons.iter().map(python_runtime).collect();
    if pythons.is_empty() {
        runtimes.push(RuntimeEnvironment {
            id: "python:none".into(),
            language: RuntimeLanguage::Python,
            display_name: "Python".into(),
            executable: None,
            version: None,
            kernel: None,
            host_interpreter: None,
            packages: Vec::new(),
            kernel_environment: BTreeMap::new(),
            availability: Availability::Unavailable {
                reason: "No Python interpreter was found.".into(),
                guidance: PYTHON_MISSING_GUIDANCE.into(),
            },
        });
    }
    let host = pythons.iter().find(|p| p.has("jupyter_client"));
    runtimes.push(kernelspec_runtime(options, host, RuntimeLanguage::R));
    runtimes.push(kernelspec_runtime(options, host, RuntimeLanguage::Julia));
    EnvironmentReport { runtimes, scanned_at_ms: now_ms() }
}

fn python_runtime(python: &ProbedPython) -> RuntimeEnvironment {
    let probe = &python.probe;
    let executable = probe.executable.clone();
    let packages: Vec<ToolVersion> = probe
        .packages
        .iter()
        .filter_map(|(name, version)| version.as_ref().map(|v| ToolVersion { name: name.clone(), version: v.clone() }))
        .collect();
    let missing: Vec<&str> = ["ipykernel", "jupyter_client"].into_iter().filter(|m| !python.has(m)).collect();
    let availability = if missing.is_empty() {
        Availability::Available
    } else {
        Availability::Unavailable {
            reason: format!("Python {} at {} lacks {}.", probe.version, executable, missing.join(" and ")),
            guidance: format!(
                "Install them into this interpreter: \"{executable}\" -m pip install {} — then refresh environments. Exposition never installs packages itself.",
                missing.join(" ")
            ),
        }
    };
    RuntimeEnvironment {
        id: format!("python:{}", python.path.display()),
        language: RuntimeLanguage::Python,
        display_name: format!("Python {} ({})", probe.version, implementation_name(&probe.implementation)),
        executable: Some(executable.clone()),
        version: Some(probe.version.clone()),
        kernel: missing.is_empty().then_some(KernelChoice::Python),
        host_interpreter: Some(python.path.display().to_string()),
        packages,
        kernel_environment: BTreeMap::new(),
        availability,
    }
}

fn implementation_name(name: &str) -> &str {
    match name {
        "cpython" => "CPython",
        "pypy" => "PyPy",
        other => other,
    }
}

fn kernelspec_runtime(
    options: &DiscoveryOptions<'_>,
    host: Option<&ProbedPython>,
    language: RuntimeLanguage,
) -> RuntimeEnvironment {
    let (label, wanted) = match language {
        RuntimeLanguage::R => ("R", "r"),
        RuntimeLanguage::Julia => ("Julia", "julia"),
        RuntimeLanguage::Python => unreachable!("python is discovered directly"),
    };
    let executable = match language {
        RuntimeLanguage::R => r_candidates().into_iter().find(|p| p.is_file()),
        _ => julia_candidates().into_iter().find(|p| p.is_file()),
    };
    let probe = executable.as_ref().and_then(|exe| probe_runtime(options, exe, language)).unwrap_or_default();
    let version = probe.version.clone();
    let spec = host.and_then(|h| h.probe.kernelspecs.iter().find(|s| s.language.eq_ignore_ascii_case(wanted)));
    let mut packages = Vec::new();
    if let Some(v) = &version {
        packages.push(ToolVersion { name: wanted.into(), version: v.clone() });
    }
    if let Some(v) = &probe.kernel_package_version {
        let name = if language == RuntimeLanguage::R { "IRkernel" } else { "IJulia" };
        packages.push(ToolVersion { name: name.into(), version: v.clone() });
    }
    let display_name = match &version {
        Some(v) => format!("{label} {v}"),
        None => label.to_owned(),
    };
    let base = RuntimeEnvironment {
        id: format!("{wanted}:none"),
        language,
        display_name,
        executable: executable.as_ref().map(|p| p.display().to_string()),
        version: version.clone(),
        kernel: None,
        host_interpreter: host.map(|h| h.path.display().to_string()),
        packages,
        kernel_environment: probe.kernel_environment.clone(),
        availability: Availability::Available,
    };
    match (spec, host, &executable) {
        (Some(spec), Some(_), _) => RuntimeEnvironment {
            id: format!("{wanted}:{}", spec.name),
            display_name: format!("{} ({})", base.display_name, spec.display_name),
            kernel: Some(KernelChoice::Kernelspec { name: spec.name.clone() }),
            ..base
        },
        (_, None, Some(exe)) => RuntimeEnvironment {
            availability: Availability::Unavailable {
                reason: format!(
                    "{label} was found at {}, but no Python with jupyter_client is available to host its kernel.",
                    exe.display()
                ),
                guidance: HOST_MISSING_GUIDANCE.into(),
            },
            ..base
        },
        (None, _, Some(exe)) => {
            let package = if language == RuntimeLanguage::R { "IRkernel" } else { "IJulia" };
            let version_text = version.as_ref().map(|v| format!(" {v}")).unwrap_or_default();
            let (reason, guidance) = if probe.kernel_package_installed {
                (
                    format!(
                        "{label}{version_text} at {} has {package} installed, but its Jupyter kernelspec is not registered.",
                        exe.display()
                    ),
                    if language == RuntimeLanguage::R { IRKERNEL_SPEC_GUIDANCE } else { IJULIA_SPEC_GUIDANCE },
                )
            } else {
                (
                    format!(
                        "{label}{version_text} is installed at {}, but its Jupyter kernel ({package}) is not installed.",
                        exe.display()
                    ),
                    if language == RuntimeLanguage::R { IRKERNEL_MISSING_GUIDANCE } else { IJULIA_MISSING_GUIDANCE },
                )
            };
            RuntimeEnvironment { availability: Availability::Unavailable { reason, guidance: guidance.into() }, ..base }
        }
        (_, _, None) => RuntimeEnvironment {
            availability: Availability::Unavailable {
                reason: format!("{label} was not found on this computer."),
                guidance: if language == RuntimeLanguage::R { R_MISSING_GUIDANCE } else { JULIA_MISSING_GUIDANCE }
                    .into(),
            },
            ..base
        },
    }
}

fn probe_spec(options: &DiscoveryOptions<'_>, program: &Path, args: Vec<String>) -> JobSpec {
    JobSpec {
        label: format!("research discovery: {}", program.display()),
        program: program.display().to_string(),
        args,
        environment: research_environment(BTreeMap::new()),
        working_directory: options.probe_dir.display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: Some(30_000),
        log_limit_bytes: Some(1024 * 1024),
        stdin: None,
    }
}

/// A project interpreter, probed as a trusted project job in the project root
/// (`-I`: the project directory is not put on `sys.path`).
fn probe_project_python(options: &DiscoveryOptions<'_>, path: &Path) -> Option<PythonProbe> {
    let scope = options.project.as_ref().filter(|p| p.trusted)?;
    let mut spec = probe_spec(options, path, vec!["-I".into(), "-c".into(), PYTHON_PROBE.into()]);
    spec.label = format!("research discovery (project interpreter): {}", path.display());
    spec.working_directory = crate::service::display_path(&scope.root);
    let output = options.launcher.run_project_probe(&spec).ok()?;
    let line = output.stdout.lines().find_map(|l| l.trim().strip_prefix(PROBE_MARKER))?;
    serde_json::from_str(line).ok()
}

fn probe_python(options: &DiscoveryOptions<'_>, path: &Path) -> Option<PythonProbe> {
    let output = options.launcher.run_probe(&probe_spec(options, path, vec!["-c".into(), PYTHON_PROBE.into()])).ok()?;
    let line = output.stdout.lines().find_map(|l| l.trim().strip_prefix(PROBE_MARKER))?;
    serde_json::from_str(line).ok()
}

/// What a runtime probe learned about R or Julia.
#[derive(Debug, Clone, Default)]
struct RuntimeProbe {
    version: Option<String>,
    kernel_package_installed: bool,
    kernel_package_version: Option<String>,
    /// Variables the kernel needs to find its libraries under the cleaned
    /// research environment (R user library, Julia depots).
    kernel_environment: BTreeMap<String, String>,
}

/// R: version, effective user library, IRkernel presence and version.
const R_PROBE: &str = "cat('EXPOSITION-PROBE', paste(R.version$major, R.version$minor, sep = '.'), \
     Sys.getenv('R_LIBS_USER'), requireNamespace('IRkernel', quietly = TRUE), \
     tryCatch(as.character(packageVersion('IRkernel')), error = function(e) ''), sep = '\\n')";

/// Julia: version, depot path, IJulia presence.
const JULIA_PROBE: &str = "println(\"EXPOSITION-PROBE\"); println(VERSION); \
     println(join(DEPOT_PATH, Sys.iswindows() ? ';' : ':')); println(Base.find_package(\"IJulia\") !== nothing)";

fn probe_runtime(options: &DiscoveryOptions<'_>, exe: &Path, language: RuntimeLanguage) -> Option<RuntimeProbe> {
    let args: Vec<String> = match language {
        RuntimeLanguage::R => vec!["--vanilla".into(), "-e".into(), R_PROBE.into()],
        _ => vec!["--startup-file=no".into(), "--history-file=no".into(), "-e".into(), JULIA_PROBE.into()],
    };
    let output = options.launcher.run_probe(&probe_spec(options, exe, args)).ok()?;
    let mut lines = output.stdout.lines().map(str::trim).skip_while(|l| *l != PROBE_MARKER).skip(1);
    let version: String = lines.next().unwrap_or("").chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut probe = RuntimeProbe { version: (!version.is_empty()).then_some(version), ..Default::default() };
    let path_value = lines.next().unwrap_or("").to_owned();
    probe.kernel_package_installed = matches!(lines.next(), Some("TRUE" | "true"));
    if language == RuntimeLanguage::R {
        probe.kernel_package_version = lines.next().filter(|v| !v.is_empty()).map(str::to_owned);
    }
    let variable = if language == RuntimeLanguage::R { "R_LIBS_USER" } else { "JULIA_DEPOT_PATH" };
    if !path_value.is_empty() && !looks_secret(variable) {
        probe.kernel_environment.insert(variable.to_owned(), path_value);
    }
    Some(probe)
}

fn version_key(version: &str) -> Vec<u32> {
    version.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn normalize(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = canonical.display().to_string();
    if cfg!(windows) { text.to_lowercase() } else { text }
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths.into_iter().filter(|p| p.is_file() && is_safe_candidate(p) && seen.insert(normalize(p))).collect()
}

/// Windows Store "app execution alias" stubs open the Store instead of running.
fn is_safe_candidate(path: &Path) -> bool {
    !path.components().any(|c| c.as_os_str().eq_ignore_ascii_case("WindowsApps"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn path_entries() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
}

/// Subdirectories of `dir` whose names start with `prefix`, newest name first.
fn matching_dirs(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.reverse();
    dirs
}

fn python_candidates(options: &DiscoveryOptions<'_>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        out.extend(py_launcher_interpreters(options));
        for dir in path_entries() {
            out.push(dir.join("python.exe"));
        }
        if let Some(local) = env_path("LOCALAPPDATA") {
            for dir in matching_dirs(&local.join("Python"), "pythoncore-") {
                out.push(dir.join("python.exe"));
            }
            for dir in matching_dirs(&local.join("Programs").join("Python"), "Python") {
                out.push(dir.join("python.exe"));
            }
        }
        for root in ["PROGRAMFILES", "PROGRAMDATA"] {
            if let Some(base) = env_path(root) {
                for dir in matching_dirs(&base, "Python") {
                    out.push(dir.join("python.exe"));
                }
                for name in ["anaconda3", "miniconda3", "miniforge3"] {
                    out.push(base.join(name).join("python.exe"));
                }
            }
        }
        if let Some(home) = env_path("USERPROFILE") {
            for name in ["anaconda3", "miniconda3", "miniforge3", "mambaforge"] {
                out.push(home.join(name).join("python.exe"));
            }
        }
    } else {
        for dir in path_entries() {
            out.push(dir.join("python3"));
            out.push(dir.join("python"));
        }
        for path in ["/usr/bin/python3", "/usr/local/bin/python3", "/opt/homebrew/bin/python3"] {
            out.push(PathBuf::from(path));
        }
        if let Some(home) = env_path("HOME") {
            for name in ["anaconda3", "miniconda3", "miniforge3"] {
                out.push(home.join(name).join("bin").join("python3"));
            }
        }
    }
    out
}

/// Interpreters registered with the Windows `py` launcher (`py -0p`).
fn py_launcher_interpreters(options: &DiscoveryOptions<'_>) -> Vec<PathBuf> {
    let mut launchers = Vec::new();
    if let Some(root) = env_path("SYSTEMROOT") {
        launchers.push(root.join("py.exe"));
    }
    if let Some(local) = env_path("LOCALAPPDATA") {
        launchers.push(local.join("Programs").join("Python").join("Launcher").join("py.exe"));
    }
    launchers.extend(path_entries().into_iter().map(|d| d.join("py.exe")));
    let Some(launcher) = launchers.into_iter().find(|p| p.is_file() && is_safe_candidate(p)) else {
        return Vec::new();
    };
    let Ok(output) = options.launcher.run_probe(&probe_spec(options, &launcher, vec!["-0p".into()])) else {
        return Vec::new();
    };
    output
        .stdout
        .lines()
        .filter_map(|line| {
            let bytes = line.as_bytes();
            let start = (0..bytes.len().saturating_sub(2))
                .find(|&i| bytes[i].is_ascii_alphabetic() && bytes[i + 1] == b':' && bytes[i + 2] == b'\\')?;
            Some(PathBuf::from(line[start..].trim()))
        })
        .collect()
}

fn r_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if cfg!(windows) {
        for dir in path_entries() {
            out.push(dir.join("Rscript.exe"));
        }
        for root in ["PROGRAMFILES", "LOCALAPPDATA"] {
            if let Some(base) = env_path(root) {
                let r_dir = if root == "LOCALAPPDATA" { base.join("Programs").join("R") } else { base.join("R") };
                for dir in matching_dirs(&r_dir, "R-") {
                    out.push(dir.join("bin").join("Rscript.exe"));
                    out.push(dir.join("bin").join("x64").join("Rscript.exe"));
                }
            }
        }
    } else {
        for dir in path_entries() {
            out.push(dir.join("Rscript"));
        }
        out.push(PathBuf::from("/Library/Frameworks/R.framework/Resources/bin/Rscript"));
    }
    out.into_iter().filter(|p| is_safe_candidate(p)).collect()
}

fn julia_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let exe = if cfg!(windows) { "julia.exe" } else { "julia" };
    for dir in path_entries() {
        out.push(dir.join(exe));
    }
    if cfg!(windows) {
        if let Some(local) = env_path("LOCALAPPDATA") {
            for dir in matching_dirs(&local.join("Programs"), "Julia") {
                out.push(dir.join("bin").join(exe));
            }
        }
        if let Some(home) = env_path("USERPROFILE") {
            out.push(home.join(".juliaup").join("bin").join(exe));
        }
    } else if let Some(home) = env_path("HOME") {
        out.push(home.join(".juliaup").join("bin").join(exe));
    }
    out.into_iter().filter(|p| is_safe_candidate(p)).collect()
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_aliases_are_never_probed() {
        assert!(!is_safe_candidate(Path::new(r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\python.exe")));
        assert!(is_safe_candidate(Path::new(r"C:\Python314\python.exe")));
    }

    #[test]
    fn versions_sort_numerically() {
        assert!(version_key("3.14.5") > version_key("3.9.20"));
    }

    /// Every "unavailable" guidance names what to install or register and says
    /// that Exposition does not do it itself, whichever case the host is in.
    #[test]
    fn every_setup_guidance_names_the_fix_and_that_exposition_never_does_it() {
        let cases = [
            (PYTHON_MISSING_GUIDANCE, "ipykernel"),
            (R_MISSING_GUIDANCE, "IRkernel"),
            (IRKERNEL_SPEC_GUIDANCE, "IRkernel::installspec"),
            (IRKERNEL_MISSING_GUIDANCE, "IRkernel"),
            (JULIA_MISSING_GUIDANCE, "IJulia"),
            (IJULIA_SPEC_GUIDANCE, "installkernel"),
            (IJULIA_MISSING_GUIDANCE, "IJulia"),
            (HOST_MISSING_GUIDANCE, "jupyter_client"),
        ];
        for (guidance, fix) in cases {
            assert!(guidance.contains(fix), "{guidance}");
            assert!(
                guidance.contains("Exposition never installs") || guidance.contains("Exposition never registers"),
                "{guidance}"
            );
        }
    }
}
