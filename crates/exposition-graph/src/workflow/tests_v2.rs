//! Schema /2 (M9): v1 compatibility, the v2 definition shapes, and the
//! validation rules for composites, conditions, repeats, and execution options.

use super::presets::{FORMAL_AUDIT, NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use super::*;

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn numerical(manuscript: bool) -> WorkflowDefinition {
    draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        script: Some(path("analysis/run.py")),
        data: Some(path("data/obs.csv")),
        manuscript: manuscript.then(|| path("main.typ")),
        lean_file: None,
    })
    .unwrap()
}

fn audit() -> WorkflowDefinition {
    draft(&WorkflowPresetRequest {
        preset: FORMAL_AUDIT.into(),
        lean_file: Some(path("Proofs/Main.lean")),
        ..Default::default()
    })
    .unwrap()
}

fn validate(def: &WorkflowDefinition) -> WorkflowValidation {
    validate_workflow(def, &ValidationContext::default())
}

fn errors(v: &WorkflowValidation) -> Vec<String> {
    v.diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Error).map(|d| d.code.clone()).collect()
}

fn has(v: &WorkflowValidation, code: &str) -> bool {
    v.diagnostics.iter().any(|d| d.code == code)
}

// ---------------------------------------------------------------- v1 compatibility

/// Hashes and serialization of the v1 presets, recorded at the M8 base
/// (5fac243) before any v2 field existed. A v1 definition must keep both, so
/// saved revisions, approval hashes, and run records stay valid.
#[test]
fn v1_definitions_serialize_byte_identically_and_keep_their_hash() {
    assert_eq!(definition_hash(&numerical(true)), "834dc29693a6c26dcdfe20021e746c56af32fc0f7dbdd5cebfdbd5568a7df6b6");
    assert_eq!(definition_hash(&numerical(false)), "a0f0bc9ee3f4a5b14e0baa5aeede6a525374b75f5f9d70d118eca9ebf6259670");
    assert_eq!(definition_hash(&audit()), "465a9872a7d86e4527ce1d4bf12b354a2446caa20944d79b644f52d61cc5eda4");
    let json = serde_json::to_string(&audit()).unwrap();
    assert_eq!(
        json,
        r#"{"schema":"exposition.workflow/1","title":"Formal audit","description":"Audit the Lean file completely, then select audited declarations and propose their evidence associations. A file badge never substitutes for declaration evidence.","preset":"formalAudit","inputs":[{"id":"leanFile","label":"Lean file","portType":"formalSource","source":{"kind":"resource","path":"Proofs/Main.lean","version":{"kind":"atRunStart"}}}],"steps":[{"id":"audit","label":"Audit the Lean file","operation":{"id":"lean.audit","version":1},"parameters":{},"payload":null},{"id":"associate","label":"Associate audited declarations","operation":{"id":"formal.associateEvidence","version":1},"parameters":{},"payload":null}],"connections":[{"kind":"data","from":{"kind":"input","input":"leanFile"},"to":{"step":"audit","port":"source"}},{"kind":"data","from":{"kind":"step","step":"audit","port":"evidence"},"to":{"step":"associate","port":"evidence"}}],"outcomes":[{"id":"evidence","kind":"auditEvidence","label":"Audit evidence","from":{"step":"audit","port":"evidence"},"optional":false},{"id":"associations","kind":"reviewableProposal","label":"Evidence associations for review","from":{"step":"associate","port":"proposal"},"optional":true}],"ceiling":{"execution":"trustedNative","modelRequests":false,"retainResults":true,"proposeChanges":true}}"#
    );
    // A stored v1 body (no v2 keys) parses to the same definition and re-serializes identically.
    let parsed = parse_definition(&json).unwrap();
    assert_eq!(parsed, audit());
    assert!(!parsed.uses_v2());
    assert_eq!(parsed.max_parallel(), 1);
    assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
}

#[test]
fn v1_presets_still_validate_cleanly_under_v1() {
    for def in [numerical(true), numerical(false), audit()] {
        assert_eq!(def.schema, WORKFLOW_SCHEMA, "presets stay v1");
        assert_eq!(errors(&validate(&def)), Vec::<String>::new(), "{}", def.title);
    }
}

// ---------------------------------------------------------------- schema /2

#[test]
fn schema_v2_is_accepted() {
    let mut def = audit();
    def.schema = WORKFLOW_SCHEMA_V2.into();
    assert_eq!(errors(&validate(&def)), Vec::<String>::new());
    def.execution = Some(WorkflowExecutionOptions { max_parallel: 2 });
    assert_eq!(errors(&validate(&def)), Vec::<String>::new());
    assert_eq!(def.max_parallel(), 2);
}

#[test]
fn v2_fields_require_schema_v2() {
    let condition = StepCondition {
        source: StepPort { step: "audit".into(), port: "evidence".into() },
        test: ConditionTest::VerdictIn { verdicts: vec!["checked".into()] },
        negate: false,
    };
    type Mutate = Box<dyn Fn(&mut WorkflowDefinition)>;
    let cases: Vec<(&str, Mutate)> = vec![
        ("execution", Box::new(|d| d.execution = Some(WorkflowExecutionOptions { max_parallel: 1 }))),
        ("condition", Box::new(move |d| d.steps[1].condition = Some(condition.clone()))),
        (
            "repeat",
            Box::new(|d| {
                d.steps[1].repeat = Some(StepRepeat::ForEach {
                    parameter: "declarations".into(),
                    values: vec![ParamValue::Text("A".into())],
                })
            }),
        ),
        (
            "composite",
            Box::new(|d| {
                d.steps[1].composite = Some(CompositeRef { workflow: WorkflowId::mint(), revision: 1 });
            }),
        ),
    ];
    for (what, set) in cases {
        let mut def = audit();
        set(&mut def);
        assert!(def.uses_v2(), "{what}");
        let v = validate(&def);
        assert!(
            v.diagnostics.iter().any(|d| d.code == "workflow.schema" && d.message.contains(WORKFLOW_SCHEMA_V2)),
            "{what} under schema /1 must be refused: {:?}",
            v.diagnostics
        );
        assert_eq!(v.state, DraftState::Incomplete, "{what}");
    }
}

#[test]
fn unknown_schemas_are_refused() {
    for schema in ["exposition.workflow/3", "exposition.workflow/0", "", "exposition.workflow-bundle/1"] {
        let mut def = audit();
        def.schema = schema.into();
        assert!(has(&validate(&def), "workflow.schema"), "{schema:?}");
    }
}

