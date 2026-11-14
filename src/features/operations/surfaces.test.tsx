// Focus routing of the operation shortcuts through the real registry and
// dispatcher: only the focused surface acts; Monaco, text inputs, rename
// fields, dialogs and selected text keep their keys; a held destructive key
// and IME composition never act; the palette acts on the surface it was
// opened from.
import { act, cleanup } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { installKeybindings } from "../../app/keybindings";
import { registry } from "../../app/registry";
import { commandPalette } from "../../app/shell/CommandPalette";

vi.mock("./ipc", () => ({ opCommands: {}, opEvents: { projectOp: vi.fn(async () => () => {}) } }));

import { operationCommands, OP_COMMAND_IDS, registerSurface, resetSurfacesForTests, startSurfaceTracking, type SurfaceActions } from "./surfaces";

const calls: string[] = [];
let disposers: (() => void)[] = [];
let uninstall: () => void = () => {};

function action(id: string, enabled = true) {
  return { enabled, run: () => void calls.push(id) };
}

function surfaceDom() {
  document.body.innerHTML = `
    <div id="surface">
      <div id="item" tabindex="0" role="row">plot.png</div>
      <input id="rename" type="text" value="plot.png" />
      <div class="monaco-editor"><textarea id="monaco"></textarea></div>
      <div role="dialog"><button id="in-dialog">OK</button></div>
      <p id="text">Some selectable log text</p>
    </div>
    <button id="outside">Elsewhere</button>`;
}

