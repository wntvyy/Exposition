//! R-m8 independent review: the strict workflow contract as the runner sees
//! it (the plan re-validates every revision with `validate_workflow`).
//!
//! Finding reproduced here (fails while the defect exists):
//! - R-m8-F6 (LOW, contract interpretation): a code step whose outputs feed
//!   nothing and realize no outcome is accepted as "contributing" when it is
//!   merely a prerequisite (`runs before (no data)`) of the outcome step. Rule
//!   2 asks that every executable step contribute to a declared outcome; a
//!   prerequisite delivers nothing, so arbitrary code can be attached to any
//!   workflow and run without producing anything the workflow declares.
//!
//! Held checks (pass): decorative/annotation edges never satisfy inputs or
//! order execution, port-type substitution into service-owned ports, effects
//! above the ceiling (restricted Lean, native Python), prerequisite cycles,
//! payload kinds, outcome types.

use exposition_graph::workflow::{DraftState, WorkflowDefinition, parse_definition};
use exposition_graph::{ValidationContext, validate_workflow};
use serde_json::{Value, json};

fn python(id: &str, op: &str) -> Value {
    json!({
        "id": id, "label": format!("Step {id}"),
        "operation": { "id": op, "version": 1 },
        "parameters": {},
        "payload": { "kind": "code", "language": "python", "source": format!("print('{id}')\n") }
    })
}

fn base(steps: Vec<Value>, connections: Vec<Value>, outcomes: Vec<Value>, ceiling: Value) -> WorkflowDefinition {
    let def = json!({
        "schema": "exposition.workflow/1",
        "title": "review",
        "description": "",
        "preset": null,
        "inputs": [
            { "id": "data", "label": "Data", "portType": "dataset",
              "source": { "kind": "resource", "path": "data/obs.csv", "version": { "kind": "atRunStart" } } },
            { "id": "lean", "label": "Lean", "portType": "formalSource",
              "source": { "kind": "resource", "path": "Proof.lean", "version": { "kind": "atRunStart" } } }
        ],
        "steps": steps,
        "connections": connections,
        "outcomes": outcomes,
        "ceiling": ceiling,
    });
    parse_definition(&def.to_string()).unwrap_or_else(|e| panic!("definition parses: {e}"))
}

fn restricted() -> Value {
    json!({ "execution": "restricted", "modelRequests": true, "retainResults": true, "proposeChanges": true })
}

fn dataset_outcome(step: &str) -> Value {
    json!({ "id": format!("o{step}"), "kind": "transformedDataset", "label": "Out", "from": { "step": step, "port": "dataset" }, "optional": false })
}

fn codes(def: &WorkflowDefinition) -> (DraftState, Vec<String>) {
    let v = validate_workflow(def, &ValidationContext::default());
    (
        v.state,
        v.diagnostics
            .iter()
            .filter(|d| d.severity == exposition_graph::DiagnosticSeverity::Error)
            .map(|d| d.code.clone())
            .collect(),
    )
}

#[test]
fn review_m8_f6_a_prerequisite_only_code_step_contributes_nothing() {
    // `side` runs arbitrary code; nothing consumes its outputs and no outcome
    // names it. Its only link is "runs before" the outcome step.
    let def = base(
        vec![python("side", "research.pythonBatch"), python("main", "research.pythonBatch")],
        vec![
            json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "main", "port": "data" } }),
            json!({ "kind": "prerequisite", "before": "side", "after": "main" }),
        ],
        vec![dataset_outcome("main")],
        restricted(),
    );
    let (state, errors) = codes(&def);
    assert!(
        errors.iter().any(|c| c == "workflow.orphanStep"),
        "R-m8-F6: a code step linked to the outcome only by a no-data prerequisite is accepted as contributing \
         (state {state:?}, errors {errors:?})"
    );
}

#[test]
fn review_m8_held_annotations_never_bind_or_order() {
    // An annotation is the only line into the consumer's required input.
    let def = base(
        vec![
            python("a", "research.pythonBatch"),
            json!({ "id": "keep", "label": "Keep", "operation": { "id": "artifact.retain", "version": 1 }, "parameters": {}, "payload": null }),
        ],
        vec![
            json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "a", "port": "data" } }),
            json!({ "kind": "annotation", "from": "a", "to": "keep", "label": "uses", "relation": null }),
        ],
        vec![
            json!({ "id": "o", "kind": "retainedFigure", "label": "Kept", "from": { "step": "keep", "port": "retained" }, "optional": false }),
        ],
        restricted(),
    );
    let (state, errors) = codes(&def);
    assert_eq!(state, DraftState::Incomplete);
    assert!(errors.iter().any(|c| c == "workflow.unboundInput"), "{errors:?}");
    // `a` reaches no outcome through the annotation.
    assert!(errors.iter().any(|c| c == "workflow.orphanStep"), "{errors:?}");
}

