import { afterEach, describe, expect, it, vi } from "vitest";

import {
  chordFromEvent,
  chordsEqual,
  formatKeybinding,
  installKeybindings,
  isEditableTarget,
  isGlobalChord,
  parseKeybinding,
} from "./keybindings";
import { registry } from "./registry";

const key = (init: KeyboardEventInit & { key: string }) => new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });

describe("parseKeybinding", () => {
  it("maps Mod to Ctrl on Windows/Linux and Cmd on macOS", () => {
    expect(parseKeybinding("Mod+Shift+P", "windows")).toEqual({ ctrl: true, meta: false, alt: false, shift: true, key: "p" });
    expect(parseKeybinding("Mod+Shift+P", "mac")).toEqual({ ctrl: false, meta: true, alt: false, shift: true, key: "p" });
  });

  it("accepts modifier aliases, named keys, and a literal plus", () => {
    expect(parseKeybinding("ctrl+alt+b", "windows")).toMatchObject({ ctrl: true, alt: true, key: "b" });
    expect(parseKeybinding("F6", "windows")).toEqual({ ctrl: false, meta: false, alt: false, shift: false, key: "f6" });
    expect(parseKeybinding("Shift+F6", "windows")).toMatchObject({ shift: true, key: "f6" });
    expect(parseKeybinding("Mod+Plus", "windows")).toMatchObject({ ctrl: true, key: "+" });
    expect(parseKeybinding("Mod+PageDown", "windows")).toMatchObject({ key: "pagedown" });
    expect(parseKeybinding("Escape", "windows").key).toBe("escape");
  });

  it("rejects malformed bindings", () => {
    expect(() => parseKeybinding("Mod+", "windows")).toThrow();
    expect(() => parseKeybinding("Hyper+K", "windows")).toThrow();
    expect(() => parseKeybinding("Mod+A+B", "windows")).toThrow();
  });
});

describe("chordFromEvent", () => {
  it("normalizes letters, including shifted and layout-altered keys", () => {
    expect(chordFromEvent(key({ key: "P", code: "KeyP", ctrlKey: true, shiftKey: true }))).toEqual({
      ctrl: true,
      meta: false,
      alt: false,
      shift: true,
      key: "p",
    });
    // Ctrl+Alt (AltGr) or non-Latin layouts change `key`; the physical letter still matches.
    expect(chordFromEvent(key({ key: "∫", code: "KeyB", ctrlKey: true, altKey: true })).key).toBe("b");
    expect(chordFromEvent(key({ key: "и", code: "KeyB", ctrlKey: true })).key).toBe("b");
    expect(chordFromEvent(key({ key: "!", code: "Digit1", ctrlKey: true, shiftKey: true })).key).toBe("1");
    expect(chordFromEvent(key({ key: " ", code: "Space" })).key).toBe("space");
    expect(chordFromEvent(key({ key: "F6", code: "F6" })).key).toBe("f6");
  });

  it("matches a parsed binding", () => {
    const event = key({ key: "b", code: "KeyB", ctrlKey: true, altKey: true });
    expect(chordsEqual(chordFromEvent(event), parseKeybinding("Mod+Alt+B", "windows"))).toBe(true);
    expect(chordsEqual(chordFromEvent(event), parseKeybinding("Mod+B", "windows"))).toBe(false);
  });
});

describe("formatKeybinding", () => {
  it("renders platform labels", () => {
    expect(formatKeybinding("Mod+Shift+P", "windows")).toEqual(["Ctrl", "Shift", "P"]);
    expect(formatKeybinding("Mod+Alt+B", "mac")).toEqual(["⌥", "⌘", "B"]);
    expect(formatKeybinding("F6", "windows")).toEqual(["F6"]);
    expect(formatKeybinding("Mod+PageDown", "windows")).toEqual(["Ctrl", "PageDown"]);
  });
});

describe("text-editing protection", () => {
  it("detects editable targets including Monaco", () => {
    const input = document.createElement("input");
    const checkbox = Object.assign(document.createElement("input"), { type: "checkbox" });
    const monaco = document.createElement("div");
    monaco.className = "monaco-editor";
    const inner = document.createElement("span");
    monaco.append(inner);
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    expect(isEditableTarget(input)).toBe(true);
    expect(isEditableTarget(checkbox)).toBe(false);
    expect(isEditableTarget(inner)).toBe(true);
    expect(isEditableTarget(editable)).toBe(true);
    expect(isEditableTarget(document.createElement("button"))).toBe(false);
  });

  it("treats modifier and function-key chords as global except text-editing chords", () => {
    expect(isGlobalChord(parseKeybinding("Mod+Shift+P", "windows"))).toBe(true);
    expect(isGlobalChord(parseKeybinding("F6", "windows"))).toBe(true);
    expect(isGlobalChord(parseKeybinding("Mod+B", "windows"))).toBe(true);
    expect(isGlobalChord(parseKeybinding("Mod+C", "windows"))).toBe(false);
    expect(isGlobalChord(parseKeybinding("Mod+Z", "windows"))).toBe(false);
    expect(isGlobalChord(parseKeybinding("Mod+ArrowLeft", "windows"))).toBe(false);
    expect(isGlobalChord(parseKeybinding("Delete", "windows"))).toBe(false);
    expect(isGlobalChord(parseKeybinding("Alt+X", "windows"))).toBe(true);
  });
});

