//! R-m9 (independent review of M9): the fixed condition predicates.
//!
//! Product-failing reproductions are `#[ignore = "R-m9 Fn: ..."]`; run them
//! with `cargo test -p exposition-graph --test review_m9_conditions -- --ignored`.

use exposition_graph::workflow::{
    CompareOp, ConditionTest, ConditionValue, Observed, evaluate_condition, verdict_class,
};
use serde_json::json;

fn compare(pointer: &str, op: CompareOp, number: &str) -> ConditionTest {
    ConditionTest::JsonCompare { pointer: pointer.into(), op, value: ConditionValue::Number(number.into()) }
}

/// R-m9 F5: `ConditionValue::Number` is documented as "a decimal number
/// written as text (exact; no float in the definition)", but the comparison
/// converts both sides to f64. Integers above 2^53 (counts, nanosecond
/// timestamps, 64-bit ids) and long decimals compare equal when they differ.
#[test]
fn review_m9_f5_number_comparison_is_exact() {
    let doc = json!({ "count": 9_007_199_254_740_993u64, "ns": 1_700_000_000_000_000_001u64 });
    let eq = |pointer: &str, n: &str| {
        evaluate_condition(&compare(pointer, CompareOp::Eq, n), false, Observed::Json(&doc)).unwrap()
    };
    let gt = |pointer: &str, n: &str| {
        evaluate_condition(&compare(pointer, CompareOp::Gt, n), false, Observed::Json(&doc)).unwrap()
    };
    let observed = (
        eq("/count", "9007199254740992"),
        gt("/count", "9007199254740992"),
        eq("/ns", "1700000000000000000"),
        eq("/count", "9007199254740992.4"),
    );
    eprintln!("(count == 2^53, count > 2^53, ns == ns-1, count == 2^53+0.4) = {observed:?}");
    assert!(!observed.0, "9007199254740993 is not equal to 9007199254740992");
    assert!(observed.1, "9007199254740993 is greater than 9007199254740992");
    assert!(!observed.2, "1700000000000000001 is not equal to 1700000000000000000");
    assert!(!observed.3, "an integer is not equal to a non-integer decimal");
}

/// Held: verdict tests read only the class of a real audit label; prose and
/// look-alike labels never match a stronger class.
#[test]
fn review_m9_held_verdict_classes_do_not_promote_look_alikes() {
    let class = |label: &str| verdict_class(label);
    assert_eq!(class("Checked"), Some("Checked"));
    assert_eq!(class("Checked with nonstandard axioms"), Some("Checked with nonstandard axioms"));
    assert_eq!(class("Checkedish"), None);
    assert_eq!(class("Checked!"), None);
    assert_eq!(class("checked"), None, "labels are exact, case included");
    assert_eq!(class("Errors (2)"), Some("Errors"));
    assert_eq!(class("Partially checked: the server stopped"), Some("Partially checked"));
    assert_eq!(class("Unverified: Checked"), Some("Unverified"));
    let test = ConditionTest::VerdictIn { verdicts: vec!["Checked".into()] };
    for label in ["Checked with nonstandard axioms", "Incomplete (1 sorry)", "Unverified: Checked", "Checked?"] {
        assert!(!evaluate_condition(&test, false, Observed::Verdict(label)).unwrap(), "{label}");
    }
    // A missing or non-evidence source is an error, never a silent false.
    assert!(evaluate_condition(&test, false, Observed::Missing).is_err());
    assert!(evaluate_condition(&test, false, Observed::Produced).is_err());
    assert!(evaluate_condition(&test, true, Observed::Produced).is_err(), "negation does not turn an error into true");
}

/// Held: a JSON comparison of the wrong kind or on a missing pointer is an
/// error (the gated step fails clearly), never false.
#[test]
fn review_m9_held_json_comparisons_fail_closed() {
    let doc = json!({ "r2": 0.93, "name": "fit", "flag": true, "list": [1], "nul": null, "big": 1e300 });
    let err = |t: ConditionTest| evaluate_condition(&t, true, Observed::Json(&doc)).is_err();
    assert!(err(compare("/missing", CompareOp::Eq, "1")));
    assert!(err(compare("/name", CompareOp::Lt, "1")));
    assert!(err(compare("/flag", CompareOp::Eq, "1")));
    assert!(err(compare("/list", CompareOp::Eq, "1")));
    assert!(err(compare("/nul", CompareOp::Eq, "0")));
    assert!(err(ConditionTest::JsonCompare {
        pointer: "/name".into(),
        op: CompareOp::Ge,
        value: ConditionValue::Text("fit".into())
    }));
    assert!(evaluate_condition(&compare("/big", CompareOp::Gt, "1e299"), false, Observed::Json(&doc)).unwrap());
}
