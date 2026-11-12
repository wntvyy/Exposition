//! Deterministic child process for execution-service integration tests.
//!
//! Usage: `exposition-testproc <mode> [args...]`
//! - `tree <depth> <secs>`: print `pid=<pid> depth=<depth>`, spawn a child with
//!   `depth-1` (inheriting stdio) when depth > 0, then sleep.
//! - `orphan <secs>`: spawn a `tree 0 <secs>` grandchild, print its pid, and exit
//!   immediately (the grandchild must not outlive the job).
//! - `env`: print the environment as sorted `NAME=VALUE` lines.
//! - `cwd`: print the working directory.
//! - `exit <code>`: exit with the given code.
//! - `print <text>` / `eprint <text>`: write text (plus newline) to stdout/stderr.
//! - `flood <bytes>`: write roughly that many bytes of 100-byte lines to stdout.
//! - `flood-cjk <bytes>` / `flood-binary <bytes>`: one large write of 3-byte
//!   UTF-8 characters / of invalid UTF-8 bytes, without newlines.
//! - `sleep <secs>`: sleep.
//! - `--version`: print `<exe stem> (fake) version 4.12.0` and append
//!   `<exe stem> --version` to `invocations.log` beside the executable.
//! - `echo-lines`: echo each stdin line as `echo: <line>` until EOF.
//! - `stdin-count`: read stdin to EOF and print the byte count.

use std::io::{BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn arg(args: &[String], index: usize) -> &str {
    args.get(index).map(String::as_str).unwrap_or("")
}

fn secs(value: &str) -> Duration {
    Duration::from_secs_f64(value.parse().unwrap_or(1.0))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let me = std::env::current_exe().expect("current exe");
    match arg(&args, 0) {
        "tree" => {
            let depth: u32 = arg(&args, 1).parse().unwrap_or(0);
            println!("pid={} depth={}", std::process::id(), depth);
            let child = (depth > 0).then(|| {
                Command::new(&me)
                    .args(["tree", &(depth - 1).to_string(), arg(&args, 2)])
                    .spawn()
                    .expect("spawn descendant")
            });
            std::thread::sleep(secs(arg(&args, 2)));
            if let Some(mut child) = child {
                let _ = child.wait();
            }
        }
        "orphan" => {
            let child = Command::new(&me)
                .args(["tree", "0", arg(&args, 1)])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn orphan");
            println!("orphan={}", child.id());
            std::mem::forget(child);
        }
        "env" => {
            let mut vars: Vec<(String, String)> = std::env::vars().collect();
            vars.sort();
            for (k, v) in vars {
                println!("{k}={v}");
            }
        }
        "cwd" => println!("{}", std::env::current_dir().expect("cwd").display()),
        "exit" => std::process::exit(arg(&args, 1).parse().unwrap_or(1)),
        "print" => println!("{}", arg(&args, 1)),
        "eprint" => eprintln!("{}", arg(&args, 1)),
        "flood" => {
            let total: usize = arg(&args, 1).parse().unwrap_or(1024);
            let line = format!("{}\n", "x".repeat(99));
            let mut out = std::io::stdout().lock();
            let mut written = 0;
            while written < total {
                if out.write_all(line.as_bytes()).is_err() {
                    break;
                }
                written += line.len();
            }
            let _ = out.flush();
        }
        "flood-cjk" | "flood-binary" => {
            // One large write of 3-byte UTF-8 characters, or of invalid UTF-8.
            let total: usize = arg(&args, 1).parse().unwrap_or(1024);
            let data: Vec<u8> = if arg(&args, 0) == "flood-cjk" {
                "数学".repeat(total / 6 + 1).into_bytes()
            } else {
                (0..total).map(|i| [0xff, 0xfe, 0x80, b'a'][i % 4]).collect()
            };
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&data);
            let _ = out.flush();
        }
        "--version" => {
            // Stand-in for a tool's version query (e.g. a fake elan layout):
            // report the name we were invoked as and log the invocation next
            // to the executable.
            let stem = me.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            println!("{stem} (fake) version 4.12.0");
            if let Some(dir) = me.parent() {
                let mut log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("invocations.log"))
                    .expect("invocation log");
                let _ = writeln!(log, "{stem} --version");
            }
        }
        "sleep" => std::thread::sleep(secs(arg(&args, 1))),
        "echo-lines" => {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                println!("echo: {line}");
            }
        }
        "stdin-count" => {
            let mut data = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut data);
            println!("{}", data.len());
        }
        other => {
            eprintln!("unknown mode {other:?}");
            std::process::exit(2);
        }
    }
}
