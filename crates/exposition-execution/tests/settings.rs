//! Settings API of the execution service: validated changes (Docker image,
//! tool overrides), persistence, discovery invalidation, and image listing.

mod common;

use std::path::PathBuf;

use common::*;
use exposition_execution::env::HostEnv;
use exposition_execution::{
    DockerListError, ExecutionConfig, ExecutionService, ExecutionSettings, ExecutionSettingsChange, Located,
    SettingsError, ToolSource, ToolStatus,
};

fn persistent_service(data_dir: &std::path::Path, host: HostEnv) -> ExecutionService {
    ExecutionService::with_config(ExecutionConfig {
        data_dir: data_dir.to_path_buf(),
        host_env: host,
        settings: None,
        too_broad_to_trust: Some(vec![]),
        tool_cache_dir: Some(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("exposition-tool-cache")),
    })
    .unwrap()
}

fn image(image: Option<&str>) -> ExecutionSettingsChange {
    ExecutionSettingsChange::DockerImage { image: image.map(str::to_owned) }
}

fn tool_path(tool: &str, path: Option<&str>) -> ExecutionSettingsChange {
    ExecutionSettingsChange::ToolPath { tool: tool.into(), path: path.map(str::to_owned) }
}

#[test]
fn docker_image_changes_are_validated_and_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    let service = persistent_service(tmp.path(), host_env());
    let state = service.apply_settings_change(image(Some(" python:3.13-slim "))).unwrap();
    assert_eq!(state.settings.docker.image.as_deref(), Some("python:3.13-slim"));
    for bad in ["--privileged", "img; calc", "python:3.13 --network=host", "Python:3", "$(calc)", "a\nb"] {
        let error = service.apply_settings_change(image(Some(bad))).unwrap_err();
        assert!(matches!(error, SettingsError::InvalidImage(_)), "{bad:?}: {error:?}");
        assert_eq!(error.code(), "settings.invalidImage");
    }
    // Refused changes leave the setting untouched, and it survives a restart.
    drop(service);
    let reloaded = persistent_service(tmp.path(), host_env());
    assert_eq!(reloaded.settings().docker.image.as_deref(), Some("python:3.13-slim"));
    // Container limits and user are unchanged defaults (not changeable here).
    assert_eq!(reloaded.settings().docker, {
        let mut expected = ExecutionSettings::default().docker;
        expected.image = Some("python:3.13-slim".into());
        expected
    });
    let cleared = reloaded.apply_settings_change(image(None)).unwrap();
    assert_eq!(cleared.settings.docker.image, None);
    assert_eq!(reloaded.apply_settings_change(image(Some("  "))).unwrap().settings.docker.image, None);
}

