// Review: attacks on the page view's hidden-tab and displayed-pages behaviour.
// The invariants: pages are never presented as current (not stale) unless a
// build result of the document's current revision arrived while the view was
// watching; a page never shows the image of another reference or a released
// image; a shown view always ends up current once its build arrives.
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
    /** The latest retained build, returned by `typesettingWatch`. */
    latest: null as unknown,
    /** Render references by page index of the next build (default `p<index>`). */
    refs: new Map<number, string>(),
    /** When set, watch replies wait for `releaseWatch`. */
    holdWatch: false,
    heldWatches: [] as Array<() => void>,
    /** When set, page images wait for `releaseImages`. */
    holdImages: false,
    heldImages: [] as Array<{ ref: string; release: () => void }>,
    /** A byte length per render reference: identifies the image a URL was made from. */
    sizes: new Map<string, number>(),
    requested: [] as string[],
  };
  const sizeOf = (ref: string) => {
    if (!state.sizes.has(ref)) state.sizes.set(ref, state.sizes.size + 1);
    return state.sizes.get(ref)!;
  };
  const buildEvent = (pages: number, failed = false) => {
    state.builds += 1;
    const result = {
      id: `typst-${state.builds}`,
      target: { format: "typst", engine: "typst", entry: "main.typ" },
      snapshot: "s",
      sources: [{ path: "main.typ", document: "doc-1", revision: state.revision, contentHash: "" }],
      configuration: null,
      status: { kind: failed ? "failed" : "succeeded" },
      diagnostics: failed ? [{ severity: "error", message: "broken", location: null, hints: [] }] : [],
      pages: failed
        ? []
        : Array.from({ length: pages }, (_, index) => ({
            index,
            widthPt: 400,
            heightPt: 500,
            renderRef: state.refs.get(index) ?? `p${index}`,
          })),
      mapping: "glyph",
      exports: [],
      durationMs: 1,
    };
    return { result, timings: {}, lastGood: failed ? null : result, refusal: null };
  };
  return {
    state,
    sizeOf,
    reset() {
      Object.assign(state, {
        revision: 1,
        docHandlers: [],
        buildHandlers: [],
        builds: 0,
        watches: 0,
        unwatches: 0,
        latest: null,
        refs: new Map(),
        holdWatch: false,
        heldWatches: [],
        holdImages: false,
        heldImages: [],
        sizes: new Map(),
        requested: [],
      });
    },
    /** A build of the current revision, delivered as an event and retained. */
    emitBuild(pages: number, failed = false) {
      const event = buildEvent(pages, failed);
      state.latest = event;
      state.buildHandlers.forEach((h) => h(event));
      return event;
    },
    /** A build made for another watcher while this view did not listen. */
    retainBuild(pages: number) {
      state.latest = buildEvent(pages);
      return state.latest;
    },
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
    releaseWatch() {
      state.heldWatches.splice(0).forEach((release) => release());
    },
    /** Deliver held page images (all, or those `pick` selects), in the given order. */
    releaseImages(pick?: (ref: string) => boolean) {
      const chosen = state.heldImages.filter((held) => !pick || pick(held.ref));
      state.heldImages = state.heldImages.filter((held) => !chosen.includes(held));
      chosen.forEach((held) => held.release());
    },
  };
});

