//! R-m9 (independent review of M9): composite authority, recursion, depth,
//! size, pinning, and expansion, exercised through the public validator and
//! expander with an in-memory resolver (what the run service and the editor
//! pass). Everything here held.

use std::collections::{BTreeMap, HashMap};

use exposition_core::path::WorkspacePath;
use exposition_graph::WorkflowId;
use exposition_graph::validate_workflow;
use exposition_graph::workflow::{
    AuthorityCeiling, CodeLanguage, CompositeRef, DraftState, ExecutionCeiling, InputSource, InputVersion, OutcomeKind,
    PortSource, PortType, StepOperation, StepPayload, StepPort, ValidationContext, WORKFLOW_SCHEMA, WORKFLOW_SCHEMA_V2,
    WorkflowConnection, WorkflowDefinition, WorkflowInput, WorkflowOutcome, WorkflowStep, expand_workflow,
};

const RESTRICTED: AuthorityCeiling = AuthorityCeiling {
    execution: ExecutionCeiling::Restricted,
    model_requests: false,
    retain_results: false,
    propose_changes: false,
};

fn data_input(id: &str) -> WorkflowInput {
    WorkflowInput {
        id: id.into(),
        label: format!("Data {id}"),
        port_type: PortType::Dataset,
        source: InputSource::Resource {
            path: WorkspacePath::parse("data/obs.csv").unwrap(),
            version: InputVersion::AtRunStart,
        },
    }
}

fn python(id: &str, native: bool, code: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Python {id}"),
        operation: StepOperation {
            id: if native { "research.nativePython".into() } else { "research.pythonBatch".into() },
            version: 1,
        },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Code { language: CodeLanguage::Python, source: code.into() }),
        composite: None,
        condition: None,
        repeat: None,
    }
}

fn composite(id: &str, reference: &CompositeRef) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Composite {id}"),
        operation: StepOperation { id: "workflow.composite".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
        composite: Some(reference.clone()),
        condition: None,
        repeat: None,
    }
}

fn data(from: PortSource, step: &str, port: &str) -> WorkflowConnection {
    WorkflowConnection::Data { from, to: StepPort { step: step.into(), port: port.into() } }
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

/// A child: `obs` → a chain of `n` Python steps → outcome `fitted`.
fn child(n: usize, native: bool, code: &str, ceiling: AuthorityCeiling) -> WorkflowDefinition {
    let steps: Vec<WorkflowStep> = (0..n).map(|i| python(&format!("s{i}"), native, code)).collect();
    let mut connections = vec![data(PortSource::Input { input: "obs".into() }, "s0", "data")];
    for i in 1..n {
        connections.push(data(
            PortSource::Step { step: format!("s{}", i - 1), port: "dataset".into() },
            &format!("s{i}"),
            "data",
        ));
    }
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA.into(),
        title: "Child".into(),
        description: String::new(),
        preset: None,
        inputs: vec![data_input("obs")],
        steps,
        connections,
        outcomes: vec![outcome("fitted", &format!("s{}", n - 1), "dataset")],
        ceiling,
        execution: None,
    }
}

/// A parent running each of `references` as a composite step on its `data` input.
fn parent(references: &[CompositeRef], ceiling: AuthorityCeiling) -> WorkflowDefinition {
    let mut steps = Vec::new();
    let mut connections = Vec::new();
    let mut outcomes = Vec::new();
    for (i, r) in references.iter().enumerate() {
        let id = format!("c{i}");
        steps.push(composite(&id, r));
        connections.push(data(PortSource::Input { input: "data".into() }, &id, "obs"));
        outcomes.push(outcome(&format!("o{i}"), &id, "fitted"));
    }
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA_V2.into(),
        title: "Parent".into(),
        description: String::new(),
        preset: None,
        inputs: vec![data_input("data")],
        steps,
        connections,
        outcomes,
        ceiling,
        execution: None,
    }
}

#[derive(Default)]
struct Store(HashMap<(WorkflowId, u64), WorkflowDefinition>);

impl Store {
    fn put(&mut self, id: &WorkflowId, revision: u64, def: WorkflowDefinition) -> CompositeRef {
        self.0.insert((id.clone(), revision), def);
        CompositeRef { workflow: id.clone(), revision }
    }
    fn validate(
        &self,
        def: &WorkflowDefinition,
        id: Option<&WorkflowId>,
    ) -> exposition_graph::workflow::WorkflowValidation {
        let resolve = |w: &WorkflowId, r: u64| self.0.get(&(w.clone(), r)).cloned();
        validate_workflow(
            def,
            &ValidationContext { ceiling: None, file_exists: None, resolve_workflow: Some(&resolve), workflow_id: id },
        )
    }
}

fn codes(v: &exposition_graph::workflow::WorkflowValidation) -> Vec<String> {
    v.diagnostics
        .iter()
        .filter(|d| d.severity == exposition_graph::DiagnosticSeverity::Error)
        .map(|d| d.code.clone())
        .collect()
}

