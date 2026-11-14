// Top-level pages against the UI standard in the real app (M7 lane D review):
// Research, Agents, and Graph with no folder open (honest states), then with a
// project: keyboard page switching (Mod+2/3/4) moves focus into the page, F6
// cycles each page's regions, every page fills its area at wide, maximized, and
// narrow sizes, the narrow window keeps every page usable without horizontal overflow, both themes, no console errors. Screens go to
// test-results/e2e/pages-review/ and, with EXPOSITION_DEV_HOME set, to
// $EXPOSITION_DEV_HOME/ui-reference/pages/.
import { copyFile, cp, mkdir, mkdtemp, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, waitFor } from "../workspace-helpers.mjs";

const WIDE = { width: 1440, height: 900 };
const NARROW = { width: 900, height: 640 };
const MAXIMIZED = { width: 2560, height: 1369 };

async function setViewport(page, size) {
  await page.setViewportSize(size);
  await page.waitForFunction((w) => window.innerWidth === w, size.width);
  await page.waitForTimeout(200);
}

async function settle(page) {
  await page
    .waitForFunction(
      () =>
        !document.querySelector("[data-entering], [data-exiting]") &&
        document.getAnimations().every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity),
      null,
      { timeout: 5000 },
    )
    .catch(() => {});
}

const PAGES = {
  research: { key: "Control+2", testId: "research-page" },
  agents: { key: "Control+3", testId: "agents-page" },
  graph: { key: "Control+4", testId: "graph-page" },
};

async function showPage(page, id) {
  await page.keyboard.press(PAGES[id].key);
  await page.getByTestId(PAGES[id].testId).waitFor({ timeout: 10000 });
  await waitFor(() => page.evaluate((p) => document.querySelector(`[data-page-button="${p}"]`)?.getAttribute("aria-current") === "page", id), {
    message: `${id} is the current page`,
  });
  await waitFor(() => page.evaluate((p) => Boolean(document.activeElement?.closest(`[data-page="${p}"]`)), id), {
    message: `keyboard focus moved into the ${id} page`,
  });
}

/** Regions F6 visits from the activity bar until it wraps (innermost regions only). */
async function f6Regions(page) {
  await page.locator('[data-page-button="editor"]').focus();
  const seen = [];
  for (let i = 0; i < 12; i++) {
    await page.keyboard.press("F6");
    const region = await page.evaluate(() => document.activeElement?.closest("[data-region]")?.getAttribute("data-region") ?? null);
    if (region === "activitybar") break;
    seen.push(region);
  }
  return seen;
}

/** Width of the shown page's root against its page area (a page fills the area at any window size). */
async function fill(page) {
  return page.evaluate(() => {
    const shown = [...document.querySelectorAll(".wb-pages > [data-page]")].find((p) => !p.hasAttribute("hidden"));
    const root = shown?.firstElementChild;
    return { area: Math.round(shown?.getBoundingClientRect().width ?? 0), root: Math.round(root?.getBoundingClientRect().width ?? 0) };
  });
}

async function assertFills(page, assert, id, size) {
  const f = await fill(page);
  assert.ok(f.area > 0 && Math.abs(f.area - f.root) <= 1, `${id} fills its page area at ${size.width}px (${JSON.stringify(f)})`);
}

/** Horizontal overflow of the shown page (content wider than its box). */
async function overflow(page) {
  return page.evaluate(() => {
    const shown = [...document.querySelectorAll(".wb-pages > [data-page]")].find((p) => !p.hasAttribute("hidden"));
    return {
      document: document.documentElement.scrollWidth - window.innerWidth,
      page: shown ? shown.scrollWidth - shown.clientWidth : 0,
    };
  });
}

