//! Process launching seam for the kernel host helper and discovery probes.
//!
//! Research never spawns processes directly: everything goes through a
//! [`KernelProcessLauncher`] given an [`exposition_core::job::JobSpec`]
//! (executable + argument array, deliberately constructed environment,
//! validated working directory, execution policy).
//!
//! TEMPORARY: [`StdProcessLauncher`] is a stand-in until the shared
//! ExecutionService (m1/execution) offers long-lived interactive jobs (stdin kept
//! open, ordered stdout stream, job-object process-tree cleanup). Replace it with
//! an adapter over that service; the trait is the only thing research depends on.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobSpec};

/// Host variables a research child may inherit. Nothing else is copied; in
/// particular no provider credentials or tokens.
#[cfg(windows)]
pub const INHERITED_VARIABLES: &[&str] = &[
    "SYSTEMROOT",
    "WINDIR",
    "SYSTEMDRIVE",
    "COMSPEC",
    "PATH",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "OS",
    "USERNAME",
    "COMPUTERNAME",
    "LANG",
    "LC_ALL",
    "TZ",
];

#[cfg(not(windows))]
pub const INHERITED_VARIABLES: &[&str] =
    &["PATH", "HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "XDG_RUNTIME_DIR"];

/// Never copied into a child even if a caller lists them.
pub fn looks_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "SESSION_COOKIE"]
        .iter()
        .any(|marker| upper.contains(marker))
}

/// The environment research children receive.
pub fn research_environment(extra: BTreeMap<String, String>) -> EnvironmentSpec {
    let mut set = BTreeMap::new();
    set.insert("PYTHONUTF8".to_owned(), "1".to_owned());
    set.insert("PYTHONIOENCODING".to_owned(), "utf-8".to_owned());
    set.insert("PYTHONUNBUFFERED".to_owned(), "1".to_owned());
    set.insert("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned());
    set.insert("PYDEVD_DISABLE_FILE_VALIDATION".to_owned(), "1".to_owned());
    set.extend(extra);
    EnvironmentSpec { inherit: INHERITED_VARIABLES.iter().map(|s| (*s).to_owned()).collect(), set }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    InvalidSpec(String),
    #[error("could not start {program}: {message}")]
    Spawn { program: String, message: String },
    #[error("timed out after {0} ms")]
    TimedOut(u64),
}

/// How a process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
}

/// Control over a launched process tree.
pub trait ProcessControl: Send + Sync {
    /// Terminate the process and its descendants.
    fn kill_tree(&self);
    /// Exit status, if the process has exited.
    fn try_wait(&self) -> Option<ExitInfo>;
}

/// A long-lived child with open stdin and line-oriented stdout.
pub struct InteractiveProcess {
    pub pid: Option<u32>,
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
    pub stderr: Box<dyn Read + Send>,
    pub control: Arc<dyn ProcessControl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutput {
    pub exit: ExitInfo,
    pub stdout: String,
    pub stderr: String,
}

pub trait KernelProcessLauncher: Send + Sync {
    /// Launch a long-lived interactive process.
    fn launch_interactive(&self, spec: &JobSpec) -> Result<InteractiveProcess, LaunchError>;
    /// Run a short discovery probe to completion (bounded by `spec.timeout_ms`).
    fn run_probe(&self, spec: &JobSpec) -> Result<ProbeOutput, LaunchError>;
}

/// TEMPORARY launcher over `std::process` (see module docs).
#[derive(Debug, Default)]
pub struct StdProcessLauncher;

const DEFAULT_PROBE_TIMEOUT_MS: u64 = 20_000;
const PROBE_OUTPUT_LIMIT: usize = 1024 * 1024;

impl StdProcessLauncher {
    fn command(spec: &JobSpec) -> Result<Command, LaunchError> {
        match &spec.policy {
            ExecutionPolicy::TrustedNative => {}
            ExecutionPolicy::Restricted { backend } => {
                return Err(LaunchError::Refused(format!(
                    "restricted execution backend {backend:?} is not available for research kernels; \
                     it is never replaced by native execution"
                )));
            }
        }
        let program = Path::new(&spec.program);
        if !program.is_absolute() {
            return Err(LaunchError::InvalidSpec(format!("program must be an absolute path: {}", spec.program)));
        }
        let cwd = Path::new(&spec.working_directory);
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(LaunchError::InvalidSpec("working directory must be an existing absolute directory".into()));
        }
        let mut command = Command::new(program);
        command.args(&spec.args).current_dir(cwd).env_clear();
        for name in &spec.environment.inherit {
            if looks_secret(name) {
                continue;
            }
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for (name, value) in &spec.environment.set {
            command.env(name, value);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        Ok(command)
    }
}

impl KernelProcessLauncher for StdProcessLauncher {
    fn launch_interactive(&self, spec: &JobSpec) -> Result<InteractiveProcess, LaunchError> {
        let mut command = Self::command(spec)?;
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| LaunchError::Spawn { program: spec.program.clone(), message: e.to_string() })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let pid = Some(child.id());
        Ok(InteractiveProcess {
            pid,
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
            control: Arc::new(StdControl { child: Mutex::new(child) }),
        })
    }

