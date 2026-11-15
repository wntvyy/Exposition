//! R-a1 (independent review of A1): a non-secret value of a host variable
//! whose NAME matches the broad credential rules becomes a "literal secret";
//! any job variable that merely contains it (e.g. the research kernel's
//! `JUPYTER_RUNTIME_DIR` under the data folder) is refused with
//! `EnvError::SecretValue`, so kernels do not start. Observed by lane E with
//! `CLAUDE_CODE_SESSION_ID` (a session id that is part of the temp path).
//!
//! Tests marked `#[ignore = "R-a1 F<n>: ..."]` demonstrate a CURRENT defect.
//! Run: `cargo test -p exposition-execution --test review_a1_env -- --include-ignored --nocapture`.

use std::path::PathBuf;

use exposition_core::job::EnvironmentSpec;
use exposition_execution::credentials::Redactor;
use exposition_execution::env::{EnvContext, HostEnv, build_environment};

fn job_env(host_var: (&str, &str), runtime_dir: &str) -> Result<(), String> {
    let host = HostEnv::from_pairs([("SystemRoot", r"C:\Windows"), host_var]);
    let redactor = Redactor::from_host_env([host_var]);
    let temp = PathBuf::from(r"C:\Temp\job");
    let ctx = EnvContext { host: &host, redactor: &redactor, temp_dir: &temp, path_dirs: &[] };
    let spec = EnvironmentSpec {
        inherit: Vec::new(),
        set: [("JUPYTER_RUNTIME_DIR".to_owned(), runtime_dir.to_owned())].into(),
    };
    build_environment(&spec, &ctx).map(|_| ()).map_err(|e| e.to_string())
}

/// Held: a real secret value inside a job variable is refused.
#[test]
fn review_a1_held_a_real_secret_in_a_job_variable_is_refused() {
    let refused = job_env(("OPENAI_API_KEY", "sk-test-0123456789abcdefghijkl"), r"C:\x\sk-test-0123456789abcdefghijkl");
    assert!(refused.is_err(), "{refused:?}");
}

/// R-a1 F6: ordinary configuration values under credential-like names make
/// ordinary paths look secret. Each case is a plausible real setup.
#[test]
#[ignore = "R-a1 F6: non-secret values of credential-named host variables refuse jobs whose paths contain them"]
fn review_a1_f6_non_secret_values_do_not_refuse_ordinary_paths() {
    let cases = [
        // Running Exposition from a shell started by Claude Code (lane E's observation).
        (
            ("CLAUDE_CODE_SESSION_ID", "5efb95c5-e1e2-456f-a33c-4f8b09ff0d72"),
            r"C:\Users\u\AppData\Local\Temp\claude\5efb95c5-e1e2-456f-a33c-4f8b09ff0d72\data\research\runtime",
        ),
        // An AWS profile named like a folder the user keeps projects in.
        (("AWS_PROFILE", "research"), r"C:\Users\u\research\exposition-data\research\runtime"),
        // Azure CLI configuration folder = the home folder.
        (("AZURE_CONFIG_DIR", r"C:\Users\u"), r"C:\Users\u\AppData\Roaming\Exposition\research\runtime"),
    ];
    let mut refused = Vec::new();
    for ((name, value), path) in cases {
        if let Err(e) = job_env((name, value), path) {
            refused.push(format!("{name}: {e}"));
        }
    }
    eprintln!("refused: {refused:#?}");
    assert!(refused.is_empty(), "DEFECT: ordinary paths refused as secrets: {refused:?}");
}
