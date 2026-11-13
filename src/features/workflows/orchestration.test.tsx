// M9 orchestration in the workflow editor: composite steps (ports from the
// pinned child, child steps read-only, "Use latest revision" only as an
// explicit edit), bounded conditions (sources limited to upstream outputs a
// fixed predicate can read), bounded repeats (forEach ≤ 20 values of the
// parameter's kind; until ≤ 10), max parallel, schema /2, and the canvas
// projection (condition edges, fan-in refusal). IPC is mocked at the
// feature's boundary; Rust remains the validator.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { OperationContract } from "../../bindings/OperationContract";
import type { WorkflowValidation } from "../../bindings/WorkflowValidation";
import type { SavedWorkflow, WorkflowDefinition } from "./contract";

const wfIpc = vi.hoisted(() => ({
  operations: vi.fn(),
  presets: vi.fn(),
  fromPreset: vi.fn(),
  validate: vi.fn(),
  save: vi.fn(),
  import: vi.fn(),
  list: vi.fn(),
  get: vi.fn(),
}));
const runIpc = vi.hoisted(() => ({
  plan: vi.fn(),
  start: vi.fn(),
  cancel: vi.fn(),
  get: vi.fn(),
  list: vi.fn(async () => []),
  acknowledge: vi.fn(),
  decideAssociation: vi.fn(),
  promotionPreview: vi.fn(),
  reproducePlan: vi.fn(),
  manifest: vi.fn(),
  exportManifest: vi.fn(),
}));
const triggerIpc = vi.hoisted(() => ({ list: vi.fn(async () => []), plan: vi.fn(), enable: vi.fn(), disable: vi.fn() }));
const editorsMock = vi.hoisted(() => ({ open: vi.fn(), setDirty: vi.fn(), setTitle: vi.fn(), registerCloseGuard: vi.fn(() => () => {}), active: vi.fn(() => null) }));
vi.mock("./ipc", () => ({
  workflowCommands: wfIpc,
  workflowRunCommands: runIpc,
  workflowTriggerCommands: triggerIpc,
  workflowEvents: { run: vi.fn(async () => () => {}), trigger: vi.fn(async () => () => {}) },
}));
vi.mock("../research/ipc", () => ({ researchCommands: {} }));
vi.mock("../research/Promotion", () => ({ PreviewView: () => null }));
vi.mock("./CodePayloadEditor", () => ({ CodePayloadEditor: () => null }));
vi.mock("../../app/editors", () => ({ editors: editorsMock }));
vi.mock("../../app/notifications", () => ({ notify: vi.fn(), dismiss: vi.fn() }));
vi.mock("../workspace/store", async () => {
  const { create } = await import("zustand");
  return { useWorkspace: create(() => ({ info: { root: "/work/p", name: "p", recovery: [] } })) };
});
vi.mock("../../lib/ipc", () => ({
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
  isCommandError: (e: unknown) => typeof e === "object" && e !== null && "code" in e && "message" in e,
  commands: {},
}));

const model = await import("./model");
const orch = await import("./orchestration");
const canvasModel = await import("./canvasModel");
const { WorkflowEditor } = await import("./WorkflowEditor");
const { WorkflowsView } = await import("./WorkflowsView");

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
  (globalThis as { DOMMatrixReadOnly?: unknown }).DOMMatrixReadOnly ??= class {
    m22 = 1;
  };
  globalThis.matchMedia ??= ((query: string) => ({ matches: false, media: query, addEventListener() {}, removeEventListener() {} })) as unknown as typeof matchMedia;
});

const runner = { id: "r", title: "Runner", containment: "restrictedContainer" as const, allows: [], available: true, availability: "Available" };
const op = (c: Partial<OperationContract> & Pick<OperationContract, "id" | "inputs" | "outputs">): OperationContract => ({
  version: 1,
  title: c.id,
  purpose: "",
  parameters: [],
  payload: null,
  effects: ["readDeclaredInputs"],
  runner,
  limits: { maxSeconds: 60, maxOutputBytes: 1000 },
  failure: "stopWorkflow",
  ...c,
});

