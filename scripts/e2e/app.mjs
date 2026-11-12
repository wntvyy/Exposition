// Launches the real Exposition desktop app (Tauri + WebView2) and connects to
// its webview over the Chrome DevTools Protocol. Each launch gets an isolated
// application data directory and WebView2 profile, so tests never touch the
// user's real state.
//
// Build first: `npm run build:app` (debug app with embedded frontend).
import { spawn, execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, mkdir } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium } from "playwright-core";

export const REPO_ROOT = resolve(import.meta.dirname, "..", "..");

export function defaultExecutable() {
  const name = process.platform === "win32" ? "exposition.exe" : "exposition";
  return join(REPO_ROOT, "target", "debug", name);
}

async function freePort() {
  return new Promise((resolvePort, reject) => {
    const server = createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolvePort(port));
    });
  });
}

async function waitForCdp(port, child, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`app exited early with code ${child.exitCode}`);
    try {
      const res = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (res.ok) return;
    } catch {
      // not listening yet
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`webview debugging endpoint did not open on port ${port}`);
}

function killTree(pid) {
  if (!pid) return;
  try {
    if (process.platform === "win32") {
      execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
    } else {
      process.kill(-pid, "SIGKILL");
    }
  } catch {
    // already gone
  }
}

/**
 * Simulate a crash: terminate the app process immediately (no close handlers,
 * no saves), then best-effort clean up any surviving descendants. `taskkill /T`
 * alone is unreliable for this because it reports failure whenever a process in
 * the tree exits on its own mid-walk (WebView2 children often do), even though
 * the app itself was killed. Callers should still wait for the child's exit.
 */
export function hardKill(pid) {
  if (!pid) return;
  try {
    process.kill(pid, "SIGKILL"); // TerminateProcess on Windows
  } catch {
    // already gone
  }
  killTree(pid);
}

const APP_ORIGINS = ["http://tauri.localhost", "https://tauri.localhost", "tauri://localhost"];

/** Wait until the webview has navigated to the bundled app and React has rendered. */
async function appPage(browser, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    for (const context of browser.contexts()) {
      for (const page of context.pages()) {
        if (APP_ORIGINS.some((origin) => page.url().startsWith(origin))) {
          await page.waitForLoadState("load");
          await page.waitForFunction(() => (document.getElementById("root")?.childElementCount ?? 0) > 0, null, {
            timeout: Math.max(1000, deadline - Date.now()),
          });
          return page;
        }
      }
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("app page did not load");
}

/**
 * @param {{ executable?: string, dataDir?: string, env?: Record<string,string>, timeoutMs?: number }} [options]
 */
export async function launchApp(options = {}) {
  const executable = options.executable ?? defaultExecutable();
  if (!existsSync(executable)) {
    throw new Error(`app executable not found at ${executable}; run \`npm run build:app\` first`);
  }
  const dataDir = options.dataDir ?? (await mkdtemp(join(tmpdir(), "exposition-e2e-")));
  const webviewDir = join(dataDir, "webview");
  await mkdir(webviewDir, { recursive: true });
  // A relaunch on the same data directory can race the previous instance's
  // WebView2 browser processes, which hold the profile for a moment after the
  // app is killed (HRESULT 0x800700AA "requested resource is in use"). Retry
  // only that specific startup failure; any other early exit fails at once.
  for (let attempt = 1; ; attempt++) {
    try {
      return await launchOnce(executable, dataDir, webviewDir, options);
    } catch (error) {
      const profileBusy = /0x800700AA/i.test(String(error?.message));
      if (!profileBusy || attempt >= 8) throw error;
      await new Promise((r) => setTimeout(r, 750 * attempt));
    }
  }
}

async function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolveExit) => {
    const timer = setTimeout(resolveExit, timeoutMs);
    child.once("exit", () => {
      clearTimeout(timer);
      resolveExit();
    });
  });
}

async function launchOnce(executable, dataDir, webviewDir, options) {
  const port = await freePort();

  const logs = [];
  const child = spawn(executable, [], {
    env: {
      ...process.env,
      EXPOSITION_DATA_DIR: dataDir,
      WEBVIEW2_USER_DATA_FOLDER: webviewDir,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
      ...options.env,
    },
    stdio: ["ignore", "pipe", "pipe"],
    detached: process.platform !== "win32",
  });
  child.stdout.on("data", (d) => logs.push(String(d)));
  child.stderr.on("data", (d) => logs.push(String(d)));

  let browser;
  try {
    await waitForCdp(port, child, options.timeoutMs ?? 45_000);
    browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
    const page = await appPage(browser, options.timeoutMs ?? 45_000);
    const consoleMessages = [];
    page.on("console", (m) => consoleMessages.push(`${m.type()}: ${m.text()}`));
    page.on("pageerror", (e) => consoleMessages.push(`pageerror: ${e.message}`));
    return {
      page,
      browser,
      child,
      dataDir,
      logs,
      consoleMessages,
      async close() {
        await browser.close().catch(() => {});
        killTree(child.pid);
        await waitForExit(child, 10_000);
      },
    };
  } catch (error) {
    await browser?.close().catch(() => {});
    killTree(child.pid);
    error.message += `\napp output:\n${logs.join("").slice(-4000)}`;
    throw error;
  }
}
