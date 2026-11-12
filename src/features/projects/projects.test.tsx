// New Project: pure helpers, the preset and objective creation flows (Rust
// calls mocked at the feature's IPC boundary), interrupted-setup detection,
// and the page rendered through the real shared components.
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BlueprintValidation } from "../../bindings/BlueprintValidation";
import type { CapabilityReport } from "../../bindings/CapabilityReport";
import type { CreationStatus } from "../../bindings/CreationStatus";
import type { PresetCatalog } from "../../bindings/PresetCatalog";
import type { ProjectBlueprint } from "../../bindings/ProjectBlueprint";
import type { ProjectCreated } from "../../bindings/ProjectCreated";

const projectIpc = vi.hoisted(() => ({
  presets: vi.fn(),
  capabilities: vi.fn(),
  presetBlueprint: vi.fn(),
  validate: vi.fn(),
  checkDestination: vi.fn(),
  create: vi.fn(),
  creationStatus: vi.fn(),
  creationResolve: vi.fn(),
}));
const ipc = vi.hoisted(() => ({ agentsTaskCreate: vi.fn() }));
const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));
const editorsMock = vi.hoisted(() => ({ open: vi.fn(), close: vi.fn(async () => true), all: vi.fn(() => []), setDirty: vi.fn() }));
const pagesMock = vi.hoisted(() => ({ show: vi.fn(() => false), active: vi.fn(() => "editor") }));
const layoutMock = vi.hoisted(() => ({ showView: vi.fn() }));
const workspace = vi.hoisted(() => ({ openWorkspace: vi.fn(async () => true), loadDir: vi.fn() }));
const agentsMock = vi.hoisted(() => ({ init: vi.fn(async () => {}), show: vi.fn() }));

