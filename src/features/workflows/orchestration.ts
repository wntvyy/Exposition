// Pure helpers for M9 orchestration in the workflow editor and views:
// composite steps (a step running another saved workflow at a pinned
// revision), bounded conditions, bounded repeats (forEach / until), and
// max parallel. The editor offers only valid choices; Rust validates every
// draft (`workflow.composite*`, `.conditionSource`, `.conditionInvalid`,
// `.repeatInvalid`, `.iterationFanIn`, `.tooManySteps`) and those diagnostics
// are shown inline like every other.
import type { Containment } from "../../bindings/Containment";
import type { OperationContract } from "../../bindings/OperationContract";
import type { ParameterSpec } from "../../bindings/ParameterSpec";
import type { ParamValue } from "../../bindings/ParamValue";
import type { PortType } from "../../bindings/PortType";
import {
  COMPOSITE_OPERATION,
  WORKFLOW_SCHEMA_V2,
  type CompareOp,
  type CompositeRef,
  type ConditionTest,
  type ConditionValue,
  type StepCondition,
  type StepRepeat,
  type WorkflowConcurrencyLimits,
  type WorkflowDefinition,
  type WorkflowStep,
} from "./contract";

// ----- schema and parallelism ----------------------------------------------------------------

/** Whether the definition uses any exposition.workflow/2 field. */
export function usesV2(def: WorkflowDefinition): boolean {
  if (def.execution) return true;
  return def.steps.some((s) => Boolean(s.composite || s.condition || s.repeat));
}

/**
 * A definition using any v2 field must declare schema /2; otherwise it keeps
 * the schema it was saved with (so an unchanged v1 draft stays byte-identical).
 */
export function normalizeSchema(def: WorkflowDefinition, savedSchema: string): WorkflowDefinition {
  const schema = usesV2(def) ? WORKFLOW_SCHEMA_V2 : savedSchema;
  return def.schema === schema ? def : { ...def, schema };
}

export const MAX_PARALLEL = 4;

export function maxParallelOf(def: Pick<WorkflowDefinition, "execution">): number {
  return def.execution?.maxParallel ?? 1;
}

/** 1 removes the field (the M8 default); 2–4 set it. */
export function setMaxParallel(def: WorkflowDefinition, n: number): WorkflowDefinition {
  const next = { ...def };
  if (n <= 1) delete next.execution;
  else next.execution = { maxParallel: Math.min(MAX_PARALLEL, Math.max(1, Math.round(n))) };
  return next;
}

/** The global caps of M9_PLAN (the service owns the real values; plans carry them). */
export const DEFAULT_LIMITS: WorkflowConcurrencyLimits = { maxActiveRuns: 3, maxInFlightSteps: 4, restricted: 2, native: 2, agent: 2, lean: 1 };

export function describeLimits(limits: WorkflowConcurrencyLimits): string {
  return (
    `Across all runs: at most ${limits.maxActiveRuns} active run${limits.maxActiveRuns === 1 ? "" : "s"} ` +
    `(one per workflow) and ${limits.maxInFlightSteps} step${limits.maxInFlightSteps === 1 ? "" : "s"} in flight; ` +
    `per kind at once: ${limits.restricted} restricted container, ${limits.native} native, ${limits.agent} model, ${limits.lean} Lean.`
  );
}

// ----- step edits ----------------------------------------------------------------------------

function patchStep(def: WorkflowDefinition, stepId: string, patch: (s: WorkflowStep) => WorkflowStep): WorkflowDefinition {
  return { ...def, steps: def.steps.map((s) => (s.id === stepId ? patch(s) : s)) };
}

function without<K extends keyof WorkflowStep>(step: WorkflowStep, key: K): WorkflowStep {
  const next = { ...step };
  delete next[key];
  return next;
}

export function setCondition(def: WorkflowDefinition, stepId: string, condition: StepCondition | null): WorkflowDefinition {
  return patchStep(def, stepId, (s) => (condition ? { ...s, condition } : without(s, "condition")));
}