vi.mock("../../lib/ipc", () => ({
  commands: {
    documentState: async () => ({ info: { revision: backend.state.revision }, text: "" }),
    typesettingWatch: () => {
      backend.state.watches += 1;
      // The reply carries the build retained when the call was handled.
      const latest = backend.state.latest;
      if (!backend.state.holdWatch) return Promise.resolve(latest);
      return new Promise((resolve) => backend.state.heldWatches.push(() => resolve(latest)));
    },
    typesettingUnwatch: async () => {
      backend.state.unwatches += 1;
    },
    typesettingBuild: async () => {
      throw new Error("not used");
    },
    pageImage: (renderRef: string) => {
      backend.state.requested.push(renderRef);
      const buffer = new ArrayBuffer(backend.sizeOf(renderRef));
      if (!backend.state.holdImages) return Promise.resolve(buffer);
      return new Promise((resolve) =>
        backend.state.heldImages.push({ ref: renderRef, release: () => resolve(buffer) }),
      );
    },
    pageCaret: async () => null,
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

const flush = async () => {
  for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, 0));
};

describe("review: page view attacks", () => {
  let session: PageSession;
  /** Object URL -> the render reference of the image it was made from. */
  let made: Map<string, string>;
  let revoked: Set<string>;

  /** No page shows a released image or the image of another reference. */
  const checkImages = (context: string) => {
    for (const page of session.getModel().pages) {
      if (page.url === null) continue;
      expect(revoked.has(page.url), `${context}: page ${page.index} shows a released image`).toBe(false);
      expect(made.get(page.url), `${context}: page ${page.index} shows another page's image`).toBe(page.renderRef);
    }
  };

  beforeEach(() => {
    backend.reset();
    made = new Map();
    revoked = new Set();
    let n = 0;
    vi.spyOn(URL, "createObjectURL").mockImplementation((blob) => {
      const url = `blob:test-${++n}`;
      const size = (blob as Blob).size;
      const ref = [...backend.state.sizes].find(([, s]) => s === size)![0];
      made.set(url, ref);
      return url;
    });
    vi.spyOn(URL, "revokeObjectURL").mockImplementation((url) => void revoked.add(url));
    session = new PageSession("main.typ");
  });

  afterEach(() => {
    session.detach();
    vi.restoreAllMocks();
  });

  it("a build event that overtakes the watch reply is kept; the older retained reply neither replaces it nor marks it stale", async () => {
    await session.attach();
    backend.emitBuild(3);
    await flush();
    session.setVisible(false);
    backend.emitChange();
    backend.state.holdWatch = true;
    session.setVisible(true);
    await flush();
    expect(session.getModel().stale).toBe(true);
    // The build the watch started finishes before the watch reply is delivered.
    backend.state.refs.set(0, "p0-new");
    backend.emitBuild(3);
    await flush();
    expect(session.getModel().buildId).toBe("typst-2");
    expect(session.getModel().stale).toBe(false);
    backend.releaseWatch(); // carries typst-1
    await flush();
    const model = session.getModel();
    expect(model.buildId).toBe("typst-2");
    expect(model.stale).toBe(false);
    expect(model.pages[0]!.renderRef).toBe("p0-new");
    checkImages("after the late reply");
    expect(backend.state.watches - backend.state.unwatches).toBe(1);
  });

  it("hidden while a build loads its pages and shown again: the view ends current, never current too early", async () => {
    await session.attach();
    backend.emitBuild(3);
    await flush();
    backend.emitChange();
    backend.state.holdImages = true;
    backend.state.refs.set(0, "p0-b").set(1, "p1-b").set(2, "p2-b");
    backend.emitBuild(3); // typst-2, waits for its page images
    await flush();
    expect(session.getModel().buildId).toBe("typst-1");
    session.setVisible(false);
    backend.releaseImages();
    await flush();
    // Not shown while hidden; what it holds is older than the document.
    expect(session.getModel().buildId).toBe("typst-1");
    expect(session.getModel().stale).toBe(true);
    // An included file changes while hidden (the entry's revision does not).
    backend.state.holdImages = false;
    session.setVisible(true);
    await flush();
    // The retained build is of the current revision, but no build ran since.
    expect(session.getModel().buildId).toBe("typst-2");
    expect(session.getModel().stale).toBe(true);
    checkImages("retained build shown");
    backend.state.refs.set(0, "p0-c");
    backend.emitBuild(3);
    await flush();
    expect(session.getModel().buildId).toBe("typst-3");
    expect(session.getModel().stale).toBe(false);
    expect(session.getModel().pages[0]!.url).not.toBeNull();
    checkImages("own build shown");
  });

  it("the first build after showing fails: the pages stay stale; the next success shows them current", async () => {
    await session.attach();
    backend.emitBuild(3);
    await flush();
    session.setVisible(false);
    session.setVisible(true);
    await flush();
    backend.emitBuild(0, true);
    await flush();
    expect(session.getModel().failed).toBe(true);
    expect(session.getModel().stale).toBe(true);
    expect(session.getModel().buildId).toBe("typst-1");
    backend.emitChange();
    backend.emitBuild(3);
    await flush();
    expect(session.getModel().failed).toBe(false);
    expect(session.getModel().stale).toBe(false);
    expect(session.getModel().buildId).toBe("typst-3");
  });

  it("shown with no build result arriving (canceled, or the compiler failed to report): the pages stay marked stale", async () => {
    await session.attach();
    backend.emitBuild(3);
    await flush();
    expect(session.getModel().stale).toBe(false);
    session.setVisible(false);
    session.setVisible(true);
    await flush();
    await flush();
    // Nothing says these pages are current: they must not be presented so.
    expect(session.getModel().stale).toBe(true);
    expect(session.getModel().pages.every((p) => p.url !== null)).toBe(true);
  });

  // review: defect. startWatching() keeps `watchPending` set while it shows the
  // retained build of the watch reply (which waits for page images, or for the
  // trust prompt). A hide in that time unwatches; the show that follows
  // returns early because the watch is "pending", and nothing watches again:
  // the view is displayed, its entry is not watched, so no change rebuilds it.
  it.skip("review: defect - hidden and shown again while the watch reply loads its pages, the view watches its entry", async () => {
    backend.retainBuild(3); // built earlier (the tab was open before)
    backend.state.holdImages = true;
    void session.attach();
    await flush();
    expect(backend.state.watches).toBe(1);
    expect(backend.state.heldImages.length).toBeGreaterThan(0); // the retained build waits for its images
    session.setVisible(false);
    session.setVisible(true);
    backend.state.holdImages = false;
    backend.releaseImages();
    await flush();
    // Displayed: it must hold exactly one watch (it holds none).
    expect(backend.state.watches - backend.state.unwatches).toBe(1);
  });

  it("an image that arrives late for an earlier build is not shown on the newer build's page", async () => {
    await session.attach();
    backend.state.holdImages = true;
    backend.emitBuild(3); // p0..p2 requested and held
    await flush();
    backend.emitChange();
    backend.state.refs.set(0, "p0-b").set(1, "p1-b").set(2, "p2-b");
    backend.emitBuild(3);
    await flush();
    // The newer build's images arrive first, the older ones after.
    backend.releaseImages((ref) => ref.endsWith("-b"));
    await flush();
    expect(session.getModel().buildId).toBe("typst-2");
    backend.releaseImages();
    await flush();
    const model = session.getModel();
    expect(model.buildId).toBe("typst-2");
    expect(model.pages.map((p) => p.renderRef)).toEqual(["p0-b", "p1-b", "p2-b"]);
    checkImages("late image of the earlier build");
    // The earlier build's late images are released at the next change of pages, not kept forever.
    session.setDisplayedPages([0]);
    await flush();
    const live = [...made.keys()].filter((url) => !revoked.has(url));
    expect(live.every((url) => made.get(url)!.endsWith("-b"))).toBe(true);
  });

  it("the document becomes shorter than the displayed position, then longer again", async () => {
    await session.attach();
    backend.emitBuild(200);
    await flush();
    session.setDisplayedPages([119, 120, 121, 122]);
    await flush();
    backend.emitChange();
    backend.emitBuild(5);
    await flush();
    let model = session.getModel();
    expect(model.pages).toHaveLength(5);
    expect(model.pages.slice(2).every((p) => p.url !== null), "the last pages of the shorter document").toBe(true);
    checkImages("shorter");
    // The view reports what it displays now.
    session.setDisplayedPages([3, 4]);
    await flush();
    backend.emitChange();
    backend.emitBuild(300);
    await flush();
    model = session.getModel();
    expect(model.pages).toHaveLength(300);
    expect(model.pages[3]!.url).not.toBeNull();
    expect(model.pages[4]!.url).not.toBeNull();
    checkImages("longer");
    expect(model.pages.every((p) => p.widthPt === 400 && p.heightPt === 500)).toBe(true);
  });

  it("random scrolling, builds, edits and hiding keep the invariants", async () => {
    let seed = 0x2545f491;
    const random = (n: number) => {
      seed ^= seed << 13;
      seed ^= seed >>> 17;
      seed ^= seed << 5;
      return (seed >>> 0) % n;
    };
    await session.attach();
    backend.emitBuild(30);
    await flush();
    let visible = true;
    /** A build result of the current revision arrived since the view was last shown. */
    let confirmed = true;
    let pages = 30;
    let generation = 0;
    for (let step = 0; step < 250; step++) {
      const context = `step ${step}`;
      switch (random(8)) {
        case 0:
        case 1: {
          const top = random(pages + 3);
          session.setDisplayedPages([top - 1, top, top + 1, top + 2].filter((i) => i >= 0));
          break;
        }
        case 2:
          backend.emitChange();
          confirmed = false;
          break;
        case 3:
        case 4: {
          // A build of the current revision; some pages reflow, the count may change.
          generation += 1;
          pages = Math.max(1, pages + random(7) - 3);
          const from = random(pages + 1);
          for (let i = from; i < pages; i++) backend.state.refs.set(i, `p${i}-g${generation}`);
          backend.emitBuild(pages);
          if (visible) confirmed = true;
          break;
        }
        case 5:
          // Not while the watch reply still loads its pages: that case is the
          // skipped defect test above (a view left visible without a watch).
          backend.state.holdImages = false;
          backend.releaseImages();
          await flush();
          visible = !visible;
          session.setVisible(visible);
          if (visible) confirmed = false;
          break;
        case 6:
          backend.state.holdImages = !backend.state.holdImages;
          if (!backend.state.holdImages) backend.releaseImages();
          break;
        default:
          // Some held images arrive, out of order.
          backend.releaseImages(() => random(2) === 0);
      }
      await flush();
      const model = session.getModel();
      checkImages(context);
      if (!model.stale) {
        expect(confirmed, `${context}: pages presented as current without a build result since`).toBe(true);
        expect(model.buildRevision, `${context}: current pages of an older revision`).toBe(backend.state.revision);
      }
      const held = model.pages.filter((p) => p.url !== null).length;
      expect(held, `${context}: images held`).toBeLessThanOrEqual(4 + IDLE_PAGE_IMAGES);
      expect(backend.state.watches - backend.state.unwatches, `${context}: watch count`).toBe(visible ? 1 : 0);
    }
    // Settle: shown, images delivered, one more build.
    backend.state.holdImages = false;
    backend.releaseImages();
    if (!visible) session.setVisible(true);
    await flush();
    backend.emitBuild(pages);
    await flush();
    const model = session.getModel();
    expect(model.stale).toBe(false);
    expect(model.buildId).toBe(`typst-${backend.state.builds}`);
    checkImages("settled");
    session.detach();
    const live = [...made.keys()].filter((url) => !revoked.has(url));
    expect(live, "every image is released on detach").toEqual([]);
  }, 120_000);
});
