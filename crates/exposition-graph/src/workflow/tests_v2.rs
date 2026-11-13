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