/** Setting a forEach over a parameter clears the step's fixed value of it (each instance gets one value). */
export function setRepeat(def: WorkflowDefinition, stepId: string, repeat: StepRepeat | null): WorkflowDefinition {
  return patchStep(def, stepId, (s) => {
    if (!repeat) return without(s, "repeat");
    if (repeat.kind === "forEach" && repeat.parameter in s.parameters) {
      const parameters = { ...s.parameters };
      delete parameters[repeat.parameter];
      return { ...s, parameters, repeat };
    }
    return { ...s, repeat };
  });
}

/** Pin a composite step to another revision (an explicit parent edit; never automatic). */
export function setCompositeRevision(def: WorkflowDefinition, stepId: string, revision: number): WorkflowDefinition {
  return patchStep(def, stepId, (s) => (s.composite ? { ...s, composite: { ...s.composite, revision } } : s));
}

/** A composite step running `ref`; ports come from the child (bindings start empty). */
export function addCompositeStep(def: WorkflowDefinition, ref: CompositeRef, title: string): WorkflowDefinition {
  const taken = new Set(def.steps.map((s) => s.id));
  let n = 1;
  while (taken.has(`step${n}`)) n += 1;
  const step: WorkflowStep = {
    id: `step${n}`,
    label: title,
    operation: { id: COMPOSITE_OPERATION, version: 1 },
    parameters: {},
    payload: null,
    composite: { ...ref },
  };
  return { ...def, steps: [...def.steps, step] };
}

// ----- composites ----------------------------------------------------------------------------

export const isComposite = (step: Pick<WorkflowStep, "operation" | "composite">) => step.operation.id === COMPOSITE_OPERATION || Boolean(step.composite);

export const compositeKey = (ref: CompositeRef) => `${ref.workflow}@${ref.revision}`;
/** Contract id of the ports derived for one composite reference (never a registered operation). */
export const compositeContractId = (ref: CompositeRef) => `${COMPOSITE_OPERATION}:${compositeKey(ref)}`;
export const isDerivedContract = (c: OperationContract) => c.id.startsWith(`${COMPOSITE_OPERATION}:`);

/** Least contained wins (lane A `v2::rank`): a composite is as contained as its least contained step. */
const CONTAINMENT_RANK: Record<Containment, number> = { applicationService: 0, readOnlyModel: 1, restrictedContainer: 2, nativeAccount: 3 };

/**
 * Derive the operation contracts of composite references, as Rust does
 * (`v2::composite_contract`): one required input per child external input,
 * named by its id with its type; one optional output per child outcome with
 * the type of the port that realizes it (unresolvable outcomes are left out);
 * containment of the least contained child runner. Display only: Rust
 * derives the same ports when it validates the draft.
 * Nested composites resolve through `children` (bounded; a cycle resolves to
 * the generic contract and Rust reports `workflow.compositeRecursive`).
 * Returns `contracts` plus one derived contract per resolvable reference.
 */
export function withComposites(contracts: OperationContract[], children: ReadonlyMap<string, WorkflowDefinition>): OperationContract[] {
  const generic = contracts.find((c) => c.id === COMPOSITE_OPERATION);
  if (!generic || children.size === 0) return contracts;
  const memo = new Map<string, OperationContract | null>();
  const resolve = (step: WorkflowStep, visiting: Set<string>): OperationContract | undefined => {
    if (step.composite) return build(step.composite, visiting) ?? undefined;
    return contracts.find((c) => c.id === step.operation.id && c.version === step.operation.version);
  };
  const build = (ref: CompositeRef, visiting: Set<string>): OperationContract | null => {
    const key = compositeKey(ref);
    if (memo.has(key)) return memo.get(key) ?? null;
    const child = children.get(key);
    if (!child || visiting.has(key)) return null;
    const inner = new Set(visiting).add(key);
    const outputs = child.outcomes.flatMap((o) => {
      const producer = child.steps.find((s) => s.id === o.from.step);
      const type = producer && resolve(producer, inner)?.outputs.find((p) => p.name === o.from.port)?.types[0];
      return type ? [{ name: o.id, label: o.label, types: [type], required: false, multiple: false }] : [];
    });
    const containment = child.steps
      .map((s) => resolve(s, inner)?.runner.containment)
      .filter((c): c is Containment => c !== undefined)
      .reduce<Containment>((a, b) => (CONTAINMENT_RANK[b] > CONTAINMENT_RANK[a] ? b : a), "applicationService");
    const contract: OperationContract = {
      ...generic,
      id: compositeContractId(ref),
      title: `Saved workflow “${child.title || ref.workflow}” (revision ${ref.revision})`,
      purpose: `Runs revision ${ref.revision} of “${child.title || ref.workflow}” as one step.`,
      runner: { ...generic.runner, containment },
      inputs: child.inputs.map((i) => ({ name: i.id, label: i.label, types: [i.portType], required: true, multiple: false })),
      outputs,
      parameters: [],
      payload: null,
    };
    memo.set(key, contract);
    return contract;
  };
  const derived: OperationContract[] = [];
  for (const key of children.keys()) {
    const at = key.lastIndexOf("@");
    const c = build({ workflow: key.slice(0, at), revision: Number(key.slice(at + 1)) }, new Set());
    if (c) derived.push(c);
  }
  return [...contracts, ...derived];
}

