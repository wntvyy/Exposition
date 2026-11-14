// M9 plan review, run monitor, and triggers: expanded composite steps
// grouped under their composite with complete code; conditions, iteration
// bounds, max parallel and global limits in plain words; reproduction plans
// with drift notices approved like any plan; concurrent steps and instances;
// ConditionNotMet (distinct from Skipped) with its reason; outcome reasons;
// run cause; per-attempt environment; manifest export; triggers enabled only
// through a reviewed standing plan. IPC mocked at the feature's boundary.
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  WorkflowDefinition,
  WorkflowEnvironment,
  WorkflowPlanStep,
  WorkflowRun,
  WorkflowRunPlan,
  WorkflowStepAttempt,
  WorkflowTrigger,
  WorkflowTriggerPlan,
} from "./contract";

const runIpc = vi.hoisted(() => ({
  plan: vi.fn(),
  start: vi.fn(),
  cancel: vi.fn(),
  get: vi.fn(),
  list: vi.fn(),
  acknowledge: vi.fn(),
  decideAssociation: vi.fn(),
  promotionPreview: vi.fn(),
  reproducePlan: vi.fn(),
  manifest: vi.fn(),
  exportManifest: vi.fn(),
}));
const triggerIpc = vi.hoisted(() => ({ list: vi.fn(), plan: vi.fn(), enable: vi.fn(), disable: vi.fn() }));
const editorsMock = vi.hoisted(() => ({ open: vi.fn(), setTitle: vi.fn(), setDirty: vi.fn(), registerCloseGuard: vi.fn(() => () => {}) }));
const notifyMock = vi.hoisted(() => vi.fn());
vi.mock("./ipc", () => ({
  workflowCommands: {},
  workflowRunCommands: runIpc,
  workflowTriggerCommands: triggerIpc,
  workflowEvents: { run: vi.fn(async () => () => {}), trigger: vi.fn(async () => () => {}) },
}));
vi.mock("../../lib/ipc", () => ({
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
  isCommandError: (e: unknown) => typeof e === "object" && e !== null && "code" in e && "message" in e,
  commands: { researchArtifact: vi.fn() },
}));
vi.mock("../../app/editors", () => ({ editors: editorsMock }));
vi.mock("../../app/notifications", () => ({ notify: notifyMock, dismiss: vi.fn() }));
vi.mock("../research/ipc", () => ({ researchCommands: {} }));
vi.mock("../research/Promotion", () => ({ PreviewView: () => null }));

const { PlanReviewDialog, groupByComposite } = await import("./PlanReview");
const { RunMonitor, RunHistory } = await import("./RunView");
const { TriggersSection, settingsProblems, suspensionTitle } = await import("./TriggersPanel");
const runModel = await import("./runModel");

const HASH = "ab12cd34ef56".padEnd(64, "0");

const planStep = (overrides: Partial<WorkflowPlanStep> & Pick<WorkflowPlanStep, "step" | "label">): WorkflowPlanStep => ({
  operation: { id: "research.pythonBatch", version: 1 },
  operationTitle: "Python batch",
  runner: "restricted-python",
  runnerTitle: "Restricted Python batch",
  containment: "restrictedContainer",
  effects: ["readDeclaredInputs", "executeRestricted"],
  payload: null,
  parameters: {},
  bindings: [],
  prerequisites: [],
  timeLimitSeconds: 60,
  maxOutputBytes: 1024,
  available: true,
  availability: "Available",
  maxModelRequests: 0,
  compositePath: [],
  condition: null,
  repeat: null,
  ...overrides,
});

const plan = (overrides: Partial<WorkflowRunPlan> = {}): WorkflowRunPlan => ({
  plan: "plan-1",
  workflow: "wf-1",
  revision: 2,
  definitionHash: "d".repeat(64),
  title: "Study",
  project: "thesis",
  inputs: [{ id: "data", label: "Data", portType: "dataset", path: "data/obs.csv", sha256: HASH, bytes: 100, pinned: false, text: null }],
  steps: [
    planStep({ step: "audit", label: "Audit", containment: "nativeAccount", operationTitle: "Lean audit" }),
    planStep({ step: "comp/fit", label: "Fit", compositePath: ["Fit model"], payload: { kind: "code", language: "python", source: "print('fit')\nprint(2)" } }),
    planStep({ step: "comp/plot", label: "Plot", compositePath: ["Fit model"], payload: { kind: "code", language: "python", source: "print('plot')" } }),
    planStep({
      step: "sweep",
      label: "Sweep",
      repeat: { kind: "forEach", parameter: "seed", values: [{ kind: "integer", value: 1 }, { kind: "integer", value: 2 }, { kind: "integer", value: 3 }] },
    }),
    planStep({
      step: "review",
      label: "Review",
      containment: "readOnlyModel",
      maxModelRequests: 2,
      condition: { source: { step: "audit", port: "evidence" }, test: { kind: "verdictIn", verdicts: ["Checked"] }, negate: false },
      payload: { kind: "prompt", text: "Summarize." },
    }),
    planStep({
      step: "refine",
      label: "Refine",
      repeat: { kind: "until", port: "summary", test: { kind: "jsonCompare", pointer: "/err", op: "lt", value: { kind: "number", value: "0.01" } }, negate: false, maxIterations: 5, onExhausted: "fail" },
    }),
  ],
  ceiling: { execution: "trustedNative", modelRequests: true, retainResults: true, proposeChanges: false },
  budgets: { maxSteps: 20, maxTotalSeconds: 900, maxOutputBytes: 1 << 20, maxModelRequests: 4 },
  execution: { restrictedBackend: "docker", restrictedImage: "python:3.13-slim", restrictedImageId: `sha256:${"e".repeat(64)}`, trustedFolder: true, nativeSteps: ["audit"], modelSteps: ["review"] },
  notices: [],
  blockers: [],
  approvalHash: "f".repeat(64),
  createdAtMs: 1_700_000_000_000,
  expiresAtMs: 1_700_001_800_000,
  maxParallel: 3,
  limits: { maxActiveRuns: 3, maxInFlightSteps: 4, restricted: 2, native: 2, agent: 2, lean: 1 },
  ...overrides,
});

