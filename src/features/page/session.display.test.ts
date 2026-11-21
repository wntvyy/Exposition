// What a page view asks the backend for: only the images of the pages it
// displays, and nothing at all while its tab is hidden. A fake backend counts
// the watch, unwatch, page-image and geometry requests.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const backend = vi.hoisted(() => {
  type Handler = (event: unknown) => void;
  const state = {
    revision: 1,
    docHandlers: [] as Handler[],
    buildHandlers: [] as Handler[],
    builds: 0,
    watches: 0,
    unwatches: 0,
    /** Render references requested through `pageImage`, in order. */
    images: [] as string[],
    carets: 0,
    /** The latest retained build, returned by `typesettingWatch`. */
    latest: null as unknown,
    /** Suffix of the render references of pages from this index on (a reflow). */
    changedFrom: Number.POSITIVE_INFINITY,
    generation: 0,
  };
  const buildEvent = (pages: number) => {
    state.builds += 1;
    const result = {
      id: `typst-${state.builds}`,
      target: { format: "typst", engine: "typst", entry: "main.typ" },
      snapshot: "s",
      sources: [{ path: "main.typ", document: "doc-1", revision: state.revision, contentHash: "" }],
      configuration: null,
      status: { kind: "succeeded" },
      diagnostics: [],
      pages: Array.from({ length: pages }, (_, index) => ({
        index,
        widthPt: 400,
        heightPt: 500,
        renderRef: index >= state.changedFrom ? `page${index}g${state.generation}` : `page${index}`,
      })),
      mapping: "glyph",
      exports: [],
      durationMs: 1,
    };
    return { result, timings: {}, lastGood: result, refusal: null };
  };
  return {
    state,
    reset() {
      Object.assign(state, {
        revision: 1,
        docHandlers: [],
        buildHandlers: [],
        builds: 0,
        watches: 0,
        unwatches: 0,
        images: [],
        carets: 0,
        latest: null,
        changedFrom: Number.POSITIVE_INFINITY,
        generation: 0,
      });
    },
    /** A successful build of the current revision with `pages` pages, delivered as an event. */
    emitBuild(pages: number) {
      const event = buildEvent(pages);
      state.latest = event;
      state.buildHandlers.forEach((h) => h(event));
    },
    /** A build that finished while nobody listened: only retained. */
    retainBuild(pages: number) {
      state.latest = buildEvent(pages);
    },
    /** The document changed (another view or an agent edited it). */
    emitChange() {
      const previous = state.revision;
      state.revision += 1;
      const event = {
        kind: "changed",
        document: "doc-1",
        transaction: `t-${state.revision}`,
        origin: { kind: "source" },
        change: "edit",
        previousRevision: previous,
        revision: state.revision,
        patches: [{ range: { start: 0, end: 0 }, text: "x" }],
        selection: null,
        canUndo: true,
        canRedo: false,
      };
      state.docHandlers.forEach((h) => h(event));
    },
  };
});

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: backend.state.revision }, text: "" }),
    typesettingWatch: async () => {
      backend.state.watches += 1;
      return backend.state.latest;
    },
    typesettingUnwatch: async () => {
      backend.state.unwatches += 1;
    },
    typesettingBuild: async () => {
      throw new Error("not used");
    },
    pageImage: async (renderRef: string) => {
      backend.state.images.push(renderRef);
      return new ArrayBuffer(4);
    },
    pageCaret: async () => {
      backend.state.carets += 1;
      return null;
    },
    pageSelection: async () => ({ rects: [], text: "" }),
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
  newId: (prefix: string) => `${prefix}-1`,
}));
vi.mock("../editor", () => ({
  openDocument: async () => ({ id: "doc-1", editReason: null, subscribe: () => () => {} }),
}));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: async () => false }));

import { IDLE_PAGE_IMAGES, PageSession } from "./session";

const flush = () => new Promise((r) => setTimeout(r, 0));
const loaded = (session: PageSession) =>
  session
    .getModel()
    .pages.filter((p) => p.url !== null)
    .map((p) => p.index);

