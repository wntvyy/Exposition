//! Child environment construction.
//!
//! A child environment starts EMPTY. The service adds:
//!
//! 1. A minimal platform base. On Windows: `SystemRoot`, `windir`,
//!    `SystemDrive`, `ComSpec`, `PATHEXT`, processor/OS facts, and the profile
//!    and application-data locations (`USERPROFILE`, `HOMEDRIVE`, `HOMEPATH`,
//!    `APPDATA`, `LOCALAPPDATA`, `ProgramData`, `ProgramFiles*`,
//!    `CommonProgramFiles*`, `PUBLIC`, `ALLUSERSPROFILE`, `USERNAME`,
//!    `USERDOMAIN`, `COMPUTERNAME`). These are required in practice: Windows
//!    crypto/RNG needs `SystemRoot`; kpathsea/LuaTeX derive `TEXMFHOME`/
//!    `TEXMFVAR` and the luaotfload font cache from the profile; Python finds
//!    user site-packages under `APPDATA`; Jupyter keeps kernel specs and runtime
//!    connection files under `APPDATA`. None of them are credentials. On Unix:
//!    `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`.
//! 2. `TEMP`/`TMP` (Unix: `TMPDIR`) pointing at a per-job temporary directory.
//! 3. A constructed `PATH`: the program's own directory, discovered toolchain
//!    directories, then the system directories. The host `PATH` is NOT used.
//! 4. The job's `inherit` allowlist, copied from the host when present.
//! 5. The job's `set` map.
//!
//! Credential-like names ([`is_credential_name`]) can never be inherited or set,
//! and values that look like secrets are refused: the job fails to start with a
//! reason instead of running with a silently altered environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use exposition_core::job::EnvironmentSpec;

use crate::credentials::{Redactor, is_credential_name};

/// A snapshot of the application's own environment.
#[derive(Clone, Debug, Default)]
pub struct HostEnv {
    vars: Vec<(String, String)>,
}

impl HostEnv {
    /// Capture the current process environment (non-UTF-8 entries are skipped).
    pub fn capture() -> Self {
        let vars =
            std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect();
        Self { vars }
    }

    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self { vars: pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect() }
    }

    /// Look up a variable (case-insensitively on Windows).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.iter().find(|(k, _)| names_equal(k, name)).map(|(_, v)| v.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

fn names_equal(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EnvError {
    #[error("environment variable {0} looks like a credential and can never be passed to a child process")]
    CredentialName(String),
    #[error("environment variable {0} has a value that looks like a secret token and was refused")]
    SecretValue(String),
    #[error("invalid environment variable name {0:?}")]
    InvalidName(String),
    #[error("environment variable {0} has an invalid value (NUL byte, or a newline where unsupported)")]
    InvalidValue(String),
}

/// Ordered environment with platform-appropriate name case handling.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    /// Keyed by normalized name (upper-case on Windows); value = (name, value).
    entries: BTreeMap<String, (String, String)>,
}

impl Environment {
    fn key(name: &str) -> String {
        if cfg!(windows) { name.to_ascii_uppercase() } else { name.to_owned() }
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        self.entries.insert(Self::key(&name), (name, value.into()));
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries.get(&Self::key(name)).map(|(_, v)| v.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(&Self::key(name))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.values().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(windows)]
const BASE_VARS: &[&str] = &[
    "SystemRoot",
    "windir",
    "SystemDrive",
    "ComSpec",
    "PATHEXT",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROCESSOR_LEVEL",
    "PROCESSOR_REVISION",
    "NUMBER_OF_PROCESSORS",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "PUBLIC",
    "ALLUSERSPROFILE",
    "USERNAME",
    "USERDOMAIN",
    "COMPUTERNAME",
];

#[cfg(not(windows))]
const BASE_VARS: &[&str] = &["HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TZ"];

/// System directories appended to every constructed `PATH`.
pub fn system_path_dirs(host: &HostEnv) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let root = PathBuf::from(host.get("SystemRoot").unwrap_or(r"C:\Windows"));
        vec![root.join("System32"), root.clone(), root.join("System32").join("Wbem")]
    }
    #[cfg(not(windows))]
    {
        let _ = host;
        ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"].iter().map(PathBuf::from).collect()
    }
}

/// Inputs for [`build_environment`].
pub struct EnvContext<'a> {
    pub host: &'a HostEnv,
    pub redactor: &'a Redactor,
    /// Per-job temporary directory (`TEMP`/`TMP`/`TMPDIR`).
    pub temp_dir: &'a Path,
    /// Directories placed before the system directories in `PATH`, in order.
    pub path_dirs: &'a [PathBuf],
}