export const contracts: OperationContract[] = [
  op({
    id: "research.pythonBatch",
    title: "Python batch",
    inputs: [{ name: "data", label: "Data", types: ["dataset"], required: true, multiple: false }],
    outputs: [
      { name: "summary", label: "Summary", types: ["dataset"], required: true, multiple: false },
      { name: "figure", label: "Figure", types: ["figure"], required: false, multiple: false },
    ],
    parameters: [
      { name: "seed", label: "Seed", kind: { kind: "integer", min: 0, max: 1000 }, required: false },
      { name: "method", label: "Method", kind: { kind: "text", maxLen: 40 }, required: false },
    ],
  }),
  op({
    id: "lean.audit",
    title: "Lean audit",
    inputs: [{ name: "source", label: "Lean file", types: ["formalSource"], required: true, multiple: false }],
    outputs: [{ name: "evidence", label: "Audited declarations", types: ["formalEvidence"], required: true, multiple: false }],
  }),
  op({
    id: "agent.review",
    title: "Read-only agent review",
    inputs: [{ name: "material", label: "Material", types: ["dataset", "formalEvidence", "figure"], required: true, multiple: true }],
    outputs: [{ name: "report", label: "Review report", types: ["reviewReport"], required: false, multiple: false }],
    payload: { kind: "prompt" },
  }),
  op({ id: "workflow.composite", title: "Run a saved workflow", purpose: "Runs a saved workflow at a pinned revision", inputs: [], outputs: [], runner: { ...runner, id: "composite", containment: "applicationService" } }),
];

/** The child a composite runs: one dataset input, one Python step, one outcome. */
const child: WorkflowDefinition = {
  schema: "exposition.workflow/1",
  title: "Fit model",
  description: "",
  preset: null,
  inputs: [{ id: "obs", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }],
  steps: [{ id: "fit", label: "Fit", operation: { id: "research.pythonBatch", version: 1 }, parameters: {}, payload: null }],
  connections: [{ kind: "data", from: { kind: "input", input: "obs" }, to: { step: "fit", port: "data" } }],
  outcomes: [{ id: "fitted", kind: "transformedDataset", label: "Fitted values", from: { step: "fit", port: "summary" }, optional: false }],
  ceiling: { execution: "restricted", modelRequests: false, retainResults: true, proposeChanges: false },
};

const parent: WorkflowDefinition = {
  schema: "exposition.workflow/1",
  title: "Study",
  description: "",
  preset: null,
  inputs: [
    { id: "data", label: "Data", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } },
    { id: "lean", label: "Proof", portType: "formalSource", source: { kind: "resource", path: "Main.lean", version: { kind: "atRunStart" } } },
  ],
  steps: [
    { id: "sim", label: "Simulate", operation: { id: "research.pythonBatch", version: 1 }, parameters: { seed: { kind: "integer", value: 7 } }, payload: null },
    { id: "audit", label: "Audit", operation: { id: "lean.audit", version: 1 }, parameters: {}, payload: null },
    { id: "review", label: "Review", operation: { id: "agent.review", version: 1 }, parameters: {}, payload: { kind: "prompt", text: "Check." } },
  ],
  connections: [
    { kind: "data", from: { kind: "input", input: "data" }, to: { step: "sim", port: "data" } },
    { kind: "data", from: { kind: "input", input: "lean" }, to: { step: "audit", port: "source" } },
    { kind: "data", from: { kind: "step", step: "sim", port: "summary" }, to: { step: "review", port: "material" } },
  ],
  outcomes: [{ id: "report", kind: "investigationReport", label: "Report", from: { step: "review", port: "report" }, optional: false }],
  ceiling: { execution: "restricted", modelRequests: true, retainResults: true, proposeChanges: false },
};

const withComposite: WorkflowDefinition = {
  ...parent,
  schema: "exposition.workflow/2",
  steps: [
    ...parent.steps,
    { id: "comp", label: "Fit model", operation: { id: "workflow.composite", version: 1 }, parameters: {}, payload: null, composite: { workflow: "wf-child", revision: 1 } },
  ],
};

