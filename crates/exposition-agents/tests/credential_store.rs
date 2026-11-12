//! The provider key in Windows Credential Manager: kept on this machine only,
//! and a key saved by an earlier version keeps working (it is moved, byte for
//! byte, never deleted).
//!
//! Every test uses its own throwaway namespace
//! (`org.exposition.desktop.test-…`); the real entry is never read, written,
//! or deleted.
#![cfg(windows)]

use exposition_agents::credentials::{KEYRING_SERVICE, KEYRING_USER, KeyringStore, SecretStore};
use exposition_agents::wincred::{self, PERSIST_ENTERPRISE, PERSIST_LOCAL_MACHINE};

const OLD_KEY: &str = "sk-ant-api03-test-only-old-key-0123456789abcdef";
const NEW_KEY: &str = "sk-ant-api03-test-only-new-key-fedcba9876543210";

/// A namespace that is removed again however the test ends.
struct Namespace {
    name: String,
    /// The tests write credentials of both persistence kinds; Credential
    /// Manager can lose a persistence change raced by another write of the
    /// same user, so they do not run concurrently with each other.
    _serial: std::sync::MutexGuard<'static, ()>,
}

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Namespace {
    fn new(label: &str) -> Self {
        let name = format!("test-{label}-{}", uuid::Uuid::new_v4().simple());
        assert!(name.starts_with("test-"), "never the real entry");
        Self { name, _serial: SERIAL.lock().unwrap_or_else(|p| p.into_inner()) }
    }
    fn service(&self) -> String {
        format!("{KEYRING_SERVICE}.{}", self.name)
    }
    fn target(&self) -> String {
        format!("{KEYRING_USER}.{}", self.service())
    }
    fn persist(&self) -> Option<u32> {
        wincred::read(&self.target()).expect("read the test credential").map(|c| c.persist)
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        if let Ok(entry) = keyring::Entry::new(&self.service(), KEYRING_USER) {
            let _ = entry.delete_credential();
        }
    }
}

#[test]
fn a_key_saved_by_an_earlier_version_keeps_working_and_stays_on_this_machine() {
    let ns = Namespace::new("migrate");
    // Saved the way earlier versions saved it: the keyring crate, roaming
    // ("enterprise") persistence.
    keyring::Entry::new(&ns.service(), KEYRING_USER).unwrap().set_password(OLD_KEY).unwrap();
    assert_eq!(ns.persist(), Some(PERSIST_ENTERPRISE), "setup: the earlier layout");

    // Next start: the key reads back unchanged, now kept on this machine only.
    let store = KeyringStore::new(Some(&ns.name));
    assert_eq!(store.get().unwrap().map(|s| s.expose().to_owned()).as_deref(), Some(OLD_KEY));
    assert_eq!(ns.persist(), Some(PERSIST_LOCAL_MACHINE), "moved to local-machine persistence");
    // Still the same credential for the keyring crate (and earlier versions).
    assert_eq!(keyring::Entry::new(&ns.service(), KEYRING_USER).unwrap().get_password().unwrap(), OLD_KEY);
    // A second start finds nothing to do and still reads the key.
    let again = KeyringStore::new(Some(&ns.name));
    assert_eq!(again.get().unwrap().map(|s| s.expose().to_owned()).as_deref(), Some(OLD_KEY));
    assert_eq!(ns.persist(), Some(PERSIST_LOCAL_MACHINE));
}

#[test]
fn a_saved_key_is_kept_on_this_machine_and_readable_by_keyring() {
    let ns = Namespace::new("save");
    let store = KeyringStore::new(Some(&ns.name));
    assert!(store.get().unwrap().is_none());
    assert_eq!(ns.persist(), None, "reading an absent key creates nothing");
    store.set(NEW_KEY).unwrap();
    assert_eq!(ns.persist(), Some(PERSIST_LOCAL_MACHINE));
    assert_eq!(store.get().unwrap().map(|s| s.expose().to_owned()).as_deref(), Some(NEW_KEY));
    assert_eq!(keyring::Entry::new(&ns.service(), KEYRING_USER).unwrap().get_password().unwrap(), NEW_KEY);
    // Replacing a roaming copy (written by an earlier version) works too.
    keyring::Entry::new(&ns.service(), KEYRING_USER).unwrap().set_password(OLD_KEY).unwrap();
    store.set(NEW_KEY).unwrap();
    assert_eq!(ns.persist(), Some(PERSIST_LOCAL_MACHINE));
    store.delete().unwrap();
    assert_eq!(ns.persist(), None);
    assert!(store.get().unwrap().is_none());
}

