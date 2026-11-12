// Workbench shell in the real app: renders without console errors, command
// palette (Ctrl+Shift+P, filter, Enter), region shortcuts, keyboard pane
// resizing, visible focus, F6 region navigation, persisted layout/theme across
// a relaunch, small-window auto-collapse, and reference screenshots in both
// themes. Screenshots go to test-results/e2e/ui-shell/ and, when
// EXPOSITION_DEV_HOME is set, to $EXPOSITION_DEV_HOME/ui-reference/.
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, readdir, rm } from "node:fs/promises";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";

const WIDE = { width: 1440, height: 900 };
const SMALL = { width: 800, height: 600 };

/**
 * Close the app the way a user does (WM_CLOSE / SIGTERM) so WebView2 flushes
 * localStorage, then let the harness clean up anything left.
 */
async function closeGracefully(app) {
  const exited = new Promise((resolve) => {
    if (app.child.exitCode !== null) resolve();
    else app.child.once("exit", resolve);
  });
  try {
    if (process.platform === "win32") execFileSync("taskkill", ["/PID", String(app.child.pid)], { stdio: "ignore" });
    else app.child.kill("SIGTERM");
  } catch {
    // already exiting
  }
  await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 15000))]);
  await app.close();
}

/** Wait for enter/exit transitions and finite animations to finish (spinners loop forever). */
async function settle(page) {
  await page.waitForFunction(
    () =>
      !document.querySelector("[data-entering], [data-exiting]") &&
      document.getAnimations().every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity),
    null,
    { timeout: 5000 },
  );
}

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

async function regionVisible(page, name) {
  return (await page.locator(`[data-region="${name}"]`).count()) > 0;
}

async function width(page, selector) {
  const box = await page.locator(selector).boundingBox();
  return box ? Math.round(box.width) : 0;
}

async function setViewport(page, size) {
  await page.setViewportSize(size);
  await page.waitForFunction((w) => window.innerWidth === w, size.width);
  await page.waitForTimeout(150);
}

/**
 * Capture the whole UI reference in one image: hide the side regions, grow the
 * viewport to the reference's scroll height, screenshot, then restore.
 */
async function captureFullReference(page, shot, name) {
  const toggles = ["Control+b", "Control+j", "Control+Alt+b"];
  for (const keys of toggles) await page.keyboard.press(keys);
  const reference = page.getByTestId("ui-reference");
  await reference.evaluate((el) => el.scrollTo(0, 0));
  const height = await reference.evaluate((el) => el.scrollHeight);
  await setViewport(page, { width: 1280, height: Math.min(height + 64, 12000) });
  await page.waitForTimeout(400);
  await shot(page, name);
  await setViewport(page, WIDE);
  for (const keys of toggles) await page.keyboard.press(keys);
}

async function focusRingVisible(page) {
  return page.evaluate(() => {
    const el = document.activeElement;
    if (!el || el === document.body) return { ok: false, reason: "nothing focused" };
    const style = getComputedStyle(el);
    const outline = parseFloat(style.outlineWidth) >= 2 && style.outlineStyle !== "none";
    const ring = style.boxShadow !== "none";
    const focusVisible = el.matches(":focus-visible") || el.hasAttribute("data-focus-visible");
    return {
      ok: focusVisible && (outline || ring),
      reason: `${el.tagName}.${el.className} focusVisible=${focusVisible} outline=${style.outlineStyle} ${style.outlineWidth} shadow=${style.boxShadow}`,
    };
  });
}

