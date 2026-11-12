//! Independent M1 review (R-m1, commit be63c73): adversarial execution tests
//! against real child processes (Windows). Uses the installed CPython found on
//! PATH (tests skip without it).
//!
//! `regression_*` tests reproduced reported defects (originally named
//! `review_defect_*`; they failed before the fixes) and now guard them.
//! `review_holds_*` tests document attacks that held up. The WMI escape is an
//! inherent limitation of job objects and is kept as an ignored,
//! documentation-only test.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use exposition_core::job::{JobId, JobOutcome, OutputStream};
use exposition_execution::env::HostEnv;
use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings, RefusalCode};

/// Absolute path of a real python.exe (not the WindowsApps alias).
fn python() -> Option<String> {
    let out = std::process::Command::new("python").args(["-c", "import sys; print(sys.executable)"]).output().ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!path.is_empty() && Path::new(&path).is_file()).then_some(path)
}

fn py_spec(f: &Fixture, py: &str, script: &str, extra: &[&str]) -> exposition_core::job::JobSpec {
    let mut spec = spec_in(&f.workspace, &[]);
    spec.program = py.to_owned();
    spec.args = ["-u", "-c", script].iter().chain(extra.iter()).map(|s| s.to_string()).collect();
    spec.label = "review python".into();
    spec
}

fn all_output(f: &Fixture, id: &JobId) -> String {
    format!("{}{}", f.stdout(id), f.service.output_text(id, OutputStream::Stderr).unwrap_or_default())
}

fn kill(pid: u32) {
    let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).output();
}

/// A heartbeat program for escape attempts: writes its pid to a file, then sleeps.
const HEARTBEAT: &str = "import os,sys,time\nopen(sys.argv[1],'w').write(str(os.getpid()))\ntime.sleep(120)";

