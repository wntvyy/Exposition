//! Process launching through the shared [`JobSpec`] contract.
//!
//! The TeX adapter never builds shell strings: every tool launch is a
//! [`JobSpec`] (absolute program + argument array + deliberately constructed
//! environment + validated working directory) handed to a [`JobRunner`]. In the
//! application the runner is the shared execution service (branch
//! `m1/execution`), which owns supervision, cancellation, log bounds, and
//! process-tree cleanup.
//!
//! [`LocalProcessRunner`] is a **temporary** minimal runner for this crate's
//! tests and for use until the execution service is integrated. It enforces the
//! same input rules (absolute program, absolute working directory inside an
//! allowed root, cleared environment with secret-looking variables never
//! inherited, no shell, `TrustedNative` policy only) but it is not a sandbox.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use exposition_core::job::{ExecutionPolicy, JobOutcome, JobSpec};

/// Captured result of one process run.
#[derive(Clone, Debug)]
pub struct ProcessOutput {
    pub outcome: JobOutcome,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub output_truncated: bool,
}

impl ProcessOutput {
    pub fn failed_to_start(reason: impl Into<String>) -> Self {
        Self {
            outcome: JobOutcome::FailedToStart { reason: reason.into() },
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
            output_truncated: false,
        }
    }

    pub fn exit_code(&self) -> Option<i32> {
        match self.outcome {
            JobOutcome::Exited { code } => Some(code),
            _ => None,
        }
    }
}

/// Runs one job to completion. Implemented by the shared execution service in
/// the application and by [`LocalProcessRunner`] in tests.
pub trait JobRunner: Send + Sync {
    fn run(&self, spec: &JobSpec) -> ProcessOutput;
}

/// Environment variable names that are never inherited into a child, even if
/// a spec asks for them (provider credentials and other secrets).
pub fn is_secret_variable(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    const MARKERS: &[&str] =
        &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "SESSION", "COOKIE", "PRIVATE"];
    const PREFIXES: &[&str] = &["ANTHROPIC", "OPENAI", "AWS_", "AZURE_", "GCP_", "GOOGLE_", "GITHUB_", "GH_", "NPM_"];
    MARKERS.iter().any(|m| upper.contains(m)) || PREFIXES.iter().any(|p| upper.starts_with(p))
}

/// Temporary process runner (see module docs). Replace with the shared
/// execution service once `m1/execution` is integrated.
pub struct LocalProcessRunner {
    allowed_roots: Vec<PathBuf>,
    default_timeout: Duration,
    default_log_limit: usize,
}

impl LocalProcessRunner {
    /// `allowed_roots`: directories a job's working directory must lie in
    /// (application-owned build directories, open workspace roots).
    pub fn new(allowed_roots: Vec<PathBuf>) -> Self {
        let allowed_roots = allowed_roots.into_iter().map(|r| r.canonicalize().unwrap_or(r)).collect();
        Self { allowed_roots, default_timeout: Duration::from_secs(300), default_log_limit: 4 << 20 }
    }

    fn validate(&self, spec: &JobSpec) -> Result<PathBuf, String> {
        if !matches!(spec.policy, ExecutionPolicy::TrustedNative) {
            // Never downgrade restricted execution to native execution.
            return Err("restricted execution is not available in the local test runner".into());
        }
        let program = Path::new(&spec.program);
        if !program.is_absolute() || !program.is_file() {
            return Err(format!("program must be an absolute path to an existing file: {}", spec.program));
        }
        let cwd = Path::new(&spec.working_directory);
        if !cwd.is_absolute() {
            return Err("working directory must be absolute".into());
        }
        let cwd = cwd.canonicalize().map_err(|e| format!("working directory is not accessible: {e}"))?;
        if !cwd.is_dir() {
            return Err("working directory is not a directory".into());
        }
        if !self.allowed_roots.iter().any(|root| cwd.starts_with(root)) {
            return Err(format!("working directory {} is outside the allowed roots", cwd.display()));
        }
        Ok(cwd)
    }
}

