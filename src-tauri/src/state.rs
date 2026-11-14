use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use exposition_agents::credentials::{KeyringStore, ObservedStore, PROVIDER_KEY_SECRET, SecretStore};
use exposition_agents::tools::execution::AppToolServices;
use exposition_agents::{AgentConfig, AgentService};
use exposition_core::path::WorkspacePath;
use exposition_execution::{ExecutionConfig, ExecutionService};
use exposition_graph::reconcile::ReconcilerHandle;
use exposition_graph::{GraphConfig, GraphService};
use exposition_lean::{LeanConfig, LeanService};
use exposition_research::launcher::ExecutionLauncher;
use exposition_research::{ResearchConfig, ResearchProject, ResearchService, ResolvedSource, SourceReader};
use exposition_tex::{TexService, TexServiceConfig};
use exposition_typesetting::{Fonts, TypesettingService};
use exposition_workflow::WorkflowRunService;
use exposition_workspace::{DocumentService, WorkspaceService};
use tauri::{AppHandle, Manager};

use crate::typesetting::TypesettingRuntime;

/// Application-wide services. One instance is managed by Tauri.
pub struct AppState {
    pub documents: Arc<DocumentService>,
    /// Open folder, files, saves, recovery, watcher, and settings.
    pub workspace: Arc<WorkspaceService>,
    /// Supervised process execution (jobs, toolchains, trust).
    pub execution: ExecutionService,
    /// Lean language servers and per-revision checking status.
    pub lean: LeanService,
    /// Research sessions, runs, and provenance (M4). Launches through `execution`.
    pub research: Arc<ResearchService>,
    /// LaTeX/LuaLaTeX builds (through `execution`) and their pages.
    pub tex: TexService,
    /// Typesetting builds, page maps, and rendering (Typst).
    pub typesetting: Arc<TypesettingRuntime>,
    /// Application data directory (settings, recovery, session metadata).
    pub data_dir: PathBuf,
    /// Agent tasks (provider, task loop, review); task history in `agents.sqlite3`.
    pub agents: AgentService,
    /// The project graph (`.exposition/graph.sqlite3`): attached for every
    /// open project, whether or not a graph view is open.
    pub graph: Arc<GraphService>,
    /// Research in the project graph: research space, retained outcomes,
    /// experiments, reviewed promotion into manuscripts.
    pub research_project: Arc<ResearchProject>,
    /// Keeps the graph reconciled with the project's files and open buffers
    /// (its own thread, subscribed to workspace/document/graph events).
    pub reconciler: ReconcilerHandle,
    /// Workflow runs: reviewed plans, the bounded scheduler (a run's
    /// `maxParallel`, application-wide limits, reserved budgets), durable run
    /// records in `workflow-runs.sqlite3` (interrupted runs never resume).
    pub workflow_runs: WorkflowRunService,
    /// Explicit, user-only workflow triggers (M9): standing plans the user
    /// enabled, in `workflow-triggers.json` (app-local, never in a project).
    pub workflow_triggers: exposition_workflow::triggers::TriggerService,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = resolve_data_dir(app)?;
        std::fs::create_dir_all(&data_dir)?;
        let documents = Arc::new(DocumentService::new());
        let mut execution_config = ExecutionConfig::new(&data_dir);
        // Test/dev override so isolated data dirs can share the extraction cache
        // of self-extracting tools (e.g. biber) instead of re-extracting.
        if let Some(dir) = std::env::var_os("EXPOSITION_TOOL_CACHE_DIR").filter(|v| !v.is_empty()) {
            execution_config.tool_cache_dir = Some(PathBuf::from(dir));
        }
        let execution = ExecutionService::with_config(execution_config)?;
        // Remove restricted containers an earlier session left behind (e.g. the
        // app was killed while one ran); the first restricted job waits for it.
        let sweeper = execution.clone();
        std::thread::spawn(move || {
            if let Err(e) = sweeper.sweep_stale_containers() {
                eprintln!("could not remove containers left by an earlier session: {e}");
            }
        });
        let workspace = WorkspaceService::new(Arc::clone(&documents), data_dir.clone());
        let graph = GraphService::new(GraphConfig { data_dir: data_dir.clone() });
        let reconciler = exposition_graph::reconcile::spawn(Arc::clone(&graph), Arc::clone(&workspace));
        let (lean, lean_actor) = LeanService::new(execution.clone(), Arc::clone(&workspace), LeanConfig::default());
        tauri::async_runtime::spawn(lean_actor);
        let tex = TexService::new(
            execution.clone(),
            Arc::clone(&workspace),
            tauri::async_runtime::handle().inner().clone(),
            TexServiceConfig::new(&data_dir),
        )?;
        let typesetting =
            TypesettingRuntime::new(Arc::new(TypesettingService::new(Arc::clone(&workspace), Fonts::with_system)));
        // Discover fonts in the background so the first build does not wait for it.
        std::thread::spawn(|| {
            Fonts::with_system();
        });
        let mut research_config = ResearchConfig::new(
            data_dir.clone(),
            Arc::new(ExecutionLauncher::new(execution.clone())),
            documents.clone(),
        );
        research_config.source_reader = Some(Arc::new(WorkspaceSources(Arc::clone(&workspace))));
        let research = Arc::new(ResearchService::new(research_config));
        // The stored provider key is redacted from every job, kernel, log, and
        // artifact by its literal value (whatever its shape) once it is known:
        // loaded now, and again whenever it is read, saved, or cleared.
        let credentials: Arc<dyn SecretStore> = {
            let redactor = execution.clone();
            Arc::new(ObservedStore::new(
                KeyringStore::new(credential_namespace().as_deref()),
                Box::new(move |key| redactor.register_secret(PROVIDER_KEY_SECRET, key)),
            ))
        };
        {
            let credentials = Arc::clone(&credentials);
            std::thread::spawn(move || {
                let _ = credentials.get();
            });
        }
        // Research ↔ project graph (retained artifacts, reviewed promotions);
        // agent insertions are recorded through it too.
        let research_project =
            Arc::new(ResearchProject::new(Arc::clone(&graph), Arc::clone(&research), Arc::clone(&documents)));
        let agents = AgentService::new(AgentConfig {
            database: Some(data_dir.join("agents.sqlite3")),
            documents: Arc::clone(&documents),
            workspace: Some(Arc::clone(&workspace)),
            credentials: credentials.clone(),
            // Agent tools call the same services people use: Typst and LaTeX
            // builds, research sessions (agent-owned), Lean checks, and
            // execution jobs.
            services: Arc::new(
                AppToolServices::new(execution.clone())
                    .with_workspace(Arc::clone(&workspace))
                    .with_typesetting(Arc::clone(&typesetting.service))
                    .with_tex(tex.clone())
                    .with_research(Arc::clone(&research))
                    .with_research_project(Arc::clone(&research_project))
                    .with_lean(lean.clone()),
            ),
            runtime: tauri::async_runtime::handle().inner().clone(),
            allow_scripted: cfg!(debug_assertions),
            anthropic_base_url: None,
            retry: None,
            // Every project task gets a scoped graph context and resolves its
            // targets through the project graph.
            graph: Some(Arc::clone(&graph)),
        })?;
        // Workflow runs. The research batch runner (restricted Docker and
        // trusted-native Python batches) is wired here; without one, Python
        // workflow steps report themselves unavailable (never downgraded).
        exposition_research::sandbox::remove_stale_batches(&research);
        let batch_runner: Option<Arc<dyn exposition_research::batch::BatchRunner>> =
            Some(Arc::new(SyncedBatchRunner {
                inner: exposition_research::sandbox::batch_runner(Arc::clone(&research), execution.clone()),
                research: Arc::clone(&research),
                availability: Mutex::new(HashMap::new()),
            }));
        let workflow_runs = crate::workflow_runs::build(
            &data_dir,
            crate::workflow_runs::RunServices {
                workspace: Arc::clone(&workspace),
                documents: Arc::clone(&documents),
                graph: Arc::clone(&graph),
                execution: execution.clone(),
                lean: lean.clone(),
                research_project: Arc::clone(&research_project),
                agents: agents.clone(),
                batch: batch_runner,
                runtime: tauri::async_runtime::handle().inner().clone(),
            },
        )?;
        let workflow_triggers = crate::workflow_triggers::build(
            &data_dir,
            workflow_runs.clone(),
            Arc::new(crate::workflow_runs::AppWorkflowHost {
                workspace: Arc::clone(&workspace),
                graph: Arc::clone(&graph),
                execution: execution.clone(),
            }),
        );
        Ok(Self {
            documents,
            workspace,
            execution,
            lean,
            research,
            tex,
            typesetting,
            data_dir,
            agents,
            graph,
            research_project,
            reconciler,
            workflow_runs,
            workflow_triggers,
        })
    }
}

