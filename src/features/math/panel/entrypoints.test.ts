// Entry points: the commands (palette, Ctrl+Shift+M from the editor area,
// source context menu) and how a caret target is resolved from the surface
// the user worked in last — the Monaco selection, or the page caret only when
// it is in the model's current revision.
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const fake = vi.hoisted(() => ({
  editors: [] as unknown[],
  addEditorAction: vi.fn(),
  contributions: new Map<string, (editor: unknown, handle: unknown) => { dispose(): void }>(),
  handles: new Map<string, unknown>(),
  open: vi.fn(async () => true),
}));

vi.mock("../../../lib/monaco", () => ({
  monaco: { editor: { getEditors: () => fake.editors, addEditorAction: fake.addEditorAction } },
}));
vi.mock("../../editor", () => ({
  handleFor: (id: string) => [...fake.handles.values()].find((h) => (h as { id: string }).id === id),
  handleForPath: (path: string) => fake.handles.get(path),
}));
vi.mock("../../editor/contributions", () => ({
  registerSourceEditorContribution: (id: string, c: (editor: unknown, handle: unknown) => { dispose(): void }) => {
    fake.contributions.set(id, c);
    return () => fake.contributions.delete(id);
  },
}));
vi.mock("../../editor/reveal", () => ({ revealSource: vi.fn(async () => true) }));
vi.mock("../../editor/sourceFocus", () => ({ requestSourceFocus: vi.fn() }));
vi.mock("./store", () => ({ equationStore: { open: fake.open } }));

import { useEditors } from "../../../app/editors";
import { installKeybindings } from "../../../app/keybindings";
import { pages, resetPagesForTests } from "../../../app/pages";
import { registry } from "../../../app/registry";
import { notePageFocus, releasePageSurface, type PageSurface } from "../../page/activePage";
import { EQUATION_COMMANDS, registerEquationCommands } from "./commands";
import { installSourceFocusTracking, resolveActiveCaret } from "./surfaces";
import { fakeDocument, type FakeDocument } from "./testing/fakeDocument";

const flush = () => new Promise((r) => setTimeout(r, 0));

function openEditor(path: string, mode: "source" | "page" | "split") {
  useEditors.setState({
    groups: {
      primary: { id: "primary", editors: [{ resource: { key: `file:${path}`, path, title: path }, mode, dirty: false, preview: false }], activeKey: `file:${path}` },
      secondary: { id: "secondary", editors: [], activeKey: null },
    },
    activeGroup: "primary",
  });
}

/** A Monaco editor double showing `doc` with a selection. */
function sourceEditor(doc: FakeDocument, start: number, end = start) {
  const model = doc.model;
  const listeners: Array<() => void> = [];
  const editor = {
    getModel: () => model,
    hasTextFocus: () => false,
    getSelection: () => ({ getStartPosition: () => model.getPositionAt(start), getEndPosition: () => model.getPositionAt(end) }),
    setPosition: vi.fn(),
    focus: vi.fn(),
    onDidFocusEditorText: (l: () => void) => {
      listeners.push(l);
      return { dispose() {} };
    },
    onDidChangeCursorPosition: () => ({ dispose() {} }),
    /** Simulate the user focusing this editor. */
    takeFocus: () => listeners.forEach((l) => l()),
  };
  fake.editors.push(editor);
  fake.contributions.get("math.equationFocus")?.(editor, doc);
  return editor;
}

function pageSurface(path: string, caret: ReturnType<PageSurface["currentCaret"]>): PageSurface {
  return { path, focus: vi.fn(), currentCaret: () => caret };
}

let uninstall: () => void;
let doc: FakeDocument;
const surfaces: PageSurface[] = [];

type MenuAction = { id: string; label: string; precondition: string; contextMenuGroupId: string; run: () => void };
const menuActions: MenuAction[] = [];

beforeAll(() => {
  registerEquationCommands();
  menuActions.push(...fake.addEditorAction.mock.calls.map((c) => c[0] as MenuAction));
  installSourceFocusTracking();
  uninstall = installKeybindings(window, "windows");
});

afterAll(() => uninstall());

beforeEach(() => {
  resetPagesForTests();
  fake.editors.length = 0;
  fake.open.mockClear();
  for (const s of surfaces.splice(0)) releasePageSurface(s);
  doc = fakeDocument("Let $x$ be.", { path: "main.tex" });
  fake.handles.clear();
  fake.handles.set("main.tex", doc);
  document.body.innerHTML = `
    <div data-region="editor"><div id="monaco" class="monaco-editor"><textarea id="monaco-input"></textarea></div></div>
    <div data-region="auxiliary"><input id="aux-input" type="text"></div>`;
});

