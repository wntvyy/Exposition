//! Restricted execution backend: Docker containers.
//!
//! A `Restricted { backend: "docker" }` job runs as
//!
//! ```text
//! docker run --rm --pull=never --network=none --read-only --cap-drop=ALL
//!   --security-opt=no-new-privileges --pids-limit=N --memory=M --memory-swap=M
//!   --cpus=C --user=<non-root uid:gid> --init --tmpfs=/tmp:...
//!   --mount=type=bind,source=<root>,target=/workspace,readonly
//!   --mount=type=bind,source=<job out dir>,target=/out
//!   --workdir=/workspace/<cwd relative to root> --env-file=<constructed env>
//!   --name=exposition-<job> --label=... --log-driver=none [-i]
//!   --entrypoint=<program> <image> <args...>
//! ```
//!
//! The image must be configured explicitly and present locally; the job is
//! refused otherwise. There is never a fallback to native execution. The
//! `docker` client itself runs natively (contained in a job object) with a
//! clean environment; cancel/timeout kills the client tree AND the container
//! (`docker kill`), and `--rm` removes it. Containers left behind by a crashed
//! session of the same data directory are removed before the next restricted
//! job.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::env::Environment;
use crate::settings::DockerSettings;

pub const BACKEND: &str = "docker";
pub const LABEL_MANAGED: &str = "org.exposition.managed";
pub const LABEL_JOB: &str = "org.exposition.job";
pub const LABEL_INSTANCE: &str = "org.exposition.instance";

/// Image references: registry/name:tag@digest characters only, never an option.
pub fn valid_image_ref(image: &str) -> bool {
    !image.is_empty()
        && image.len() <= 255
        && image.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && image.chars().all(|c| c.is_ascii_alphanumeric() || "_.-/:@".contains(c))
}

/// Container-side program: a bare name or absolute POSIX path, never an option.
pub fn valid_container_program(program: &str) -> bool {
    !program.is_empty() && !program.starts_with('-') && !program.contains('\0') && !program.contains('\n')
}

/// Quote one `--mount` CSV field when it contains a separator or quote.
fn csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

/// Container name for a job id.
pub fn container_name(job_id: &str) -> String {
    let safe: String = job_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    format!("exposition-{safe}")
}

pub struct RunPlan<'a> {
    pub job_id: &'a str,
    pub instance: &'a str,
    pub image: &'a str,
    pub settings: &'a DockerSettings,
    /// Host directory mounted read-only at `/workspace`.
    pub mount_root: &'a Path,
    /// Working directory relative to `mount_root` (components).
    pub workdir_components: Vec<String>,
    /// Host directory mounted read-write at `/out`.
    pub out_dir: &'a Path,
    pub env_file: &'a Path,
    pub program: &'a str,
    pub args: &'a [String],
    /// Keep stdin attached (`-i`).
    pub stdin: bool,
}

/// Arguments for the `docker` client.
pub fn run_args(plan: &RunPlan<'_>) -> Vec<String> {
    let s = plan.settings;
    let mut workdir = String::from("/workspace");
    for c in &plan.workdir_components {
        workdir.push('/');
        workdir.push_str(c);
    }
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        "--network=none".into(),
        "--read-only".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        format!("--pids-limit={}", s.pids_limit),
        format!("--memory={}", s.memory),
        format!("--memory-swap={}", s.memory),
        format!("--cpus={}", s.cpus),
        format!("--user={}", s.user),
        "--init".into(),
        "--hostname=sandbox".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=256m".into(),
        format!(
            "--mount=type=bind,{},target=/workspace,readonly",
            csv_field(&format!("source={}", plan.mount_root.display()))
        ),
        format!("--mount=type=bind,{},target=/out", csv_field(&format!("source={}", plan.out_dir.display()))),
        format!("--workdir={workdir}"),
        format!("--env-file={}", plan.env_file.display()),
        format!("--name={}", container_name(plan.job_id)),
        format!("--label={LABEL_MANAGED}=1"),
        format!("--label={LABEL_JOB}={}", plan.job_id),
        format!("--label={LABEL_INSTANCE}={}", plan.instance),
        "--log-driver=none".into(),
    ];
    if plan.stdin {
        args.push("-i".into());
    }
    args.push(format!("--entrypoint={}", plan.program));
    args.push(plan.image.to_owned());
    args.extend(plan.args.iter().cloned());
    args
}

/// Write the container environment as a Docker env-file.
pub fn write_env_file(path: &Path, env: &Environment) -> std::io::Result<()> {
    let mut text = String::new();
    for (name, value) in env.iter() {
        text.push_str(name);
        text.push('=');
        text.push_str(value);
        text.push('\n');
    }
    std::fs::write(path, text)
}

