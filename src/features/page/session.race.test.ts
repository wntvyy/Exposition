// Page typing racing concurrent (agent) edits: keystrokes typed on the page are
// never silently dropped. A fake document service stands in for the Rust side
// and delivers document/build events in the orders the real app produced in
// the M6 acceptance run (D2).
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => {
  type Handler = (event: unknown) => void;
  const state = {
    text: "",
    revision: 1,
    docHandlers: [] as Handler[],
    buildHandlers: [] as Handler[],
    builds: 0,
    /** Called inside pageEdit after the edit applied, before the reply resolves. */
    afterApply: null as null | (() => void),
  };
  const emitDoc = (event: unknown) => state.docHandlers.forEach((h) => h(event));
  const changed = (id: string, origin: unknown, previousRevision: number, at: number, text: string) => ({
    kind: "changed",
    document: "doc-1",
    transaction: id,
    origin,
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
      Object.assign(state, { text, revision: 1, docHandlers: [], buildHandlers: [], builds: 0, afterApply: null });
    },
    /** Apply an insertion at the current revision and return its change event (not yet delivered). */
    insert(id: string, origin: unknown, at: number, text: string) {
      const event = changed(id, origin, state.revision, at, text);
      state.text = state.text.slice(0, at) + text + state.text.slice(at);
      state.revision += 1;
      return event;
    },
    emitDoc,
    /** A successful build of the current revision. */
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
      const event = { result, timings: {}, lastGood: result, refusal: null };
      state.buildHandlers.forEach((h) => h(event));
    },
  };
});

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: backend.state.revision }, text: backend.state.text }),
    typesettingWatch: async () => null,
    typesettingUnwatch: async () => {},
    typesettingBuild: async () => {
      throw new Error("not used");
    },
    pageImage: async () => new ArrayBuffer(4),
    pageCaret: async () => null,
    pageSelection: async () => ({ rects: [], text: "" }),
    pageEdit: async (request: {
      id: string;
      baseRevision: number;
      selection: { start: number; end: number };
      op: { kind: string; text?: string };
    }) => {
      // As in Rust: an edit based on an older revision whose text is not
      // retained cannot be planned; the caller must resynchronise.
      if (request.baseRevision !== backend.state.revision) {
        return { kind: "stale", currentRevision: backend.state.revision };
      }
      const text = request.op.text ?? "";
      const at = request.selection.end;
      const event = backend.insert(request.id, { kind: "page" }, at, text);
      backend.emitDoc(event);
      const reply = { kind: "applied", revision: backend.state.revision, caret: at + text.length, mode: "markup", rebased: false };
      const hook = backend.state.afterApply;
      backend.state.afterApply = null;
      hook?.();
      return reply;
    },
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

import { PageSession } from "./session";

const flush = () => new Promise((r) => setTimeout(r, 0));

async function typeKeys(session: PageSession, keys: string) {
  await Promise.all([...keys].map((ch) => session.edit({ kind: "insert", text: ch })));
}

describe("page typing racing concurrent edits (M6 D2)", () => {
  let session: PageSession;

  beforeEach(async () => {
    backend.reset("Hello world.");
    session = new PageSession("main.typ");
    await session.attach();
    backend.emitBuild();
    await flush();
    await session.reveal(5); // caret after "Hello"
  });

  afterEach(() => session.detach());

  it("keeps typing when an agent edit's event arrives before the page edit's reply and a build is shown", async () => {
    // The real-app order: the page edit applies; an agent edit lands before
    // the caret; both change events reach the view before the page edit's
    // reply; then a build of the newest revision is shown.
    backend.state.afterApply = () => backend.emitDoc(backend.insert("agent-1", { kind: "agent", taskId: "t" }, 0, "X"));
    await session.edit({ kind: "insert", text: "a" });
    backend.emitBuild();
    await flush();
    await typeKeys(session, "b c");
    expect(backend.state.text).toBe("XHelloab c world.");
    expect(session.getModel().message).toBeNull();
  });

  it("waits for a concurrent edit's event instead of dropping keystrokes", async () => {
    // An agent edit lands in the service but its event is delayed: the page's
    // next edits are stale until the event arrives (after the old retry window).
    await typeKeys(session, "a");
    const late = backend.insert("agent-2", { kind: "agent", taskId: "t" }, 0, "Y");
    setTimeout(() => {
      backend.emitDoc(late);
      backend.emitBuild();
    }, 150);
    await typeKeys(session, "bcd");
    expect(backend.state.text).toBe("YHelloabcd world.");
    expect(session.getModel().message).toBeNull();
  });

  it("refuses explicitly, keeping the unapplied text, when the caret cannot be mapped", async () => {
    // The concurrent change's event never arrives (e.g. events were lost):
    // the keystrokes must not be applied at a guessed position, and the user
    // is told which text was not applied.
    await typeKeys(session, "a");
    backend.insert("agent-3", { kind: "agent", taskId: "t" }, 0, "Z");
    await typeKeys(session, "bc");
    expect(backend.state.text).toBe("ZHelloa world.");
    const message = session.getModel().message;
    expect(message?.tone).toBe("warning");
    expect(message?.text).toContain("“bc”");
  }, 15000);
});
