// Workflow drafts: compatible-only binding choices and structural edits, and
// the draft editor (live Rust validation mocked at the feature's IPC
// boundary): never runnable, diagnostics shown, saving creates a revision.
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OperationContract } from "../../bindings/OperationContract";
import type { SavedWorkflow } from "../../bindings/SavedWorkflow";
import type { WorkflowDefinition } from "../../bindings/WorkflowDefinition";
import type { WorkflowValidation } from "../../bindings/WorkflowValidation";

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
const editorsMock = vi.hoisted(() => ({
  open: vi.fn(),
  setDirty: vi.fn(),
  setTitle: vi.fn(),
  registerCloseGuard: vi.fn(() => () => {}),
  active: vi.fn(() => ({ resource: { key: "file:Proofs/Main.lean", path: "Proofs/Main.lean" } })),
}));
const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));

vi.mock("./ipc", () => ({ workflowCommands: wfIpc }));
vi.mock("./CodePayloadEditor", () => ({
  CodePayloadEditor: ({ value, onChange }: { value: string; onChange: (v: string) => void }) => (
    <textarea data-testid="workflow-code" value={value} onChange={(e) => onChange(e.target.value)} />
  ),
}));
vi.mock("../../app/editors", () => ({ editors: editorsMock }));
vi.mock("../../app/notifications", () => notifications);
vi.mock("../../lib/ipc", () => ({
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
}));

const model = await import("./model");
const actions = await import("./actions");
const { WorkflowEditor } = await import("./WorkflowEditor");

const runner = { id: "r", title: "Runner", containment: "applicationService" as const, allows: [], available: false, availability: "Workflow execution arrives in a later version of Exposition." };
const contracts: OperationContract[] = [
  {
    id: "lean.audit",
    version: 1,
    title: "Lean audit",
    purpose: "Audit",
    inputs: [{ name: "source", label: "Lean file", types: ["formalSource"], required: true, multiple: false }],
    outputs: [{ name: "evidence", label: "Audited declarations", types: ["formalEvidence"], required: false, multiple: false }],
    parameters: [],
    payload: null,
    effects: ["readDeclaredInputs", "executeNative", "registerFormalEvidence"],
    runner,
    limits: { maxSeconds: 1, maxOutputBytes: 1 },
    failure: "stopWorkflow",
  },
  {
    id: "formal.associateEvidence",
    version: 1,
    title: "Associate audited declarations",
    purpose: "Associate",
    inputs: [{ name: "evidence", label: "Audited declarations", types: ["formalEvidence"], required: true, multiple: false }],
    outputs: [{ name: "proposal", label: "Association proposal", types: ["associationProposal"], required: false, multiple: false }],
    parameters: [],
    payload: null,
    effects: ["readDeclaredInputs", "proposeGraphChange"],
    runner,
    limits: { maxSeconds: 1, maxOutputBytes: 1 },
    failure: "stopWorkflow",
  },
  {
    id: "agent.review",
    version: 1,
    title: "Read-only agent summary or review",
    purpose: "Review",
    inputs: [{ name: "material", label: "Material", types: ["formalEvidence", "dataset", "document"], required: true, multiple: true }],
    outputs: [{ name: "report", label: "Review report", types: ["reviewReport"], required: false, multiple: false }],
    parameters: [],
    payload: { kind: "prompt" },
    effects: ["readDeclaredInputs", "sendToModel"],
    runner,
    limits: { maxSeconds: 1, maxOutputBytes: 1 },
    failure: "stopWorkflow",
  },
];

const audit: WorkflowDefinition = {
  schema: "exposition.workflow/1",
  title: "Formal audit",
  description: "",
  preset: "formalAudit",
  inputs: [
    { id: "leanFile", label: "Lean file", portType: "formalSource", source: { kind: "resource", path: "Proofs/Main.lean", version: { kind: "atRunStart" } } },
    { id: "notes", label: "Notes", portType: "document", source: { kind: "unset" } },
  ],
  steps: [
    { id: "audit", label: "Audit", operation: { id: "lean.audit", version: 1 }, parameters: {}, payload: null },
    { id: "associate", label: "Associate", operation: { id: "formal.associateEvidence", version: 1 }, parameters: {}, payload: null },
  ],
  connections: [
    { kind: "data", from: { kind: "input", input: "leanFile" }, to: { step: "audit", port: "source" } },
    { kind: "data", from: { kind: "step", step: "audit", port: "evidence" }, to: { step: "associate", port: "evidence" } },
  ],
  outcomes: [{ id: "evidence", kind: "auditEvidence", label: "Audit evidence", from: { step: "audit", port: "evidence" }, optional: false }],
  ceiling: { execution: "trustedNative", modelRequests: false, retainResults: true, proposeChanges: true },
};

const validation = (errors: number): WorkflowValidation => ({
  state: errors ? "incomplete" : "wellFormed",
  runnable: false,
  runnableReason: "Workflow execution arrives in a later version of Exposition; this draft is saved but cannot run yet.",
  diagnostics: errors
    ? [{ severity: "error", code: "workflow.unboundInput", message: "“Audited declarations” of step “Associate” is not bound", rule: 1, step: "associate", port: "evidence", input: null, outcome: null, connection: null }]
    : [],
  errors,
  warnings: 0,
  effects: [],
  plan: [{ step: "audit", label: "Audit", operation: "Lean audit (v1)", runner: "Runner", effects: ["executeNative"] }],
});

const saved = (revision: number, def: WorkflowDefinition, errors = 0): SavedWorkflow => ({
  id: "wf-1",
  revision,
  hash: "a".repeat(64),
  entity: "ent-1",
  definition: def,
  validation: validation(errors),
  origin: { kind: "user" },
  imported: false,
  savedAtMs: 0,
  unchanged: false,
  revisions: [],
  graphRevision: 1,
});