impl JobRunner for LocalProcessRunner {
    fn run(&self, spec: &JobSpec) -> ProcessOutput {
        let cwd = match self.validate(spec) {
            Ok(cwd) => cwd,
            Err(reason) => return ProcessOutput::failed_to_start(reason),
        };
        let mut command = Command::new(&spec.program);
        command.args(&spec.args).current_dir(&cwd).env_clear();
        for name in &spec.environment.inherit {
            if is_secret_variable(name) {
                continue;
            }
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for (name, value) in &spec.environment.set {
            command.env(name, value);
        }
        command.stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let started = Instant::now();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => return ProcessOutput::failed_to_start(format!("could not start {}: {e}", spec.program)),
        };
        if let (Some(input), Some(mut stdin)) = (spec.stdin.clone(), child.stdin.take()) {
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = stdin.write_all(input.as_bytes());
            });
        }
        let limit = spec.log_limit_bytes.map(|l| l as usize).unwrap_or(self.default_log_limit);
        let stdout = child.stdout.take().map(|s| spawn_reader(s, limit));
        let stderr = child.stderr.take().map(|s| spawn_reader(s, limit));
        let timeout = spec.timeout_ms.map(Duration::from_millis).unwrap_or(self.default_timeout);

        let outcome = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    break match status.code() {
                        Some(code) => JobOutcome::Exited { code },
                        None => JobOutcome::Terminated { description: status.to_string() },
                    };
                }
                Ok(None) if started.elapsed() > timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break JobOutcome::TimedOut;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                Err(e) => break JobOutcome::Terminated { description: e.to_string() },
            }
        };
        let (stdout, out_truncated) = stdout.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
        let (stderr, err_truncated) = stderr.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
        ProcessOutput {
            outcome,
            stdout,
            stderr,
            duration_ms: started.elapsed().as_millis() as u64,
            output_truncated: out_truncated || err_truncated,
        }
    }
}

fn spawn_reader(mut stream: impl Read + Send + 'static, limit: usize) -> std::thread::JoinHandle<(String, bool)> {
    std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut truncated = false;
        let mut buf = [0u8; 8192];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 {
                break;
            }
            kept.extend_from_slice(&buf[..n]);
            if kept.len() > limit {
                // Keep the most recent output.
                let excess = kept.len() - limit;
                kept.drain(..excess);
                truncated = true;
            }
        }
        (String::from_utf8_lossy(&kept).into_owned(), truncated)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_variables_are_recognised() {
        for name in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY", "MY_PASSWORD"] {
            assert!(is_secret_variable(name), "{name}");
        }
        for name in ["PATH", "SystemRoot", "TEMP", "TEXINPUTS", "max_print_line"] {
            assert!(!is_secret_variable(name), "{name}");
        }
    }

    #[test]
    fn rejects_relative_programs_and_foreign_directories() {
        let dir = tempfile::tempdir().unwrap();
        let runner = LocalProcessRunner::new(vec![dir.path().to_path_buf()]);
        let mut spec = JobSpec {
            label: "x".into(),
            program: "pdflatex".into(),
            args: vec![],
            environment: Default::default(),
            working_directory: dir.path().display().to_string(),
            policy: ExecutionPolicy::TrustedNative,
            timeout_ms: None,
            log_limit_bytes: None,
            stdin: None,
        };
        assert!(matches!(runner.run(&spec).outcome, JobOutcome::FailedToStart { .. }));
        spec.program = std::env::current_exe().unwrap().display().to_string();
        spec.working_directory = std::env::temp_dir().display().to_string();
        assert!(matches!(runner.run(&spec).outcome, JobOutcome::FailedToStart { .. }));
        spec.working_directory = dir.path().display().to_string();
        spec.policy = ExecutionPolicy::Restricted { backend: "container".into() };
        assert!(matches!(runner.run(&spec).outcome, JobOutcome::FailedToStart { .. }));
    }
}
