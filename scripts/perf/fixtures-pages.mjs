// Deterministic documents and in-page counters for the page-view measurements
// (`scripts/perf/app/pages-*.mjs`). The Typst document has exactly one page
// per section, the same shape as `crates/exposition-typesetting/tests/perf_pages.rs`.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** A Typst document of exactly `pages` pages: prose, inline and display mathematics. */
export function typstPages({ pages, name = "doc" }) {
  let text =
    '#set page(width: 14cm, height: 18cm, margin: 1.5cm)\n#set par(justify: true)\n#set heading(numbering: "1.")\n#set math.equation(numbering: "(1)")\n\n';
  for (let i = 0; i < pages; i++) {
    text += `= ${name} section ${i} <sec-${i}>\nLet the map be continuous on a compact set, with $a_${i}^2 + b_${i}^2 = c_${i}^2$ and $alpha + beta <= gamma$ inline. anchor${i} plain prose follows here.\n\n`;
    for (let j = 0; j < 5; j++) {
      text += `$ sum_(k = 0)^n k^${j} = (n (n + 1)) / 2 + integral_0^${i} x^${j} dif x $ <eq-${i}-${j}>\n\n`;
    }
    text += "Hence the supremum is attained by some point of the domain, which proves the claim.\n";
    if (i + 1 < pages) text += "#pagebreak()\n\n";
  }
  return text;
}

/**
 * Writes `main.typ`, `doc1.typ` … `doc<count-1>.typ` (each `pages` pages) and
 * an unrelated `notes.txt` into `root`; returns the document paths.
 */
export function writePagesProject(root, { documents = 1, pages }) {
  mkdirSync(root, { recursive: true });
  const paths = [];
  for (let d = 0; d < documents; d++) {
    const path = d === 0 ? "main.typ" : `doc${d}.typ`;
    writeFileSync(join(root, path), typstPages({ pages, name: path.replace(".typ", "") }));
    paths.push(path);
  }
  writeFileSync(join(root, "notes.txt"), "notes\n");
  return paths;
}

/**
 * Count, in the page: finished builds per entry, build status events, and
 * `page_image` requests with their bytes. Commands travel as `fetch` requests
 * to the IPC origin (`window.__TAURI_INTERNALS__` itself is not writable), so
 * `fetch` is wrapped and requests are told apart by their URL.
 */
export async function instrument(page) {
  await page.evaluate(async () => {
    if (window.__perf) return;
    const perf = { builds: [], statuses: 0, ipc: 0, pageImages: 0, pageImageBytes: 0, pageImagesInFlight: 0 };
    window.__perf = perf;
    const original = window.fetch;
    window.fetch = function (input, init) {
      const url = typeof input === "string" ? input : (input?.url ?? "");
      const result = original.call(this, input, init);
      if (/^(https?:\/\/ipc\.localhost|ipc:\/\/localhost)\//.test(url)) {
        perf.ipc += 1;
        if (/\/page_image$/.test(url)) {
          perf.pageImages += 1;
          perf.pageImagesInFlight += 1;
          result
            .then((response) => response.clone().arrayBuffer())
            .then((bytes) => {
              perf.pageImageBytes += bytes.byteLength;
            })
            .catch(() => {})
            .finally(() => {
              perf.pageImagesInFlight -= 1;
            });
        }
      }
      return result;
    };
    const internals = window.__TAURI_INTERNALS__;
    const listen = (event, handler) =>
      internals.invoke("plugin:event|listen", { event, target: { kind: "Any" }, handler: internals.transformCallback(handler) });
    await listen("typesetting:build", (e) => perf.builds.push(e.payload.result.target.entry));
    await listen("typesetting:status", () => {
      perf.statuses += 1;
    });
  });
  // The wrapper must see the application's commands.
  const seen = await page.evaluate(async () => {
    const before = window.__perf.ipc;
    await window.__TAURI_INTERNALS__.invoke("document_list");
    return window.__perf.ipc > before;
  });
  if (!seen) throw new Error("page instrumentation does not see IPC requests");
}

