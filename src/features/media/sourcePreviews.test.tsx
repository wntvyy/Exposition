import { act, fireEvent, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ImageReference } from "../../bindings/ImageReference";

const ipc = vi.hoisted(() => ({
  mediaReadImage: vi.fn<(path: string) => Promise<ArrayBuffer>>(),
  workspaceOff: vi.fn(),
  workspace: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: { mediaReadImage: ipc.mediaReadImage, mediaImageReferences: vi.fn() },
  events: { workspace: ipc.workspace },
  isCommandError: (v: unknown) => typeof v === "object" && v !== null && "code" in v,
}));

vi.mock("../workspace/store", () => ({
  useWorkspace: { getState: () => ({ info: { root: "C:\\proj" } }) },
}));

import { resetImageCacheForTests, setImageDecoderForTests } from "./loader";
import { LINE_HEIGHT, planZones, PREVIEW_HEIGHT, PreviewController, previewPrefs, supportsSourcePreviews } from "./sourcePreviews";

// ----- a minimal Monaco editor/model double ------------------------------------------------------

function fakeModel(initial: string) {
  let text = initial;
  let version = 1;
  const listeners = new Set<() => void>();
  const lineOf = (offset: number) => text.slice(0, offset).split("\n").length;
  const model = {
    getValue: () => text,
    getVersionId: () => version,
    getValueLength: () => text.length,
    getPositionAt: (offset: number) => ({ lineNumber: lineOf(offset), column: 1 }),
    getOffsetAt: (p: { lineNumber: number; column: number }) =>
      text.split("\n").slice(0, p.lineNumber - 1).join("\n").length + (p.lineNumber > 1 ? 1 : 0) + p.column - 1,
    onDidChangeContent: (cb: () => void) => {
      listeners.add(cb);
      return { dispose: () => listeners.delete(cb) };
    },
    // Edit entry points: previews must never call them.
    pushEditOperations: vi.fn(),
    applyEdits: vi.fn(),
    setValue: vi.fn(),
    pushStackElement: vi.fn(),
    /** Simulate a user edit. */
    type(next: string) {
      text = next;
      version += 1;
      listeners.forEach((l) => l());
    },
  };
  return model;
}

interface Zone {
  afterLineNumber: number;
  heightInPx?: number;
  domNode: HTMLElement;
}

function fakeEditor() {
  const zones = new Map<string, Zone>();
  let next = 0;
  const editor = {
    zones,
    changeViewZones(cb: (a: unknown) => void) {
      cb({
        addZone: (z: Zone) => {
          const id = String(++next);
          zones.set(id, z);
          document.body.append(z.domNode);
          return id;
        },
        removeZone: (id: string) => {
          zones.get(id)?.domNode.remove();
          zones.delete(id);
        },
        layoutZone: vi.fn(),
      });
    },
    onDidFocusEditorWidget: () => ({ dispose() {} }),
    hasTextFocus: () => false,
    getPosition: () => ({ lineNumber: 1, column: 1 }),
  };
  return editor;
}

const found = (written: string, start: number, path = written): ImageReference => ({
  range: { start, end: start + written.length + 5 },
  written,
  location: { state: "found", path },
});

function png(): ArrayBuffer {
  const b = new Uint8Array(33);
  b.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52]);
  new DataView(b.buffer).setUint32(16, 2);
  new DataView(b.buffer).setUint32(20, 2);
  return b.buffer;
}

beforeEach(() => {
  ipc.workspaceOff.mockReset();
  ipc.workspace.mockReset();
  ipc.workspace.mockImplementation(async () => ipc.workspaceOff);
  localStorage.clear();
  resetImageCacheForTests();
  let n = 0;
  URL.createObjectURL = vi.fn(() => `blob:p-${++n}`);
  URL.revokeObjectURL = vi.fn();
  setImageDecoderForTests(async () => ({ width: 2, height: 2, preview: null }));
  ipc.mediaReadImage.mockReset();
  ipc.mediaReadImage.mockImplementation(async () => png());
});
afterEach(() => {
  setImageDecoderForTests(null);
  document.body.innerHTML = "";
});

