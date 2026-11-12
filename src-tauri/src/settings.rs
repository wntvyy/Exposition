//! Settings commands: execution settings (validated, narrow changes), local
//! Docker images for the restricted-execution image picker, and the trusted
//! folder list.
//!
//! Nothing here can grant trust (only `trust_grant`, called from the explicit
//! trust dialog, does) or run an arbitrary program: the image listing is a
//! fixed application probe in the execution service.

use exposition_execution::{
    DockerImage, DockerListError, ExecutionSettingsChange, ExecutionSettingsState, SettingsError, TrustRules,
    TrustedRoot,
};
use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::CommandError;
use crate::state::AppState;

/// Trusted folders and the rules for what may be trusted.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TrustList {
    pub roots: Vec<TrustedRoot>,
    pub rules: TrustRules,
}

fn settings_error(error: SettingsError) -> CommandError {
    CommandError::new(error.code(), error.to_string())
}

/// Boundary checks before the service's own validation.
fn validate_change(change: &ExecutionSettingsChange) -> Result<(), CommandError> {
    let bad = |m: &str| Err(CommandError::new("settings.invalidRequest", m));
    match change {
        ExecutionSettingsChange::DockerImage { image } => {
            if image.as_ref().is_some_and(|i| i.len() > 512 || i.contains('\0')) {
                return bad("invalid image reference");
            }
        }
        ExecutionSettingsChange::ToolPath { tool, path } => {
            if tool.is_empty() || tool.len() > 64 || !tool.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return bad("invalid tool id");
            }
            if path.as_ref().is_some_and(|p| p.len() > 4096 || p.contains('\0')) {
                return bad("invalid path");
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn execution_settings_get(state: State<'_, AppState>) -> Result<ExecutionSettingsState, CommandError> {
    Ok(state.execution.settings_state())
}

/// Apply one validated change (Docker image or tool location).
#[tauri::command]
pub async fn execution_settings_update(
    state: State<'_, AppState>,
    change: ExecutionSettingsChange,
) -> Result<ExecutionSettingsState, CommandError> {
    validate_change(&change)?;
    let execution = state.execution.clone();
    // Validation touches the filesystem; keep it off the async runtime threads.
    tauri::async_runtime::spawn_blocking(move || execution.apply_settings_change(change))
        .await
        .map_err(|e| CommandError::new("settings.io", e.to_string()))?
        .map_err(settings_error)
}

/// Locally present, tagged images (`docker image ls`; never pulls).
#[tauri::command]
pub async fn docker_images_list(state: State<'_, AppState>) -> Result<Vec<DockerImage>, CommandError> {
    state.execution.docker_images().await.map_err(|e: DockerListError| CommandError::new(e.code(), e.to_string()))
}

#[tauri::command]
pub async fn trust_list(state: State<'_, AppState>) -> Result<TrustList, CommandError> {
    Ok(TrustList { roots: state.execution.trusted_roots(), rules: state.execution.trust_rules() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_validation() {
        let tool =
            |t: &str, p: Option<&str>| ExecutionSettingsChange::ToolPath { tool: t.into(), path: p.map(Into::into) };
        assert!(validate_change(&tool("python", Some("C:/x/python.exe"))).is_ok());
        assert!(validate_change(&tool("python", None)).is_ok());
        assert!(validate_change(&tool("", None)).is_err());
        assert!(validate_change(&tool("../python", None)).is_err());
        assert!(validate_change(&tool("python", Some("a\0b"))).is_err());
        assert!(validate_change(&tool("python", Some(&"x".repeat(5000)))).is_err());
        let image = |i: &str| ExecutionSettingsChange::DockerImage { image: Some(i.into()) };
        assert!(validate_change(&image("python:3.13-slim")).is_ok());
        assert!(validate_change(&image(&"a".repeat(600))).is_err());
    }
}