/// The JSON shapes are part of the contract (frontend, agents, bundles).
#[test]
fn v2_json_shapes() {
    let mut def = audit();
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.execution = Some(WorkflowExecutionOptions { max_parallel: 3 });
    let wf = WorkflowId::parse("wf-0123456789abcdef0123456789abcdef").unwrap_or_else(|_| WorkflowId::mint());
    def.steps[0].composite = Some(CompositeRef { workflow: wf.clone(), revision: 7 });
    def.steps[0].condition = Some(StepCondition {
        source: StepPort { step: "s".into(), port: "dataset".into() },
        test: ConditionTest::JsonCompare {
            pointer: "/metrics/r2".into(),
            op: CompareOp::Ge,
            value: ConditionValue::Number("0.95".into()),
        },
        negate: true,
    });
    def.steps[1].repeat = Some(StepRepeat::Until {
        port: "evidence".into(),
        test: ConditionTest::OutputPresent,
        negate: false,
        max_iterations: 3,
        on_exhausted: Exhausted::UseLast,
    });
    let v = serde_json::to_value(&def).unwrap();
    assert_eq!(v["execution"], serde_json::json!({ "maxParallel": 3 }));
    assert_eq!(v["steps"][0]["composite"], serde_json::json!({ "workflow": wf.as_str(), "revision": 7 }));
    assert_eq!(
        v["steps"][0]["condition"],
        serde_json::json!({
            "source": { "step": "s", "port": "dataset" },
            "test": { "kind": "jsonCompare", "pointer": "/metrics/r2", "op": "ge", "value": { "kind": "number", "value": "0.95" } },
            "negate": true
        })
    );
    assert_eq!(
        v["steps"][1]["repeat"],
        serde_json::json!({
            "kind": "until", "port": "evidence", "test": { "kind": "outputPresent" }, "negate": false,
            "maxIterations": 3, "onExhausted": "useLast"
        })
    );
    assert!(v["steps"][1].get("composite").is_none(), "absent v2 fields are omitted");
    let each = StepRepeat::ForEach { parameter: "seed".into(), values: vec![ParamValue::Integer(1)] };
    assert_eq!(
        serde_json::to_value(&each).unwrap(),
        serde_json::json!({ "kind": "forEach", "parameter": "seed", "values": [{ "kind": "integer", "value": 1 }] })
    );
    let verdicts = ConditionTest::VerdictIn { verdicts: vec!["checked".into()] };
    assert_eq!(
        serde_json::to_value(&verdicts).unwrap(),
        serde_json::json!({ "kind": "verdictIn", "verdicts": ["checked"] })
    );
    // Round trip through the strict parser.
    let back = parse_definition(&serde_json::to_string(&def).unwrap()).unwrap();
    assert_eq!(back, def);
}

#[test]
fn v2_shapes_are_strict() {
    let base = || {
        let mut def = audit();
        def.schema = WORKFLOW_SCHEMA_V2.into();
        serde_json::to_value(&def).unwrap()
    };
    let hostile = [
        ("/execution", serde_json::json!({ "maxParallel": 2, "unbounded": true })),
        ("/steps/0/composite", serde_json::json!({ "workflow": "wf-x", "revision": 1, "trust": true })),
        (
            "/steps/0/condition",
            serde_json::json!({ "source": { "step": "a", "port": "b" }, "test": { "kind": "expression", "code": "x>1" }, "negate": false }),
        ),
        (
            "/steps/0/condition",
            serde_json::json!({ "source": { "step": "a", "port": "b" }, "test": { "kind": "outputPresent", "eval": "1" }, "negate": false }),
        ),
        ("/steps/0/repeat", serde_json::json!({ "kind": "while", "port": "x" })),
        ("/steps/0/repeat", serde_json::json!({ "kind": "forEach", "parameter": "p", "values": [], "parallel": 99 })),
        (
            "/steps/0/repeat",
            serde_json::json!({ "kind": "until", "port": "x", "test": { "kind": "outputPresent" }, "negate": false, "maxIterations": 3, "onExhausted": "retryForever" }),
        ),
    ];
    for (pointer, value) in hostile {
        let mut json = base();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        json.pointer_mut(if parent.is_empty() { "" } else { parent }).unwrap()[key] = value.clone();
        assert!(parse_definition(&json.to_string()).is_err(), "{pointer} = {value} accepted");
    }
}

/// Unit variants of tagged enums (v1 `unset`/`atRunStart`, v2
/// `outputPresent`) cannot smuggle extra keys past the strict parser.
#[test]
fn unit_variants_cannot_smuggle_fields() {
    let mut json = serde_json::to_value(audit()).unwrap();
    json["inputs"][0]["source"] = serde_json::json!({ "kind": "unset", "trust": true });
    let err = parse_definition(&json.to_string()).unwrap_err();
    assert!(err.contains("/inputs/0/source/trust"), "{err}");
    let mut json = serde_json::to_value(audit()).unwrap();
    json["inputs"][0]["source"]["version"] = serde_json::json!({ "kind": "atRunStart", "approved": true });
    assert!(parse_definition(&json.to_string()).is_err());
    // Missing optional keys are fine (they parse to their defaults).
    let mut json = serde_json::to_value(audit()).unwrap();
    json["steps"][0].as_object_mut().unwrap().remove("payload");
    assert_eq!(parse_definition(&json.to_string()).unwrap(), audit());
}

// ---------------------------------------------------------------- fixtures for v2 rules

use std::collections::HashMap;

fn restricted() -> AuthorityCeiling {
    AuthorityCeiling {
        execution: ExecutionCeiling::Restricted,
        model_requests: false,
        retain_results: false,
        propose_changes: false,
    }
}

