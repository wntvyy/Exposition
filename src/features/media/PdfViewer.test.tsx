import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { PdfViewInfo } from "../../bindings/PdfViewInfo";

const ipc = vi.hoisted(() => ({
  mediaPdfOpen: vi.fn<(path: string) => Promise<PdfViewInfo>>(),
  mediaPdfPage: vi.fn<(token: number, page: number, scale: number) => Promise<ArrayBuffer>>(),
  mediaPdfClose: vi.fn<(token: number) => Promise<void>>(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { workspace: () => Promise.resolve(() => {}) },
  isCommandError: (v: unknown) => typeof v === "object" && v !== null && "code" in v,
}));

import { handleWorkspaceEvent, resetImageCacheForTests } from "./loader";
import {
  currentPage,
  fitWidthZoom,
  IDLE_PAGE_CACHE,
  layoutPages,
  MAX_CONCURRENT_PAGE_REQUESTS,
  MAX_MOUNTED_PAGES,
  mountedPages,
  nextPageIndex,
  PAGE_GAP,
  pageScrollTop,
  PDF_PX_PER_PT,
  PdfLoadError,
  PdfPages,
  pdfProblemOf,
  previousPageIndex,
  renderScale,
  stepPdfZoom,
} from "./pdf";
import { PdfViewer } from "./PdfViewer";

const resource = (path: string) => ({ key: `file:${path}`, path, title: path.split("/").pop()! });
const A4 = { width: 595, height: 842 };
const info = (token: number, pages: number, total = pages): PdfViewInfo => ({
  token,
  pageCount: total,
  pages: Array.from({ length: pages }, () => A4),
  bytes: 2048,
});
const png = () => new Uint8Array([0x89, 0x50, 0x4e, 0x47]).buffer as ArrayBuffer;

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
}
function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

let created: string[] = [];
let revoked: string[] = [];
const liveUrls = () => created.filter((u) => !revoked.includes(u));

/** jsdom has no layout: give the stage a size and a scroll position that sticks. */
function stageSize(width: number, height: number) {
  Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => width });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => height });
}

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    disconnect() {}
    unobserve() {}
  } as never;
});