/** Every composite reference in a definition. */
export function compositeRefs(def: WorkflowDefinition): CompositeRef[] {
  const seen = new Set<string>();
  const out: CompositeRef[] = [];
  for (const s of def.steps) {
    if (s.composite && !seen.has(compositeKey(s.composite))) {
      seen.add(compositeKey(s.composite));
      out.push(s.composite);
    }
  }
  return out;
}

// ----- conditions and predicates -------------------------------------------------------------

/**
 * Exactly the audit verdict labels a VerdictIn test may name: Rust's
 * `KNOWN_VERDICTS` (crates/exposition-graph/src/workflow.rs; a test pins this
 * list to it). An overall label such as "Errors (2)" belongs to the class it
 * starts with. Rust refuses any other label (`workflow.conditionInvalid`).
 */
export const KNOWN_VERDICTS = [
  "Checked",
  "Checked with nonstandard axioms",
  "Incomplete",
  "Relies on compiled code",
  "Partially checked",
  "Unverified",
  "Errors",
] as const;

export const COMPARE_OPS: { id: CompareOp; label: string; symbol: string }[] = [
  { id: "eq", label: "equals", symbol: "=" },
  { id: "ne", label: "does not equal", symbol: "≠" },
  { id: "lt", label: "is less than", symbol: "<" },
  { id: "le", label: "is at most", symbol: "≤" },
  { id: "gt", label: "is greater than", symbol: ">" },
  { id: "ge", label: "is at least", symbol: "≥" },
];

/** Comparisons a value kind allows: ordering only for numbers (Rust: "text can only be compared for equality"). */
export function compareOpsFor(kind: ConditionValue["kind"]): typeof COMPARE_OPS {
  return kind === "number" ? COMPARE_OPS : COMPARE_OPS.filter((o) => o.id === "eq" || o.id === "ne");
}

export type TestKind = ConditionTest["kind"];

export const TEST_KIND_LABELS: Record<TestKind, string> = {
  verdictIn: "Audit verdict is one of…",
  jsonCompare: "A JSON value compares…",
  outputPresent: "The output was produced",
};

/** Which predicates fit a source port: verdicts on formal evidence, JSON on a dataset, presence on an optional output. */
export function testKindsFor(type: PortType | null, optional: boolean): TestKind[] {
  const kinds: TestKind[] = [];
  if (type === "formalEvidence") kinds.push("verdictIn");
  if (type === "dataset") kinds.push("jsonCompare");
  if (optional) kinds.push("outputPresent");
  return kinds;
}

export function defaultTest(kind: TestKind): ConditionTest {
  switch (kind) {
    case "verdictIn":
      return { kind: "verdictIn", verdicts: ["Checked"] };
    case "jsonCompare":
      return { kind: "jsonCompare", pointer: "", op: "eq", value: { kind: "number", value: "0" } };
    case "outputPresent":
      return { kind: "outputPresent" };
  }
}

export function valueText(value: ConditionValue): string {
  switch (value.kind) {
    case "boolean":
      return value.value ? "true" : "false";
    case "number":
      return value.value;
    case "text":
      return `“${value.value}”`;
  }
}

