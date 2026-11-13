//! Per-language restricted images (M9: R and Julia workflow batches).
//!
//! A language's restricted job runs only in that language's configured image:
//! never in the default (Python/agent) image, never natively, never pulled.
//! The refusal tests need no Docker. The Docker tests run only with
//! `EXPOSITION_DOCKER_TESTS=1` and the local images `r-base:4.6.2` and
//! `julia:1.13` (override with `EXPOSITION_TEST_R_IMAGE` /
//! `EXPOSITION_TEST_JULIA_IMAGE`); they never pull.

mod common;

use common::*;
use exposition_core::job::{ExecutionPolicy, JobOutcome, JobSpec};
use exposition_execution::{
    DockerSettings, ExecutionSettings, ExecutionSettingsChange, RefusalCode, RestrictedLanguage, SettingsError,
};

const PYTHON_IMAGE: &str = "python:3.13-slim";

fn r_image() -> String {
    std::env::var("EXPOSITION_TEST_R_IMAGE").unwrap_or_else(|_| "r-base:4.6.2".into())
}

fn julia_image() -> String {
    std::env::var("EXPOSITION_TEST_JULIA_IMAGE").unwrap_or_else(|_| "julia:1.13".into())
}

fn docker_enabled() -> bool {
    let on = std::env::var("EXPOSITION_DOCKER_TESTS").is_ok_and(|v| v == "1");
    if !on {
        eprintln!(
            "SKIPPED: set EXPOSITION_DOCKER_TESTS=1 (Docker Desktop with local {} and {})",
            r_image(),
            julia_image()
        );
    }
    on
}

