//! Schema /2 rules: composite steps (a saved workflow at a pinned revision),
//! run conditions, and bounded repeats; the expansion of a definition into
//! the flat step list a run plan uses; and the fixed condition predicates
//! (no expression evaluator).
//!
//! A composite step's ports come from its child: one required input per child
//! external input (same id and type) and one output per child outcome (the
//! type of the port that realizes it). Its effects are the union of the
//! child's, its containment the least contained of the child's runners (what
//! running it can require), and the child's ceiling must lie within the
//! parent's (rule 4). Recursion (by workflow id, any revision), nesting deeper
//! than [`MAX_COMPOSITE_DEPTH`], and more than [`MAX_EXPANDED_STEPS`] expanded
//! steps are refused. Without a resolver a composite is reported unchecked
//! and the draft cannot be runnable.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::registry::{Containment, OperationContract, OperationLimits, PortSpec, PortType};
use super::{
    At, AuthorityCeiling, CompareOp, CompositeRef, ConditionTest, ConditionValue, DiagnosticSeverity, Diags,
    InputSource, KNOWN_VERDICTS, PortSource, ResolveWorkflow, StepCondition, StepPort, StepRepeat, Validated,
    ValidationContext, WorkflowConnection, WorkflowDefinition, WorkflowId, WorkflowStep, truncate,
};

/// Composite levels below a definition (a child's child's child at most).
pub const MAX_COMPOSITE_DEPTH: usize = 3;
/// Steps after expanding every composite.
pub const MAX_EXPANDED_STEPS: usize = 100;
/// Composite references resolved while validating one definition (with
/// multiplicity). A valid workflow needs at most 3 per expanded step.
pub const MAX_COMPOSITE_EXPANSIONS: usize = 3 * MAX_EXPANDED_STEPS;
/// Values of a `forEach` repeat.
pub const MAX_FOR_EACH_VALUES: usize = 20;
/// Iterations of an `until` repeat.
pub const MAX_UNTIL_ITERATIONS: u32 = 10;
/// Length of a condition's JSON pointer.
pub const MAX_POINTER: usize = 256;
/// Length of a condition's text constant.
pub const MAX_CONDITION_TEXT: usize = 1000;
/// A JSON dataset a condition reads (larger: the condition fails clearly).
pub const MAX_CONDITION_JSON_BYTES: usize = 1024 * 1024;
/// The runner id of a composite step (its child steps run through their own).
pub const COMPOSITE_RUNNER: &str = "composite";

/// Codes of a child's errors that the parent reports under the same code.
const PROPAGATED: [&str; 3] = ["workflow.compositeRecursive", "workflow.compositeTooDeep", "workflow.tooManySteps"];

/// Composite expansion state shared by one validation (root and children).
pub(super) struct Nest {
    /// Workflows being expanded, outermost first (the validated workflow's own
    /// id first when known).
    stack: Vec<WorkflowId>,
    /// Composite level of the definition being validated (0: the root).
    depth: usize,
    expansions: usize,
    cache: HashMap<(WorkflowId, u64), Option<WorkflowDefinition>>,
}

impl Nest {
    pub(super) fn new(root: Option<&WorkflowId>) -> Self {
        Nest { stack: root.into_iter().cloned().collect(), depth: 0, expansions: 0, cache: HashMap::new() }
    }
}

/// A resolved, well-formed composite child.
pub(super) struct Child {
    pub contract: OperationContract,
    pub plan: Vec<super::PlannedStep>,
    pub leaves: usize,
}

fn rank(c: Containment) -> u8 {
    match c {
        Containment::ApplicationService => 0,
        Containment::ReadOnlyModel => 1,
        Containment::RestrictedContainer => 2,
        Containment::NativeAccount => 3,
    }
}

