//! The execution environment of one step attempt (M9: stronger
//! reproducibility). A recorded input hash alone does not establish
//! reproducibility (spec §8), so every attempt records what it ran with:
//! runner and containment, platform, application version, the restricted
//! image (name and identity), interpreter and its packages, the Lean
//! toolchain with the Lake manifest digest, the model of an agent step, and
//! the seed a batch step was given.
//!
//! Adapters report what they know ([`crate::adapters::StepResult::environment`]);
//! the runner records the base environment of the step when an adapter
//! reports none. Capture never runs an unapproved command, never reads
//! environment variables, and never records absolute paths.

use exposition_graph::WorkflowRunId;
use exposition_graph::workflow::registry::Containment;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{WorkflowPlanStep, WorkflowRun, WorkflowRunCause};

/// The application version that runs steps.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A container image by name and identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentImage {
    /// The configured reference, e.g. `python:3.13-slim`.
    pub name: String,
    /// The engine's identity (`sha256:…`) the step ran in.
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentInterpreter {
    /// `python`, `R`, `julia`.
    pub name: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentPackage {
    pub name: String,
    /// Version, or the exact revision of a Lake dependency.
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentToolchain {
    /// E.g. `leanprover/lean4:v4.35.0`.
    pub name: String,
    /// E.g. `4.35.0`.
    pub version: Option<String>,
    /// SHA-256 (hex) of the Lake project's `lake-manifest.json`, if present.
    pub manifest_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironmentModel {
    /// `anthropic`, `scripted`.
    pub provider: String,
    /// The model that served the step (the configured one when unknown).
    pub model: String,
}

/// What one attempt ran with (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEnvironment {
    /// Runner id (`restrictedPython`, `nativePython`, `leanAudit`, …).
    pub runner: String,
    pub containment: Containment,
    /// Platform the step's code ran on (the image's for restricted steps).
    pub os: String,
    pub arch: String,
    /// The Exposition version that ran the step.
    pub app_version: String,
    #[serde(default)]
    pub image: Option<WorkflowEnvironmentImage>,
    #[serde(default)]
    pub interpreter: Option<WorkflowEnvironmentInterpreter>,
    /// Sorted by name, at most 1000 (a note says when more were captured).
    #[serde(default)]
    pub packages: Vec<WorkflowEnvironmentPackage>,
    /// SHA-256 over every captured package (`name==version` lines, sorted).
    #[serde(default)]
    pub packages_digest: Option<String>,
    #[serde(default)]
    pub toolchain: Option<WorkflowEnvironmentToolchain>,
    #[serde(default)]
    pub model: Option<WorkflowEnvironmentModel>,
    /// `EXPOSITION_SEED` as given to the step (always below 2^32).
    #[serde(default)]
    pub seed: Option<u64>,
    /// Where parts came from, and what is unknown and why.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl WorkflowEnvironment {
    /// Runner, containment, host platform, and application version.
    pub fn base(runner: &str, containment: Containment) -> Self {
        Self {
            runner: runner.to_owned(),
            containment,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            app_version: APP_VERSION.to_owned(),
            image: None,
            interpreter: None,
            packages: Vec::new(),
            packages_digest: None,
            toolchain: None,
            model: None,
            seed: None,
            notes: Vec::new(),
        }
    }

    /// The base environment of a planned step (what the runner records when
    /// the adapter reports nothing more).
    pub fn of_step(step: &WorkflowPlanStep) -> Self {
        Self::base(&step.runner, step.containment)
    }
}

/// Bound on how far a chain of reproductions is followed.
const MAX_REPRODUCTION_CHAIN: usize = 32;

/// The run whose id seeds `run`'s steps: the run itself, or — for a
/// reproduction — the original it reproduces (following reproductions of
/// reproductions), so a reproduction gives every step the seed the original
/// step received. `lookup` reads a stored run.
pub fn seed_basis(run: &WorkflowRun, lookup: &dyn Fn(&WorkflowRunId) -> Option<WorkflowRun>) -> String {
    let mut basis = run.id.clone();
    let mut cause = run.cause.clone();
    for _ in 0..MAX_REPRODUCTION_CHAIN {
        let WorkflowRunCause::Reproduce { of } = cause else { break };
        basis = of.clone();
        match lookup(&of) {
            Some(original) => cause = original.cause,
            None => break,
        }
    }
    basis.to_string()
}

#[cfg(test)]
mod hook_tests {
    //! The runner records an environment on every dispatched attempt (the
    //! adapter's, or the step's base environment), and passes the seed basis
    //! (the original run for reproductions, following chains).

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use exposition_core::path::WorkspacePath;
    use exposition_graph::workflow::presets::{NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
    use exposition_graph::workflow::registry::RESEARCH_NATIVE_PYTHON;
    use exposition_graph::workflow::{
        CodeLanguage, ExecutionCeiling, OutcomeKind, PortSource, StepOperation, StepPayload, StepPort,
        WorkflowConnection, WorkflowDefinition, WorkflowOutcome, WorkflowStep,
    };
    use exposition_graph::{
        GraphConfig, GraphService, OperationRef, Origin, SaveWorkflowRequest, SavedWorkflow, ValidationContext,
        WorkflowId,
    };

    use super::*;
    use crate::adapters::{Adapters, StepAdapter, StepControl, StepFailure, StepRequest, StepResult, runner_ids};
    use crate::model::{StartWorkflowRunRequest, WorkflowRunError};
    use crate::service::{PlanOptions, WorkflowHost, WorkflowRunConfig, WorkflowRunService};

    struct Host {
        root: PathBuf,
        graph: Arc<GraphService>,
    }

    impl WorkflowHost for Host {
        fn project_root(&self) -> Option<PathBuf> {
            Some(self.root.clone())
        }
        fn trusted(&self, _root: &Path) -> bool {
            true
        }
        fn restricted(&self) -> (Option<String>, Option<String>) {
            (None, None)
        }
        fn restricted_image_id(&self) -> Option<String> {
            None
        }
        fn workflow(
            &self,
            _root: &Path,
            id: &WorkflowId,
            revision: Option<u64>,
        ) -> Result<SavedWorkflow, WorkflowRunError> {
            self.graph.workflow(id, revision).map_err(|e| WorkflowRunError::new(&e.code, e.message))
        }
    }

    /// Reports an environment (or none) and remembers the seed basis it got.
    struct Reporting {
        report: Option<WorkflowEnvironment>,
        fail: bool,
        bases: Mutex<Vec<String>>,
    }

    impl StepAdapter for Reporting {
        fn availability(&self, _root: &Path) -> Result<(), String> {
            Ok(())
        }
        fn run(&self, request: &StepRequest<'_>, control: &StepControl<'_>) -> Result<StepResult, StepFailure> {
            self.bases.lock().unwrap().push(request.seed_basis.to_owned());
            let op = OperationRef { kind: "researchRun".into(), id: format!("op-{}", request.attempt) };
            (control.on_dispatched)(&op);
            if self.fail {
                return Err(StepFailure {
                    environment: self.report.clone(),
                    ..StepFailure::new("research.batchFailed", "failed on purpose")
                });
            }
            Ok(StepResult { operation: Some(op), environment: self.report.clone(), ..Default::default() })
        }
    }

    struct Fixture {
        _data: tempfile::TempDir,
        _project: tempfile::TempDir,
        graph: Arc<GraphService>,
        service: WorkflowRunService,
        adapter: Arc<Reporting>,
    }

    fn fixture(report: Option<WorkflowEnvironment>, fail: bool) -> Fixture {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(project.path()).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/obs.csv"), "x\n1\n").unwrap();
        let graph = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
        assert!(graph.attach(&root).is_usable());
        let adapter = Arc::new(Reporting { report, fail, bases: Mutex::new(Vec::new()) });
        let mut adapters: Adapters = BTreeMap::new();
        adapters.insert(runner_ids::NATIVE_PYTHON.into(), adapter.clone());
        let service = WorkflowRunService::new(WorkflowRunConfig {
            database: None,
            host: Arc::new(Host { root, graph: graph.clone() }),
            adapters,
            lean: None,
            research: None,
        })
        .unwrap();
        Fixture { _data: data, _project: project, graph, service, adapter }
    }

    fn definition() -> WorkflowDefinition {
        let mut def = draft(&WorkflowPresetRequest {
            preset: NUMERICAL_EXPERIMENT.into(),
            data: Some(WorkspacePath::parse("data/obs.csv").unwrap()),
            ..Default::default()
        })
        .unwrap();
        def.steps = vec![WorkflowStep {
            id: "fit".into(),
            label: "Fit".into(),
            operation: StepOperation { id: RESEARCH_NATIVE_PYTHON.into(), version: 1 },
            parameters: BTreeMap::new(),
            payload: Some(StepPayload::Code { language: CodeLanguage::Python, source: "print(1)\n".into() }),
            composite: None,
            condition: None,
            repeat: None,
        }];
        def.connections = vec![WorkflowConnection::Data {
            from: PortSource::Input { input: "data".into() },
            to: StepPort { step: "fit".into(), port: "data".into() },
        }];
        def.ceiling.execution = ExecutionCeiling::TrustedNative;
        def.outcomes = vec![WorkflowOutcome {
            id: "o-fit".into(),
            kind: OutcomeKind::TransformedDataset,
            label: "Dataset".into(),
            from: StepPort { step: "fit".into(), port: "dataset".into() },
            optional: false,
        }];
        def
    }

    impl Fixture {
        fn save(&self) -> WorkflowId {
            self.graph
                .save_workflow(
                    &Origin::User,
                    &SaveWorkflowRequest { id: None, expected_revision: None, definition: definition() },
                    &ValidationContext::default(),
                )
                .unwrap()
                .id
        }

        fn run(&self, id: &WorkflowId, cause: WorkflowRunCause) -> WorkflowRun {
            let plan = self.service.plan_with(id, None, PlanOptions { cause, ..Default::default() }).unwrap();
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            let run = self
                .service
                .start(&StartWorkflowRunRequest { plan: plan.plan, approval_hash: plan.approval_hash })
                .unwrap();
            assert!(self.service.wait_idle(Duration::from_secs(20)));
            self.service.get(&run.id).unwrap()
        }
    }

    fn attempt_environment(run: &WorkflowRun) -> Option<WorkflowEnvironment> {
        run.steps[0].attempts.last().unwrap().environment.clone()
    }

    #[test]
    fn an_attempt_records_the_environment_its_adapter_reported() {
        let mut reported = WorkflowEnvironment::base(runner_ids::NATIVE_PYTHON, Containment::NativeAccount);
        reported.seed = Some(7);
        reported.notes.push("from the adapter".into());
        for fail in [false, true] {
            let f = fixture(Some(reported.clone()), fail);
            let run = f.run(&f.save(), WorkflowRunCause::Manual);
            assert_eq!(attempt_environment(&run).as_ref(), Some(&reported), "fail={fail}");
        }
    }

    #[test]
    fn an_attempt_without_a_reported_environment_records_the_step_base() {
        for fail in [false, true] {
            let f = fixture(None, fail);
            let run = f.run(&f.save(), WorkflowRunCause::Manual);
            let env = attempt_environment(&run).expect("the base environment is recorded");
            assert_eq!(env, WorkflowEnvironment::of_step(&run.plan.steps[0]), "fail={fail}");
            assert_eq!((env.runner.as_str(), env.containment), (runner_ids::NATIVE_PYTHON, Containment::NativeAccount));
            assert_eq!((env.os.as_str(), env.app_version.as_str()), (std::env::consts::OS, APP_VERSION));
        }
    }

    #[test]
    fn reproductions_are_seeded_from_the_original_run_following_chains() {
        let f = fixture(None, false);
        let id = f.save();
        let original = f.run(&id, WorkflowRunCause::Manual);
        let copy = f.run(&id, WorkflowRunCause::Reproduce { of: original.id.clone() });
        let copy_of_copy = f.run(&id, WorkflowRunCause::Reproduce { of: copy.id.clone() });
        let bases = f.adapter.bases.lock().unwrap().clone();
        let o = original.id.to_string();
        assert_eq!(bases, vec![o.clone(), o.clone(), o], "every reproduction is seeded from the original");

        let lookup = |_: &WorkflowRunId| None;
        assert_eq!(seed_basis(&copy_of_copy, &lookup), copy.id.to_string(), "an unknown original ends the chain");
        assert_eq!(seed_basis(&original, &lookup), original.id.to_string());
    }
}