beforeEach(() => {
  vi.clearAllMocks();
  wfIpc.operations.mockResolvedValue(contracts);
  wfIpc.get.mockResolvedValue(saved(1, audit));
  wfIpc.validate.mockResolvedValue(validation(0));
});

afterEach(() => cleanup());

describe("model", () => {
  it("offers only type-compatible sources and nothing that would form a cycle", () => {
    const sources = model.compatibleSources(audit, contracts, "associate", "evidence").map((s) => s.key);
    expect(sources).toEqual(["step:audit.evidence"]);
    // The audit's own input accepts a Lean source only: the document input is not offered.
    expect(model.compatibleSources(audit, contracts, "audit", "source").map((s) => s.key)).toEqual(["input:leanFile"]);
    // `associate` is downstream of `audit`, so it is never offered to `audit`.
    const withReview = model.addStep(audit, contracts[2]!);
    const review = withReview.steps[2]!;
    expect(review.payload).toEqual({ kind: "prompt", text: "" });
    const options = model.compatibleSources(withReview, contracts, review.id, "material").map((s) => s.key);
    expect(options).toEqual(["input:notes", "step:audit.evidence"]);
  });

  it("rebinds, removes steps with their connections and outcomes, and finds outcome sources", () => {
    const unbound = model.setBindings(audit, "associate", "evidence", []);
    expect(model.bindingsInto(unbound, "associate", "evidence")).toEqual([]);
    const removed = model.removeStep(audit, "audit");
    expect(removed.steps.map((s) => s.id)).toEqual(["associate"]);
    expect(removed.connections).toEqual([]);
    expect(removed.outcomes).toEqual([]);
    expect(model.outcomeCandidates(audit, contracts, "auditEvidence").map((c) => c.key)).toEqual(["audit.evidence"]);
    expect(model.outcomeCandidates(audit, contracts, "retainedFigure")).toEqual([]);
    const pre = model.setPrerequisites(audit, "associate", ["audit"]);
    expect(model.prerequisitesOf(pre, "associate")).toEqual(["audit"]);
  });

  it("binds a preset draft to the active file when its type fits", () => {
    expect(actions.presetRequestFor("formalAudit", "Proofs/Main.lean")).toMatchObject({ leanFile: "Proofs/Main.lean", script: null });
    expect(actions.presetRequestFor("numericalExperiment", "sim.py")).toMatchObject({ script: "sim.py", leanFile: null });
    expect(actions.presetRequestFor("numericalExperiment", "main.typ")).toMatchObject({ manuscript: "main.typ", script: null });
  });
});

describe("draft actions", () => {
  it("saves a preset draft (incomplete is fine) and opens it", async () => {
    wfIpc.fromPreset.mockResolvedValue(audit);
    wfIpc.save.mockResolvedValue(saved(1, audit, 1));
    expect(await actions.newDraftFromPreset("formalAudit")).toBe("wf-1");
    expect(wfIpc.fromPreset).toHaveBeenCalledWith(expect.objectContaining({ preset: "formalAudit", leanFile: "Proofs/Main.lean" }));
    expect(wfIpc.save).toHaveBeenCalledWith({ id: null, expectedRevision: null, definition: audit });
    expect(editorsMock.open).toHaveBeenCalledWith({ key: "workflow:wf-1", title: "Formal audit" });
    expect(notifications.notify.mock.calls[0]![0].body).toMatch(/Incomplete .*Not runnable in this version/);
  });

  it("reports a refused import without opening anything", async () => {
    wfIpc.import.mockRejectedValue({ code: "workflow.invalid", message: "unknown field `trust`" });
    expect(await actions.importDraft('{"trust":true}')).toEqual({ ok: false, error: "unknown field `trust`" });
    expect(editorsMock.open).not.toHaveBeenCalled();
  });
});

describe("WorkflowEditor", () => {
  it("is labelled not runnable, shows live diagnostics, and saves a new revision", async () => {
    wfIpc.validate.mockResolvedValue(validation(1));
    wfIpc.save.mockImplementation(async (req: { definition: WorkflowDefinition }) => saved(2, req.definition, 1));
    render(<WorkflowEditor resource={{ key: "workflow:wf-1", title: "Formal audit" }} mode="source" active />);
    const status = await screen.findByTestId("workflow-status");
    expect(status.getAttribute("data-runnable")).toBe("false");
    expect(screen.getByText("Draft — not runnable in this version")).toBeTruthy();
    // Edit the title: dirty, validated in Rust, then saved with the expected revision.
    fireEvent.change(screen.getByTestId("workflow-title").querySelector("input")!, { target: { value: "Audit Main.lean" } });
    await waitFor(() => expect(wfIpc.validate).toHaveBeenCalledWith(expect.objectContaining({ title: "Audit Main.lean" })));
    await screen.findByText(/is not bound/);
    expect(editorsMock.setDirty).toHaveBeenLastCalledWith("workflow:wf-1", true);
    fireEvent.click(screen.getByTestId("workflow-save"));
    await waitFor(() =>
      expect(wfIpc.save).toHaveBeenCalledWith({ id: "wf-1", expectedRevision: 1, definition: expect.objectContaining({ title: "Audit Main.lean" }) }),
    );
    await waitFor(() => expect(screen.getByTestId("workflow-editor").getAttribute("data-revision")).toBe("2"));
    expect(screen.getByTestId("workflow-status").getAttribute("data-runnable")).toBe("false");
    expect(editorsMock.registerCloseGuard).toHaveBeenCalledWith("workflow:wf-1", expect.any(Function));
  });
});