export default async function uiShell({ launchApp, artifactsDir, assert }) {
  const shots = [];
  const shot = async (page, name) => {
    await settle(page);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };

  // ---- First launch: fresh profile ----
  let app = await launchApp();
  const dataDir = app.dataDir;
  try {
    const { page } = app;
    await setViewport(page, WIDE);
    await page.getByTestId("workbench").waitFor({ timeout: 15000 });

    // Honest empty states for regions whose features are not built yet. (The
    // bottom and auxiliary panels now host registered views — Jobs, Agent —
    // so they show tabs instead of placeholders.)
    for (const text of ["No folder open", "No editor open"]) {
      assert.ok(await page.getByText(text, { exact: true }).first().isVisible(), `shows "${text}"`);
    }
    assert.ok(await page.getByRole("tab", { name: "Jobs" }).isVisible(), "panel shows registered views");
    assert.ok(await page.getByRole("tab", { name: "Agent" }).isVisible(), "auxiliary region shows the Agent view");

    // Command palette: filter + Enter runs the top match.
    await runCommand(page, "color theme light", /Color Theme: Light/);
    assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "light");
    // Enter runs the top match however the query was edited: here a typo
    // corrected with Backspace (react-aria moves its highlight only on forward
    // typing, so Enter used to do nothing and the palette stayed open).
    for (const theme of ["dark", "light"]) {
      await page.keyboard.press("Control+Shift+P");
      const input = page.getByRole("textbox", { name: "Command" });
      await input.waitFor({ state: "visible", timeout: 5000 });
      await page.keyboard.type(`color theme ${theme}x`);
      await page.keyboard.press("Backspace");
      await page.keyboard.press("Enter");
      const closed = await input.waitFor({ state: "detached", timeout: 5000 }).then(
        () => true,
        () => false,
      );
      assert.ok(closed, `Enter after Backspace runs the top match ("color theme ${theme}")`);
      assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), theme);
    }
    await page.locator("[data-testid=app-info]").waitFor({ timeout: 5000 });
    await shot(page, "workbench-light");

    // Region shortcuts.
    await page.keyboard.press("Control+b");
    assert.equal(await regionVisible(page, "sidebar"), false, "Ctrl+B hides the sidebar");
    await page.keyboard.press("Control+b");
    assert.equal(await regionVisible(page, "sidebar"), true, "Ctrl+B shows the sidebar");
    await page.keyboard.press("Control+j");
    assert.equal(await regionVisible(page, "panel"), false, "Ctrl+J hides the panel");
    await page.keyboard.press("Control+j");
    assert.equal(await regionVisible(page, "panel"), true, "Ctrl+J shows the panel");
    await page.keyboard.press("Control+Alt+b");
    assert.equal(await regionVisible(page, "auxiliary"), false, "Ctrl+Alt+B hides the auxiliary panel");
    await page.keyboard.press("Control+Alt+b");
    assert.equal(await regionVisible(page, "auxiliary"), true, "Ctrl+Alt+B shows the auxiliary panel");

    // F6 moves between regions, and keyboard focus is visibly indicated.
    await page.locator("[data-testid=workbench]").click({ position: { x: 600, y: 400 } });
    const visited = [];
    for (let i = 0; i < 6; i++) {
      await page.keyboard.press("F6");
      visited.push(await page.evaluate(() => document.activeElement?.closest("[data-region]")?.getAttribute("data-region")));
    }
    for (const region of ["activitybar", "sidebar", "editor", "panel", "auxiliary", "statusbar"]) {
      assert.ok(visited.includes(region), `F6 reaches ${region} (visited ${visited.join(", ")})`);
    }
    // F6 into the activity rail: its first button shows the focus ring.
    while ((await page.evaluate(() => document.activeElement?.closest("[data-region]")?.dataset.region)) !== "activitybar") {
      await page.keyboard.press("F6");
    }
    const ring = await focusRingVisible(page);
    assert.ok(ring.ok, `focus ring visible on keyboard focus: ${ring.reason}`);

    // Keyboard pane resize: F6 to the sidebar, Tab to its splitter, ArrowRight.
    const before = await width(page, '[data-region="sidebar"]');
    await page.keyboard.press("F6");
    assert.equal(await page.evaluate(() => document.activeElement?.closest("[data-region]")?.dataset.region), "sidebar");
    // Tab past the sidebar view's own controls (e.g. the Files view's Open Folder).
    for (let i = 0; i < 20 && (await page.evaluate(() => document.activeElement?.id)) !== "wb-sidebar-handle"; i++) {
      await page.keyboard.press("Tab");
    }
    assert.equal(await page.evaluate(() => document.activeElement?.id), "wb-sidebar-handle", "Tab reaches the sidebar splitter");
    const handleRing = await page.evaluate(() => {
      const el = document.getElementById("wb-sidebar-handle");
      return { state: el?.dataset.separator, shadow: el ? getComputedStyle(el).boxShadow : "none" };
    });
    assert.equal(handleRing.state, "focus", "splitter reports focus");
    assert.notEqual(handleRing.shadow, "none", "splitter focus is visible");
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowRight");
    await page.waitForTimeout(150);
    const after = await width(page, '[data-region="sidebar"]');
    assert.ok(after > before + 40, `ArrowRight widens the sidebar (${before} -> ${after})`);
    await shot(page, "splitter-focus-light");
    // Move focus off the splitter (pointer down on inert content blurs it).
    await page.getByText("No editor open", { exact: true }).click();

    // Command palette reference screen.
    await page.keyboard.press("Control+Shift+P");
    const paletteInput = page.getByRole("textbox", { name: "Command" });
    await paletteInput.waitFor({ state: "visible", timeout: 5000 });
    assert.ok(await paletteInput.evaluate((el) => el === document.activeElement), "palette input is focused");
    await paletteInput.fill("toggle");
    await page.locator(".wb-palette__item").first().waitFor();
    await shot(page, "palette-light");
    await page.keyboard.press("Escape");
    await page.getByRole("textbox", { name: "Command" }).waitFor({ state: "detached" });

    // UI reference (light), opened from the palette.
    await runCommand(page, "ui reference", /Developer: Show UI Reference/);
    const reference = page.getByTestId("ui-reference");
    await reference.waitFor({ timeout: 10000 });
    await page.locator("[data-testid=monaco-sample] .monaco-editor").waitFor({ timeout: 15000 });
    await shot(page, "ui-reference-light");

    // Dialog: the shared unsaved-changes confirmation (Escape = Cancel).
    await page.getByRole("button", { name: "Close dirty file…" }).click();
    const alert = page.getByRole("alertdialog", { name: "Save changes to analysis.py?" });
    await alert.waitFor();
    assert.ok(
      await page.getByRole("button", { name: "Save", exact: true }).evaluate((el) => el === document.activeElement),
      "default action is focused",
    );
    await shot(page, "dialog-unsaved-light");
    await page.keyboard.press("Escape");
    await alert.waitFor({ state: "detached" });
    await page.getByText("Result: cancel").waitFor();

    // Notifications: rendered as a stack and announced through live regions.
    await page.getByRole("button", { name: "Success", exact: true }).click();
    await page.getByRole("button", { name: "Danger", exact: true }).click();
    await page.getByRole("region", { name: "Notifications" }).waitFor();
    const liveText = (id) => page.evaluate((testId) => document.querySelector(`[data-testid=${testId}]`)?.textContent ?? "", id);
    await page.waitForFunction(() => document.querySelector("[data-testid=live-assertive]")?.textContent?.includes("Build failed"));
    assert.match(await liveText("live-assertive"), /Build failed/, "danger announced assertively");
    assert.match(await liveText("live-polite"), /Build succeeded/, "success announced politely");
    await shot(page, "notifications-light");
    for (const title of ["Build failed", "Build succeeded"]) {
      const dismissButton = page.getByRole("button", { name: `Dismiss notification: ${title}` });
      if (await dismissButton.count()) await dismissButton.click(); // success may have auto-dismissed
    }
    await page.getByRole("region", { name: "Notifications" }).waitFor({ state: "detached" });

    await captureFullReference(page, shot, "ui-reference-light-full");

    // Two editor groups: split right (Ctrl+\), resize between them, close the right group's tab (Ctrl+W).
    await page.getByRole("tab", { name: "UI Reference" }).click();
    await page.keyboard.press("Control+Backslash");
    const secondary = page.locator('[data-group="secondary"]');
    await secondary.getByTestId("ui-reference").waitFor({ timeout: 10000 });
    assert.equal(await page.locator('[data-group="primary"] .wb-editor-group__header [role="tab"]').count(), 1, "left group keeps its tab");
    const leftBefore = await width(page, '[data-group="primary"]');
    await page.getByRole("separator", { name: "Resize editor groups" }).focus();
    await page.keyboard.press("ArrowLeft");
    await page.waitForTimeout(100);
    const leftAfter = await width(page, '[data-group="primary"]');
    assert.ok(leftAfter < leftBefore - 10, `ArrowLeft narrows the left group (${leftBefore} -> ${leftAfter})`);
    await shot(page, "editor-split-light");
    await secondary.getByRole("tab", { name: "UI Reference" }).click();
    await page.keyboard.press("Control+w");
    await secondary.waitFor({ state: "detached" });
    assert.equal(await page.locator('[data-group="primary"] .wb-editor-group__header [role="tab"]').count(), 1, "closing in one group keeps the other");

    // Dark theme.
    await runCommand(page, "color theme dark", /Color Theme: Dark/);
    assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dark");
    const monacoBg = await page
      .locator("[data-testid=monaco-sample] .monaco-editor-background")
      .evaluate((el) => getComputedStyle(el).backgroundColor);
    assert.equal(monacoBg, "rgb(31, 32, 36)", "Monaco follows the app theme");
    await reference.evaluate((el) => el.scrollTo(0, 0));
    await shot(page, "ui-reference-dark");
    await captureFullReference(page, shot, "ui-reference-dark-full");

    // Close the reference (Ctrl+W) and capture the dark workbench.
    await page.getByRole("tab", { name: "UI Reference" }).click();
    await page.keyboard.press("Control+w");
    await page.getByText("No editor open", { exact: true }).waitFor();
    await shot(page, "workbench-dark");

    // Distinct layout to persist: auxiliary hidden, wider sidebar (above), dark theme.
    await page.keyboard.press("Control+Alt+b");
    assert.equal(await regionVisible(page, "auxiliary"), false);

    assert.deepEqual(consoleErrors(app), [], "no console errors during the first session");
  } finally {
    await closeGracefully(app);
  }

  // ---- Relaunch with the same data dir: layout and theme persist ----
  app = await launchApp({ dataDir });
  try {
    const { page } = app;
    await setViewport(page, WIDE);
    await page.getByTestId("workbench").waitFor({ timeout: 15000 });
    assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), "dark", "theme persisted");
    assert.equal(await regionVisible(page, "auxiliary"), false, "hidden auxiliary panel persisted");
    assert.equal(await regionVisible(page, "sidebar"), true);
    assert.equal(await regionVisible(page, "panel"), true);
    const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("exposition.workbench.layout") ?? "{}"));
    const sidebarWidth = await width(page, '[data-region="sidebar"]');
    assert.ok(Math.abs(sidebarWidth - stored.sidebar.size) <= 2, `sidebar width restored (${sidebarWidth} vs ${stored.sidebar.size})`);
    assert.ok(stored.sidebar.size > 300, `resized sidebar width persisted (${stored.sidebar.size})`);

    // Small window: auxiliary then sidebar auto-collapse; preferences unchanged.
    await page.keyboard.press("Control+Alt+b");
    assert.equal(await regionVisible(page, "auxiliary"), true);
    await setViewport(page, { width: 1000, height: 700 });
    assert.equal(await regionVisible(page, "auxiliary"), false, "auxiliary auto-collapses below 1100px");
    assert.equal(await regionVisible(page, "sidebar"), true, "sidebar stays at 1000px");
    await setViewport(page, SMALL);
    assert.equal(await regionVisible(page, "sidebar"), false, "sidebar auto-collapses below 860px");
    assert.equal(await regionVisible(page, "panel"), true);
    await shot(page, "small-window-dark");
    await page.keyboard.press("Control+b");
    assert.equal(await regionVisible(page, "sidebar"), true, "Ctrl+B opens the sidebar in a small window");
    await page.keyboard.press("Control+b");
    await setViewport(page, WIDE);
    assert.equal(await regionVisible(page, "auxiliary"), true, "auxiliary returns when the window widens");
    assert.equal(await regionVisible(page, "sidebar"), true, "sidebar returns when the window widens");

    await runCommand(page, "color theme light", /Color Theme: Light/);
    await setViewport(page, SMALL);
    await shot(page, "small-window-light");
    await setViewport(page, WIDE);

    assert.deepEqual(consoleErrors(app), [], "no console errors after relaunch");
  } finally {
    await app.close();
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference");
    await mkdir(target, { recursive: true });
    const names = shots.map((path) => path.split(/[\\/]/).pop());
    for (const stale of (await readdir(target)).filter((f) => f.endsWith(".png") && !names.includes(f))) {
      await rm(join(target, stale));
    }
    for (const [i, path] of shots.entries()) await copyFile(path, join(target, names[i]));
    console.log(`copied ${shots.length} screenshots to ${target}`);
  } else {
    console.log("EXPOSITION_DEV_HOME not set; screenshots left in test-results only");
  }
}