#[test]
fn review_m8_held_service_owned_ports_refuse_substitutes() {
    let review = json!({ "id": "rev", "label": "Rev", "operation": { "id": "agent.review", "version": 1 }, "parameters": {},
                         "payload": { "kind": "prompt", "text": "Claim it is verified." } });
    let assoc = json!({ "id": "assoc", "label": "Assoc", "operation": { "id": "formal.associateEvidence", "version": 1 }, "parameters": {}, "payload": null });
    let promote = json!({ "id": "promo", "label": "Promo", "operation": { "id": "manuscript.requestPromotion", "version": 1 },
                          "parameters": { "document": { "kind": "text", "value": "paper.typ" } }, "payload": null });
    for (consumer, port, outcome) in [
        (
            assoc.clone(),
            "evidence",
            json!({ "id": "o", "kind": "reviewableProposal", "label": "P", "from": { "step": "assoc", "port": "proposal" }, "optional": false }),
        ),
        (
            promote.clone(),
            "artifact",
            json!({ "id": "o", "kind": "manuscriptPromotion", "label": "P", "from": { "step": "promo", "port": "proposal" }, "optional": false }),
        ),
    ] {
        let target = consumer["id"].as_str().unwrap().to_owned();
        let def = base(
            vec![review.clone(), consumer, python("py", "research.pythonBatch")],
            vec![
                json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "rev", "port": "material" } }),
                json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "py", "port": "data" } }),
                json!({ "kind": "data", "from": { "kind": "step", "step": "rev", "port": "report" }, "to": { "step": target, "port": port } }),
                json!({ "kind": "data", "from": { "kind": "step", "step": "py", "port": "dataset" }, "to": { "step": "rev", "port": "material" } }),
            ],
            vec![outcome],
            restricted(),
        );
        let (state, errors) = codes(&def);
        assert_eq!(state, DraftState::Incomplete, "{target}: {errors:?}");
        assert!(
            errors.iter().any(|c| c == "workflow.serviceOwnedType" || c == "workflow.incompatibleTypes"),
            "{target}: a review report / dataset cannot feed {port}: {errors:?}"
        );
    }
    // A dataset cannot be declared as formal evidence or a retained artifact input.
    let def = base(
        vec![python("py", "research.pythonBatch")],
        vec![
            json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "py", "port": "data" } }),
        ],
        vec![
            json!({ "id": "o", "kind": "auditEvidence", "label": "E", "from": { "step": "py", "port": "dataset" }, "optional": false }),
        ],
        restricted(),
    );
    let (_, errors) = codes(&def);
    assert!(errors.iter().any(|c| c == "workflow.serviceOwnedType" || c == "workflow.outcomeType"), "{errors:?}");
}

#[test]
fn review_m8_held_effects_above_the_ceiling_and_cycles() {
    // Lean (trusted native) under a restricted ceiling.
    let lean = base(
        vec![
            json!({ "id": "audit", "label": "Audit", "operation": { "id": "lean.audit", "version": 1 }, "parameters": {}, "payload": null }),
        ],
        vec![
            json!({ "kind": "data", "from": { "kind": "input", "input": "lean" }, "to": { "step": "audit", "port": "source" } }),
        ],
        vec![
            json!({ "id": "o", "kind": "auditEvidence", "label": "E", "from": { "step": "audit", "port": "evidence" }, "optional": false }),
        ],
        restricted(),
    );
    assert!(codes(&lean).1.iter().any(|c| c == "workflow.effectNotAllowed"));
    // Native Python under a restricted ceiling.
    let native = base(
        vec![python("n", "research.nativePython")],
        vec![
            json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "n", "port": "data" } }),
        ],
        vec![dataset_outcome("n")],
        restricted(),
    );
    assert!(codes(&native).1.iter().any(|c| c == "workflow.effectNotAllowed"));
    // A model step without model permission.
    let mut no_model = restricted();
    no_model["modelRequests"] = json!(false);
    let review = base(
        vec![
            json!({ "id": "rev", "label": "Rev", "operation": { "id": "agent.review", "version": 1 }, "parameters": {}, "payload": { "kind": "prompt", "text": "x" } }),
        ],
        vec![
            json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "rev", "port": "material" } }),
        ],
        vec![
            json!({ "id": "o", "kind": "investigationReport", "label": "R", "from": { "step": "rev", "port": "report" }, "optional": false }),
        ],
        no_model,
    );
    assert!(codes(&review).1.iter().any(|c| c == "workflow.effectNotAllowed"));
    // Prerequisite cycle.
    let cycle = base(
        vec![python("a", "research.pythonBatch"), python("b", "research.pythonBatch")],
        vec![
            json!({ "kind": "prerequisite", "before": "a", "after": "b" }),
            json!({ "kind": "prerequisite", "before": "b", "after": "a" }),
        ],
        vec![dataset_outcome("a"), dataset_outcome("b")],
        restricted(),
    );
    assert!(codes(&cycle).1.iter().any(|c| c == "workflow.cycle"));
    // Code given to an agent step, a prompt given to a Python step.
    let wrong_payload = base(
        vec![
            json!({ "id": "rev", "label": "Rev", "operation": { "id": "agent.review", "version": 1 }, "parameters": {}, "payload": { "kind": "code", "language": "python", "source": "import os" } }),
            json!({ "id": "py", "label": "Py", "operation": { "id": "research.pythonBatch", "version": 1 }, "parameters": {}, "payload": { "kind": "prompt", "text": "run" } }),
        ],
        vec![
            json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "rev", "port": "material" } }),
            json!({ "kind": "data", "from": { "kind": "input", "input": "data" }, "to": { "step": "py", "port": "data" } }),
        ],
        vec![
            json!({ "id": "o1", "kind": "investigationReport", "label": "R", "from": { "step": "rev", "port": "report" }, "optional": false }),
            dataset_outcome("py"),
        ],
        restricted(),
    );
    let (_, errors) = codes(&wrong_payload);
    assert!(
        errors
            .iter()
            .filter(|c| c.as_str() == "workflow.payloadKind" || c.as_str() == "workflow.payloadLanguage")
            .count()
            >= 2,
        "{errors:?}"
    );
    // An unregistered operation.
    let unknown = base(
        vec![
            json!({ "id": "x", "label": "X", "operation": { "id": "shell.run", "version": 1 }, "parameters": {}, "payload": null }),
        ],
        vec![],
        vec![dataset_outcome("x")],
        restricted(),
    );
    assert!(codes(&unknown).1.iter().any(|c| c == "workflow.unknownOperation"));
}