const env: WorkflowEnvironment = {
  runner: "restricted-python",
  containment: "restrictedContainer",
  os: "linux",
  arch: "x86_64",
  appVersion: "0.1.0",
  image: { name: "python:3.13-slim", id: `sha256:${"e".repeat(64)}` },
  interpreter: { name: "Python", version: "3.13.1" },
  packages: [
    { name: "numpy", version: "2.3.0" },
    { name: "pandas", version: "2.3.1" },
  ],
  packagesDigest: "c".repeat(64),
  toolchain: null,
  model: null,
  seed: 4242,
  notes: ["Packages captured inside the approved image."],
};

const attempt = (overrides: Partial<WorkflowStepAttempt> = {}): WorkflowStepAttempt => ({
  attempt: 1,
  state: "succeeded",
  operation: { kind: "researchRun", id: "rr-1" },
  intentAtMs: 1_700_000_000_000,
  dispatchedAtMs: 1_700_000_000_100,
  finishedAtMs: 1_700_000_004_100,
  code: null,
  message: null,
  outputs: [],
  rejected: [],
  log: null,
  environment: env,
  ...overrides,
});

const runStep = (step: string, label: string, state: WorkflowRun["steps"][number]["state"], extra: Partial<WorkflowRun["steps"][number]> = {}): WorkflowRun["steps"][number] => ({
  step,
  label,
  operationTitle: "Python batch",
  runnerTitle: "Restricted Python batch",
  containment: "restrictedContainer",
  state,
  attempts: [],
  instances: [],
  reason: null,
  ...extra,
});

const run = (overrides: Partial<WorkflowRun> = {}): WorkflowRun => ({
  id: "run-2",
  cause: { kind: "manual" },
  workflow: "wf-1",
  revision: 2,
  definitionHash: "d".repeat(64),
  title: "Study",
  state: "running",
  code: null,
  message: null,
  startedAtMs: 1_700_000_000_000,
  finishedAtMs: null,
  budgets: { maxSteps: 20, maxTotalSeconds: 900, maxOutputBytes: 1000, maxModelRequests: 4 },
  usage: { steps: 4, seconds: 10, outputBytes: 100, modelRequests: 0 },
  steps: [
    runStep("audit", "Audit", "succeeded", { attempts: [attempt({ environment: { ...env, runner: "lean", containment: "nativeAccount", image: null, interpreter: null, packages: [], packagesDigest: null, toolchain: { name: "Lean", version: "4.35.0", manifestDigest: "9".repeat(64) }, seed: null, notes: [] } })] }),
    runStep("comp/fit", "Fit", "running", { attempts: [attempt({ state: "dispatched", finishedAtMs: null })] }),
    runStep("comp/plot", "Plot", "skipped", { reason: "“Fit → figure” was not produced." }),
    runStep("sweep", "Sweep", "running", {
      instances: [
        {
          index: 0,
          parameter: { kind: "integer", value: 1 },
          state: "succeeded",
          reason: null,
          attempts: [
            attempt({
              outputs: [
                {
                  port: "summary",
                  portType: "dataset",
                  value: { kind: "file", path: "research/runs/r9/out.bin", sha256: HASH, bytes: 12, mediaType: "application/octet-stream", researchRun: "rr-9", artifact: "a9" },
                  provenance: { run: "run-2", step: "sweep", attempt: 1, instance: 0, operation: { kind: "researchRun", id: "rr-9" } } as never,
                },
              ],
            }),
          ],
        },
        { index: 1, parameter: { kind: "integer", value: 2 }, state: "running", attempts: [attempt({ state: "dispatched", finishedAtMs: null, environment: undefined })], reason: null },
        { index: 2, parameter: { kind: "integer", value: 3 }, state: "pending", attempts: [], reason: null },
      ],
    }),
    runStep("review", "Review", "conditionNotMet", { containment: "readOnlyModel", reason: "The verdict was “Errors”." }),
    runStep("refine", "Refine", "pending"),
  ],
  outcomes: [
    { id: "fig", label: "Figure", optional: true, step: "comp/plot", port: "figure", value: null, reason: "Plot was skipped: its input was not produced." },
    { id: "report", label: "Report", optional: false, step: "review", port: "report", value: null, reason: "Review's condition was not met." },
  ],
  acknowledged: false,
  plan: plan(),
  ...overrides,
});

beforeEach(() => {
  vi.clearAllMocks();
});
afterEach(() => cleanup());

// ----- plan review ---------------------------------------------------------------------------

