#!/usr/bin/env node
// Runs end-to-end scenarios against the built desktop app.
// Usage: `npm run e2e` (all scenarios) or `npm run e2e -- smoke documents`.
// Each scenario module in ./scenarios exports `default async function (ctx)`
// where ctx = { launchApp, artifactsDir, fixture(name), assert }.
import assert from "node:assert/strict";
import { mkdir, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

import { launchApp, REPO_ROOT } from "./app.mjs";

const scenariosDir = join(import.meta.dirname, "scenarios");
const requested = process.argv.slice(2);
const available = (await readdir(scenariosDir)).filter((f) => f.endsWith(".mjs")).map((f) => f.replace(/\.mjs$/, ""));
// `live-*` scenarios call paid external services (e.g. a model provider) and
// run only when named explicitly.
const selected = requested.length ? requested : available.filter((name) => !name.startsWith("live-"));
const unknown = selected.filter((s) => !available.includes(s));
if (unknown.length) {
  console.error(`unknown scenarios: ${unknown.join(", ")}; available: ${available.join(", ")}`);
  process.exit(2);
}

const artifactsRoot = join(REPO_ROOT, "test-results", "e2e");
const results = [];
for (const name of selected) {
  const artifactsDir = join(artifactsRoot, name);
  await mkdir(artifactsDir, { recursive: true });
  const started = Date.now();
  const module = await import(pathToFileURL(join(scenariosDir, `${name}.mjs`)).href);
  // Scenarios close their app in `finally`, before a failure reaches this
  // runner, so each app records its last state (screenshot + console tail)
  // just before it closes. On failure these files are the evidence.
  let launched = 0;
  const launchWithLastState = async (options) => {
    const app = await launchApp(options);
    const n = ++launched;
    const close = app.close.bind(app);
    app.close = async () => {
      try {
        await app.page.screenshot({ path: join(artifactsDir, `last-state-${n}.png`), timeout: 5_000 });
      } catch {}
      try {
        await writeFile(join(artifactsDir, `last-console-${n}.txt`), app.consoleMessages.slice(-200).join("\n"));
      } catch {}
      return close();
    };
    return app;
  };
  const ctx = {
    launchApp: launchWithLastState,
    artifactsDir,
    assert,
    fixture: (fixtureName) => join(REPO_ROOT, "tests", "fixtures", fixtureName),
  };
  try {
    await module.default(ctx);
    results.push({ name, ok: true, seconds: (Date.now() - started) / 1000 });
  } catch (error) {
    results.push({ name, ok: false, seconds: (Date.now() - started) / 1000, error });
  }
}

for (const r of results) {
  console.log(`${r.ok ? "PASS" : "FAIL"}  ${r.name} (${r.seconds.toFixed(1)}s)`);
  if (!r.ok) console.log(String(r.error?.stack ?? r.error).replace(/^/gm, "      "));
}
console.log(`artifacts: ${artifactsRoot}`);
process.exit(results.every((r) => r.ok) ? 0 : 1);
