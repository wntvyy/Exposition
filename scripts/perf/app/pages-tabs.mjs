// Page tabs: with N documents open in Page mode and one of them displayed,
// how many builds and page-image requests one edit causes, and how long a
// hidden tab takes to show a current build when it is displayed again.
//   small: 3 documents of 10 pages; large: 5 documents of 50 pages.
import { join } from "node:path";

import { openFromTree, openWorkspace, tabLocator } from "../../e2e/workspace-helpers.mjs";
import {
  documentOf,
  insertBefore,
  instrument,
  openInPageMode,
  pageViewSettled,
  takeCounters,
  writePagesProject,
} from "../fixtures-pages.mjs";

const SCALES = { small: { documents: 3, pages: 10 }, large: { documents: 5, pages: 50 }, stress: { documents: 8, pages: 200 } };
const RUNS = 5;
/** Time for work caused by an edit to show up after the displayed view settled. */
const QUIET_MS = 1500;

export default async function pagesTabs(ctx) {
  const dims = SCALES[ctx.scale] ?? SCALES.small;
  const root = join(ctx.workDir, "project");
  const paths = writePagesProject(root, dims);
  const app = await ctx.launchApp();
  const { page } = app;
  try {
    await openWorkspace(page, root);
    await instrument(page);
    for (const path of paths) {
      await openInPageMode(page, path);
      await pageViewSettled(page, (await documentOf(page, path)).revision);
    }
    // The last document is displayed; the others are hidden Page tabs.
    const shown = paths.at(-1);
    await ctx.sleep(QUIET_MS);
    await takeCounters(page);

    // 1. One edit in the displayed document.
    let samples = [];
    let counters = null;
    for (let i = 0; i < RUNS + 1; i++) {
      const started = performance.now();
      const revision = await insertBefore(page, shown, "anchor1 ", "x");
      await pageViewSettled(page, revision);
      const elapsed = performance.now() - started;
      await ctx.sleep(QUIET_MS);
      counters = await takeCounters(page);
      if (i > 0) samples.push(elapsed);
    }
    ctx.record({ bench: "app.pages.tabs.edit_displayed_document", scale: dims, samples_ms: samples, counters });

    // 2. One edit in a file no document reads, with every Page tab hidden.
    await openFromTree(page, "notes.txt");
    await page.locator('[data-testid="source-editor"][data-path="notes.txt"] .monaco-editor').waitFor({ timeout: 20_000 });
    await ctx.sleep(QUIET_MS);
    await takeCounters(page);
    samples = [];
    for (let i = 0; i < RUNS; i++) {
      const started = performance.now();
      await insertBefore(page, "notes.txt", "notes", "x");
      samples.push(performance.now() - started);
      await ctx.sleep(QUIET_MS);
      counters = await takeCounters(page);
    }
    ctx.record({ bench: "app.pages.tabs.edit_unrelated_all_hidden", scale: dims, samples_ms: samples, counters });

    // 3. Edit a hidden document (through the document service, as an agent
    //    or another view would), then display its tab: time until it shows a
    //    build of the current revision.
    samples = [];
    for (let i = 0; i < RUNS + 1; i++) {
      const path = paths[i % (paths.length - 1)];
      const revision = await insertBefore(page, path, "anchor1 ", "y");
      await ctx.sleep(QUIET_MS);
      await takeCounters(page);
      const started = performance.now();
      await tabLocator(page, path).click();
      await pageViewSettled(page, revision);
      const elapsed = performance.now() - started;
      await ctx.sleep(QUIET_MS);
      counters = await takeCounters(page);
      if (i > 0) samples.push(elapsed);
      await tabLocator(page, "notes.txt").click();
      await ctx.sleep(300);
    }
    ctx.record({ bench: "app.pages.tabs.show_hidden_tab", scale: dims, samples_ms: samples, counters });
    ctx.record({ bench: "app.pages.tabs.end_state", scale: dims, counters: await ctx.metrics(app) });
  } finally {
    await app.close();
  }
}