describe("plan review (M9)", () => {
  it("groups expanded composite steps under their composite with their complete code, and states conditions, iteration bounds, max parallel and the global limits", async () => {
    runIpc.plan.mockResolvedValue(plan());
    render(<PlanReviewDialog workflow="wf-1" revision={2} isOpen onOpenChange={() => {}} onStarted={vi.fn()} />);
    await screen.findByTestId("workflow-plan-details");
    const group = screen.getByTestId("plan-group-comp");
    expect(group.getAttribute("data-composite-path")).toBe("Fit model");
    expect(group.textContent).toContain("Inside the workflow step “Fit model” — 2 steps, shown in full");
    expect(within(group).getByTestId("plan-step-comp/fit")).toBeTruthy();
    expect(within(group).getByTestId("plan-code-comp/fit").textContent).toBe("print('fit')\nprint(2)\n");
    expect(within(group).getByTestId("plan-code-comp/plot")).toBeTruthy();
    expect(within(group).queryByTestId("plan-step-audit")).toBeNull();
    expect(screen.getByTestId("plan-condition-review").textContent).toMatch(
      /Runs only if the audit verdict of “Audit → evidence” is one of “Checked”; otherwise it is skipped \(condition not met\)/,
    );
    expect(screen.getByTestId("plan-repeat-sweep").textContent).toMatch(/Runs once for each of 3 values of “seed” \(1, 2, 3\): 3 instances/);
    expect(screen.getByTestId("plan-repeat-refine").textContent).toMatch(/until the JSON value at \/err in its output “summary” is less than 0.01; at most 5 iterations.*the step fails/);
    const concurrency = screen.getByTestId("plan-concurrency");
    expect(concurrency.getAttribute("data-max-parallel")).toBe("3");
    expect(concurrency.textContent).toMatch(/Up to 3 independent steps/);
    expect(concurrency.textContent).toMatch(/at most 3 active runs \(one per workflow\) and 4 steps in flight; per kind at once: 2 restricted container, 2 native, 2 model, 1 Lean/);
    expect(screen.getByText("Steps, in dependency order (up to 3 at a time)")).toBeTruthy();
    expect(screen.getByTestId("plan-limits-review").textContent).toBe(
      "At most 1 min and 1.0 KiB of output; up to 2 model requests — reserved from the run's budgets before the step starts, so steps running together cannot exceed them",
    );
  });

  it("keeps the M8 wording for a one-at-a-time plan without composites", async () => {
    runIpc.plan.mockResolvedValue(plan({ maxParallel: 1, steps: [planStep({ step: "a", label: "A" })] }));
    render(<PlanReviewDialog workflow="wf-1" revision={2} isOpen onOpenChange={() => {}} onStarted={vi.fn()} />);
    await screen.findByTestId("workflow-plan-details");
    expect(screen.getByText("Steps, in the order they run (one at a time)")).toBeTruthy();
    expect(screen.getByTestId("plan-concurrency").textContent).toMatch(/^One step at a time\./);
    expect(document.querySelector("[data-testid^='plan-group-']")).toBeNull();
    expect(groupByComposite([planStep({ step: "a", label: "A" }), planStep({ step: "c/x", label: "X", compositePath: ["C"] }), planStep({ step: "b", label: "B" })]).map((g) => g.prefix)).toEqual([
      "",
      "c",
      "",
    ]);
  });
});

// ----- run monitor ---------------------------------------------------------------------------

