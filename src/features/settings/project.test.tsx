// Project settings: the target rule shared with the TeX service, edits that
// stay valid for the workspace service, and the Project settings view
// (add, edit engine, remove, default) driven through the real components.
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ProjectSettings } from "../../bindings/ProjectSettings";

const ipc = vi.hoisted(() => ({
  projectSettingsGet: vi.fn(),
  projectSettingsUpdate: vi.fn(),
  settingsGet: vi.fn(),
  settingsUpdate: vi.fn(),
}));
const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));
const active = vi.hoisted(() => ({ path: "paper/main.tex" as string | null }));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { workspace: async () => () => {} },
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
}));
vi.mock("../../app/notifications", () => notifications);
vi.mock("../../app/editors", () => ({
  editors: {
    active: () => (active.path ? { resource: { key: `file:${active.path}`, path: active.path } } : undefined),
    all: () => [],
  },
  useActiveEditor: () => (active.path ? { resource: { key: `file:${active.path}`, path: active.path } } : undefined),
}));
vi.mock("../workspace/store", async () => {
  const { create } = await import("zustand");
  return { useWorkspace: create(() => ({ info: { root: "C:\\p", name: "p", recovery: [] } })) };
});

const model = await import("./project");
const { ProjectSection } = await import("./ProjectSection");
const { switchEngine } = await import("./engine");
const { initialSettingsState, useSettings } = await import("./store");
const { resetAppSettingsForTests } = await import("../editor/settings");

const base: ProjectSettings = {
  version: 1,
  entry: "paper/main.tex",
  targets: [
    { name: "slides", format: "typst", engine: "typst", entry: "slides.typ" },
    { name: "pdf", format: "latex", engine: "pdfLatex", entry: "paper/main.tex" },
    { name: "lua", format: "latex", engine: "luaLatex", entry: "paper/main.tex" },
  ],
  defaultTarget: "lua",
};

describe("project model", () => {
  it("picks the target the TeX service builds a file with", () => {
    expect(model.targetForEntry(base, "paper/main.tex")).toMatchObject({ engine: "luaLatex", target: { name: "lua" } });
    expect(model.targetForEntry({ ...base, defaultTarget: "slides" }, "paper/main.tex").target?.name).toBe("pdf");
    expect(model.targetForEntry(base, "other.tex")).toEqual({ target: null, engine: "pdfLatex" });
  });

  it("switches the engine of the chosen target, or adds a target for the file", () => {
    const switched = model.withEntryEngine(base, "paper/main.tex", "pdfLatex");
    expect(switched.targets.map((t) => `${t.name}:${t.engine}`)).toEqual(["slides:typst", "pdf:pdfLatex", "lua:pdfLatex"]);
    const added = model.withEntryEngine({ ...base, targets: [...base.targets, { ...base.targets[1]!, name: "notes" }] }, "notes.tex", "luaLatex");
    expect(added.targets.at(-1)).toEqual({ name: "notes 2", format: "latex", engine: "luaLatex", entry: "notes.tex" });
  });

  it("validates drafts as the workspace service would", () => {
    const draft = (name: string, entry: string) => ({ name, entry, engine: "pdfLatex" as const });
    expect(model.validateDraft(draft("new", "a\\b.tex"), base.targets, null)).toBeNull();
    expect(model.validateDraft(draft(" pdf ", "a.tex"), base.targets, null)).toMatch(/exists already/);
    expect(model.validateDraft(draft("pdf", "a.tex"), base.targets, "pdf")).toBeNull();
    expect(model.validateDraft(draft("", "a.tex"), base.targets, null)).toMatch(/name/);
    expect(model.validateDraft(draft("x".repeat(65), "a.tex"), base.targets, null)).toMatch(/64/);
    expect(model.validateDraft(draft("n", "C:\\a.tex"), base.targets, null)).toMatch(/relative/);
    expect(model.validateDraft(draft("n", "../a.tex"), base.targets, null)).toMatch(/inside the project/);
    expect(model.validateDraft(draft("n", "a.typ"), base.targets, null)).toMatch(/\.tex/);
    expect(model.normalizeEntry(".\\paper\\\\main.tex")).toBe("paper/main.tex");
  });

  it("keeps the default target consistent on rename and removal", () => {
    const renamed = model.upsertTarget(base, "lua", { name: "luatex", entry: "paper/main.tex", engine: "luaLatex" });
    expect(renamed.defaultTarget).toBe("luatex");
    expect(model.removeTarget(renamed, "luatex").defaultTarget).toBeNull();
    expect(model.removeTarget(base, "pdf").targets.map((t) => t.name)).toEqual(["slides", "lua"]);
  });
});

