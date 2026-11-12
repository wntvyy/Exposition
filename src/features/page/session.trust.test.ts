// Trust changes reach a LaTeX page view through one mechanism: the app's
// trust-change forwarder, which rebuilds the watched LaTeX entries and
// announces the change. A grant (through this view's prompt or any other
// surface) therefore leads to exactly one build request, and the view's
// refusal clears on a grant and comes back on a revoke.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ROOT = "C:/papers/thesis";

const backend = vi.hoisted(() => {
  type Handler = (event: unknown) => void;
  const state = {
    trusted: false,
    /** Build requests: a view's watch, or the app rebuilding watched entries. */
    buildRequests: 0,
    buildHandlers: [] as Handler[],
    trustHandlers: [] as Handler[],
    builds: 0,
  };
  const event = (refused: boolean) => {
    state.builds += 1;
    const result = {
      id: `tex-${state.builds}`,
      target: { format: "latex", engine: "pdfLatex", entry: "main.tex" },
      snapshot: "",
      // A refused build compiled nothing: it has no source revisions.
      sources: refused ? [] : [{ path: "main.tex", document: "doc-1", revision: 1, contentHash: "" }],
      configuration: null,
      status: refused ? { kind: "error", message: "not trusted" } : { kind: "succeeded" },
      diagnostics: [],
      pages: [],
      mapping: "glyph",
      exports: [],
      durationMs: 1,
    };
    const refusal = refused
      ? { code: "untrustedWorkspace", message: "not trusted", workspaceRoot: "C:/papers/thesis" }
      : null;
    return { result, timings: {}, lastGood: refused ? null : result, refusal };
  };
  return {
    state,
    reset() {
      Object.assign(state, { trusted: false, buildRequests: 0, buildHandlers: [], trustHandlers: [], builds: 0 });
    },
    /** Deliver the build the backend runs now (refused unless trusted). */
    finishBuild() {
      const e = event(!state.trusted);
      state.buildHandlers.forEach((h) => h(e));
    },
    /** A grant or revoke from any surface, as `forward_trust` handles it. */
    changeTrust(granted: boolean) {
      state.trusted = granted;
      state.buildRequests += 1; // tex.rebuild_watched()
      state.trustHandlers.forEach((h) => h({ root: "C:/papers/thesis", granted }));
    },
  };
});

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: 1 }, text: "\\documentclass{article}" }),
    typesettingWatch: async () => {
      backend.state.buildRequests += 1; // a watch requests a draft build
      return null;
    },
    typesettingUnwatch: async () => {},
    trustStatus: async (root: string) => ({ root, trusted: backend.state.trusted }),
    trustGrant: async (root: string) => {
      backend.changeTrust(true);
      return { root, trusted: true };
    },
    pageImage: async () => new ArrayBuffer(4),
    pageCaret: async () => null,
  },
  events: {
    documents: async () => () => {},
    typesettingBuild: async (handler: (e: unknown) => void) => {
      backend.state.buildHandlers.push(handler);
      return () => {};
    },
    trustChanged: async (handler: (e: unknown) => void) => {
      backend.state.trustHandlers.push(handler);
      return () => {};
    },
  },
  errorMessage: (e: unknown) => String(e),
  isCommandError: () => false,
  newId: (prefix: string) => `${prefix}-1`,
}));
vi.mock("../editor", () => ({ openDocument: async () => ({ id: "doc-1" }) }));
const prompt = vi.hoisted(() => ({ confirm: vi.fn(async () => "trust") }));
vi.mock("../../ui/components", () => ({ confirm: prompt.confirm }));
vi.mock("../../app/notifications", () => ({ notify: vi.fn(), dismiss: vi.fn() }));

import { PageSession } from "./session";

const flush = async () => {
  for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
};

describe("a LaTeX page view and workspace trust", () => {
  let session: PageSession;

  beforeEach(async () => {
    backend.reset();
    prompt.confirm.mockClear();
    session = new PageSession("main.tex");
    await session.attach();
    expect(backend.state.buildRequests).toBe(1); // the view's watch
  });

  afterEach(() => session.detach());

  it("a grant through the view's prompt leads to exactly one build request", async () => {
    // The first build is refused: the view prompts (no pages yet) and the
    // user trusts the folder.
    backend.finishBuild();
    await flush();
    expect(prompt.confirm).toHaveBeenCalledTimes(1);
    expect(backend.state.buildRequests).toBe(2); // only the app's rebuild
    expect(session.getModel().untrustedRoot).toBeNull();
    backend.finishBuild();
    await flush();
    expect(session.getModel().untrustedRoot).toBeNull();
    expect(backend.state.buildRequests).toBe(2);
  });

  it("follows grants and revokes from other surfaces without requesting builds itself", async () => {
    prompt.confirm.mockResolvedValueOnce("cancel");
    backend.finishBuild();
    await flush();
    expect(session.getModel().untrustedRoot).toBe(ROOT);
    const requests = backend.state.buildRequests;

    // Trusted from the Research panel / Settings / Lean: the refusal clears.
    backend.changeTrust(true);
    await flush();
    expect(session.getModel().untrustedRoot).toBeNull();
    expect(backend.state.buildRequests).toBe(requests + 1);
    backend.finishBuild();
    await flush();

    // Revoked: the app's rebuild is refused and the view says so again,
    // without prompting (it shows pages).
    backend.changeTrust(false);
    backend.finishBuild();
    await flush();
    expect(session.getModel().untrustedRoot).toBe(ROOT);
    expect(prompt.confirm).toHaveBeenCalledTimes(1);
    expect(backend.state.buildRequests).toBe(requests + 2);

    // "Trust workspace…" on the banner: one grant, one build request.
    await session.trust();
    await flush();
    expect(backend.state.buildRequests).toBe(requests + 3);
    expect(session.getModel().untrustedRoot).toBeNull();
  });

  it("requests its build once when the folder turns out to be trusted already", async () => {
    prompt.confirm.mockResolvedValueOnce("cancel");
    backend.finishBuild();
    await flush();
    const requests = backend.state.buildRequests;
    backend.state.trusted = true; // trusted without an announcement reaching this view
    await session.trust();
    await flush();
    expect(backend.state.buildRequests).toBe(requests + 1);
    expect(session.getModel().untrustedRoot).toBeNull();
  });
});
