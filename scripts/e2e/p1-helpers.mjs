// Shared helpers for the P1 project-operations scenarios (scenarios/p1-*.mjs).
// Everything runs on DISPOSABLE temporary projects. Real OS behavior (the
// Windows Recycle Bin, the clipboard file list) is read back through
// PowerShell so the scenarios check what the OS actually holds.
import { execFileSync } from "node:child_process";
import { copyFile, mkdir } from "node:fs/promises";
import { join } from "node:path";

import { waitFor } from "./workspace-helpers.mjs";

/** Run a Windows PowerShell script (returns stdout). */
export function ps(script) {
  return execFileSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8" });
}

/** Recycle Bin items whose original folder is `folder` (absolute): [{ name, from }]. */
export function recycleBinItemsFrom(folder) {
  if (process.platform !== "win32") return [];
  const out = ps(
    "$ErrorActionPreference='SilentlyContinue';" +
      "$bin=(New-Object -ComObject Shell.Application).Namespace(10);" +
      "foreach($i in $bin.Items()){ $from=$i.ExtendedProperty('System.Recycle.DeletedFrom'); Write-Output ($from + '|' + $i.Name) }",
  );
  const norm = (p) => p.replace(/[\\/]+$/, "").toLowerCase();
  return out
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => {
      const at = line.lastIndexOf("|");
      return { from: line.slice(0, at), name: line.slice(at + 1) };
    })
    .filter((item) => norm(item.from) === norm(folder));
}

/** Absolute paths of the OS clipboard's file list (CF_HDROP). */
export function osClipboardFiles() {
  if (process.platform !== "win32") return [];
  return ps("Get-Clipboard -Format FileDropList | ForEach-Object { $_.FullName }")
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** Put files on the OS clipboard as a file list (what Explorer's Copy does). */
export function setOsClipboardFiles(paths) {
  ps(`Set-Clipboard -Path ${paths.map((p) => `'${p.replace(/'/g, "''")}'`).join(",")}`);
}

/** Put plain text on the OS clipboard (no file list). */
export function setOsClipboardText(text) {
  ps(`Set-Clipboard -Value '${text.replace(/'/g, "''")}'`);
}

/** A row of the Files tree. */
export function fileRow(page, path) {
  return page.locator(`[data-testid="file-tree"] [data-path="${path}"]`).first();
}

/** Expand the folders above `path` in the Files tree. */
export async function expandTo(page, path) {
  const parts = path.split("/");
  for (let i = 1; i < parts.length; i++) {
    const dir = parts.slice(0, i).join("/");
    const row = fileRow(page, dir);
    await row.waitFor();
    if ((await row.getAttribute("aria-expanded")) !== "true") await row.locator(".ui-tree-item__chevron").first().click();
  }
  await fileRow(page, path).waitFor();
}

/** Selected paths of the Files tree. */
export async function selectedPaths(page) {
  return page.locator('[data-testid="file-tree"] [aria-selected="true"]').evaluateAll((rows) => rows.map((r) => r.getAttribute("data-path")));
}

/**
 * Select exactly `paths` in the Files tree without opening anything
 * (Ctrl+click toggles); focus ends on the last row.
 */
export async function selectFiles(page, paths) {
  for (const path of paths) await expandTo(page, path);
  for (const path of await selectedPaths(page)) {
    if (!paths.includes(path)) await fileRow(page, path).locator(".ui-tree-item__title").click({ modifiers: ["Control"] });
  }
  for (const path of paths) {
    const now = await selectedPaths(page);
    if (!now.includes(path)) await fileRow(page, path).locator(".ui-tree-item__title").click({ modifiers: ["Control"] });
  }
  await waitFor(async () => {
    const now = await selectedPaths(page);
    return now.length === paths.length && paths.every((p) => now.includes(p));
  }, { message: `selection ${paths.join(", ")}` });
  await fileRow(page, paths.at(-1)).focus();
}

/** Wait for a notification whose title matches; returns { title, body, tone, actions }. */
export async function waitForNotification(page, title, { timeout = 15_000 } = {}) {
  const matcher = typeof title === "string" ? (t) => t === title : (t) => title.test(t);
  let last = [];
  return waitFor(
    async () => {
      last = await notifications(page);
      return last.find((n) => matcher(n.title)) ?? null;
    },
    { timeout, message: `notification ${title} (seen: ${JSON.stringify(last.map((n) => n.title))})` },
  );
}

export async function notifications(page) {
  return page.locator('[data-testid="notification"]').evaluateAll((items) =>
    items.map((n) => ({
      id: n.getAttribute("data-notification-id"),
      tone: n.getAttribute("data-tone"),
      title: n.querySelector(".ui-notification__title")?.textContent ?? "",
      body: n.querySelector(".ui-notification__body")?.textContent ?? "",
      actions: [...n.querySelectorAll(".ui-notification__actions button")].map((b) => b.textContent),
    })),
  );
}

/** Press a notification's action button (e.g. "Undo"). */
export async function pressNotificationAction(page, title, action) {
  const item = page.locator('[data-testid="notification"]').filter({ has: page.locator(".ui-notification__title", { hasText: title }) }).first();
  await item.getByRole("button", { name: action }).click();
}

export async function dismissNotifications(page) {
  for (const button of await page.getByRole("button", { name: /^Dismiss notification/ }).all()) await button.click().catch(() => {});
}

/** Assert that no alert dialog appears within `ms` (a routine operation never asks). */
export async function expectNoDialog(page, assert, ms = 600, message = "no confirmation dialog") {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    assert.equal(await page.getByRole("alertdialog").count(), 0, message);
    await new Promise((r) => setTimeout(r, 50));
  }
}

/** The combined decision dialog. */
export function decision(page) {
  return page.locator('[data-testid="op-decision"]');
}

export function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

/** Copy screenshots to $EXPOSITION_DEV_HOME/ui-reference/p1/ when set. */
export async function publishShots(shots) {
  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (!devHome) return;
  const target = join(devHome, "ui-reference", "p1");
  await mkdir(target, { recursive: true });
  for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
}

export async function setTheme(page, theme) {
  await page.evaluate((t) => {
    localStorage.setItem("exposition.theme", t);
    document.documentElement.dataset.theme = t;
  }, theme);
}

/** Wait until overlays finished their enter/exit transitions (for reference screenshots). */
export async function settle(page) {
  await page
    .waitForFunction(() => !document.querySelector("[data-entering], [data-exiting]"), null, { timeout: 3000 })
    .catch(() => {});
  await page.waitForTimeout(200);
}