/// Resolve and validate a composite step's child; report problems on `step`.
pub(super) fn resolve_composite(
    d: &mut Diags,
    step: &WorkflowStep,
    reference: &CompositeRef,
    parent_ceiling: &AuthorityCeiling,
    base: OperationContract,
    context: &ValidationContext<'_>,
    nest: &mut Nest,
) -> Option<Child> {
    let target = format!("workflow {} revision {}", reference.workflow, reference.revision);
    let Some(resolve) = context.resolve_workflow else {
        d.warning(
            "workflow.compositeUnchecked",
            None,
            format!("step “{}” runs saved {target}, which was not checked here; it cannot run until it is", step.label),
        )
        .step(&step.id);
        return None;
    };
    if reference.revision == 0 {
        d.error("workflow.compositeMissing", None, format!("step “{}”: revisions start at 1", step.label))
            .step(&step.id);
        return None;
    }
    if nest.stack.contains(&reference.workflow) {
        d.error(
            "workflow.compositeRecursive",
            None,
            format!(
                "step “{}” would run {target} inside itself; a workflow cannot include itself, directly or through another composite",
                step.label
            ),
        )
        .step(&step.id);
        return None;
    }
    if nest.depth >= MAX_COMPOSITE_DEPTH {
        d.error(
            "workflow.compositeTooDeep",
            None,
            format!("step “{}” nests composite workflows more than {MAX_COMPOSITE_DEPTH} levels deep", step.label),
        )
        .step(&step.id);
        return None;
    }
    nest.expansions += 1;
    if nest.expansions > MAX_COMPOSITE_EXPANSIONS {
        d.error(
            "workflow.tooManySteps",
            None,
            format!("the workflow expands more than {MAX_COMPOSITE_EXPANSIONS} composite references"),
        )
        .step(&step.id);
        return None;
    }
    let child = nest
        .cache
        .entry((reference.workflow.clone(), reference.revision))
        .or_insert_with(|| resolve(&reference.workflow, reference.revision))
        .clone();
    let Some(child) = child else {
        d.error(
            "workflow.compositeMissing",
            None,
            format!("step “{}” runs {target}, which does not exist in this project", step.label),
        )
        .step(&step.id);
        return None;
    };
    // Inside a parent, the child's external inputs come from the parent's bindings.
    let mut part = child.clone();
    for input in &mut part.inputs {
        input.source = InputSource::Unset;
    }
    let inner_context = ValidationContext {
        ceiling: None,
        file_exists: None,
        resolve_workflow: context.resolve_workflow,
        workflow_id: None,
    };
    nest.stack.push(reference.workflow.clone());
    nest.depth += 1;
    let inner = super::validate_in(&part, &inner_context, nest);
    nest.depth -= 1;
    nest.stack.pop();

    let place = format!("“{}” (revision {})", truncate(&child.title, 80), reference.revision);
    let mut broken = false;
    let mut reported: BTreeSet<&str> = BTreeSet::new();
    let mut other: Vec<&str> = Vec::new();
    for x in inner
        .validation
        .diagnostics
        .iter()
        .filter(|x| x.severity == DiagnosticSeverity::Error && x.code != "workflow.inputUnset")
    {
        broken = true;
        if let Some(code) = PROPAGATED.iter().find(|c| **c == x.code) {
            if reported.insert(code) {
                d.error(code, x.rule, format!("step “{}” runs {place}: {}", step.label, x.message)).step(&step.id);
            }
        } else {
            other.push(&x.message);
        }
    }
    if !other.is_empty() {
        d.error(
            "workflow.compositeNotWellFormed",
            None,
            format!(
                "step “{}” runs {place}, which is not well-formed ({} error{}: {})",
                step.label,
                other.len(),
                if other.len() == 1 { "" } else { "s" },
                other.iter().take(3).copied().collect::<Vec<_>>().join("; ")
            ),
        )
        .step(&step.id);
    }
    if broken {
        return None;
    }
    if !child.ceiling.within(parent_ceiling) {
        d.error(
            "workflow.compositeCeiling",
            Some(4),
            format!(
                "step “{}” runs {place}, whose ceiling allows more than this workflow's (execution, model requests, retention, or proposals)",
                step.label
            ),
        )
        .step(&step.id);
    }
    Some(Child {
        contract: composite_contract(base, &child, reference.revision, &inner),
        plan: inner.validation.plan,
        leaves: inner.leaves,
    })
}

