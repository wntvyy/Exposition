// Outline navigation selects exactly the item's range, re-locates it after an
// edit only when it can be identified with certainty, and refuses (with a
// notice) instead of jumping to an unrelated declaration.
import { beforeEach, describe, expect, it, vi } from "vitest";

const shell = vi.hoisted(() => ({
  handle: null as unknown,
  active: null as unknown,
  revealSource: vi.fn(async () => true),
  requestReveal: vi.fn(),
  requestPage: vi.fn(),
  notify: vi.fn(),
}));

vi.mock("../editor", () => ({ handleForPath: () => shell.handle }));
vi.mock("../editor/find", () => ({ activeSourceEditor: () => null }));
vi.mock("../editor/reveal", () => ({ revealSource: shell.revealSource }));
vi.mock("../typesetting/store", () => ({ requestReveal: shell.requestReveal, requestPage: shell.requestPage, summarize: () => "none" }));
vi.mock("../../app/notifications", () => ({ notify: shell.notify }));
vi.mock("../../app/editors", () => ({ editors: { active: () => shell.active, setMode: vi.fn() }, useEditors: { getState: () => ({ activeGroup: "primary" }) } }));
vi.mock("../../app/registry", () => ({
  registry: { editorFor: (r: { path?: string }) => ({ modes: r.path?.endsWith(".typ") ? ["source", "page", "split"] : ["source"] }) },
}));
vi.mock("../../lib/ipc", () => ({ commands: {}, errorMessage: (e: unknown) => String(e) }));

import type { DocumentOutline } from "../../bindings/DocumentOutline";
import type { OutlineItem } from "../../bindings/OutlineItem";
import { navigateToItem } from "./navigate";
import type { OutlineStore } from "./store";

function item(id: string, header: string, start: number): OutlineItem {
  return {
    id,
    kind: "declaration",
    entityKind: "theorem",
    syntax: id.split("|")[0]!,
    typeLabel: "Theorem",
    title: id.split("|")[1]!,
    label: null,
    level: 0,
    range: { start, end: start + header.length },
    selection: { start, end: start + header.length },
    header,
    children: [],
  };
}

function outline(revision: number, items: OutlineItem[], path = "main.lean"): DocumentOutline {
  return { path, revision, unsaved: true, extractor: "x/1", coverage: "complete", detail: null, items, itemCount: items.length };
}

/** A document handle over `text` at `revision` (Monaco-like model API). */
function fakeHandle(text: string, revision: number, pending = 0) {
  const lineStarts = [0];
  for (let i = 0; i < text.length; i++) if (text[i] === "\n") lineStarts.push(i + 1);
  return {
    closed: false,
    sync: { revision, pendingCount: pending, idle: vi.fn(async () => {}) },
    model: {
      isDisposed: () => false,
      getValueLength: () => text.length,
      getPositionAt: (offset: number) => {
        let line = 0;
        while (line + 1 < lineStarts.length && lineStarts[line + 1]! <= offset) line++;
        return { lineNumber: line + 1, column: offset - lineStarts[line]! + 1 };
      },
      getLineContent: (n: number) => text.slice(lineStarts[n - 1], (lineStarts[n] ?? text.length + 1) - 1),
    },
  };
}

function storeAnswering(answer: DocumentOutline | null): OutlineStore {
  return { refresh: vi.fn(async () => answer) } as unknown as OutlineStore;
}

beforeEach(() => {
  vi.clearAllMocks();
  shell.active = { resource: { key: "file:main.lean", path: "main.lean", title: "main.lean" }, mode: "source" };
});

const TEXT = "theorem a : True := trivial\ntheorem b : True := trivial\n";

