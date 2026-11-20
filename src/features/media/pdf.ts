// Project PDF files for the read-only PDF tab: page layout, zoom, and the
// per-view page-image cache.
//
// Security and resource rules (see also src-tauri/src/media.rs and
// crates/exposition-tex/src/pdf_view.rs):
// - The webview never receives a PDF. `media_pdf_open` reads and parses a
//   `.pdf` file inside the open project in Rust (at most 64 MiB, links out of
//   the folder refused) and answers page sizes; `media_pdf_page` answers one
//   page as PNG bytes. Nothing in the PDF is executed or followed (no scripts,
//   actions, links, or network loads): pages are pictures, shown only through
//   <img> from a blob: URL created here.
// - Only pages near the viewport are requested (at most MAX_MOUNTED_PAGES at
//   once, MAX_CONCURRENT_PAGE_REQUESTS in flight); page images are shared per
//   (page, scale), a few stay cached after their last use, and everything is
//   revoked when the view is disposed (tab closed or switched, file replaced,
//   project switched). A request nobody needs anymore is dropped before it is
//   sent; a late answer is revoked at once.
import type { PdfPageSize } from "../../bindings/PdfPageSize";
import type { PdfViewInfo } from "../../bindings/PdfViewInfo";
import { commands, isCommandError } from "../../lib/ipc";

/** CSS pixels per typographic point at 100 % zoom (as the page editor). */
export const PDF_PX_PER_PT = 96 / 72;
export const PDF_ZOOM_STEPS = [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1, 1.25, 1.5, 2, 3, 4];
export const PDF_MIN_ZOOM = PDF_ZOOM_STEPS[0]!;
export const PDF_MAX_ZOOM = PDF_ZOOM_STEPS[PDF_ZOOM_STEPS.length - 1]!;
/** Space around and between pages (CSS pixels; `--space-4`). */
export const PAGE_GAP = 16;
/** Page elements (and page images) kept in the DOM at once. */
export const MAX_MOUNTED_PAGES = 12;
/** Page requests in flight per view (Rust rasterizes one page at a time). */
export const MAX_CONCURRENT_PAGE_REQUESTS = 2;
/** Page images kept after their last use, per view. */
export const IDLE_PAGE_CACHE = 6;
/** Render scales the Rust side accepts (pixels per point). */
const MIN_RENDER_SCALE = 0.25;
const MAX_RENDER_SCALE = 8;

export type PdfProblem = "missing" | "blocked" | "tooLarge" | "encrypted" | "damaged" | "empty" | "notPdf" | "unavailable";

export class PdfLoadError extends Error {
  constructor(
    readonly problem: PdfProblem,
    message: string,
    /** The load was dropped (view closed, file changed, project switched); not a result. */
    readonly superseded = false,
  ) {
    super(message);
    this.name = "PdfLoadError";
  }
}

const PROBLEMS: Record<string, PdfProblem> = {
  "path.notFound": "missing",
  "path.brokenLink": "missing",
  "file.isDirectory": "missing",
  "path.outsideWorkspace": "blocked",
  "pdf.tooLarge": "tooLarge",
  "pdf.encrypted": "encrypted",
  "pdf.damaged": "damaged",
  "pdf.empty": "empty",
  "pdf.notPdf": "notPdf",
};

/** Codes that mean "this answer is not for the view shown now", never a result to show. */
const SUPERSEDED_CODES = new Set(["pdf.closed", "workspace.changed", "workspace.notOpen"]);

export function pdfProblemOf(error: unknown): PdfLoadError {
  if (error instanceof PdfLoadError) return error;
  if (isCommandError(error)) {
    return new PdfLoadError(PROBLEMS[error.code] ?? "unavailable", error.message, SUPERSEDED_CODES.has(error.code));
  }
  return new PdfLoadError("unavailable", error instanceof Error ? error.message : String(error));
}

// ----- zoom ----------------------------------------------------------------------------------

/** Next zoom step above (direction 1) or below (-1) `zoom`. */
export function stepPdfZoom(zoom: number, direction: 1 | -1): number {
  if (direction > 0) return PDF_ZOOM_STEPS.find((z) => z > zoom + 1e-6) ?? PDF_MAX_ZOOM;
  return [...PDF_ZOOM_STEPS].reverse().find((z) => z < zoom - 1e-6) ?? PDF_MIN_ZOOM;
}

/** Zoom at which the widest page (points) fills a stage `stageWidth` CSS pixels wide. */
export function fitWidthZoom(widestPt: number, stageWidth: number): number {
  const available = stageWidth - 2 * PAGE_GAP;
  if (widestPt <= 0 || available <= 0) return 1;
  return Math.min(PDF_MAX_ZOOM, Math.max(PDF_MIN_ZOOM, available / (widestPt * PDF_PX_PER_PT)));
}

/**
 * Raster scale (pixels per point) to request for pages shown at `zoom` on a
 * display with `devicePixelRatio`: rounded up to a quarter so nearby zoom
 * levels share images, within what the Rust side accepts.
 */