/// `EXPOSITION_DATA_DIR` overrides the platform application data directory so
/// tests and side-by-side profiles never touch the user's real state.
fn resolve_data_dir(app: &AppHandle) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dir) = std::env::var_os("EXPOSITION_DATA_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    Ok(app.path().app_local_data_dir()?)
}

/// Credential entry namespace of development builds that did not ask for one.
const DEVELOPMENT_NAMESPACE: &str = "development";
/// Set to `1` to let a development build use the real credential entry (only
/// the live provider scenarios do).
const REAL_CREDENTIAL_OPT_IN: &str = "EXPOSITION_USE_REAL_CREDENTIAL";

/// Which credential entry this process uses. Release builds always use the
/// real entry and ignore both variables. Development builds never read the
/// user's real key unless explicitly told to: they use
/// `EXPOSITION_CREDENTIAL_NAMESPACE` when set (e2e runs), else the real entry
/// only with `EXPOSITION_USE_REAL_CREDENTIAL=1`, else a development entry.
fn credential_namespace() -> Option<String> {
    #[cfg(debug_assertions)]
    {
        namespace_for(|name| std::env::var(name).ok())
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

#[cfg_attr(not(any(debug_assertions, test)), allow(dead_code))]
fn namespace_for(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    let requested = var("EXPOSITION_CREDENTIAL_NAMESPACE")
        .filter(|ns| !ns.is_empty() && ns.len() <= 64 && ns.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if requested.is_some() {
        return requested;
    }
    if var(REAL_CREDENTIAL_OPT_IN).as_deref() == Some("1") {
        return None;
    }
    Some(DEVELOPMENT_NAMESPACE.to_owned())
}

/// Research reads run inputs through the workspace snapshot, like builds do.
struct WorkspaceSources(Arc<WorkspaceService>);

impl SourceReader for WorkspaceSources {
    fn read(&self, path: &WorkspacePath) -> Result<ResolvedSource, String> {
        let snapshot = self.0.snapshot().map_err(|e| e.to_string())?;
        let input = snapshot.read(path).map_err(|e| e.to_string())?;
        let text = input.text.ok_or_else(|| format!("{path} is not a text file"))?;
        Ok(ResolvedSource {
            text: text.to_string(),
            revision: input.revision,
            content_hash: input.content_hash,
            root: snapshot.root().to_path_buf(),
        })
    }
}

/// A cached availability answer per project root, batch language, and mode.
type AvailabilityKey = (PathBuf, exposition_research::batch::BatchLanguage, exposition_research::batch::BatchMode);
type AvailabilityAnswer = (std::time::Instant, Result<(), String>);

/// The research batch runner as the workflow runner uses it: the research
/// service is pointed at the run's project before each call (the same rule
/// as the research commands), and availability answers (which may ask Docker
/// whether the image is present) are reused for a few seconds, because drafts
/// are re-validated as they are edited. Python, R, and Julia batches (M9)
/// all go through it.
struct SyncedBatchRunner {
    inner: Arc<dyn exposition_research::batch::BatchRunner>,
    research: Arc<ResearchService>,
    availability: Mutex<HashMap<AvailabilityKey, AvailabilityAnswer>>,
}

const BATCH_AVAILABILITY_TTL: std::time::Duration = std::time::Duration::from_secs(5);

impl SyncedBatchRunner {
    fn sync(&self, root: &std::path::Path) -> Result<(), exposition_research::ResearchError> {
        let canonical = std::fs::canonicalize(root).ok();
        let current = self.research.workspace_root();
        if canonical.is_none() || current != canonical {
            self.research.set_workspace(root, false)?;
        }
        Ok(())
    }
}

impl exposition_research::batch::BatchRunner for SyncedBatchRunner {
    fn availability(&self, root: &std::path::Path, mode: exposition_research::batch::BatchMode) -> Result<(), String> {
        self.language_availability(root, exposition_research::batch::BatchLanguage::Python, mode)
    }

    fn run(
        &self,
        root: &std::path::Path,
        request: &exposition_research::batch::BatchRequest,
        control: &exposition_research::batch::BatchControl<'_>,
    ) -> Result<exposition_research::batch::BatchResult, exposition_research::ResearchError> {
        self.run_language(root, exposition_research::batch::BatchLanguage::Python, request, control)
    }

    fn language_availability(
        &self,
        root: &std::path::Path,
        language: exposition_research::batch::BatchLanguage,
        mode: exposition_research::batch::BatchMode,
    ) -> Result<(), String> {
        let key = (root.to_path_buf(), language, mode);
        if let Some((at, answer)) = self.availability.lock().unwrap_or_else(|p| p.into_inner()).get(&key)
            && at.elapsed() < BATCH_AVAILABILITY_TTL
        {
            return answer.clone();
        }
        let answer = self
            .sync(root)
            .map_err(|e| e.to_string())
            .and_then(|()| self.inner.language_availability(root, language, mode));
        let mut cache = self.availability.lock().unwrap_or_else(|p| p.into_inner());
        cache.retain(|_, (at, _)| at.elapsed() < BATCH_AVAILABILITY_TTL);
        cache.insert(key, (std::time::Instant::now(), answer.clone()));
        answer
    }

    fn run_language(
        &self,
        root: &std::path::Path,
        language: exposition_research::batch::BatchLanguage,
        request: &exposition_research::batch::BatchRequest,
        control: &exposition_research::batch::BatchControl<'_>,
    ) -> Result<exposition_research::batch::BatchResult, exposition_research::ResearchError> {
        self.sync(root)?;
        // A run re-checks everything itself; never answer it from the cache.
        self.availability.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.inner.run_language(root, language, request, control)
    }

    fn restricted_image(
        &self,
        language: exposition_research::batch::BatchLanguage,
    ) -> Result<(String, Option<String>), String> {
        self.inner.restricted_image(language)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A development build started without credential variables (`npm run
    /// dev`, or an e2e run whose scenario sets no namespace) must not read the
    /// user's real key.
    #[cfg(debug_assertions)]
    #[test]
    fn development_builds_use_a_development_entry_by_default() {
        let set = |name: &str| std::env::var_os(name).is_some();
        if set("EXPOSITION_CREDENTIAL_NAMESPACE") || set("EXPOSITION_USE_REAL_CREDENTIAL") {
            eprintln!("SKIPPED: a credential variable is set in this test's environment");
            return;
        }
        assert_eq!(credential_namespace().as_deref(), Some("development"));
    }

    #[test]
    fn namespace_resolution() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned())
        };
        assert_eq!(namespace_for(env(&[])).as_deref(), Some(DEVELOPMENT_NAMESPACE));
        assert_eq!(namespace_for(env(&[("EXPOSITION_USE_REAL_CREDENTIAL", "1")])), None, "explicit opt-in");
        assert_eq!(
            namespace_for(env(&[("EXPOSITION_USE_REAL_CREDENTIAL", "yes")])).as_deref(),
            Some(DEVELOPMENT_NAMESPACE)
        );
        assert_eq!(
            namespace_for(env(&[
                ("EXPOSITION_CREDENTIAL_NAMESPACE", "e2e-1"),
                ("EXPOSITION_USE_REAL_CREDENTIAL", "1")
            ]))
            .as_deref(),
            Some("e2e-1"),
            "a namespace wins over the opt-in"
        );
        assert_eq!(
            namespace_for(env(&[("EXPOSITION_CREDENTIAL_NAMESPACE", "bad ns!")])).as_deref(),
            Some(DEVELOPMENT_NAMESPACE),
            "an invalid namespace never falls back to the real entry"
        );
    }
}