/// The stored key is redacted by its literal value, whatever its shape: it is
/// registered with the redactor when saved or loaded and forgotten when
/// cleared. The fake key has no known token shape (no `sk-ant-` prefix).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stored_key_is_redacted_by_value_whatever_its_shape() {
    use exposition_agents::credentials::{ObservedStore, PROVIDER_KEY_SECRET};
    use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobOutcome, JobSpec, OutputStream};
    use exposition_execution::{ExecutionConfig, ExecutionService, ExecutionSettings};

    const PLAIN_KEY: &str = "Zq9plainKeyNoKnownShape7f3a2b1c";
    let ns = Namespace::new("redact");
    let data = tempfile::tempdir().unwrap();
    let service = |dir: &std::path::Path| {
        let mut config = ExecutionConfig::new(dir);
        config.settings = Some(ExecutionSettings::default());
        config.too_broad_to_trust = Some(Vec::new());
        ExecutionService::with_config(config).unwrap()
    };
    let observed = |execution: &ExecutionService| {
        let redactor = execution.clone();
        ObservedStore::new(
            KeyringStore::new(Some(&ns.name)),
            Box::new(move |key| redactor.register_secret(PROVIDER_KEY_SECRET, key)),
        )
    };
    let execution = service(&data.path().join("a"));
    assert_eq!(execution.redact(PLAIN_KEY), PLAIN_KEY, "setup: no token pattern matches this key");

    // Saved: redacted from now on.
    let store = observed(&execution);
    store.set(PLAIN_KEY).unwrap();
    assert_eq!(execution.redact(&format!("key={PLAIN_KEY}.")), "key=[REDACTED].");

    // A process that prints it: its output and arguments are redacted.
    if let exposition_execution::Located::Found { path: python, .. } = execution.locate_tool("python") {
        let project = tempfile::tempdir().unwrap();
        let root = execution.register_workspace_root(project.path()).unwrap();
        execution.grant_trust(&root.display().to_string()).unwrap();
        let result = execution
            .run(JobSpec {
                label: "print".into(),
                program: python.display().to_string(),
                args: vec!["-c".into(), "import sys; print('printed:' + sys.argv[1])".into(), PLAIN_KEY.into()],
                environment: EnvironmentSpec::default(),
                working_directory: root.display().to_string(),
                policy: ExecutionPolicy::TrustedNative,
                timeout_ms: Some(60_000),
                log_limit_bytes: None,
                stdin: None,
            })
            .await;
        assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
        let stdout = execution.output_text(&result.id, OutputStream::Stdout).unwrap_or_default();
        assert!(stdout.contains("printed:[REDACTED]") && !stdout.contains(PLAIN_KEY), "{stdout}");
        let listed = format!("{:?}", execution.list());
        assert!(!listed.contains(PLAIN_KEY), "job listings redact it");
    } else {
        eprintln!("SKIPPED the process part: python was not found");
    }

    // The next start: loading the stored key registers it again.
    let next = service(&data.path().join("b"));
    assert_eq!(next.redact(PLAIN_KEY), PLAIN_KEY);
    let loaded = observed(&next);
    assert_eq!(loaded.get().unwrap().map(|s| s.expose().to_owned()).as_deref(), Some(PLAIN_KEY));
    assert_eq!(next.redact(PLAIN_KEY), "[REDACTED]");

    // Cleared: forgotten.
    loaded.delete().unwrap();
    assert_eq!(next.redact(PLAIN_KEY), PLAIN_KEY);
    assert!(store.get().unwrap().is_none());
    assert_eq!(execution.redact(PLAIN_KEY), PLAIN_KEY, "a read that finds no key forgets it too");
}