describe("run monitor (M9)", () => {
  it("shows every step and instance in flight at once", () => {
    render(<RunMonitor run={run()} onRun={vi.fn()} />);
    // A running run has no manifest to export yet.
    expect(screen.queryByTestId("workflow-run-export-manifest")).toBeNull();
    const now = screen.getByTestId("workflow-run-concurrent");
    expect(now.getAttribute("data-count")).toBe("2");
    expect(now.textContent).toContain("Running now (2 of at most 3 at once)");
    expect(screen.getByTestId("workflow-running-comp/fit").textContent).toContain("Fit");
    expect(screen.getByTestId("workflow-running-sweep-1").textContent).toContain("seed = 2");
    expect(runModel.runningNow(run()).map((r) => [r.step, r.instance])).toEqual([
      ["comp/fit", null],
      ["sweep", 1],
    ]);
  });

  it("lists repeat instances with their parameter values and states", () => {
    render(<RunMonitor run={run()} onRun={vi.fn()} />);
    const step = screen.getByTestId("workflow-run-step-sweep");
    expect(step.getAttribute("data-instances")).toBe("3");
    expect(screen.getByTestId("workflow-instances-summary-sweep").textContent).toBe("3 instances: 1 succeeded, 1 running, 1 not reached");
    const second = screen.getByTestId("workflow-instance-sweep-1");
    expect(second.getAttribute("data-state")).toBe("running");
    expect(second.textContent).toContain("seed = 2");
    expect(second.textContent).toContain("instance 2");
    expect(screen.getByTestId("workflow-attempt-sweep-i0-1").getAttribute("data-state")).toBe("succeeded");
    // The output names the instance that produced it.
    expect(screen.getByTestId("workflow-output-sweep-summary").textContent).toContain("(attempt 1, instance 1)");
    expect(screen.getByTestId("workflow-run-repeat-sweep").textContent).toMatch(/Runs once for each of 3 values/);
    // An until iteration has no parameter: it is named by its iteration.
    expect(runModel.instanceLabel({ index: 1, parameter: null, state: "succeeded", attempts: [], reason: null })).toBe("Iteration 2");
  });

  it("shows Condition not met with its reason, distinct from Skipped, and why outcomes were not produced", () => {
    render(<RunMonitor run={run()} onRun={vi.fn()} />);
    const review = screen.getByTestId("workflow-run-step-review");
    expect(review.getAttribute("data-state")).toBe("conditionNotMet");
    expect(review.textContent).toContain("Condition not met");
    const reason = screen.getByTestId("workflow-step-reason-review");
    expect(reason.getAttribute("data-state")).toBe("conditionNotMet");
    expect(reason.textContent).toBe("Not run: its condition was not met. The verdict was “Errors”. It runs only if the audit verdict of “Audit → evidence” is one of “Checked”.");
    const skipped = screen.getByTestId("workflow-step-reason-comp/plot");
    expect(skipped.getAttribute("data-state")).toBe("skipped");
    expect(skipped.textContent).toBe("Not run: an earlier step did not produce what it needs. “Fit → figure” was not produced.");
    expect(screen.getByTestId("workflow-run-step-comp/plot").textContent).toContain("Skipped");
    expect(screen.getByTestId("workflow-run-step-comp/plot").textContent).toContain("Inside “Fit model”");
    expect(screen.getByTestId("workflow-outcome-reason-report").textContent).toBe("Review's condition was not met.");
    expect(screen.getByTestId("workflow-outcome-reason-fig").textContent).toMatch(/skipped/);
  });

  it("states the run's cause: manual, a trigger with the changed inputs (old → new digests), or a reproduction", () => {
    const { rerender } = render(<RunMonitor run={run()} onRun={vi.fn()} />);
    expect(screen.getByTestId("workflow-run-cause").getAttribute("data-kind")).toBe("manual");
    expect(screen.getByTestId("workflow-run-cause").textContent).toContain("Started by you from a reviewed plan");
    rerender(<RunMonitor run={run({ cause: { kind: "trigger", trigger: "trg-1", changed: [{ input: "data", before: "1".repeat(64), after: "2".repeat(64) }] } })} onRun={vi.fn()} />);
    const cause = screen.getByTestId("workflow-run-cause");
    expect(cause.getAttribute("data-kind")).toBe("trigger");
    expect(cause.textContent).toContain("Started by trigger trg-1 under its approved standing plan after 1 watched input changed");
    const change = cause.querySelector('[data-input="data"]')!;
    expect(change.textContent).toContain("data/obs.csv");
    expect(change.textContent).toContain("111111111111… → 222222222222…");
    rerender(<RunMonitor run={run({ state: "succeeded", finishedAtMs: 1_700_000_009_000, cause: { kind: "reproduce", of: "run-1" } })} onRun={vi.fn()} />);
    expect(screen.getByTestId("workflow-run-cause").textContent).toContain("Reproduction of run run-1");
    fireEvent.click(screen.getByTestId("workflow-run-cause-open"));
    expect(editorsMock.open).toHaveBeenCalledWith({ key: "workflow-run:run-1", title: "Run: Study" });
  });

  it("shows each attempt's recorded environment: runner, image identity, interpreter, packages + digest, toolchain, seed", () => {
    render(<RunMonitor run={run()} onRun={vi.fn()} />);
    const fit = screen.getByTestId("workflow-attempt-comp/fit-1");
    fireEvent.click(within(fit).getByRole("button", { name: "Environment" }));
    const view = screen.getByTestId("workflow-env-comp/fit-1");
    const fact = (label: string) => view.querySelector(`[data-fact="${label}"]`)!.textContent;
    expect(fact("Runner")).toContain("restricted-python (Restricted container)");
    expect(fact("System")).toBe("SystemLinux x86_64 · Exposition 0.1.0".replace("Linux", "linux"));
    expect(fact("Image")).toContain("python:3.13-slim");
    expect(view.querySelector('[data-fact="Image"] [data-full]')!.getAttribute("data-full")).toBe("e".repeat(64));
    expect(fact("Interpreter")).toContain("Python 3.13.1");
    expect(fact("Packages")).toContain("2 packages");
    expect(view.querySelector('[data-fact="Packages"] [data-full]')!.getAttribute("data-full")).toBe("c".repeat(64));
    expect(fact("Seed")).toContain("4242");
    fireEvent.click(within(view).getByRole("button", { name: /Show the 2 packages/ }));
    expect(within(view).getByRole("list", { name: "Packages" }).textContent).toContain("numpy 2.3.0");
    // Lean: toolchain + manifest digest.
    const audit = screen.getByTestId("workflow-attempt-audit-1");
    fireEvent.click(within(audit).getByRole("button", { name: "Environment" }));
    expect(screen.getByTestId("workflow-env-audit-1").querySelector('[data-fact="Toolchain"]')!.textContent).toContain("Lean 4.35.0");
    // An attempt without a recorded environment says so.
    expect(screen.getByTestId("workflow-env-missing-sweep-i1-1").textContent).toBe("No environment was recorded for this attempt.");
  });

  it("exports the run manifest and shows where it was written", async () => {
    runIpc.exportManifest.mockResolvedValue({ path: ".exposition/manifests/run-2.json", bytes: 4096, sha256: HASH });
    render(<RunMonitor run={run({ state: "succeeded", finishedAtMs: 1_700_000_009_000 })} onRun={vi.fn()} />);
    fireEvent.click(screen.getByTestId("workflow-run-export-manifest"));
    await waitFor(() => expect(runIpc.exportManifest).toHaveBeenCalledWith("run-2"));
    const done = await screen.findByTestId("workflow-run-manifest");
    expect(done.getAttribute("data-path")).toBe(".exposition/manifests/run-2.json");
    expect(done.textContent).toContain("4.0 KiB");
    expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ tone: "success", title: "Run manifest exported" }));
    // The same JSON can be read in place, on request only.
    expect(runIpc.manifest).not.toHaveBeenCalled();
    runIpc.manifest.mockResolvedValue('{"schema":"exposition.workflow-run-manifest/1","run":"run-2","approvalHash":"fff"}');
    fireEvent.click(screen.getByTestId("workflow-run-manifest-show"));
    await waitFor(() => expect(screen.getByTestId("workflow-run-manifest-view").getAttribute("data-state")).toBe("ready"));
    expect(runIpc.manifest).toHaveBeenCalledWith("run-2");
    expect(screen.getByLabelText("Run manifest (JSON)").textContent).toContain('"approvalHash": "fff"');
    // Dismissing the backend's save dialog is not an error.
    runIpc.exportManifest.mockRejectedValueOnce({ code: "workflow.exportCanceled", message: "Export canceled." });
    fireEvent.click(screen.getByTestId("workflow-run-export-manifest"));
    await waitFor(() => expect(runIpc.exportManifest).toHaveBeenCalledTimes(2));
    await waitFor(() => expect((screen.getByTestId("workflow-run-export-manifest") as HTMLButtonElement).disabled).toBe(false));
    expect(screen.queryByText("Not done")).toBeNull();
  });

  it("reproduces a run through a reviewed plan with drift notices, started with that plan's approval", async () => {
    const driftNotices = [
      { step: "comp/fit", aspect: "image", recorded: `sha256:${"e".repeat(64)}`, current: `sha256:${"7".repeat(64)}`, message: "The restricted image changed since the run." },
      { step: "review", aspect: "model", recorded: "claude-opus-5-5", current: "claude-opus-5-6", message: "The review model differs." },
    ];
    // Lane D: a wrapper around a normal plan; the drift is also summarized in the plan's notices.
    runIpc.reproducePlan.mockResolvedValue({
      plan: plan({ plan: "plan-r", approvalHash: "9".repeat(64), inputs: [{ ...plan().inputs[0]!, pinned: true }], notices: [driftNotices[0]!.message, "Step Review sends material to the model provider."] }),
      reproduces: "run-2",
      drift: driftNotices,
    });
    runIpc.start.mockResolvedValue(run({ id: "run-3", cause: { kind: "reproduce", of: "run-2" } }));
    render(<RunMonitor run={run({ state: "succeeded", finishedAtMs: 1_700_000_009_000 })} onRun={vi.fn()} />);
    fireEvent.click(screen.getByTestId("workflow-run-reproduce"));
    await screen.findByTestId("workflow-plan-details");
    expect(runIpc.reproducePlan).toHaveBeenCalledWith("run-2");
    expect(runIpc.plan).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog").textContent).toContain("Reproduce “Study”");
    const drift = screen.getByTestId("plan-drift");
    expect(drift.querySelectorAll("li")).toHaveLength(2);
    expect(drift.querySelector('[data-aspect="image"]')!.textContent).toContain("The restricted image changed since the run.");
    expect(drift.querySelector('[data-aspect="model"]')!.textContent).toContain("claude-opus-5-6");
    // Each fact once: the drift's summary in the notices is not repeated; other notices stay.
    const notices = within(screen.getByRole("dialog")).getByTestId("plan-notices").textContent!;
    expect(notices).not.toContain("The restricted image changed since the run.");
    expect(notices).toContain("sends material to the model provider");
    expect(screen.getByTestId("plan-reproduces").textContent).toContain("run-2");
    expect(within(screen.getByRole("dialog")).getByTestId("plan-input-data").textContent).toContain("Pinned");
    expect(runIpc.start).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("workflow-start-run"));
    await waitFor(() => expect(runIpc.start).toHaveBeenCalledWith({ plan: "plan-r", approvalHash: "9".repeat(64) }));
    await waitFor(() => expect(editorsMock.open).toHaveBeenCalledWith({ key: "workflow-run:run-3", title: "Run: Study" }));
  });

  it("marks triggered and reproduced runs in the history", async () => {
    runIpc.list.mockResolvedValue([
      { id: "a", workflow: "wf-1", revision: 2, title: "Study", state: "succeeded", startedAtMs: 3, finishedAtMs: 4, stepsDone: 2, stepsTotal: 2, acknowledged: false, cause: { kind: "trigger", trigger: "trg-1", changed: [] } },
      { id: "b", workflow: "wf-1", revision: 2, title: "Study", state: "succeeded", startedAtMs: 2, finishedAtMs: 3, stepsDone: 2, stepsTotal: 2, acknowledged: false, cause: { kind: "reproduce", of: "c" } },
      { id: "c", workflow: "wf-1", revision: 2, title: "Study", state: "succeeded", startedAtMs: 1, finishedAtMs: 2, stepsDone: 2, stepsTotal: 2, acknowledged: false, cause: { kind: "manual" } },
    ]);
    render(<RunHistory workflow="wf-1" />);
    await screen.findByTestId("workflow-runs");
    expect(screen.getByTestId("workflow-run-item-a").textContent).toContain("Trigger");
    expect(screen.getByTestId("workflow-run-item-b").textContent).toContain("Reproduction");
    expect(screen.getByTestId("workflow-run-item-c").textContent).not.toMatch(/Trigger|Reproduction/);
  });
});

