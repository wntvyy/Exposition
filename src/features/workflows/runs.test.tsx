// Workflow runs (M8 UI): plan review (full payload, blockers, stale plan),
// the run monitor (states, cancel, interrupted + acknowledge, refused
// outputs, inert previews, association decisions), run history, and the
// canvas connection rules. IPC is mocked at the feature's boundary.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { OperationContract } from "../../bindings/OperationContract";
import type { WorkflowDefinition } from "./contract";
import type { WorkflowRun } from "./contract";
import type { WorkflowRunPlan } from "./contract";
import type { WorkflowStepAttempt } from "./contract";

const runIpc = vi.hoisted(() => ({
  plan: vi.fn(),
  start: vi.fn(),
  cancel: vi.fn(),
  get: vi.fn(),
  list: vi.fn(),
  acknowledge: vi.fn(),
  decideAssociation: vi.fn(),
  promotionPreview: vi.fn(),
}));
const eventHandlers = vi.hoisted(() => [] as ((e: { run: string; workflow: string }) => void)[]);
const events = vi.hoisted(() => ({
  run: vi.fn(async (handler: (e: { run: string; workflow: string }) => void) => {
    eventHandlers.push(handler);
    return () => {};
  }),
}));
const libIpc = vi.hoisted(() => ({ researchArtifact: vi.fn() }));
const editorsMock = vi.hoisted(() => ({ open: vi.fn(), setTitle: vi.fn(), setDirty: vi.fn(), registerCloseGuard: vi.fn(() => () => {}) }));
const research = vi.hoisted(() => ({ approvePromotion: vi.fn(), cancelPromotion: vi.fn() }));

vi.mock("./ipc", () => ({ workflowCommands: {}, workflowRunCommands: runIpc, workflowEvents: events }));
vi.mock("../../lib/ipc", () => ({
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
  isCommandError: (e: unknown) => typeof e === "object" && e !== null && "code" in e && "message" in e,
  commands: libIpc,
}));
vi.mock("../../app/editors", () => ({ editors: editorsMock }));
vi.mock("../../app/notifications", () => ({ notify: vi.fn(), dismiss: vi.fn() }));
vi.mock("../research/ipc", () => ({ researchCommands: research }));
vi.mock("../research/Promotion", () => ({
  PreviewView: ({ preview }: { preview: { path: string } }) => <div data-testid="promotion-preview">{preview.path}</div>,
}));

const { PlanReviewDialog } = await import("./PlanReview");
const { RunMonitor, RunHistory, WorkflowRunView } = await import("./RunView");
const canvasModel = await import("./canvasModel");
const { WorkflowCanvas } = await import("./WorkflowCanvas");

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
  URL.createObjectURL = vi.fn(() => "blob:preview-1");
  URL.revokeObjectURL = vi.fn();
});

const HASH = "ab12cd34ef56".padEnd(64, "0");
const CODE = "import numpy as np\nx = np.linspace(0, 1, 5)\nprint(x.sum())";

const plan = (overrides: Partial<WorkflowRunPlan> = {}): WorkflowRunPlan => ({
  plan: "plan-1",
  workflow: "wf-1",
  revision: 3,
  definitionHash: "d".repeat(64),
  title: "Numerical experiment",
  project: "thesis",
  inputs: [{ id: "data", label: "Measurements", portType: "dataset", path: "research/data/m.csv", sha256: HASH, bytes: 2048, pinned: true, text: null }],
  steps: [
    {
      step: "run",
      label: "Simulate",
      operation: { id: "python.batch", version: 1 },
      operationTitle: "Python batch",
      runner: "restricted-python",
      runnerTitle: "Restricted Python batch",
      containment: "restrictedContainer",
      effects: ["readDeclaredInputs", "executeRestricted", "writeRunOutputs"],
      payload: { kind: "code", language: "python", source: CODE },
      parameters: { seed: { kind: "integer", value: 7 } },
      bindings: [{ port: "data", from: { kind: "input", input: "data" } }],
      prerequisites: [],
      timeLimitSeconds: 120,
      maxOutputBytes: 10 * 1024 * 1024,
      available: true,
      availability: "Docker image python:3.13-slim is available",
    },
    {
      step: "review",
      label: "Summarize",
      operation: { id: "agent.review", version: 1 },
      operationTitle: "Read-only agent review",
      runner: "agent",
      runnerTitle: "Read-only agent",
      containment: "readOnlyModel",
      effects: ["readDeclaredInputs", "sendToModel"],
      payload: { kind: "prompt", text: "Summarize the figure.\nList caveats." },
      parameters: {},
      bindings: [{ port: "material", from: { kind: "step", step: "run", port: "figure" } }],
      prerequisites: ["run"],
      timeLimitSeconds: 300,
      maxOutputBytes: 65536,
      available: true,
      availability: "Provider configured",
    },
  ],
  ceiling: { execution: "restricted", modelRequests: true, retainResults: true, proposeChanges: false },
  budgets: { maxSteps: 10, maxTotalSeconds: 900, maxOutputBytes: 50 * 1024 * 1024, maxModelRequests: 4 },
  execution: { restrictedBackend: "docker", restrictedImage: "python:3.13-slim", restrictedImageId: `sha256:${"e".repeat(64)}`, trustedFolder: false, nativeSteps: [], modelSteps: ["review"] },
  notices: ["Step Summarize sends the figure to the model provider."],
  blockers: [],
  approvalHash: "f".repeat(64),
  createdAtMs: 1_700_000_000_000,
  expiresAtMs: 1_700_001_800_000,
  ...overrides,
});

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
  ...overrides,
});

