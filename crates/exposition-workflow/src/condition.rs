//! Reading a validated output for a step condition or an `until` predicate.
//!
//! The predicates are fixed (no expression evaluator) and decided by the
//! definition's own evaluator ([`evaluate_condition`]), so validation and the
//! runner agree on what a test means. This module observes the source safely:
//! a JSON dataset is at most [`MAX_CONDITION_JSON`] bytes and is re-hashed
//! against the digest recorded when it was validated (an altered file is
//! refused); formal evidence contributes only its verdict label; a review
//! report is prose and can never satisfy a verdict test. A source that cannot
//! be read is an error, never "false".

use std::path::Path;

use exposition_graph::workflow::{
    CompareOp, ConditionTest, ConditionValue, MAX_CONDITION_JSON_BYTES, Observed, evaluate_condition,
};

use crate::adapters::bare_hash;
use crate::model::WorkflowOutputValue;
use crate::plan::{hash_project_file, resolve};

/// A JSON dataset a condition reads is at most this large.
pub const MAX_CONDITION_JSON: u64 = MAX_CONDITION_JSON_BYTES as u64;

/// Why a predicate could not be evaluated (stable code, message).
pub type Unreadable = (&'static str, String);

fn unreadable(message: impl Into<String>) -> Unreadable {
    ("workflow.conditionUnreadable", message.into())
}

fn op_text(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Eq => "=",
        CompareOp::Ne => "≠",
        CompareOp::Lt => "<",
        CompareOp::Le => "≤",
        CompareOp::Gt => ">",
        CompareOp::Ge => "≥",
    }
}

/// The test in plain language (for reasons shown to the user).
pub fn describe(test: &ConditionTest) -> String {
    describe_as(test, false)
}

/// The opposite comparison (`not <` is `≥`, …).
fn negated(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Eq => CompareOp::Ne,
        CompareOp::Ne => CompareOp::Eq,
        CompareOp::Lt => CompareOp::Ge,
        CompareOp::Le => CompareOp::Gt,
        CompareOp::Gt => CompareOp::Le,
        CompareOp::Ge => CompareOp::Lt,
    }
}

/// The test, or (`negate`) its negation, in plain language: "the output is
/// not produced", "the verdict is not one of …", "the value at /x ≥ 3".
pub fn describe_as(test: &ConditionTest, negate: bool) -> String {
    let not = if negate { "not " } else { "" };
    match test {
        ConditionTest::OutputPresent => format!("the output is {not}produced"),
        ConditionTest::VerdictIn { verdicts } => {
            format!(
                "the verdict is {not}one of {}",
                verdicts.iter().map(|v| format!("“{v}”")).collect::<Vec<_>>().join(", ")
            )
        }
        ConditionTest::JsonCompare { pointer, op, value } => {
            let value = match value {
                ConditionValue::Number(n) => n.clone(),
                ConditionValue::Text(t) => format!("“{t}”"),
                ConditionValue::Boolean(b) => b.to_string(),
            };
            let op = if negate { negated(*op) } else { *op };
            format!("the value at {pointer} {} {value}", op_text(op))
        }
    }
}

/// Read the JSON dataset `value` names, exactly as validated.
fn read_json(root: &Path, value: &WorkflowOutputValue, what: &str) -> Result<serde_json::Value, Unreadable> {
    let WorkflowOutputValue::File { path, sha256, bytes, .. } = value else {
        return Err(unreadable(format!("{what} is not a dataset file")));
    };
    if *bytes > MAX_CONDITION_JSON {
        return Err(unreadable(format!("{what} ({path}) is larger than {} KiB", MAX_CONDITION_JSON / 1024)));
    }
    let (found, size) = hash_project_file(root, path).map_err(unreadable)?;
    if found != bare_hash(sha256) || size > MAX_CONDITION_JSON {
        return Err(("workflow.outputChanged", format!("{path} was altered after it was validated")));
    }
    let resolved = resolve(root, path).map_err(unreadable)?;
    let content = std::fs::read(resolved).map_err(|e| unreadable(format!("{path} cannot be read: {e}")))?;
    // The bytes read are exactly the validated ones.
    if crate::adapters::sha256_hex(&content) != bare_hash(sha256) {
        return Err(("workflow.outputChanged", format!("{path} was altered after it was validated")));
    }
    serde_json::from_slice(&content).map_err(|e| unreadable(format!("{path} is not JSON ({e})")))
}