async fn read_pid(path: &Path) -> Option<u32> {
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse()
        {
            return Some(pid);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

#[tokio::test(flavor = "multi_thread")]
async fn review_holds_child_environment_has_no_host_secrets() {
    let Some(py) = python() else { return eprintln!("no python; skipped") };
    let f = fixture();
    let id =
        f.service.launch(py_spec(&f, &py, "import os\nfor k,v in sorted(os.environ.items()): print(f'{k}={v}')", &[]));
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{}", all_output(&f, &id));
    let env = f.stdout(&id);
    for needle in ["FAKEFAKE", FAKE_GITHUB_TOKEN, "ANTHROPIC", "GITHUB_TOKEN", "host-only-value"] {
        assert!(!env.contains(needle), "{needle} leaked into the child environment:\n{env}");
    }
    let names: Vec<&str> = env.lines().filter_map(|l| l.split('=').next()).collect();
    eprintln!("child environment names: {names:?}");
}

/// Arguments reach the child exactly (no shell re-parsing), including quotes,
/// trailing backslashes, metacharacters, and percent signs.
#[tokio::test(flavor = "multi_thread")]
async fn review_holds_arguments_round_trip_exactly() {
    let Some(py) = python() else { return };
    let f = fixture();
    let hostile =
        ["a b", "c\"d", "e\\", "\\\"f", "&calc", "%PATH%", "^|<>", "", "x\ny", "' or 1=1 --", "\\\\server\\share\\"];
    let id = f.service.launch(py_spec(&f, &py, "import sys,json; print(json.dumps(sys.argv[1:]))", &hostile));
    f.service.wait(&id).await.unwrap();
    let got: Vec<String> = serde_json::from_str(f.stdout(&id).trim()).unwrap();
    assert_eq!(got, hostile);
}

/// Batch files, cmd.exe, PowerShell scripts, and extension tricks are refused.
#[tokio::test(flavor = "multi_thread")]
async fn review_holds_scripts_and_shells_refused() {
    let f = fixture();
    let dir = f.workspace.clone();
    for name in ["a.bat", "b.CMD", "c.ps1", "d.bat.", "e.bat ", "f.com", "g.vbs", "h.exe::$DATA"] {
        let path = dir.join(name.trim_end_matches(['.', ' ']));
        let _ = std::fs::write(&path, "@echo pwned > pwned.txt\r\n");
        let mut spec = f.spec(&[]);
        spec.program = format!("{}\\{name}", dir.display());
        let id = f.service.launch(spec);
        let result = f.service.wait(&id).await.unwrap();
        assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{name}: {result:?}");
    }
    for program in [r"C:\Windows\System32\cmd.exe", r"C:\WINDOWS\SYSTEM32\CMD.EXE", "cmd", "powershell", "cmd.exe"] {
        let mut spec = f.spec(&["/c", "echo pwned > pwned.txt"]);
        spec.program = program.into();
        let id = f.service.launch(spec);
        let result = f.service.wait(&id).await.unwrap();
        assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{program}: {result:?}");
    }
    assert!(!dir.join("pwned.txt").exists());
}

/// A child cannot break away from the job object even when it asks to.
#[tokio::test(flavor = "multi_thread")]
async fn review_holds_breakaway_request_is_denied_and_detached_child_dies_on_cancel() {
    let Some(py) = python() else { return };
    let f = fixture();
    let beat = f.tmp.path().join("detached.pid");
    let script = format!(
        "import subprocess,sys,time\n\
         flags = 0x00000008 | 0x00000200  # DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP\n\
         try:\n    subprocess.Popen([sys.executable,'-c',{HEARTBEAT:?},r'{nope}'], creationflags=flags|0x01000000)\n    print('BREAKAWAY-ALLOWED', flush=True)\n\
         except OSError as e:\n    print('BREAKAWAY-DENIED', e.winerror, flush=True)\n\
         subprocess.Popen([sys.executable,'-c',{HEARTBEAT:?},r'{beat}'], creationflags=flags)\n\
         print('spawned', flush=True)\ntime.sleep(120)",
        nope = f.tmp.path().join("breakaway.pid").display(),
        beat = beat.display(),
    );
    let id = f.service.launch(py_spec(&f, &py, &script, &[]));
    f.wait_stdout(&id, Duration::from_secs(20), |t| t.contains("spawned")).await;
    let pid = read_pid(&beat).await.expect("detached child started");
    let breakaway = read_pid(&f.tmp.path().join("breakaway.pid")).await;
    assert!(f.stdout(&id).contains("BREAKAWAY-DENIED"), "{}", f.stdout(&id));
    f.service.cancel(&id);
    f.service.wait(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let survived = alive(pid);
    if survived {
        kill(pid);
    }
    if let Some(b) = breakaway {
        kill(b);
    }
    assert!(!survived, "DETACHED_PROCESS child survived cancel");
}

/// `cmd /c start` launched by a child (cmd.exe itself is fine as a grandchild)
/// stays inside the job and dies on cancel.
#[tokio::test(flavor = "multi_thread")]
async fn review_holds_cmd_start_grandchild_dies_on_cancel() {
    let Some(py) = python() else { return };
    let f = fixture();
    let beat = f.tmp.path().join("start.pid");
    let hb = f.tmp.path().join("hb.py");
    std::fs::write(&hb, HEARTBEAT).unwrap();
    let script = r#"import subprocess,sys,time
subprocess.Popen(['cmd','/c','start','/min',sys.executable,sys.argv[1],sys.argv[2]])
print('spawned', flush=True)
time.sleep(120)"#;
    let id = f.service.launch(py_spec(&f, &py, script, &[&hb.display().to_string(), &beat.display().to_string()]));
    f.wait_stdout(&id, Duration::from_secs(20), |t| t.contains("spawned")).await;
    let pid = read_pid(&beat).await.expect("start child ran");
    f.service.cancel(&id);
    f.service.wait(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let survived = alive(pid);
    if survived {
        kill(pid);
    }
    assert!(!survived, "`start` child survived cancel");
}

/// DOCUMENTED LIMITATION (was `review_defect_wmi_created_process_escapes_cancel`):
/// a trusted native job can create a process through WMI
/// (`Win32_Process.Create`, here via PowerShell's Invoke-CimMethod). WMI's
/// provider host creates it OUTSIDE the job object, so it survives cancel,
/// timeout, and application exit, and gets the user's full environment. This is
/// inherent to job objects: trusted native execution runs with the user's
/// privileges and is not a sandbox (the trust dialog says so). Restricted
/// (container) execution is the isolation boundary. Run with `--ignored` to
/// observe; it reports the behavior and cleans up instead of failing.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "documents an inherent job-object limitation (WMI process creation escapes containment)"]
async fn limitation_wmi_created_process_escapes_the_job_object() {
    let Some(py) = python() else { return };
    let f = fixture();
    let beat = f.tmp.path().join("wmi.pid");
    let hb = f.tmp.path().join("hb.py");
    std::fs::write(&hb, HEARTBEAT).unwrap();
    let ps = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    let script = r#"import subprocess,sys,time
cl = ' '.join(sys.argv[1:4])
cmd = "$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{CommandLine='%s'}; $r.ProcessId" % cl
r = subprocess.run([sys.argv[4],'-NoProfile','-NonInteractive','-Command',cmd], capture_output=True, text=True)
print('wmi', r.returncode, r.stdout.strip(), r.stderr.strip()[:300], flush=True)
time.sleep(120)"#;
    let args = [py.clone(), hb.display().to_string(), beat.display().to_string(), ps.to_string()];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut spec = py_spec(&f, &py, script, &args);
    spec.timeout_ms = Some(90_000);
    let id = f.service.launch(spec);
    f.wait_stdout(&id, Duration::from_secs(60), |t| t.contains("wmi ")).await;
    eprintln!("{}", f.stdout(&id));
    let Some(pid) = read_pid(&beat).await else {
        f.service.cancel(&id);
        eprintln!("WMI process creation not available here; skipped: {}", all_output(&f, &id));
        return;
    };
    f.service.cancel(&id);
    f.service.wait(&id).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let survived = alive(pid);
    if survived {
        kill(pid);
    }
    eprintln!(
        "LIMITATION: WMI-created process {pid} {} the job cancel",
        if survived { "survived (killed by the test)" } else { "did not survive" }
    );
}

/// REGRESSION (was FINDING, high): a burst of non-ASCII output larger than the log limit
/// between two 100 ms flushes panics the stream reader thread
/// (`StreamCapture::enqueue` slices `ready[excess..]` at a byte offset that is
/// not a char boundary). The panic poisons the job's output mutex; the
/// supervising thread then panics too, so the job NEVER finishes: `wait` never
/// resolves, the job stays `running`, its concurrency slot is never released,
/// and every later `output()` call (the `job_output` IPC command) panics.
/// Any UTF-8 text with accents/CJK, or binary output (decoded to 3-byte U+FFFD),
/// triggers it.
#[tokio::test(flavor = "multi_thread")]
async fn regression_non_ascii_output_burst_completes() {
    let Some(py) = python() else { return };
    let f = fixture();
    // ~3 MB of three-byte UTF-8 characters (CJK) in one write; default 1 MB log
    // limit. (Two-byte characters happen not to trigger it: the cut stays
    // even.)
    let id = f.service.launch(py_spec(
        &f,
        &py,
        "import sys\nsys.stdout.buffer.write('数学'.encode('utf-8') * 500_000)\nsys.stdout.flush()",
        &[],
    ));
    let finished = tokio::time::timeout(Duration::from_secs(60), f.service.wait(&id)).await;
    let state = f.service.summary(&id).map(|s| s.state);
    let output_panics =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.service.output(&id, None, 1024))).is_err();
    assert!(
        finished.is_ok(),
        "job never finished within 60 s (state {state:?}); job_output for it panics: {output_panics}"
    );
    assert_eq!(finished.unwrap().unwrap().outcome, JobOutcome::Exited { code: 0 });
}

/// Consequence of the reader panic: the wedged job never releases its
/// concurrency slot, so with the slot limit reached every later job stays
/// queued forever (here the limit is 1; by default it is cores/2, 2–8).
#[tokio::test(flavor = "multi_thread")]
async fn regression_large_non_ascii_output_does_not_starve_the_queue() {
    let Some(py) = python() else { return };
    let f = fixture_with(ExecutionSettings { max_concurrent_jobs: Some(1), ..Default::default() }, true);
    let wedged = f.service.launch(py_spec(
        &f,
        &py,
        "import sys\nsys.stdout.buffer.write('数学'.encode('utf-8') * 500_000)\nsys.stdout.flush()",
        &[],
    ));
    let _ = tokio::time::timeout(Duration::from_secs(15), f.service.wait(&wedged)).await;
    let next = f.service.launch(f.spec(&["exit", "0"]));
    let finished = tokio::time::timeout(Duration::from_secs(20), f.service.wait(&next)).await;
    assert!(
        finished.is_ok(),
        "a trivial job stayed {:?} behind the wedged job",
        f.service.summary(&next).map(|s| s.state)
    );
}

/// Huge binary output stays bounded and the job completes (failed before the fix
/// for the same reason as the test above: binary decodes to 3-byte U+FFFD).
#[tokio::test(flavor = "multi_thread")]
async fn regression_huge_binary_output_completes_bounded() {
    let Some(py) = python() else { return };
    let f = fixture();
    let script = "import os,sys\nfor _ in range(16): sys.stdout.buffer.write(os.urandom(1<<20))\nsys.stdout.flush()";
    let mut spec = py_spec(&f, &py, script, &[]);
    spec.log_limit_bytes = Some(256 * 1024);
    let id = f.service.launch(spec);
    let finished = tokio::time::timeout(Duration::from_secs(60), f.service.wait(&id)).await;
    assert!(
        finished.is_ok(),
        "job with binary output never finished (state {:?})",
        f.service.summary(&id).map(|s| s.state)
    );
    let result = finished.unwrap().unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
    assert!(f.stdout(&id).len() <= 256 * 1024);
}

/// ASCII floods stay bounded and complete (the panic needs multi-byte text).
#[tokio::test(flavor = "multi_thread")]
async fn review_holds_huge_ascii_output_is_bounded() {
    let Some(py) = python() else { return };
    let f = fixture();
    let script = "import sys\nfor _ in range(32): sys.stdout.write('x' * (1<<20))\nsys.stdout.flush()";
    let mut spec = py_spec(&f, &py, script, &[]);
    spec.log_limit_bytes = Some(256 * 1024);
    let id = f.service.launch(spec);
    let result = tokio::time::timeout(Duration::from_secs(120), f.service.wait(&id)).await.expect("finished").unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
    assert!(result.output_truncated);
    assert!(f.stdout(&id).len() <= 256 * 1024);
}

/// REGRESSION (was FINDING, low): redaction could be bypassed when a secret is emitted in two
/// writes separated by an idle pause and the first write ends after only the
/// first two characters ("sk"). The idle flush releases "sk"; the remainder no
/// longer matches any token pattern or the literal secret.
#[tokio::test(flavor = "multi_thread")]
async fn regression_secret_split_by_idle_pause_is_redacted() {
    let Some(py) = python() else { return };
    let f = fixture();
    // The child never receives the key; this models output that contains it
    // (e.g. a tool echoing a config file). The test passes it as an argument
    // split in two so the full key never appears in one argument either.
    let (a, b) = FAKE_ANTHROPIC_KEY.split_at(2);
    let script = "import sys,time\nsys.stdout.write('key=' + sys.argv[1]); sys.stdout.flush(); time.sleep(2)\nprint(sys.argv[2], flush=True)";
    let id = f.service.launch(py_spec(&f, &py, script, &[a, b]));
    f.service.wait(&id).await.unwrap();
    let out = f.stdout(&id);
    assert!(!out.contains("FAKEFAKEFAKE"), "secret body leaked: {out:?}");
}

/// REGRESSION (was FINDING): trust refused the home directory itself but accepted its parent
/// (`C:\Users`) and other ancestors/siblings of refused roots, and trust
/// covers every descendant — one grant trusts every project of every user.
#[tokio::test(flavor = "multi_thread")]
async fn regression_trust_refuses_ancestors_of_broad_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let service = ExecutionService::with_config(ExecutionConfig {
        data_dir: tmp.path().join("data"),
        host_env: HostEnv::capture(),
        settings: Some(ExecutionSettings::default()),
        too_broad_to_trust: None, // the application's default list
        tool_cache_dir: None,
    })
    .unwrap();
    let profile = PathBuf::from(std::env::var("USERPROFILE").unwrap());
    // The refused root itself:
    assert!(service.grant_trust(&profile.display().to_string()).is_err());
    let mut accepted = Vec::new();
    for root in [
        profile.parent().unwrap().to_path_buf(), // C:\Users
        profile.join("AppData"),
        profile.join("AppData").join("Local").join("Temp"),
        PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into())),
        PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32"),
    ] {
        if root.is_dir() && service.grant_trust(&root.display().to_string()).is_ok_and(|s| s.trusted) {
            accepted.push(root.display().to_string());
        }
    }
    // Any project anywhere under the profile is now trusted.
    let project = tmp.path().join("some-downloaded-project");
    std::fs::create_dir_all(&project).unwrap();
    let covered = service.trust_status(&project.display().to_string()).unwrap();
    assert!(
        accepted.is_empty(),
        "broad roots trusted: {accepted:?}; an unrelated new project is trusted={} via {:?}",
        covered.trusted,
        covered.trusted_via
    );
}

/// The refusal code for a job in an untrusted workspace is precise and no
/// process starts (sanity check for the trust tests above).
#[tokio::test(flavor = "multi_thread")]
async fn review_holds_untrusted_workspace_never_spawns() {
    let Some(py) = python() else { return };
    let f = fixture_with(ExecutionSettings::default(), false);
    let marker = f.workspace.join("ran.txt");
    let id = f.service.launch(py_spec(&f, &py, &format!("open(r'{}','w').write('x')", marker.display()), &[]));
    f.service.wait(&id).await.unwrap();
    assert_eq!(f.service.summary(&id).unwrap().refusal.unwrap().code, RefusalCode::UntrustedWorkspace);
    assert!(!marker.exists());
}
