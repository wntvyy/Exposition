//! R-a1 F6 regression (adapted from the independent reviewer's
//! review/a1 `tests/review_a1_env.rs`): a host variable whose NAME looks
//! like a credential but whose VALUE cannot be a secret (a path, a short
//! word, a profile, region, or session id) is not a literal secret, so job
//! variables that merely contain it (e.g. the research kernel's
//! `JUPYTER_RUNTIME_DIR` under the data folder) are not refused. Real
//! secrets stay refused and redacted, and a refusal names the host variable
//! (never its value) so the person can act.

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

/// Held: a real secret value inside a job variable is refused, and the
/// refusal names the host variable but never shows the value.
#[test]
fn a_real_secret_in_a_job_variable_is_refused_and_named() {
    let secret = "sk-test-0123456789abcdefghijkl";
    let refused = job_env(("OPENAI_API_KEY", secret), &format!(r"C:\x\{secret}")).unwrap_err();
    assert!(refused.contains("JUPYTER_RUNTIME_DIR"), "{refused}");
    assert!(!refused.contains(secret), "the value is never shown: {refused}");
    // A plain (unshaped) secret: refused because it is the value of a credential variable.
    let plain = "Zq9plainKeyNoKnownShape7f3a2b1c";
    let refused = job_env(("DB_PASSWORD", plain), &format!(r"C:\x\{plain}")).unwrap_err();
    assert!(refused.contains("DB_PASSWORD"), "names the host variable so the person can act: {refused}");
    assert!(!refused.contains(plain), "{refused}");
    // A short password is still a secret under a password-like name.
    assert!(job_env(("DB_PASSWORD", "hunter2pw"), r"C:\x\hunter2pw\runtime").is_err());
}

/// R-a1 F6: ordinary configuration values under credential-like names do
/// not make ordinary paths look secret. Each case is a plausible real setup.
#[test]
fn non_secret_values_do_not_refuse_ordinary_paths() {
    let cases = [
        (
            ("CLAUDE_CODE_SESSION_ID", "5efb95c5-e1e2-456f-a33c-4f8b09ff0d72"),
            r"C:\Users\u\AppData\Local\Temp\claude\5efb95c5-e1e2-456f-a33c-4f8b09ff0d72\data\research\runtime",
        ),
        (("AWS_PROFILE", "research"), r"C:\Users\u\research\exposition-data\research\runtime"),
        (("AZURE_CONFIG_DIR", r"C:\Users\u"), r"C:\Users\u\AppData\Roaming\Exposition\research\runtime"),
        (("AWS_DEFAULT_REGION", "eu-central-1"), r"C:\data\eu-central-1\research\runtime"),
        (("GOOGLE_APPLICATION_CREDENTIALS", r"C:\Users\u\keys"), r"C:\Users\u\keys-backup\research\runtime"),
        (("OPENAI_ORG", "org-research"), r"C:\org-research\research\runtime"),
    ];
    let mut refused = Vec::new();
    for ((name, value), path) in cases {
        if let Err(e) = job_env((name, value), path) {
            refused.push(format!("{name}: {e}"));
        }
    }
    assert!(refused.is_empty(), "ordinary paths refused as secrets: {refused:#?}");
}