describe("page images follow the displayed pages", () => {
  let session: PageSession;
  let created: string[];
  let revoked: string[];

  beforeEach(() => {
    backend.reset();
    created = [];
    revoked = [];
    let n = 0;
    vi.spyOn(URL, "createObjectURL").mockImplementation(() => {
      const url = `blob:test-${++n}`;
      created.push(url);
      return url;
    });
    vi.spyOn(URL, "revokeObjectURL").mockImplementation((url) => void revoked.push(url));
    session = new PageSession("main.typ");
  });

  afterEach(() => {
    session.detach();
    vi.restoreAllMocks();
  });

  it("requests the first pages of a long build, not every page, and keeps a box for each page", async () => {
    await session.attach();
    backend.emitBuild(200);
    await flush();
    const model = session.getModel();
    expect(model.pages).toHaveLength(200);
    expect(model.pages.every((p) => p.widthPt === 400 && p.heightPt === 500)).toBe(true);
    expect(backend.state.images).toEqual(["page0", "page1", "page2"]);
    expect(loaded(session)).toEqual([0, 1, 2]);
  });

  it("requests a page when it is displayed, once, and nothing for a build that leaves the displayed pages unchanged", async () => {
    await session.attach();
    backend.emitBuild(200);
    await flush();
    session.setDisplayedPages([119, 120, 121, 122]);
    await flush();
    expect(backend.state.images.slice(3)).toEqual(["page119", "page120", "page121", "page122"]);
    expect(loaded(session)).toEqual([0, 1, 2, 119, 120, 121, 122]);
    // The same pages reported again (a scroll within them) request nothing.
    session.setDisplayedPages([119, 120, 121, 122]);
    await flush();
    expect(backend.state.images).toHaveLength(7);
    // A reflow from page 150 on: the displayed pages are unchanged.
    backend.state.changedFrom = 150;
    backend.state.generation = 1;
    backend.emitChange();
    backend.emitBuild(200);
    await flush();
    expect(backend.state.images).toHaveLength(7);
    expect(session.getModel().stale).toBe(false);
    // A reflow that changes the displayed pages requests exactly those.
    backend.state.changedFrom = 100;
    backend.state.generation = 2;
    backend.emitChange();
    backend.emitBuild(200);
    await flush();
    expect(backend.state.images.slice(7)).toEqual(["page119g2", "page120g2", "page121g2", "page122g2"]);
    expect(session.getModel().pages[120]!.renderRef).toBe("page120g2");
    expect(session.getModel().pages[120]!.url).not.toBeNull();
  });

  it("shows a new build only when the displayed pages are loaded, and never an image of another reference", async () => {
    await session.attach();
    backend.emitBuild(10);
    await flush();
    const before = session.getModel().pages[1]!.url;
    backend.state.changedFrom = 0;
    backend.state.generation = 1;
    backend.emitChange();
    backend.emitBuild(10);
    // Not swapped yet: the previous build stays on screen, marked stale.
    expect(session.getModel().buildId).toBe("typst-1");
    expect(session.getModel().stale).toBe(true);
    await flush();
    const model = session.getModel();
    expect(model.buildId).toBe("typst-2");
    expect(model.pages[1]!.renderRef).toBe("page1g1");
    expect(model.pages[1]!.url).not.toBe(before);
    // Pages that are not displayed carry no image of the previous build.
    expect(model.pages.slice(3).every((p) => p.url === null)).toBe(true);
    // The previous build's images were released.
    expect(revoked).toContain(before);
  });

  it("keeps a bounded number of images while scrolling through a long document and releases all on detach", async () => {
    await session.attach();
    backend.emitBuild(200);
    await flush();
    for (let top = 0; top + 3 <= 200; top += 3) {
      session.setDisplayedPages([top, top + 1, top + 2]);
      await flush();
    }
    const held = loaded(session).length;
    expect(held).toBeLessThanOrEqual(3 + IDLE_PAGE_IMAGES);
    expect(created.length - revoked.length).toBe(held);
    // Every page was requested exactly once on the way.
    expect(new Set(backend.state.images).size).toBe(backend.state.images.length);
    session.detach();
    expect(revoked).toHaveLength(created.length);
  });

  it("drops an image that arrives after the view was closed", async () => {
    await session.attach();
    backend.emitBuild(5);
    session.detach();
    await flush();
    expect(created.length).toBe(revoked.length);
  });
});