function press(target: Element, init: KeyboardEventInit) {
  const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

function focus(id: string): HTMLElement {
  const el = document.getElementById(id)!;
  el.focus();
  return el;
}

const flush = () => act(async () => await new Promise((r) => setTimeout(r, 0)));

beforeAll(() => {
  // Windows bindings (Delete / Shift+Delete).
  Object.defineProperty(navigator, "platform", { value: "Win32", configurable: true });
});

beforeEach(() => {
  calls.length = 0;
  surfaceDom();
  startSurfaceTracking();
  disposers = operationCommands().map((c) => registry.registerCommand(c));
  const actions: SurfaceActions = {
    copy: action("copy"),
    paste: action("paste"),
    duplicate: action("duplicate"),
    delete: action("delete"),
    deletePermanently: action("deletePermanently"),
  };
  disposers.push(registerSurface({ id: "test", element: () => document.getElementById("surface"), actions: () => actions }));
  uninstall = installKeybindings(window, "windows");
});

afterEach(() => {
  uninstall();
  for (const d of disposers) d();
  resetSurfacesForTests();
  commandPalette.close();
  window.getSelection()?.removeAllRanges();
  cleanup();
});

afterAll(() => {
  document.body.innerHTML = "";
});

describe("focused surface", () => {
  it("Delete, Shift+Delete, Ctrl+C, Ctrl+V, Ctrl+D act on the focused selection", async () => {
    const item = focus("item");
    press(item, { key: "Delete", code: "Delete" });
    press(item, { key: "Delete", code: "Delete", shiftKey: true });
    press(item, { key: "c", code: "KeyC", ctrlKey: true });
    press(item, { key: "v", code: "KeyV", ctrlKey: true });
    press(item, { key: "d", code: "KeyD", ctrlKey: true });
    await flush();
    expect(calls).toEqual(["delete", "deletePermanently", "copy", "paste", "duplicate"]);
  });

  it("a held Delete key acts once (auto-repeat suppressed)", async () => {
    const item = focus("item");
    press(item, { key: "Delete", code: "Delete" });
    press(item, { key: "Delete", code: "Delete", repeat: true });
    press(item, { key: "Delete", code: "Delete", repeat: true });
    await flush();
    expect(calls).toEqual(["delete"]);
  });

  it("IME composition never acts", async () => {
    const item = focus("item");
    press(item, { key: "Delete", code: "Delete", isComposing: true });
    press(item, { key: "Process", code: "KeyD", ctrlKey: true, keyCode: 229 } as KeyboardEventInit);
    await flush();
    expect(calls).toEqual([]);
  });

  it("an unavailable action does nothing", async () => {
    disposers.pop()?.();
    disposers.push(
      registerSurface({ id: "empty", element: () => document.getElementById("surface"), actions: () => ({ delete: action("delete", false) }) }),
    );
    const item = focus("item");
    press(item, { key: "Delete", code: "Delete" });
    await flush();
    expect(calls).toEqual([]);
  });
});

describe("text and other surfaces keep their keys", () => {
  it("a rename/text field inside the surface: Delete, Ctrl+D, Ctrl+C, Ctrl+Z do not touch resources", async () => {
    const input = focus("rename");
    press(input, { key: "Delete", code: "Delete" });
    press(input, { key: "Backspace", code: "Backspace" });
    press(input, { key: "d", code: "KeyD", ctrlKey: true });
    press(input, { key: "c", code: "KeyC", ctrlKey: true });
    press(input, { key: "z", code: "KeyZ", ctrlKey: true });
    await flush();
    expect(calls).toEqual([]);
  });

  it("Monaco: Ctrl+D stays its command, Delete edits text", async () => {
    const monaco = focus("monaco");
    press(monaco, { key: "d", code: "KeyD", ctrlKey: true });
    press(monaco, { key: "Delete", code: "Delete" });
    press(monaco, { key: "Delete", code: "Delete", shiftKey: true });
    await flush();
    expect(calls).toEqual([]);
  });

  it("a dialog keeps its keys", async () => {
    const button = focus("in-dialog");
    press(button, { key: "Delete", code: "Delete" });
    press(button, { key: "d", code: "KeyD", ctrlKey: true });
    await flush();
    expect(calls).toEqual([]);
  });

  it("focus outside any surface: nothing acts", async () => {
    const outside = focus("outside");
    press(outside, { key: "Delete", code: "Delete" });
    press(outside, { key: "c", code: "KeyC", ctrlKey: true });
    await flush();
    expect(calls).toEqual([]);
  });

  it("focus on the page body (clicked into empty space): nothing acts", async () => {
    focus("item");
    (document.activeElement as HTMLElement).blur();
    press(document.body, { key: "Delete", code: "Delete" });
    await flush();
    expect(calls).toEqual([]);
  });

  it("selected text: Ctrl+C copies the text, not the selection's items", async () => {
    const item = focus("item");
    const range = document.createRange();
    range.selectNodeContents(document.getElementById("text")!);
    window.getSelection()!.removeAllRanges();
    window.getSelection()!.addRange(range);
    expect({ active: document.activeElement?.id, collapsed: window.getSelection()?.isCollapsed }).toEqual({ active: "item", collapsed: false });
    press(item, { key: "c", code: "KeyC", ctrlKey: true });
    await flush();
    expect(calls).toEqual([]);
  });

  it("an event a focused component already handled is left alone", async () => {
    const item = focus("item");
    item.addEventListener("keydown", (e) => e.preventDefault(), { once: true });
    press(item, { key: "Delete", code: "Delete" });
    await flush();
    expect(calls).toEqual([]);
  });
});

describe("command palette", () => {
  it("lists and runs the commands for the surface it was opened from", async () => {
    focus("item");
    commandPalette.open();
    // Focus moves into the palette (an overlay).
    const overlay = document.createElement("div");
    overlay.className = "ui-modal-overlay";
    overlay.innerHTML = `<input id="palette-input" />`;
    document.body.appendChild(overlay);
    focus("palette-input");
    expect(registry.command(OP_COMMAND_IDS.delete)?.enabled?.()).toBe(true);
    commandPalette.close();
    overlay.remove();
    await registry.runCommand(OP_COMMAND_IDS.delete);
    expect(calls).toEqual(["delete"]);
  });

  it("opened from elsewhere, the surface commands are not offered", () => {
    focus("outside");
    commandPalette.open();
    expect(registry.command(OP_COMMAND_IDS.delete)?.enabled?.()).toBe(false);
    expect(registry.command(OP_COMMAND_IDS.copy)?.enabled?.()).toBe(false);
  });
});