describe("planning", () => {
  it("covers Markdown, LaTeX, and Typst sources only", () => {
    for (const p of ["a.md", "b/c.tex", "d.typ", "e.markdown", "F.TEX"]) expect(supportsSourcePreviews(p), p).toBe(true);
    for (const p of ["a.py", "b.lean", "c.bib", ".md", null]) expect(supportsSourcePreviews(p), String(p)).toBe(false);
  });

  it("places one zone after each reference's last line, keyed by path and occurrence", () => {
    const refs = [
      found("a.png", 0),
      { ...found("gone.png", 20), location: { state: "missing" } } as ImageReference,
      found("a.png", 40),
      { ...found("../x.png", 60), location: { state: "blocked" } } as ImageReference,
      { ...found("d.pdf", 80), location: { state: "unsupported", path: "d.pdf" } } as ImageReference,
    ];
    const plan = planZones(refs, (o) => Math.floor(o / 20) + 1, (w) => w === "a.png" && false, new Set());
    expect(plan.map((p) => [p.key, p.afterLineNumber, p.compact])).toEqual([
      ["a.png#1", 1, false],
      ["gone.png#1", 2, true],
      ["a.png#2", 3, false],
      ["../x.png#1", 4, true],
      ["d.pdf#1", 5, true],
    ]);
    const hidden = planZones(refs.slice(0, 1), () => 1, () => true);
    expect(hidden[0]).toMatchObject({ hidden: true, compact: true });
  });
});

describe("preferences", () => {
  it("persist per project file in the local view-preference storage", () => {
    const doc = previewPrefs.docKey("paper/main.tex");
    expect(doc).toBe("C:\\proj|paper/main.tex");
    previewPrefs.setHidden(doc, "figs/a.png", true);
    previewPrefs.setOff(doc, true);
    const stored = JSON.parse(localStorage.getItem("exposition.sourceImagePreviews")!);
    expect(stored.docs[doc]).toMatchObject({ off: true, hidden: ["figs/a.png"] });
    previewPrefs.setOff(doc, false);
    previewPrefs.setHidden(doc, "figs/a.png", false);
    expect(JSON.parse(localStorage.getItem("exposition.sourceImagePreviews")!).docs[doc]).toBeUndefined();
  });

  it("keep at most 200 documents", () => {
    for (let i = 0; i < 205; i++) previewPrefs.setOff(`C:\\p|f${i}.md`, true);
    const docs = JSON.parse(localStorage.getItem("exposition.sourceImagePreviews")!).docs;
    expect(Object.keys(docs).length).toBe(200);
  });
});

