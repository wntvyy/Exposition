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
//!   [--mount=type=bind,source=<job input dir>,target=/exposition-input,readonly]
//!   --workdir=/workspace/<cwd relative to root> --env-file=<constructed env>
//!   --name=exposition-<job> --label=... --log-driver=none -i
//!   --entrypoint=/bin/sh <image> -c <supervisor script> exposition-job <program> <args...>
//! ```
//!
//! The image must be configured explicitly and present locally; the job is
//! refused otherwise. There is never a fallback to native execution. The
//! `docker` client itself runs natively (contained in a job object) with a
//! clean environment; cancel/timeout kills the client tree AND removes the
//! container (`docker rm -f`, which stops it first).
//!
//! A container must not outlive the application. The supervisor script (POSIX
//! `sh`, see [`supervisor_script`]) runs the program with `exec`, so it is
//! the init process's only child, after starting two watchers:
//! - the job's stdin pipe is held open by the application for the job's whole
//!   life and is never written; the program reads its own input from a
//!   read-only file instead. When the application exits or is killed, the
//!   docker client dies with it (job object), Docker closes the container's
//!   stdin (`-i`, stdin-once), and the watcher kills the program, which stops
//!   and removes the container;
//! - when the job has a time limit, `sleep` + `kill` ends the program once the
//!   limit (plus a short grace) has passed, even if nothing outside is left
//!   to enforce it.
//!
//! Image requirement: `/bin/sh` (POSIX) and `sleep` (for the time limit). If
//! `sleep` is missing only the in-container time limit is lost.
//! Containers of this data directory left behind by an earlier session are
//! removed at application start ([`remove_stale`]) and before the first
//! restricted job.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::env::Environment;
use crate::settings::DockerSettings;

pub const BACKEND: &str = "docker";
pub const LABEL_MANAGED: &str = "org.exposition.managed";
pub const LABEL_JOB: &str = "org.exposition.job";
pub const LABEL_INSTANCE: &str = "org.exposition.instance";

/// Whether `image` is a valid reference: the full grammar of
/// [`validate_image_reference`], the one check used everywhere.
pub fn valid_image_ref(image: &str) -> bool {
    validate_image_reference(image).is_ok()
}

/// Longest repository name Docker accepts (`NameTotalLengthMax`).
const MAX_NAME_LEN: usize = 255;

static REFERENCE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    // The image reference grammar of github.com/distribution/reference, with
    // `\w` spelled out as ASCII (RE2's `\w` is ASCII; Rust's is Unicode):
    //   reference := name [ ":" tag ] [ "@" digest ]
    //   name      := [ domain [ ":" port ] "/" ] path-component ( "/" path-component )*
    let alphanumeric = "[a-z0-9]+";
    let separator = "(?:[._]|__|[-]+)";
    let path_component = format!("{alphanumeric}(?:{separator}{alphanumeric})*");
    let domain_component = "(?:[a-zA-Z0-9]|[a-zA-Z0-9][a-zA-Z0-9-]*[a-zA-Z0-9])";
    let host = format!(r"(?:{domain_component}(?:\.{domain_component})*|\[[a-fA-F0-9:]+\])");
    let domain = format!("{host}(?::[0-9]+)?");
    let name = format!("(?:{domain}/)?{path_component}(?:/{path_component})*");
    let tag = "[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}";
    let digest = "[A-Za-z][A-Za-z0-9]*(?:[-_+.][A-Za-z][A-Za-z0-9]*)*:[0-9a-fA-F]{32,}";
    regex::Regex::new(&format!("^({name})(?::{tag})?(?:@{digest})?$")).expect("reference grammar")
});

/// Check an image reference against Docker's reference grammar
/// (`[registry[:port]/]name[:tag][@digest]`, lowercase repository path).
/// Used to validate settings: the result is also always passed to the docker
/// client as one argument, never through a shell.
pub fn validate_image_reference(image: &str) -> Result<(), String> {
    if image.is_empty() {
        return Err("enter an image reference such as python:3.13-slim".into());
    }
    if image.len() > 512 {
        return Err("the image reference is too long".into());
    }
    let Some(captures) = REFERENCE.captures(image) else {
        return Err(format!(
            "`{image}` is not a valid Docker image reference ([registry/]name[:tag][@digest]; repository names are lowercase letters, digits, and . _ - separators)"
        ));
    };
    if captures.get(1).map_or(0, |m| m.len()) > MAX_NAME_LEN {
        return Err(format!("the repository name may be at most {MAX_NAME_LEN} characters"));
    }
    Ok(())
}

/// A locally present, tagged image (from `docker image ls`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DockerImage {
    /// `repository:tag`, usable as `execution.docker.image`.
    pub reference: String,
    /// Short image id.
    pub id: String,
    /// Size as reported by Docker (e.g. "178MB").
    pub size: String,
    /// When the image was created, as reported by Docker (e.g. "6 days ago").
    pub created: String,
}

