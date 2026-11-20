// Screenshots of the V2 UI samples (phase 1 review).
//
//   node prototypes/v2-samples/shoot.mjs [--only <term>[,<term>…]] [--url http://127.0.0.1:5175]
//
// Starts the sample dev server when nothing answers on the port (and stops it
// afterwards), or uses the running one. Drives Microsoft Edge (playwright-core,
// channel "msedge") at 1440×900 with the review bar hidden (chrome=0) and
// writes prototypes/v2-samples/shots/<name>.png. Interaction states are reached
// by real clicks (context menu, tab, Advanced, field focus, Needs you jump).
// `--only` keeps shots whose name contains every comma-separated term.
import { spawn } from "node:child_process";
import { mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright-core";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");
const outDir = join(here, "shots");

const args = process.argv.slice(2);
const flag = (name) => {
  const values = [];
  for (let i = 0; i < args.length; i += 1) if (args[i] === name && args[i + 1]) values.push(args[i + 1]);
  return values;
};
const base = flag("--url")[0] ?? "http://127.0.0.1:5175";
const only = flag("--only").flatMap((v) => v.split(",")).filter(Boolean);

const DIRECTIONS = ["balanced"];
const PALETTES = ["dark", "light", "ocean", "sage"];

/** @type {{ name: string; q: string; full?: boolean; act?: (page: import("playwright-core").Page) => Promise<void> }[]} */
const SHOTS = [];
for (const d of DIRECTIONS) {
  SHOTS.push(
    { name: `${d}-editor`, q: `s=${d}&view=editor` },
    {
      name: `${d}-editor-menu`,
      q: `s=${d}&view=editor`,
      act: async (page) => {
        await page.click('[data-path="analysis/arrhenius_fit.py"]', { button: "right" });
        await page.waitForSelector('[data-testid="v2-files-menu"]');
      },
    },
    {
      name: `${d}-editor-tab-python`,
      q: `s=${d}&view=editor`,
      act: async (page) => {
        await page.click('[role="tab"][aria-label="arrhenius_fit.py"]');
      },
    },
    { name: `${d}-settings`, q: `s=${d}&view=settings` },
    {
      name: `${d}-settings-advanced-focus`,
      q: `s=${d}&view=settings`,
      act: async (page) => {
        await page.click('[data-testid="v2-advanced"]');
        await page.click("#v2-openai-key");
      },
    },
    { name: `${d}-settings-appearance`, q: `s=${d}&view=settings&page=appearance` },
    { name: `${d}-agent`, q: `s=${d}&view=agent` },
    {
      name: `${d}-agent-question`,
      q: `s=${d}&view=agent`,
      act: async (page) => {
        await page.click('.v2-needs__item:has-text("Needs your answer")');
        await page.click('.v2-option:has-text("Kelvin")');
      },
    },
    { name: `${d}-agent-w360`, q: `s=${d}&view=agent&w=360` },
  );
}
for (const p of PALETTES) SHOTS.push({ name: `palette-${p}`, q: `s=palette&palette=${p}`, full: true });
for (const p of PALETTES) SHOTS.push({ name: `icons-${p}`, q: `s=icons&palette=${p}`, full: true });

async function up() {
  try {
    const res = await fetch(base, { signal: AbortSignal.timeout(1500) });
    return res.ok;
  } catch {
    return false;
  }
}

async function ensureServer() {
  if (await up()) return null;
  console.log("Starting the sample dev server…");
  const child = spawn(process.execPath, [join(repo, "node_modules/vite/bin/vite.js"), "--config", "prototypes/v2-samples/vite.config.ts"], {
    cwd: repo,
    stdio: "ignore",
  });
  for (let i = 0; i < 60; i += 1) {
    await new Promise((r) => setTimeout(r, 500));
    if (await up()) return child;
  }
  child.kill();
  throw new Error(`The dev server did not answer on ${base}`);
}

const shots = only.length ? SHOTS.filter((s) => only.every((t) => s.name.includes(t))) : SHOTS;
if (!shots.length) {
  console.error("No screenshots match.");
  process.exit(2);
}

mkdirSync(outDir, { recursive: true });
const server = await ensureServer();
const browser = await chromium.launch({ channel: "msedge" });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 });
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));
page.on("console", (m) => {
  if (m.type() === "error") errors.push(m.text());
});
try {
  for (const s of shots) {
    await page.goto(`${base}/?${s.q}&chrome=0`, { waitUntil: "networkidle" });
    await page.waitForSelector(".v2s-frame *", { timeout: 15000 });
    // Icons are <img> elements: wait until every one has decoded.
    await page.waitForFunction(() => [...document.images].every((img) => img.complete && img.naturalWidth > 0), null, { timeout: 10000 });
    if (s.act) await s.act(page);
    await page.mouse.move(1439, 899);
    await page.waitForTimeout(150);
    const file = join(outDir, `${s.name}.png`);
    await page.screenshot({ path: file, fullPage: Boolean(s.full) });
    console.log(`shot ${s.name}`);
  }
} finally {
  await browser.close();
  if (server) server.kill();
}
if (errors.length) {
  console.error(`Page errors (${errors.length}):`);
  for (const e of [...new Set(errors)].slice(0, 20)) console.error(`  ${e}`);
  process.exitCode = 1;
}