describe("a hidden page view does no work", () => {
  let session: PageSession;

  beforeEach(() => {
    backend.reset();
    vi.spyOn(URL, "createObjectURL").mockImplementation(() => "blob:test");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    session = new PageSession("main.typ");
  });

  afterEach(() => {
    session.detach();
    vi.restoreAllMocks();
  });

  it("does not watch its entry when it is attached hidden", async () => {
    session.setVisible(false);
    await session.attach();
    backend.emitBuild(5);
    await flush();
    expect(backend.state.watches).toBe(0);
    expect(backend.state.images).toEqual([]);
    expect(session.getModel().pages).toEqual([]);
  });

  it("stops watching when hidden and ignores builds, changes cost no requests", async () => {
    await session.attach();
    backend.emitBuild(5);
    await flush();
    await session.reveal(0);
    expect(backend.state.watches).toBe(1);
    const images = backend.state.images.length;
    const carets = backend.state.carets;
    session.setVisible(false);
    expect(backend.state.unwatches).toBe(1);
    // Another view of the same entry keeps building it; this one does nothing.
    backend.state.changedFrom = 0;
    backend.state.generation = 1;
    for (let i = 0; i < 5; i++) {
      backend.emitChange();
      backend.emitBuild(5);
    }
    await flush();
    expect(backend.state.images).toHaveLength(images);
    expect(backend.state.carets).toBe(carets);
    expect(session.getModel().buildId).toBe("typst-1");
    // The pages it still holds are older than the document: marked stale.
    expect(session.getModel().stale).toBe(true);
  });

  it("shown again, watches once, stays stale until a build result arrives, then shows the current build", async () => {
    await session.attach();
    backend.emitBuild(5);
    await flush();
    expect(session.getModel().stale).toBe(false);
    session.setVisible(false);
    // A file the document includes changed while it was hidden (its own
    // revision did not), and nobody built it.
    session.setVisible(true);
    await flush();
    expect(backend.state.watches).toBe(2);
    expect(backend.state.unwatches).toBe(1);
    // The retained build is shown, but not presented as current.
    expect(session.getModel().buildId).toBe("typst-1");
    expect(session.getModel().stale).toBe(true);
    backend.state.changedFrom = 0;
    backend.state.generation = 1;
    backend.emitBuild(5);
    await flush();
    const model = session.getModel();
    expect(model.buildId).toBe("typst-2");
    expect(model.stale).toBe(false);
    expect(model.pages[0]!.renderRef).toBe("page0g1");
    expect(model.pages[0]!.url).not.toBeNull();
  });

  it("shown again after the document changed, shows the retained newer build as stale until its own build", async () => {
    await session.attach();
    backend.emitBuild(5);
    await flush();
    session.setVisible(false);
    backend.emitChange();
    backend.emitChange();
    // Built for another watcher while this view was hidden: retained, for an
    // older revision than the document has now.
    backend.state.revision -= 1;
    backend.retainBuild(5);
    backend.state.revision += 1;
    session.setVisible(true);
    await flush();
    expect(session.getModel().buildId).toBe("typst-2");
    expect(session.getModel().stale).toBe(true);
    backend.emitBuild(5);
    await flush();
    expect(session.getModel().buildId).toBe("typst-3");
    expect(session.getModel().buildRevision).toBe(3);
    expect(session.getModel().stale).toBe(false);
  });

  it("a quick hide and show while the watch is pending leaves exactly one watch", async () => {
    const attaching = session.attach();
    session.setVisible(false);
    session.setVisible(true);
    await attaching;
    await flush();
    expect(backend.state.watches - backend.state.unwatches).toBe(1);
    session.setVisible(false);
    session.setVisible(true);
    session.setVisible(false);
    await flush();
    expect(backend.state.watches - backend.state.unwatches).toBe(0);
  });
});