/// The composite step's contract, from its validated child.
fn composite_contract(
    mut c: OperationContract,
    child: &WorkflowDefinition,
    revision: u64,
    inner: &Validated,
) -> OperationContract {
    c.title = format!("Saved workflow “{}” (revision {revision})", truncate(&child.title, 80));
    c.inputs = child
        .inputs
        .iter()
        .map(|i| PortSpec {
            name: i.id.clone(),
            label: i.label.clone(),
            types: vec![i.port_type],
            required: true,
            multiple: false,
        })
        .collect();
    c.outputs = child
        .outcomes
        .iter()
        .filter_map(|o| {
            let ty = *inner.contracts.get(&o.from.step)?.output(&o.from.port)?.types.first()?;
            Some(PortSpec {
                name: o.id.clone(),
                label: o.label.clone(),
                types: vec![ty],
                required: false,
                multiple: false,
            })
        })
        .collect();
    c.effects = inner.validation.effects.clone();
    c.runner.allows = c.effects.clone();
    c.runner.containment = inner
        .contracts
        .values()
        .map(|x| x.runner.containment)
        .max_by_key(|x| rank(*x))
        .unwrap_or(Containment::ApplicationService);
    c.limits = inner.contracts.values().fold(OperationLimits { max_seconds: 0, max_output_bytes: 0 }, |acc, x| {
        OperationLimits {
            max_seconds: acc.max_seconds.saturating_add(x.limits.max_seconds),
            max_output_bytes: acc.max_output_bytes.saturating_add(x.limits.max_output_bytes),
        }
    });
    c
}

// ---------------------------------------------------------------- conditions and repeats

/// Whether a test can read an output of this type.
pub(super) fn test_fits(test: &ConditionTest, ty: PortType) -> Result<(), String> {
    match test {
        ConditionTest::VerdictIn { .. } if ty != PortType::FormalEvidence => {
            Err(format!("a verdict test reads formal evidence, not a {}", ty.title()))
        }
        ConditionTest::JsonCompare { .. } if ty != PortType::Dataset => {
            Err(format!("a JSON comparison reads a dataset (a JSON file), not a {}", ty.title()))
        }
        _ => Ok(()),
    }
}

/// An RFC 6901 JSON pointer of bounded length.
pub(super) fn pointer_problem(pointer: &str) -> Option<String> {
    if pointer.len() > MAX_POINTER {
        return Some(format!("the JSON pointer is longer than {MAX_POINTER} characters"));
    }
    if pointer.chars().any(char::is_control) {
        return Some("the JSON pointer contains control characters".into());
    }
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Some("a JSON pointer is empty (the whole document) or starts with “/”".into());
    }
    let bytes = pointer.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'~' && !matches!(bytes.get(i + 1), Some(b'0' | b'1')) {
            return Some("in a JSON pointer “~” is written “~0” and “/” inside a name “~1”".into());
        }
    }
    None
}