/// Decide `test` (negated when `negate`) on `value` (`None`: the output was
/// not produced): whether the step should run / the predicate holds, and
/// what was observed, in plain language.
pub fn evaluate(
    root: &Path,
    test: &ConditionTest,
    negate: bool,
    value: Option<&WorkflowOutputValue>,
    what: &str,
) -> Result<(bool, String), Unreadable> {
    let json;
    let (observed, observation) = match (test, value) {
        (_, None) => (Observed::Missing, format!("{what} was not produced")),
        (ConditionTest::OutputPresent, Some(_)) => (Observed::Produced, format!("{what} was produced")),
        (ConditionTest::VerdictIn { .. }, Some(WorkflowOutputValue::FormalEvidence { verdict, .. })) => {
            (Observed::Verdict(verdict), format!("the verdict of {what} is “{verdict}”"))
        }
        (ConditionTest::VerdictIn { .. }, Some(_)) => {
            return Err(unreadable(format!("{what} is not formal evidence, so it has no verdict")));
        }
        (ConditionTest::JsonCompare { pointer, .. }, Some(v)) => {
            json = read_json(root, v, what)?;
            let shown = match json.pointer(pointer) {
                Some(serde_json::Value::String(s)) => format!("“{s}”"),
                Some(found) => found.to_string(),
                None => return Err(unreadable(format!("{what} has no value at {pointer}"))),
            };
            (Observed::Json(&json), format!("the value at {pointer} of {what} is {shown}"))
        }
    };
    let result = evaluate_condition(test, negate, observed).map_err(|m| unreadable(format!("{what}: {m}")))?;
    Ok((result, observation))
}

#[cfg(test)]
mod tests {
    use exposition_core::path::WorkspacePath;

    use super::*;

    fn dataset(root: &Path, name: &str, content: &str) -> WorkflowOutputValue {
        std::fs::write(root.join(name), content).unwrap();
        WorkflowOutputValue::File {
            path: WorkspacePath::parse(name).unwrap(),
            sha256: crate::adapters::sha256_hex(content.as_bytes()),
            bytes: content.len() as u64,
            media_type: "application/json".into(),
            research_run: None,
            artifact: None,
        }
    }

    fn compare(pointer: &str, op: CompareOp, value: ConditionValue) -> ConditionTest {
        ConditionTest::JsonCompare { pointer: pointer.into(), op, value }
    }