/** Read the counters and reset them. */
export function takeCounters(page) {
  return page.evaluate(() => {
    const perf = window.__perf;
    const out = { builds: perf.builds.length, buildEntries: [...new Set(perf.builds)].length, statuses: perf.statuses, pageImages: perf.pageImages, pageImageBytes: perf.pageImageBytes };
    perf.builds = [];
    perf.statuses = 0;
    perf.pageImages = 0;
    perf.pageImageBytes = 0;
    return out;
  });
}

/** State of the page view that is displayed now (hidden tabs are skipped). */
export function visiblePageView(page) {
  return page.evaluate(async () => {
    const view = [...document.querySelectorAll('[data-testid="page-editor"]')].find((e) => e.offsetParent !== null);
    if (!view) return null;
    const scroller = view.querySelector('[data-testid="page-scroller"]');
    const box = scroller.getBoundingClientRect();
    const pages = [...view.querySelectorAll('[data-testid="page"]')];
    const inViewport = pages.filter((p) => {
      const r = p.getBoundingClientRect();
      return r.bottom > box.top && r.top < box.bottom;
    });
    return {
      build: view.dataset.build || null,
      buildRevision: view.dataset.buildRevision === "" ? null : Number(view.dataset.buildRevision),
      stale: view.dataset.stale === "true",
      failed: view.dataset.failed === "true",
      pages: pages.length,
      images: pages.filter((p) => p.querySelector("img")).length,
      pagesInViewport: inViewport.length,
      viewportImagesLoaded: inViewport.filter((p) => p.querySelector("img")?.complete).length,
    };
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Wait until the displayed page view shows a build of `revision` of its
 * document (not stale) with every page in the viewport loaded, and no page
 * request is in flight. Returns the view state.
 */
export async function pageViewSettled(page, revision, { timeout = 180_000 } = {}) {
  const deadline = Date.now() + timeout;
  let last = null;
  for (;;) {
    last = await visiblePageView(page);
    const inFlight = await page.evaluate(() => window.__perf?.pageImagesInFlight ?? 0);
    if (last && last.build && !last.stale && !last.failed && last.buildRevision === revision && last.pagesInViewport > 0 && last.viewportImagesLoaded === last.pagesInViewport && inFlight === 0) {
      return last;
    }
    if (Date.now() > deadline) throw new Error(`page view did not settle at revision ${revision}: ${JSON.stringify(last)}`);
    await sleep(25);
  }
}

/** The open document of `path`: `{ id, revision }`. */
export async function documentOf(page, path) {
  const list = await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("document_list"));
  const doc = list.find((d) => d.path === path);
  if (!doc) throw new Error(`${path} is not open`);
  return { id: doc.id, revision: doc.revision };
}

let editSerial = 0;
/** Insert `text` before the first `needle` of an open document, as a source edit; returns the new revision. */
export async function insertBefore(page, path, needle, text) {
  const doc = await documentOf(page, path);
  const state = await page.evaluate((id) => window.__TAURI_INTERNALS__.invoke("document_state", { document: id }), doc.id);
  const at = state.text.indexOf(needle);
  if (at < 0) throw new Error(`${needle} not found in ${path}`);
  const outcome = await page.evaluate(
    (request) => window.__TAURI_INTERNALS__.invoke("document_apply", { request }),
    {
      id: `perf-${Date.now()}-${++editSerial}`,
      origin: { kind: "source" },
      edits: [{ document: doc.id, baseRevision: state.info.revision, patches: [{ range: { start: at, end: at }, text }] }],
      undo: { kind: "newGroup" },
      conflict: "reject",
      label: null,
      selection: null,
    },
  );
  return outcome.applied[0].revision;
}

/** Open `path` from the file tree and switch its tab to Page mode. */
export async function openInPageMode(page, path) {
  const row = page.locator(`[data-testid="file-tree"] [data-path="${path}"]`).first();
  await row.waitFor({ timeout: 20_000 });
  await row.locator(".ui-tree-item__title").first().click();
  const button = page.locator(".ui-segmented__item", { hasText: "Page" }).first();
  await button.waitFor({ timeout: 20_000 });
  await button.click();
  await page.locator('[data-testid="page-editor"]:visible').first().waitFor({ timeout: 20_000 });
}