const validation = (overrides: Partial<WorkflowValidation> = {}): WorkflowValidation => ({
  state: "wellFormed",
  runnable: true,
  runnableReason: "Every runner is available.",
  diagnostics: [],
  errors: 0,
  warnings: 0,
  effects: [],
  plan: [],
  ...overrides,
});

const saved = (id: string, revision: number, def: WorkflowDefinition, latest = revision): SavedWorkflow => ({
  id,
  revision,
  hash: "a".repeat(64),
  entity: null,
  definition: def,
  validation: validation(),
  origin: { kind: "user" },
  imported: false,
  savedAtMs: 0,
  unchanged: false,
  revisions: Array.from({ length: latest }, (_, i) => ({ revision: latest - i, hash: "b".repeat(64), state: "wellFormed" as const, savedAtMs: 0 })),
  graphRevision: 1,
});

/** Open a react-aria Select (by its test id) and choose the option named `name`. */
async function choose(testId: string, name: string | RegExp) {
  const select = screen.getByTestId(testId);
  fireEvent.click(within(select).getByRole("button"));
  const option = await screen.findByRole("option", { name });
  fireEvent.click(option);
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  wfIpc.operations.mockResolvedValue(contracts);
  wfIpc.validate.mockResolvedValue(validation());
  wfIpc.list.mockResolvedValue([]);
});
afterEach(() => cleanup());

// ----- pure model -----------------------------------------------------------------------------

describe("schema and parallelism", () => {
  it("declares schema /2 only when a v2 field is used, and keeps a v1 draft byte-identical otherwise", () => {
    expect(orch.normalizeSchema(parent, "exposition.workflow/1")).toBe(parent);
    const conditional = orch.setCondition(parent, "review", { source: { step: "audit", port: "evidence" }, test: { kind: "verdictIn", verdicts: ["Checked"] }, negate: false });
    expect(orch.normalizeSchema(conditional, "exposition.workflow/1").schema).toBe("exposition.workflow/2");
    const back = orch.setCondition(conditional, "review", null);
    expect(orch.normalizeSchema(back, "exposition.workflow/1")).toEqual(parent);
    expect("condition" in back.steps[2]!).toBe(false);
    const parallel = orch.setMaxParallel(parent, 3);
    expect(parallel.execution).toEqual({ maxParallel: 3 });
    expect(orch.usesV2(parallel)).toBe(true);
    expect("execution" in orch.setMaxParallel(parallel, 1)).toBe(false);
    expect(orch.setMaxParallel(parent, 9).execution).toEqual({ maxParallel: 4 });
  });
});