const PATH_SEPARATOR: char = if cfg!(windows) { ';' } else { ':' };

/// Build a native child environment from empty. See the module docs.
pub fn build_environment(spec: &EnvironmentSpec, ctx: &EnvContext<'_>) -> Result<Environment, EnvError> {
    let mut env = Environment::default();
    for name in BASE_VARS {
        if let Some(value) = ctx.host.get(name) {
            env.insert(*name, value);
        }
    }
    #[cfg(windows)]
    {
        if !env.contains("SystemRoot") {
            env.insert("SystemRoot", r"C:\Windows");
        }
        let temp = ctx.temp_dir.display().to_string();
        env.insert("TEMP", temp.clone());
        env.insert("TMP", temp);
    }
    #[cfg(not(windows))]
    env.insert("TMPDIR", ctx.temp_dir.display().to_string());

    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in ctx.path_dirs.iter().cloned().chain(system_path_dirs(ctx.host)) {
        if !dirs.iter().any(|d| crate::paths::same_path(d, &dir)) {
            dirs.push(dir);
        }
    }
    let path = dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(&PATH_SEPARATOR.to_string());
    env.insert("PATH", path);

    apply_spec(&mut env, spec, ctx.host, ctx.redactor, false)?;
    Ok(env)
}

/// Build the environment for a container (Restricted/docker) job. Nothing from
/// the host platform base is included; values may not contain newlines because
/// they are passed through a Docker env-file.
pub fn build_container_environment(
    spec: &EnvironmentSpec,
    host: &HostEnv,
    redactor: &Redactor,
) -> Result<Environment, EnvError> {
    let mut env = Environment::default();
    env.insert("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
    env.insert("HOME", "/tmp");
    env.insert("TMPDIR", "/tmp");
    env.insert("LANG", "C.UTF-8");
    apply_spec(&mut env, spec, host, redactor, true)?;
    Ok(env)
}

fn validate_name(name: &str) -> Result<(), EnvError> {
    if name.is_empty() || name.contains('=') || name.contains('\0') || name.chars().any(char::is_control) {
        return Err(EnvError::InvalidName(name.to_owned()));
    }
    if is_credential_name(name) {
        return Err(EnvError::CredentialName(name.to_owned()));
    }
    Ok(())
}

fn validate_value(name: &str, value: &str, redactor: &Redactor, single_line: bool) -> Result<(), EnvError> {
    if value.contains('\0') || (single_line && (value.contains('\n') || value.contains('\r'))) {
        return Err(EnvError::InvalidValue(name.to_owned()));
    }
    if redactor.contains_secret(value) {
        return Err(EnvError::SecretValue(name.to_owned()));
    }
    Ok(())
}

fn apply_spec(
    env: &mut Environment,
    spec: &EnvironmentSpec,
    host: &HostEnv,
    redactor: &Redactor,
    single_line: bool,
) -> Result<(), EnvError> {
    for name in &spec.inherit {
        validate_name(name)?;
        if let Some(value) = host.get(name) {
            validate_value(name, value, redactor, single_line)?;
            env.insert(name.clone(), value);
        }
    }
    for (name, value) in &spec.set {
        validate_name(name)?;
        validate_value(name, value, redactor, single_line)?;
        env.insert(name.clone(), value.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> HostEnv {
        HostEnv::from_pairs([
            ("SystemRoot", r"C:\Windows"),
            ("USERPROFILE", r"C:\Users\someone"),
            ("APPDATA", r"C:\Users\someone\AppData\Roaming"),
            ("HOME", "/home/someone"),
            ("PATH", r"C:\host\only\bin"),
            ("ANTHROPIC_API_KEY", "fake-anthropic-key-for-tests"),
            ("GITHUB_TOKEN", "ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
            ("MPLBACKEND", "Agg"),
            ("LEAKY", "prefix sk-ant-api03-abcdefghijklmnop"),
            ("RANDOM_HOST_VAR", "should-not-appear"),
        ])
    }

    fn build(spec: &EnvironmentSpec) -> Result<Environment, EnvError> {
        let host = host();
        let redactor = Redactor::from_host_env(host.iter());
        let temp = PathBuf::from(if cfg!(windows) { r"C:\data\jobs\j1\tmp" } else { "/data/jobs/j1/tmp" });
        let tool = PathBuf::from(if cfg!(windows) { r"C:\tools\tex\bin" } else { "/opt/tex/bin" });
        build_environment(spec, &EnvContext { host: &host, redactor: &redactor, temp_dir: &temp, path_dirs: &[tool] })
    }

    #[test]
    fn starts_from_empty_with_platform_base() {
        let env = build(&EnvironmentSpec::default()).unwrap();
        assert!(env.get("ANTHROPIC_API_KEY").is_none());
        assert!(env.get("GITHUB_TOKEN").is_none());
        assert!(env.get("RANDOM_HOST_VAR").is_none());
        assert!(env.get("MPLBACKEND").is_none(), "nothing is inherited implicitly");
        let path = env.get("PATH").unwrap();
        assert!(!path.contains("host"), "host PATH must not leak: {path}");
        assert!(path.contains("tex"), "toolchain dirs come first: {path}");
        if cfg!(windows) {
            assert_eq!(env.get("SystemRoot"), Some(r"C:\Windows"));
            assert_eq!(env.get("USERPROFILE"), Some(r"C:\Users\someone"));
            assert_eq!(env.get("TEMP"), Some(r"C:\data\jobs\j1\tmp"));
            assert_eq!(env.get("tmp"), Some(r"C:\data\jobs\j1\tmp"), "names are case-insensitive on Windows");
            assert!(path.ends_with(r"C:\Windows\System32\Wbem"), "{path}");
        } else {
            assert_eq!(env.get("HOME"), Some("/home/someone"));
            assert_eq!(env.get("TMPDIR"), Some("/data/jobs/j1/tmp"));
        }
    }

    #[test]
    fn inherit_and_set_are_applied_in_order() {
        let spec = EnvironmentSpec {
            inherit: vec!["MPLBACKEND".into(), "NOT_PRESENT".into()],
            set: [("MPLBACKEND".to_string(), "pdf".to_string()), ("SOURCE_DATE_EPOCH".into(), "0".into())].into(),
        };
        let env = build(&spec).unwrap();
        assert_eq!(env.get("MPLBACKEND"), Some("pdf"), "set overrides inherit");
        assert_eq!(env.get("SOURCE_DATE_EPOCH"), Some("0"));
        assert!(env.get("NOT_PRESENT").is_none());
    }

    #[test]
    fn credential_names_are_refused_for_inherit_and_set() {
        for name in ["ANTHROPIC_API_KEY", "anthropic_api_key", "GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY", "DB_PASSWORD"] {
            let inherit = EnvironmentSpec { inherit: vec![name.into()], set: Default::default() };
            assert_eq!(build(&inherit), Err(EnvError::CredentialName(name.into())));
            let set = EnvironmentSpec { inherit: vec![], set: [(name.to_string(), "x".to_string())].into() };
            assert_eq!(build(&set), Err(EnvError::CredentialName(name.into())));
        }
    }

    #[test]
    fn secret_values_and_invalid_names_are_refused() {
        let leaky = EnvironmentSpec { inherit: vec!["LEAKY".into()], set: Default::default() };
        assert_eq!(build(&leaky), Err(EnvError::SecretValue("LEAKY".into())));
        let set = EnvironmentSpec {
            inherit: vec![],
            set: [("HARMLESS_NAME".to_string(), "fake-anthropic-key-for-tests".to_string())].into(),
        };
        assert_eq!(build(&set), Err(EnvError::SecretValue("HARMLESS_NAME".into())));
        for bad in ["", "A=B", "NUL\0NAME"] {
            let spec = EnvironmentSpec { inherit: vec![bad.into()], set: Default::default() };
            assert_eq!(build(&spec), Err(EnvError::InvalidName(bad.into())));
        }
        let nul = EnvironmentSpec { inherit: vec![], set: [("X".to_string(), "a\0b".to_string())].into() };
        assert_eq!(build(&nul), Err(EnvError::InvalidValue("X".into())));
    }

    #[test]
    fn container_environment_has_no_host_base() {
        let host = host();
        let redactor = Redactor::from_host_env(host.iter());
        let spec = EnvironmentSpec { inherit: vec!["MPLBACKEND".into()], set: Default::default() };
        let env = build_container_environment(&spec, &host, &redactor).unwrap();
        assert_eq!(env.get("HOME"), Some("/tmp"));
        assert_eq!(env.get("MPLBACKEND"), Some("Agg"));
        assert!(env.get("USERPROFILE").is_none() && env.get("SystemRoot").is_none());
        let multiline = EnvironmentSpec { inherit: vec![], set: [("X".to_string(), "a\nb".to_string())].into() };
        assert_eq!(build_container_environment(&multiline, &host, &redactor), Err(EnvError::InvalidValue("X".into())));
        let cred = EnvironmentSpec { inherit: vec!["ANTHROPIC_API_KEY".into()], set: Default::default() };
        assert!(matches!(build_container_environment(&cred, &host, &redactor), Err(EnvError::CredentialName(_))));
    }
}