describe("Project settings view", () => {
  beforeEach(() => {
    useSettings.setState(initialSettingsState, true);
    resetAppSettingsForTests();
    for (const fn of [...Object.values(ipc), notifications.notify]) fn.mockReset();
    active.path = "paper/main.tex";
    ipc.settingsGet.mockResolvedValue({
      settings: { version: 1, editor: {}, files: { reopenLastProject: false } },
      warning: null,
    });
    ipc.projectSettingsGet.mockResolvedValue({ settings: base, exists: true, warning: null });
    ipc.projectSettingsUpdate.mockImplementation(async (settings: ProjectSettings) => settings);
  });

  afterEach(cleanup);

  it("lists LaTeX targets and changes an engine, keeping other targets", async () => {
    render(<ProjectSection />);
    const rows = await screen.findAllByTestId("latex-target");
    expect(rows.map((r) => `${r.dataset.name}:${r.dataset.engine}`)).toEqual(["pdf:pdfLatex", "lua:luaLatex"]);
    expect(within(rows[1]!).getByText("Default")).toBeTruthy();
    fireEvent.click(within(rows[0]!).getByRole("button", { name: /Engine of pdf/ }));
    fireEvent.click(await screen.findByRole("option", { name: "LuaLaTeX" }));
    await waitFor(() => expect(ipc.projectSettingsUpdate).toHaveBeenCalledTimes(1));
    const saved = ipc.projectSettingsUpdate.mock.calls[0]![0] as ProjectSettings;
    expect(saved.targets.map((t) => `${t.name}:${t.engine}`)).toEqual(["slides:typst", "pdf:luaLatex", "lua:luaLatex"]);
  });

  it("adds a target for the active .tex file and removes one", async () => {
    ipc.projectSettingsGet.mockResolvedValue({ settings: { ...base, targets: [base.targets[0]!], defaultTarget: null }, exists: true, warning: null });
    render(<ProjectSection />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Add target…" }))[0]!);
    const dialog = await screen.findByRole("dialog", { name: "Add LaTeX target" });
    expect((within(dialog).getByLabelText("Main file") as HTMLInputElement).value).toBe("paper/main.tex");
    expect((within(dialog).getByLabelText("Name") as HTMLInputElement).value).toBe("main");
    fireEvent.click(within(dialog).getByRole("button", { name: "Add target" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect((ipc.projectSettingsUpdate.mock.calls[0]![0] as ProjectSettings).targets.at(-1)).toEqual({
      name: "main",
      format: "latex",
      engine: "pdfLatex",
      entry: "paper/main.tex",
    });
    fireEvent.click(await screen.findByRole("button", { name: "Remove target main" }));
    await waitFor(() => expect(ipc.projectSettingsUpdate).toHaveBeenCalledTimes(2));
    expect((ipc.projectSettingsUpdate.mock.calls[1]![0] as ProjectSettings).targets.map((t) => t.name)).toEqual(["slides"]);
  });

  it("shows the service's refusal and keeps the dialog open", async () => {
    render(<ProjectSection />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Add target…" }))[0]!);
    const dialog = await screen.findByRole("dialog", { name: "Add LaTeX target" });
    fireEvent.change(within(dialog).getByLabelText("Main file"), { target: { value: "../outside.tex" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add target" }));
    expect(await within(dialog).findByText(/inside the project folder/)).toBeTruthy();
    expect(ipc.projectSettingsUpdate).not.toHaveBeenCalled();
  });

  it("toggles reopening the last project at startup", async () => {
    ipc.settingsUpdate.mockImplementation(async (settings: unknown) => settings);
    render(<ProjectSection />);
    const toggle = await screen.findByRole("switch", { name: "Reopen the last project" });
    await waitFor(() => expect((toggle as HTMLInputElement).disabled).toBe(false));
    fireEvent.click(toggle);
    await waitFor(() => expect(ipc.settingsUpdate).toHaveBeenCalledTimes(1));
    expect(ipc.settingsUpdate.mock.calls[0]![0]).toMatchObject({ files: { reopenLastProject: true } });
  });
});

describe("engine switch for the active file", () => {
  beforeEach(() => {
    useSettings.setState(initialSettingsState, true);
    for (const fn of [...Object.values(ipc), notifications.notify]) fn.mockReset();
    ipc.projectSettingsUpdate.mockImplementation(async (settings: ProjectSettings) => settings);
  });

  it("adds a LuaLaTeX target for a file that had none", async () => {
    active.path = "notes.tex";
    ipc.projectSettingsGet.mockResolvedValue({ settings: base, exists: true, warning: null });
    expect(await switchEngine("luaLatex")).toBe(true);
    const saved = ipc.projectSettingsUpdate.mock.calls[0]![0] as ProjectSettings;
    expect(saved.targets.at(-1)).toEqual({ name: "notes", format: "latex", engine: "luaLatex", entry: "notes.tex" });
    expect(notifications.notify).toHaveBeenCalledWith(expect.objectContaining({ title: "notes.tex now builds with LuaLaTeX" }));
  });

  it("does nothing but say so when the file already uses that engine", async () => {
    active.path = "paper/main.tex";
    ipc.projectSettingsGet.mockResolvedValue({ settings: base, exists: true, warning: null });
    expect(await switchEngine("luaLatex")).toBe(true);
    expect(ipc.projectSettingsUpdate).not.toHaveBeenCalled();
  });
});
