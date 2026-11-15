//! The research environment chosen for each project, remembered app-locally
//! (`<data>/research/selections.json`, keyed by the canonical project root;
//! never written into a project). A remembered id that discovery no longer
//! reports is simply not used: callers fall back to the first usable one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::discovery::now_ms;
use crate::error::ResearchError;
use crate::service::{ResearchService, display_path};

const FILE: &str = "selections.json";
const FORMAT: &str = "exposition.researchSelections/1";
/// Projects remembered (the least recently chosen are forgotten first).
const MAX_PROJECTS: usize = 200;
const MAX_ID_LEN: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Selection {
    environment: String,
    updated_at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Selections {
    format: String,
    #[serde(default)]
    projects: BTreeMap<String, Selection>,
}

pub(crate) struct SelectionStore {
    path: PathBuf,
    lock: Mutex<()>,
}

fn key(root: &Path) -> String {
    let text = display_path(root);
    if cfg!(windows) { text.to_lowercase() } else { text }
}

impl SelectionStore {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self { path: data_dir.join("research").join(FILE), lock: Mutex::new(()) }
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn read(&self) -> Selections {
        let Ok(bytes) = std::fs::read(&self.path) else { return Selections::default() };
        match serde_json::from_slice::<Selections>(&bytes) {
            Ok(selections) if selections.format == FORMAT => selections,
            _ => {
                // Unreadable: keep the original aside and start empty.
                let _ = std::fs::rename(&self.path, self.path.with_extension(format!("invalid-{}", now_ms())));
                Selections::default()
            }
        }
    }

    pub(crate) fn get(&self, root: &Path) -> Option<String> {
        let _guard = self.guard();
        self.read().projects.get(&key(root)).map(|s| s.environment.clone())
    }

    pub(crate) fn set(&self, root: &Path, environment: Option<&str>) -> std::io::Result<()> {
        let _guard = self.guard();
        let mut selections = self.read();
        selections.format = FORMAT.into();
        match environment {
            Some(id) => {
                selections
                    .projects
                    .insert(key(root), Selection { environment: id.to_owned(), updated_at_ms: now_ms() });
            }
            None => {
                selections.projects.remove(&key(root));
            }
        }
        while selections.projects.len() > MAX_PROJECTS {
            let oldest = selections.projects.iter().min_by_key(|(_, s)| s.updated_at_ms).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => selections.projects.remove(&k),
                None => break,
            };
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(&selections).map_err(std::io::Error::other)?;
        let partial = self.path.with_extension(format!("partial-{}", std::process::id()));
        std::fs::write(&partial, bytes)?;
        std::fs::rename(&partial, &self.path).inspect_err(|_| {
            let _ = std::fs::remove_file(&partial);
        })
    }
}

impl ResearchService {
    /// The environment remembered for the open project, if any. It may no
    /// longer be discovered; callers then fall back.
    pub fn selected_environment(&self) -> Result<Option<String>, ResearchError> {
        let root = self.workspace_root().ok_or(ResearchError::NoWorkspace)?;
        Ok(self.selections().get(&root))
    }

    /// Remember (or forget, with `None`) the environment chosen for the open
    /// project. Only an environment discovery reported can be remembered.
    pub async fn select_environment(&self, environment: Option<String>) -> Result<(), ResearchError> {
        let root = self.workspace_root().ok_or(ResearchError::NoWorkspace)?;
        if let Some(id) = &environment {
            if id.is_empty() || id.len() > MAX_ID_LEN || id.chars().any(char::is_control) {
                return Err(ResearchError::InvalidRequest("invalid environment id".into()));
            }
            let report = self.environments(false).await?;
            if !report.runtimes.iter().any(|r| &r.id == id) {
                return Err(ResearchError::UnknownEnvironment(id.clone()));
            }
        }
        self.selections()
            .set(&root, environment.as_deref())
            .map_err(|e| ResearchError::io("remembering the research environment", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_are_per_project_and_survive_a_new_store() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let store = SelectionStore::new(dir.path());
        store.set(&a, Some("python:one")).unwrap();
        store.set(&b, Some("r:ir")).unwrap();
        let again = SelectionStore::new(dir.path());
        assert_eq!(again.get(&a).as_deref(), Some("python:one"));
        assert_eq!(again.get(&b).as_deref(), Some("r:ir"));
        again.set(&a, None).unwrap();
        assert_eq!(again.get(&a), None);
        assert_eq!(again.get(&b).as_deref(), Some("r:ir"));
    }

    #[test]
    fn an_unreadable_file_is_kept_aside_and_nothing_is_selected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("research")).unwrap();
        std::fs::write(dir.path().join("research").join(FILE), b"{not json").unwrap();
        let store = SelectionStore::new(dir.path());
        assert_eq!(store.get(dir.path()), None);
        let kept = std::fs::read_dir(dir.path().join("research"))
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("invalid-"));
        assert!(kept, "the unreadable file is kept aside");
    }

    #[test]
    fn at_most_the_newest_projects_are_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let store = SelectionStore::new(dir.path());
        for i in 0..(MAX_PROJECTS + 5) {
            store.set(&dir.path().join(format!("p{i}")), Some("python:x")).unwrap();
        }
        let count = store.read().projects.len();
        assert_eq!(count, MAX_PROJECTS);
    }
}