describe("equation commands", () => {
  it("registers the palette commands and the source context menu", () => {
    for (const id of Object.values(EQUATION_COMMANDS)) expect(registry.command(id)).toBeTruthy();
    expect(registry.command(EQUATION_COMMANDS.insertOrEdit)!.keybinding).toBe("Mod+Shift+M");
    expect(menuActions.map((a) => a.label)).toEqual(["Insert Equation", "Insert Display Equation", "Edit Equation"]);
    for (const a of menuActions) {
      expect(a.contextMenuGroupId).toBe("1_modification");
      expect(a.precondition).toContain("editorLangId == latex");
    }
  });

  it("is available only for math documents on the Editor page", () => {
    const command = registry.command(EQUATION_COMMANDS.insertOrEdit)!;
    openEditor("notes.txt", "source");
    expect(command.enabled!()).toBe(false);
    openEditor("main.tex", "split");
    expect(command.enabled!()).toBe(true);
    disposeAfter(registry.registerPage({ id: "t-research", title: "Research", icon: () => null, render: () => null }));
    pages.show("t-research");
    expect(command.enabled!()).toBe(false);
  });

  it("runs from the palette and the context menu with the right mode", async () => {
    openEditor("main.tex", "source");
    await registry.runCommand(EQUATION_COMMANDS.insertDisplay);
    expect(fake.open).toHaveBeenLastCalledWith("display", undefined);
    await registry.runCommand(EQUATION_COMMANDS.edit);
    expect(fake.open).toHaveBeenLastCalledWith("edit", undefined);
    menuActions.find((a) => a.label === "Insert Equation")!.run();
    expect(fake.open).toHaveBeenLastCalledWith("inline", "source");
  });

  it("takes Ctrl+Shift+M in the editor area (Monaco or page input), not in other regions", async () => {
    openEditor("main.tex", "split");
    const press = (target: HTMLElement) => {
      target.focus();
      const event = new KeyboardEvent("keydown", { key: "M", code: "KeyM", ctrlKey: true, shiftKey: true, bubbles: true, cancelable: true });
      target.dispatchEvent(event);
      return event;
    };
    expect(press(document.getElementById("monaco-input")!).defaultPrevented).toBe(true);
    await flush();
    expect(fake.open).toHaveBeenLastCalledWith("auto", "source");
    fake.open.mockClear();
    expect(press(document.getElementById("aux-input")!).defaultPrevented).toBe(false);
    await flush();
    expect(fake.open).not.toHaveBeenCalled();
  });
});

const disposers: Array<() => void> = [];
function disposeAfter(d: () => void) {
  disposers.push(d);
}
afterAll(() => disposers.forEach((d) => d()));

describe("resolving the caret target", () => {
  it("uses the Monaco selection of the source view", async () => {
    openEditor("main.tex", "source");
    sourceEditor(doc, 4, 7);
    const resolved = await resolveActiveCaret();
    expect(resolved).toMatchObject({ surface: "source", path: "main.tex", start: 4, end: 7 });
  });

  it("uses the surface the user worked in last in Split", async () => {
    openEditor("main.tex", "split");
    const editor = sourceEditor(doc, 1);
    const page = pageSurface("main.tex", { offset: 5, anchor: 2, revision: 1, mode: "math" });
    surfaces.push(page);
    editor.takeFocus();
    notePageFocus(page);
    expect(await resolveActiveCaret()).toMatchObject({ surface: "page", start: 2, end: 5 });
    editor.takeFocus();
    expect(await resolveActiveCaret()).toMatchObject({ surface: "source", start: 1, end: 1 });
    // An entry point can name its surface (the page toolbar, a key pressed on the page).
    expect(await resolveActiveCaret("page")).toMatchObject({ surface: "page" });
  });

  it("uses the page in Page mode and refuses a page caret of another revision", async () => {
    openEditor("main.tex", "page");
    sourceEditor(doc, 1); // a hidden source editor of another group is not used in Page mode
    const page = pageSurface("main.tex", { offset: 5, anchor: null, revision: 3, mode: "markup" });
    surfaces.push(page);
    notePageFocus(page);
    const resolved = await resolveActiveCaret();
    expect(resolved).toEqual({ error: "The page has not caught up with the latest change to the source yet. Try again in a moment." });
  });

  it("explains what is missing", async () => {
    openEditor("notes.txt", "source");
    expect(await resolveActiveCaret()).toEqual({ error: "Equations can be edited in LaTeX, Typst, and Markdown documents." });
    openEditor("main.tex", "page");
    expect(await resolveActiveCaret()).toEqual({ error: "Show the source or the page of the document to choose where the equation goes." });
    const page = pageSurface("main.tex", null);
    surfaces.push(page);
    notePageFocus(page);
    expect(await resolveActiveCaret()).toEqual({ error: "Place the caret on the page first (click in the text or an equation)." });
  });
});
