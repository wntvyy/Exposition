// Memory after opening a long document in Page mode and closing it again,
// repeatedly: JS heap, DOM nodes, listeners and process memory after the first
// cycle and after the last (a bounded warm cache is not a leak; growth per
// cycle is what to look for).
//   small: 50 pages, 6 cycles; large: 200 pages, 10 cycles.
import { join } from "node:path";

import { closeTab, openWorkspace } from "../../e2e/workspace-helpers.mjs";
import { documentOf, instrument, openInPageMode, pageViewSettled, writePagesProject } from "../fixtures-pages.mjs";

const SCALES = { small: { pages: 50, cycles: 6 }, large: { pages: 200, cycles: 10 }, stress: { pages: 500, cycles: 10 } };

export default async function pagesMemory(ctx) {
  const dims = SCALES[ctx.scale] ?? SCALES.small;
  const root = join(ctx.workDir, "project");
  writePagesProject(root, { documents: 1, pages: dims.pages });
  const app = await ctx.launchApp();
  const { page } = app;
  try {
    await openWorkspace(page, root);
    await instrument(page);
    const samples = [];
    let first = null;
    for (let cycle = 0; cycle < dims.cycles; cycle++) {
      const started = performance.now();
      await openInPageMode(page, "main.typ");
      await pageViewSettled(page, (await documentOf(page, "main.typ")).revision);
      samples.push(performance.now() - started);
      await closeTab(page, "main.typ");
      await page.locator('[data-testid="page-editor"]').first().waitFor({ state: "detached", timeout: 20_000 });
      await ctx.sleep(1000);
      if (cycle === 0) first = await ctx.metrics(app);
    }
    const last = await ctx.metrics(app);
    ctx.record({ bench: "app.pages.memory.after_first_cycle", scale: dims, counters: first });
    ctx.record({ bench: "app.pages.memory.after_last_cycle", scale: dims, samples_ms: samples.slice(1), counters: last });
    const growth = {};
    for (const key of Object.keys(last)) {
      if (typeof last[key] === "number" && typeof first[key] === "number") growth[key] = Number((last[key] - first[key]).toFixed(1));
    }
    ctx.record({ bench: "app.pages.memory.growth_first_to_last", scale: dims, counters: growth });
  } finally {
    await app.close();
  }
}