describe("PreviewController", () => {
  async function setup(text: string, answer: (text: string) => ImageReference[]) {
    const model = fakeModel(text);
    const editor = fakeEditor();
    const query = vi.fn(async (_path: string, t: string) => answer(t));
    const controller = new PreviewController(editor as never, { path: "doc/notes.md", model } as never, query);
    await act(async () => {
      await vi.waitFor(() => expect(query).toHaveBeenCalled());
    });
    await act(async () => {});
    return { model, editor, query, controller };
  }

  it("shows previews in view zones without touching the text, dirty state, or undo", async () => {
    const text = "# T\n![a](a.png)\n\nmore\n";
    const { model, editor, controller } = await setup(text, () => [found("a.png", 4)]);
    const zones = [...editor.zones.values()];
    expect(zones).toHaveLength(1);
    expect(zones[0]).toMatchObject({ afterLineNumber: 2, heightInPx: PREVIEW_HEIGHT });
    await waitFor(() => expect(document.querySelector("[data-testid=source-image-preview-image]")?.getAttribute("src")).toMatch(/^blob:/));
    // Hide and show this image, then the whole document.
    await act(async () => fireEvent.click(document.querySelector('[aria-label="Hide preview of a.png"]')!));
    await waitFor(() => expect([...editor.zones.values()][0]?.heightInPx).toBe(LINE_HEIGHT));
    expect(document.querySelector("[data-testid=source-image-preview]")?.getAttribute("data-state")).toBe("hidden");
    await act(async () => fireEvent.click(document.querySelector('[aria-label="Show preview of a.png"]')!));
    await waitFor(() => expect([...editor.zones.values()][0]?.heightInPx).toBe(PREVIEW_HEIGHT));
    await act(async () => controller.setDocumentPreviews(false));
    await waitFor(() => expect(editor.zones.size).toBe(0));
    await act(async () => controller.setDocumentPreviews(true));
    await waitFor(() => expect(editor.zones.size).toBe(1));
    // View-only: the model was never edited and its version never moved.
    expect(model.getValue()).toBe(text);
    expect(model.getVersionId()).toBe(1);
    for (const edit of [model.pushEditOperations, model.applyEdits, model.setValue, model.pushStackElement]) {
      expect(edit).not.toHaveBeenCalled();
    }
    controller.dispose();
    expect(editor.zones.size).toBe(0);
  });

  it("follows unsaved edits and drops answers for an older text", async () => {
    let release!: () => void;
    const { model, editor, query, controller } = await setup("![a](a.png)\n", (t) =>
      t.includes("b.png") ? [found("b.png", 6)] : [found("a.png", 0)],
    );
    expect([...editor.zones.values()]).toHaveLength(1);
    // A slow answer for the first edit arrives after a second edit: ignored.
    query.mockImplementationOnce(
      (_p, t) => new Promise((resolve) => (release = () => resolve(t.includes("b.png") ? [found("stale.png", 0)] : []))),
    );
    model.type("text\n![b](b.png)\n");
    await vi.waitFor(() => expect(query).toHaveBeenCalledTimes(2), { timeout: 2000 });
    model.type("text\n![b](b.png)\nmore\n");
    release();
    await vi.waitFor(() => expect(query).toHaveBeenCalledTimes(3), { timeout: 2000 });
    await act(async () => {});
    const shown = [...document.querySelectorAll("[data-testid=source-image-preview]")].map((e) => e.getAttribute("data-written"));
    expect(shown).toEqual(["b.png"]);
    expect([...editor.zones.values()][0]?.afterLineNumber).toBe(2);
    controller.dispose();
  });

  it("states unresolved, blocked, and undisplayable references in one line, never loading them", async () => {
    const { editor, controller } = await setup("x", () => [
      { ...found("gone.png", 0), location: { state: "missing" } },
      { ...found("../../out.png", 0), location: { state: "blocked" } },
      { ...found("fig.pdf", 0), location: { state: "unsupported", path: "fig.pdf" } },
    ]);
    expect([...editor.zones.values()].map((z) => z.heightInPx)).toEqual([LINE_HEIGHT, LINE_HEIGHT, LINE_HEIGHT]);
    const states = [...document.querySelectorAll("[data-testid=source-image-preview]")].map((e) => e.getAttribute("data-state"));
    expect(states).toEqual(["missing", "blocked", "unsupported"]);
    expect(document.body.textContent).toContain("Image not found: gone.png");
    expect(document.body.textContent).toContain("Outside the project, not shown: ../../out.png");
    expect(document.body.textContent).toContain("No preview for PDF files: fig.pdf");
    expect(ipc.mediaReadImage).not.toHaveBeenCalled();
    controller.dispose();
  });

  it("shares one workspace-event subscription among all controllers (perf FE-8)", async () => {
    const answer = vi.fn(() => [found("a.png", 0)]);
    const first = await setup("![a](a.png) one", answer);
    const second = await setup("![a](a.png) two", answer);
    expect(ipc.workspace).toHaveBeenCalledTimes(1);
    const deliver = ipc.workspace.mock.calls[0]![0] as (event: unknown) => void;
    const queries = () => first.query.mock.calls.length + second.query.mock.calls.length;
    const before = queries();
    // A change to a source file concerns no preview.
    deliver({ kind: "filesChanged", rescan: false, changes: [{ path: "doc/notes.md", kind: "modified" }] });
    await new Promise((r) => setTimeout(r, 250));
    expect(queries()).toBe(before);
    // A changed image: every controller locates its references again.
    deliver({ kind: "filesChanged", rescan: false, changes: [{ path: "a.png", kind: "modified" }] });
    await vi.waitFor(() => expect(queries()).toBe(before + 2));
    // Another project: previews are dropped at once and looked up again.
    deliver({ kind: "closed" });
    expect(first.editor.zones.size + second.editor.zones.size).toBe(0);
    await vi.waitFor(() => expect(queries()).toBe(before + 4));
    first.controller.dispose();
    expect(ipc.workspaceOff).not.toHaveBeenCalled();
    second.controller.dispose();
    await vi.waitFor(() => expect(ipc.workspaceOff).toHaveBeenCalledTimes(1));
    // The next controller subscribes again.
    const third = await setup("x", () => []);
    expect(ipc.workspace).toHaveBeenCalledTimes(2);
    third.controller.dispose();
  });

  it("shrinks to one line when the image cannot be displayed", async () => {
    ipc.mediaReadImage.mockImplementation(async () => new TextEncoder().encode("not an image").buffer as ArrayBuffer);
    const { editor, controller } = await setup("x", () => [found("fake.png", 0)]);
    await waitFor(() => expect([...editor.zones.values()][0]?.heightInPx).toBe(LINE_HEIGHT));
    expect(document.body.textContent).toContain("Unsupported image format: fake.png");
    controller.dispose();
  });
});