fn settings(image: Option<&str>, r: Option<&str>, julia: Option<&str>) -> ExecutionSettings {
    ExecutionSettings {
        docker: DockerSettings {
            image: image.map(str::to_owned),
            r_image: r.map(str::to_owned),
            julia_image: julia.map(str::to_owned),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn restricted(f: &Fixture, program: &str, args: &[&str]) -> JobSpec {
    let mut spec = f.spec(&[]);
    spec.program = program.into();
    spec.args = args.iter().map(|a| (*a).to_owned()).collect();
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: None };
    spec.timeout_ms = Some(120_000);
    spec
}

fn refusal(f: &Fixture, id: &exposition_core::job::JobId) -> (RefusalCode, String) {
    let r = f.service.summary(id).unwrap().refusal.expect("refused");
    (r.code, r.message)
}

/// The default image never stands in for a language without its own image.
#[tokio::test(flavor = "multi_thread")]
async fn a_language_without_its_image_is_refused_even_when_the_default_image_is_set() {
    let f = fixture_with(settings(Some(PYTHON_IMAGE), None, None), true);
    for language in [RestrictedLanguage::R, RestrictedLanguage::Julia] {
        let id = f.service.launch_restricted_language(restricted(&f, "Rscript", &["-e", "1"]), language);
        let result = f.service.wait(&id).await.unwrap();
        assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{:?}", result.outcome);
        let (code, message) = refusal(&f, &id);
        assert_eq!(code, RefusalCode::RestrictedBackendUnavailable, "{message}");
        assert!(message.contains(language.setting()) && message.contains("not run natively"), "{message}");
        let reason = f.service.restricted_preflight_for(Some(language)).unwrap_err();
        assert!(reason.contains(language.setting()) && reason.contains("never pulls"), "{reason}");
    }
}

/// A language image applies only to restricted jobs: a native job asking
/// for one is refused (it would otherwise run on the host).
#[tokio::test(flavor = "multi_thread")]
async fn a_language_image_on_a_native_job_is_refused() {
    let f = fixture_with(settings(None, Some("r-base:4.6.2"), None), true);
    let spec = f.spec(&["echo", "must-not-run"]);
    assert_eq!(spec.policy, ExecutionPolicy::TrustedNative);
    let id = f.service.launch_restricted_language(spec, RestrictedLanguage::R);
    let result = f.service.wait(&id).await.unwrap();
    assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{:?}", result.outcome);
    let (code, message) = refusal(&f, &id);
    assert_eq!(code, RefusalCode::InvalidArgument, "{message}");
    assert!(!f.stdout(&id).contains("must-not-run"));
}

#[test]
fn language_images_are_set_through_the_validated_settings_change() {
    let f = fixture_with(ExecutionSettings::default(), false);
    let change = |language, image: Option<&str>| ExecutionSettingsChange::LanguageImage {
        language,
        image: image.map(str::to_owned),
    };
    let state = f.service.apply_settings_change(change(RestrictedLanguage::R, Some("  r-base:4.6.2 "))).unwrap();
    assert_eq!(state.settings.docker.r_image.as_deref(), Some("r-base:4.6.2"));
    assert_eq!(state.settings.docker.image, None, "the default image is a separate setting");
    let state = f.service.apply_settings_change(change(RestrictedLanguage::Julia, Some("julia:1.13"))).unwrap();
    assert_eq!(state.settings.docker.julia_image.as_deref(), Some("julia:1.13"));
    for bad in ["R-Base:4", "r-base 4", "--privileged", "a;b"] {
        let err = f.service.apply_settings_change(change(RestrictedLanguage::R, Some(bad))).unwrap_err();
        assert!(matches!(err, SettingsError::InvalidImage(_)), "{bad}: {err:?}");
    }
    assert_eq!(f.service.settings().docker.r_image.as_deref(), Some("r-base:4.6.2"), "a refused change keeps the old");
    let state = f.service.apply_settings_change(change(RestrictedLanguage::R, Some(""))).unwrap();
    assert_eq!(state.settings.docker.r_image, None, "an empty image clears the setting");
    assert_eq!(state.settings.docker.julia_image.as_deref(), Some("julia:1.13"));
}

fn image_id(image: &str) -> String {
    let out = std::process::Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", image])
        .output()
        .expect("docker image inspect");
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(id.starts_with("sha256:"), "{image} is not present locally: {id}");
    id
}

/// Each language's job runs in its own image with the restricted
/// containment, and the default image is a different one.
#[tokio::test(flavor = "multi_thread")]
async fn docker_language_jobs_run_in_their_own_image_with_the_same_containment() {
    if !docker_enabled() {
        return;
    }
    let (r, julia) = (r_image(), julia_image());
    let f = fixture_with(settings(Some(PYTHON_IMAGE), Some(&r), Some(&julia)), false);
    let r_target = f.service.restricted_preflight_for(Some(RestrictedLanguage::R)).unwrap();
    assert_eq!((r_target.image.as_str(), r_target.image_id.clone()), (r.as_str(), Some(image_id(&r))));
    let julia_target = f.service.restricted_preflight_for(Some(RestrictedLanguage::Julia)).unwrap();
    assert_eq!(julia_target.image_id, Some(image_id(&julia)));
    assert_eq!(f.service.restricted_preflight().unwrap().image, PYTHON_IMAGE);

    let script = "cat(R.version.string, '\\n'); cat('uid', system('id -u', intern = TRUE), '\\n'); \
                  cat('net', tryCatch({ con <- socketConnection('1.1.1.1', 53, timeout = 3); close(con); 'CONNECTED' }, \
                  error = function(e) 'blocked'), '\\n'); cat('key', Sys.getenv('ANTHROPIC_API_KEY'), '\\n'); \
                  cat('rw', tryCatch({ writeLines('x', 'new.txt'); 'WROTE' }, error = function(e) 'refused', \
                  warning = function(w) 'refused'), '\\n')";
    let id = f.service.launch_restricted_language(restricted(&f, "Rscript", &["-e", script]), RestrictedLanguage::R);
    let result = f.service.wait(&id).await.unwrap();
    let out = f.stdout(&id);
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{out}");
    assert!(out.contains("R version"), "{out}");
    assert!(out.contains("uid 65534"), "non-root: {out}");
    assert!(out.contains("net blocked") && !out.contains("CONNECTED"), "{out}");
    assert!(!out.contains(FAKE_ANTHROPIC_KEY) && out.lines().any(|l| l.trim() == "key"), "no host key: {out}");
    assert!(out.contains("rw refused") && !out.contains("WROTE"), "read-only workspace: {out}");
    assert!(!f.workspace.join("new.txt").exists());

    let id = f.service.launch_restricted_language(
        restricted(&f, "julia", &["--startup-file=no", "-e", "println(\"julia \", VERSION)"]),
        RestrictedLanguage::Julia,
    );
    // The official image keeps julia in /usr/local/julia/bin, outside the
    // constructed PATH: the batch adapter adds it; a bare job does not find it.
    let result = f.service.wait(&id).await.unwrap();
    let mut spec =
        restricted(&f, "/usr/local/julia/bin/julia", &["--startup-file=no", "-e", "println(\"julia \", VERSION)"]);
    spec.environment.set.insert("HOME".into(), "/tmp".into());
    let id2 = f.service.launch_restricted_language(spec, RestrictedLanguage::Julia);
    let result2 = f.service.wait(&id2).await.unwrap();
    assert_eq!(result2.outcome, JobOutcome::Exited { code: 0 }, "{}", f.stdout(&id2));
    assert!(f.stdout(&id2).contains("julia 1."), "{}", f.stdout(&id2));
    assert_ne!(result.outcome, JobOutcome::Exited { code: 0 }, "julia is not on the constructed PATH");

    // The R program does not exist in the Julia image: an R job is never
    // run there (it was given the R image, not "any configured image").
    let id = f.service.launch_restricted_language(restricted(&f, "Rscript", &["-e", "1"]), RestrictedLanguage::Julia);
    let result = f.service.wait(&id).await.unwrap();
    assert_ne!(result.outcome, JobOutcome::Exited { code: 0 }, "Rscript is not in the Julia image");
}

/// A language job pinned to an identity runs only while that language's
/// image has it (the default image's identity does not count).
#[tokio::test(flavor = "multi_thread")]
async fn docker_language_jobs_are_pinned_to_their_images_identity() {
    if !docker_enabled() {
        return;
    }
    let r = r_image();
    let f = fixture_with(settings(Some(PYTHON_IMAGE), Some(&r), None), false);
    let mut spec = restricted(&f, "Rscript", &["-e", "cat('pinned ok')"]);
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: Some(image_id(&r)) };
    let id = f.service.launch_restricted_language(spec, RestrictedLanguage::R);
    let result = f.service.wait(&id).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 }, "{}", f.stdout(&id));
    assert!(f.stdout(&id).contains("pinned ok"));

    // Pinned to the Python image's identity: refused, nothing runs.
    let mut spec = restricted(&f, "Rscript", &["-e", "cat('must not run')"]);
    spec.policy = ExecutionPolicy::Restricted { backend: "docker".into(), image_id: Some(image_id(PYTHON_IMAGE)) };
    let id = f.service.launch_restricted_language(spec, RestrictedLanguage::R);
    let result = f.service.wait(&id).await.unwrap();
    assert!(matches!(result.outcome, JobOutcome::FailedToStart { .. }), "{:?}", result.outcome);
    assert_eq!(refusal(&f, &id).0, RefusalCode::RestrictedBackendUnavailable);
    assert!(!f.stdout(&id).contains("must not run"));
}