#[test]
fn review_m9_held_a_child_cannot_widen_the_parents_ceiling() {
    let mut store = Store::default();
    // A native child (its own ceiling allows it) under a restricted parent.
    let native = store.put(&WorkflowId::mint(), 1, child(1, true, "print(1)\n", AuthorityCeiling::ALL));
    let v = store.validate(&parent(&[native], RESTRICTED), None);
    assert_eq!(v.state, DraftState::Incomplete);
    let c = codes(&v);
    assert!(c.contains(&"workflow.compositeCeiling".to_owned()), "{c:?}");
    assert!(c.contains(&"workflow.effectNotAllowed".to_owned()), "the native effect itself is refused too: {c:?}");
    // A restricted child whose declared ceiling is wider than the parent's is refused as well.
    let wide = store.put(&WorkflowId::mint(), 1, child(1, false, "print(1)\n", AuthorityCeiling::ALL));
    let c = codes(&store.validate(&parent(&[wide], RESTRICTED), None));
    assert_eq!(c, vec!["workflow.compositeCeiling".to_owned()]);
    // Within the ceiling: well-formed, and the plan is the expanded child.
    let ok = store.put(&WorkflowId::mint(), 1, child(1, false, "print(1)\n", RESTRICTED));
    let v = store.validate(&parent(&[ok], RESTRICTED), None);
    assert_eq!(v.state, DraftState::WellFormed, "{:?}", codes(&v));
    assert_eq!(v.plan.iter().map(|p| p.step.as_str()).collect::<Vec<_>>(), ["c0/s0"]);
}

#[test]
fn review_m9_held_recursion_through_revisions_depth_and_size_are_refused() {
    let mut store = Store::default();
    let (a, b) = (WorkflowId::mint(), WorkflowId::mint());
    // A1 plain; B1 runs A1; A2 runs B1: A2 includes its own workflow (through B).
    let a1 = store.put(&a, 1, child(1, false, "print('a1')\n", RESTRICTED));
    let b1 = store.put(&b, 1, parent(&[a1], RESTRICTED));
    let mut a2 = parent(&[b1], RESTRICTED);
    // B1's outcome is `o0`; A2 consumes it through its composite output.
    a2.outcomes = vec![outcome("o0", "c0", "o0")];
    a2.connections = vec![data(PortSource::Input { input: "data".into() }, "c0", "data")];
    let c = codes(&store.validate(&a2, Some(&a)));
    assert!(c.contains(&"workflow.compositeRecursive".to_owned()), "{c:?}");

    // Depth: W1 plain, W2→W1, W3→W2, W4→W3 (3 composite levels) is the limit; W5→W4 is refused.
    let mut prev = store.put(&WorkflowId::mint(), 1, child(1, false, "print(0)\n", RESTRICTED));
    let mut defs = Vec::new();
    for _ in 0..4 {
        let mut p = parent(&[prev.clone()], RESTRICTED);
        p.outcomes = vec![outcome("fitted", "c0", "fitted")];
        // Every level exposes `fitted` and takes `obs`, like the child.
        p.inputs = vec![data_input("obs")];
        p.connections = vec![data(PortSource::Input { input: "obs".into() }, "c0", "obs")];
        defs.push(p.clone());
        prev = store.put(&WorkflowId::mint(), 1, p);
    }
    let three = store.validate(&defs[2], None);
    assert_eq!(three.state, DraftState::WellFormed, "3 levels are allowed: {:?}", codes(&three));
    let four = codes(&store.validate(&defs[3], None));
    assert!(four.contains(&"workflow.compositeTooDeep".to_owned()), "{four:?}");

    // Size: three composites of a 40-step child expand to 120 > 100 steps.
    let big = store.put(&WorkflowId::mint(), 1, child(40, false, "print(1)\n", RESTRICTED));
    let c = codes(&store.validate(&parent(&[big.clone(), big.clone(), big.clone()], RESTRICTED), None));
    assert!(c.contains(&"workflow.tooManySteps".to_owned()), "{c:?}");
    let two = store.validate(&parent(&[big.clone(), big], RESTRICTED), None);
    assert_eq!(two.state, DraftState::WellFormed, "80 expanded steps are fine: {:?}", codes(&two));
}

#[test]
fn review_m9_held_the_expansion_runs_exactly_the_pinned_child_revision_with_its_code() {
    let mut store = Store::default();
    let id = WorkflowId::mint();
    let r1 = store.put(&id, 1, child(2, false, "MARKER_REVISION_ONE = 1\n", RESTRICTED));
    let _r2 = store.put(&id, 2, child(2, false, "MARKER_REVISION_TWO = 2\n", RESTRICTED));
    let def = parent(&[r1], RESTRICTED);
    let resolve = |w: &WorkflowId, r: u64| store.0.get(&(w.clone(), r)).cloned();
    let expanded = expand_workflow(&def, &resolve).unwrap();
    let ids: Vec<&str> = expanded.definition.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["c0/s0", "c0/s1"]);
    for step in &expanded.definition.steps {
        match &step.payload {
            Some(StepPayload::Code { source, .. }) => {
                assert!(source.contains("MARKER_REVISION_ONE"), "{source}");
                assert!(!source.contains("MARKER_REVISION_TWO"));
            }
            other => panic!("the expanded child step carries its full code: {other:?}"),
        }
    }
    assert_eq!(expanded.steps[0].composite_path, vec!["Composite c0".to_owned()]);
    // The child's input is bound to what the parent bound to the composite port.
    assert!(expanded.definition.connections.iter().any(|c| matches!(c,
        WorkflowConnection::Data { from: PortSource::Input { input }, to } if input == "data" && to.step == "c0/s0")));
    // The parent's outcome is realized by the child's last step.
    assert_eq!(expanded.definition.outcomes[0].from, StepPort { step: "c0/s1".into(), port: "dataset".into() });
    // A missing pinned revision never falls back to another revision.
    let missing = parent(&[CompositeRef { workflow: id.clone(), revision: 3 }], RESTRICTED);
    assert!(expand_workflow(&missing, &resolve).is_err());
    assert!(codes(&store.validate(&missing, None)).contains(&"workflow.compositeMissing".to_owned()));
}