export default async function pagesReview({ launchApp, artifactsDir, assert, fixture }) {
  const shots = [];
  const shot = async (page, name) => {
    await settle(page);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };
  const base = await mkdtemp(join(tmpdir(), "xp-pages-"));
  const root = join(base, "project");
  await cp(fixture("graph-basic"), root, { recursive: true });

  const app = await launchApp();
  try {
    const { page } = app;
    await setViewport(page, WIDE);
    await page.getByTestId("workbench").waitFor({ timeout: 15000 });
    await runCommand(page, "color theme light", /Color Theme: Light/);

    // --- No folder open: every page says so and offers the way forward ---------
    const names = await page.getByRole("toolbar", { name: "Pages" }).getByRole("button").evaluateAll((els) => els.map((e) => e.getAttribute("aria-label")));
    assert.deepEqual(names, ["Editor", "Research", "Agents", "Graph"], "pages in rail order");
    for (const id of ["research", "agents", "graph"]) {
      await showPage(page, id);
      await assertFills(page, assert, id, WIDE);
      const text = await page.getByTestId(PAGES[id].testId).innerText();
      assert.match(text, /No folder open|Open a project folder|Open Folder/i, `${id} with no folder says so (${text.slice(0, 120)})`);
      await shot(page, `${id}-no-folder-light`);
    }

    // --- With a project -------------------------------------------------------
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.tex"]').waitFor({ state: "attached", timeout: 15000 });
    const expected = {
      research: ["research-header", "research-explorer", "research-sources", "research-output", "research-inspector"],
      agents: ["agents-tasks", "agents-detail"],
      graph: ["graph-header", "graph-toolbar", "graph-main", "graph-inspector"],
    };
    for (const id of ["research", "agents", "graph"]) {
      await showPage(page, id);
      if (id === "graph") await page.getByTestId("graph-canvas").waitFor({ timeout: 15000 });
      if (id === "research") await page.getByTestId("research-enable").waitFor({ timeout: 15000 });
      const regions = await f6Regions(page);
      for (const region of expected[id]) assert.ok(regions.includes(region), `F6 reaches ${region} on ${id} (visited ${regions.join(", ")})`);
      assert.ok(regions.includes("statusbar"), `F6 reaches the status bar from ${id}`);
      assert.ok(!regions.some((r) => ["sidebar", "editor", "panel", "auxiliary"].includes(r)), `the hidden Editor page takes no focus (${regions.join(", ")})`);
      await assertFills(page, assert, id, WIDE);
      await shot(page, `${id}-project-light`);
    }

    // --- Maximized window: every page grows with the window -------------------
    await setViewport(page, MAXIMIZED);
    for (const id of ["research", "agents", "graph"]) {
      await showPage(page, id);
      await assertFills(page, assert, id, MAXIMIZED);
    }

    // --- Narrow window: usable, no horizontal overflow ------------------------
    await setViewport(page, NARROW);
    for (const id of ["research", "agents", "graph"]) {
      await showPage(page, id);
      const o = await overflow(page);
      assert.ok(o.document <= 0 && o.page <= 1, `${id} has no horizontal overflow at ${NARROW.width}px (${JSON.stringify(o)})`);
      await assertFills(page, assert, id, NARROW);
      await shot(page, `${id}-narrow-light`);
    }

    // Research in a narrow window: the inspector is a drawer that opens for a selection and closes.
    await showPage(page, "research");
    await page.getByRole("button", { name: "Enable Research" }).click();
    await page.getByRole("button", { name: "New experiment" }).click();
    const drawer = page.getByTestId("research-inspector-drawer");
    await drawer.waitFor({ timeout: 10000 });
    assert.equal(await drawer.getAttribute("data-region"), "research-inspector");
    await shot(page, "research-narrow-inspector-light");
    await drawer.getByRole("button", { name: "Close inspector" }).click();
    await drawer.waitFor({ state: "detached" });

    // --- Dark theme -------------------------------------------------------------
    await runCommand(page, "color theme dark", /Color Theme: Dark/);
    for (const id of ["research", "agents", "graph"]) {
      await showPage(page, id);
      await shot(page, `${id}-narrow-dark`);
    }
    await setViewport(page, WIDE);
    for (const id of ["research", "agents", "graph"]) {
      await showPage(page, id);
      await shot(page, `${id}-project-dark`);
    }

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await app.close();
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "pages");
    await mkdir(target, { recursive: true });
    const names = shots.map((path) => path.split(/[\\/]/).pop());
    for (const stale of (await readdir(target)).filter((f) => f.endsWith(".png") && !names.includes(f))) await rm(join(target, stale));
    for (const [i, path] of shots.entries()) await copyFile(path, join(target, names[i]));
    console.log(`copied ${shots.length} screenshots to ${target}`);
  }
  await rm(base, { recursive: true, force: true }).catch(() => {});
}