describe("installKeybindings", () => {
  const offs: (() => void)[] = [];
  afterEach(() => {
    while (offs.length) offs.pop()!();
  });

  it("runs the bound command and prevents the default action", async () => {
    const run = vi.fn();
    offs.push(registry.registerCommand({ id: "t.toggle", title: "Toggle", keybinding: "Mod+B", run }));
    offs.push(installKeybindings(window, "windows"));
    const event = key({ key: "b", code: "KeyB", ctrlKey: true });
    document.body.dispatchEvent(event);
    await Promise.resolve();
    expect(run).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it("leaves non-global chords to text inputs and respects handled events", async () => {
    const run = vi.fn();
    offs.push(registry.registerCommand({ id: "t.del", title: "Delete", keybinding: "Delete", run }));
    offs.push(registry.registerCommand({ id: "t.palette", title: "Palette", keybinding: "Mod+Shift+P", run }));
    offs.push(installKeybindings(window, "windows"));
    const input = document.createElement("input");
    document.body.append(input);
    input.dispatchEvent(key({ key: "Delete", code: "Delete" }));
    expect(run).not.toHaveBeenCalled();
    // Global chords still reach the workbench from inside an input.
    input.dispatchEvent(key({ key: "P", code: "KeyP", ctrlKey: true, shiftKey: true }));
    await Promise.resolve();
    expect(run).toHaveBeenCalledTimes(1);
    // An editor that handled the key (preventDefault) keeps it.
    const handled = key({ key: "P", code: "KeyP", ctrlKey: true, shiftKey: true });
    handled.preventDefault();
    input.dispatchEvent(handled);
    await Promise.resolve();
    expect(run).toHaveBeenCalledTimes(1);
    input.remove();
  });

  it("honors an explicit workbench scope and ignores IME composition", async () => {
    const run = vi.fn();
    offs.push(
      registry.registerCommand({ id: "t.scoped", title: "Scoped", keybinding: "Mod+K", keybindingScope: "workbench", run }),
    );
    offs.push(installKeybindings(window, "windows"));
    const input = document.createElement("textarea");
    document.body.append(input);
    input.dispatchEvent(key({ key: "k", code: "KeyK", ctrlKey: true }));
    document.body.dispatchEvent(key({ key: "k", code: "KeyK", ctrlKey: true, isComposing: true }));
    await Promise.resolve();
    expect(run).not.toHaveBeenCalled();
    document.body.dispatchEvent(key({ key: "k", code: "KeyK", ctrlKey: true }));
    await Promise.resolve();
    expect(run).toHaveBeenCalledTimes(1);
    input.remove();
  });

  it("lets AltGr characters through in text inputs but keeps the chord elsewhere", async () => {
    const run = vi.fn();
    offs.push(registry.registerCommand({ id: "t.aux", title: "Aux", keybinding: "Mod+Alt+B", run }));
    offs.push(installKeybindings(window, "windows"));
    const input = document.createElement("input");
    document.body.append(input);
    // Hungarian layout: AltGr+B types "{".
    const typed = key({ key: "{", code: "KeyB", ctrlKey: true, altKey: true });
    input.dispatchEvent(typed);
    await Promise.resolve();
    expect(run).not.toHaveBeenCalled();
    expect(typed.defaultPrevented).toBe(false);
    document.body.dispatchEvent(key({ key: "{", code: "KeyB", ctrlKey: true, altKey: true }));
    input.dispatchEvent(key({ key: "b", code: "KeyB", ctrlKey: true, altKey: true }));
    await Promise.resolve();
    expect(run).toHaveBeenCalledTimes(2);
    input.remove();
  });

  it("skips disabled commands so another binding can apply", async () => {
    const first = vi.fn();
    const second = vi.fn();
    offs.push(registry.registerCommand({ id: "t.a", title: "A", keybinding: "Mod+J", enabled: () => false, run: first }));
    offs.push(registry.registerCommand({ id: "t.b", title: "B", keybinding: "Mod+J", run: second }));
    offs.push(installKeybindings(window, "windows"));
    document.body.dispatchEvent(key({ key: "j", code: "KeyJ", ctrlKey: true }));
    await Promise.resolve();
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });
});