#[test]
fn tool_overrides_are_validated_and_located() {
    let tmp = tempfile::tempdir().unwrap();
    let service = persistent_service(&tmp.path().join("data"), host_env());
    let state = service.settings_state();
    for id in [
        "pdflatex", "lualatex", "latexmk", "biber", "bibtex", "python", "rscript", "r", "julia", "lean", "lake",
        "docker", "git",
    ] {
        assert!(state.overridable_tools.iter().any(|t| t == id), "{id} must be overridable");
    }
    for component in ["python-jupyter", "irkernel", "ijulia"] {
        assert!(!state.overridable_tools.iter().any(|t| t == component));
        let error = service.apply_settings_change(tool_path(component, Some(&testproc()))).unwrap_err();
        assert!(matches!(error, SettingsError::UnknownTool(_)), "{error:?}");
    }
    assert!(matches!(
        service.apply_settings_change(tool_path("not-a-tool", Some(&testproc()))),
        Err(SettingsError::UnknownTool(_))
    ));

    let missing = tmp.path().join(if cfg!(windows) { "nope.exe" } else { "nope" });
    let error = service.apply_settings_change(tool_path("python", Some(&missing.display().to_string()))).unwrap_err();
    assert!(error.to_string().contains("does not exist"), "{error}");
    assert_eq!(error.code(), "settings.invalidToolPath");
    #[cfg(windows)]
    {
        let script = tmp.path().join("python.cmd");
        std::fs::write(&script, "@echo off").unwrap();
        let error =
            service.apply_settings_change(tool_path("python", Some(&script.display().to_string()))).unwrap_err();
        assert!(error.to_string().contains("cmd.exe"), "{error}");
    }
    assert!(service.settings().tool_paths.is_empty(), "refused overrides are not stored");

    let exe = testproc();
    let state = service.apply_settings_change(tool_path("python", Some(&exe))).unwrap();
    assert_eq!(state.settings.tool_paths.get("python"), Some(&exe));
    assert_eq!(
        service.locate_tool("python"),
        Located::Found { path: PathBuf::from(&exe), source: ToolSource::Override }
    );
    // Kernel components follow their interpreter's override.
    assert!(matches!(service.locate_tool("python-jupyter"), Located::Found { source: ToolSource::Override, .. }));
    // Persisted.
    drop(service);
    let reloaded = persistent_service(&tmp.path().join("data"), host_env());
    assert_eq!(reloaded.settings().tool_paths.get("python"), Some(&exe));
    // Clearing returns to discovery.
    let cleared = reloaded.apply_settings_change(tool_path("python", None)).unwrap();
    assert!(cleared.settings.tool_paths.is_empty());
    assert!(!matches!(reloaded.locate_tool("python"), Located::Found { source: ToolSource::Override, .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_override_invalidates_cached_discovery() {
    let tmp = tempfile::tempdir().unwrap();
    // A sparse host environment keeps discovery quick.
    let host = HostEnv::from_pairs(
        [("PATH", String::new())]
            .into_iter()
            .chain(std::env::var("SystemRoot").ok().map(|v| ("SystemRoot", v)))
            .map(|(k, v)| (k.to_string(), v)),
    );
    let service = persistent_service(tmp.path(), host);
    let before = service.toolchains().await;
    let git = before.iter().find(|t| t.id == "git").unwrap();
    assert_ne!(git.source, Some(ToolSource::Override));
    service.apply_settings_change(tool_path("git", Some(&testproc()))).unwrap();
    let after = service.toolchains().await;
    let git = after.iter().find(|t| t.id == "git").unwrap();
    assert_eq!(git.source, Some(ToolSource::Override));
    assert_eq!(git.path.as_deref(), Some(testproc().as_str()));
    assert_eq!(git.status, ToolStatus::Available);
    assert!(git.version.as_deref().is_some_and(|v| v.contains("4.12.0")), "{git:?}");
}

/// Probes run without trust, so a tool location may not be project content
/// (even in a trusted project: trust can be revoked, the override would stay)
/// or job output.
#[test]
fn tool_overrides_may_not_point_into_projects_or_job_output() {
    let f = fixture_with(ExecutionSettings::default(), true);
    let name = if cfg!(windows) { "python.exe" } else { "python" };
    let in_project = f.workspace.join("tools").join(name);
    std::fs::create_dir_all(in_project.parent().unwrap()).unwrap();
    std::fs::write(&in_project, "x").unwrap();
    let error =
        f.service.apply_settings_change(tool_path("python", Some(&in_project.display().to_string()))).unwrap_err();
    assert!(error.to_string().contains("inside the open project"), "{error}");

    let job_output = f.service.job_dir(&exposition_core::job::JobId("job-x".into())).join("out").join(name);
    std::fs::create_dir_all(job_output.parent().unwrap()).unwrap();
    std::fs::write(&job_output, "x").unwrap();
    let error =
        f.service.apply_settings_change(tool_path("python", Some(&job_output.display().to_string()))).unwrap_err();
    assert!(error.to_string().contains("execution data folder"), "{error}");
    assert!(f.service.settings().tool_paths.is_empty());

    // An installed copy outside projects and job output is fine.
    assert!(f.service.apply_settings_change(tool_path("python", Some(&testproc()))).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn docker_listing_reports_a_missing_client() {
    let tmp = tempfile::tempdir().unwrap();
    let mut settings = ExecutionSettings::default();
    settings.tool_paths.insert("docker".into(), tmp.path().join("no-docker.exe").display().to_string());
    let f = fixture_with(settings, false);
    let error = f.service.docker_images().await.unwrap_err();
    assert!(matches!(error, DockerListError::NotFound(_)), "{error:?}");
    assert_eq!(error.code(), "docker.notFound");
}

/// With `EXPOSITION_TEST_DOCKER_IMAGE` set to a local image, the listing
/// (an application probe; nothing is pulled) includes it.
#[tokio::test(flavor = "multi_thread")]
async fn docker_listing_includes_local_image() {
    let Ok(expected) = std::env::var("EXPOSITION_TEST_DOCKER_IMAGE") else {
        eprintln!("SKIPPED: set EXPOSITION_TEST_DOCKER_IMAGE to a local image");
        return;
    };
    let f = fixture_with(ExecutionSettings::default(), false);
    let images = f.service.docker_images().await.expect("docker image ls");
    assert!(images.iter().any(|i| i.reference == expected), "{expected} not in {images:?}");
    let job = f.service.list().into_iter().find(|j| j.info.label == "List local Docker images").unwrap();
    assert_eq!(job.kind, exposition_execution::JobKind::Probe);
    assert!(!job.info.args.iter().any(|a| a == "pull"));
}
