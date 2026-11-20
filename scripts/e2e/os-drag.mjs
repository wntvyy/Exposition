// Real operating-system drag and drop onto the app window (Windows): a
// wrapper around os-drag.ps1 (OLE DoDragDrop with a CF_HDROP file list from a
// helper window, a held pointer, and a release over the app).
import { spawn } from "node:child_process";
import { join } from "node:path";

import { REPO_ROOT } from "./app.mjs";

const HELPER = join(REPO_ROOT, "scripts", "e2e", "os-drag.ps1");

/**
 * Run the drag helper against the app (`pid`) at the client point `x`,`y`
 * (physical pixels), holding there for `holdMs` first so the page can be
 * observed during the drag (the call is asynchronous for that reason).
 * `mode`: "files" drops `paths`, "text" drops `text`, "mouse" drags with the
 * pointer from `x`,`y` to `x2`,`y2`. Resolves with the helper's report.
 */
export function osDrag({ pid, x, y, mode = "files", paths, text, x2, y2, holdMs = 1500 }) {
  const q = (s) => `'${String(s).replace(/'/g, "''")}'`;
  const parts = [`& ${q(HELPER)}`, `-AppPid ${pid}`, `-X ${Math.round(x)}`, `-Y ${Math.round(y)}`, `-Mode ${mode}`];
  if (paths?.length) parts.push(`-Paths @(${paths.map(q).join(",")})`);
  if (text) parts.push(`-Text ${q(text)}`);
  if (x2 !== undefined) parts.push(`-X2 ${Math.round(x2)} -Y2 ${Math.round(y2)}`);
  parts.push(`-HoldMs ${holdMs}`);
  const command = parts.join(" ");
  return new Promise((resolve, reject) => {
    const child = spawn("powershell", ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", command], { stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    child.on("exit", (code) => {
      const line = out.trim().split(/\r?\n/).pop() ?? "";
      let result;
      try {
        result = JSON.parse(line);
      } catch {
        reject(new Error(`os-drag failed (${code}): ${out} ${err}`));
        return;
      }
      // The helper never drags onto another application's window.
      if (result.ok) resolve(result);
      else reject(new Error(`os-drag refused: ${line}`));
    });
  });
}

/** Drag `paths` onto the app and drop them at the client point (see `osDrag`). */
export function osDragFiles({ pid, x, y, paths, holdMs = 1500 }) {
  return osDrag({ pid, x, y, mode: "files", paths, holdMs });
}

/** The center of a locator as a client point in physical pixels (the window's client area is the webview). */
export async function clientPoint(page, locator) {
  await locator.scrollIntoViewIfNeeded();
  await page.waitForTimeout(150);
  const box = await locator.boundingBox();
  const dpr = await page.evaluate(() => window.devicePixelRatio);
  return { x: (box.x + box.width / 2) * dpr, y: (box.y + box.height / 2) * dpr };
}
