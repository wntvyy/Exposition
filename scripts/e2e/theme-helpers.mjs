// Four-theme reference screenshots (UI standard V2).
//
// `applyTheme` switches the theme the way a user does (the "Color Theme: …"
// command), so the theme store, Monaco, and theme-dependent icons all follow;
// it then waits for `<html data-theme>`. The palette takes focus, so a
// transient state (an open menu, a focused field) must be re-created after
// the switch: pass `prepare` to `shootThemes`.
//
// Shots go to `<outDir>/<name>-<theme>.png` and, when EXPOSITION_DEV_HOME is
// set, to `$EXPOSITION_DEV_HOME/ui-reference/v2/<area>/`.
import { copyFile, mkdir } from "node:fs/promises";
import { join } from "node:path";

import { runCommand } from "./palette.mjs";

/** [id, label] in the order Settings lists them. */
export const THEMES = [
  ["light", "Light"],
  ["dark", "Dark"],
  ["ocean", "Ocean"],
  ["sage", "Sage"],
];

/** Switch to a built-in theme through its command and wait until it applies. */
export async function applyTheme(page, theme) {
  const label = THEMES.find(([id]) => id === theme)?.[1];
  if (!label) throw new Error(`unknown theme ${theme}`);
  const title = `Color Theme: ${label}`;
  await runCommand(page, title, new RegExp(`^\\s*${title}`));
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
  // Let Monaco and transitions repaint before a screenshot.
  await page.waitForTimeout(150);
}

/**
 * Capture `name` in all four themes. `prepare(page, theme)` runs after each
 * switch (re-open a menu, focus a field, scroll). Returns the written paths.
 */
export async function shootThemes(page, { outDir, area, name, prepare, fullPage = false }) {
  await mkdir(outDir, { recursive: true });
  const published = process.env.EXPOSITION_DEV_HOME ? join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "v2", area) : null;
  if (published) await mkdir(published, { recursive: true });
  const paths = [];
  for (const [theme] of THEMES) {
    await applyTheme(page, theme);
    if (prepare) await prepare(page, theme);
    const path = join(outDir, `${name}-${theme}.png`);
    await page.screenshot({ path, fullPage });
    if (published) await copyFile(path, join(published, `${name}-${theme}.png`));
    paths.push(path);
  }
  return paths;
}
