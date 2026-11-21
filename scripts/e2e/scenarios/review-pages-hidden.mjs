// Review of hidden Page tabs in the real app: a Page tab hidden behind another
// tab does not build; shown again it must catch up with what changed while it
// was hidden (an included file edited in another tab, a broken include, the
// fix), also after rapid tab switching, and it must go on rebuilding after
// that. What the status item and the hidden view say while hidden is recorded
// in the run's observations (test-results/e2e/review-pages-hidden/observations.json).
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { focusEditor, invoke, openFromTree, openInEditor, openWorkspace, tabLocator, waitFor } from "../workspace-helpers.mjs";

const ENTRY = "main.typ";
const INCLUDED = "chapter.typ";

export default async function reviewPagesHidden({ launchApp, artifactsDir, assert }) {
  const workspace = join(await mkdtemp(join(tmpdir(), "exposition-review-hidden-")), "project");
  await mkdir(workspace, { recursive: true });
  await writeFile(join(workspace, ENTRY), '= Title\n\n#include "chapter.typ"\n\nMain body text.\n');
  await writeFile(join(workspace, INCLUDED), "Chapter text.\n");
  const app = await launchApp();
  const { page } = app;
  const observations = {};
  try {
    await openWorkspace(page, workspace);
    // Both files are open; the entry is opened last and switched to Page mode.
    await openInEditor(page, INCLUDED);
    await openFromTree(page, ENTRY);
    const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
    await modeButton("Page").waitFor({ timeout: 10000 });
    await modeButton("Page").click();
    const editor = page.locator('[data-testid="page-editor"]');
    await editor.waitFor({ timeout: 10000 });
    const view = () =>
      editor.evaluate((el) => ({
        build: el.dataset.build,
        revision: el.dataset.buildRevision,
        stale: el.dataset.stale,
        failed: el.dataset.failed,
        pages: el.querySelectorAll('[data-testid="page"]').length,
        images: el.querySelectorAll('[data-testid="page"] img').length,
        ref: el.querySelector('[data-testid="page"]')?.getAttribute("data-render-ref") ?? null,
      }));
    const status = () =>
      page.evaluate(() => {
        const el = document.querySelector('[data-testid="build-status"]');
        return el ? { status: el.getAttribute("data-status"), text: el.textContent } : null;
      });
    const documents = async () => Object.fromEntries((await invoke(page, "document_list")).map((d) => [d.path, d]));
    const text = async (path) => (await invoke(page, "document_state", { document: (await documents())[path].id })).text;
    /** The entry's page view shows a successful build of the current sources that has `needle` of the included file. */
    const current = async (needle, message) =>
      waitFor(
        async () => {
          const v = await view();
          if (v.stale !== "false" || v.failed !== "false" || !v.build || v.images === 0) return false;
          const offset = (await text(INCLUDED)).indexOf(needle);
          if (offset < 0) return false;
          const caret = await invoke(page, "page_caret", { build: v.build, path: INCLUDED, offset, affinity: "downstream" });
          return caret ? v : false;
        },
        { timeout: 20000, message },
      );
    let counter = 0;
    /** Append to the included file through the document service (as another view or an agent would). */
    const append = async (addition) => {
      const doc = (await documents())[INCLUDED];
      const state = await invoke(page, "document_state", { document: doc.id });
      const at = state.text.length;
      await invoke(page, "document_apply", {
        request: {
          id: `review-${++counter}`,
          origin: { kind: "source" },
          edits: [{ document: doc.id, baseRevision: state.info.revision, patches: [{ range: { start: at, end: at }, text: addition }] }],
          undo: { kind: "newGroup" },
          conflict: "reject",
          label: null,
          selection: null,
        },
      });
    };

    const first = await current("Chapter text", "the first build");
    assert.equal(first.pages, 1);

    // 1. The Page tab is hidden behind the included file's source tab; that file is edited.
    await tabLocator(page, INCLUDED).click();
    await focusEditor(page, INCLUDED, "end");
    await page.keyboard.type("Hidden addition.");
    await waitFor(async () => (await text(INCLUDED)).includes("Hidden addition."), { message: "the typed text" });
    await page.waitForTimeout(2500);
    observations.hiddenAfterValidEdit = { view: await view(), status: await status() };
    assert.equal(observations.hiddenAfterValidEdit.view.build, first.build, "a hidden Page tab shows no new build");
    // ... and broken.
    await page.keyboard.type(" #nosuchfunction()");
    await waitFor(async () => (await text(INCLUDED)).includes("#nosuchfunction()"), { message: "the typed error" });
    await page.waitForTimeout(2500);
    observations.hiddenAfterBrokenEdit = {
      view: await view(),
      status: await status(),
      problems: await page.locator('[data-testid="problem"]').count(),
    };
    await page.screenshot({ path: join(artifactsDir, "01-hidden-broken-include.png") });

    // 2. Shown again: it builds, the build fails, the pages are marked as from an earlier revision.
    await tabLocator(page, ENTRY).click();
    const failed = await waitFor(
      async () => {
        const v = await view();
        return v.failed === "true" && v.stale === "true" ? v : false;
      },
      { timeout: 20000, message: "the failed build after showing the tab" },
    );
    assert.equal(failed.build, first.build, "the last good pages stay");
    observations.shownBroken = { view: failed, status: await status() };
    await page.screenshot({ path: join(artifactsDir, "02-shown-broken-include.png") });

    // 3. Fixed while hidden, then rapid tab switching ending on the Page tab.
    await tabLocator(page, INCLUDED).click();
    await focusEditor(page, INCLUDED, "end");
    for (let i = 0; i < " #nosuchfunction()".length; i++) await page.keyboard.press("Backspace");
    await waitFor(async () => !(await text(INCLUDED)).includes("#nosuch"), { message: "the error removed" });
    for (let i = 0; i < 12; i++) await tabLocator(page, i % 2 === 0 ? ENTRY : INCLUDED).click({ delay: 0 });
    await tabLocator(page, ENTRY).click();
    const fixed = await current("Hidden addition.", "the pages with the text typed while the tab was hidden");
    assert.notEqual(fixed.build, first.build);
    assert.notEqual(fixed.ref, first.ref, "the page changed");
    await page.screenshot({ path: join(artifactsDir, "03-shown-current.png") });

    // 4. The displayed view goes on rebuilding: after every round of hiding
    // and showing (at several speeds), a change of the included file reaches the pages.
    observations.rounds = [];
    for (let round = 0; round < 8; round++) {
      const pause = [0, 0, 5, 15, 30, 60, 120, 0][round];
      for (let i = 0; i < 2 + (round % 3); i++) {
        await tabLocator(page, INCLUDED).click();
        if (pause) await page.waitForTimeout(pause);
        await tabLocator(page, ENTRY).click();
        if (pause) await page.waitForTimeout(pause);
      }
      const marker = `Round ${round} text.`;
      await append(` ${marker}`);
      const shown = await current(marker, `round ${round}: the change made after switching tabs (pause ${pause} ms)`);
      observations.rounds.push({ round, pause, build: shown.build });
    }
    await page.screenshot({ path: join(artifactsDir, "04-after-rounds.png") });
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
}