describe("composites", () => {
  const children = new Map([["wf-child@1", child]]);

  it("derives a composite step's ports from the pinned child (inputs by id and type; outputs by outcome with the realizing port's type)", () => {
    const effective = orch.withComposites(contracts, children);
    const step = withComposite.steps[3]!;
    const c = model.contractOf(effective, step)!;
    expect(c.id).toBe("workflow.composite:wf-child@1");
    expect(orch.isDerivedContract(c)).toBe(true);
    expect(c.inputs).toEqual([{ name: "obs", label: "Observations", types: ["dataset"], required: true, multiple: false }]);
    // As Rust derives it (v2::composite_contract): outputs optional, least contained child runner, titled with the revision.
    expect(c.outputs).toEqual([{ name: "fitted", label: "Fitted values", types: ["dataset"], required: false, multiple: false }]);
    expect(c.runner.containment).toBe("restrictedContainer");
    expect(c.title).toBe("Saved workflow “Fit model” (revision 1)");
    // Binding choices follow the derived ports like any other step.
    expect(model.compatibleSources(withComposite, effective, "comp", "obs").map((s) => s.key)).toEqual(["input:data", "step:sim.summary"]);
    // Without the child loaded the generic contract (no ports) applies.
    expect(model.contractOf(contracts, step)!.inputs).toEqual([]);
  });

  it("resolves nested composites and never loops on a recursive reference", () => {
    const middle: WorkflowDefinition = {
      ...child,
      title: "Middle",
      inputs: [{ id: "x", label: "X", portType: "dataset", source: { kind: "unset" } }],
      steps: [{ id: "inner", label: "Inner", operation: { id: "workflow.composite", version: 1 }, parameters: {}, payload: null, composite: { workflow: "wf-child", revision: 1 } }],
      connections: [],
      outcomes: [{ id: "out", kind: "transformedDataset", label: "Out", from: { step: "inner", port: "fitted" }, optional: true }],
    };
    const effective = orch.withComposites(contracts, new Map([["wf-mid@2", middle], ["wf-child@1", child]]));
    const derived = effective.find((c) => c.id === "workflow.composite:wf-mid@2")!;
    expect(derived.outputs).toEqual([{ name: "out", label: "Out", types: ["dataset"], required: false, multiple: false }]);
    const selfRef: WorkflowDefinition = { ...middle, steps: [{ ...middle.steps[0]!, composite: { workflow: "wf-self", revision: 1 } }] };
    expect(() => orch.withComposites(contracts, new Map([["wf-self@1", selfRef]]))).not.toThrow();
  });

  it("adds a composite step pinned to a revision and changes the pin only by an explicit edit", () => {
    const added = orch.addCompositeStep(parent, { workflow: "wf-child", revision: 2 }, "Fit model");
    const step = added.steps[added.steps.length - 1]!;
    expect(step).toMatchObject({ id: "step1", operation: { id: "workflow.composite", version: 1 }, composite: { workflow: "wf-child", revision: 2 }, payload: null });
    expect(orch.setCompositeRevision(added, step.id, 3).steps.at(-1)!.composite).toEqual({ workflow: "wf-child", revision: 3 });
    expect(orch.compositeRefs(withComposite)).toEqual([{ workflow: "wf-child", revision: 1 }]);
  });
});

describe("conditions", () => {
  it("offers only outputs a fixed predicate can read that are not downstream; a condition orders its source first", () => {
    const opts = model.conditionSources(parent, contracts, "review").map((o) => [o.key, o.type, o.optional]);
    // sim.summary (dataset), sim.figure (optional), audit.evidence (formal evidence); never the step itself.
    expect(opts).toEqual([
      ["sim.summary", "dataset", false],
      ["sim.figure", "figure", true],
      ["audit.evidence", "formalEvidence", false],
    ]);
    // review is downstream of sim: sim may not be conditioned on review's (optional) report.
    expect(model.conditionSources(parent, contracts, "sim").map((o) => o.key)).toEqual(["audit.evidence"]);
    const cond = orch.setCondition(parent, "sim", { source: { step: "audit", port: "evidence" }, test: { kind: "verdictIn", verdicts: ["Checked"] }, negate: false });
    expect(model.downstreamOf(cond, "audit")).toEqual(new Set(["sim", "review"]));
    // Hence audit can no longer read sim's output (cycle).
    expect(model.conditionSources(cond, contracts, "audit").map((o) => o.key)).toEqual([]);
    expect(orch.testKindsFor("formalEvidence", false)).toEqual(["verdictIn"]);
    expect(orch.testKindsFor("dataset", true)).toEqual(["jsonCompare", "outputPresent"]);
    expect(orch.testKindsFor("figure", false)).toEqual([]);
  });

  it("describes conditions and repeats in plain words, and removing a step removes conditions on its outputs", () => {
    const cond = { source: { step: "sim", port: "summary" }, test: { kind: "jsonCompare" as const, pointer: "/slope", op: "ge" as const, value: { kind: "number" as const, value: "1.5" } }, negate: true };
    expect(orch.describeCondition(cond, () => "“Simulate → Summary”")).toMatch(
      /^Runs only if the JSON value at \/slope in “Simulate → Summary” does not satisfy “is at least 1.5”; otherwise it is skipped \(condition not met\)/,
    );
    expect(orch.describeRepeat({ kind: "forEach", parameter: "seed", values: [{ kind: "integer", value: 1 }, { kind: "integer", value: 2 }] }, (p) => p, () => "“Seed”")).toBe(
      "Runs once for each of 2 values of “Seed” (1, 2): 2 instances, each counted against the step budget; outputs are collected in this order.",
    );
    expect(
      orch.describeRepeat({ kind: "until", port: "summary", test: { kind: "outputPresent" }, negate: false, maxIterations: 4, onExhausted: "useLast" }, (p) => `its output “${p}”`),
    ).toBe("Runs again, one iteration at a time, until its output “summary” was produced; at most 4 iterations, each counted against the step budget. If it never holds: the last iteration's output is used.");
    const conditioned = orch.setCondition(parent, "review", cond);
    expect(model.removeStep(conditioned, "sim").steps.find((s) => s.id === "review")!.condition).toBeUndefined();
    expect(orch.pointerProblem("slope")).toMatch(/starts with “\/”/);
    expect(orch.pointerProblem("/a~1b")).toBeNull();
    expect(orch.pointerProblem("/a~2")).toMatch(/~0/);
    expect(orch.numberProblem("1e3")).toBeNull();
    expect(orch.numberProblem("abc")).toMatch(/decimal/);
  });
});

