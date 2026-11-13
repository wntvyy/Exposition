//! A code step runs only scripts of its own language (M9 R/Julia batches):
//! an external code input bound to a batch's `script` port must have the
//! operation's language extension (rule 3, `workflow.inputTypeMismatch`).

use exposition_graph::workflow::WorkflowDefinition;
use exposition_graph::{DiagnosticSeverity, ValidationContext, validate_workflow};
use serde_json::json;

fn definition(operation: &str, language: &str, script: &str) -> WorkflowDefinition {
    let native = operation.contains("native");
    serde_json::from_value(json!({
        "schema": "exposition.workflow/1",
        "title": "Script language",
        "description": "",
        "preset": null,
        "inputs": [{"id": "script", "label": "Script", "portType": "codeSource",
                    "source": {"kind": "resource", "path": script, "version": {"kind": "atRunStart"}}}],
        "steps": [{"id": "run", "label": "Run", "operation": {"id": operation, "version": 1},
                   "parameters": {}, "payload": null}],
        "connections": [{"kind": "data", "from": {"kind": "input", "input": "script"},
                         "to": {"step": "run", "port": "script"}}],
        "outcomes": [{"id": "dataset", "kind": "transformedDataset", "label": "Dataset",
                      "from": {"step": "run", "port": "dataset"}, "optional": false}],
        "ceiling": {"execution": if native { "trustedNative" } else { "restricted" },
                    "modelRequests": false, "retainResults": false, "proposeChanges": false}
    }))
    .unwrap_or_else(|e| panic!("{language} {operation}: {e}"))
}

fn mismatches(operation: &str, script: &str) -> Vec<String> {
    let validation = validate_workflow(&definition(operation, "", script), &ValidationContext::default());
    validation
        .diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error && d.code == "workflow.inputTypeMismatch")
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn a_script_of_another_language_is_refused_by_validation() {
    for (operation, script) in [
        ("research.rBatch", "analysis/fit.py"),
        ("research.nativeR", "analysis/fit.jl"),
        ("research.juliaBatch", "analysis/fit.R"),
        ("research.nativeJulia", "analysis/fit.py"),
        ("research.pythonBatch", "analysis/fit.R"),
        ("research.nativePython", "analysis/fit.jl"),
    ] {
        let found = mismatches(operation, script);
        assert_eq!(found.len(), 1, "{operation} with {script}: {found:?}");
        assert!(found[0].contains(script), "{}", found[0]);
    }
}

#[test]
fn a_script_of_the_steps_language_is_accepted() {
    for (operation, script) in [
        ("research.rBatch", "analysis/fit.R"),
        ("research.nativeR", "analysis/fit.r"),
        ("research.juliaBatch", "analysis/fit.jl"),
        ("research.nativeJulia", "src/Model.JL"),
        ("research.pythonBatch", "analysis/fit.py"),
        ("research.nativePython", "analysis/fit.py"),
    ] {
        assert!(mismatches(operation, script).is_empty(), "{operation} with {script}");
    }
}