// ----- triggers ------------------------------------------------------------------------------

const definition: WorkflowDefinition = {
  schema: "exposition.workflow/2",
  title: "Study",
  description: "",
  preset: null,
  inputs: [
    { id: "data", label: "Data", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } },
    { id: "script", label: "Script", portType: "codeSource", source: { kind: "resource", path: "fit.py", version: { kind: "atRunStart" } } },
  ],
  steps: [],
  connections: [],
  outcomes: [],
  ceiling: { execution: "trustedNative", modelRequests: false, retainResults: true, proposeChanges: false },
};

const watchedData = { input: "data", label: "Data", path: "data/obs.csv", portType: "dataset" as const };
const limits = { minDebounceSeconds: 2, maxDebounceSeconds: 3600, minIntervalMinutes: 15, maxIntervalMinutes: 10080, maxRunsPerHour: 12, maxRunsPerDay: 100, maxModelRequestsPerDay: 1000, maxWatchedInputs: 16 };

const trigger = (overrides: Partial<WorkflowTrigger> = {}): WorkflowTrigger => ({
  id: "wtrg-1",
  workflow: "wf-1",
  revision: 2,
  title: "Study",
  project: "thesis",
  kind: { kind: "inputsChanged", inputs: ["data"] },
  watched: [watchedData],
  debounceSeconds: 30,
  maxRunsPerHour: 6,
  maxRunsPerDay: 24,
  allowNative: false,
  maxModelRequestsPerDay: 0,
  modelRequestsPerRun: 0,
  standingHash: "5".repeat(64),
  state: { kind: "active" },
  createdAtMs: 1_700_000_000_000,
  enabledAtMs: 1_700_000_000_000,
  lastFiredMs: 1_700_000_500_000,
  queued: false,
  nextCheckMs: null,
  listening: true,
  runsLastHour: 1,
  runsLastDay: 3,
  modelRequestsLastDay: 0,
  history: [],
  ...overrides,
});