fn step(id: &str, op: &str) -> WorkflowStep {
    WorkflowStep {
        id: id.into(),
        label: format!("Step {id}"),
        operation: StepOperation { id: op.into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
        composite: None,
        condition: None,
        repeat: None,
    }
}

fn py(id: &str) -> WorkflowStep {
    let mut s = step(id, registry::RESEARCH_PYTHON_BATCH);
    s.payload = Some(StepPayload::Code { language: CodeLanguage::Python, source: "print(1)".into() });
    s
}

fn review(id: &str) -> WorkflowStep {
    let mut s = step(id, registry::AGENT_REVIEW);
    s.payload = Some(StepPayload::Prompt { text: "Summarize.".into() });
    s
}

fn composite(id: &str, wf: &WorkflowId, revision: u64) -> WorkflowStep {
    let mut s = step(id, COMPOSITE_OPERATION);
    s.label = format!("Composite {id}");
    s.composite = Some(CompositeRef { workflow: wf.clone(), revision });
    s
}

fn wire(from_step: &str, from_port: &str, to_step: &str, to_port: &str) -> WorkflowConnection {
    WorkflowConnection::Data {
        from: PortSource::Step { step: from_step.into(), port: from_port.into() },
        to: StepPort { step: to_step.into(), port: to_port.into() },
    }
}

fn feed(input: &str, to_step: &str, to_port: &str) -> WorkflowConnection {
    WorkflowConnection::Data {
        from: PortSource::Input { input: input.into() },
        to: StepPort { step: to_step.into(), port: to_port.into() },
    }
}

fn outcome(id: &str, kind: OutcomeKind, step: &str, port: &str) -> WorkflowOutcome {
    WorkflowOutcome {
        id: id.into(),
        kind,
        label: format!("Outcome {id}"),
        from: StepPort { step: step.into(), port: port.into() },
        optional: false,
    }
}

fn data_input(id: &str, source: InputSource) -> WorkflowInput {
    WorkflowInput { id: id.into(), label: format!("Input {id}"), port_type: PortType::Dataset, source }
}

fn obs() -> InputSource {
    InputSource::Resource { path: path("data/obs.csv"), version: InputVersion::AtRunStart }
}

fn base(title: &str) -> WorkflowDefinition {
    WorkflowDefinition {
        schema: WORKFLOW_SCHEMA_V2.into(),
        title: title.into(),
        description: String::new(),
        preset: None,
        inputs: Vec::new(),
        steps: Vec::new(),
        connections: Vec::new(),
        outcomes: Vec::new(),
        ceiling: restricted(),
        execution: None,
    }
}

/// A reusable child: input `data` (left unset: the parent supplies it) ->
/// restricted Python `fit` -> outcome `fitted` (dataset).
pub(super) fn child_fit() -> WorkflowDefinition {
    let mut d = base("Fit");
    d.schema = WORKFLOW_SCHEMA.into();
    d.inputs = vec![data_input("data", InputSource::Unset)];
    d.steps = vec![py("fit")];
    d.connections = vec![feed("data", "fit", "data")];
    d.outcomes = vec![outcome("fitted", OutcomeKind::TransformedDataset, "fit", "dataset")];
    d
}

/// A child that wraps another composite with the same interface.
pub(super) fn wrap(inner: &WorkflowId, revision: u64) -> WorkflowDefinition {
    let mut d = base("Wrapper");
    d.inputs = vec![data_input("data", InputSource::Unset)];
    d.steps = vec![composite("inner", inner, revision)];
    d.connections = vec![feed("data", "inner", "data")];
    d.outcomes = vec![outcome("fitted", OutcomeKind::TransformedDataset, "inner", "fitted")];
    d
}

/// Parent: project data -> composite `sub` -> agent review -> report.
pub(super) fn parent(child: &WorkflowId, revision: u64) -> WorkflowDefinition {
    let mut d = base("Parent");
    d.ceiling.model_requests = true;
    d.inputs = vec![data_input("data", obs())];
    d.steps = vec![composite("sub", child, revision), review("review")];
    d.connections = vec![feed("data", "sub", "data"), wire("sub", "fitted", "review", "material")];
    d.outcomes = vec![outcome("report", OutcomeKind::InvestigationReport, "review", "report")];
    d
}

struct Lib(HashMap<(WorkflowId, u64), WorkflowDefinition>);

impl Lib {
    fn new(entries: Vec<(&WorkflowId, u64, WorkflowDefinition)>) -> Self {
        Lib(entries.into_iter().map(|(id, rev, def)| ((id.clone(), rev), def)).collect())
    }
    fn get(&self, id: &WorkflowId, rev: u64) -> Option<WorkflowDefinition> {
        self.0.get(&(id.clone(), rev)).cloned()
    }
}

fn validate_with(def: &WorkflowDefinition, lib: &Lib) -> WorkflowValidation {
    let resolve = |id: &WorkflowId, rev: u64| lib.get(id, rev);
    validate_workflow(def, &ValidationContext { resolve_workflow: Some(&resolve), ..Default::default() })
}

fn has_error(v: &WorkflowValidation, code: &str) -> bool {
    v.diagnostics.iter().any(|d| d.code == code && d.severity == DiagnosticSeverity::Error)
}

fn all_available(_: &str) -> Result<(), String> {
    Ok(())
}

// ---------------------------------------------------------------- composites

#[test]
fn composite_ports_effects_and_expanded_plan_come_from_the_pinned_child() {
    let c = WorkflowId::mint();
    let lib = Lib::new(vec![(&c, 1, child_fit())]);
    let v = validate_with(&parent(&c, 1), &lib);
    assert_eq!(errors(&v), Vec::<String>::new(), "{:?}", v.diagnostics);
    assert_eq!(v.state, DraftState::WellFormed);
    let ids: Vec<&str> = v.plan.iter().map(|p| p.step.as_str()).collect();
    assert_eq!(ids, vec!["sub/fit", "review"], "children expanded in dependency order");
    let fit = &v.plan[0];
    assert_eq!(fit.runner_id.as_deref(), Some("restrictedPython"));
    assert_eq!(fit.composite_path, Some(vec!["Composite sub".to_owned()]));
    assert!(fit.effects.contains(&WorkflowEffect::ExecuteRestricted));
    assert_eq!(v.plan[1].composite_path, None);
    for e in [WorkflowEffect::ExecuteRestricted, WorkflowEffect::SendToModel, WorkflowEffect::WriteRunOutputs] {
        assert!(v.effects.contains(&e), "effects are the union of the children's: {e:?}");
    }
    // Runnable once every expanded child's runner is available; not when one is missing.
    let mut runnable = v.clone();
    apply_runnability(&mut runnable, &parent(&c, 1), &all_available);
    assert!(runnable.runnable, "{}", runnable.runnable_reason);
    let mut blocked = v.clone();
    apply_runnability(&mut blocked, &parent(&c, 1), &|id| {
        if id == "restrictedPython" { Err("no image".into()) } else { Ok(()) }
    });
    assert!(!blocked.runnable);
    assert!(blocked.runnable_reason.contains("no image"), "{}", blocked.runnable_reason);
}

#[test]
fn composite_ports_are_typed_and_required() {
    let c = WorkflowId::mint();
    let lib = Lib::new(vec![(&c, 1, child_fit())]);
    // The composite's input is required.
    let mut def = parent(&c, 1);
    def.connections.retain(|x| !matches!(x, WorkflowConnection::Data { to, .. } if to.step == "sub"));
    def.inputs.clear();
    assert!(has_error(&validate_with(&def, &lib), "workflow.unboundInput"));
    // Unknown composite port.
    let mut def = parent(&c, 1);
    def.connections[1] = wire("sub", "nope", "review", "material");
    assert!(has_error(&validate_with(&def, &lib), "workflow.unknownPort"));
    // The outcome type is the child's: a dataset cannot be audit evidence.
    let mut def = parent(&c, 1);
    def.outcomes.push(outcome("ev", OutcomeKind::AuditEvidence, "sub", "fitted"));
    assert!(has_error(&validate_with(&def, &lib), "workflow.serviceOwnedType"));
}

#[test]
fn composite_without_a_resolver_is_unchecked_and_never_runnable() {
    let c = WorkflowId::mint();
    let def = parent(&c, 1);
    let v = validate(&def);
    let unchecked: Vec<_> = v.diagnostics.iter().filter(|d| d.code == "workflow.compositeUnchecked").collect();
    assert_eq!(unchecked.len(), 1, "{:?}", v.diagnostics);
    assert_eq!(unchecked[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(unchecked[0].step.as_deref(), Some("sub"));
    let mut v2 = v.clone();
    apply_runnability(&mut v2, &def, &all_available);
    assert!(!v2.runnable, "an unchecked composite is never runnable");
    assert!(v2.runnable_reason.to_lowercase().contains("composite"), "{}", v2.runnable_reason);
}

#[test]
fn composite_reference_must_exist_and_be_well_formed() {
    let c = WorkflowId::mint();
    let lib = Lib::new(vec![(&c, 1, child_fit())]);
    assert!(has_error(&validate_with(&parent(&c, 2), &lib), "workflow.compositeMissing"));
    assert!(has_error(&validate_with(&parent(&WorkflowId::mint(), 1), &lib), "workflow.compositeMissing"));
    assert!(has_error(&validate_with(&parent(&c, 0), &lib), "workflow.compositeMissing"));
    let mut broken = child_fit();
    broken.steps[0].operation.id = "shell.run".into();
    let lib = Lib::new(vec![(&c, 1, broken)]);
    let v = validate_with(&parent(&c, 1), &lib);
    assert!(has_error(&v, "workflow.compositeNotWellFormed"), "{:?}", v.diagnostics);
    assert_eq!(v.state, DraftState::Incomplete);
}

#[test]
fn composite_operation_and_reference_go_together() {
    let c = WorkflowId::mint();
    let lib = Lib::new(vec![(&c, 1, child_fit())]);
    // A reference on another operation.
    let mut def = parent(&c, 1);
    def.steps[1].composite = Some(CompositeRef { workflow: c.clone(), revision: 1 });
    assert!(has_error(&validate_with(&def, &lib), "workflow.compositeOperation"));
    // The composite operation without a reference.
    let mut def = parent(&c, 1);
    def.steps[0].composite = None;
    assert!(has_error(&validate_with(&def, &lib), "workflow.compositeOperation"));
    // A composite takes no parameters or payload.
    let mut def = parent(&c, 1);
    def.steps[0].parameters.insert("timeoutSeconds".into(), ParamValue::Integer(5));
    assert!(has_error(&validate_with(&def, &lib), "workflow.unknownParameter"));
    let mut def = parent(&c, 1);
    def.steps[0].payload = Some(StepPayload::Prompt { text: "hi".into() });
    assert!(has_error(&validate_with(&def, &lib), "workflow.unexpectedPayload"));
}

#[test]
fn composite_child_ceiling_must_fit_within_the_parent() {
    let c = WorkflowId::mint();
    let mut native_child = child_fit();
    native_child.ceiling.execution = ExecutionCeiling::TrustedNative;
    native_child.steps[0].operation.id = registry::RESEARCH_NATIVE_PYTHON.into();
    let lib = Lib::new(vec![(&c, 1, native_child)]);
    let v = validate_with(&parent(&c, 1), &lib);
    assert!(
        v.diagnostics.iter().any(|d| d.code == "workflow.compositeCeiling" && d.rule == Some(4)),
        "{:?}",
        v.diagnostics
    );
    assert!(has_error(&v, "workflow.effectNotAllowed"), "the child's native effect exceeds the parent's ceiling");
    // A wider child ceiling is refused even when its steps would fit.
    let mut wide = child_fit();
    wide.ceiling = AuthorityCeiling::ALL;
    let lib = Lib::new(vec![(&c, 1, wide)]);
    assert!(has_error(&validate_with(&parent(&c, 1), &lib), "workflow.compositeCeiling"));
}

#[test]
fn composite_recursion_is_refused() {
    let p = WorkflowId::mint();
    let c = WorkflowId::mint();
    let d = WorkflowId::mint();
    // The parent includes itself (an earlier revision).
    let lib = Lib::new(vec![(&p, 1, child_fit())]);
    let resolve = |id: &WorkflowId, rev: u64| lib.get(id, rev);
    let ctx = ValidationContext { resolve_workflow: Some(&resolve), workflow_id: Some(&p), ..Default::default() };
    assert!(has_error(&validate_workflow(&parent(&p, 1), &ctx), "workflow.compositeRecursive"));
    // A child includes the parent.
    let lib = Lib::new(vec![(&c, 1, wrap(&p, 1)), (&p, 1, child_fit())]);
    let resolve = |id: &WorkflowId, rev: u64| lib.get(id, rev);
    let ctx = ValidationContext { resolve_workflow: Some(&resolve), workflow_id: Some(&p), ..Default::default() };
    assert!(has_error(&validate_workflow(&parent(&c, 1), &ctx), "workflow.compositeRecursive"));
    // A cycle between saved children (a tampered store): c -> d -> c.
    let lib = Lib::new(vec![(&c, 1, wrap(&d, 1)), (&d, 1, wrap(&c, 1))]);
    let v = validate_with(&parent(&c, 1), &lib);
    assert!(has_error(&v, "workflow.compositeRecursive"), "{:?}", v.diagnostics);
    // A child that includes itself directly.
    let lib = Lib::new(vec![(&c, 1, wrap(&c, 1))]);
    assert!(has_error(&validate_with(&parent(&c, 1), &lib), "workflow.compositeRecursive"));
}

#[test]
fn composite_depth_is_at_most_three() {
    let ids: Vec<WorkflowId> = (0..5).map(|_| WorkflowId::mint()).collect();
    let mut entries = vec![(&ids[0], 1, child_fit())];
    for i in 1..5 {
        entries.push((&ids[i], 1, wrap(&ids[i - 1], 1)));
    }
    let lib = Lib::new(entries);
    // parent -> ids[2] -> ids[1] -> ids[0]: three nested composites.
    let v = validate_with(&parent(&ids[2], 1), &lib);
    assert_eq!(errors(&v), Vec::<String>::new(), "{:?}", v.diagnostics);
    assert_eq!(v.plan[0].step, "sub/inner/inner/fit");
    assert_eq!(v.plan[0].composite_path.as_ref().map(|p| p.len()), Some(3));
    // parent -> ids[3] -> ... -> ids[0]: four.
    let v = validate_with(&parent(&ids[3], 1), &lib);
    assert!(has_error(&v, "workflow.compositeTooDeep"), "{:?}", v.diagnostics);
    assert_eq!(v.state, DraftState::Incomplete);
}

/// A chain of `n` restricted Python steps with no external inputs.
fn chain_child(n: usize) -> WorkflowDefinition {
    let mut d = base("Chain");
    d.steps = (0..n).map(|i| py(&format!("s{i}"))).collect();
    d.connections = (1..n).map(|i| wire(&format!("s{}", i - 1), "dataset", &format!("s{i}"), "data")).collect();
    d.outcomes = vec![outcome("fitted", OutcomeKind::TransformedDataset, &format!("s{}", n - 1), "dataset")];
    d
}

#[test]
fn expanded_steps_are_at_most_one_hundred() {
    let c = WorkflowId::mint();
    let lib = Lib::new(vec![(&c, 1, chain_child(34))]);
    let mut def = base("Many");
    for i in 0..3 {
        def.steps.push(composite(&format!("c{i}"), &c, 1));
        def.outcomes.push(outcome(&format!("o{i}"), OutcomeKind::TransformedDataset, &format!("c{i}"), "fitted"));
    }
    let v = validate_with(&def, &lib);
    assert!(has_error(&v, "workflow.tooManySteps"), "102 expanded steps: {:?}", v.diagnostics);
    def.steps.pop();
    def.outcomes.pop();
    let v = validate_with(&def, &lib);
    assert_eq!(errors(&v), Vec::<String>::new(), "68 expanded steps: {:?}", v.diagnostics);
    assert_eq!(v.plan.len(), 68);
}

#[test]
fn composites_cannot_repeat_or_carry_conditions() {
    let c = WorkflowId::mint();
    let lib = Lib::new(vec![(&c, 1, child_fit())]);
    let mut def = parent(&c, 1);
    def.steps[0].repeat =
        Some(StepRepeat::ForEach { parameter: "timeoutSeconds".into(), values: vec![ParamValue::Integer(5)] });
    assert!(has_error(&validate_with(&def, &lib), "workflow.repeatInvalid"));
    let mut def = parent(&c, 1);
    def.steps.insert(0, py("pre"));
    def.connections.push(wire("pre", "dataset", "review", "material"));
    def.steps[1].condition = Some(StepCondition {
        source: StepPort { step: "pre".into(), port: "dataset".into() },
        test: ConditionTest::OutputPresent,
        negate: false,
    });
    assert!(has_error(&validate_with(&def, &lib), "workflow.conditionInvalid"));
}

// ---------------------------------------------------------------- conditions

/// Audit preset (v2) with a condition on `associate`.
fn audit_with(test: ConditionTest, source_port: &str) -> WorkflowDefinition {
    let mut def = audit();
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.steps[1].condition = Some(StepCondition {
        source: StepPort { step: "audit".into(), port: source_port.into() },
        test,
        negate: false,
    });
    def
}

#[test]
fn condition_source_is_an_upstream_output_of_a_compatible_type() {
    let ok = audit_with(ConditionTest::VerdictIn { verdicts: vec!["Checked".into()] }, "evidence");
    assert_eq!(errors(&validate(&ok)), Vec::<String>::new(), "{:?}", validate(&ok).diagnostics);
    let ok = audit_with(ConditionTest::OutputPresent, "evidence");
    assert_eq!(errors(&validate(&ok)), Vec::<String>::new());
    // Wrong type: a JSON comparison needs a dataset.
    let bad = audit_with(
        ConditionTest::JsonCompare { pointer: "/a".into(), op: CompareOp::Eq, value: ConditionValue::Boolean(true) },
        "evidence",
    );
    assert!(has_error(&validate(&bad), "workflow.conditionSource"));
    // Unknown port / unknown step / itself.
    assert!(has_error(&validate(&audit_with(ConditionTest::OutputPresent, "nope")), "workflow.conditionSource"));
    let mut bad = audit_with(ConditionTest::OutputPresent, "evidence");
    bad.steps[1].condition.as_mut().unwrap().source.step = "ghost".into();
    assert!(has_error(&validate(&bad), "workflow.conditionSource"));
    let mut bad = audit_with(ConditionTest::OutputPresent, "proposal");
    bad.steps[1].condition.as_mut().unwrap().source.step = "associate".into();
    assert!(has_error(&validate(&bad), "workflow.conditionSource"));
    // A downstream source is a cycle.
    let mut bad = audit();
    bad.schema = WORKFLOW_SCHEMA_V2.into();
    bad.steps[0].condition = Some(StepCondition {
        source: StepPort { step: "associate".into(), port: "proposal".into() },
        test: ConditionTest::OutputPresent,
        negate: true,
    });
    let v = validate(&bad);
    assert!(has_error(&v, "workflow.cycle"), "{:?}", v.diagnostics);
    // A JSON comparison on a dataset output is fine.
    let mut def = numerical(false);
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.steps[1].condition = Some(StepCondition {
        source: StepPort { step: "run".into(), port: "dataset".into() },
        test: ConditionTest::JsonCompare {
            pointer: "/fit/r2".into(),
            op: CompareOp::Ge,
            value: ConditionValue::Number("0.9".into()),
        },
        negate: false,
    });
    assert_eq!(errors(&validate(&def)), Vec::<String>::new(), "{:?}", validate(&def).diagnostics);
}

#[test]
fn a_condition_orders_its_step_after_the_source_and_counts_as_use() {
    // Two independent steps; b's condition reads a's dataset.
    let mut def = base("Ordered");
    def.ceiling.model_requests = true;
    def.inputs = vec![data_input("data", obs())];
    def.steps = vec![review("b"), py("a")];
    def.connections = vec![feed("data", "b", "material")];
    def.outcomes = vec![outcome("report", OutcomeKind::InvestigationReport, "b", "report")];
    let v = validate(&def);
    assert!(has_error(&v, "workflow.orphanStep"), "a is an orphan without the condition");
    def.steps[0].condition = Some(StepCondition {
        source: StepPort { step: "a".into(), port: "dataset".into() },
        test: ConditionTest::JsonCompare {
            pointer: "".into(),
            op: CompareOp::Ne,
            value: ConditionValue::Text("x".into()),
        },
        negate: false,
    });
    let v = validate(&def);
    assert_eq!(errors(&v), Vec::<String>::new(), "{:?}", v.diagnostics);
    let order: Vec<&str> = v.plan.iter().map(|p| p.step.as_str()).collect();
    assert_eq!(order, vec!["a", "b"]);
}

#[test]
fn condition_tests_are_bounded_and_well_formed() {
    let json = |pointer: &str, op: CompareOp, value: ConditionValue| {
        let mut def = numerical(false);
        def.schema = WORKFLOW_SCHEMA_V2.into();
        def.steps[1].condition = Some(StepCondition {
            source: StepPort { step: "run".into(), port: "dataset".into() },
            test: ConditionTest::JsonCompare { pointer: pointer.into(), op, value },
            negate: false,
        });
        validate(&def)
    };
    let num = |s: &str| ConditionValue::Number(s.into());
    let long = format!("/{}", "x".repeat(256));
    for (pointer, op, value) in [
        ("a", CompareOp::Eq, num("1")),
        ("/a~2", CompareOp::Eq, num("1")),
        ("/a~", CompareOp::Eq, num("1")),
        (long.as_str(), CompareOp::Eq, num("1")),
        ("/a", CompareOp::Eq, num("abc")),
        ("/a", CompareOp::Eq, num("1e")),
        ("/a", CompareOp::Eq, num("NaN")),
        ("/a", CompareOp::Eq, num("")),
        ("/a", CompareOp::Eq, num("01")),
        ("/a", CompareOp::Lt, ConditionValue::Text("b".into())),
        ("/a", CompareOp::Gt, ConditionValue::Boolean(true)),
        ("/a", CompareOp::Eq, ConditionValue::Text("x".repeat(1001))),
        ("/a\u{0}", CompareOp::Eq, num("1")),
    ] {
        assert!(
            has_error(&json(pointer, op, value.clone()), "workflow.conditionInvalid"),
            "{pointer:?} {op:?} {value:?}"
        );
    }
    for (pointer, op, value) in [
        ("", CompareOp::Eq, num("1")),
        ("/a~0b/c~1d/0", CompareOp::Le, num("-1.5e3")),
        ("/a", CompareOp::Ne, ConditionValue::Boolean(false)),
        ("/a", CompareOp::Eq, ConditionValue::Text("ok".into())),
    ] {
        let v = json(pointer, op, value.clone());
        assert_eq!(errors(&v), Vec::<String>::new(), "{pointer:?} {op:?} {value:?}: {:?}", v.diagnostics);
    }
    for verdicts in [vec![], vec!["checked".to_owned()], vec!["Proved".to_owned()], vec!["Checked".to_owned(); 2]] {
        let v = validate(&audit_with(ConditionTest::VerdictIn { verdicts: verdicts.clone() }, "evidence"));
        assert!(has_error(&v, "workflow.conditionInvalid"), "{verdicts:?}");
    }
    let all: Vec<String> = KNOWN_VERDICTS.iter().map(|s| s.to_string()).collect();
    let v = validate(&audit_with(ConditionTest::VerdictIn { verdicts: all }, "evidence"));
    assert_eq!(errors(&v), Vec::<String>::new());
}

// ---------------------------------------------------------------- repeats

/// Restricted Python `run` repeated for each timeout, collected by an agent review.
fn for_each(values: Vec<ParamValue>) -> WorkflowDefinition {
    let mut def = base("Sweep");
    def.ceiling.model_requests = true;
    def.inputs = vec![data_input("data", obs())];
    def.steps = vec![py("run"), review("review")];
    def.steps[0].repeat = Some(StepRepeat::ForEach { parameter: "timeoutSeconds".into(), values });
    def.connections = vec![feed("data", "run", "data"), wire("run", "dataset", "review", "material")];
    def.outcomes = vec![outcome("report", OutcomeKind::InvestigationReport, "review", "report")];
    def
}

#[test]
fn for_each_values_match_the_declared_parameter() {
    let ints = |n: i64| (1..=n).map(ParamValue::Integer).collect::<Vec<_>>();
    let v = validate(&for_each(ints(3)));
    assert_eq!(errors(&v), Vec::<String>::new(), "{:?}", v.diagnostics);
    assert_eq!(errors(&validate(&for_each(ints(20)))), Vec::<String>::new());
    for values in [
        vec![],
        ints(21),
        vec![ParamValue::Text("10".into())],
        vec![ParamValue::Integer(0)],
        vec![ParamValue::Integer(3601)],
    ] {
        assert!(has_error(&validate(&for_each(values.clone())), "workflow.repeatInvalid"), "{values:?}");
    }
    // Unknown parameter.
    let mut def = for_each(ints(2));
    def.steps[0].repeat = Some(StepRepeat::ForEach { parameter: "shell".into(), values: ints(2) });
    assert!(has_error(&validate(&def), "workflow.repeatInvalid"));
    // Also set directly: ambiguous.
    let mut def = for_each(ints(2));
    def.steps[0].parameters.insert("timeoutSeconds".into(), ParamValue::Integer(5));
    assert!(has_error(&validate(&def), "workflow.repeatInvalid"));
}

#[test]
fn a_required_parameter_supplied_by_for_each_is_not_missing() {
    let mut def = numerical(true);
    def.schema = WORKFLOW_SCHEMA_V2.into();
    let promote = def.steps.iter_mut().find(|s| s.id == "promote").unwrap();
    promote.parameters.remove("document");
    promote.repeat = Some(StepRepeat::ForEach {
        parameter: "document".into(),
        values: vec![ParamValue::Text("main.typ".into()), ParamValue::Text("notes.md".into())],
    });
    let v = validate(&def);
    assert!(!has_error(&v, "workflow.missingParameter"), "{:?}", v.diagnostics);
    // Path values are validated as paths.
    let promote = def.steps.iter_mut().find(|s| s.id == "promote").unwrap();
    promote.repeat =
        Some(StepRepeat::ForEach { parameter: "document".into(), values: vec![ParamValue::Text("../x.typ".into())] });
    assert!(has_error(&validate(&def), "workflow.repeatInvalid"));
}

#[test]
fn for_each_outputs_feed_only_multiple_ports() {
    // run.figure -> retain.artifact (one connection only).
    let mut def = numerical(false);
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.steps[0].repeat = Some(StepRepeat::ForEach {
        parameter: "timeoutSeconds".into(),
        values: vec![ParamValue::Integer(10), ParamValue::Integer(20)],
    });
    let v = validate(&def);
    assert!(
        v.diagnostics.iter().any(|d| d.code == "workflow.iterationFanIn" && d.step.as_deref() == Some("retain")),
        "{:?}",
        v.diagnostics
    );
    // An outcome straight from a repeated step.
    let mut def = for_each(vec![ParamValue::Integer(1)]);
    def.outcomes.push(outcome("raw", OutcomeKind::TransformedDataset, "run", "dataset"));
    assert!(has_error(&validate(&def), "workflow.iterationFanIn"));
    // A condition cannot read one instance of a repeated step.
    let mut def = for_each(vec![ParamValue::Integer(1)]);
    def.steps[1].condition = Some(StepCondition {
        source: StepPort { step: "run".into(), port: "dataset".into() },
        test: ConditionTest::OutputPresent,
        negate: false,
    });
    assert!(has_error(&validate(&def), "workflow.conditionSource"));
}

fn until(port: &str, test: ConditionTest, max_iterations: u32) -> WorkflowDefinition {
    let mut def = numerical(false);
    def.schema = WORKFLOW_SCHEMA_V2.into();
    def.steps[0].repeat = Some(StepRepeat::Until {
        port: port.into(),
        test,
        negate: false,
        max_iterations,
        on_exhausted: Exhausted::Fail,
    });
    def
}

#[test]
fn until_reads_its_own_output_and_is_bounded() {
    let r2 = || ConditionTest::JsonCompare {
        pointer: "/r2".into(),
        op: CompareOp::Ge,
        value: ConditionValue::Number("0.99".into()),
    };
    let v = validate(&until("dataset", r2(), 5));
    assert_eq!(errors(&v), Vec::<String>::new(), "{:?}", v.diagnostics);
    assert_eq!(errors(&validate(&until("figure", ConditionTest::OutputPresent, 10))), Vec::<String>::new());
    for (port, test, n) in [
        ("dataset", r2(), 0),
        ("dataset", r2(), 11),
        ("nope", ConditionTest::OutputPresent, 3),
        ("script", ConditionTest::OutputPresent, 3),
        ("figure", r2(), 3),
        ("dataset", ConditionTest::VerdictIn { verdicts: vec!["Checked".into()] }, 3),
        (
            "dataset",
            ConditionTest::JsonCompare {
                pointer: "r2".into(),
                op: CompareOp::Ge,
                value: ConditionValue::Number("1".into()),
            },
            3,
        ),
    ] {
        assert!(has_error(&validate(&until(port, test.clone(), n)), "workflow.repeatInvalid"), "{port} {test:?} {n}");
    }
}

// ---------------------------------------------------------------- execution options

#[test]
fn max_parallel_is_one_to_four() {
    for (n, ok) in [(0, false), (1, true), (2, true), (4, true), (5, false), (u32::MAX, false)] {
        let mut def = audit();
        def.schema = WORKFLOW_SCHEMA_V2.into();
        def.execution = Some(WorkflowExecutionOptions { max_parallel: n });
        assert_eq!(!has_error(&validate(&def), "workflow.maxParallel"), ok, "{n}");
    }
}

#[test]
fn an_unchecked_composite_adds_no_spurious_errors() {
    let c = WorkflowId::mint();
    let v = validate(&parent(&c, 1));
    assert_eq!(errors(&v), Vec::<String>::new(), "{:?}", v.diagnostics);
    assert_eq!(v.state, DraftState::WellFormed, "only the warning; runnability refuses it");
}

#[test]
fn the_registry_lists_the_composite_operation() {
    let op = registry::operation(COMPOSITE_OPERATION, 1).expect("workflow.composite v1 is registered");
    assert!(op.inputs.is_empty() && op.outputs.is_empty() && op.effects.is_empty(), "ports come from the child");
    assert!(op.payload.is_none() && op.parameters.is_empty());
    assert_eq!(op.runner.id, COMPOSITE_RUNNER);
    assert!(!op.runner.available, "the bare registry never claims availability");
    let listed = with_availability(&|_| Err("nothing installed".into()));
    let composite = listed.iter().find(|o| o.id == COMPOSITE_OPERATION).unwrap();
    assert!(composite.runner.available, "its steps are checked one by one instead");
    // Any other version is unsupported.
    let lib = Lib::new(vec![]);
    let mut def = parent(&WorkflowId::mint(), 1);
    def.steps[0].operation.version = 2;
    assert!(has_error(&validate_with(&def, &lib), "workflow.unsupportedVersion"));
}

#[test]
fn expansion_rewires_composites_into_flat_steps() {
    let ids: Vec<WorkflowId> = (0..3).map(|_| WorkflowId::mint()).collect();
    let lib = Lib::new(vec![(&ids[0], 1, child_fit()), (&ids[1], 1, wrap(&ids[0], 1)), (&ids[2], 1, wrap(&ids[1], 1))]);
    let resolve = |id: &WorkflowId, rev: u64| lib.get(id, rev);
    let mut def = parent(&ids[2], 1);
    def.steps.push(py("after"));
    def.connections.push(WorkflowConnection::Prerequisite { before: "sub".into(), after: "after".into() });
    def.outcomes.push(outcome("more", OutcomeKind::TransformedDataset, "after", "dataset"));
    def.outcomes.push(outcome("direct", OutcomeKind::TransformedDataset, "sub", "fitted"));
    let x = expand_workflow(&def, &resolve).unwrap();
    let ids_out: Vec<&str> = x.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids_out, vec!["sub/inner/inner/fit", "review", "after"]);
    assert_eq!(x.steps[0].composite_path, vec!["Composite sub", "Composite inner", "Composite inner"]);
    assert_eq!(x.steps[0].sources.len(), 3);
    assert_eq!(x.steps[0].sources[0], CompositeRef { workflow: ids[2].clone(), revision: 1 });
    let flat = &x.definition;
    assert_eq!(flat.steps[0].id, "sub/inner/inner/fit");
    assert_eq!(flat.steps[0].operation.id, registry::RESEARCH_PYTHON_BATCH);
    // The child's input is fed by the parent's binding of the composite port.
    assert!(flat.connections.contains(&feed("data", "sub/inner/inner/fit", "data")), "{:?}", flat.connections);
    // The composite's output is the step realizing the child's outcome.
    assert!(flat.connections.contains(&wire("sub/inner/inner/fit", "dataset", "review", "material")));
    // A prerequisite on the composite applies to its steps.
    assert!(
        flat.connections.contains(&WorkflowConnection::Prerequisite {
            before: "sub/inner/inner/fit".into(),
            after: "after".into()
        })
    );
    let direct = flat.outcomes.iter().find(|o| o.id == "direct").unwrap();
    assert_eq!(direct.from, StepPort { step: "sub/inner/inner/fit".into(), port: "dataset".into() });
    assert!(flat.steps.iter().all(|s| s.composite.is_none()), "no composite left");
    assert!(flat.inputs.iter().all(|i| i.id == "data"), "only the parent's inputs");
    // No composites: unchanged.
    let plain = expand_workflow(&audit(), &resolve).unwrap();
    assert_eq!(plain.definition, audit());
    // Refusals mirror validation.
    let lib = Lib::new(vec![(&ids[0], 1, wrap(&ids[0], 1))]);
    let resolve = |id: &WorkflowId, rev: u64| lib.get(id, rev);
    assert!(expand_workflow(&parent(&ids[0], 1), &resolve).is_err(), "recursion");
    assert!(expand_workflow(&parent(&ids[1], 1), &resolve).is_err(), "missing");
}

#[test]
fn expansion_keeps_conditions_pointing_at_flat_steps() {
    let c = WorkflowId::mint();
    let lib = Lib::new(vec![(&c, 1, child_fit())]);
    let resolve = |id: &WorkflowId, rev: u64| lib.get(id, rev);
    let mut def = parent(&c, 1);
    def.steps[1].condition = Some(StepCondition {
        source: StepPort { step: "sub".into(), port: "fitted".into() },
        test: ConditionTest::JsonCompare {
            pointer: "/ok".into(),
            op: CompareOp::Eq,
            value: ConditionValue::Boolean(true),
        },
        negate: false,
    });
    assert_eq!(errors(&validate_with(&def, &lib)), Vec::<String>::new());
    let x = expand_workflow(&def, &resolve).unwrap();
    let review = x.definition.steps.iter().find(|s| s.id == "review").unwrap();
    assert_eq!(review.condition.as_ref().unwrap().source, StepPort { step: "sub/fit".into(), port: "dataset".into() });
}

#[test]
fn verdict_classes_and_predicates() {
    assert_eq!(verdict_class("Checked"), Some("Checked"));
    assert_eq!(verdict_class("Checked with nonstandard axioms"), Some("Checked with nonstandard axioms"));
    assert_eq!(verdict_class("Incomplete (2 sorry)"), Some("Incomplete"));
    assert_eq!(verdict_class("Errors (3)"), Some("Errors"));
    assert_eq!(verdict_class("Partially checked: timeout"), Some("Partially checked"));
    assert_eq!(verdict_class("Checked — audit list truncated: 5 of 9 declarations listed"), Some("Checked"));
    assert_eq!(verdict_class("Checkedness"), None);
    assert_eq!(verdict_class("Not checked"), None);
    let checked = ConditionTest::VerdictIn { verdicts: vec!["Checked".into()] };
    assert_eq!(evaluate_condition(&checked, false, Observed::Verdict("Checked")), Ok(true));
    assert_eq!(evaluate_condition(&checked, false, Observed::Verdict("Errors (1)")), Ok(false));
    assert_eq!(evaluate_condition(&checked, true, Observed::Verdict("Errors (1)")), Ok(true));
    assert!(evaluate_condition(&checked, false, Observed::Missing).is_err(), "unreadable source fails clearly");
    let present = ConditionTest::OutputPresent;
    assert_eq!(evaluate_condition(&present, false, Observed::Missing), Ok(false));
    assert_eq!(evaluate_condition(&present, false, Observed::Produced), Ok(true));
    let doc = serde_json::json!({ "fit": { "r2": 0.97, "ok": true, "name": "linear" } });
    let cmp = |pointer: &str, op: CompareOp, value: ConditionValue| {
        evaluate_condition(
            &ConditionTest::JsonCompare { pointer: pointer.into(), op, value },
            false,
            Observed::Json(&doc),
        )
    };
    let num = |s: &str| ConditionValue::Number(s.into());
    assert_eq!(cmp("/fit/r2", CompareOp::Ge, num("0.95")), Ok(true));
    assert_eq!(cmp("/fit/r2", CompareOp::Lt, num("0.95")), Ok(false));
    assert_eq!(cmp("/fit/r2", CompareOp::Eq, num("0.97")), Ok(true));
    assert_eq!(cmp("/fit/ok", CompareOp::Eq, ConditionValue::Boolean(true)), Ok(true));
    assert_eq!(cmp("/fit/name", CompareOp::Ne, ConditionValue::Text("linear".into())), Ok(false));
    assert!(cmp("/fit/missing", CompareOp::Eq, num("1")).is_err());
    assert!(cmp("/fit/name", CompareOp::Gt, num("1")).is_err(), "type mismatch fails clearly");
    assert!(cmp("/fit", CompareOp::Eq, num("1")).is_err());
}

// ---------------------------------------------------------------- persistence

fn service() -> (tempfile::TempDir, tempfile::TempDir, std::sync::Arc<crate::service::GraphService>) {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let graph = crate::service::GraphService::new(crate::service::GraphConfig { data_dir: data.path().to_path_buf() });
    let status = graph.attach(&std::fs::canonicalize(project.path()).unwrap());
    assert!(status.is_usable(), "{status:?}");
    (data, project, graph)
}

fn save(
    graph: &crate::service::GraphService,
    id: Option<&WorkflowId>,
    expected: Option<u64>,
    def: &WorkflowDefinition,
) -> SavedWorkflow {
    graph
        .save_workflow(
            &crate::model::Origin::User,
            &SaveWorkflowRequest { id: id.cloned(), expected_revision: expected, definition: def.clone() },
            &ValidationContext::default(),
        )
        .unwrap()
}

#[test]
fn saving_resolves_composites_from_the_store_and_never_touches_children() {
    let (_d, _p, graph) = service();
    let child = save(&graph, None, None, &child_fit());
    assert_eq!(child.revision, 1);
    let saved = save(&graph, None, None, &parent(&child.id, 1));
    assert_eq!(saved.validation.state, DraftState::WellFormed, "{:?}", saved.validation.diagnostics);
    assert!(!saved.validation.diagnostics.iter().any(|d| d.code == "workflow.compositeUnchecked"));
    let plan: Vec<&str> = saved.validation.plan.iter().map(|p| p.step.as_str()).collect();
    assert_eq!(plan, vec!["sub/fit", "review"]);
    // The child is unchanged by saving its parent.
    let child_now = graph.workflow(&child.id, None).unwrap();
    assert_eq!((child_now.revision, child_now.hash.as_str()), (1, child.hash.as_str()));
    assert_eq!(child_now.revisions.len(), 1);

    // Editing the child makes revision 2; the parent keeps its pinned revision 1.
    let mut edited = child_fit();
    edited.steps[0].payload = Some(StepPayload::Code { language: CodeLanguage::Python, source: "print(2)".into() });
    let child2 = save(&graph, Some(&child.id), Some(1), &edited);
    assert_eq!(child2.revision, 2);
    let parent_now = graph.workflow(&saved.id, None).unwrap();
    assert_eq!(parent_now.revision, 1, "the parent was not re-saved");
    assert_eq!(
        parent_now.definition.steps[0].composite,
        Some(CompositeRef { workflow: child.id.clone(), revision: 1 })
    );
    assert_eq!(graph.workflow_definition(&child.id, 1), Some(child_fit()), "revision 1 is immutable");
    // "Use latest revision" is an ordinary parent edit: a new parent revision.
    let mut latest = parent(&child.id, 2);
    latest.title = "Parent".into();
    let parent2 = save(&graph, Some(&saved.id), Some(1), &latest);
    assert_eq!(parent2.revision, 2);
    assert_eq!(parent2.validation.state, DraftState::WellFormed, "{:?}", parent2.validation.diagnostics);
    assert_eq!(graph.workflow(&saved.id, Some(1)).unwrap().definition, parent(&child.id, 1));
}

#[test]
fn saving_refuses_nothing_but_records_composite_problems() {
    let (_d, _p, graph) = service();
    // A reference to nothing is saved as an incomplete draft.
    let missing = save(&graph, None, None, &parent(&WorkflowId::mint(), 1));
    assert_eq!(missing.validation.state, DraftState::Incomplete);
    assert!(missing.validation.diagnostics.iter().any(|d| d.code == "workflow.compositeMissing"));
    // A workflow cannot include an earlier revision of itself.
    let p = save(&graph, None, None, &child_fit());
    let mut selfish = parent(&p.id, 1);
    selfish.title = "Self".into();
    let p2 = save(&graph, Some(&p.id), Some(1), &selfish);
    assert_eq!(p2.revision, 2);
    assert!(
        p2.validation.diagnostics.iter().any(|d| d.code == "workflow.compositeRecursive"),
        "{:?}",
        p2.validation.diagnostics
    );
    // Any revision of an enclosing workflow counts: a@2 -> b@1 -> a@1 is refused.
    let a = save(&graph, None, None, &child_fit());
    let b = save(&graph, None, None, &wrap(&a.id, 1));
    let a2 = save(&graph, Some(&a.id), Some(1), &wrap(&b.id, 1));
    assert!(a2.validation.diagnostics.iter().any(|d| d.code == "workflow.compositeRecursive"));
    let looped = save(&graph, None, None, &parent(&a.id, 2));
    assert!(looped.validation.diagnostics.iter().any(|d| d.code == "workflow.compositeRecursive"));
}