describe("navigateToItem", () => {
  it("selects the item's range when the outline answers the current text", async () => {
    shell.handle = fakeHandle(TEXT, 4);
    const b = item("lean.theorem|b|0", "theorem b : True := trivial", 28);
    const shown = outline(4, [item("lean.theorem|a|0", "theorem a : True := trivial", 0), b]);
    expect(await navigateToItem(b, shown, storeAnswering(null))).toBe("shown");
    expect(shell.revealSource).toHaveBeenCalledWith("main.lean", b.selection);
    expect(shell.notify).not.toHaveBeenCalled();
  });

  it("uses the shared page reveal for typeset documents", async () => {
    const text = "= Intro\n#lemma[x]\n";
    shell.handle = fakeHandle(text, 2);
    shell.active = { resource: { key: "file:main.typ", path: "main.typ", title: "main.typ" }, mode: "page" };
    const lemma = item("typst.call:lemma||0", "#lemma[x]", 8);
    expect(await navigateToItem(lemma, outline(2, [lemma], "main.typ"), storeAnswering(null))).toBe("shown");
    expect(shell.requestReveal).toHaveBeenCalledWith("main.typ", 8, 17);
    expect(shell.revealSource).not.toHaveBeenCalled();
  });

  it("follows an item that moved after an edit above it", async () => {
    const edited = "-- note\n" + TEXT;
    shell.handle = fakeHandle(edited, 5);
    const b = item("lean.theorem|b|0", "theorem b : True := trivial", 28);
    const shown = outline(4, [item("lean.theorem|a|0", "theorem a : True := trivial", 0), b]);
    const movedB = item("lean.theorem|b|0", "theorem b : True := trivial", 36);
    const fresh = outline(5, [item("lean.theorem|a|0", "theorem a : True := trivial", 8), movedB]);
    expect(await navigateToItem(b, shown, storeAnswering(fresh))).toBe("shown");
    expect(shell.revealSource).toHaveBeenCalledWith("main.lean", movedB.selection);
  });

  it("refuses an item that was removed, without moving to the next declaration", async () => {
    const edited = "theorem b : True := trivial\n";
    shell.handle = fakeHandle(edited, 5);
    const a = item("lean.theorem|a|0", "theorem a : True := trivial", 0);
    const shown = outline(4, [a, item("lean.theorem|b|0", "theorem b : True := trivial", 28)]);
    const fresh = outline(5, [item("lean.theorem|b|0", "theorem b : True := trivial", 0)]);
    expect(await navigateToItem(a, shown, storeAnswering(fresh))).toBe("refused");
    expect(shell.revealSource).not.toHaveBeenCalled();
    expect(shell.requestReveal).not.toHaveBeenCalled();
    expect(shell.notify).toHaveBeenCalledWith(expect.objectContaining({ tone: "info" }));
  });

  it("refuses when identical untitled items shifted", async () => {
    const text = "\\begin{theorem}\nB\n\\end{theorem}\n";
    shell.handle = fakeHandle(text, 3);
    const first = item("latex.env:theorem||0", "\\begin{theorem}", 0);
    const second = item("latex.env:theorem||1", "\\begin{theorem}", 40);
    const fresh = outline(3, [item("latex.env:theorem||0", "\\begin{theorem}", 0)], "main.lean");
    expect(await navigateToItem(first, outline(2, [first, second]), storeAnswering(fresh))).toBe("refused");
    expect(shell.revealSource).not.toHaveBeenCalled();
  });

  it("refuses when the source at the range is not the item's first line", async () => {
    shell.handle = fakeHandle(TEXT.replace("theorem b", "lemma b"), 4);
    const b = item("lean.theorem|b|0", "theorem b : True := trivial", 28);
    expect(await navigateToItem(b, outline(4, [b]), storeAnswering(null))).toBe("refused");
    expect(shell.revealSource).not.toHaveBeenCalled();
  });

  it("refuses when the document is no longer open", async () => {
    shell.handle = undefined;
    const b = item("lean.theorem|b|0", "theorem b : True := trivial", 28);
    expect(await navigateToItem(b, outline(4, [b]), storeAnswering(null))).toBe("refused");
  });

  it("waits for local edits to reach the service before re-reading", async () => {
    const handle = fakeHandle(TEXT, 4, 1);
    shell.handle = handle;
    const b = item("lean.theorem|b|0", "theorem b : True := trivial", 28);
    const store = storeAnswering(null);
    expect(await navigateToItem(b, outline(4, [b]), store)).toBe("refused");
    expect(handle.sync.idle).toHaveBeenCalled();
    expect(store.refresh).toHaveBeenCalled();
  });
});