/// Why local images could not be listed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DockerListError {
    /// No docker client (or a broken configured path).
    #[error("{0}")]
    NotFound(String),
    /// The client ran but the engine did not answer.
    #[error("{0}")]
    Unreachable(String),
    #[error("could not list Docker images: {0}")]
    Failed(String),
}

impl DockerListError {
    pub fn code(&self) -> &'static str {
        match self {
            DockerListError::NotFound(_) => "docker.notFound",
            DockerListError::Unreachable(_) => "docker.unreachable",
            DockerListError::Failed(_) => "docker.failed",
        }
    }
}

/// Arguments for listing local images, one JSON object per line.
pub const IMAGE_LIST_ARGS: &[&str] = &["image", "ls", "--format", "{{json .}}"];

/// Parse `docker image ls --format '{{json .}}'` output. Untagged (`<none>`)
/// images and references outside the grammar are skipped; the result is
/// sorted and free of duplicates.
pub fn parse_image_list(stdout: &str) -> Vec<DockerImage> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Line {
        #[serde(default)]
        repository: String,
        #[serde(default)]
        tag: String,
        #[serde(default, rename = "ID")]
        id: String,
        #[serde(default)]
        size: String,
        #[serde(default)]
        created_since: String,
    }
    let mut images: Vec<DockerImage> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Line>(line.trim()).ok())
        .filter(|l| !l.repository.is_empty() && l.repository != "<none>" && !l.tag.is_empty() && l.tag != "<none>")
        .map(|l| DockerImage {
            reference: format!("{}:{}", l.repository, l.tag),
            id: l.id.trim_start_matches("sha256:").chars().take(12).collect(),
            size: l.size,
            created: l.created_since,
        })
        .filter(|image| validate_image_reference(&image.reference).is_ok())
        .collect();
    images.sort_by(|a, b| a.reference.cmp(&b.reference));
    images.dedup_by(|a, b| a.reference == b.reference);
    images
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

/// How the container's stdin is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerStdin<'a> {
    /// Batch job: stdin is the liveness pipe (held open by the application,
    /// never written). The program's input is the file `stdin` in this host
    /// directory (mounted read-only), or empty.
    Batch { input_dir: Option<&'a Path> },
    /// Interactive job: stdin is the program's own input stream.
    Interactive,
}

/// Where a batch job's input directory is mounted.
pub const INPUT_MOUNT: &str = "/exposition-input";
/// Name of the input file inside the input directory.
pub const INPUT_FILE: &str = "stdin";
/// Grace added to a job's time limit inside the container, so the
/// application's own timeout normally ends the job first.
pub const TIME_LIMIT_GRACE: Duration = Duration::from_secs(5);

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
    pub stdin: ContainerStdin<'a>,
    /// The job's time limit, enforced inside the container as well.
    pub time_limit: Option<Duration>,
}

/// The container's entrypoint script (POSIX `sh -c`, program and arguments
/// in `"$@"`): ties the container's lifetime to the application and to the
/// job's time limit, then `exec`s the program.
pub fn supervisor_script(stdin: ContainerStdin<'_>, time_limit: Option<Duration>) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let ContainerStdin::Batch { .. } = stdin {
        // EOF on the liveness pipe: the application (or its docker client) is gone.
        lines.push("exec 3<&0".into());
        lines.push("(while IFS= read -r _ <&3; do :; done; kill -9 $$) >/dev/null 2>&1 &".into());
    }
    if let Some(limit) = time_limit {
        let seconds = (limit + TIME_LIMIT_GRACE).as_secs_f64().ceil() as u64;
        // `&&`: a missing `sleep` must not kill the job at once.
        lines.push(format!("(sleep {seconds} && kill -9 $$) </dev/null >/dev/null 2>&1 &"));
    }
    lines.push(match stdin {
        ContainerStdin::Batch { input_dir: Some(_) } => format!("exec \"$@\" <{INPUT_MOUNT}/{INPUT_FILE} 3<&-"),
        ContainerStdin::Batch { input_dir: None } => "exec \"$@\" </dev/null 3<&-".to_owned(),
        ContainerStdin::Interactive => "exec \"$@\"".to_owned(),
    });
    lines.join("\n")
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
    ];
    if let ContainerStdin::Batch { input_dir: Some(dir) } = plan.stdin {
        args.push(format!(
            "--mount=type=bind,{},target={INPUT_MOUNT},readonly",
            csv_field(&format!("source={}", dir.display()))
        ));
    }
    args.extend([
        format!("--workdir={workdir}"),
        format!("--env-file={}", plan.env_file.display()),
        format!("--name={}", container_name(plan.job_id)),
        format!("--label={LABEL_MANAGED}=1"),
        format!("--label={LABEL_JOB}={}", plan.job_id),
        format!("--label={LABEL_INSTANCE}={}", plan.instance),
        "--log-driver=none".into(),
        // Always attached: stdin is the liveness pipe or the program's input.
        "-i".into(),
        "--entrypoint=/bin/sh".into(),
        plan.image.to_owned(),
        "-c".into(),
        supervisor_script(plan.stdin, plan.time_limit),
        // `$0` of the script; the program and its arguments follow as `"$@"`.
        "exposition-job".into(),
        plan.program.to_owned(),
    ]);
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