/// A JSON number (no leading zeros, no NaN/infinity), at most 64 characters.
pub(super) fn number_ok(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let b = s.as_bytes();
    let mut i = usize::from(b[0] == b'-');
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    let int_start = i;
    let n = digits(&mut i);
    if n == 0 || (n > 1 && b[int_start] == b'0') {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// What is wrong with a test, if anything.
pub(super) fn test_problem(test: &ConditionTest) -> Option<String> {
    match test {
        ConditionTest::VerdictIn { verdicts } => {
            if verdicts.is_empty() {
                return Some("name at least one audit verdict".into());
            }
            if verdicts.len() > KNOWN_VERDICTS.len() {
                return Some(format!("name at most {} verdicts", KNOWN_VERDICTS.len()));
            }
            let mut seen = BTreeSet::new();
            for v in verdicts {
                if !KNOWN_VERDICTS.contains(&v.as_str()) {
                    return Some(format!(
                        "{:?} is not an audit verdict (one of: {})",
                        truncate(v, 60),
                        KNOWN_VERDICTS.join(", ")
                    ));
                }
                if !seen.insert(v) {
                    return Some(format!("{v:?} is listed twice"));
                }
            }
            None
        }
        ConditionTest::JsonCompare { pointer, op, value } => {
            if let Some(p) = pointer_problem(pointer) {
                return Some(p);
            }
            let ordering = matches!(op, CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge);
            match value {
                ConditionValue::Number(n) => (!number_ok(n) || Decimal::parse(n).is_none())
                    .then(|| format!("{:?} is not a decimal number (exponents of at most 6 digits)", truncate(n, 40))),
                ConditionValue::Text(t) => {
                    if ordering {
                        Some("text can only be compared for equality".into())
                    } else {
                        crate::validate::check_text(t, MAX_CONDITION_TEXT, "the text").err()
                    }
                }
                ConditionValue::Boolean(_) => ordering.then(|| "true/false can only be compared for equality".into()),
            }
        }
        ConditionTest::OutputPresent => None,
    }
}

/// Check every step's condition and repeat; a condition adds an ordering
/// (and contributing) edge from its source to its step.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_conditions_and_repeats(
    d: &mut Diags,
    def: &WorkflowDefinition,
    contracts: &BTreeMap<String, OperationContract>,
    known: &BTreeSet<&str>,
    repeated: &BTreeSet<&str>,
    exec_edges: &mut BTreeSet<(String, String)>,
    data_edges: &mut BTreeSet<(String, String)>,
) {
    for step in &def.steps {
        if !known.contains(step.id.as_str()) {
            continue;
        }
        let composite = step.operation.id == super::COMPOSITE_OPERATION || step.composite.is_some();
        if let Some(condition) = &step.condition {
            if composite {
                d.error(
                    "workflow.conditionInvalid",
                    None,
                    format!(
                        "composite step “{}” cannot carry a condition; put conditions on its child's steps",
                        step.label
                    ),
                )
                .step(&step.id);
            } else {
                check_condition(d, step, condition, contracts, known, repeated, exec_edges, data_edges);
            }
        }
        let Some(repeat) = &step.repeat else { continue };
        if composite {
            d.error("workflow.repeatInvalid", None, format!("composite step “{}” cannot repeat", step.label))
                .step(&step.id);
            continue;
        }
        let Some(contract) = contracts.get(&step.id) else { continue };
        // Association proposals are reviewed and decided once per step; the
        // instances of a repeated step could never be decided.
        if contract.id == super::registry::FORMAL_ASSOCIATE {
            d.error(
                "workflow.repeatInvalid",
                None,
                format!(
                    "step “{}” proposes evidence associations, which are reviewed and decided once per step, so it cannot repeat",
                    step.label
                ),
            )
            .step(&step.id);
            continue;
        }
        let problem = match repeat {
            StepRepeat::ForEach { parameter, values } => match contract.parameter(parameter) {
                None => {
                    Some(format!("{} has no parameter {:?} to repeat over", contract.title, truncate(parameter, 40)))
                }
                Some(_) if step.parameters.contains_key(parameter) => {
                    Some(format!("“{parameter}” is set both on the step and by its repetition; keep one"))
                }
                Some(_) if values.is_empty() || values.len() > MAX_FOR_EACH_VALUES => {
                    Some(format!("repeat over 1–{MAX_FOR_EACH_VALUES} values (it has {})", values.len()))
                }
                Some(spec) => values.iter().enumerate().find_map(|(i, v)| {
                    super::parameter_problem(&spec.kind, v)
                        .or_else(|| super::registry::code_parameter_problem(contract, v))
                        .map(|p| format!("value {} of “{}” {p}", i + 1, spec.label))
                }),
            },
            StepRepeat::Until { port, test, max_iterations, .. } => match contract.output(port) {
                None => Some(format!("{} has no output {:?} to test", contract.title, truncate(port, 40))),
                Some(_) if !(1..=MAX_UNTIL_ITERATIONS).contains(max_iterations) => {
                    Some(format!("repeat 1–{MAX_UNTIL_ITERATIONS} times (it says {max_iterations})"))
                }
                Some(out) => test_fits(test, out.types[0]).err().or_else(|| test_problem(test)),
            },
        };
        if let Some(problem) = problem {
            d.error("workflow.repeatInvalid", None, format!("step “{}”: {problem}", step.label)).step(&step.id);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_condition(
    d: &mut Diags,
    step: &WorkflowStep,
    condition: &StepCondition,
    contracts: &BTreeMap<String, OperationContract>,
    known: &BTreeSet<&str>,
    repeated: &BTreeSet<&str>,
    exec_edges: &mut BTreeSet<(String, String)>,
    data_edges: &mut BTreeSet<(String, String)>,
) {
    let StepPort { step: source, port } = &condition.source;
    let source_problem = if source == &step.id {
        Some("a step's condition cannot read its own output".to_owned())
    } else if !known.contains(source.as_str()) {
        Some(format!("its condition reads unknown step {:?}", truncate(source, 40)))
    } else if repeated.contains(source.as_str()) {
        Some(format!("its condition reads step {source:?}, which repeats; a condition reads one value"))
    } else {
        let Some(contract) = contracts.get(source) else {
            // An unknown operation was reported already; an unchecked
            // composite still orders the step.
            exec_edges.insert((source.clone(), step.id.clone()));
            data_edges.insert((source.clone(), step.id.clone()));
            return;
        };
        match contract.output(port) {
            None => Some(format!("{} has no output {:?}", contract.title, truncate(port, 40))),
            Some(out) => test_fits(&condition.test, out.types[0]).err(),
        }
    };
    if let Some(problem) = source_problem {
        d.error("workflow.conditionSource", None, format!("step “{}”: {problem}", step.label))
            .step(&step.id)
            .port(port);
        return;
    }
    exec_edges.insert((source.clone(), step.id.clone()));
    data_edges.insert((source.clone(), step.id.clone()));
    if let Some(problem) = test_problem(&condition.test) {
        d.error("workflow.conditionInvalid", None, format!("step “{}”: {problem}", step.label)).step(&step.id);
    }
}

// ---------------------------------------------------------------- predicates

/// The class of an audit verdict label: the longest known verdict it starts
/// with, followed by nothing, “ (”, “:”, or “ —” (e.g. "Errors (2)" →
/// "Errors", "Checked — audit list truncated …" → "Checked").
pub fn verdict_class(label: &str) -> Option<&'static str> {
    KNOWN_VERDICTS
        .iter()
        .copied()
        .filter(|k| {
            label.strip_prefix(k).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with(" (") || rest.starts_with(':') || rest.starts_with(" —")
            })
        })
        .max_by_key(|k| k.len())
}

/// What a condition's source produced, as the runner observed it after
/// validating the output.
#[derive(Clone, Copy, Debug)]
pub enum Observed<'a> {
    /// The (optional) output was not produced.
    Missing,
    /// Formal evidence with this overall verdict label.
    Verdict(&'a str),
    /// A dataset parsed as JSON (at most [`MAX_CONDITION_JSON_BYTES`]).
    Json(&'a serde_json::Value),
    /// Any other produced output.
    Produced,
}

/// Evaluate a fixed predicate. `Err` means the condition cannot be decided
/// (the step fails clearly rather than guessing).
pub fn evaluate_condition(test: &ConditionTest, negate: bool, observed: Observed<'_>) -> Result<bool, String> {
    let holds = match (test, observed) {
        (ConditionTest::OutputPresent, o) => !matches!(o, Observed::Missing),
        (_, Observed::Missing) => return Err("the output the condition reads was not produced".into()),
        (ConditionTest::VerdictIn { verdicts }, Observed::Verdict(label)) => {
            let class = verdict_class(label);
            verdicts.iter().any(|v| v == label || Some(v.as_str()) == class)
        }
        (ConditionTest::JsonCompare { pointer, op, value }, Observed::Json(doc)) => {
            let found =
                doc.pointer(pointer).ok_or_else(|| format!("{pointer:?} does not exist in the JSON document"))?;
            compare(found, *op, value).map_err(|m| format!("{pointer:?}: {m}"))?
        }
        _ => return Err("the condition cannot read this kind of output".into()),
    };
    Ok(holds != negate)
}

fn compare(found: &serde_json::Value, op: CompareOp, value: &ConditionValue) -> Result<bool, String> {
    use std::cmp::Ordering;
    let ordering = match (found, value) {
        (serde_json::Value::Number(n), ConditionValue::Number(want)) => {
            // Exactly, as decimals: never through a float (integers above
            // 2^53 and long decimals keep every digit). The document's
            // integers are exact; a fractional document number is compared
            // as the shortest decimal that reads back as its parsed value.
            let have = Decimal::parse(&n.to_string()).ok_or("the number cannot be compared")?;
            let want = Decimal::parse(want).ok_or("the constant is not a number (or is out of range)")?;
            have.cmp(&want)
        }
        (serde_json::Value::String(s), ConditionValue::Text(want)) => {
            if s == want {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        }
        (serde_json::Value::Bool(b), ConditionValue::Boolean(want)) => {
            if b == want {
                Ordering::Equal
            } else {
                Ordering::Less
            }
        }
        (other, _) => {
            let kind = match other {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "true/false",
                serde_json::Value::Number(_) => "a number",
                serde_json::Value::String(_) => "text",
                serde_json::Value::Array(_) => "a list",
                serde_json::Value::Object(_) => "an object",
            };
            return Err(format!("the value is {kind}, which this test does not compare"));
        }
    };
    let equal_only = !matches!(value, ConditionValue::Number(_));
    Ok(match op {
        CompareOp::Eq => ordering == Ordering::Equal,
        CompareOp::Ne => ordering != Ordering::Equal,
        _ if equal_only => return Err("only numbers can be ordered".into()),
        CompareOp::Lt => ordering == Ordering::Less,
        CompareOp::Le => ordering != Ordering::Greater,
        CompareOp::Gt => ordering == Ordering::Greater,
        CompareOp::Ge => ordering != Ordering::Less,
    })
}

/// A decimal number held exactly: `±digits × 10^exponent`, with no leading
/// or trailing zero digits (zero has no digits and is never negative).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Decimal {
    negative: bool,
    digits: Vec<u8>,
    exponent: i64,
}

impl Decimal {
    /// `-?digits[.digits][(e|E)[+-]digits]`; `None` otherwise or when the
    /// exponent is out of range.
    pub(super) fn parse(text: &str) -> Option<Decimal> {
        let (negative, rest) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let (mantissa, exponent) = match rest.find(['e', 'E']) {
            Some(i) => {
                let e = &rest[i + 1..];
                let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
                if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                (&rest[..i], e.parse::<i64>().ok()?)
            }
            None => (rest, 0),
        };
        let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if mantissa.contains('.') && frac.is_empty() {
            return None;
        }
        let mut digits: Vec<u8> = int.bytes().chain(frac.bytes()).map(|b| b - b'0').collect();
        let mut exponent = exponent - frac.len() as i64;
        let leading = digits.iter().take_while(|d| **d == 0).count();
        digits.drain(..leading);
        while digits.last() == Some(&0) {
            digits.pop();
            exponent += 1;
        }
        if digits.is_empty() {
            return Some(Decimal { negative: false, digits, exponent: 0 });
        }
        Some(Decimal { negative, digits, exponent })
    }

    fn sign(&self) -> i8 {
        match (self.digits.is_empty(), self.negative) {
            (true, _) => 0,
            (false, true) => -1,
            (false, false) => 1,
        }
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match self.sign().cmp(&other.sign()) {
            Ordering::Equal if self.sign() == 0 => return Ordering::Equal,
            Ordering::Equal => {}
            unequal => return unequal,
        }
        // Magnitudes: the place of the leading digit, then the digits (a
        // shorter digit string is a prefix padded with zeros).
        let top = |d: &Decimal| d.digits.len() as i64 + d.exponent;
        let magnitude = top(self).cmp(&top(other)).then_with(|| self.digits.cmp(&other.digits));
        if self.sign() < 0 { magnitude.reverse() } else { magnitude }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------- expansion

/// One step of an expanded definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedStep {
    /// Flat id: `<compositeStep>/<childStep>` for expanded children.
    pub id: String,
    /// Labels of the enclosing composite steps, outermost first.
    pub composite_path: Vec<String>,
    /// The pinned child revisions it came from, outermost first.
    pub sources: Vec<CompositeRef>,
}

/// A definition with every composite step replaced by its pinned child's
/// steps (ids prefixed, the child's external inputs bound to whatever the
/// parent bound to the composite's ports, the composite's outputs taken from
/// the steps that realize the child's outcomes, prerequisites on a composite
/// applied to every child step). Flat ids contain `/`, so the result is for
/// planning only, not for re-validation or saving. Validate first: this
/// assumes a well-formed definition and refuses what validation refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedWorkflow {
    pub definition: WorkflowDefinition,
    pub steps: Vec<ExpandedStep>,
}

/// Expand every composite step through `resolve`.
pub fn expand_workflow(def: &WorkflowDefinition, resolve: &ResolveWorkflow<'_>) -> Result<ExpandedWorkflow, String> {
    let mut budget = 0usize;
    let mut stack = Vec::new();
    let out = expand(def, resolve, &mut stack, &mut budget)?;
    if out.steps.len() > MAX_EXPANDED_STEPS {
        return Err(format!("the workflow expands to more than {MAX_EXPANDED_STEPS} steps"));
    }
    Ok(out)
}

fn expand(
    def: &WorkflowDefinition,
    resolve: &ResolveWorkflow<'_>,
    stack: &mut Vec<WorkflowId>,
    budget: &mut usize,
) -> Result<ExpandedWorkflow, String> {
    // Children first: step id → its expanded child.
    let mut children: BTreeMap<&str, ExpandedWorkflow> = BTreeMap::new();
    for step in &def.steps {
        let Some(reference) = &step.composite else { continue };
        if stack.contains(&reference.workflow) {
            return Err(format!("composite step {:?} includes its own workflow", step.id));
        }
        if stack.len() >= MAX_COMPOSITE_DEPTH {
            return Err(format!("composite step {:?} nests more than {MAX_COMPOSITE_DEPTH} levels deep", step.id));
        }
        *budget += 1;
        if *budget > MAX_COMPOSITE_EXPANSIONS {
            return Err(format!("the workflow expands more than {MAX_COMPOSITE_EXPANSIONS} composite references"));
        }
        let child = resolve(&reference.workflow, reference.revision).ok_or_else(|| {
            format!(
                "composite step {:?}: workflow {} revision {} is missing",
                step.id, reference.workflow, reference.revision
            )
        })?;
        stack.push(reference.workflow.clone());
        let expanded = expand(&child, resolve, stack, budget)?;
        stack.pop();
        children.insert(step.id.as_str(), expanded);
    }
    if children.is_empty() {
        let steps = def
            .steps
            .iter()
            .map(|s| ExpandedStep { id: s.id.clone(), composite_path: Vec::new(), sources: Vec::new() })
            .collect();
        return Ok(ExpandedWorkflow { definition: def.clone(), steps });
    }
    let prefixed = |composite: &str, id: &str| format!("{composite}/{id}");
    // A composite's output port → the flat (step, port) realizing that child outcome.
    let output_of = |step: &str, port: &str| -> Result<StepPort, String> {
        match children.get(step) {
            None => Ok(StepPort { step: step.to_owned(), port: port.to_owned() }),
            Some(child) => {
                let o = child
                    .definition
                    .outcomes
                    .iter()
                    .find(|o| o.id == port)
                    .ok_or_else(|| format!("composite step {step:?} has no output {port:?}"))?;
                Ok(StepPort { step: prefixed(step, &o.from.step), port: o.from.port.clone() })
            }
        }
    };
    let source_of = |from: &PortSource| -> Result<PortSource, String> {
        match from {
            PortSource::Step { step, port } => {
                let flat = output_of(step, port)?;
                Ok(PortSource::Step { step: flat.step, port: flat.port })
            }
            PortSource::Input { .. } => Ok(from.clone()),
        }
    };
    let mut flat = def.clone();
    flat.steps.clear();
    flat.connections.clear();
    let mut steps = Vec::new();
    for step in &def.steps {
        match children.get(step.id.as_str()) {
            None => {
                let mut s = step.clone();
                if let Some(condition) = &mut s.condition {
                    condition.source = output_of(&condition.source.step, &condition.source.port)?;
                }
                flat.steps.push(s);
                steps.push(ExpandedStep { id: step.id.clone(), composite_path: Vec::new(), sources: Vec::new() });
            }
            Some(child) => {
                let reference = step.composite.clone().ok_or("composite step without a reference")?;
                for (s, info) in child.definition.steps.iter().zip(&child.steps) {
                    let mut s = s.clone();
                    s.id = prefixed(&step.id, &s.id);
                    if let Some(condition) = &mut s.condition {
                        condition.source.step = prefixed(&step.id, &condition.source.step);
                    }
                    flat.steps.push(s);
                    let mut path = vec![step.label.clone()];
                    path.extend(info.composite_path.iter().cloned());
                    let mut sources = vec![reference.clone()];
                    sources.extend(info.sources.iter().cloned());
                    steps.push(ExpandedStep { id: prefixed(&step.id, &info.id), composite_path: path, sources });
                }
                // The child's own wiring, its inputs bound to the parent's sources.
                for c in &child.definition.connections {
                    match c {
                        WorkflowConnection::Data { from, to } => {
                            let to = StepPort { step: prefixed(&step.id, &to.step), port: to.port.clone() };
                            match from {
                                PortSource::Step { step: s, port } => flat.connections.push(WorkflowConnection::Data {
                                    from: PortSource::Step { step: prefixed(&step.id, s), port: port.clone() },
                                    to,
                                }),
                                PortSource::Input { input } => {
                                    for parent in &def.connections {
                                        if let WorkflowConnection::Data { from: pf, to: pt } = parent
                                            && pt.step == step.id
                                            && &pt.port == input
                                        {
                                            flat.connections.push(WorkflowConnection::Data {
                                                from: source_of(pf)?,
                                                to: to.clone(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        WorkflowConnection::Prerequisite { before, after } => {
                            flat.connections.push(WorkflowConnection::Prerequisite {
                                before: prefixed(&step.id, before),
                                after: prefixed(&step.id, after),
                            });
                        }
                        WorkflowConnection::Annotation { .. } => {}
                    }
                }
            }
        }
    }
    let members = |id: &str| -> Vec<String> {
        match children.get(id) {
            None => vec![id.to_owned()],
            Some(child) => child.definition.steps.iter().map(|s| prefixed(id, &s.id)).collect(),
        }
    };
    for c in &def.connections {
        match c {
            WorkflowConnection::Data { to, .. } if children.contains_key(to.step.as_str()) => {} // bound inside
            WorkflowConnection::Data { from, to } => {
                flat.connections.push(WorkflowConnection::Data { from: source_of(from)?, to: to.clone() });
            }
            WorkflowConnection::Prerequisite { before, after } => {
                for b in members(before) {
                    for a in members(after) {
                        flat.connections.push(WorkflowConnection::Prerequisite { before: b.clone(), after: a });
                    }
                }
            }
            WorkflowConnection::Annotation { from, to, .. } => {
                if !children.contains_key(from.as_str()) && !children.contains_key(to.as_str()) {
                    flat.connections.push(c.clone());
                }
            }
        }
    }
    for o in &mut flat.outcomes {
        o.from = output_of(&o.from.step, &o.from.port)?;
    }
    Ok(ExpandedWorkflow { definition: flat, steps })
}