const run = (overrides: Partial<WorkflowRun> = {}): WorkflowRun => ({
  id: "run-1",
  workflow: "wf-1",
  revision: 3,
  definitionHash: "d".repeat(64),
  title: "Numerical experiment",
  state: "succeeded",
  code: null,
  message: null,
  startedAtMs: 1_700_000_000_000,
  finishedAtMs: 1_700_000_010_000,
  budgets: { maxSteps: 10, maxTotalSeconds: 900, maxOutputBytes: 1000, maxModelRequests: 4 },
  usage: { steps: 2, seconds: 10, outputBytes: 500, modelRequests: 1 },
  steps: [
    { step: "run", label: "Simulate", operationTitle: "Python batch", runnerTitle: "Restricted Python batch", containment: "restrictedContainer", state: "succeeded", attempts: [attempt()] },
  ],
  outcomes: [{ id: "fig", label: "Retained figure", optional: false, step: "run", port: "figure", value: null }],
  acknowledged: false,
  plan: plan(),
  ...overrides,
});

beforeEach(() => {
  vi.clearAllMocks();
  eventHandlers.length = 0;
});
afterEach(() => cleanup());

describe("plan review", () => {
  function open(onStarted = vi.fn()) {
    render(<PlanReviewDialog workflow="wf-1" revision={3} isOpen onOpenChange={() => {}} onStarted={onStarted} />);
    return onStarted;
  }

  it("shows everything the run approves: inputs with hashes, ordered steps, containment, effects, full code and prompt, budgets", async () => {
    runIpc.plan.mockResolvedValue(plan());
    open();
    const details = await screen.findByTestId("workflow-plan-details");
    expect(runIpc.plan).toHaveBeenCalledWith("wf-1", 3);
    expect(screen.getByTestId("plan-project").textContent).toBe("thesis");
    // The restricted image is bound by identity, shown in full on hover.
    expect(screen.getByTestId("plan-image-id").querySelector("[data-full]")!.getAttribute("data-full")).toBe("e".repeat(64));
    // Input: path, short hash with the full hash on hover, size, pinned.
    const input = screen.getByTestId("plan-input-data");
    expect(input.textContent).toContain("research/data/m.csv");
    expect(input.textContent).toContain("ab12cd34ef56…");
    expect(input.querySelector(`[title="${HASH}"]`)).toBeTruthy();
    expect(input.textContent).toContain("2.0 KiB");
    expect(input.textContent).toContain("Pinned");
    expect(within(input).getByRole("button", { name: "Copy SHA-256 of research/data/m.csv" })).toBeTruthy();
    // Steps in execution order with containment and effects in plain words.
    const steps = [...details.querySelectorAll("[data-testid^='plan-step-']")].map((el) => el.getAttribute("data-testid"));
    expect(steps).toEqual(["plan-step-run", "plan-step-review"]);
    const sim = screen.getByTestId("plan-step-run");
    expect(sim.textContent).toContain("Restricted container");
    expect(sim.textContent).toContain("Runs code in the restricted container");
    expect(sim.textContent).toContain("seed");
    expect(sim.textContent).toContain("Input “Measurements” (research/data/m.csv)");
    expect(screen.getByTestId("plan-step-review").textContent).toContain("Sends material to the model provider");
    // The complete code (every line, numbered) and the complete prompt.
    const code = screen.getByTestId("plan-code-run");
    expect(code.textContent).toBe(`${CODE}\n`);
    expect(code.querySelectorAll("[data-line]")).toHaveLength(3);
    expect(screen.getByTestId("plan-prompt-review").textContent).toContain("List caveats.");
    expect(screen.getByTestId("plan-budgets").textContent).toContain("10 steps");
    expect(screen.getByTestId("plan-notices").textContent).toContain("sends the figure to the model provider");
    expect((screen.getByTestId("workflow-start-run") as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows the complete code of a script input on the step that runs it (R-m8-F5)", async () => {
    const script = "import os\n# MARKER-LINE\nprint(42)";
    const base = plan();
    runIpc.plan.mockResolvedValue(
      plan({
        inputs: [...base.inputs, { id: "script", label: "Python script", portType: "codeSource", path: "experiment.py", sha256: HASH, bytes: script.length, pinned: false, text: script }],
        steps: [{ ...base.steps[0]!, payload: null, bindings: [...base.steps[0]!.bindings, { port: "script", from: { kind: "input", input: "script" } }] }, base.steps[1]!],
      }),
    );
    open();
    await screen.findByTestId("workflow-plan-details");
    expect(screen.queryByTestId("plan-code-run")).toBeNull();
    const code = screen.getByTestId("plan-script-run");
    expect(code.textContent).toBe(`${script}\n`);
    expect(code.querySelectorAll("[data-line]")).toHaveLength(3);
    expect(screen.getByTestId("plan-step-run").textContent).toContain("Code from experiment.py");
  });

  it("starts only on Start run, with the plan id and its approval hash", async () => {
    runIpc.plan.mockResolvedValue(plan());
    runIpc.start.mockResolvedValue(run({ state: "running" }));
    const onStarted = open();
    await screen.findByTestId("workflow-plan-details");
    expect(runIpc.start).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("workflow-start-run"));
    await waitFor(() => expect(runIpc.start).toHaveBeenCalledWith({ plan: "plan-1", approvalHash: "f".repeat(64) }));
    await waitFor(() => expect(onStarted).toHaveBeenCalledWith(expect.objectContaining({ id: "run-1" })));
  });

  it("lists every blocker and disables Start run", async () => {
    runIpc.plan.mockResolvedValue(
      plan({
        blockers: [
          { code: "workflow.restrictedUnavailable", message: "No Docker image is configured for restricted runs.", step: "run", input: null },
          { code: "workflow.inputMissing", message: "research/data/m.csv does not exist.", step: null, input: "data" },
        ],
      }),
    );
    open();
    const blockers = await screen.findByTestId("plan-blockers");
    expect(blockers.textContent).toContain("No Docker image is configured for restricted runs.");
    expect(blockers.textContent).toContain("research/data/m.csv does not exist.");
    expect(blockers.querySelectorAll("li")).toHaveLength(2);
    expect((screen.getByTestId("workflow-start-run") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("plan-start-reason").textContent).toMatch(/2 blockers/);
    fireEvent.click(screen.getByTestId("workflow-start-run"));
    expect(runIpc.start).not.toHaveBeenCalled();
  });

  it("shows a stale approval inline and reviews the new plan on request", async () => {
    runIpc.plan.mockResolvedValueOnce(plan()).mockResolvedValueOnce(plan({ plan: "plan-2", approvalHash: "e".repeat(64) }));
    runIpc.start.mockRejectedValueOnce({ code: "workflow.planStale", message: "research/data/m.csv changed after the review." });
    open();
    await screen.findByTestId("workflow-plan-details");
    fireEvent.click(screen.getByTestId("workflow-start-run"));
    const error = await screen.findByTestId("plan-start-error");
    expect(error.getAttribute("data-code")).toBe("workflow.planStale");
    expect(error.textContent).toContain("changed after the review");
    expect(error.textContent).toContain("Nothing was started");
    expect((screen.getByTestId("workflow-start-run") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByTestId("plan-review-new"));
    await waitFor(() => expect(screen.getByTestId("workflow-plan-details").getAttribute("data-plan")).toBe("plan-2"));
    expect(runIpc.plan).toHaveBeenCalledTimes(2);
    expect(screen.queryByTestId("plan-start-error")).toBeNull();
    // Starting again uses the new plan's approval, never the stale one.
    runIpc.start.mockResolvedValue(run());
    fireEvent.click(screen.getByTestId("workflow-start-run"));
    await waitFor(() => expect(runIpc.start).toHaveBeenLastCalledWith({ plan: "plan-2", approvalHash: "e".repeat(64) }));
  });
});

describe("run monitor", () => {
  it("shows a running run with usage and cancels it", async () => {
    const onRun = vi.fn();
    runIpc.cancel.mockResolvedValue(run({ state: "canceled" }));
    render(<RunMonitor run={run({ state: "running", finishedAtMs: null, steps: [{ ...run().steps[0]!, state: "running", attempts: [attempt({ state: "dispatched", finishedAtMs: null })] }] })} onRun={onRun} />);
    expect(screen.getByTestId("workflow-run-state").getAttribute("data-state")).toBe("running");
    expect(screen.getByTestId("workflow-run-usage").textContent).toContain("2 of 10");
    expect(screen.getByTestId("workflow-run-step-run").getAttribute("data-state")).toBe("running");
    expect(screen.getByTestId("workflow-attempt-run-1").textContent).toContain("researchRun");
    fireEvent.click(screen.getByTestId("workflow-run-cancel"));
    await waitFor(() => expect(runIpc.cancel).toHaveBeenCalledWith("run-1"));
    await waitFor(() => expect(onRun).toHaveBeenCalledWith(expect.objectContaining({ state: "canceled" })));
  });

  it("distinguishes succeeded, failed, and canceled runs", () => {
    const { rerender } = render(<RunMonitor run={run()} onRun={vi.fn()} />);
    expect(screen.getByTestId("workflow-run-state").textContent).toContain("Run succeeded");
    expect(screen.queryByTestId("workflow-run-cancel")).toBeNull();
    rerender(
      <RunMonitor
        run={run({
          state: "failed",
          code: "research.batchFailed",
          message: "The script exited with status 1.",
          steps: [{ ...run().steps[0]!, state: "failed", attempts: [attempt({ state: "failed", code: "research.batchFailed", message: "Traceback: ZeroDivisionError", log: "line 3\nZeroDivisionError" })] }],
        })}
        onRun={vi.fn()}
      />,
    );
    expect(screen.getByTestId("workflow-run-failure").getAttribute("data-code")).toBe("research.batchFailed");
    expect(screen.getByTestId("workflow-attempt-error-run").textContent).toContain("ZeroDivisionError");
    expect(screen.getByTestId("workflow-log-run").tagName).toBe("PRE");
    rerender(<RunMonitor run={run({ state: "canceled" })} onRun={vi.fn()} />);
    expect(screen.getByText("This run was canceled")).toBeTruthy();
    expect(screen.getByTestId("workflow-run-state").textContent).toContain("Run canceled");
  });

  it("explains an interrupted run, acknowledges it, and offers a new review (never a resume)", async () => {
    const onRun = vi.fn();
    runIpc.acknowledge.mockResolvedValue(run({ state: "interrupted", acknowledged: true }));
    runIpc.plan.mockResolvedValue(plan({ plan: "plan-9" }));
    render(
      <RunMonitor
        run={run({ state: "interrupted", steps: [{ ...run().steps[0]!, state: "interrupted", attempts: [attempt({ state: "interrupted", finishedAtMs: null })] }] })}
        onRun={onRun}
      />,
    );
    const banner = screen.getByTestId("workflow-run-interrupted");
    expect(banner.textContent).toContain("The application stopped during this run");
    expect(banner.textContent).toContain("Nothing was repeated automatically");
    expect(screen.getByTestId("workflow-attempt-run-1").textContent).toContain("Outcome unknown");
    fireEvent.click(screen.getByTestId("workflow-run-acknowledge"));
    await waitFor(() => expect(runIpc.acknowledge).toHaveBeenCalledWith("run-1"));
    expect(onRun).toHaveBeenCalledWith(expect.objectContaining({ acknowledged: true }));
    fireEvent.click(screen.getByTestId("workflow-run-review-new"));
    await screen.findByTestId("workflow-plan-details");
    expect(runIpc.plan).toHaveBeenCalledWith("wf-1", 3);
    expect(runIpc.start).not.toHaveBeenCalled();
  });

  it("shows refused outputs with their reasons, distinct from validated outputs", () => {
    render(
      <RunMonitor
        run={run({
          steps: [
            {
              ...run().steps[0]!,
              attempts: [
                attempt({
                  outputs: [
                    {
                      port: "data",
                      portType: "dataset",
                      value: { kind: "file", path: "research/runs/r1/out.bin", sha256: HASH, bytes: 10, mediaType: "application/octet-stream", researchRun: "rr-1", artifact: "a1" },
                      provenance: { run: "run-1", step: "run", attempt: 1, operation: { kind: "researchRun", id: "rr-1" } },
                    },
                  ],
                  rejected: [{ name: "link-to-home", reason: "symbolic link escaping the output area" }],
                }),
              ],
            },
          ],
        })}
        onRun={vi.fn()}
      />,
    );
    const refused = screen.getByTestId("workflow-rejected-run");
    expect(refused.textContent).toContain("Refused:");
    expect(refused.textContent).toContain("link-to-home");
    expect(refused.textContent).toContain("symbolic link escaping the output area");
    const output = screen.getByTestId("workflow-output-run-data");
    expect(output.textContent).toContain("research/runs/r1/out.bin");
    expect(output.textContent).toContain("ab12cd34ef56…");
    // No preview attempted for an opaque file.
    expect(libIpc.researchArtifact).not.toHaveBeenCalled();
  });

  it("previews an SVG only through <img> from a blob URL and never injects markup", async () => {
    const svg = '<svg xmlns="http://www.w3.org/2000/svg"><script>window.pwned=1</script><rect width="4" height="4"/></svg>';
    libIpc.researchArtifact.mockResolvedValue(new TextEncoder().encode(svg).buffer);
    const file = (path: string, mediaType: string, artifact: string) => ({
      port: artifact,
      portType: "figure" as const,
      value: { kind: "file" as const, path, sha256: HASH, bytes: svg.length, mediaType, researchRun: "rr-1", artifact },
      provenance: { run: "run-1", step: "run", attempt: 1, operation: { kind: "researchRun", id: "rr-1" } },
    });
    const { container } = render(
      <RunMonitor
        run={run({
          steps: [{ ...run().steps[0]!, attempts: [attempt({ outputs: [file("research/runs/r1/plot.svg", "image/svg+xml", "fig"), file("research/runs/r1/page.html", "text/html", "html")] })] }],
        })}
        onRun={vi.fn()}
      />,
    );
    const img = await screen.findByTestId("workflow-output-image");
    expect(img.tagName).toBe("IMG");
    expect(img.getAttribute("src")).toBe("blob:preview-1");
    expect(img.getAttribute("alt")).toBe("Preview of research/runs/r1/plot.svg");
    expect(libIpc.researchArtifact).toHaveBeenCalledWith("rr-1", "fig");
    // The HTML output is listed by path and hash only; nothing was fetched or rendered for it.
    expect(libIpc.researchArtifact).not.toHaveBeenCalledWith("rr-1", "html");
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector('rect[width="4"]')).toBeNull();
    expect(container.innerHTML).not.toContain("pwned");
    expect(container.querySelector("iframe, object, embed")).toBeNull();
    expect((window as { pwned?: number }).pwned).toBeUndefined();
  });

  it("previews CSV as plain cells", async () => {
    libIpc.researchArtifact.mockResolvedValue(new TextEncoder().encode('x,y\n1,"<b>2</b>"\n3,4\n').buffer);
    const { container } = render(
      <RunMonitor
        run={run({
          steps: [
            {
              ...run().steps[0]!,
              attempts: [
                attempt({
                  outputs: [
                    {
                      port: "table",
                      portType: "table",
                      value: { kind: "file", path: "research/runs/r1/t.csv", sha256: HASH, bytes: 20, mediaType: "text/csv", researchRun: "rr-1", artifact: "t" },
                      provenance: { run: "run-1", step: "run", attempt: 1, operation: { kind: "researchRun", id: "rr-1" } },
                    },
                  ],
                }),
              ],
            },
          ],
        })}
        onRun={vi.fn()}
      />,
    );
    const table = await screen.findByTestId("workflow-output-table");
    expect([...table.querySelectorAll("th")].map((th) => th.textContent)).toEqual(["x", "y"]);
    expect(table.textContent).toContain("<b>2</b>");
    expect(container.querySelector("b")).toBeNull();
  });

  it("records an association decision for the selected declarations, with their exact verdicts", async () => {
    const onRun = vi.fn();
    const formal = {
      port: "evidence",
      portType: "formalEvidence" as const,
      value: {
        kind: "formalEvidence" as const,
        file: "Proofs/Main.lean",
        fileSha256: HASH,
        verdict: "Checked with axioms",
        declarations: [
          { name: "Main.thm", verdict: "Checked", statement: "theorem thm : 1 + 1 = 2", axioms: [] },
          { name: "Main.uses_choice", verdict: "Checked with axioms", statement: null, axioms: ["Classical.choice"] },
          { name: "Main.native", verdict: "Relies on compiled code", statement: null, axioms: ["Lean.ofReduceBool"] },
        ],
        evidence: ["ev-1"],
      },
      provenance: { run: "run-1", step: "audit", attempt: 1, operation: { kind: "leanAudit", id: "la-1" } },
    };
    const proposal = {
      port: "proposal",
      portType: "associationProposal" as const,
      value: { kind: "associationProposal" as const, proposal: "p-1", declarations: ["Main.thm", "Main.uses_choice", "Main.native"], status: "pending" as const, accepted: [] as string[], evidence: [] as string[] },
      provenance: { run: "run-1", step: "associate", attempt: 1, operation: { kind: "review", id: "rv-1" } },
    };
    const auditRun = run({
      steps: [
        { step: "audit", label: "Audit", operationTitle: "Lean audit", runnerTitle: "Lean", containment: "nativeAccount", state: "succeeded", attempts: [attempt({ outputs: [formal] })] },
        { step: "associate", label: "Associate", operationTitle: "Associate", runnerTitle: "Application", containment: "applicationService", state: "succeeded", attempts: [attempt({ outputs: [proposal] })] },
      ],
    });
    runIpc.decideAssociation.mockResolvedValue({ ...auditRun });
    render(<RunMonitor run={auditRun} onRun={onRun} />);
    const list = screen.getByTestId("workflow-association-checklist");
    expect(list.querySelector('[data-declaration="Main.thm"]')!.textContent).toContain("Checked");
    expect(list.querySelector('[data-declaration="Main.uses_choice"]')!.textContent).toContain("Checked with axioms");
    expect(list.querySelector('[data-declaration="Main.uses_choice"]')!.textContent).toContain("Classical.choice");
    expect(list.querySelector('[data-declaration="Main.native"]')!.textContent).toContain("Relies on compiled code");
    expect(screen.getByText("Runs natively with your account's privileges")).toBeTruthy();
    // Nothing selected by default: Accept is disabled.
    expect((screen.getByTestId("workflow-association-accept") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(within(list).getByRole("checkbox", { name: "Main.uses_choice" }));
    fireEvent.click(screen.getByTestId("workflow-association-accept"));
    await waitFor(() => expect(runIpc.decideAssociation).toHaveBeenCalledWith({ run: "run-1", step: "associate", accept: ["Main.uses_choice"] }));
    expect(onRun).toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("workflow-association-reject"));
    await waitFor(() => expect(runIpc.decideAssociation).toHaveBeenLastCalledWith({ run: "run-1", step: "associate", accept: [] }));
  });

  it("reviews a promotion proposal's exact change before approving it", async () => {
    runIpc.promotionPreview.mockResolvedValue({ path: "paper/main.typ", update: false });
    research.approvePromotion.mockResolvedValue({ path: "paper/main.typ", attachmentKey: "fig-1" });
    runIpc.get.mockResolvedValue(run());
    const promo = {
      port: "promotion",
      portType: "promotionProposal" as const,
      value: { kind: "promotionProposal" as const, promotion: "prom-1", document: "paper/main.typ", status: "pending" as const, attachmentKey: null, expiresAtMs: null },
      provenance: { run: "run-1", step: "run", attempt: 1, operation: { kind: "review", id: "x" } },
    };
    render(<RunMonitor run={run({ steps: [{ ...run().steps[0]!, attempts: [attempt({ outputs: [promo] })] }] })} onRun={vi.fn()} />);
    const approveMissing = screen.queryByTestId("workflow-promotion-approve");
    expect(approveMissing).toBeNull();
    fireEvent.click(screen.getByTestId("workflow-promotion-review"));
    await screen.findByTestId("promotion-preview");
    expect(runIpc.promotionPreview).toHaveBeenCalledWith("prom-1");
    fireEvent.click(screen.getByTestId("workflow-promotion-approve"));
    await waitFor(() => expect(research.approvePromotion).toHaveBeenCalledWith("prom-1"));
  });

  it("shows decided proposals as recorded: which declarations were associated, the inserted attachment, the review deadline", () => {
    const provenance = { run: "run-1", step: "associate", attempt: 1, operation: { kind: "review", id: "rv-1" } };
    const association = {
      port: "proposal",
      portType: "associationProposal" as const,
      value: { kind: "associationProposal" as const, proposal: "p-1", declarations: ["Main.thm", "Main.other"], status: "accepted" as const, accepted: ["Main.thm"], evidence: ["ev-1"] },
      provenance,
    };
    const inserted = {
      port: "promotion",
      portType: "promotionProposal" as const,
      value: { kind: "promotionProposal" as const, promotion: "prom-1", document: "paper/main.typ", status: "accepted" as const, attachmentKey: "fig-1", expiresAtMs: null },
      provenance,
    };
    const waiting = {
      ...inserted,
      value: { ...inserted.value, promotion: "prom-2", status: "pending" as const, attachmentKey: null, expiresAtMs: Date.UTC(2026, 10, 13, 12) },
    };
    render(<RunMonitor run={run({ steps: [{ ...run().steps[0]!, attempts: [attempt({ outputs: [association, inserted, waiting] })] }] })} onRun={vi.fn()} />);
    const list = screen.getByTestId("workflow-association-checklist");
    expect(list.querySelector('[data-declaration="Main.thm"]')!.getAttribute("data-associated")).toBe("true");
    expect(list.querySelector('[data-declaration="Main.thm"]')!.textContent).toContain("Associated");
    expect(list.querySelector('[data-declaration="Main.other"]')!.getAttribute("data-associated")).toBe("false");
    expect(list.querySelector('[data-declaration="Main.other"]')!.textContent).toContain("Not associated");
    expect(screen.queryByTestId("workflow-association-accept")).toBeNull();
    expect(screen.getByTestId("workflow-promotion-attachment").textContent).toContain("fig-1");
    expect(screen.getByTestId("workflow-promotion-expiry").textContent).toContain("can be reviewed until");
  });

  it("keeps the run view current through workflow-run events for that run only", async () => {
    runIpc.get.mockResolvedValueOnce(run({ state: "running", finishedAtMs: null })).mockResolvedValueOnce(run({ state: "succeeded" }));
    render(<WorkflowRunView resource={{ key: "workflow-run:run-1", title: "Run" }} mode="source" active />);
    await waitFor(() => expect(screen.getByTestId("workflow-run").getAttribute("data-state")).toBe("running"));
    await waitFor(() => expect(eventHandlers.length).toBeGreaterThan(0));
    act(() => eventHandlers.forEach((h) => h({ run: "other", workflow: "wf-1" })));
    expect(runIpc.get).toHaveBeenCalledTimes(1);
    act(() => eventHandlers.forEach((h) => h({ run: "run-1", workflow: "wf-1" })));
    await waitFor(() => expect(screen.getByTestId("workflow-run").getAttribute("data-state")).toBe("succeeded"));
  });

  it("lists runs newest first and opens one", async () => {
    runIpc.list.mockResolvedValue([
      { id: "old", workflow: "wf-1", revision: 1, title: "A", state: "failed", startedAtMs: 1, finishedAtMs: 2, stepsDone: 1, stepsTotal: 2, acknowledged: false },
      { id: "new", workflow: "wf-1", revision: 2, title: "A", state: "interrupted", startedAtMs: 5, finishedAtMs: null, stepsDone: 0, stepsTotal: 2, acknowledged: false },
    ]);
    render(<RunHistory workflow="wf-1" />);
    const list = await screen.findByTestId("workflow-runs");
    expect(runIpc.list).toHaveBeenCalledWith("wf-1");
    const ids = [...list.querySelectorAll("li")].map((li) => li.getAttribute("data-testid"));
    expect(ids).toEqual(["workflow-run-item-new", "workflow-run-item-old"]);
    expect(screen.getByTestId("workflow-run-item-new").textContent).toContain("Needs review");
    fireEvent.click(within(screen.getByTestId("workflow-run-item-new")).getByRole("button"));
    expect(editorsMock.open).toHaveBeenCalledWith({ key: "workflow-run:new", title: "Run: A" });
  });
});

// ----- canvas -----------------------------------------------------------------------------

const runner = { id: "r", title: "Runner", containment: "applicationService" as const, allows: [], available: true, availability: "Available" };
const contract = (c: Partial<OperationContract> & Pick<OperationContract, "id" | "inputs" | "outputs">): OperationContract => ({
  version: 1,
  title: c.id,
  purpose: "",
  parameters: [],
  payload: null,
  effects: ["readDeclaredInputs"],
  runner,
  limits: { maxSeconds: 1, maxOutputBytes: 1 },
  failure: "stopWorkflow",
  ...c,
});
const contracts: OperationContract[] = [
  contract({
    id: "lean.audit",
    inputs: [{ name: "source", label: "Lean file", types: ["formalSource"], required: true, multiple: false }],
    outputs: [{ name: "evidence", label: "Audited declarations", types: ["formalEvidence"], required: false, multiple: false }],
  }),
  contract({
    id: "formal.associateEvidence",
    inputs: [{ name: "evidence", label: "Audited declarations", types: ["formalEvidence"], required: true, multiple: false }],
    outputs: [{ name: "proposal", label: "Association proposal", types: ["associationProposal"], required: false, multiple: false }],
  }),
];
const def: WorkflowDefinition = {
  schema: "exposition.workflow/1",
  title: "Formal audit",
  description: "",
  preset: null,
  inputs: [
    { id: "leanFile", label: "Lean file", portType: "formalSource", source: { kind: "resource", path: "Proofs/Main.lean", version: { kind: "atRunStart" } } },
    { id: "notes", label: "Notes", portType: "document", source: { kind: "unset" } },
  ],
  steps: [
    { id: "audit", label: "Audit", operation: { id: "lean.audit", version: 1 }, parameters: {}, payload: null },
    { id: "associate", label: "Associate", operation: { id: "formal.associateEvidence", version: 1 }, parameters: {}, payload: null },
  ],
  connections: [{ kind: "data", from: { kind: "step", step: "audit", port: "evidence" }, to: { step: "associate", port: "evidence" } }],
  outcomes: [{ id: "evidence", kind: "auditEvidence", label: "Audit evidence", from: { step: "audit", port: "evidence" }, optional: false }],
  ceiling: { execution: "trustedNative", modelRequests: false, retainResults: true, proposeChanges: true },
};

describe("canvas connections", () => {
  const { connect } = canvasModel;
  it("accepts a compatible output → input connection as a data binding", () => {
    const result = connect(def, contracts, { source: "input:leanFile", sourceHandle: "out", target: "step:audit", targetHandle: "in:source" });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.draft.connections).toContainEqual({ kind: "data", from: { kind: "input", input: "leanFile" }, to: { step: "audit", port: "source" } });
    }
  });

  it("refuses an incompatible type with the reason and changes nothing", () => {
    const result = connect(def, contracts, { source: "input:notes", sourceHandle: "out", target: "step:audit", targetHandle: "in:source" });
    expect(result).toEqual({ ok: false, message: expect.stringMatching(/Refused: .*accepts Lean source, but “Notes” is Document/) });
  });

  it("refuses cycles, self-feeds, a second source on a single port, and an incompatible outcome", () => {
    const back = connect(def, contracts, { source: "step:associate", sourceHandle: "out:proposal", target: "step:audit", targetHandle: "in:source" });
    expect(back.ok).toBe(false);
    const prereqCycle = connect(def, contracts, { source: "step:associate", sourceHandle: "pre-out", target: "step:audit", targetHandle: "pre-in" });
    expect(prereqCycle).toEqual({ ok: false, message: expect.stringMatching(/cycle/) });
    const self = connect(def, contracts, { source: "step:audit", sourceHandle: "out:evidence", target: "step:audit", targetHandle: "in:source" });
    expect(self.ok).toBe(false);
    const bound = connect(
      { ...def, steps: [...def.steps, { id: "audit2", label: "Audit 2", operation: { id: "lean.audit", version: 1 }, parameters: {}, payload: null }] },
      contracts,
      { source: "step:audit2", sourceHandle: "out:evidence", target: "step:associate", targetHandle: "in:evidence" },
    );
    expect(bound).toEqual({ ok: false, message: expect.stringMatching(/takes one source and already has one/) });
    const outcome = connect(def, contracts, { source: "step:associate", sourceHandle: "out:proposal", target: "outcome:evidence", targetHandle: "in" });
    expect(outcome).toEqual({ ok: false, message: expect.stringMatching(/Audit evidence needs Formal evidence/) });
  });

  it("adds a prerequisite between steps and removes connections by id", () => {
    const two = { ...def, connections: [] };
    const pre = connect(two, contracts, { source: "step:audit", sourceHandle: "pre-out", target: "step:associate", targetHandle: "pre-in" });
    expect(pre.ok).toBe(true);
    if (pre.ok) {
      expect(pre.draft.connections).toEqual([{ kind: "prerequisite", before: "audit", after: "associate" }]);
      expect(canvasModel.removeEdge(pre.draft, "c0")!.connections).toEqual([]);
    }
    expect(canvasModel.removeEdge(def, "o:evidence")).toBeNull();
  });

  it("projects data (solid), prerequisite (dashed) and annotation (dotted, no data) edges, and lays out by dependency order", () => {
    const withAll: WorkflowDefinition = {
      ...def,
      connections: [
        ...def.connections,
        { kind: "prerequisite", before: "audit", after: "associate" },
        { kind: "annotation", from: "audit", to: "associate", label: "see notes", relation: null },
      ],
    };
    const edges = canvasModel.edgesOf(withAll, contracts, []);
    expect(edges.map((e) => [e.kind, e.label])).toEqual([
      ["data", "Formal evidence"],
      ["prerequisite", "runs before (no data)"],
      ["annotation", "annotation (no data): see notes"],
      ["outcome", "Formal evidence"],
    ]);
    const layout = canvasModel.autoLayout(withAll, contracts);
    expect(layout.get("input:leanFile")!.x).toBeLessThan(layout.get("step:audit")!.x);
    expect(layout.get("step:audit")!.x).toBeLessThan(layout.get("step:associate")!.x);
    expect(layout.get("step:associate")!.x).toBeLessThan(layout.get("outcome:evidence")!.x);
  });

  it("renders typed, labelled ports with diagnostics badges on the nodes and ports they name", async () => {
    const onChange = vi.fn();
    render(
      <WorkflowCanvas
        workflowId="wf-1"
        draft={def}
        contracts={contracts}
        diagnostics={[{ severity: "error", code: "workflow.unboundInput", message: "“Lean file” of step “Audit” is not bound", rule: 1, step: "audit", port: "source", input: null, outcome: null, connection: null }]}
        selection={null}
        onSelectionChange={vi.fn()}
        onChange={onChange}
      />,
    );
    const port = await screen.findByTestId("wfc-port-audit-in-source");
    expect(port.textContent).toContain("Lean file");
    expect(port.textContent).toContain("Lean source");
    expect(port.textContent).toContain("required");
    expect(port.textContent).toContain("1 problem");
    const handle = port.querySelector(".react-flow__handle")!;
    expect(handle.getAttribute("data-shape")).toBe("square");
    expect(handle.getAttribute("aria-label")).toBe("Input Lean file (Lean source), required");
    expect(screen.getByTestId("wfc-outcome-evidence")).toBeTruthy();
    expect(screen.getByTestId("wfc-input-notes").textContent).toContain("No file chosen");
  });
});