export function renderScale(zoom: number, devicePixelRatio: number): number {
  const wanted = zoom * PDF_PX_PER_PT * Math.max(1, Math.min(3, devicePixelRatio || 1));
  return Math.min(MAX_RENDER_SCALE, Math.max(MIN_RENDER_SCALE, Math.ceil(wanted * 4 - 1e-6) / 4));
}

// ----- layout --------------------------------------------------------------------------------

export interface PageLayout {
  /** Top of each page inside the scrolled content (CSS pixels). */
  tops: number[];
  widths: number[];
  heights: number[];
  /** Size of the scrolled content, margins included. */
  width: number;
  height: number;
}

/** Pages stacked top to bottom at `zoom`, `PAGE_GAP` around and between them. */
export function layoutPages(pages: readonly PdfPageSize[], zoom: number): PageLayout {
  const k = zoom * PDF_PX_PER_PT;
  const tops: number[] = [];
  const widths: number[] = [];
  const heights: number[] = [];
  let y = PAGE_GAP;
  let widest = 0;
  for (const page of pages) {
    const w = Math.max(1, Math.round(page.width * k));
    const h = Math.max(1, Math.round(page.height * k));
    tops.push(y);
    widths.push(w);
    heights.push(h);
    widest = Math.max(widest, w);
    y += h + PAGE_GAP;
  }
  return { tops, widths, heights, width: widest + 2 * PAGE_GAP, height: y };
}

/** Index of the first page whose bottom is below `y` (the last page when none is). */
function firstPageEndingAfter(layout: PageLayout, y: number): number {
  let lo = 0;
  let hi = layout.tops.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (layout.tops[mid]! + layout.heights[mid]! > y) hi = mid;
    else lo = mid + 1;
  }
  return Math.max(0, lo);
}

/**
 * The page the view is at (0-based): the first page still visible at the top
 * of the viewport; the last page once the view is scrolled to its end.
 */
export function currentPage(layout: PageLayout, scrollTop: number, viewportHeight: number): number {
  const count = layout.tops.length;
  if (count === 0) return 0;
  if (viewportHeight > 0 && scrollTop > 0 && scrollTop + viewportHeight >= layout.height - 1) return count - 1;
  return firstPageEndingAfter(layout, scrollTop + 1);
}

/** Pages to keep mounted: those in the viewport plus one on each side, at most `MAX_MOUNTED_PAGES`. */
export function mountedPages(layout: PageLayout, scrollTop: number, viewportHeight: number): { first: number; last: number } {
  const count = layout.tops.length;
  if (count === 0) return { first: 0, last: -1 };
  const top = firstPageEndingAfter(layout, scrollTop);
  let bottom = top;
  const end = scrollTop + Math.max(1, viewportHeight);
  while (bottom + 1 < count && layout.tops[bottom + 1]! < end) bottom += 1;
  // Never more than the cap, even when the viewport shows more (tiny pages).
  bottom = Math.min(bottom, top + MAX_MOUNTED_PAGES - 1);
  let first = top;
  let last = bottom;
  if (last - first + 1 < MAX_MOUNTED_PAGES && first > 0) first -= 1;
  if (last - first + 1 < MAX_MOUNTED_PAGES && last < count - 1) last += 1;
  return { first, last };
}

/** Scroll position that puts page `index` at the top of the viewport. */
export function pageScrollTop(layout: PageLayout, index: number): number {
  const i = Math.min(layout.tops.length - 1, Math.max(0, index));
  return i <= 0 ? 0 : Math.max(0, layout.tops[i]! - PAGE_GAP / 2);
}

/**
 * Where "Previous page" scrolls to from `scrollTop`: the page before the
 * current one, or an earlier one when that would not move the view (at the
 * end of the document several pages may be visible).
 */
export function previousPageIndex(layout: PageLayout, scrollTop: number, viewportHeight: number): number {
  let index = Math.max(0, currentPage(layout, scrollTop, viewportHeight) - 1);
  while (index > 0 && pageScrollTop(layout, index) >= scrollTop - 1) index -= 1;
  return index;
}

/** Where "Next page" scrolls to: the first page whose top is below the viewport's top. */
export function nextPageIndex(layout: PageLayout, scrollTop: number): number {
  const count = layout.tops.length;
  for (let index = firstPageEndingAfter(layout, scrollTop + 1); index < count; index++) {
    if (pageScrollTop(layout, index) > scrollTop + 1) return index;
  }
  return count - 1;
}

// ----- page images ---------------------------------------------------------------------------

export interface PageHandle {
  /** Resolves with a blob: URL (owned by the cache) or rejects with a `PdfLoadError`. */
  promise: Promise<string>;
  /** The caller no longer shows the page image. */
  release: () => void;
}

interface PageEntry {
  key: string;
  page: number;
  scale: number;
  refs: number;
  url: string | null;
  settled: boolean;
  started: boolean;
  promise: Promise<string>;
  resolve: (url: string) => void;
  reject: (error: PdfLoadError) => void;
}

