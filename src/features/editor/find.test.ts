// Find routing and focus rules: Ctrl+F reaches Monaco's Find widget only for
// the editor area (source, or the source beside the pages for a page view);
// other regions, text fields, terminals, and pages keep the key.
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const fake = vi.hoisted(() => ({
  editors: [] as unknown[],
  addEditorAction: vi.fn(),
}));

vi.mock("../../lib/monaco", () => ({
  monaco: { editor: { getEditors: () => fake.editors, addEditorAction: fake.addEditorAction } },
}));

import { useEditors } from "../../app/editors";
import { installKeybindings } from "../../app/keybindings";
import { pages, resetPagesForTests } from "../../app/pages";
import { registry } from "../../app/registry";
import {
  FIND_COMMAND_ID,
  FIND_IN_SOURCE_COMMAND_ID,
  findAllowedFrom,
  findRoute,
  MONACO_FIND_ACTION,
  registerFind,
} from "./find";

function fakeMonaco(host: HTMLElement) {
  const run = vi.fn(async () => {});
  const editor = {
    getContainerDomNode: () => host,
    getModel: () => ({}),
    focus: vi.fn(),
    getAction: vi.fn((id: string) => (id === MONACO_FIND_ACTION ? { run } : null)),
  };
  return { editor, run };
}

function ctrlF(target: HTMLElement): KeyboardEvent {
  target.focus();
  const event = new KeyboardEvent("keydown", { key: "f", code: "KeyF", ctrlKey: true, bubbles: true, cancelable: true });
  target.dispatchEvent(event);
  return event;
}

const flush = () => new Promise((r) => setTimeout(r, 0));

let uninstall: () => void;
const contextMenuActions: unknown[] = [];
const disposers: Array<() => void> = [];

beforeAll(() => {
  registerFind();
  contextMenuActions.push(...fake.addEditorAction.mock.calls.map((c) => c[0]));
  disposers.push(
    registry.registerEditor({
      id: "t.typeset",
      priority: 10,
      matches: (r) => r.path?.endsWith(".typ") ?? false,
      modes: ["source", "page", "split"],
      component: () => null,
    }),
    registry.registerEditor({ id: "t.text", matches: (r) => r.path?.endsWith(".txt") ?? false, modes: ["source"], component: () => null }),
    registry.registerEditor({ id: "t.image", matches: (r) => r.path?.endsWith(".png") ?? false, modes: ["source"], component: () => null }),
  );
  uninstall = installKeybindings(window, "windows");
});

afterAll(() => {
  uninstall();
  for (const d of disposers) d();
});

let monacoHost: HTMLElement;
let pageInput: HTMLTextAreaElement;
let sidebarInput: HTMLInputElement;
let terminal: HTMLTextAreaElement;
let tabButton: HTMLButtonElement;

function openEditor(path: string, mode: "source" | "page" | "split") {
  useEditors.setState({
    groups: {
      primary: { id: "primary", editors: [{ resource: { key: `file:${path}`, path, title: path }, mode, dirty: true, preview: false }], activeKey: `file:${path}` },
      secondary: { id: "secondary", editors: [], activeKey: null },
    },
    activeGroup: "primary",
  });
}

beforeEach(() => {
  resetPagesForTests();
  fake.editors.length = 0;
  document.body.innerHTML = `
    <div data-region="sidebar"><input id="sidebar-input" type="text"></div>
    <div data-region="panel"><textarea id="terminal"></textarea></div>
    <div data-region="editor">
      <section class="wb-editor-group" data-group="primary">
        <button id="tab">a</button>
        <div role="tabpanel" inert><div class="other-tab"></div></div>
        <div role="tabpanel"><div id="monaco-host"></div><textarea id="page-input"></textarea></div>
      </section>
    </div>`;
  monacoHost = document.getElementById("monaco-host")!;
  pageInput = document.getElementById("page-input") as HTMLTextAreaElement;
  sidebarInput = document.getElementById("sidebar-input") as HTMLInputElement;
  terminal = document.getElementById("terminal") as HTMLTextAreaElement;
  tabButton = document.getElementById("tab") as HTMLButtonElement;
});