/// Output of a short helper invocation of the docker client.
pub struct HelperOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Run the docker client with fixed arguments, a clean environment, and a timeout.
pub fn helper(
    docker: &Path,
    args: &[&str],
    env: &Environment,
    cwd: &Path,
    timeout: Duration,
) -> Result<HelperOutput, String> {
    let mut command = Command::new(docker);
    command
        .args(args)
        .env_clear()
        .envs(env.iter())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, containment) =
        crate::process::spawn_contained(&mut command).map_err(|e| format!("could not run docker: {e}"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let mut err = String::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_string(&mut out);
        }
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_string(&mut err);
        }
        let status = child.wait().ok();
        let _ = tx.send((status, out, err));
    });
    match rx.recv_timeout(timeout) {
        Ok((status, stdout, stderr)) => Ok(HelperOutput { code: status.and_then(|s| s.code()), stdout, stderr }),
        Err(_) => {
            containment.kill_tree();
            Err(format!("docker {} did not finish within {}s", args.first().unwrap_or(&""), timeout.as_secs()))
        }
    }
}

/// Check that the engine is reachable and the image is present locally.
pub fn preflight(docker: &Path, image: &str, env: &Environment, cwd: &Path) -> Result<(), String> {
    let out = helper(docker, &["image", "inspect", "--format", "{{.Id}}", image], env, cwd, Duration::from_secs(20))?;
    if out.code == Some(0) {
        return Ok(());
    }
    let err = out.stderr.to_ascii_lowercase();
    if err.contains("no such image") || err.contains("not found") {
        Err(format!(
            "container image `{image}` is not present locally. Exposition never pulls images; run `docker pull {image}` yourself if you want it, or configure a local image."
        ))
    } else {
        Err(format!(
            "the Docker engine is not reachable ({}). Start Docker Desktop and retry.",
            out.stderr.lines().next().unwrap_or("no details").trim()
        ))
    }
}

/// Kill and remove a job's container (idempotent).
pub fn kill_container(docker: &Path, job_id: &str, env: &Environment, cwd: &Path) {
    let name = container_name(job_id);
    let _ = helper(docker, &["rm", "-f", &name], env, cwd, Duration::from_secs(20));
}

/// Remove containers of this data directory left by an earlier session.
pub fn remove_stale(docker: &Path, instance: &str, env: &Environment, cwd: &Path) {
    let filter = format!("label={LABEL_INSTANCE}={instance}");
    if let Ok(out) = helper(docker, &["ps", "-aq", "--filter", &filter], env, cwd, Duration::from_secs(20)) {
        for id in out.stdout.split_whitespace() {
            if id.chars().all(|c| c.is_ascii_hexdigit()) {
                let _ = helper(docker, &["rm", "-f", id], env, cwd, Duration::from_secs(20));
            }
        }
    }
}

/// Whether a container for the job currently exists (for tests/diagnostics).
pub fn container_exists(docker: &Path, job_id: &str, env: &Environment, cwd: &Path) -> bool {
    let filter = format!("name=^{}$", container_name(job_id));
    helper(docker, &["ps", "-aq", "--filter", &filter], env, cwd, Duration::from_secs(20))
        .map(|o| !o.stdout.trim().is_empty())
        .unwrap_or(false)
}

/// Stable short label identifying one application data directory.
pub fn instance_label(data_dir: &Path) -> String {
    // FNV-1a over the lower-cased path: stable across runs, not a secret.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data_dir.display().to_string().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub fn out_dir(job_dir: &Path) -> PathBuf {
    job_dir.join("out")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_and_program_validation() {
        assert!(valid_image_ref("node:26-trixie-slim"));
        assert!(valid_image_ref("ghcr.io/org/img@sha256:abc"));
        assert!(!valid_image_ref("--privileged"));
        assert!(!valid_image_ref("img; rm -rf /"));
        assert!(!valid_image_ref(""));
        assert!(valid_container_program("node"));
        assert!(valid_container_program("/usr/bin/python3"));
        assert!(!valid_container_program("--privileged"));
    }

    #[test]
    fn run_args_enforce_isolation() {
        let settings = DockerSettings::default();
        let args = run_args(&RunPlan {
            job_id: "job-1",
            instance: "abc",
            image: "node:26-trixie-slim",
            settings: &settings,
            mount_root: Path::new(r"C:\work\a,b"),
            workdir_components: vec!["src".into()],
            out_dir: Path::new(r"C:\data\jobs\job-1\out"),
            env_file: Path::new(r"C:\data\jobs\job-1\container.env"),
            program: "node",
            args: &["-e".into(), "1".into()],
            stdin: false,
        });
        for required in [
            "--rm",
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--user=65534:65534",
            "--workdir=/workspace/src",
            "--entrypoint=node",
        ] {
            assert!(args.iter().any(|a| a == required), "missing {required}: {args:?}");
        }
        assert!(args.iter().any(|a| a == r#"--mount=type=bind,"source=C:\work\a,b",target=/workspace,readonly"#));
        let image_at = args.iter().position(|a| a == "node:26-trixie-slim").unwrap();
        assert_eq!(&args[image_at + 1..], ["-e", "1"]);
        assert!(!args.iter().any(|a| a == "-i"));
    }
}