// Independent review of the shared workspace subscription (perf FE-8): leaks and missed updates
// as controllers come and go.
describe("review: shared workspace subscription", () => {
  async function make(text: string) {
    const query = vi.fn(async () => [] as ImageReference[]);
    const controller = new PreviewController(fakeEditor() as never, { path: "doc/notes.md", model: fakeModel(text) } as never, query);
    await act(async () => {
      await vi.waitFor(() => expect(query).toHaveBeenCalled());
    });
    return { controller, query };
  }

  it("subscribes once however many controllers exist and unsubscribes exactly once when the last one goes", async () => {
    const all: Array<Awaited<ReturnType<typeof make>>> = [];
    for (let i = 0; i < 5; i++) all.push(await make(`x${i}`));
    expect(ipc.workspace).toHaveBeenCalledTimes(1);
    // Disposed in another order, some twice.
    for (const index of [2, 0, 2, 4, 1]) all[index]!.controller.dispose();
    await new Promise((r) => setTimeout(r, 20));
    expect(ipc.workspaceOff).not.toHaveBeenCalled();
    const deliver = ipc.workspace.mock.calls[0]![0] as (event: unknown) => void;
    const before = all[3]!.query.mock.calls.length;
    const disposedBefore = all[0]!.query.mock.calls.length;
    deliver({ kind: "filesChanged", rescan: true, changes: [] });
    await vi.waitFor(() => expect(all[3]!.query.mock.calls.length).toBe(before + 1));
    expect(all[0]!.query.mock.calls.length).toBe(disposedBefore);
    all[3]!.controller.dispose();
    all[3]!.controller.dispose();
    await vi.waitFor(() => expect(ipc.workspaceOff).toHaveBeenCalledTimes(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(ipc.workspaceOff).toHaveBeenCalledTimes(1);
  });

  it("a controller created while the previous subscription is still being set up or torn down gets events", async () => {
    // The subscription resolves late.
    let resolve!: (off: () => void) => void;
    ipc.workspace.mockImplementationOnce(() => new Promise<() => void>((r) => (resolve = r)));
    const first = await make("one");
    first.controller.dispose(); // disposed before the subscription exists
    const second = await make("two"); // subscribes anew at once
    expect(ipc.workspace).toHaveBeenCalledTimes(2);
    resolve(ipc.workspaceOff);
    await vi.waitFor(() => expect(ipc.workspaceOff).toHaveBeenCalledTimes(1));
    const deliver = ipc.workspace.mock.calls[1]![0] as (event: unknown) => void;
    const before = second.query.mock.calls.length;
    deliver({ kind: "filesChanged", rescan: false, changes: [{ path: "img/a.PNG", kind: "created" }] });
    await vi.waitFor(() => expect(second.query.mock.calls.length).toBe(before + 1));
    second.controller.dispose();
    await vi.waitFor(() => expect(ipc.workspaceOff).toHaveBeenCalledTimes(2));
  });
});