describe("findRoute", () => {
  it("chooses the source editor, the source beside the pages, or nothing", () => {
    expect(findRoute("source", ["source"], true)).toBe("source");
    expect(findRoute("split", ["source", "page", "split"], true)).toBe("source");
    expect(findRoute("page", ["source", "page", "split"], false)).toBe("pageToSource");
    expect(findRoute("source", ["source"], false)).toBeNull(); // e.g. an image viewer
    expect(findRoute("page", ["page"], false)).toBeNull();
  });
});

describe("findAllowedFrom", () => {
  it("allows the editor area and nothing focused, not other regions or pages", () => {
    expect(findAllowedFrom(pageInput)).toBe(true);
    expect(findAllowedFrom(document.body)).toBe(true);
    expect(findAllowedFrom(sidebarInput)).toBe(false);
    expect(findAllowedFrom(terminal)).toBe(false);
  });

  it("is not taken on another page", () => {
    disposers.push(registry.registerPage({ id: "t-agents", title: "Agents", icon: () => null, render: () => null }));
    pages.show("t-agents");
    expect(findAllowedFrom(pageInput)).toBe(false);
  });
});

describe("Ctrl+F", () => {
  it("opens Monaco's Find in a source view without touching the document", async () => {
    openEditor("notes.txt", "source");
    const { editor, run } = fakeMonaco(monacoHost);
    fake.editors.push(editor);
    const event = ctrlF(tabButton);
    expect(event.defaultPrevented).toBe(true);
    await flush();
    expect(editor.focus).toHaveBeenCalled();
    expect(run).toHaveBeenCalledTimes(1);
    // Find changes nothing about the document: still dirty, same mode.
    expect(useEditors.getState().groups.primary.editors[0]).toMatchObject({ dirty: true, mode: "source" });
  });

  it("leaves other text fields and terminals their own Ctrl+F", async () => {
    openEditor("notes.txt", "source");
    const { editor, run } = fakeMonaco(monacoHost);
    fake.editors.push(editor);
    expect(ctrlF(sidebarInput).defaultPrevented).toBe(false);
    expect(ctrlF(terminal).defaultPrevented).toBe(false);
    await flush();
    expect(run).not.toHaveBeenCalled();
  });

  it("does nothing for an editor without a source editor", async () => {
    openEditor("plot.png", "source");
    expect(registry.command(FIND_COMMAND_ID)?.enabled?.()).toBe(false);
    expect(ctrlF(tabButton).defaultPrevented).toBe(false);
  });

  it("from a page view opens Find in Source: Split, the source focused, Find open", async () => {
    openEditor("main.typ", "page");
    expect(registry.command(FIND_COMMAND_ID)?.enabled?.()).toBe(false);
    expect(registry.command(FIND_IN_SOURCE_COMMAND_ID)?.enabled?.()).toBe(true);
    const event = ctrlF(pageInput);
    expect(event.defaultPrevented).toBe(true);
    await flush();
    expect(useEditors.getState().groups.primary.editors[0]?.mode).toBe("split");
    // The source editor appears once Split renders.
    const { editor, run } = fakeMonaco(monacoHost);
    fake.editors.push(editor);
    await vi.waitFor(() => expect(run).toHaveBeenCalledTimes(1));
    expect(editor.focus).toHaveBeenCalled();
    expect(useEditors.getState().groups.primary.editors[0]?.dirty).toBe(true);
  });

  it("is not taken while another page is shown", async () => {
    openEditor("notes.txt", "source");
    fake.editors.push(fakeMonaco(monacoHost).editor);
    disposers.push(registry.registerPage({ id: "t-research", title: "Research", icon: () => null, render: () => null }));
    pages.show("t-research");
    expect(ctrlF(tabButton).defaultPrevented).toBe(false);
  });

  it("is listed in the palette from anywhere and adds Find to the editor context menu", () => {
    openEditor("notes.txt", "source");
    fake.editors.push(fakeMonaco(monacoHost).editor);
    // Outside a key press (palette, menus) Find is available whatever has focus.
    sidebarInput.focus();
    expect(registry.command(FIND_COMMAND_ID)).toMatchObject({ title: "Find", category: "Edit", keybinding: "Mod+F" });
    expect(registry.command(FIND_COMMAND_ID)?.enabled?.()).toBe(true);
    expect(contextMenuActions).toContainEqual(expect.objectContaining({ id: "exposition.find", label: "Find" }));
  });
});