const standing = (overrides: Partial<WorkflowTriggerPlan> = {}): WorkflowTriggerPlan => ({
  plan: plan({ plan: "plan-s" }),
  kind: { kind: "inputsChanged", inputs: ["data"] },
  watched: [watchedData],
  standingHash: "6".repeat(64),
  mayChange: ["The saved content of Data (data/obs.csv)"],
  suspendsOn: ["A new revision of the workflow", "Folder trust", "The restricted image identity", "Any code input or step code"],
  nativeSteps: ["Audit"],
  modelRequestsPerRun: 2,
  defaults: { debounceSeconds: 30, maxRunsPerHour: 6, maxRunsPerDay: 24, allowNative: false, maxModelRequestsPerDay: 20 },
  limits,
  notices: ["Runs start unattended while the project is open."],
  blockers: [],
  expiresAtMs: 1_700_001_800_000,
  ...overrides,
});

describe("triggers panel", () => {
  it("lists triggers with state, suspended reason, last fired, counts, queued, listening and history; turns one off", async () => {
    triggerIpc.list.mockResolvedValue([
      trigger({
        queued: true,
        history: [
          { atMs: 1_700_000_500_000, event: { kind: "fired", run: "run-9", changed: [{ input: "data", before: "1".repeat(64), after: "2".repeat(64) }] } },
          { atMs: 1_700_000_600_000, event: { kind: "queued" } },
          { atMs: 1_700_000_700_000, event: { kind: "rateLimited", untilMs: 1_700_003_600_000 } },
        ],
      }),
      trigger({ id: "wtrg-2", state: { kind: "suspended", code: "trigger.planChanged", message: "The workflow has a new revision (3)." }, lastFiredMs: null }),
      trigger({ id: "wtrg-3", kind: { kind: "interval", minutes: 60, inputs: [] }, watched: [], state: { kind: "disabled" } }),
      trigger({ id: "wtrg-4", listening: false }),
    ]);
    triggerIpc.disable.mockResolvedValue(trigger({ state: { kind: "disabled" } }));
    render(<TriggersSection workflow="wf-1" revision={3} definition={definition} reviewBlock={null} />);
    const active = await screen.findByTestId("workflow-trigger-wtrg-1");
    expect(triggerIpc.list).toHaveBeenCalledWith("wf-1");
    expect(active.getAttribute("data-state")).toBe("active");
    expect(active.textContent).toContain("When the saved content of Data (data/obs.csv) changes");
    expect(active.textContent).toContain("Active");
    expect(active.textContent).toContain("1 run queued");
    expect(active.textContent).toContain("Revision 2 (the workflow is now at revision 3)");
    expect(active.textContent).toMatch(/Waits for 30 s without further changes · at most 6 runs an hour and 24 a day/);
    expect(screen.getByTestId("trigger-last-fired-wtrg-1").textContent).toMatch(/^Last fired .* runs this hour 1 of 6, today 3 of 24/);
    fireEvent.click(within(active).getByRole("button", { name: /Recent history \(3\)/ }));
    const history = screen.getByTestId("trigger-history-wtrg-1");
    expect([...history.querySelectorAll("li")].map((li) => li.getAttribute("data-event"))).toEqual(["rateLimited", "queued", "fired"]);
    expect(history.textContent).toContain("Started a run after 1 watched input changed");
    expect(history.textContent).toContain("(data: 111111111111… → 222222222222…)");
    fireEvent.click(within(history).getByRole("button", { name: "Open run" }));
    expect(editorsMock.open).toHaveBeenCalledWith({ key: "workflow-run:run-9", title: "Run: Study" });
    const suspended = screen.getByTestId("trigger-suspended-wtrg-2");
    expect(suspended.getAttribute("data-code")).toBe("trigger.planChanged");
    expect(suspended.textContent).toContain("The workflow has a new revision (3).");
    expect(suspended.textContent).toContain("Suspended: the approved plan no longer matches");
    expect(screen.getByTestId("trigger-last-fired-wtrg-2").textContent).toMatch(/^Has not fired yet/);
    expect(screen.getByTestId("workflow-trigger-wtrg-3").textContent).toContain("Every 1 h");
    expect(screen.getByTestId("workflow-trigger-wtrg-3").textContent).toContain("Off");
    expect(screen.getByTestId("workflow-trigger-wtrg-4").textContent).toContain("Not listening yet");
    fireEvent.click(screen.getByTestId("trigger-disable-wtrg-1"));
    await waitFor(() => expect(triggerIpc.disable).toHaveBeenCalledWith("wtrg-1"));
    await waitFor(() => expect(triggerIpc.list).toHaveBeenCalledTimes(2));
  });

  it("enables a trigger only after reviewing its standing plan, with explicit native permission, sending the plan id, standing hash, and settings", async () => {
    triggerIpc.list.mockResolvedValue([]);
    triggerIpc.plan.mockResolvedValue(standing());
    triggerIpc.enable.mockResolvedValue(trigger());
    render(<TriggersSection workflow="wf-1" revision={2} definition={definition} reviewBlock={null} />);
    await screen.findByText("No triggers");
    fireEvent.click(screen.getByTestId("workflow-trigger-add"));
    const dialog = await screen.findByTestId("trigger-dialog");
    expect(dialog.getAttribute("data-phase")).toBe("choose");
    // Nothing watched yet: the review is not offered.
    expect((screen.getByTestId("trigger-review") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("trigger-problems").textContent).toContain("Choose at least one input to watch.");
    // Code inputs cannot be watched.
    expect(screen.queryByTestId("trigger-watch-script")).toBeNull();
    expect(screen.getByTestId("trigger-watch").textContent).toMatch(/Code inputs cannot be watched \(Script\)/);
    fireEvent.click(within(screen.getByTestId("trigger-watch-data")).getByRole("checkbox"));
    fireEvent.click(screen.getByTestId("trigger-review"));
    await waitFor(() => expect(triggerIpc.plan).toHaveBeenCalledWith({ workflow: "wf-1", kind: { kind: "inputsChanged", inputs: ["data"] } }));
    const review = await screen.findByTestId("trigger-standing");
    expect(review.getAttribute("data-standing-hash")).toBe("6".repeat(64));
    expect(screen.getByTestId("trigger-may-change-list").textContent).toBe("The saved content of Data (data/obs.csv)");
    expect(screen.getByTestId("trigger-suspends-on").querySelectorAll("li")).toHaveLength(4);
    expect(screen.getByTestId("trigger-notices").textContent).toContain("unattended");
    // Defaults come from the plan; native steps must be allowed explicitly.
    expect((screen.getByTestId("trigger-debounce").querySelector("input") as HTMLInputElement).value).toBe("30");
    const native = screen.getByTestId("trigger-native");
    expect(native.getAttribute("data-allowed")).toBe("false");
    expect(native.textContent).toContain("Runs natively with your account's privileges on every fire: “Audit”.");
    expect((screen.getByTestId("trigger-enable") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("trigger-enable-reason").textContent).toMatch(/allow them explicitly/);
    // Rate limits are bounded by the plan's limits: 13 an hour is refused.
    fireEvent.change(screen.getByTestId("trigger-per-hour").querySelector("input")!, { target: { value: "13" } });
    expect(screen.getByTestId("trigger-per-hour").textContent).toContain("A whole number from 1 to 12.");
    fireEvent.change(screen.getByTestId("trigger-per-hour").querySelector("input")!, { target: { value: "4" } });
    fireEvent.change(screen.getByTestId("trigger-debounce").querySelector("input")!, { target: { value: "3" } });
    // Model requests: at least one run's budget (2).
    expect(screen.getByTestId("trigger-model-per-day").textContent).toContain("(2–1000)");
    fireEvent.click(within(native).getByRole("checkbox"));
    await waitFor(() => expect((screen.getByTestId("trigger-enable") as HTMLButtonElement).disabled).toBe(false));
    expect(within(review).getByTestId("workflow-plan-details").getAttribute("data-plan")).toBe("plan-s");
    expect(triggerIpc.enable).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("trigger-enable"));
    await waitFor(() =>
      expect(triggerIpc.enable).toHaveBeenCalledWith({
        plan: "plan-s",
        standingHash: "6".repeat(64),
        settings: { debounceSeconds: 3, maxRunsPerHour: 4, maxRunsPerDay: 24, allowNative: true, maxModelRequestsPerDay: 20 },
      }),
    );
  });

  it("refuses to enable a standing plan with blockers, shows a stale approval (trigger.planStale), re-reviews a suspended trigger, and offers no trigger for an unrunnable revision", async () => {
    triggerIpc.list.mockResolvedValue([trigger({ id: "wtrg-2", state: { kind: "suspended", code: "trigger.planChanged", message: "New revision." }, debounceSeconds: 9, allowNative: true })]);
    triggerIpc.plan
      .mockResolvedValueOnce(standing({ nativeSteps: [], modelRequestsPerRun: 0, blockers: [{ code: "workflow.untrustedFolder", message: "The folder is not trusted.", step: null, input: null }] }))
      .mockResolvedValueOnce(standing({ nativeSteps: [], modelRequestsPerRun: 0 }))
      .mockResolvedValueOnce(standing({ nativeSteps: [], modelRequestsPerRun: 0, standingHash: "7".repeat(64) }));
    triggerIpc.enable.mockRejectedValueOnce({ code: "trigger.planStale", message: "data/obs.csv changed after the review." });
    const { rerender } = render(<TriggersSection workflow="wf-1" revision={3} definition={definition} reviewBlock={null} />);
    // Review again: the trigger's kind and settings are carried into the new review.
    fireEvent.click(await screen.findByTestId("trigger-reapprove-wtrg-2"));
    await screen.findByTestId("trigger-dialog");
    fireEvent.click(screen.getByTestId("trigger-review"));
    await waitFor(() => expect(triggerIpc.plan).toHaveBeenCalledWith({ workflow: "wf-1", kind: { kind: "inputsChanged", inputs: ["data"] } }));
    const blockers = await screen.findByTestId("trigger-blockers");
    expect(blockers.textContent).toContain("The folder is not trusted.");
    expect((screen.getByTestId("trigger-debounce").querySelector("input") as HTMLInputElement).value).toBe("9");
    expect((screen.getByTestId("trigger-enable") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("trigger-enable-reason").textContent).toMatch(/1 blocker/);
    fireEvent.click(screen.getByTestId("trigger-back"));
    fireEvent.click(await screen.findByTestId("trigger-review"));
    await screen.findByTestId("trigger-standing");
    await waitFor(() => expect((screen.getByTestId("trigger-enable") as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByTestId("trigger-enable"));
    const stale = await screen.findByTestId("trigger-enable-error");
    expect(stale.getAttribute("data-code")).toBe("trigger.planStale");
    expect(stale.textContent).toContain("Nothing was enabled.");
    expect((screen.getByTestId("trigger-enable") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByTestId("trigger-review-new"));
    await waitFor(() => expect(screen.getByTestId("trigger-standing").getAttribute("data-standing-hash")).toBe("7".repeat(64)));
    expect(screen.queryByTestId("trigger-enable-error")).toBeNull();
    // A revision that cannot be reviewed offers no new trigger.
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    rerender(<TriggersSection workflow="wf-1" revision={3} definition={definition} reviewBlock="Save the draft first." />);
    await waitFor(() => expect((screen.getByTestId("workflow-trigger-add") as HTMLButtonElement).disabled).toBe(true));
  });
});

// ----- docs audit findings (each failed before its fix) -------------------------------------

describe("docs audit: triggers and verdicts", () => {
  it("says a new trigger replaces the workflow's current one (one trigger per workflow)", async () => {
    triggerIpc.list.mockResolvedValue([trigger({ id: "wtrg-1", state: { kind: "active" } })]);
    triggerIpc.plan.mockResolvedValue(standing({ nativeSteps: [], modelRequestsPerRun: 0 }));
    triggerIpc.enable.mockResolvedValue(trigger({ id: "wtrg-5" }));
    render(<TriggersSection workflow="wf-1" revision={2} definition={definition} reviewBlock={null} />);
    const add = await screen.findByTestId("workflow-trigger-add");
    await waitFor(() => expect(add.textContent).toBe("Replace trigger…"));
    fireEvent.click(add);
    expect((await screen.findByTestId("trigger-replaces")).textContent).toMatch(/This workflow already has a trigger \(active\)\. Enabling this one replaces it/);
    fireEvent.click(within(screen.getByTestId("trigger-watch-data")).getByRole("checkbox"));
    fireEvent.click(screen.getByTestId("trigger-review"));
    await screen.findByTestId("trigger-standing");
    expect(screen.getByTestId("trigger-enable").textContent).toBe("Replace trigger");
  });

  it("checks runs per hour against runs per day", () => {
    const base = { debounceSeconds: 30, maxRunsPerHour: 12, maxRunsPerDay: 5, allowNative: false, maxModelRequestsPerDay: 0 };
    expect(settingsProblems(base, limits, { nativeSteps: [], modelRequestsPerRun: 0 })).toContain("Runs per hour cannot exceed runs per day (5).");
    expect(settingsProblems({ ...base, maxRunsPerHour: 5 }, limits, { nativeSteps: [], modelRequestsPerRun: 0 })).toEqual([]);
  });

  it("shows the backend's suspension message once, under a title that fits its code", async () => {
    triggerIpc.list.mockResolvedValue([
      trigger({ id: "wtrg-t", state: { kind: "suspended", code: "trigger.trustRevoked", message: "Trust was revoked for the project folder, so the trigger is suspended until you review and enable it again." } }),
      trigger({ id: "wtrg-p", state: { kind: "suspended", code: "trigger.planChanged", message: "The revision changed (2 → 3). The trigger is suspended until you review and enable it again." } }),
    ]);
    render(<TriggersSection workflow="wf-1" revision={3} definition={definition} reviewBlock={null} />);
    const trust = await screen.findByTestId("trigger-suspended-wtrg-t");
    expect(trust.textContent).toContain("Suspended: folder trust was revoked");
    expect(trust.textContent).not.toContain("no longer matches");
    expect(trust.textContent).not.toContain("It does not fire until");
    expect(trust.textContent!.match(/until you review/g)).toHaveLength(1);
    expect(screen.getByTestId("trigger-suspended-wtrg-p").textContent).toContain("Suspended: the approved plan no longer matches");
    expect(suspensionTitle("trigger.nativeNotAllowed")).toBe("Suspended: native steps are not allowed");
    expect(suspensionTitle("workflow.untrustedFolder")).toBe("Suspended");
  });

  it("tones the audit's exact verdict labels by their class (only Checked is success)", () => {
    expect(runModel.verdictTone("Checked")).toBe("success");
    expect(runModel.verdictTone("Checked with nonstandard axioms")).toBe("warning");
    expect(runModel.verdictTone("Incomplete (1 sorry)")).toBe("warning");
    expect(runModel.verdictTone("Errors (2)")).toBe("danger");
    expect(runModel.verdictTone("Unverified: no audit evidence")).toBe("neutral");
  });
});

describe("lane F: R and Julia runners in plans and runs", () => {
  it("lists the image each R/Julia runner is bound to, with its identity", async () => {
    const base = plan();
    runIpc.plan.mockResolvedValue(
      plan({
        execution: {
          ...base.execution,
          runnerImages: [
            { runner: "restrictedR", image: "r-base:4.6.2", imageId: `sha256:${"a".repeat(64)}` },
            { runner: "restrictedJulia", image: "julia:1.13", imageId: null },
          ],
        },
      }),
    );
    render(<PlanReviewDialog workflow="wf-1" revision={2} isOpen onOpenChange={() => {}} onStarted={vi.fn()} />);
    const images = await screen.findByTestId("plan-runner-images");
    const r = images.querySelector('[data-runner="restrictedR"]')!;
    expect(r.textContent).toContain("r-base:4.6.2");
    expect(r.querySelector("[data-full]")!.getAttribute("data-full")).toBe("a".repeat(64));
    expect(images.querySelector('[data-runner="restrictedJulia"]')!.textContent).toContain("julia:1.13 (identity not recorded)");
  });

  it("omits runner images for plans without them", async () => {
    runIpc.plan.mockResolvedValue(plan());
    render(<PlanReviewDialog workflow="wf-1" revision={2} isOpen onOpenChange={() => {}} onStarted={vi.fn()} />);
    await screen.findByTestId("workflow-plan-details");
    expect(screen.queryByTestId("plan-runner-images")).toBeNull();
  });

  it("shows an R attempt's interpreter", () => {
    const facts = runModel.environmentFacts({ ...env, runner: "restrictedR", interpreter: { name: "R", version: "4.6.2" }, packages: [], packagesDigest: null });
    expect(facts.find((f) => f.label === "Interpreter")?.value).toBe("R 4.6.2");
  });
});
