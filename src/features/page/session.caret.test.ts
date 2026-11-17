// The page caret offered to caret-based tools (the equation editor): always
// in the document's current revision, mapped over concurrent changes, and
// null — never an older revision's or a guessed offset — when it cannot be
// mapped or typing is paused after a refusal.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => {
  type Handler = (event: unknown) => void;
  const state = { text: "", revision: 1, docHandlers: [] as Handler[], buildHandlers: [] as Handler[], builds: 0 };
  const changed = (id: string, previousRevision: number, at: number, text: string) => ({
    kind: "changed",
    document: "doc-1",
    transaction: id,
    origin: { kind: "agent", taskId: "t" },
    change: "edit",
    previousRevision,
    revision: previousRevision + 1,
    patches: [{ range: { start: at, end: at }, text }],
    selection: null,
    canUndo: true,
    canRedo: false,
  });
  return {
    state,
    reset(text: string) {
      Object.assign(state, { text, revision: 1, docHandlers: [], buildHandlers: [], builds: 0 });
    },
    insert(id: string, at: number, text: string) {
      const event = changed(id, state.revision, at, text);
      state.text = state.text.slice(0, at) + text + state.text.slice(at);
      state.revision += 1;
      return event;
    },
    emitDoc: (event: unknown) => state.docHandlers.forEach((h) => h(event)),
    emitBuild() {
      state.builds += 1;
      const result = {
        id: `typst-${state.builds}`,
        target: { format: "typst", engine: "typst", entry: "main.typ" },
        snapshot: "s",
        sources: [{ path: "main.typ", document: "doc-1", revision: state.revision, contentHash: "" }],
        configuration: null,
        status: { kind: "succeeded" },
        diagnostics: [],
        pages: [],
        mapping: "glyph",
        exports: [],
        durationMs: 1,
      };
      state.buildHandlers.forEach((h) => h({ result, timings: {}, lastGood: result, refusal: null }));
    },
  };
});

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: backend.state.revision }, text: backend.state.text }),
    typesettingWatch: async () => null,
    typesettingUnwatch: async () => {},
    pageImage: async () => new ArrayBuffer(4),
    pageCaret: async () => null,
    pageSelection: async () => ({ rects: [], text: "" }),
    pageEdit: async (request: { baseRevision: number }) =>
      request.baseRevision !== backend.state.revision
        ? { kind: "stale", currentRevision: backend.state.revision }
        : { kind: "unchanged", revision: backend.state.revision, caret: 0 },
  },
  events: {
    documents: async (handler: (e: unknown) => void) => {
      backend.state.docHandlers.push(handler);
      return () => {};
    },
    typesettingBuild: async (handler: (e: unknown) => void) => {
      backend.state.buildHandlers.push(handler);
      return () => {};
    },
    trustChanged: async () => () => {},
  },
  errorMessage: (e: unknown) => String(e),
  isCommandError: () => false,
  newId: (() => {
    let n = 0;
    return (prefix: string) => `${prefix}-${++n}`;
  })(),
}));
vi.mock("../editor", () => ({
  openDocument: async () => ({ id: "doc-1", editReason: null, subscribe: () => () => {} }),
}));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: async () => false }));

import { lastPageSurface, notePageFocus, releasePageSurface } from "./activePage";
import { PageSession } from "./session";

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("PageSession.currentCaret", () => {
  let session: PageSession;

  beforeEach(async () => {
    backend.reset("Hello world.");
    session = new PageSession("main.typ");
    await session.attach();
    backend.emitBuild();
    await flush();
  });

  afterEach(() => session.detach());

  it("is null before the caret is placed", () => {
    expect(session.currentCaret()).toBeNull();
  });

  it("gives the caret in the current revision, mapped over a concurrent change", async () => {
    await session.reveal(5);
    expect(session.currentCaret()).toEqual({ offset: 5, anchor: null, revision: 1, mode: null });
    backend.emitDoc(backend.insert("agent-1", 0, "XY"));
    expect(session.currentCaret()).toEqual({ offset: 7, anchor: null, revision: 2, mode: null });
  });

  it("is null when a change it must be mapped over was not received", async () => {
    await session.reveal(5);
    backend.insert("agent-lost", 0, "Z"); // revision 2: its event never arrives
    backend.emitDoc(backend.insert("agent-2", 0, "W")); // revision 3 arrives
    expect(session.currentCaret()).toBeNull();
  });
});

describe("page surface registry", () => {
  it("returns the most recently focused page view of a document", () => {
    const a = { path: "main.typ", currentCaret: () => null, focus: () => {} };
    const b = { path: "main.typ", currentCaret: () => null, focus: () => {} };
    const other = { path: "other.typ", currentCaret: () => null, focus: () => {} };
    notePageFocus(a);
    notePageFocus(other);
    notePageFocus(b);
    expect(lastPageSurface("main.typ")?.surface).toBe(b);
    notePageFocus(a);
    expect(lastPageSurface("main.typ")?.surface).toBe(a);
    releasePageSurface(a);
    expect(lastPageSurface("main.typ")?.surface).toBe(b);
    releasePageSurface(b);
    releasePageSurface(other);
    expect(lastPageSurface("main.typ")).toBeNull();
  });
});