beforeEach(() => {
  resetImageCacheForTests();
  created = [];
  revoked = [];
  let n = 0;
  URL.createObjectURL = vi.fn((blob: Blob) => {
    const url = `blob:p-${++n}-${blob.type}`;
    created.push(url);
    return url;
  });
  URL.revokeObjectURL = vi.fn((url: string) => void revoked.push(url));
  ipc.mediaPdfOpen.mockReset();
  ipc.mediaPdfPage.mockReset().mockImplementation(async () => png());
  ipc.mediaPdfClose.mockReset().mockResolvedValue();
  stageSize(1000, 800);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const viewer = () => screen.getByTestId("pdf-viewer");
const shownPages = () => screen.queryAllByTestId("pdf-page").map((p) => Number(p.getAttribute("data-page")));
const stage = () => screen.getByRole("group");
function scrollTo(top: number) {
  const el = stage();
  el.scrollTop = top;
  fireEvent.scroll(el);
}

describe("PDF zoom and layout", () => {
  it("steps through fixed zoom levels within 10%–400%", () => {
    expect(stepPdfZoom(1, 1)).toBe(1.25);
    expect(stepPdfZoom(1, -1)).toBe(0.75);
    expect(stepPdfZoom(1.1, 1)).toBe(1.25);
    expect(stepPdfZoom(4, 1)).toBe(4);
    expect(stepPdfZoom(0.1, -1)).toBe(0.1);
  });

  it("fits the widest page to the stage width, within the zoom range", () => {
    expect(fitWidthZoom(600, 800 + 2 * PAGE_GAP)).toBeCloseTo(1, 5);
    expect(fitWidthZoom(600, 400 + 2 * PAGE_GAP)).toBeCloseTo(0.5, 5);
    expect(fitWidthZoom(10, 5000)).toBe(4);
    expect(fitWidthZoom(100000, 300)).toBe(0.1);
    expect(fitWidthZoom(600, 0)).toBe(1);
  });

  it("asks for rasters in quarter steps, sharper on dense displays, within the accepted range", () => {
    expect(renderScale(1, 1)).toBe(1.5);
    expect(renderScale(1, 2)).toBe(2.75);
    expect(renderScale(0.1, 1)).toBe(0.25);
    expect(renderScale(4, 3)).toBe(8);
    expect(renderScale(1.02, 1)).toBe(renderScale(1.1, 1));
  });

  it("stacks pages and finds the current and mounted pages from the scroll position", () => {
    const layout = layoutPages([A4, { width: 842, height: 595 }, A4], 1);
    const h = Math.round(842 * PDF_PX_PER_PT);
    const w = Math.round(595 * PDF_PX_PER_PT);
    expect(layout.tops).toEqual([PAGE_GAP, 2 * PAGE_GAP + h, 3 * PAGE_GAP + h + w]);
    expect(layout.width).toBe(h + 2 * PAGE_GAP);
    expect(layout.height).toBe(4 * PAGE_GAP + 2 * h + w);
    expect(currentPage(layout, 0, 800)).toBe(0);
    expect(currentPage(layout, layout.tops[1]! - 8, 800)).toBe(1);
    expect(currentPage(layout, layout.tops[1]! - 30, 800)).toBe(0);
    expect(currentPage(layout, layout.height - 800, 800)).toBe(2);
    expect(mountedPages(layout, 0, 800)).toEqual({ first: 0, last: 1 });
    expect(mountedPages(layout, layout.tops[1]!, 800)).toEqual({ first: 0, last: 2 });
    expect(mountedPages(layoutPages([], 1), 0, 800)).toEqual({ first: 0, last: -1 });
  });

  it("never mounts more than the cap, however many pages the viewport shows", () => {
    const layout = layoutPages(Array.from({ length: 500 }, () => ({ width: 100, height: 20 })), 0.1);
    const { first, last } = mountedPages(layout, 300, 5000);
    expect(last - first + 1).toBeLessThanOrEqual(MAX_MOUNTED_PAGES);
  });

  it("moves page by page, also away from the end of the document", () => {
    const layout = layoutPages(Array.from({ length: 4 }, () => ({ width: 300, height: 150 })), 1);
    const viewport = 500; // two and a half pages
    const end = layout.height - viewport;
    expect(nextPageIndex(layout, 0)).toBe(1);
    expect(pageScrollTop(layout, 1)).toBe(layout.tops[1]! - PAGE_GAP / 2);
    // From the end (the last page counts as current) Previous reaches a page above.
    expect(currentPage(layout, end, viewport)).toBe(3);
    const target = previousPageIndex(layout, end, viewport);
    expect(pageScrollTop(layout, target)).toBeLessThan(end);
    // Previous goes to the page before the current one.
    expect(previousPageIndex(layout, pageScrollTop(layout, 2) + 40, 100)).toBe(1);
    expect(previousPageIndex(layout, pageScrollTop(layout, 1), 100)).toBe(0);
    expect(previousPageIndex(layout, 0, viewport)).toBe(0);
    expect(nextPageIndex(layout, pageScrollTop(layout, 3))).toBe(3);
  });

  it("maps command errors to problems and marks answers for another view as superseded", () => {
    expect(pdfProblemOf({ code: "pdf.encrypted", message: "m" }).problem).toBe("encrypted");
    expect(pdfProblemOf({ code: "path.outsideWorkspace", message: "m" }).problem).toBe("blocked");
    expect(pdfProblemOf({ code: "pdf.closed", message: "m" }).superseded).toBe(true);
    expect(pdfProblemOf({ code: "workspace.changed", message: "m" }).superseded).toBe(true);
    expect(pdfProblemOf({ code: "pdf.damaged", message: "m" }).superseded).toBe(false);
    expect(pdfProblemOf(new Error("boom")).problem).toBe("unavailable");
  });
});

describe("PdfPages", () => {
  it("shares a page image, limits requests in flight, and never sends a request nobody waits for", async () => {
    const gates: Deferred<ArrayBuffer>[] = [];
    ipc.mediaPdfPage.mockImplementation(() => {
      const d = deferred<ArrayBuffer>();
      gates.push(d);
      return d.promise;
    });
    const pages = new PdfPages(7);
    const a = pages.acquire(0, 1.5);
    const a2 = pages.acquire(0, 1.5);
    const b = pages.acquire(1, 1.5);
    const c = pages.acquire(2, 1.5);
    const d = pages.acquire(3, 1.5);
    expect(ipc.mediaPdfPage).toHaveBeenCalledTimes(MAX_CONCURRENT_PAGE_REQUESTS);
    expect(a.promise).toBe(a2.promise);
    // Page 3 scrolls away before its turn: it is never requested.
    c.release();
    await expect(c.promise).rejects.toMatchObject({ superseded: true });
    gates[0]!.resolve(png());
    gates[1]!.resolve(png());
    expect(await a.promise).toBe(created[0]);
    await b.promise;
    await waitFor(() => expect(ipc.mediaPdfPage).toHaveBeenCalledTimes(3));
    expect(ipc.mediaPdfPage.mock.calls.map((call) => call[1])).toEqual([0, 1, 3]);
    gates[2]!.resolve(png());
    await d.promise;
    expect(pages.size).toBe(3);
    pages.dispose();
    expect(liveUrls()).toEqual([]);
    expect(ipc.mediaPdfClose).toHaveBeenCalledWith(7);
  });

  it("keeps a few unused page images and revokes the rest", async () => {
    const pages = new PdfPages(1);
    for (let i = 0; i < IDLE_PAGE_CACHE + 4; i++) {
      const handle = pages.acquire(i, 1);
      await handle.promise;
      handle.release();
      handle.release();
    }
    expect(pages.size).toBe(IDLE_PAGE_CACHE);
    expect(liveUrls().length).toBe(IDLE_PAGE_CACHE);
    // A cached page is reused without a request.
    const calls = ipc.mediaPdfPage.mock.calls.length;
    const again = pages.acquire(IDLE_PAGE_CACHE + 3, 1);
    expect(await again.promise).toBe(created[created.length - 1]);
    expect(ipc.mediaPdfPage).toHaveBeenCalledTimes(calls);
    pages.dispose();
    expect(liveUrls()).toEqual([]);
  });

  it("drops an answer that arrives after disposal and creates no URL for it", async () => {
    const gate = deferred<ArrayBuffer>();
    ipc.mediaPdfPage.mockReturnValue(gate.promise);
    const pages = new PdfPages(3);
    const handle = pages.acquire(0, 1);
    pages.dispose();
    gate.resolve(png());
    await expect(handle.promise).rejects.toBeInstanceOf(PdfLoadError);
    expect(created).toEqual([]);
    await expect(pages.acquire(1, 1).promise).rejects.toMatchObject({ superseded: true });
    expect(ipc.mediaPdfPage).toHaveBeenCalledTimes(1);
  });

  it("reports a view the Rust side dropped once, and a failed page as that page's failure", async () => {
    const lost = vi.fn();
    const pages = new PdfPages(4, lost);
    ipc.mediaPdfPage.mockRejectedValueOnce({ code: "pdf.renderFailed", message: "Page 1 could not be rendered." });
    await expect(pages.acquire(0, 1).promise).rejects.toMatchObject({ superseded: false, message: "Page 1 could not be rendered." });
    expect(lost).not.toHaveBeenCalled();
    ipc.mediaPdfPage.mockRejectedValue({ code: "pdf.closed", message: "gone" });
    await expect(pages.acquire(1, 1).promise).rejects.toMatchObject({ superseded: true });
    await expect(pages.acquire(2, 1).promise).rejects.toMatchObject({ superseded: true });
    expect(lost).toHaveBeenCalledTimes(1);
  });
});

describe("PdfViewer", () => {
  it("shows a loading state, then the pages near the viewport with page count and size", async () => {
    const open = deferred<PdfViewInfo>();
    ipc.mediaPdfOpen.mockReturnValue(open.promise);
    render(<PdfViewer resource={resource("out/paper one.pdf")} mode="source" active />);
    expect(viewer().getAttribute("data-status")).toBe("loading");
    expect(screen.getByText("Loading PDF…")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Next page" }).hasAttribute("disabled")).toBe(true);
    await act(async () => open.resolve(info(1, 30)));
    await waitFor(() => expect(viewer().getAttribute("data-status")).toBe("ready"));
    expect(ipc.mediaPdfOpen).toHaveBeenCalledWith("out/paper one.pdf");
    expect(screen.getByTestId("pdf-facts").textContent).toBe("30 pages · PDF · 2 KB");
    expect(screen.getByTestId("pdf-page-indicator").textContent).toBe("Page 1 of 30");
    // Only pages near the viewport exist, not all 30.
    expect(shownPages()).toEqual([1, 2]);
    const img = await screen.findByAltText("Page 1");
    expect(img.tagName).toBe("IMG");
    expect(img.getAttribute("src")).toMatch(/^blob:p-\d+-image\/png$/);
    expect(ipc.mediaPdfPage.mock.calls.every(([token, page]) => token === 1 && page < 2)).toBe(true);
  });

  it("navigates by page and by scrolling, mounting only nearby pages", async () => {
    ipc.mediaPdfOpen.mockResolvedValue(info(1, 30));
    render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    await screen.findByAltText("Page 1");
    const layout = layoutPages(info(1, 30).pages, fitWidthZoom(595, 1000));
    const previous = screen.getByRole("button", { name: "Previous page" });
    const next = screen.getByRole("button", { name: "Next page" });
    expect(previous.hasAttribute("disabled")).toBe(true);
    fireEvent.click(next);
    expect(viewer().getAttribute("data-page")).toBe("2");
    expect(stage().scrollTop).toBe(pageScrollTop(layout, 1));
    fireEvent.click(next);
    expect(screen.getByTestId("pdf-page-indicator").textContent).toBe("Page 3 of 30");
    expect(shownPages()).toEqual([2, 3, 4]);
    fireEvent.click(previous);
    expect(viewer().getAttribute("data-page")).toBe("2");
    // Scrolling far away: the first pages are gone, their images released to the idle cache.
    scrollTo(pageScrollTop(layout, 20));
    expect(viewer().getAttribute("data-page")).toBe("21");
    expect(shownPages()).toEqual([20, 21, 22]);
    await screen.findByAltText("Page 21");
    scrollTo(layout.height - 800);
    expect(viewer().getAttribute("data-page")).toBe("30");
    expect(next.hasAttribute("disabled")).toBe(true);
    await waitFor(() => expect(liveUrls().length).toBeLessThanOrEqual(MAX_MOUNTED_PAGES + IDLE_PAGE_CACHE));
  });

  it("fits the width by default and zooms with the toolbar and the keyboard", async () => {
    ipc.mediaPdfOpen.mockResolvedValue(info(1, 3));
    render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    await screen.findByAltText("Page 1");
    const fitted = fitWidthZoom(595, 1000);
    expect(viewer().getAttribute("data-fit")).toBe("true");
    expect(Number(viewer().getAttribute("data-zoom"))).toBeCloseTo(fitted, 2);
    fireEvent.click(screen.getByRole("button", { name: "Actual size" }));
    expect(viewer().getAttribute("data-zoom")).toBe("1");
    expect(viewer().getAttribute("data-fit")).toBe("false");
    expect(screen.getByTestId("pdf-zoom").textContent).toBe("100%");
    expect(screen.getByAltText("Page 1").getAttribute("width")).toBe(String(Math.round(595 * PDF_PX_PER_PT)));
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    expect(viewer().getAttribute("data-zoom")).toBe("1.25");
    fireEvent.keyDown(stage(), { key: "-" });
    fireEvent.keyDown(stage(), { key: "-" });
    expect(viewer().getAttribute("data-zoom")).toBe("0.75");
    fireEvent.keyDown(stage(), { key: "0" });
    expect(viewer().getAttribute("data-zoom")).toBe("1");
    fireEvent.click(screen.getByRole("button", { name: "Fit width" }));
    expect(viewer().getAttribute("data-fit")).toBe("true");
    // The picture already shown stays while a sharper one is requested.
    expect(screen.getByAltText("Page 1").getAttribute("src")).toMatch(/^blob:/);
    expect(ipc.mediaPdfOpen).toHaveBeenCalledTimes(1);
  });

  it("names missing, outside, oversized, encrypted, damaged, and empty files", async () => {
    const cases: [string, string, string][] = [
      ["path.notFound", "missing", "PDF not found"],
      ["path.outsideWorkspace", "blocked", "PDF outside the project"],
      ["pdf.tooLarge", "tooLarge", "PDF too large to show"],
      ["pdf.encrypted", "encrypted", "Can't show an encrypted PDF"],
      ["pdf.damaged", "damaged", "Can't show this PDF"],
      ["pdf.empty", "empty", "No pages to show"],
      ["internal.task", "unavailable", "Could not load this PDF"],
    ];
    for (const [code, problem, title] of cases) {
      ipc.mediaPdfOpen.mockRejectedValueOnce({ code, message: `${code} message.` });
      const { unmount } = render(<PdfViewer resource={resource("bad.pdf")} mode="source" active />);
      const state = await screen.findByTestId("pdf-viewer-state");
      expect(state.getAttribute("data-problem"), code).toBe(problem);
      expect(state.textContent).toContain(title);
      expect(viewer().getAttribute("data-status")).toBe(problem);
      expect(screen.queryAllByTestId("pdf-page")).toEqual([]);
      unmount();
    }
    expect(ipc.mediaPdfPage).not.toHaveBeenCalled();
  });

  it("retries an unexpected failure", async () => {
    ipc.mediaPdfOpen.mockRejectedValueOnce({ code: "internal.task", message: "PDF task failed." });
    render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    await screen.findByTestId("pdf-viewer-state");
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(2, 1));
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await screen.findByAltText("Page 1");
    expect(screen.getByTestId("pdf-facts").textContent).toBe("1 page · PDF · 2 KB");
  });

  it("says when only the first pages of a very long PDF are shown", async () => {
    ipc.mediaPdfOpen.mockResolvedValue(info(1, 2000, 2345));
    render(<PdfViewer resource={resource("long.pdf")} mode="source" active />);
    await screen.findByAltText("Page 1");
    expect(screen.getByText("Showing the first 2,000 of 2,345 pages.")).toBeTruthy();
    expect(screen.getByTestId("pdf-page-indicator").textContent).toBe("Page 1 of 2000");
    expect(screen.getByTestId("pdf-facts").textContent).toBe("2,345 pages · PDF · 2 KB");
  });

  it("shows a page that cannot be rendered as that page's problem, not the file's", async () => {
    ipc.mediaPdfOpen.mockResolvedValue(info(1, 2));
    ipc.mediaPdfPage.mockImplementation(async (_token, page) => {
      if (page === 0) throw { code: "pdf.renderFailed", message: "Page 1 could not be rendered." };
      return png();
    });
    render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    await screen.findByAltText("Page 2");
    await screen.findByText("Page 1 could not be rendered.");
    expect(viewer().getAttribute("data-status")).toBe("ready");
    expect(screen.getAllByTestId("pdf-page")[0]!.getAttribute("data-state")).toBe("failed");
  });

  it("is read-only: no editing surface, no page-editing, build, or sync control", async () => {
    ipc.mediaPdfOpen.mockResolvedValue(info(1, 2));
    const { container } = render(<PdfViewer resource={resource("paper.pdf")} mode="page" active />);
    await screen.findByAltText("Page 1");
    expect(container.querySelector("textarea, input, [contenteditable=true], .monaco-editor, a[href], iframe, object, embed")).toBeNull();
    const names = screen.getAllByRole("button").map((b) => b.getAttribute("aria-label") ?? b.textContent);
    expect(names).toEqual(["Previous page", "Next page", "Fit width", "Actual size", "Zoom out", "Zoom in"]);
    // Typing and clicking on the pages change nothing and ask Rust for nothing but pages.
    fireEvent.keyDown(stage(), { key: "a" });
    fireEvent.click(screen.getByAltText("Page 1"));
    fireEvent.doubleClick(screen.getByAltText("Page 1"));
    expect(Object.keys(ipc).sort()).toEqual(["mediaPdfClose", "mediaPdfOpen", "mediaPdfPage"]);
  });

  it("opens the file again when it is replaced, keeping the old pages until the new ones arrive", async () => {
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(1, 2));
    render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    await screen.findByAltText("Page 1");
    const firstUrls = [...created];
    const reopened = deferred<PdfViewInfo>();
    ipc.mediaPdfOpen.mockReturnValueOnce(reopened.promise);
    await act(async () => handleWorkspaceEvent({ kind: "filesChanged", changes: [{ path: "paper.pdf", kind: "modified" }], rescan: false }));
    // Still the previous pages, not a loading state.
    expect(viewer().getAttribute("data-status")).toBe("ready");
    expect(viewer().getAttribute("data-view")).toBe("1");
    await act(async () => reopened.resolve(info(2, 5)));
    await waitFor(() => expect(viewer().getAttribute("data-view")).toBe("2"));
    expect(screen.getByTestId("pdf-facts").textContent).toBe("5 pages · PDF · 2 KB");
    // The replaced view is released: its pictures revoked, its token closed.
    expect(ipc.mediaPdfClose).toHaveBeenCalledWith(1);
    for (const url of firstUrls) expect(revoked).toContain(url);
    await waitFor(() => expect(ipc.mediaPdfPage.mock.calls.some(([token]) => token === 2)).toBe(true));
    // Another file changing does nothing.
    await act(async () => handleWorkspaceEvent({ kind: "filesChanged", changes: [{ path: "other.pdf", kind: "modified" }], rescan: false }));
    expect(ipc.mediaPdfOpen).toHaveBeenCalledTimes(2);
  });

  it("reports a deleted file and shows it again when it comes back", async () => {
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(1, 2));
    render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    await screen.findByAltText("Page 1");
    ipc.mediaPdfOpen.mockRejectedValueOnce({ code: "path.notFound", message: "paper.pdf does not exist." });
    await act(async () => handleWorkspaceEvent({ kind: "filesChanged", changes: [{ path: "paper.pdf", kind: "removed" }], rescan: false }));
    expect((await screen.findByTestId("pdf-viewer-state")).getAttribute("data-problem")).toBe("missing");
    expect(ipc.mediaPdfClose).toHaveBeenCalledWith(1);
    expect(liveUrls()).toEqual([]);
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(3, 1));
    await act(async () => handleWorkspaceEvent({ kind: "filesChanged", changes: [{ path: "paper.pdf", kind: "created" }], rescan: false }));
    await screen.findByAltText("Page 1");
    expect(viewer().getAttribute("data-view")).toBe("3");
  });

  it("follows a renamed file (the tab is re-keyed) and never shows the old file's pages under the new path", async () => {
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(1, 2));
    const { rerender } = render(<PdfViewer resource={resource("old.pdf")} mode="source" active />);
    await screen.findByAltText("Page 1");
    const renamed = deferred<PdfViewInfo>();
    ipc.mediaPdfOpen.mockReturnValueOnce(renamed.promise);
    rerender(<PdfViewer resource={resource("new.pdf")} mode="source" active />);
    expect(viewer().getAttribute("data-path")).toBe("new.pdf");
    expect(viewer().getAttribute("data-status")).toBe("loading");
    expect(screen.queryAllByTestId("pdf-page")).toEqual([]);
    expect(ipc.mediaPdfClose).toHaveBeenCalledWith(1);
    await act(async () => renamed.resolve(info(2, 2)));
    await screen.findByAltText("Page 1");
    expect(ipc.mediaPdfOpen).toHaveBeenLastCalledWith("new.pdf");
    expect(viewer().getAttribute("data-view")).toBe("2");
  });

  it("discards a result that arrives for a previous file and releases its view", async () => {
    const slow = deferred<PdfViewInfo>();
    ipc.mediaPdfOpen.mockReturnValueOnce(slow.promise);
    const { rerender } = render(<PdfViewer resource={resource("slow.pdf")} mode="source" active />);
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(2, 1));
    rerender(<PdfViewer resource={resource("fast.pdf")} mode="source" active />);
    await screen.findByAltText("Page 1");
    await act(async () => slow.resolve(info(1, 9)));
    expect(viewer().getAttribute("data-view")).toBe("2");
    expect(screen.getByTestId("pdf-facts").textContent).toBe("1 page · PDF · 2 KB");
    expect(ipc.mediaPdfClose).toHaveBeenCalledWith(1);
    expect(ipc.mediaPdfPage.mock.calls.every(([token]) => token === 2)).toBe(true);
  });

  it("discards results of the previous project when the project is switched during the load", async () => {
    const before = deferred<PdfViewInfo>();
    ipc.mediaPdfOpen.mockReturnValueOnce(before.promise);
    render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    const after = deferred<PdfViewInfo>();
    ipc.mediaPdfOpen.mockReturnValueOnce(after.promise);
    await act(async () => handleWorkspaceEvent({ kind: "opened" } as never));
    expect(ipc.mediaPdfOpen).toHaveBeenCalledTimes(2);
    // The answer of the first project arrives late: released, never shown.
    await act(async () => before.resolve(info(1, 7)));
    expect(viewer().getAttribute("data-status")).toBe("loading");
    expect(ipc.mediaPdfClose).toHaveBeenCalledWith(1);
    expect(ipc.mediaPdfPage).not.toHaveBeenCalled();
    // Rust refusing the stale call (`workspace.changed`) is not shown as an error either.
    await act(async () => after.reject({ code: "workspace.changed", message: "The project changed." }));
    expect(viewer().getAttribute("data-status")).toBe("loading");
    expect(screen.queryByTestId("pdf-viewer-state")).toBeNull();
  });

  it("opens the file again when the Rust side dropped the view", async () => {
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(1, 2));
    ipc.mediaPdfPage.mockRejectedValue({ code: "pdf.closed", message: "This PDF view is no longer open." });
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(2, 2));
    render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    await waitFor(() => expect(ipc.mediaPdfOpen).toHaveBeenCalledTimes(1));
    ipc.mediaPdfPage.mockImplementation(async (token) => {
      if (token === 1) throw { code: "pdf.closed", message: "This PDF view is no longer open." };
      return png();
    });
    await waitFor(() => expect(viewer().getAttribute("data-view")).toBe("2"));
    await screen.findByAltText("Page 1");
    expect(screen.queryByText("This PDF view is no longer open.")).toBeNull();
  });

  it("releases everything when the tab closes: pictures revoked, view closed, late answers dropped", async () => {
    ipc.mediaPdfOpen.mockResolvedValueOnce(info(1, 3));
    const pending = deferred<ArrayBuffer>();
    ipc.mediaPdfPage.mockImplementation(async (_token, page) => (page === 1 ? pending.promise : png()));
    const { unmount } = render(<PdfViewer resource={resource("paper.pdf")} mode="source" active />);
    await screen.findByAltText("Page 1");
    expect(liveUrls().length).toBe(1);
    unmount();
    expect(liveUrls()).toEqual([]);
    expect(ipc.mediaPdfClose).toHaveBeenCalledWith(1);
    const made = created.length;
    await act(async () => pending.resolve(png()));
    expect(created.length).toBe(made);

    // Closed while the file was still being opened: the late view is released.
    const slow = deferred<PdfViewInfo>();
    ipc.mediaPdfOpen.mockReturnValueOnce(slow.promise);
    const second = render(<PdfViewer resource={resource("slow.pdf")} mode="source" active />);
    second.unmount();
    await act(async () => slow.resolve(info(9, 4)));
    expect(ipc.mediaPdfClose).toHaveBeenCalledWith(9);
    expect(ipc.mediaPdfPage.mock.calls.every(([token]) => token === 1)).toBe(true);
  });
});
