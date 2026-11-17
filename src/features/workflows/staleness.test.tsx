// Open workflow views stay current with state that changes elsewhere
// (stabilization ST, lane st/state). Each test failed before its fix:
// - XR-K-02: a run tab across Close Folder / switching / reopening the project;
// - E16: the mounted Workflows list's Recent runs across a project switch;
// - XR-K-03: an open editor's review eligibility and runner text after the
//   restricted image (or trust) changes, without a save;
// - XR-E-02: an open composite parent discovers a child revision saved in
//   another tab, and the pin moves only on request;
// - XR-E-03: the mounted Workflows list shows a draft its own New Draft
//   created, and a revision saved in an editor.
// IPC is mocked at the feature's boundary.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { OperationContract } from "../../bindings/OperationContract";
import type { WorkflowSummary } from "../../bindings/WorkflowSummary";
import type { WorkflowValidation } from "../../bindings/WorkflowValidation";
import type { SavedWorkflow, WorkflowDefinition, WorkflowRun, WorkflowRunPlan, WorkflowRunSummary } from "./contract";

const wfIpc = vi.hoisted(() => ({
  operations: vi.fn(),
  presets: vi.fn(),
  fromPreset: vi.fn(),
  validate: vi.fn(),
  save: vi.fn(),
  import: vi.fn(),
  list: vi.fn(),
  get: vi.fn(),
  bundleExport: vi.fn(),
  bundleImport: vi.fn(),
}));
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
  exportManifest: vi.fn(),
}));
const triggerIpc = vi.hoisted(() => ({ list: vi.fn(async () => []), plan: vi.fn(), enable: vi.fn(), disable: vi.fn() }));
const runHandlers = vi.hoisted(() => [] as ((e: { run: string; workflow: string }) => void)[]);
const trustHandlers = vi.hoisted(() => [] as ((change: unknown) => void)[]);
const editorsMock = vi.hoisted(() => ({
  open: vi.fn(),
  close: vi.fn(),
  setDirty: vi.fn(),
  setTitle: vi.fn(),
  registerCloseGuard: vi.fn(() => () => {}),
  active: vi.fn(() => null),
}));
vi.mock("./ipc", () => ({
  workflowCommands: wfIpc,
  workflowRunCommands: runIpc,
  workflowTriggerCommands: triggerIpc,
  workflowEvents: {
    run: vi.fn(async (handler: (e: { run: string; workflow: string }) => void) => {
      runHandlers.push(handler);
      return () => {};
    }),
    trigger: vi.fn(async () => () => {}),
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("../research/ipc", () => ({ researchCommands: {} }));
vi.mock("../research/Promotion", () => ({ PreviewView: () => null }));
vi.mock("./CodePayloadEditor", () => ({ CodePayloadEditor: () => null }));
vi.mock("../../app/editors", () => ({ editors: editorsMock }));
vi.mock("../../app/notifications", () => ({ notify: vi.fn(), dismiss: vi.fn() }));
vi.mock("../workspace/store", async () => {
  const { create } = await import("zustand");
  return { useWorkspace: create(() => ({ info: { root: "/work/a", name: "a", recovery: [] } as { root: string; name: string; recovery: never[] } | null })) };
});
vi.mock("../settings/store", async () => {
  const { create } = await import("zustand");
  return { useSettings: create(() => ({ execution: null as unknown })) };
});
const graphHandlers = vi.hoisted(() => [] as ((event: unknown) => void)[]);
vi.mock("../../lib/graphIpc", () => ({
  graphCommands: {},
  graphEvents: {
    graph: vi.fn(async (handler: (event: unknown) => void) => {
      graphHandlers.push(handler);
      return () => {};
    }),
    graphResync: vi.fn(async () => () => {}),
  },
}));
vi.mock("../../lib/ipc", () => ({
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
  isCommandError: (e: unknown) => typeof e === "object" && e !== null && "code" in e && "message" in e,
  commands: {},
  events: {
    trustChanged: vi.fn(async (handler: (change: unknown) => void) => {
      trustHandlers.push(handler);
      return () => {};
    }),
  },
}));

const { WorkflowRunView } = await import("./RunView");
const { WorkflowEditor } = await import("./WorkflowEditor");
const { WorkflowsView } = await import("./WorkflowsView");
const { useWorkspace } = await import("../workspace/store");
const { useSettings } = await import("../settings/store");

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

/** Execution settings as the settings store holds them (only the images matter here). */
const executionWith = (docker: object) => ({ settings: { docker }, warning: null }) as never;

const openProject = (root: string | null) =>
  act(() => useWorkspace.setState({ info: root ? { root, name: root.split("/").pop()!, recovery: [] } : null }));

beforeEach(() => {
  vi.clearAllMocks();
  runHandlers.length = 0;
  localStorage.clear();
  useWorkspace.setState({ info: { root: "/work/a", name: "a", recovery: [] } });
  triggerIpc.list.mockResolvedValue([]);
  wfIpc.presets.mockResolvedValue([]);
  wfIpc.list.mockResolvedValue([]);
  runIpc.list.mockResolvedValue([]);
});
afterEach(() => cleanup());

// ----- fixtures ----------------------------------------------------------------------------

const plan = (): WorkflowRunPlan => ({
  plan: "plan-1",
  workflow: "wf-1",
  revision: 2,
  definitionHash: "d".repeat(64),
  title: "Slow experiment",
  project: "a",
  inputs: [],
  steps: [],
  ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  budgets: { maxSteps: 10, maxTotalSeconds: 900, maxOutputBytes: 1000, maxModelRequests: 0 },
  maxParallel: 1,
  limits: { maxActiveRuns: 3, maxInFlightSteps: 4, restricted: 2, native: 2, agent: 2, lean: 1 },
  execution: { restrictedBackend: "docker", restrictedImage: "python:3.13-slim", restrictedImageId: null, trustedFolder: false, nativeSteps: [], modelSteps: [] },
  notices: [],
  blockers: [],
  approvalHash: "f".repeat(64),
  createdAtMs: 1_700_000_000_000,
  expiresAtMs: 1_700_001_800_000,
});

const run = (state: WorkflowRun["state"]): WorkflowRun => ({
  id: "run-1",
  workflow: "wf-1",
  revision: 2,
  definitionHash: "d".repeat(64),
  title: "Slow experiment",
  state,
  cause: { kind: "manual" },
  code: state === "canceled" ? "workflow.projectClosed" : null,
  message: state === "canceled" ? "The project was closed." : null,
  startedAtMs: 1_700_000_000_000,
  finishedAtMs: state === "running" ? null : 1_700_000_002_000,
  budgets: { maxSteps: 10, maxTotalSeconds: 900, maxOutputBytes: 1000, maxModelRequests: 0 },
  usage: { steps: 1, seconds: 2, outputBytes: 0, modelRequests: 0 },
  steps: [
    {
      step: "run",
      label: "Run the code",
      operationTitle: "Python batch",
      runnerTitle: "Restricted Python batch",
      containment: "restrictedContainer",
      state: state === "running" ? "running" : "canceled",
      attempts: [],
      instances: [],
      reason: null,
    },
  ],
  outcomes: [],
  acknowledged: false,
  plan: plan(),
});

const noProject = { code: "workflow.noProject", message: "No project is open." };
const unknownRun = { code: "workflow.unknownRun", message: "No such run in the open project." };

const runSummary = (id: string, title: string): WorkflowRunSummary => ({
  id,
  workflow: "wf-1",
  revision: 1,
  title,
  state: "succeeded",
  cause: { kind: "manual" },
  startedAtMs: 1_700_000_000_000,
  finishedAtMs: 1_700_000_001_000,
  stepsDone: 1,
  stepsTotal: 1,
  acknowledged: false,
});

const summary = (id: string, title: string, revision = 1): WorkflowSummary => ({
  id,
  revision,
  title,
  preset: null,
  state: "wellFormed",
  runnable: false,
  errors: 0,
  warnings: 0,
  entity: null,
  origin: { kind: "user" },
  imported: false,
  savedAtMs: 0,
});

const restricted = (available: boolean, id = "research.pythonBatch", reason = "Restricted execution is unavailable: no container image is configured. It never falls back to native execution."): OperationContract => ({
  id,
  version: 1,
  title: id === "research.rBatch" ? "R batch" : "Python batch",
  purpose: "Runs code",
  inputs: [{ name: "data", label: "Data", types: ["dataset"], required: false, multiple: false }],
  outputs: [{ name: "dataset", label: "Dataset", types: ["dataset"], required: false, multiple: false }],
  parameters: [],
  payload: null,
  effects: ["readDeclaredInputs", "executeRestricted"],
  runner: {
    id: id === "research.rBatch" ? "restricted-r" : "restricted-python",
    title: "Restricted batch",
    containment: "restrictedContainer",
    allows: [],
    available,
    availability: available ? "Available" : reason,
  },
  limits: { maxSeconds: 60, maxOutputBytes: 1000 },
  failure: "stopWorkflow",
});

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

const definition = (title: string, operation = "research.pythonBatch"): WorkflowDefinition => ({
  schema: "exposition.workflow/1",
  title,
  description: "",
  preset: null,
  inputs: [],
  steps: [{ id: "run", label: "Run the code", operation: { id: operation, version: 1 }, parameters: {}, payload: null }],
  connections: [],
  outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
  ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
});

const saved = (id: string, revision: number, def: WorkflowDefinition, latest = revision, v: WorkflowValidation = validation()): SavedWorkflow => ({
  id,
  revision,
  hash: "a".repeat(64),
  entity: null,
  definition: def,
  validation: v,
  origin: { kind: "user" },
  imported: false,
  savedAtMs: 0,
  unchanged: false,
  revisions: Array.from({ length: latest }, (_, i) => ({ revision: latest - i, hash: "b".repeat(64), state: "wellFormed" as const, savedAtMs: 0 })),
  graphRevision: 1,
});

// ----- XR-K-02 -----------------------------------------------------------------------------

describe("XR-K-02: a run tab when its project closes, switches, and reopens", () => {
  const openRunTab = () => render(<WorkflowRunView resource={{ key: "workflow-run:run-1", title: "Run" }} mode="source" active />);

  it("never offers Cancel run for a run whose project is closed, and shows the final state after reopening without closing the tab", async () => {
    runIpc.get.mockResolvedValue(run("running"));
    openRunTab();
    await waitFor(() => expect(screen.getByTestId("workflow-run").getAttribute("data-state")).toBe("running"));
    expect(screen.getByTestId("workflow-run-cancel")).toBeTruthy();

    // Close Folder: the backend cancels the run; reads need the open project.
    runIpc.get.mockRejectedValue(noProject);
    openProject(null);
    // The terminal event arrives after the project closed; its re-read fails.
    act(() => runHandlers.forEach((h) => h({ run: "run-1", workflow: "wf-1" })));
    await waitFor(() => expect(screen.queryByTestId("workflow-run-cancel")).toBeNull());
    expect(screen.queryByText("Running now")).toBeNull();
    const detached = screen.getByTestId("workflow-run-detached");
    expect(detached.textContent).toContain("This run's project is not open");
    expect(detached.textContent).toContain("/work/a");

    // Reopen the same project: the tab re-reads the run by itself.
    runIpc.get.mockResolvedValue(run("canceled"));
    openProject("/work/a");
    await waitFor(() => expect(screen.getByTestId("workflow-run").getAttribute("data-state")).toBe("canceled"));
    expect(screen.queryByTestId("workflow-run-cancel")).toBeNull();
    expect(screen.getByText("This run was canceled")).toBeTruthy();
  });

  it("is unavailable while another project is open, and current again when its project reopens", async () => {
    runIpc.get.mockResolvedValue(run("running"));
    openRunTab();
    await waitFor(() => expect(screen.getByTestId("workflow-run").getAttribute("data-state")).toBe("running"));
    runIpc.get.mockRejectedValue(unknownRun);
    openProject("/work/b");
    await waitFor(() => expect(screen.queryByTestId("workflow-run-cancel")).toBeNull());
    expect(screen.getByTestId("workflow-run-detached")).toBeTruthy();
    runIpc.get.mockResolvedValue(run("canceled"));
    openProject("/work/a");
    await waitFor(() => expect(screen.getByTestId("workflow-run").getAttribute("data-state")).toBe("canceled"));
  });

  it("says when an open run could not be re-read, instead of silently showing the old state", async () => {
    runIpc.get.mockResolvedValueOnce(run("running")).mockRejectedValueOnce({ code: "internal.task", message: "Workflow task failed: busy" });
    openRunTab();
    await waitFor(() => expect(screen.getByTestId("workflow-run").getAttribute("data-state")).toBe("running"));
    act(() => runHandlers.forEach((h) => h({ run: "run-1", workflow: "wf-1" })));
    const banner = await screen.findByTestId("workflow-run-refresh-error");
    expect(banner.textContent).toContain("Workflow task failed: busy");
    runIpc.get.mockResolvedValue(run("canceled"));
    fireEvent.click(within(banner).getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.getByTestId("workflow-run").getAttribute("data-state")).toBe("canceled"));
    expect(screen.queryByTestId("workflow-run-refresh-error")).toBeNull();
  });
});

// ----- E16 ---------------------------------------------------------------------------------

describe("E16: the mounted Workflows list after a project switch", () => {
  it("shows the new project's runs, never the previous project's", async () => {
    wfIpc.list.mockResolvedValue([summary("wf-1", "Source workflow")]);
    runIpc.list.mockResolvedValue([runSummary("run-a1", "Source workflow"), runSummary("run-a2", "Source workflow")]);
    render(<WorkflowsView resource={{ key: "workflows:", title: "Workflows" }} mode="source" active />);
    await waitFor(() => expect(screen.getByTestId("workflow-runs").querySelectorAll("[role='listitem']")).toHaveLength(2));

    wfIpc.list.mockResolvedValue([summary("wf-2", "Destination workflow")]);
    runIpc.list.mockResolvedValue([]);
    openProject("/work/b");
    await waitFor(() => expect(screen.getByTestId("workflow-drafts").textContent).toContain("Destination workflow"));
    await waitFor(() => expect(screen.queryByTestId("workflow-runs")).toBeNull());
    expect(screen.getByText("No runs yet")).toBeTruthy();
  });
});

// ----- XR-K-03 -----------------------------------------------------------------------------

describe("XR-K-03: an open workflow editor follows runner availability", () => {
  const configureImage = (image: string | null) =>
    act(() => useSettings.setState({ execution: executionWith({ image }) }));

  it("enables Review and run and updates the step's runner text when the image is configured, without a save", async () => {
    const def = definition("Experiment");
    const blocked = validation({ runnable: false, runnableReason: "Run the code: Restricted execution is unavailable: no container image is configured." });
    wfIpc.get.mockResolvedValue(saved("wf-1", 2, def, 2, blocked));
    wfIpc.operations.mockResolvedValue([restricted(false)]);
    wfIpc.validate.mockResolvedValue(blocked);
    render(<WorkflowEditor resource={{ key: "workflow:wf-1", title: "Experiment" }} mode="source" active />);
    const step = await screen.findByTestId("workflow-step-run");
    expect(step.textContent).toContain("unavailable: Restricted execution is unavailable: no container image is configured");
    await waitFor(() => expect(wfIpc.validate).toHaveBeenCalled());
    expect(screen.getByTestId("workflow-status").getAttribute("data-can-review")).toBe("false");

    // Settings › Execution: the image is set (the backend now reports the runner available).
    wfIpc.operations.mockResolvedValue([restricted(true)]);
    wfIpc.validate.mockResolvedValue(validation());
    configureImage("python:3.13-slim");
    await waitFor(() => expect(screen.getByTestId("workflow-status").getAttribute("data-can-review")).toBe("true"));
    await waitFor(() => expect(screen.getByTestId("workflow-step-run").textContent).toContain("Restricted batch (Restricted container) — available."));
    expect(screen.getByTestId("workflow-step-run").textContent).not.toContain("no container image");
    expect((screen.getByTestId("workflow-review-run") as HTMLButtonElement).disabled).toBe(false);
    // Nothing was saved: the revision is unchanged.
    expect(wfIpc.save).not.toHaveBeenCalled();
    expect(screen.getByTestId("workflow-editor").getAttribute("data-revision")).toBe("2");

    // Clearing the image blocks it again.
    wfIpc.operations.mockResolvedValue([restricted(false)]);
    wfIpc.validate.mockResolvedValue(blocked);
    configureImage(null);
    await waitFor(() => expect(screen.getByTestId("workflow-status").getAttribute("data-can-review")).toBe("false"));
    await waitFor(() => expect(screen.getByTestId("workflow-step-run").textContent).toContain("no container image is configured"));
  });

  it("follows a language image (R) and a trust change the same way", async () => {
    const def = definition("R experiment", "research.rBatch");
    const reason = "Restricted execution is unavailable: no R image is configured. It never falls back to native execution.";
    const blocked = validation({ runnable: false, runnableReason: reason });
    wfIpc.get.mockResolvedValue(saved("wf-r", 1, def, 1, blocked));
    wfIpc.operations.mockResolvedValue([restricted(false, "research.rBatch", reason)]);
    wfIpc.validate.mockResolvedValue(blocked);
    render(<WorkflowEditor resource={{ key: "workflow:wf-r", title: "R experiment" }} mode="source" active />);
    await waitFor(() => expect(screen.getByTestId("workflow-step-run").textContent).toContain("no R image is configured"));
    wfIpc.operations.mockResolvedValue([restricted(true, "research.rBatch")]);
    wfIpc.validate.mockResolvedValue(validation());
    act(() => useSettings.setState({ execution: executionWith({ image: null, rImage: "r-base:4.6.2" }) }));
    await waitFor(() => expect(screen.getByTestId("workflow-status").getAttribute("data-can-review")).toBe("true"));
    await waitFor(() => expect(screen.getByTestId("workflow-step-run").textContent).toContain("— available."));

    // A trust change (granted or revoked anywhere) re-reads availability too.
    wfIpc.operations.mockResolvedValue([restricted(false, "research.rBatch", reason)]);
    wfIpc.validate.mockResolvedValue(blocked);
    await waitFor(() => expect(trustHandlers.length).toBeGreaterThan(0));
    act(() => trustHandlers.forEach((h) => h({ root: "/work/a", granted: false })));
    await waitFor(() => expect(screen.getByTestId("workflow-status").getAttribute("data-can-review")).toBe("false"));
  });
});

describe("D1 (verifier): an open workflow and a folder trusted elsewhere", () => {
  it("becomes reviewable when the folder is trusted from another surface (the Lean panel), without a save", async () => {
    const untrusted =
      "The project folder is not trusted. This step runs natively with your account's privileges (not a sandbox); trust the folder first.";
    const audit: OperationContract = {
      ...restricted(false, "lean.audit", untrusted),
      title: "Lean audit",
      runner: { id: "lean", title: "Lean", containment: "nativeAccount", allows: [], available: false, availability: untrusted },
    };
    const def = definition("Formal audit", "lean.audit");
    const blocked = validation({ runnable: false, runnableReason: `Not runnable now: step “Run the code” (Lean): ${untrusted}` });
    wfIpc.get.mockResolvedValue(saved("wf-audit", 1, def, 1, blocked));
    wfIpc.operations.mockResolvedValue([audit]);
    wfIpc.validate.mockResolvedValue(blocked);
    render(<WorkflowEditor resource={{ key: "workflow:wf-audit", title: "Formal audit" }} mode="source" active />);
    await waitFor(() => expect(screen.getByTestId("workflow-step-run").textContent).toContain("The project folder is not trusted"));
    await waitFor(() => expect(wfIpc.validate).toHaveBeenCalled());
    expect(screen.getByTestId("workflow-status").getAttribute("data-can-review")).toBe("false");

    // "Trust workspace…" in the Lean panel: the backend announces trust:changed.
    wfIpc.operations.mockResolvedValue([{ ...audit, runner: { ...audit.runner, available: true, availability: "Available" } }]);
    wfIpc.validate.mockResolvedValue(validation());
    act(() => trustHandlers.forEach((h) => h({ root: "/work/a", granted: true })));
    await waitFor(() => expect(screen.getByTestId("workflow-status").getAttribute("data-can-review")).toBe("true"));
    await waitFor(() => expect(screen.getByTestId("workflow-step-run").textContent).toContain("— available."));
    expect(wfIpc.save).not.toHaveBeenCalled();
  });
});

// ----- XR-E-02 -----------------------------------------------------------------------------

describe("XR-E-02: an open composite parent and a child saved in another tab", () => {
  it("offers Use latest revision without reopening, and changes the pin only when chosen", async () => {
    const child = definition("Child");
    const parentDef: WorkflowDefinition = {
      ...definition("Parent"),
      schema: "exposition.workflow/2",
      steps: [{ id: "comp", label: "Child", operation: { id: "workflow.composite", version: 1 }, parameters: {}, payload: null, composite: { workflow: "wf-child", revision: 1 } }],
      outcomes: [],
    };
    let childLatest = 1;
    wfIpc.get.mockImplementation(async (id: string, revision?: number | null) => {
      if (id === "wf-child") return saved("wf-child", revision ?? childLatest, child, childLatest);
      return saved("wf-parent", 2, parentDef);
    });
    wfIpc.operations.mockResolvedValue([
      restricted(true),
      { ...restricted(true, "workflow.composite"), id: "workflow.composite", inputs: [], outputs: [], runner: { id: "composite", title: "Composite", containment: "applicationService", allows: [], available: true, availability: "Available" } },
    ]);
    wfIpc.validate.mockResolvedValue(validation());
    render(
      <>
        <div data-testid="child-tab">
          <WorkflowEditor resource={{ key: "workflow:wf-child", title: "Child" }} mode="source" active />
        </div>
        <div data-testid="parent-tab">
          <WorkflowEditor resource={{ key: "workflow:wf-parent", title: "Parent" }} mode="source" active={false} />
        </div>
      </>,
    );
    const parentTab = screen.getByTestId("parent-tab");
    await waitFor(() => expect(within(parentTab).getByTestId("composite-revision-comp").textContent).toBe("1"));
    expect(within(parentTab).queryByTestId("composite-use-latest-comp")).toBeNull();

    // Save revision 2 of the child in its own tab.
    const childTab = screen.getByTestId("child-tab");
    await within(childTab).findByTestId("workflow-editor");
    fireEvent.change(within(childTab).getByLabelText("Description"), { target: { value: "Now with a description" } });
    childLatest = 2;
    wfIpc.save.mockResolvedValue(saved("wf-child", 2, { ...child, description: "Now with a description" }, 2));
    fireEvent.click(within(childTab).getByTestId("workflow-save"));
    await waitFor(() => expect(wfIpc.save).toHaveBeenCalledTimes(1));

    // The open parent discovers it; the pin stays at 1 and nothing is dirty.
    const useLatest = await within(parentTab).findByTestId("composite-use-latest-comp", undefined, { timeout: 3000 });
    expect(useLatest.textContent).toContain("Use latest revision (2)");
    expect(within(parentTab).getByTestId("composite-revision-comp").textContent).toBe("1");
    expect(editorsMock.setDirty).not.toHaveBeenCalledWith("workflow:wf-parent", true);
    fireEvent.click(useLatest);
    await waitFor(() => expect(within(parentTab).getByTestId("composite-revision-comp").textContent).toBe("2"));
    await waitFor(() => expect(editorsMock.setDirty).toHaveBeenLastCalledWith("workflow:wf-parent", true));
    expect(wfIpc.save).toHaveBeenCalledTimes(1);
  });
});

// ----- XR-E-03 -----------------------------------------------------------------------------

describe("XR-E-03: the mounted Workflows list and drafts created or saved elsewhere", () => {
  it("lists a draft made with its own New Draft, and a later revision saved in the editor", async () => {
    wfIpc.list.mockResolvedValue([summary("wf-child", "Child")]);
    wfIpc.presets.mockResolvedValue([{ id: "numericalExperiment", title: "Numerical experiment", description: "A script over data." }]);
    render(<WorkflowsView resource={{ key: "workflows:", title: "Workflows" }} mode="source" active />);
    await waitFor(() => expect(screen.getByTestId("workflow-drafts").textContent).toContain("Child"));
    await waitFor(() => expect((screen.getByTestId("workflow-new") as HTMLButtonElement).disabled).toBe(false));

    const created = definition("Numerical experiment");
    wfIpc.fromPreset.mockResolvedValue(created);
    wfIpc.save.mockResolvedValue(saved("wf-new", 1, created));
    wfIpc.list.mockResolvedValue([summary("wf-new", "Numerical experiment"), summary("wf-child", "Child")]);
    fireEvent.click(screen.getByTestId("workflow-new"));
    fireEvent.click(await screen.findByTestId("workflow-new-numericalExperiment"));
    await waitFor(() => expect(editorsMock.open).toHaveBeenCalledWith({ key: "workflow:wf-new", title: "Numerical experiment" }));
    await waitFor(() => expect(screen.getByTestId("workflow-drafts").querySelector('[data-workflow="wf-new"]')).toBeTruthy());

    // Renamed and saved in its editor (another tab): the list shows the new title and revision.
    wfIpc.get.mockResolvedValue(saved("wf-new", 1, created));
    wfIpc.operations.mockResolvedValue([restricted(true)]);
    wfIpc.validate.mockResolvedValue(validation());
    render(<WorkflowEditor resource={{ key: "workflow:wf-new", title: "Numerical experiment" }} mode="source" active />);
    const editor = await screen.findByTestId("workflow-editor");
    fireEvent.change(within(editor).getByLabelText("Title"), { target: { value: "XR E09 parent" } });
    wfIpc.save.mockResolvedValue(saved("wf-new", 2, { ...created, title: "XR E09 parent" }, 2));
    wfIpc.list.mockResolvedValue([summary("wf-new", "XR E09 parent", 2), summary("wf-child", "Child")]);
    fireEvent.click(within(editor).getByTestId("workflow-save"));
    await waitFor(() => {
      const row = screen.getByTestId("workflow-drafts").querySelector('[data-workflow="wf-new"]');
      expect(row?.textContent).toContain("XR E09 parent");
      expect(row?.textContent).toContain("Revision 2");
    });
  });

  it("lists a workflow an agent task saved (a graph change), but ignores file extraction", async () => {
    wfIpc.list.mockResolvedValue([summary("wf-child", "Child")]);
    render(<WorkflowsView resource={{ key: "workflows:", title: "Workflows" }} mode="source" active />);
    await waitFor(() => expect(screen.getByTestId("workflow-drafts").textContent).toContain("Child"));
    await waitFor(() => expect(graphHandlers.length).toBeGreaterThan(0));
    const delta = { revision: 9, entitiesAdded: ["e-1"], entitiesUpdated: [], entitiesRemoved: [], relationsAdded: [], relationsUpdated: [], relationsRemoved: [], truncated: false };
    const listed = wfIpc.list.mock.calls.length;
    act(() => graphHandlers.forEach((h) => h({ kind: "changed", delta, origin: { kind: "extracted", extractor: "typst" } })));
    await new Promise((r) => setTimeout(r, 300));
    expect(wfIpc.list.mock.calls.length).toBe(listed);
    wfIpc.list.mockResolvedValue([{ ...summary("wf-agent", "Agent proposal"), origin: { kind: "agent", taskId: "task-1" } }, summary("wf-child", "Child")]);
    act(() => graphHandlers.forEach((h) => h({ kind: "changed", delta, origin: { kind: "agent", taskId: "task-1" } })));
    await waitFor(() => expect(screen.getByTestId("workflow-drafts").querySelector('[data-workflow="wf-agent"]')).toBeTruthy());
  });
});