    fn run_probe(&self, spec: &JobSpec) -> Result<ProbeOutput, LaunchError> {
        let mut command = Self::command(spec)?;
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| LaunchError::Spawn { program: spec.program.clone(), message: e.to_string() })?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let out_reader = std::thread::spawn(move || read_bounded(stdout));
        let err_reader = std::thread::spawn(move || read_bounded(stderr));
        let timeout = Duration::from_millis(spec.timeout_ms.unwrap_or(DEFAULT_PROBE_TIMEOUT_MS));
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    kill_tree(&mut child);
                    let _ = child.wait();
                    return Err(LaunchError::TimedOut(timeout.as_millis() as u64));
                }
            }
        };
        Ok(ProbeOutput {
            exit: ExitInfo { code: status.code() },
            stdout: out_reader.join().unwrap_or_default(),
            stderr: err_reader.join().unwrap_or_default(),
        })
    }
}

fn read_bounded(mut reader: impl Read) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buffer.len() < PROBE_OUTPUT_LIMIT {
                    buffer.extend_from_slice(&chunk[..n.min(PROBE_OUTPUT_LIMIT - buffer.len())]);
                }
            }
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

struct StdControl {
    child: Mutex<Child>,
}

impl ProcessControl for StdControl {
    fn kill_tree(&self) {
        let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
        kill_tree(&mut child);
        let _ = child.wait();
    }

    fn try_wait(&self) -> Option<ExitInfo> {
        let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
        child.try_wait().ok().flatten().map(|status| ExitInfo { code: status.code() })
    }
}

fn kill_tree(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(windows)]
    {
        // Without job objects (owned by the ExecutionService), terminate the
        // tree with the system taskkill; the kernel also exits on its own when
        // its parent helper disappears.
        if let Some(root) = std::env::var_os("SYSTEMROOT") {
            let taskkill = Path::new(&root).join("System32").join("taskkill.exe");
            let mut command = Command::new(taskkill);
            command.args(["/PID", &child.id().to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null());
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
            let _ = command.status();
        }
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_never_inherited() {
        for name in ["ANTHROPIC_API_KEY", "GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY", "DB_PASSWORD", "MY_AUTH"] {
            assert!(looks_secret(name), "{name}");
        }
        for name in INHERITED_VARIABLES {
            assert!(!looks_secret(name), "{name}");
        }
        let env = research_environment(BTreeMap::new());
        assert!(env.inherit.iter().all(|name| !looks_secret(name)));
        assert_eq!(env.set.get("PYTHONUTF8").map(String::as_str), Some("1"));
    }

    #[test]
    fn restricted_policy_is_refused_not_downgraded() {
        let spec = JobSpec {
            label: "probe".into(),
            program: std::env::current_exe().unwrap().display().to_string(),
            args: vec![],
            environment: EnvironmentSpec::default(),
            working_directory: std::env::temp_dir().display().to_string(),
            policy: ExecutionPolicy::Restricted { backend: "container".into() },
            timeout_ms: None,
            log_limit_bytes: None,
            stdin: None,
        };
        assert!(matches!(StdProcessLauncher.run_probe(&spec), Err(LaunchError::Refused(_))));
        let relative = JobSpec { program: "python".into(), policy: ExecutionPolicy::TrustedNative, ..spec };
        assert!(matches!(StdProcessLauncher.run_probe(&relative), Err(LaunchError::InvalidSpec(_))));
    }
}