/// Whether `id` is an engine image identity (`sha256:` + 64 lowercase hex).
pub fn valid_image_id(id: &str) -> bool {
    id.strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// The identity (`sha256:…`) of a locally present image, or `None` when the
/// engine cannot say.
pub fn image_id(docker: &Path, image: &str, env: &Environment, cwd: &Path) -> Option<String> {
    helper(docker, &["image", "inspect", "--format", "{{.Id}}", image], env, cwd, Duration::from_secs(20))
        .ok()
        .filter(|o| o.code == Some(0))
        .map(|o| o.stdout.trim().to_owned())
        .filter(|id| valid_image_id(id))
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
/// Returns how many were removed, or why the engine could not be asked.
pub fn remove_stale(docker: &Path, instance: &str, env: &Environment, cwd: &Path) -> Result<usize, String> {
    let filter = format!("label={LABEL_INSTANCE}={instance}");
    let out = helper(docker, &["ps", "-aq", "--filter", &filter], env, cwd, Duration::from_secs(20))?;
    if out.code != Some(0) {
        return Err(out.stderr.lines().next().unwrap_or("docker ps failed").trim().to_owned());
    }
    let mut removed = 0;
    for id in out.stdout.split_whitespace() {
        if id.chars().all(|c| c.is_ascii_hexdigit())
            && helper(docker, &["rm", "-f", id], env, cwd, Duration::from_secs(20)).is_ok_and(|o| o.code == Some(0))
        {
            removed += 1;
        }
    }
    Ok(removed)
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

/// Host directory of a batch job's input file (mounted at [`INPUT_MOUNT`]).
pub fn input_dir(job_dir: &Path) -> PathBuf {
    job_dir.join("in")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_and_program_validation() {
        assert!(valid_image_ref("node:26-trixie-slim"));
        assert!(valid_image_ref("ghcr.io/org/img@sha256:0123456789abcdef0123456789abcdef"));
        assert!(!valid_image_ref("ghcr.io/org/img@sha256:abc"), "a digest needs at least 32 hex digits");
        assert!(!valid_image_ref("--privileged"));
        assert!(!valid_image_ref("img; rm -rf /"));
        assert!(!valid_image_ref(""));
        assert!(valid_container_program("node"));
        assert!(valid_container_program("/usr/bin/python3"));
        assert!(!valid_container_program("--privileged"));
    }

    /// Launch and settings accept exactly the same images: references the
    /// old character-set check let through are refused at launch too.
    #[test]
    fn one_image_validator_for_settings_and_launch() {
        for loose_but_invalid in ["Python:3.13", "img::tag", "img:", "a..b", "img/", "img:tag:other", "a_-b"] {
            assert!(validate_image_reference(loose_but_invalid).is_err(), "{loose_but_invalid}");
            assert!(!valid_image_ref(loose_but_invalid), "{loose_but_invalid} must be refused at launch too");
        }
        for valid in ["python:3.13-slim", "ghcr.io/org/img:v1", "localhost:5000/img:latest"] {
            assert!(valid_image_ref(valid) && validate_image_reference(valid).is_ok(), "{valid}");
        }
    }

    #[test]
    fn image_reference_grammar() {
        for valid in [
            "python:3.13-slim",
            "node:26-trixie-slim",
            "ubuntu",
            "library/ubuntu:24.04",
            "ghcr.io/org/team/img:v1.2_3",
            "localhost:5000/img:latest",
            "registry.example.com:443/a/b",
            "[::1]:5000/img",
            "public.ecr.aws/supabase/postgres:17.11.0.006",
            "img@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "img:tag@sha256:0123456789abcdef0123456789abcdef",
            "a__b:x",
            "a---b",
        ] {
            assert_eq!(validate_image_reference(valid), Ok(()), "{valid} should be valid");
        }
        for invalid in [
            "",
            "--privileged",
            "-img",
            "img; rm -rf /",
            "img && calc",
            "$(calc)",
            "img`calc`",
            "img|calc",
            "img calc",
            "img\ncalc",
            "Python:3.13",
            "img:",
            "img:-tag",
            ":tag",
            "img::tag",
            "img@sha256:abc",
            "img/",
            "/img",
            "a..b",
            "a_-b",
            "img:tag:other",
            "img:töt",
            "--network=host",
        ] {
            assert!(validate_image_reference(invalid).is_err(), "{invalid:?} should be refused");
        }
        let long_tag = format!("img:{}", "t".repeat(129));
        assert!(validate_image_reference(&long_tag).is_err());
        let long_name = "a".repeat(256);
        assert!(validate_image_reference(&long_name).is_err());
        assert!(validate_image_reference(&"a".repeat(255)).is_ok());
    }

    #[test]
    fn image_list_parsing_skips_untagged_and_invalid() {
        let out = concat!(
            r#"{"Containers":"N/A","CreatedAt":"2026-11-05 03:02:05 -0500 EST","CreatedSince":"6 days ago","Digest":"<none>","ID":"e48f59cdba69","Repository":"python","SharedSize":"N/A","Size":"178MB","Tag":"3.13-slim","UniqueSize":"N/A","VirtualSize":"178MB"}"#,
            "\n",
            r#"{"ID":"sha256:43b9ba1142b8aaaa","Repository":"node","Size":"337MB","Tag":"24-trixie-slim","CreatedSince":"3 weeks ago"}"#,
            "\n",
            r#"{"ID":"abc","Repository":"<none>","Size":"1MB","Tag":"<none>"}"#,
            "\n",
            r#"{"ID":"def","Repository":"dangling","Size":"1MB","Tag":"<none>"}"#,
            "\n",
            r#"{"ID":"bad","Repository":"Bad Name","Size":"1MB","Tag":"x"}"#,
            "\n",
            "not json\n",
            r#"{"ID":"e48f59cdba69","Repository":"python","Size":"178MB","Tag":"3.13-slim"}"#,
            "\n",
        );
        let images = parse_image_list(out);
        let refs: Vec<&str> = images.iter().map(|i| i.reference.as_str()).collect();
        assert_eq!(refs, ["node:24-trixie-slim", "python:3.13-slim"]);
        assert_eq!(images[0].id, "43b9ba1142b8");
        assert_eq!(images[1].size, "178MB");
        assert_eq!(images[1].created, "6 days ago");
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
            stdin: ContainerStdin::Batch { input_dir: None },
            time_limit: Some(Duration::from_secs(600)),
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
            "--init",
            "-i",
            "--entrypoint=/bin/sh",
        ] {
            assert!(args.iter().any(|a| a == required), "missing {required}: {args:?}");
        }
        assert!(args.iter().any(|a| a == r#"--mount=type=bind,"source=C:\work\a,b",target=/workspace,readonly"#));
        assert!(!args.iter().any(|a| a.contains(INPUT_MOUNT)), "no input mount without input");
        let image_at = args.iter().position(|a| a == "node:26-trixie-slim").unwrap();
        assert_eq!(args[image_at + 1], "-c");
        assert_eq!(&args[image_at + 3..], ["exposition-job", "node", "-e", "1"]);
        let script = &args[image_at + 2];
        assert!(script.contains("read -r _ <&3") && script.contains("kill -9 $$"), "{script}");
        assert!(script.contains("sleep 605 && kill -9 $$"), "time limit plus grace: {script}");
        assert!(script.ends_with("exec \"$@\" </dev/null 3<&-"), "{script}");
    }

    #[test]
    fn supervisor_script_variants() {
        let input = Path::new(r"C:\data\jobs\job-1\in");
        let batch = supervisor_script(ContainerStdin::Batch { input_dir: Some(input) }, None);
        assert!(batch.starts_with("exec 3<&0\n"), "{batch}");
        assert!(!batch.contains("sleep"), "no time limit: {batch}");
        assert!(batch.ends_with("exec \"$@\" </exposition-input/stdin 3<&-"), "{batch}");
        let interactive = supervisor_script(ContainerStdin::Interactive, Some(Duration::from_millis(1500)));
        assert!(!interactive.contains("<&3"), "interactive stdin is the program's: {interactive}");
        assert!(interactive.contains("sleep 7 && kill -9 $$"), "{interactive}");
        assert!(interactive.ends_with("exec \"$@\""), "{interactive}");
        let settings = DockerSettings::default();
        let args = run_args(&RunPlan {
            job_id: "job-2",
            instance: "abc",
            image: "python:3.13-slim",
            settings: &settings,
            mount_root: Path::new(r"C:\work"),
            workdir_components: Vec::new(),
            out_dir: Path::new(r"C:\data\jobs\job-2\out"),
            env_file: Path::new(r"C:\data\jobs\job-2\container.env"),
            program: "python",
            args: &["-".into()],
            stdin: ContainerStdin::Batch { input_dir: Some(input) },
            time_limit: None,
        });
        assert!(
            args.iter()
                .any(|a| a == r"--mount=type=bind,source=C:\data\jobs\job-1\in,target=/exposition-input,readonly"),
            "{args:?}"
        );
    }
}
