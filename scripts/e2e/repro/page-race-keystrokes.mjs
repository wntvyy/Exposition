#!/usr/bin/env node
// Reproduction (M6 acceptance finding): page-view typing in Split view while a
// stale agent document_apply lands before the caret can drop the keystrokes
// that follow; the page view then says "The document changed; click to place
// the caret again". Nothing is corrupted (Monaco equals the service text), but
// the typed characters are lost.
//
// Not a suite scenario. Usage (debug or release build):
//   npm run e2e -- m6-paper-project      # leaves a project with figures behind
//   node scripts/e2e/repro/page-race-keystrokes.mjs <project dir> [attempts]
// <project dir>: the "project" of test-results/e2e/m6-paper-project/results.json
// (or any Typst project whose main.typ contains "This short paper" and
// "dominates for large"). Set EXPOSITION_E2E_EXECUTABLE for another build.
// Each attempt runs in a fresh app on a fresh copy of the project and prints
// whether every typed character arrived. It is timing dependent: in the full
// m6-paper-project scenario it happened in 3 of 3 runs; in isolation on the m6
// paper (with figures) in 1 of 8 to 1 of 2 attempts; never on the small
// typst-basic fixture (debug build, Ryzen 7 9800X3D).
import { cp, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { launchApp } from "../app.mjs";
import { invoke, openFromTree } from "../workspace-helpers.mjs";

const source = process.argv[2];
const attempts = Number(process.argv[3] ?? 8);
if (!source) {
  console.error("usage: node scripts/e2e/repro/page-race-keystrokes.mjs <project dir> [attempts]");
  process.exit(2);
}

async function attempt(n) {
  const root = join(await mkdtemp(join(tmpdir(), "xp-race-")), "project");
  await cp(source, root, { recursive: true, filter: (p) => !/[\\/](\.lake|stress)([\\/]|$)/.test(p) });
  const app = await launchApp();
  const { page } = app;
  try {
    await invoke(page, "workspace_open", { path: root });
    await openFromTree(page, "main.typ");
    await page.locator(".ui-segmented__item", { hasText: "Split" }).first().click();
    await page.waitForFunction(() => document.querySelector('[data-testid="page-editor"]')?.dataset.build);
    const doc = (await invoke(page, "document_list")).find((d) => d.path === "main.typ").id;
    const state = () => invoke(page, "document_state", { document: doc });
    const base = (await state()).text;
    const caretAt = base.indexOf("dominates for large");
    const before = base.indexOf("This short paper");
    // After the failure the page view shows no caret at all.
    const caret = () =>
      page.locator('[data-testid="page-caret"]').getAttribute("data-offset", { timeout: 1000 }).catch(() => "none (caret gone)");

    // Click on the page right before "dominates".
    const build = await page.locator('[data-testid="page-editor"]').getAttribute("data-build");
    const g = await invoke(page, "page_caret", { build, path: "main.typ", offset: caretAt, affinity: "downstream" });
    const el = page.locator(`[data-testid="page"][data-page="${g.page}"]`);
    await el.evaluate(
      (pageEl, [top, x]) => {
        const scroller = pageEl.closest('[data-testid="page-scroller"]');
        const scale = pageEl.getBoundingClientRect().width / Number(pageEl.dataset.widthPt);
        const view = scroller.getBoundingClientRect();
        scroller.scrollTop += pageEl.getBoundingClientRect().top + top * scale - (view.top + view.height / 2);
        scroller.scrollLeft += pageEl.getBoundingClientRect().left + x * scale - (view.left + view.width / 2);
      },
      [g.top, g.x],
    );
    const box = await el.boundingBox();
    const scale = box.width / Number(await el.getAttribute("data-width-pt"));
    await page.mouse.click(box.x + (g.x + 0.2) * scale, box.y + ((g.top + g.bottom) / 2) * scale);
    await page.waitForFunction((o) => document.querySelector('[data-testid="page-caret"]')?.getAttribute("data-offset") === String(o), caretAt);

    const apply = (baseRevision, start, text, end = start) =>
      invoke(page, "document_apply", {
        request: {
          id: `agent-${n}-${Math.random()}`,
          origin: { kind: "agent", taskId: "repro" },
          edits: [{ document: doc, baseRevision, patches: [{ range: { start, end }, text }] }],
          undo: { kind: "newGroup" },
          conflict: "rebaseDisjoint",
          label: null,
          selection: null,
        },
      }).then((v) => v, (e) => ({ error: e }));

    const r0 = (await state()).info.revision;
    await page.keyboard.type("clearly ");
    await page.waitForFunction((o) => document.querySelector('[data-testid="page-caret"]')?.getAttribute("data-offset") === String(o), caretAt + 8);
    await apply(r0, before, "Agent remark one. "); // stale, disjoint: rebased
    await apply(r0, caretAt - 5, "OVERLAP", caretAt + 4); // stale, overlapping: refused
    const r1 = (await state()).info.revision;
    await Promise.all([page.keyboard.type("visibly "), apply(r1, before + 22, "Agent remark two: ")]);
    await page.waitForTimeout(1500);

    const text = (await state()).text;
    const expected =
      base.slice(0, before) +
      "Agent remark one. " +
      base.slice(before, before + 4) +
      "Agent remark two: " +
      base.slice(before + 4, caretAt) +
      "clearly visibly " +
      base.slice(caretAt);
    const i = text.indexOf("dominates for large");
    const message = (await page.locator('[data-testid="page-editor"]').innerText()).split("\n").find((l) => /caret/i.test(l)) ?? "";
    console.log(
      `attempt ${n}: ${text === expected ? "ok" : "KEYSTROKES LOST"}; typed run ${JSON.stringify(text.slice(i - 17, i))}; caret ${await caret()}; ${message}`,
    );
    return text === expected;
  } finally {
    await app.close();
  }
}

let lost = 0;
for (let n = 1; n <= attempts; n++) if (!(await attempt(n))) lost += 1;
console.log(`${lost} of ${attempts} attempts lost keystrokes`);
process.exit(lost ? 1 : 0);
