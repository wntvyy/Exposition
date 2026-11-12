// A LaTeX page view refused because its workspace is not trusted leaves that
// state as soon as the folder is trusted anywhere (another view, the Research
// or Jobs panel): it requests its build again (M6 acceptance 1b).
import { afterEach, describe, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => ({
  trusted: false,
  watches: 0,
  unwatches: 0,
  buildHandlers: [] as Array<(event: unknown) => void>,
  trustListeners: new Set<(root: string) => void>(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: 1 }, text: "\\documentclass{article}" }),
    typesettingWatch: async () => {
      backend.watches += 1;
      return null;
    },
    typesettingUnwatch: async () => {
      backend.unwatches += 1;
    },
    trustStatus: async (root: string) => ({ root, trusted: backend.trusted }),
    pageImage: async () => new ArrayBuffer(4),
    pageCaret: async () => null,
  },
  events: {
    documents: async () => () => {},
    typesettingBuild: async (handler: (event: unknown) => void) => {
      backend.buildHandlers.push(handler);
      return () => {};
    },
  },
  errorMessage: (e: unknown) => String(e),
  isCommandError: () => false,
  newId: (prefix: string) => `${prefix}-1`,
}));
vi.mock("../editor", () => ({ openDocument: async () => ({ id: "doc-1" }) }));
vi.mock("../jobs/trust", () => ({
  // The view's own prompt is declined; trust comes from elsewhere.
  requestWorkspaceTrust: async () => false,
  onWorkspaceTrusted: (listener: (root: string) => void) => {
    backend.trustListeners.add(listener);
    return () => backend.trustListeners.delete(listener);
  },
}));

import { PageSession } from "./session";

const flush = () => new Promise((r) => setTimeout(r, 0));
const ROOT = "C:/papers/thesis";

function refusedBuild() {
  const result = {
    id: "tex-1",
    target: { format: "latex", engine: "pdfLatex", entry: "main.tex" },
    snapshot: "",
    sources: [],
    configuration: null,
    status: { kind: "error", message: "not trusted" },
    diagnostics: [],
    pages: [],
    mapping: "none",
    exports: [],
    durationMs: 0,
  };
  return {
    result,
    timings: {},
    lastGood: null,
    refusal: { code: "untrustedWorkspace", message: "not trusted", workspaceRoot: ROOT },
  };
}

describe("a page view refused for an untrusted workspace", () => {
  let session: PageSession | null = null;
  afterEach(() => session?.detach());

  it("builds again when the folder is trusted from another surface", async () => {
    session = new PageSession("main.tex");
    await session.attach();
    backend.buildHandlers.forEach((h) => h(refusedBuild()));
    await flush();
    expect(session.getModel().untrustedRoot).toBe(ROOT);
    const watches = backend.watches;

    // Another folder was trusted: still untrusted, nothing requested.
    backend.trustListeners.forEach((l) => l("C:/elsewhere"));
    await flush();
    expect(session.getModel().untrustedRoot).toBe(ROOT);
    expect(backend.watches).toBe(watches);

    // This folder was trusted from the Research panel.
    backend.trusted = true;
    backend.trustListeners.forEach((l) => l(ROOT));
    await flush();
    await flush();
    expect(session.getModel().untrustedRoot).toBeNull();
    expect(backend.watches).toBe(watches + 1);
    // The build was requested without adding a watch for this view.
    expect(backend.unwatches).toBe(1);
  });
});
