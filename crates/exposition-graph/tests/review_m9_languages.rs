//! R-m9 (independent review of M9, lane F): script-language and batch
//! parameter rules at validation time, through the public validator.
//!
//! Product-failing reproductions are `#[ignore = "R-m9 Fn: ..."]`; run them
//! with `cargo test -p exposition-graph --test review_m9_languages -- --ignored`.

use std::collections::{BTreeMap, HashMap};

use exposition_core::path::WorkspacePath;
use exposition_graph::workflow::{
    AuthorityCeiling, CompositeRef, DraftState, InputSource, InputVersion, OutcomeKind, ParamValue, PortSource,
    PortType, StepOperation, StepPort, ValidationContext, WORKFLOW_SCHEMA, WORKFLOW_SCHEMA_V2, WorkflowConnection,
    WorkflowDefinition, WorkflowInput, WorkflowOutcome, WorkflowStep,
};
use exposition_graph::{DiagnosticSeverity, WorkflowId, validate_workflow};

fn code_input(id: &str, path: &str) -> WorkflowInput {
    WorkflowInput {
        id: id.into(),
        label: format!("Code {id}"),
        port_type: PortType::CodeSource,
        source: InputSource::Resource { path: WorkspacePath::parse(path).unwrap(), version: InputVersion::AtRunStart },
    }
}

fn step(id: &str, operation: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Step {id}"),
        operation: StepOperation { id: operation.into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
        composite: None,
        condition: None,
        repeat: None,
    }
}

fn to(from: PortSource, step: &str, port: &str) -> WorkflowConnection {
    WorkflowConnection::Data { from, to: StepPort { step: step.into(), port: port.into() } }
}

fn input(id: &str) -> PortSource {
    PortSource::Input { input: id.into() }
}

fn outcome(id: &str, step: &str, port: &str) -> WorkflowOutcome {
    WorkflowOutcome {
        id: id.into(),
        kind: OutcomeKind::TransformedDataset,
        label: format!("Outcome {id}"),
        from: StepPort { step: step.into(), port: port.into() },
        optional: false,
    }
}

/// A native R step running the script input `script` (`path`).
fn r_workflow(path: &str) -> WorkflowDefinition {
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA.into(),
        title: "R analysis".into(),
        description: String::new(),
        preset: None,
        inputs: vec![code_input("script", path)],
        steps: vec![step("fit", "research.nativeR")],
        connections: vec![to(input("script"), "fit", "script")],
        outcomes: vec![outcome("fitted", "fit", "dataset")],
        ceiling: AuthorityCeiling::ALL,
        execution: None,
    }
}

fn errors(
    def: &WorkflowDefinition,
    store: &HashMap<(WorkflowId, u64), WorkflowDefinition>,
) -> (DraftState, Vec<String>) {
    let resolve = |w: &WorkflowId, r: u64| store.get(&(w.clone(), r)).cloned();
    let v = validate_workflow(
        def,
        &ValidationContext { ceiling: None, file_exists: None, resolve_workflow: Some(&resolve), workflow_id: None },
    );
    let codes =
        v.diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Error).map(|d| d.code.clone()).collect();
    (v.state, codes)
}

/// Held (control): a `.py` script bound directly to an R step is refused.
#[test]
fn review_m9_held_a_python_script_never_reaches_an_r_step_directly() {
    let store = HashMap::new();
    assert_eq!(errors(&r_workflow("analysis.R"), &store).0, DraftState::WellFormed);
    let (state, codes) = errors(&r_workflow("experiment.py"), &store);
    assert_eq!(state, DraftState::Incomplete);
    assert!(codes.contains(&"workflow.inputTypeMismatch".to_owned()), "{codes:?}");
}

/// R-m9 F6 (composite half): the script-language rule is checked only on a
/// direct input → step connection. Through a composite the parent binds the
/// child's `script` port to a `.py` file, the composite contract has no
/// language, and the child is validated with its inputs unset, so the parent
/// is well-formed (and runnable) although its R step will refuse the file
/// when it is dispatched.
#[test]
fn review_m9_f6_a_python_script_never_reaches_an_r_step_through_a_composite() {
    let mut store = HashMap::new();
    let child = WorkflowId::mint();
    store.insert((child.clone(), 1), r_workflow("analysis.R"));
    let parent = WorkflowDefinition {
        schema: WORKFLOW_SCHEMA_V2.into(),
        title: "Parent".into(),
        description: String::new(),
        preset: None,
        inputs: vec![code_input("code", "experiment.py")],
        steps: vec![WorkflowStep {
            composite: Some(CompositeRef { workflow: child, revision: 1 }),
            ..step("c", "workflow.composite")
        }],
        connections: vec![to(input("code"), "c", "script")],
        outcomes: vec![outcome("o", "c", "fitted")],
        ceiling: AuthorityCeiling::ALL,
        execution: None,
    };
    let (state, codes) = errors(&parent, &store);
    eprintln!("parent with experiment.py → R step through a composite: {state:?} {codes:?}");
    assert_eq!(state, DraftState::Incomplete, "a .py file bound to an R step's script must be refused");
}

/// R-m9 F6 (parameter half): a batch's `value` is passed as one environment
/// line, and the batch runner refuses a value with a line break before
/// anything runs; validation accepts it, so the draft is well-formed and
/// runnable and the plan shows no blocker.
#[test]
fn review_m9_f6_validation_refuses_a_batch_value_the_runner_refuses() {
    let mut def = r_workflow("analysis.R");
    def.steps[0].parameters.insert("value".into(), ParamValue::Text("first line\nsecond line".into()));
    let (state, codes) = errors(&def, &HashMap::new());
    eprintln!("value with a line break: {state:?} {codes:?}");
    assert_eq!(state, DraftState::Incomplete, "the value can never reach the batch");
}