describe("repeats", () => {
  it("clears the fixed parameter a forEach varies, and a forEach step's outputs feed only inputs that take several sources", () => {
    const repeated = orch.setRepeat(parent, "sim", { kind: "forEach", parameter: "seed", values: [{ kind: "integer", value: 1 }, { kind: "integer", value: 2 }] });
    expect(repeated.steps[0]!.parameters).toEqual({});
    // review.material takes several sources: sim.summary is still offered; a single port (comp.obs) no longer is.
    expect(model.compatibleSources(repeated, contracts, "review", "material").map((s) => s.key)).toContain("step:sim.summary");
    const withChild = { ...repeated, steps: [...repeated.steps, withComposite.steps[3]!] };
    const effective = orch.withComposites(contracts, new Map([["wf-child@1", child]]));
    expect(model.compatibleSources(withChild, effective, "comp", "obs").map((s) => s.key)).toEqual(["input:data"]);
    const refused = canvasModel.connect(withChild, effective, { source: "step:sim", sourceHandle: "out:summary", target: "step:comp", targetHandle: "in:obs" });
    expect(refused).toEqual({ ok: false, message: expect.stringMatching(/runs once for each value.*takes one source/) });
    expect(orch.setRepeat(repeated, "sim", null).steps[0]!.repeat).toBeUndefined();
  });

  it("projects a condition as a dash-dot edge from the tested output that the canvas never deletes, and lays the step out after its source", () => {
    const cond = orch.setCondition(parent, "review", { source: { step: "audit", port: "evidence" }, test: { kind: "verdictIn", verdicts: ["Checked", "Checked with axioms"] }, negate: false });
    const edge = canvasModel.edgesOf(cond, contracts, []).find((e) => e.kind === "condition")!;
    expect(edge).toMatchObject({ id: "cond:review", source: "step:audit", sourceHandle: "out:evidence", target: "step:review", targetHandle: "pre-in" });
    expect(edge.label).toBe("condition (no data)");
    expect(edge.description).toMatch(/Review runs only if the audit verdict of Audit → Audited declarations is one of “Checked”, “Checked with axioms”/);
    expect(canvasModel.removeEdge(cond, "cond:review")).toBeNull();
    const layout = canvasModel.autoLayout(cond, contracts);
    expect(layout.get("step:audit")!.x).toBeLessThan(layout.get("step:review")!.x);
  });
});

// ----- editor ---------------------------------------------------------------------------------

function openEditor(def: WorkflowDefinition, { latestChild = 1 }: { latestChild?: number } = {}) {
  wfIpc.get.mockImplementation(async (id: string, revision: number | null) => {
    if (id === "wf-child") return saved("wf-child", revision ?? latestChild, child, latestChild);
    return saved("wf-1", 1, def);
  });
  render(<WorkflowEditor resource={{ key: "workflow:wf-1", title: def.title }} mode="source" active />);
}