    #[test]
    fn json_sources_are_read_as_validated() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let v = dataset(&root, "m.json", r#"{"fit":{"r2":0.93,"ok":true,"name":"lin"},"a/b":1,"list":[5]}"#);
        let run = |t: &ConditionTest, negate: bool| evaluate(&root, t, negate, Some(&v), "the metrics");
        let num = |s: &str| ConditionValue::Number(s.into());
        assert!(run(&compare("/fit/r2", CompareOp::Ge, num("0.9")), false).unwrap().0);
        assert!(!run(&compare("/fit/r2", CompareOp::Ge, num("0.9")), true).unwrap().0, "negated");
        assert!(run(&compare("/a~1b", CompareOp::Eq, num("1")), false).unwrap().0, "RFC 6901 escapes");
        assert!(run(&compare("/list/0", CompareOp::Gt, num("4.5")), false).unwrap().0);
        assert!(run(&compare("/fit/ok", CompareOp::Eq, ConditionValue::Boolean(true)), false).unwrap().0);
        assert!(run(&compare("/fit/name", CompareOp::Eq, ConditionValue::Text("lin".into())), false).unwrap().0);
        let (_, seen) = run(&compare("/fit/r2", CompareOp::Lt, num("0.9")), false).unwrap();
        assert!(seen.contains("/fit/r2") && seen.contains("0.93"), "{seen}");
        // No value, another type: unreadable, never false.
        let missing = run(&compare("/fit/missing", CompareOp::Eq, num("1")), false).unwrap_err();
        assert_eq!(missing.0, "workflow.conditionUnreadable");
        let kind = run(&compare("/fit/name", CompareOp::Eq, num("1")), true).unwrap_err();
        assert_eq!(kind.0, "workflow.conditionUnreadable");
        // Not JSON, altered after validation, too large, not a dataset.
        let text = dataset(&root, "t.json", "not json");
        let e = evaluate(&root, &compare("", CompareOp::Eq, num("1")), false, Some(&text), "x").unwrap_err();
        assert_eq!(e.0, "workflow.conditionUnreadable");
        std::fs::write(root.join("m.json"), r#"{"fit":{"r2":0.1}}"#).unwrap();
        assert_eq!(run(&compare("/fit/r2", CompareOp::Ge, num("0.9")), false).unwrap_err().0, "workflow.outputChanged");
        let big = dataset(&root, "big.json", &format!("[{}0]", "0,".repeat(600_000)));
        let e = evaluate(&root, &compare("/0", CompareOp::Eq, num("0")), false, Some(&big), "x").unwrap_err();
        assert_eq!(e.0, "workflow.conditionUnreadable");
        let report = WorkflowOutputValue::Report { text: "{}".into(), task: None };
        let e = evaluate(&root, &compare("", CompareOp::Eq, num("1")), false, Some(&report), "x").unwrap_err();
        assert_eq!(e.0, "workflow.conditionUnreadable");
    }

    #[test]
    fn verdicts_come_only_from_formal_evidence_and_presence_from_production() {
        let root = Path::new(".");
        let evidence = WorkflowOutputValue::FormalEvidence {
            file: WorkspacePath::parse("A.lean").unwrap(),
            file_sha256: "a".repeat(64),
            verdict: "Checked".into(),
            declarations: Vec::new(),
            evidence: Vec::new(),
        };
        let test = ConditionTest::VerdictIn { verdicts: vec!["Checked".into()] };
        assert!(evaluate(root, &test, false, Some(&evidence), "the audit").unwrap().0);
        assert!(evaluate(root, &test, false, None, "the audit").is_err(), "a missing audit is not a verdict");
        let report = WorkflowOutputValue::Report { text: "Checked".into(), task: None };
        assert!(evaluate(root, &test, false, Some(&report), "the review").is_err(), "prose is not a verdict");
        assert!(evaluate(root, &ConditionTest::OutputPresent, false, Some(&report), "x").unwrap().0);
        assert!(!evaluate(root, &ConditionTest::OutputPresent, false, None, "x").unwrap().0);
        assert!(evaluate(root, &ConditionTest::OutputPresent, true, None, "x").unwrap().0);
        assert_eq!(describe(&test), "the verdict is one of “Checked”");
    }

    /// Negated tests read as natural sentences, never "… does not hold".
    #[test]
    fn negations_read_naturally() {
        let verdict = ConditionTest::VerdictIn { verdicts: vec!["Checked".into(), "Incomplete".into()] };
        assert_eq!(describe_as(&verdict, true), "the verdict is not one of “Checked”, “Incomplete”");
        assert_eq!(describe_as(&ConditionTest::OutputPresent, true), "the output is not produced");
        assert_eq!(describe_as(&ConditionTest::OutputPresent, false), "the output is produced");
        let compare = |op, value| ConditionTest::JsonCompare { pointer: "/score".into(), op, value };
        let n = |s: &str| ConditionValue::Number(s.into());
        for (op, negated) in [
            (CompareOp::Eq, "≠"),
            (CompareOp::Ne, "="),
            (CompareOp::Lt, "≥"),
            (CompareOp::Le, ">"),
            (CompareOp::Gt, "≤"),
            (CompareOp::Ge, "<"),
        ] {
            assert_eq!(describe_as(&compare(op, n("0.5")), true), format!("the value at /score {negated} 0.5"));
        }
        assert_eq!(
            describe_as(&compare(CompareOp::Eq, ConditionValue::Text("ok".into())), true),
            "the value at /score ≠ “ok”"
        );
    }
}