vi.mock("./ipc", () => ({ projectCommands: projectIpc }));
vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { workspace: async () => () => {} },
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
}));
vi.mock("../../app/notifications", () => notifications);
vi.mock("../../app/editors", () => ({ editors: editorsMock }));
vi.mock("../../app/pages", () => ({ EDITOR_PAGE: "editor", pages: pagesMock }));
vi.mock("../../app/layout", () => ({ layout: layoutMock }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("../editor/documentClient", () => ({ fileKey: (p: string) => `file:${p}` }));
vi.mock("../agents/store", () => ({ AGENT_VIEW_ID: "agents.panel", agents: agentsMock }));
vi.mock("../workspace/store", async () => {
  const { create } = await import("zustand");
  return {
    FILES_VIEW_ID: "workspace.files",
    ROOT: "",
    loadDir: workspace.loadDir,
    openWorkspace: workspace.openWorkspace,
    useWorkspace: create(() => ({
      info: { root: "C:\\work\\spectral-gaps", name: "spectral-gaps", recovery: [] },
      recent: [{ path: "C:\\work\\older", name: "older", openedAt: 0 }],
    })),
  };
});

const model = await import("./model");
const store = await import("./store");
const { NewProjectView } = await import("./NewProjectView");

const catalog: PresetCatalog = {
  presets: [
    { id: "paper", title: "Mathematical paper", description: "A manuscript.", manuscript: true, computation: false, lean: false, researchOnly: false, workflow: null },
    { id: "research", title: "Blank research project", description: "Research only.", manuscript: false, computation: false, lean: false, researchOnly: true, workflow: null },
  ],
  defaultLeanToolchain: "leanprover/lean4:v4.35.0",
};

const report: CapabilityReport = {
  capabilities: [
    { capability: "typst", title: "Typst", available: true, detail: "Built into Exposition", guidance: null },
    { capability: "lean", title: "Lean", available: false, detail: "No Lean toolchain is installed", guidance: "Install one with elan" },
  ],
  runtimes: [{ runtime: "python", available: true, detail: "Python 3.14" }],
  leanToolchains: [],
};

const blueprint: ProjectBlueprint = {
  schema: "exposition.blueprint/1",
  source: "preset:paper",
  title: "Spectral gaps",
  purpose: "A manuscript.",
  folders: ["figures"],
  files: [{ path: "main.typ", content: "= Spectral gaps\n" }],
  research: null,
  entities: [],
  relations: [],
  collections: [],
  workflows: [],
  capabilities: [{ capability: "lean", purpose: "Checks" }],
  buildTargets: [],
  entry: { kind: "editor", path: "main.typ" },
};

const preview = (valid: boolean): BlueprintValidation => ({
    valid,
    diagnostics: valid ? [] : [{ severity: "error", code: "blueprint.path", message: '"../x": \'..\' would leave the project folder', subject: "../x" }],
    summary: {
      title: "Spectral gaps",
      purpose: "A manuscript.",
      source: "preset:paper",
      folders: ["figures"],
      files: [{ path: "main.typ", bytes: 16, roles: ["manuscript"] }],
      totalBytes: 16,
      research: null,
      hasManuscript: true,
      entities: [{ kind: "claim", label: "Main result", source: "main.typ" }],
      relations: ["Main result depends on Key definition"],
      collections: ["Manuscript (4 members)"],
      workflows: [],
      capabilities: [],
      buildTargets: [],
      entry: { kind: "editor", path: "main.typ" },
    },
});

const created: ProjectCreated = {
  root: "C:\\work\\spectral-gaps",
  operation: "op-1",
  title: "Spectral gaps",
  applied: { project: "prj-1", graphRevision: 3, resources: [{ path: "main.typ", entity: "ent-1" }], entities: [], workflows: [], researchSpace: null },
  entry: { kind: "editor", path: "main.typ" },
};

beforeEach(() => {
  vi.clearAllMocks();
  store.resetProjectsForTests();
  projectIpc.presets.mockResolvedValue(catalog);
  projectIpc.capabilities.mockResolvedValue(report);
  projectIpc.presetBlueprint.mockResolvedValue(blueprint);
  projectIpc.validate.mockResolvedValue(preview(true));
  projectIpc.checkDestination.mockResolvedValue({ path: "C:\\work\\spectral-gaps", ok: true, message: null, createsFolder: true });
  projectIpc.create.mockResolvedValue(created);
  projectIpc.creationStatus.mockResolvedValue([]);
  pagesMock.show.mockReturnValue(false);
});

afterEach(() => cleanup());

describe("model", () => {
  it("derives safe folder names", () => {
    expect(model.folderNameFor("Spectral gaps")).toBe("spectral-gaps");
    expect(model.folderNameFor('a/b:c*"d')).toBe("a-b-c-d");
    expect(model.folderNameFor("CON")).toBe("con-project");
    expect(model.folderNameFor("  ...  ")).toBe("new-project");
    expect(model.folderNameFor("x".repeat(100))).toHaveLength(60);
  });

  it("joins and splits locations in the parent's style", () => {
    expect(model.joinPath("C:\\work\\", "paper")).toBe("C:\\work\\paper");
    expect(model.joinPath("C:", "paper")).toBe("C:\\paper");
    expect(model.joinPath("/home/me", "paper")).toBe("/home/me/paper");
    expect(model.parentOf("C:\\work\\paper")).toBe("C:\\work");
  });

  it("asks only for the options a preset uses", () => {
    const request = model.presetRequest(catalog.presets[1]!, {
      title: " Notes ",
      format: "latexLua",
      runtime: "r",
      leanToolchain: "x",
      includeWorkflow: true,
    });
    expect(request).toEqual({ preset: "research", title: "Notes", format: null, runtime: null, leanToolchain: null, includeWorkflow: false, purpose: null });
  });

  it("reports declared tools only once discovery answers, checking Lean's exact pin", () => {
    const lean = { ...blueprint, files: [...blueprint.files, { path: "lean-toolchain", content: "leanprover/lean4:v4.35.0\n" }] };
    expect(model.declaredCapabilities(lean, null)).toBeNull();
    const missing = model.declaredCapabilities(lean, report)!;
    expect(missing).toHaveLength(1);
    expect(missing[0]).toMatchObject({ capability: "lean", available: false });
    expect(missing[0]!.guidance).toMatch(/elan toolchain install leanprover\/lean4:v4.35.0/);
    const installed = model.declaredCapabilities(lean, {
      ...report,
      capabilities: report.capabilities.map((c) => (c.capability === "lean" ? { ...c, available: true } : c)),
      leanToolchains: ["leanprover/lean4:v4.35.0"],
    })!;
    expect(installed[0]).toMatchObject({ available: true, guidance: null });
  });

  it("writes an objective prompt that allows research-only projects and forbids installs and execution", () => {
    const prompt = model.objectivePrompt("Study random graphs");
    expect(prompt).toContain("Study random graphs");
    expect(prompt).toMatch(/research-only project is fine/);
    expect(prompt).toMatch(/Do not install software and do not run code/);
  });
});

describe("preset path", () => {
  it("previews, creates with the previewed blueprint, opens the project, and shows the manuscript", async () => {
    await store.loadCatalog();
    store.setLocation("C:\\work\\spectral-gaps");
    await store.checkDestination();
    expect(projectIpc.validate).toHaveBeenCalledWith(JSON.stringify(blueprint));
    const result = await store.createFromPreset();
    expect(result).toEqual(created);
    expect(projectIpc.create).toHaveBeenCalledWith({ destination: "C:\\work\\spectral-gaps", blueprint: JSON.stringify(blueprint) });
    expect(workspace.openWorkspace).toHaveBeenCalledWith(created.root);
    expect(editorsMock.open).toHaveBeenCalledWith({ key: "file:main.typ", path: "main.typ", title: "main.typ" });
    expect(pagesMock.show).toHaveBeenCalledWith("editor");
    expect(notifications.notify).toHaveBeenCalledWith(expect.objectContaining({ tone: "success", body: expect.stringContaining("Nothing was trusted or run") }));
  });

  it("never creates an invalid blueprint", async () => {
    projectIpc.validate.mockResolvedValue(preview(false));
    await store.loadCatalog();
    expect(await store.createFromPreset()).toBeNull();
    expect(projectIpc.create).not.toHaveBeenCalled();
  });

  it("reports a refused destination and leaves the page open", async () => {
    projectIpc.create.mockRejectedValue({ code: "workspace.tooBroad", message: "C:\\Users is the users folder." });
    await store.loadCatalog();
    expect(await store.createFromPreset()).toBeNull();
    expect(useStoreError()).toMatch(/users folder/);
    expect(editorsMock.close).not.toHaveBeenCalledWith("new-project:");
  });

  it("derives the default location from the most recent project's folder and follows the title", async () => {
    await store.loadCatalog();
    expect(store.useProjects.getState().location).toBe("C:\\work\\mathematical-paper");
    store.setOptions({ title: "Spectral gaps" });
    expect(store.useProjects.getState().location).toBe("C:\\work\\spectral-gaps");
    store.setLocation("D:\\elsewhere\\mine");
    store.setOptions({ title: "Other" });
    expect(store.useProjects.getState().location).toBe("D:\\elsewhere\\mine");
  });

  it("opens the Research page (or the notes) for a research-only project", () => {
    pagesMock.show.mockImplementation(((id: string) => id === "research") as never);
    store.openEntry({ kind: "research" }, { ...blueprint, research: { id: "rs-abcd", label: "Research" } });
    expect(pagesMock.show).toHaveBeenCalledWith("research");
    expect(editorsMock.open).not.toHaveBeenCalled();
    pagesMock.show.mockReturnValue(false);
    store.openEntry(
      { kind: "research" },
      { ...blueprint, research: { id: "rs-abcd", label: "Research" }, files: [{ path: ".exposition/research/rs-abcd/notes.md", content: "" }] },
    );
    expect(editorsMock.open).toHaveBeenCalledWith(expect.objectContaining({ path: ".exposition/research/rs-abcd/notes.md" }));
  });
});

function useStoreError() {
  return store.useProjects.getState().error;
}

describe("objective path", () => {
  it("creates an empty project and starts a whole-project agent task with no commands by default", async () => {
    ipc.agentsTaskCreate.mockResolvedValue({ id: "task-9" });
    await store.loadCatalog();
    store.setOptions({ mode: "objective", objective: "Study the spectral gap of random graphs", permission: "autoApply" });
    store.setLocation("C:\\work\\gap-study");
    const result = await store.createFromObjective();
    expect(result).toEqual(created);
    expect(projectIpc.presetBlueprint).toHaveBeenLastCalledWith(
      expect.objectContaining({ preset: "empty", purpose: "Study the spectral gap of random graphs" }),
    );
    expect(ipc.agentsTaskCreate).toHaveBeenCalledWith({
      prompt: expect.stringContaining("Study the spectral gap of random graphs"),
      workspaceRoot: "C:\\work\\spectral-gaps",
      scope: { kind: "workspace" },
      permission: "autoApply",
      execution: "disabled",
      consent: null,
      budgets: null,
    });
    expect(layoutMock.showView).toHaveBeenCalledWith("auxiliary", "agents.panel");
    expect(agentsMock.show).toHaveBeenCalledWith("task", "task-9");
  });

  it("needs an objective", async () => {
    store.setOptions({ mode: "objective", objective: "  " });
    expect(await store.createFromObjective()).toBeNull();
    expect(projectIpc.create).not.toHaveBeenCalled();
  });
});

describe("interrupted creation", () => {
  const setup: CreationStatus = {
    operation: "op-7",
    title: "Spectral gaps",
    root: "C:\\work\\spectral-gaps",
    state: { kind: "needsReconciliation", message: "Exposition stopped before this project was fully created." },
    inProgress: false,
    files: [
      { path: "main.typ", state: "created" },
      { path: "lakefile.toml", state: "missing" },
    ],
    created: 1,
    missing: 1,
    changed: 0,
    workflowsSaved: 0,
    workflowsTotal: 1,
    graphApplied: false,
    recordAvailable: true,
    actions: ["finish", "keep"],
    startedAtMs: 0,
  };

  it("offers a review of an interrupted setup, never treating it as complete", async () => {
    projectIpc.creationStatus.mockResolvedValue([setup, { ...setup, operation: "op-8", inProgress: true }]);
    const found = await store.checkSetups();
    expect(found.map((s) => s.operation)).toEqual(["op-7"]);
    const call = notifications.notify.mock.calls[0]![0];
    expect(call).toMatchObject({ id: "project-setup-interrupted", tone: "warning", timeoutMs: null });
    expect(call.body).toMatch(/1 file created, 1 missing, project structure not recorded/);
    call.actions[0].run();
    expect(editorsMock.open).toHaveBeenCalledWith({ key: "project-setup:op-7", title: "Project Setup" });
  });

  it("resolves with the chosen action", async () => {
    projectIpc.creationResolve.mockResolvedValue({ statuses: [], applied: null, entry: { kind: "editor", path: "main.typ" } });
    expect(await store.resolveSetup("op-7", "finish")).toBe(true);
    expect(projectIpc.creationResolve).toHaveBeenCalledWith("op-7", "finish");
    expect(notifications.dismiss).toHaveBeenCalledWith("project-setup-interrupted");
    expect(editorsMock.open).toHaveBeenCalledWith(expect.objectContaining({ path: "main.typ" }));
  });
});

describe("New Project page", () => {
  it("shows the page before tool discovery answers", async () => {
    let answer: (r: CapabilityReport) => void = () => {};
    projectIpc.capabilities.mockReturnValue(new Promise((resolve) => (answer = resolve)));
    render(<NewProjectView resource={{ key: "new-project:", title: "New Project" }} mode="source" active />);
    await screen.findByTestId("project-capabilities-checking");
    expect(screen.getByTestId("project-preview-files").textContent).toContain("main.typ");
    answer(report);
    await screen.findByTestId("project-capabilities");
  });

  it("shows what will be created and declared tools honestly", async () => {
    render(<NewProjectView resource={{ key: "new-project:", title: "New Project" }} mode="source" active />);
    await screen.findByTestId("project-capabilities");
    expect(screen.getByTestId("project-preview-files").textContent).toContain("main.typ");
    expect(screen.getByTestId("project-preview-entities").textContent).toContain("Main result");
    const lean = screen.getByTestId("project-capabilities").querySelector("[data-capability=lean]");
    expect(lean?.getAttribute("data-available")).toBe("false");
    expect(lean?.textContent).toMatch(/Install one with elan/);
    expect(screen.getByTestId("project-authorization").textContent).toMatch(/does not trust the folder, run any code, install anything/);
    await waitFor(() => expect(screen.getByTestId("project-create").hasAttribute("disabled")).toBe(false));
  });

  it("keeps Create disabled for an invalid blueprint and lists why", async () => {
    projectIpc.validate.mockResolvedValue(preview(false));
    render(<NewProjectView resource={{ key: "new-project:", title: "New Project" }} mode="source" active />);
    await screen.findByTestId("project-diagnostics");
    expect(screen.getByTestId("project-diagnostics").textContent).toContain("would leave the project folder");
    expect(screen.getByTestId("project-create").hasAttribute("disabled")).toBe(true);
  });
});
