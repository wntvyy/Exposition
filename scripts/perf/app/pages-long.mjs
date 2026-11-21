// A long document in Page mode: page images requested and mounted against the
// pages in the viewport, time until the viewport shows the build on open and
// after a one-character edit on the first and on a late page.
//   small: 50 pages; large: 200 pages; stress: 500 pages.
import { join } from "node:path";

import { openWorkspace } from "../../e2e/workspace-helpers.mjs";
import {
  documentOf,
  insertBefore,
  instrument,
  openInPageMode,
  pageViewSettled,
  takeCounters,
  writePagesProject,
} from "../fixtures-pages.mjs";

const PAGES = { small: 50, large: 200, stress: 500 };
const RUNS = 5;
const QUIET_MS = 1500;

export default async function pagesLong(ctx) {
  const pages = PAGES[ctx.scale] ?? PAGES.small;
  const scale = { pages };
  const root = join(ctx.workDir, "project");
  writePagesProject(root, { documents: 1, pages });
  const app = await ctx.launchApp();
  const { page } = app;
  try {
    await openWorkspace(page, root);
    await instrument(page);
    const started = performance.now();
    await openInPageMode(page, "main.typ");
    const view = await pageViewSettled(page, (await documentOf(page, "main.typ")).revision);
    const opened = performance.now() - started;
    await ctx.sleep(QUIET_MS);
    const onOpen = await takeCounters(page);
    const after = await pageViewSettled(page, (await documentOf(page, "main.typ")).revision);
    ctx.record({
      bench: "app.pages.long.open",
      scale,
      samples_ms: [opened],
      counters: {
        pageImages: onOpen.pageImages,
        pageImageBytes: onOpen.pageImageBytes,
        builds: onOpen.builds,
        pageElements: after.pages,
        mountedImages: after.images,
        pagesInViewport: view.pagesInViewport,
      },
    });
    ctx.record({ bench: "app.pages.long.open_state", scale, counters: await ctx.metrics(app) });

    for (const [name, anchor] of [
      ["first_page", "anchor0 "],
      ["late_page", `anchor${Math.floor(pages * 0.75)} `],
    ]) {
      const samples = [];
      let counters = null;
      for (let i = 0; i < RUNS + 1; i++) {
        const begun = performance.now();
        const revision = await insertBefore(page, "main.typ", anchor, "x");
        const settled = await pageViewSettled(page, revision);
        const elapsed = performance.now() - begun;
        await ctx.sleep(QUIET_MS);
        counters = { ...(await takeCounters(page)), mountedImages: settled.images, pagesInViewport: settled.pagesInViewport };
        if (i > 0) samples.push(elapsed);
      }
      ctx.record({ bench: `app.pages.long.edit_${name}`, scale, samples_ms: samples, counters });
    }

    // Scroll through the whole document in viewport steps: images requested
    // and images mounted at the end.
    const begun = performance.now();
    const steps = await page.evaluate(async () => {
      const view = [...document.querySelectorAll('[data-testid="page-editor"]')].find((e) => e.offsetParent !== null);
      const scroller = view.querySelector('[data-testid="page-scroller"]');
      let count = 0;
      while (scroller.scrollTop + scroller.clientHeight < scroller.scrollHeight - 1) {
        scroller.scrollTop += scroller.clientHeight;
        count += 1;
        await new Promise((r) => setTimeout(r, 60));
      }
      return count;
    });
    const end = await pageViewSettled(page, (await documentOf(page, "main.typ")).revision);
    const scrolled = performance.now() - begun;
    await ctx.sleep(QUIET_MS);
    ctx.record({
      bench: "app.pages.long.scroll_through",
      scale,
      samples_ms: [scrolled],
      counters: { ...(await takeCounters(page)), scrollSteps: steps, mountedImages: end.images, pageElements: end.pages },
    });
    ctx.record({ bench: "app.pages.long.end_state", scale, counters: await ctx.metrics(app) });
  } finally {
    await app.close();
  }
}