/** A predicate in plain words about `subject` (e.g. "the verdict of Audit → Evidence"). */
export function describeTest(test: ConditionTest, subject: string, negate: boolean): string {
  switch (test.kind) {
    case "verdictIn": {
      const set = test.verdicts.length ? test.verdicts.map((v) => `“${v}”`).join(", ") : "(no verdict chosen)";
      return `the audit verdict of ${subject} is ${negate ? "not " : ""}one of ${set}`;
    }
    case "jsonCompare": {
      const op = COMPARE_OPS.find((o) => o.id === test.op);
      const where = test.pointer === "" ? "the whole JSON document" : `the JSON value at ${test.pointer}`;
      return `${where} in ${subject} ${negate ? "does not satisfy “" : ""}${op?.label ?? test.op} ${valueText(test.value)}${negate ? "”" : ""}`;
    }
    case "outputPresent":
      return `${subject} was ${negate ? "not " : ""}produced`;
  }
}

/** A compact predicate for canvas labels and badges ("verdict ∈ {Checked}", "/n ≥ 3", "produced"). */
export function shortTest(test: ConditionTest, negate: boolean): string {
  const not = negate ? "not " : "";
  switch (test.kind) {
    case "verdictIn":
      return `${not}verdict ∈ {${test.verdicts.join(", ")}}`;
    case "jsonCompare":
      return `${not}${test.pointer || "document"} ${COMPARE_OPS.find((o) => o.id === test.op)?.symbol ?? test.op} ${valueText(test.value)}`;
    case "outputPresent":
      return `${not}produced`;
  }
}

/** "Runs only if …" for a step condition; `portLabel` names a step output for the reader. */
export function describeCondition(condition: StepCondition, portLabel: (step: string, port: string) => string): string {
  return `Runs only if ${describeTest(condition.test, portLabel(condition.source.step, condition.source.port), condition.negate)}; otherwise it is skipped (condition not met), and steps that need its outputs are skipped too.`;
}

export function paramText(value: ParamValue): string {
  switch (value.kind) {
    case "boolean":
      return value.value ? "yes" : "no";
    case "integer":
      return String(value.value);
    default:
      return value.value;
  }
}

export const MAX_FOR_EACH_VALUES = 20;
export const MAX_UNTIL_ITERATIONS = 10;

/** Plain-language iteration bounds. `ownPort` names one of the step's own outputs. */
export function describeRepeat(repeat: StepRepeat, ownPort: (port: string) => string, paramLabel: (name: string) => string = (n) => n): string {
  if (repeat.kind === "forEach") {
    const n = repeat.values.length;
    return `Runs once for each of ${n} value${n === 1 ? "" : "s"} of ${paramLabel(repeat.parameter)} (${repeat.values.map(paramText).join(", ")}): ${n} instance${n === 1 ? "" : "s"}, each counted against the step budget; outputs are collected in this order.`;
  }
  const until = describeTest(repeat.test, ownPort(repeat.port), repeat.negate);
  return `Runs again, one iteration at a time, until ${until}; at most ${repeat.maxIterations} iteration${repeat.maxIterations === 1 ? "" : "s"}, each counted against the step budget. If it never holds: ${repeat.onExhausted === "fail" ? "the step fails" : "the last iteration's output is used"}.`;
}

/** A default value of a parameter's kind (for a new forEach value). */
export function defaultParamValue(spec: ParameterSpec, index = 0): ParamValue {
  switch (spec.kind.kind) {
    case "integer":
      return { kind: "integer", value: Math.min(spec.kind.max, spec.kind.min + index) };
    case "boolean":
      return { kind: "boolean", value: index % 2 === 0 };
    default:
      return { kind: "text", value: "" };
  }
}

/** Client-side hints only (Rust is the validator); empty when nothing looks wrong. */
export function pointerProblem(pointer: string): string | null {
  if (pointer.length > 256) return "A JSON pointer is at most 256 characters.";
  if (pointer !== "" && !pointer.startsWith("/")) return "A JSON pointer is empty (the whole document) or starts with “/”, e.g. /summary/slope.";
  if (/~(?![01])/.test(pointer)) return "In a JSON pointer, “~” must be written ~0 and “/” inside a name ~1.";
  return null;
}

export function numberProblem(text: string): string | null {
  return /^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?$/.test(text.trim()) ? null : "Enter a decimal number, e.g. 0.05 or -3.";
}