describe("WorkflowEditor (M9)", () => {
  it("shows a composite step's child and pinned revision, its steps read-only, and adopts the latest revision only on request", async () => {
    openEditor(withComposite, { latestChild: 3 });
    const section = await screen.findByTestId("composite-comp");
    await waitFor(() => expect(section.textContent).toContain("“Fit model”"));
    expect(screen.getByTestId("composite-revision-comp").textContent).toBe("1");
    expect(wfIpc.get).toHaveBeenCalledWith("wf-child", 1);
    // The derived ports appear as bindings of the step.
    expect(within(screen.getByTestId("workflow-step-comp")).getByText(/Observations \(Dataset\)/)).toBeTruthy();
    // Nothing changed by itself: not dirty, still pinned to 1.
    expect(editorsMock.setDirty).not.toHaveBeenCalledWith("workflow:wf-1", true);
    fireEvent.click(screen.getByTestId("composite-expand-comp"));
    const steps = screen.getByTestId("composite-children-comp");
    expect(steps.textContent).toContain("Fit");
    expect(steps.querySelector("input, textarea, button")).toBeNull();
    fireEvent.click(screen.getByTestId("composite-use-latest-comp"));
    await waitFor(() => expect(screen.getByTestId("composite-revision-comp").textContent).toBe("3"));
    await waitFor(() => expect(wfIpc.validate).toHaveBeenLastCalledWith(expect.objectContaining({ steps: expect.arrayContaining([expect.objectContaining({ id: "comp", composite: { workflow: "wf-child", revision: 3 } })]) })));
    await waitFor(() => expect(editorsMock.setDirty).toHaveBeenLastCalledWith("workflow:wf-1", true));
  });

  it("adds a step that runs another saved workflow, pinned to its latest revision", async () => {
    wfIpc.list.mockResolvedValue([
      { id: "wf-1", revision: 1, title: "Study", preset: null, state: "wellFormed", runnable: true, errors: 0, warnings: 0, entity: null, origin: { kind: "user" }, imported: false, savedAtMs: 0 },
      { id: "wf-child", revision: 4, title: "Fit model", preset: null, state: "wellFormed", runnable: true, errors: 0, warnings: 0, entity: null, origin: { kind: "user" }, imported: false, savedAtMs: 0 },
    ]);
    openEditor(parent);
    await screen.findByTestId("workflow-step-sim");
    await choose("workflow-new-step", /Run a saved workflow/);
    await screen.findByTestId("workflow-new-composite");
    await waitFor(() => expect(wfIpc.list).toHaveBeenCalled());
    // The workflow itself is never offered (no self-reference).
    fireEvent.click(within(screen.getByTestId("workflow-new-composite")).getByRole("button"));
    expect(screen.queryByRole("option", { name: /^Study/ })).toBeNull();
    fireEvent.click(await screen.findByRole("option", { name: /Fit model/ }));
    fireEvent.click(screen.getByTestId("workflow-add-step"));
    await waitFor(() =>
      expect(wfIpc.validate).toHaveBeenLastCalledWith(
        expect.objectContaining({
          schema: "exposition.workflow/2",
          steps: expect.arrayContaining([expect.objectContaining({ operation: { id: "workflow.composite", version: 1 }, composite: { workflow: "wf-child", revision: 4 } })]),
        }),
      ),
    );
  });

  it("edits a condition with sources limited to upstream outputs and a fixed verdict predicate", async () => {
    openEditor(parent);
    const section = await screen.findByTestId("condition-review");
    fireEvent.click(within(section).getByRole("checkbox", { name: /Run this step only when a condition holds/ }));
    await screen.findByTestId("condition-source-review");
    fireEvent.click(within(screen.getByTestId("condition-source-review")).getByRole("button"));
    const offered = (await screen.findAllByRole("option")).map((o) => o.textContent);
    expect(offered.some((t) => t?.includes("Audit → Audited declarations"))).toBe(true);
    expect(offered.some((t) => t?.includes("Review"))).toBe(false);
    fireEvent.click(screen.getByRole("option", { name: /Audit → Audited declarations/ }));
    await waitFor(() => expect(screen.getByTestId("condition-review-test").getAttribute("data-kind")).toBe("verdictIn"));
    fireEvent.click(within(screen.getByTestId("condition-review-verdicts")).getByRole("checkbox", { name: "Checked with axioms" }));
    fireEvent.click(within(section).getByRole("checkbox", { name: /Negate/ }));
    await waitFor(() =>
      expect(wfIpc.validate).toHaveBeenLastCalledWith(
        expect.objectContaining({
          schema: "exposition.workflow/2",
          steps: expect.arrayContaining([
            expect.objectContaining({
              id: "review",
              condition: { source: { step: "audit", port: "evidence" }, test: { kind: "verdictIn", verdicts: ["Checked", "Checked with axioms"] }, negate: true },
            }),
          ]),
        }),
      ),
    );
    expect(screen.getByTestId("condition-summary-review").textContent).toMatch(/Runs only if the audit verdict of “Audit → Audited declarations” is not one of “Checked”, “Checked with axioms”/);
  });

  it("edits a JSON comparison on a dataset output and shows the backend's condition diagnostics inline", async () => {
    wfIpc.validate.mockResolvedValue(
      validation({
        state: "incomplete",
        runnable: false,
        errors: 1,
        diagnostics: [{ severity: "error", code: "workflow.conditionInvalid", message: "The JSON pointer must start with “/”.", rule: null, step: "review", port: null, input: null, outcome: null, connection: null }],
      }),
    );
    openEditor(orch.setCondition(parent, "review", { source: { step: "sim", port: "summary" }, test: { kind: "jsonCompare", pointer: "", op: "eq", value: { kind: "number", value: "0" } }, negate: false }));
    const pointer = await screen.findByTestId("condition-review-pointer");
    fireEvent.change(pointer.querySelector("input")!, { target: { value: "slope" } });
    expect(pointer.textContent).toMatch(/starts with “\/”/);
    await choose("condition-review-op", /is at least/);
    fireEvent.change(screen.getByTestId("condition-review-value").querySelector("input")!, { target: { value: "1.5" } });
    await waitFor(() =>
      expect(wfIpc.validate).toHaveBeenLastCalledWith(
        expect.objectContaining({ steps: expect.arrayContaining([expect.objectContaining({ id: "review", condition: expect.objectContaining({ test: { kind: "jsonCompare", pointer: "slope", op: "ge", value: { kind: "number", value: "1.5" } } }) })]) }),
      ),
    );
    const card = screen.getByTestId("workflow-step-review");
    await waitFor(() => expect(card.querySelector('[data-code="workflow.conditionInvalid"]')).toBeTruthy());
  });

  it("repeats a step for each of at most 20 values of a declared parameter, and caps until at 10 iterations", async () => {
    openEditor(parent);
    await screen.findByTestId("repeat-sim");
    await choose("repeat-kind-sim", /Once for each value of a parameter/);
    const values = await screen.findByTestId("repeat-values-sim");
    // The existing fixed seed becomes the first value; the fixed field now says it varies.
    expect(values.querySelectorAll("li")).toHaveLength(1);
    expect((values.querySelector("input") as HTMLInputElement).value).toBe("7");
    expect(within(screen.getByTestId("workflow-step-sim")).getByText(/Varies: one instance per value/)).toBeTruthy();
    const add = screen.getByTestId("repeat-add-value-sim") as HTMLButtonElement;
    for (let i = 0; i < 19; i++) fireEvent.click(add);
    await waitFor(() => expect(screen.getByTestId("repeat-values-sim").querySelectorAll("li")).toHaveLength(20));
    expect(add.disabled).toBe(true);
    await waitFor(() =>
      expect(wfIpc.validate).toHaveBeenLastCalledWith(
        expect.objectContaining({ steps: expect.arrayContaining([expect.objectContaining({ id: "sim", parameters: {}, repeat: expect.objectContaining({ kind: "forEach", parameter: "seed" }) })]) }),
      ),
    );
    const last = wfIpc.validate.mock.lastCall![0] as WorkflowDefinition;
    const repeat = last.steps[0]!.repeat!;
    expect(repeat.kind === "forEach" && repeat.values.map((v) => v.value)).toEqual([7, ...Array.from({ length: 19 }, (_, i) => i + 1)]);
    expect(screen.getByTestId("repeat-summary-sim").textContent).toMatch(/Runs once for each of 20 values of “Seed”/);
    // until: at most 10 iterations are offered.
    await choose("repeat-kind-sim", /Again until a condition/);
    fireEvent.click(within(await screen.findByTestId("repeat-max-sim")).getByRole("button"));
    const counts = (await screen.findAllByRole("option")).map((o) => o.textContent);
    expect(counts).toEqual(["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]);
  });

  it("sets max parallel 1–4 (1 = one at a time, the field omitted)", async () => {
    openEditor(parent);
    await screen.findByTestId("workflow-max-parallel");
    fireEvent.click(within(screen.getByTestId("workflow-max-parallel")).getByRole("button"));
    expect((await screen.findAllByRole("option")).map((o) => o.querySelector("[slot=label]")?.textContent)).toEqual(["1 — one at a time", "2", "3", "4"]);
    fireEvent.click(screen.getByRole("option", { name: /^3/ }));
    await waitFor(() => expect(wfIpc.validate).toHaveBeenLastCalledWith(expect.objectContaining({ schema: "exposition.workflow/2", execution: { maxParallel: 3 } })));
    await choose("workflow-max-parallel", /one at a time/);
    await waitFor(() => expect(wfIpc.validate).toHaveBeenLastCalledWith(expect.not.objectContaining({ execution: expect.anything() })));
    expect((wfIpc.validate.mock.lastCall![0] as WorkflowDefinition).schema).toBe("exposition.workflow/1");
  });

  it("shows a composite's child and newer revision on the canvas node, with its steps on request", async () => {
    localStorage.setItem("exposition.workflows.view", "canvas");
    openEditor(withComposite, { latestChild: 2 });
    const info = await screen.findByTestId("wfc-composite-comp");
    await waitFor(() => expect(info.textContent).toContain("“Fit model”"));
    expect(info.textContent).toContain("revision 1 (pinned)");
    expect(info.textContent).toContain("Revision 2 available");
    fireEvent.click(screen.getByTestId("wfc-composite-toggle-comp"));
    expect(screen.getByTestId("wfc-composite-children-comp").textContent).toContain("Fit");
    expect(screen.getByTestId("wfc-port-comp-in-obs").textContent).toContain("Observations");
    expect(screen.getByTestId("wfc-port-comp-out-fitted").textContent).toContain("Fitted values");
    act(() => localStorage.removeItem("exposition.workflows.view"));
  });
});

describe("WorkflowsView (M9)", () => {
  it("marks workflows whose trigger is on or suspended", async () => {
    const summary = (id: string, title: string) => ({ id, revision: 1, title, preset: null, state: "wellFormed", runnable: true, errors: 0, warnings: 0, entity: null, origin: { kind: "user" }, imported: false, savedAtMs: 0 });
    wfIpc.list.mockResolvedValue([summary("wf-a", "A"), summary("wf-b", "B"), summary("wf-c", "C")]);
    wfIpc.presets.mockResolvedValue([]);
    const trigger = (workflow: string, state: object) => ({ id: `t-${workflow}`, workflow, state });
    triggerIpc.list.mockResolvedValue([trigger("wf-a", { kind: "active" }), trigger("wf-b", { kind: "suspended", code: "x", message: "y" }), trigger("wf-c", { kind: "disabled" })] as never);
    render(<WorkflowsView resource={{ key: "workflows:", title: "Workflows" }} mode="source" active />);
    await waitFor(() => expect(triggerIpc.list).toHaveBeenCalledWith(null));
    const row = async (id: string) => (await screen.findByTestId("workflow-drafts")).querySelector(`[data-workflow="${id}"]`)!;
    await waitFor(async () => expect((await row("wf-a")).textContent).toContain("Trigger on"));
    expect((await row("wf-b")).textContent).toContain("Trigger suspended");
    expect((await row("wf-c")).textContent).not.toMatch(/Trigger/);
  });
});