const superseded = () => new PdfLoadError("unavailable", "The PDF view was closed.", true);

/**
 * Page images of one open PDF view (one `media_pdf_open` token). Dispose it
 * when the view is replaced or gone: every URL is revoked and the Rust side
 * releases the parsed file.
 */
export class PdfPages {
  private readonly entries = new Map<string, PageEntry>();
  private readonly queue: PageEntry[] = [];
  /** Unreferenced, loaded entries in least-recently-used order. */
  private readonly idle: string[] = [];
  private inFlight = 0;
  private dead = false;
  private lost = false;

  constructor(
    readonly token: number,
    /** The Rust side no longer has this view (dropped, or the project changed): open the file again. */
    private readonly onLost: () => void = () => {},
  ) {}

  get disposed(): boolean {
    return this.dead;
  }

  /** Page images held now (shown or cached). */
  get size(): number {
    return [...this.entries.values()].filter((e) => e.url !== null).length;
  }

  acquire(page: number, scale: number): PageHandle {
    const key = `${page}@${scale}`;
    let entry = this.entries.get(key);
    if (!entry) {
      let resolve!: (url: string) => void;
      let reject!: (error: PdfLoadError) => void;
      const promise = new Promise<string>((res, rej) => {
        resolve = res;
        reject = rej;
      });
      promise.catch(() => {});
      entry = { key, page, scale, refs: 0, url: null, settled: false, started: false, promise, resolve, reject };
      if (this.dead) {
        entry.settled = true;
        reject(superseded());
      } else {
        this.entries.set(key, entry);
        this.queue.push(entry);
      }
    }
    const held = entry;
    held.refs += 1;
    const at = this.idle.indexOf(key);
    if (at >= 0) this.idle.splice(at, 1);
    this.pump();
    let released = false;
    return {
      promise: held.promise,
      release: () => {
        if (released) return;
        released = true;
        held.refs -= 1;
        if (held.refs > 0 || this.dead) return;
        if (!held.started) {
          // Not requested yet and nobody waits for it: never request it.
          this.forget(held);
          const queued = this.queue.indexOf(held);
          if (queued >= 0) this.queue.splice(queued, 1);
          held.settled = true;
          held.reject(superseded());
        } else if (held.url !== null) {
          this.idle.push(held.key);
          while (this.idle.length > IDLE_PAGE_CACHE) {
            const old = this.entries.get(this.idle.shift()!);
            if (old) this.forget(old);
          }
        } else if (held.settled) {
          // A failed page: asked again next time it is shown.
          this.forget(held);
        }
      },
    };
  }

  private forget(entry: PageEntry) {
    if (this.entries.get(entry.key) === entry) this.entries.delete(entry.key);
    if (entry.url !== null) {
      URL.revokeObjectURL(entry.url);
      entry.url = null;
    }
  }

  private pump() {
    while (!this.dead && this.inFlight < MAX_CONCURRENT_PAGE_REQUESTS && this.queue.length > 0) {
      const entry = this.queue.shift()!;
      entry.started = true;
      this.inFlight += 1;
      void this.request(entry).finally(() => {
        this.inFlight -= 1;
        this.pump();
      });
    }
  }

  private async request(entry: PageEntry): Promise<void> {
    try {
      const bytes = await commands.mediaPdfPage(this.token, entry.page, entry.scale);
      if (this.dead) throw superseded();
      const url = URL.createObjectURL(new Blob([bytes], { type: "image/png" }));
      entry.settled = true;
      if (entry.refs === 0) {
        // Everyone who asked is gone (scrolled away): keep it as an idle image.
        entry.url = url;
        this.idle.push(entry.key);
        while (this.idle.length > IDLE_PAGE_CACHE) {
          const old = this.entries.get(this.idle.shift()!);
          if (old) this.forget(old);
        }
      } else {
        entry.url = url;
      }
      entry.resolve(url);
    } catch (error) {
      const problem = pdfProblemOf(error);
      entry.settled = true;
      if (this.entries.get(entry.key) === entry && entry.refs === 0) this.entries.delete(entry.key);
      entry.reject(problem);
      if (problem.superseded && !this.dead && !this.lost) {
        this.lost = true;
        this.onLost();
      }
    }
  }

  /** Revoke every page image, drop pending requests, and release the view in Rust. */
  dispose(): void {
    if (this.dead) return;
    this.dead = true;
    for (const entry of this.entries.values()) {
      if (entry.url !== null) URL.revokeObjectURL(entry.url);
      entry.url = null;
      if (!entry.settled && !entry.started) {
        entry.settled = true;
        entry.reject(superseded());
      }
    }
    this.entries.clear();
    this.queue.length = 0;
    this.idle.length = 0;
    void commands.mediaPdfClose(this.token).catch(() => {});
  }
}

export interface OpenPdf {
  path: string;
  info: PdfViewInfo;
  pages: PdfPages;
}

/** "1 page", "12 pages". */
export function pageCountLabel(count: number): string {
  return `${count.toLocaleString("en-US")} ${count === 1 ? "page" : "pages"}`;
}
