// M6 acceptance: the shared paper project, end to end in the real app.
//
// One project (tests/fixtures/m6-paper): a Typst paper (main.typ: prose, a
// display equation, a figure placeholder), an analysis script
// (analysis/fit.py: a matplotlib figure and a pandas table), a Lean project
// (lakefile.toml, lean-toolchain v4.35.0, Paper/Gauss.lean with the lemma the
// paper cites), and a LaTeX supplement. Every app launch gets fake
// ANTHROPIC_API_KEY / GITHUB_TOKEN values in its environment.
//
//   1a open untrusted: nothing runs; Research, Lean, and TeX show the untrusted
//      state
//   1b trust through the dialog; every surface must leave the untrusted state
//   2  agent draft (scripted provider, debug builds only): edit_document,
//      build_document, run_python in the agent's own session, insert_artifact,
//      check_lean; review diff; revert one file, keep the other
//   3  Monaco source edit + page edits (prose, equation token) in Split view;
//      Ctrl+Z from the page and from Monaco through the shared history
//   4  research: Run File, insert figure + table from the panel, change the
//      script, rerun, Update in document; the page shows the new figure
//   5  Lean: Checked with the standard axioms; sorry -> Incomplete; fixed ->
//      Checked
//   6  Save All, close, relaunch on the same data dir, reopen from Recent:
//      bytes on disk; PDF export matches the shown pages
//   7  failure cases: read-only save, stale agent edits racing page typing
//      (3 rounds; the race is timing dependent), canceled research run and TeX
//      build (process trees), inert kernel HTML/SVG, secrets (kernel, Lean,
//      Docker, logs, events, files), restricted execution (Docker, no
//      network; refused without an image)
//   8  final state, light and dark
//
// Each step is recorded PASS / FAIL / NOT VERIFIED (with the reason) and its
// measured time in test-results/e2e/m6-paper-project/results.json; the
// scenario fails if any step fails. Release/installed builds have no scripted
// provider and no dev commands: those parts are NOT VERIFIED, never faked.
// Provider keys are never set or cleared here (release builds use the user's
// real credential entry); no live provider request is made.
//
// Needs: Python with ipykernel/jupyter_client/matplotlib/pandas/numpy, Lean
// 4.35.0 through elan, TeX Live (pdflatex), Docker with the local image
// python:3.13-slim. Screenshots: test-results/e2e/m6-paper-project/ and, when
// EXPOSITION_DEV_HOME is set, $EXPOSITION_DEV_HOME/ui-reference/m6/.
import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import { chmod, copyFile, cp, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { waitForAsync } from "../wait.mjs";
import {
  documentState,
  editorLocator,
  invoke,
  invokeError,
  openFromTree,
  openInEditor,
  tabDirty,
  waitFor,
  waitForMirror,
} from "../workspace-helpers.mjs";
import { runCommand } from "../palette.mjs";

const PAPER = "main.typ";
const FIT = "analysis/fit.py";
const LEMMA = "Paper/Gauss.lean";
const SUPPLEMENT = "supplement.tex";
const IMAGE = "python:3.13-slim";
const LEAN_TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const KERNEL_TIMEOUT = 120_000;
const LEAN_TIMEOUT = 300_000;
const VIEWPORT = { width: 1440, height: 900 };
const STANDARD_AXIOMS = ["propext", "Classical.choice", "Quot.sound"];
/** Prefix of the fake secrets; never sent to the webview or written into code. */
const LEAK_MARKER = "m6leak";
const TOOL_PROCESS = /^(lean|lake|pdflatex|lualatex|xelatex|latexmk|perl|docker|rterm|r|julia)\.exe$/i;

// ---------------------------------------------------------------- processes

/** Snapshot of the OS process table: pid -> { parent, name, commandLine }. */
function processTable() {
  const out = execFileSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json -Compress",
    ],
    { encoding: "utf8", maxBuffer: 128 * 1024 * 1024 },
  );
  return new Map(
    JSON.parse(out).map((p) => [p.ProcessId, { parent: p.ParentProcessId, name: p.Name, commandLine: p.CommandLine ?? "" }]),
  );
}

/** All descendants of `rootPid` (one table snapshot). */
function descendants(rootPid, table = processTable()) {
  const out = [];
  const queue = [rootPid];
  while (queue.length) {
    const parent = queue.shift();
    for (const [pid, info] of table) {
      if (info.parent === parent && !out.some((p) => p.pid === pid)) {
        out.push({ pid, ...info });
        queue.push(pid);
      }
    }
  }
  return out;
}

/** Processes whose command line contains `marker`. */
function processesMentioning(marker, table = processTable()) {
  const needle = marker.toLowerCase();
  return [...table].filter(([, p]) => p.commandLine.toLowerCase().includes(needle)).map(([pid, p]) => ({ pid, ...p }));
}

/** Processes of `list` still alive (same pid and image name). */
function survivors(list, table = processTable()) {
  return list.filter((p) => table.has(p.pid) && table.get(p.pid).name === p.name);
}

/** Close like a user (WM_CLOSE); resolves true if the app exited on its own. */
async function closeGracefully(app, timeoutMs = 30_000) {
  const exited = new Promise((resolve) => {
    if (app.child.exitCode !== null) resolve(true);
    else app.child.once("exit", () => resolve(true));
  });
  try {
    execFileSync("taskkill", ["/PID", String(app.child.pid)], { stdio: "ignore" });
  } catch {
    // already exiting
  }
  const ok = await Promise.race([exited, new Promise((resolve) => setTimeout(() => resolve(false), timeoutMs))]);
  await app.close();
  return ok;
}

// ---------------------------------------------------------------- files

async function listFiles(dir, base = dir, out = []) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) await listFiles(full, base, out);
    else if (entry.isFile()) out.push(full.slice(base.length + 1).replace(/\\/g, "/"));
  }
  return out;
}

async function snapshotFiles(root, filter = () => true) {
  const map = new Map();
  for (const rel of (await listFiles(root)).filter(filter)) map.set(rel, await readFile(join(root, rel)));
  return map;
}

/** Files under `dir` (up to 64 MB each) containing any of `needles`. */
async function filesContaining(dir, needles) {
  const hits = [];
  if (!existsSync(dir)) return hits;
  for (const rel of await listFiles(dir)) {
    const full = join(dir, rel);
    let info;
    try {
      info = await stat(full);
    } catch {
      continue;
    }
    if (info.size > 64 * 1024 * 1024) continue;
    let bytes;
    try {
      bytes = await readFile(full);
    } catch {
      continue; // locked (e.g. a live WebView2 database)
    }
    for (const needle of needles) {
      if (bytes.includes(Buffer.from(needle, "utf8")) || bytes.includes(Buffer.from(needle, "utf16le"))) hits.push(rel);
    }
  }
  return hits;
}

/** Remove `// exposition:begin K` … `// exposition:end K` blocks and normalize blank lines. */
function withoutAttachments(text) {
  const lines = text.split("\n");
  const kept = [];
  let inside = null;
  for (const line of lines) {
    const begin = line.match(/^\/\/ exposition:begin (\S+)$/);
    if (!inside && begin) {
      inside = begin[1];
      continue;
    }
    if (inside) {
      if (line === `// exposition:end ${inside}`) inside = null;
      continue;
    }
    kept.push(line);
  }
  return kept.join("\n").replace(/\n{3,}/g, "\n\n");
}

/** A caption as the attach service writes it into Typst markup. */
const typstCaption = (text) =>
  [...text].map((ch, i) => ((i === 0 && /[+=]/.test(ch)) || /[\\#[\]$*_`<>@~/\-."']/.test(ch) ? `\\${ch}` : ch)).join("");

/** Non-blank lines outside attachment blocks (block placement may absorb a blank line). */
const contentLines = (text) => withoutAttachments(text).split("\n").filter((line) => line.trim() !== "");

const attachmentKeys = (text) => [...text.matchAll(/^\/\/ exposition:begin (\S+)$/gm)].map((m) => m[1]);

// ---------------------------------------------------------------- UI helpers

async function setTheme(page, theme) {
  await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
  await page.waitForTimeout(250);
}

async function selectOption(page, scope, label, option) {
  await scope.getByRole("button", { name: label }).first().click();
  await page.getByRole("option", { name: option, exact: true }).click();
}

async function dismissNotifications(page) {
  if ((await page.locator('[role="dialog"], [role="alertdialog"]').count()) > 0) return;
  const dismiss = page.getByRole("button", { name: /^Dismiss notification/ });
  for (let i = 0; i < 12 && (await dismiss.count()) > 0; i++) await dismiss.first().click().catch(() => {});
}

/**
 * Record callback payloads (events and command responses) delivered to the
 * webview that contain the leak marker. Only the marker prefix enters the page.
 */
async function installLeakProbe(page) {
  return page.evaluate((marker) => {
    const callbacks = window.__TAURI_INTERNALS__?.callbacks;
    if (!(callbacks instanceof Map)) return false;
    const state = { seen: 0, leaks: [] };
    window.__m6Probe = state;
    const wrap = (fn) => (data) => {
      state.seen += 1;
      try {
        const text = JSON.stringify(data);
        if (text && text.includes(marker)) state.leaks.push(text.slice(0, 300));
      } catch {
        // unserializable payloads cannot carry the text
      }
      return fn(data);
    };
    for (const [id, fn] of [...callbacks]) Map.prototype.set.call(callbacks, id, wrap(fn));
    callbacks.set = function set(id, fn) {
      return Map.prototype.set.call(this, id, wrap(fn));
    };
    return true;
  }, LEAK_MARKER);
}

const leakProbe = (page) => page.evaluate(() => window.__m6Probe ?? null);

// ---------------------------------------------------------------- scenario

export default async function m6PaperProject({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  assert.ok(existsSync(join(elanHome, "toolchains", LEAN_TOOLCHAIN_DIR, "bin")), `Lean ${LEAN_TOOLCHAIN_DIR} is required (elan)`);

  const base = await mkdtemp(join(tmpdir(), "xp-m6-"));
  const root = join(base, "paper");
  const dataDir = join(base, "data");
  await cp(fixture("m6-paper"), root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  const fixtureFiles = await snapshotFiles(root);
  const original = (rel) => fixtureFiles.get(rel).toString("utf8");

  const tag = randomBytes(6).toString("hex");
  const SECRETS = {
    ANTHROPIC_API_KEY: `${LEAK_MARKER}-anthropic-${tag}-value`,
    GITHUB_TOKEN: `${LEAK_MARKER}-github-${tag}-value`,
  };
  // A unique credential namespace (debug builds honor it); keys are never set or cleared here.
  const env = { ...SECRETS, EXPOSITION_CREDENTIAL_NAMESPACE: `m6-accept-${tag}` };

  const results = [];
  const shots = [];
  const allLogs = [];
  const allConsole = [];
  const probes = [];
  let current = null;
  let isDebug = false;
  let app = null;
  let page = null;

  const note = (key, value) => {
    current.details[key] = value;
  };
  const timing = (label, ms) => {
    current.timings.push({ label, ms });
  };
  /** A recorded (non-fatal) check: the step fails at its end, later checks still run. */
  const softly = (ok, message) => {
    if (!ok) {
      current.failures.push(message);
      console.log(`      CHECK FAILED ${current.id}: ${message}`);
    }
    return ok;
  };
  const notVerified = (part, reason) => {
    current.notVerified.push({ part, reason });
    console.log(`      NOT VERIFIED ${current.id} ${part}: ${reason}`);
  };
  const shot = async (name) => {
    await dismissNotifications(page).catch(() => {});
    await page.waitForTimeout(250);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };

  /** Run one acceptance step; failures are recorded and the next step still runs. */
  async function step(id, title, fn) {
    current = { id, title, status: "PASS", ms: 0, timings: [], details: {}, notVerified: [], failures: [] };
    const started = Date.now();
    try {
      await fn();
      if (current.failures.length) {
        current.status = "FAIL";
        current.error = current.failures.join("\n");
      } else if (current.notVerified.length && current.details.verifiedParts === 0) {
        current.status = "NOT VERIFIED";
      }
    } catch (error) {
      current.status = "FAIL";
      current.error = [...current.failures, String(error?.stack ?? error)].join("\n");
      if (page) await page.screenshot({ path: join(artifactsDir, `failure-${id}.png`) }).catch(() => {});
    }
    current.ms = Date.now() - started;
    results.push(current);
    const extra = current.notVerified.length ? ` (not verified: ${current.notVerified.map((n) => n.part).join(", ")})` : "";
    console.log(`      ${current.status.padEnd(12)} ${id} ${title} — ${(current.ms / 1000).toFixed(1)} s${extra}`);
    if (current.error) console.log(current.error.replace(/^/gm, "        "));
  }

  async function launch() {
    const launched = await launchApp({ dataDir, env });
    app = launched;
    page = launched.page;
    await page.setViewportSize(VIEWPORT);
    const installed = await installLeakProbe(page);
    assert.ok(installed, "the event/response probe could be installed");
    return launched;
  }

  async function retire(closingApp) {
    probes.push(await leakProbe(closingApp.page).catch(() => null));
    allLogs.push(closingApp.logs.join(""));
    allConsole.push(...closingApp.consoleMessages);
  }

  // ---- shared page-editor helpers (operate on the visible page editor) -----
  const pageEditor = () => page.locator('[data-testid="page-editor"]:visible').first();
  const modeButton = (label) => page.locator(".ui-segmented__item:visible", { hasText: label }).first();
  const docIdOf = async (path) => (await invoke(page, "document_list")).find((d) => d.path === path)?.id;
  const textOf = async (path) => (await documentState(page, await docIdOf(path))).text;

  async function settled(path, label, timeout = 30_000) {
    const started = Date.now();
    await waitForAsync(
      page,
      async (p) => {
        const el = [...document.querySelectorAll('[data-testid="page-editor"]')].find((e) => e.offsetParent !== null);
        if (!el) return false;
        const imgs = [...el.querySelectorAll('[data-testid="page"] img')];
        if (!(el.dataset.stale === "false" && el.dataset.build && imgs.length > 0 && imgs.every((i) => i.complete))) return false;
        const doc = (await window.__TAURI_INTERNALS__.invoke("document_list")).find((d) => d.path === p);
        return !!doc && el.dataset.buildRevision === String(doc.revision);
      },
      path,
      { timeout, message: `${path} pages built for the current revision (${label})` },
    );
    timing(label, Date.now() - started);
    return pageEditor().getAttribute("data-build");
  }

  async function point(path, offset, affinity = "downstream", dx = 0) {
    const build = await pageEditor().getAttribute("data-build");
    const g = await invoke(page, "page_caret", { build, path, offset, affinity });
    assert.ok(g, `no page geometry for offset ${offset}`);
    const el = pageEditor().locator(`[data-testid="page"][data-page="${g.page}"]`);
    await el.evaluate(
      (pageEl, [top, x]) => {
        const scroller = pageEl.closest('[data-testid="page-scroller"]');
        const scale = pageEl.getBoundingClientRect().width / Number(pageEl.dataset.widthPt);
        const view = scroller.getBoundingClientRect();
        scroller.scrollTop += pageEl.getBoundingClientRect().top + top * scale - (view.top + view.height / 2);
        scroller.scrollLeft += pageEl.getBoundingClientRect().left + x * scale - (view.left + view.width / 2);
      },
      [g.top, g.x + dx],
    );
    const box = await el.boundingBox();
    const scale = box.width / Number(await el.getAttribute("data-width-pt"));
    return { x: box.x + (g.x + dx) * scale, y: box.y + ((g.top + g.bottom) / 2) * scale };
  }

  async function clickPageAt(path, offset, affinity, dx) {
    const p = await point(path, offset, affinity, dx);
    await page.mouse.click(p.x, p.y);
    await page.waitForFunction(
      (o) =>
        [...document.querySelectorAll('[data-testid="page-caret"]')].some((c) => c.getAttribute("data-offset") === String(o)),
      offset,
      { timeout: 10_000 },
    );
  }

  async function rendered(path, start, end) {
    const build = await pageEditor().getAttribute("data-build");
    return (await invoke(page, "page_selection", { build, path, start, end })).text;
  }

  /** Pixel hashes of the visible page images (what the user sees). */
  const pageImageHashes = () =>
    page.evaluate(async () => {
      const el = [...document.querySelectorAll('[data-testid="page-editor"]')].find((e) => e.offsetParent !== null);
      const out = [];
      for (const img of el.querySelectorAll('[data-testid="page"] img')) {
        const canvas = document.createElement("canvas");
        canvas.width = img.naturalWidth;
        canvas.height = img.naturalHeight;
        const context = canvas.getContext("2d");
        context.drawImage(img, 0, 0);
        const digest = await crypto.subtle.digest("SHA-256", context.getImageData(0, 0, canvas.width, canvas.height).data);
        out.push([...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join(""));
      }
      return out;
    });

  async function showPageMode(path, mode = "Page") {
    await openFromTree(page, path);
    await modeButton(mode).waitFor({ timeout: 10_000 });
    await modeButton(mode).click();
    await pageEditor().waitFor({ timeout: 15_000 });
  }

  // ---- research helpers -----------------------------------------------------
  const output = () => page.getByTestId("research-output");

  async function newRunFrom(trigger) {
    const previous = (await output().count()) ? await output().getAttribute("data-run-id") : null;
    await trigger();
    return waitFor(
      async () => {
        const id = (await output().count()) ? await output().getAttribute("data-run-id") : null;
        return id && id !== previous ? id : null;
      },
      { timeout: 30_000, message: "a new research run" },
    );
  }

  async function runInPanel(code, status = "succeeded", timeout = KERNEL_TIMEOUT) {
    const runId = await newRunFrom(async () => {
      const area = page.getByTestId("research-code");
      await area.fill(code);
      await area.focus();
      await page.keyboard.press("Control+Enter");
    });
    if (status) await page.locator(`[data-testid="research-output"][data-run-id="${runId}"][data-status="${status}"]`).waitFor({ timeout });
    return runId;
  }

  async function runOutputText(runId) {
    const out = await invoke(page, "research_run_output", { runId, after: null, limit: 200 });
    return out.entries.filter((e) => e.item.kind === "stream").map((e) => e.item.text).join("");
  }

  async function startResearchSession() {
    await runCommand(page, "Show Research", /Show Research/);
    await page.getByTestId("research-panel").waitFor({ timeout: 15_000 });
    const started = Date.now();
    await page.getByRole("button", { name: "Start session" }).click();
    await page.locator('[data-testid="research-session-status"][data-state="idle"]').waitFor({ timeout: KERNEL_TIMEOUT });
    timing("research session start", Date.now() - started);
  }

  async function attachFromPanel(container, { caption, mode, key }) {
    await container.getByRole("button", { name: "Insert or update in document…" }).click();
    const group = container.getByTestId("research-attach");
    await group.waitFor();
    const docSelect = group.getByRole("button", { name: /Document/ }).first();
    await docSelect.waitFor();
    if (!(await docSelect.innerText()).includes(PAPER)) {
      await docSelect.click();
      await page.getByRole("option", { name: PAPER }).click();
    }
    if (caption !== undefined) await group.getByRole("textbox", { name: "Caption" }).fill(caption);
    if (mode === "insert") {
      await group.getByRole("button", { name: "Insert into document" }).click();
    } else {
      const keySelect = group.getByRole("button", { name: /Attachment to update/ });
      await keySelect.waitFor();
      if (!(await keySelect.innerText()).includes(key)) {
        await keySelect.click();
        await page.getByRole("option", { name: key, exact: true }).click();
      }
      await group.getByRole("button", { name: "Update in document" }).click();
    }
  }

  // ---- agent helpers ----------------------------------------------------------
  const agentPanel = () => page.getByTestId("agents-panel");
  const waitForTaskStatus = (kind, timeout = 60_000) =>
    page.waitForFunction((k) => document.querySelector("[data-testid=agents-task-status]")?.dataset.status === k, kind, { timeout });

  async function startAgentTask({ prompt, permission, execution }) {
    await runCommand(page, "New Agent Task", /New Agent Task/);
    const panel = agentPanel();
    await panel.getByTestId("agents-composer").waitFor({ timeout: 10_000 });
    await page.fill("[data-testid=agents-prompt]", prompt);
    await panel.getByTestId("agents-folder").waitFor();
    await selectOption(page, panel, /Document changes/, permission);
    await selectOption(page, panel, /Commands and code/, execution);
    await panel.getByRole("button", { name: "Start task" }).click();
    await page.waitForSelector("[data-testid=agents-task-view]");
  }

  async function latestTaskCalls() {
    const task = (await invoke(page, "agents_task_list"))[0];
    const detail = await invoke(page, "agents_task_detail", { task: task.id });
    return { task, detail, calls: detail.transcript.filter((i) => i.kind === "toolCall") };
  }

  // ===========================================================================
  await launch();
  try {
    const providerStatus = await invoke(page, "agents_provider_status");
    isDebug = providerStatus.scriptedAvailable === true;
    console.log(`      build: ${isDebug ? "debug (scripted provider available)" : "release (no scripted provider)"}; provider configured: ${providerStatus.configured}`);
    await setTheme(page, "light");

    let paperId;
    // ===================================================================== 1
    await step("1a", "Open the project untrusted: nothing executes; Research, Lean, TeX show untrusted", async () => {
      const opened = await invoke(page, "workspace_open", { path: root }); // the folder dialog itself cannot be automated
      note("root", opened.root);
      await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });

      // The paper's pages build in-process without trust (Typst runs no external tools).
      await showPageMode(PAPER);
      await settled(PAPER, "untrusted Typst build");
      paperId = await docIdOf(PAPER);

      // Research: banner, and kernels are refused.
      await runCommand(page, "Show Research", /Show Research/);
      const research = page.getByTestId("research-panel");
      await research.waitFor({ timeout: 15_000 });
      await research.getByText("Workspace not trusted").waitFor({ timeout: 15_000 });
      const refused = await invokeError(page, "research_session_start", { request: { environmentId: "python:any" } });
      assert.equal(refused.error?.code, "research.notTrusted", "kernels are refused in an untrusted workspace");

      // Lean: untrusted, no server.
      await runCommand(page, "show lean infoview", /Show Lean Infoview/);
      await openInEditor(page, LEMMA);
      const lean = page.getByTestId("lean-panel");
      await waitFor(async () => (await lean.getAttribute("data-status")) === "untrusted", { timeout: 30_000, message: "Lean untrusted" });
      assert.match(await page.getByTestId("lean-status").innerText(), /Untrusted/);

      // TeX: opening the supplement's pages asks for trust; declining shows the untrusted state.
      await openFromTree(page, SUPPLEMENT);
      await modeButton("Page").waitFor({ timeout: 10_000 });
      await modeButton("Page").click();
      const dialog = page.getByRole("alertdialog", { name: "Trust this workspace?" });
      await dialog.waitFor({ timeout: 10_000 });
      assert.ok((await dialog.getByTestId("trust-root").innerText()).endsWith("paper"), "the prompt names the project folder");
      await dialog.getByRole("button", { name: /Don.t trust/ }).click();
      await dialog.waitFor({ state: "detached" });
      await page.getByTestId("page-scroller").getByText("Workspace not trusted", { exact: true }).waitFor({ timeout: 10_000 });
      await page.locator('[data-testid="build-status"][data-status="untrusted"]').waitFor({ timeout: 10_000 });
      await shot("01-untrusted-tex-lean-research-light");

      // Nothing ran: no started non-probe job, no tool or kernel process, no project file changed.
      const jobs = await invoke(page, "job_list");
      // Only discovery of system interpreters (outside the project, in the app's
      // probe folder) may run; nothing from or in the project.
      const norm = (p) => String(p).replace(/^\\\\\?\\/, "").replace(/\\/g, "/").toLowerCase();
      const inProject = (p) => norm(p).startsWith(norm(opened.root)) || norm(p).startsWith(norm(root));
      const started = jobs.filter((j) => j.kind !== "probe" && j.info.startedAtMs !== null);
      const offending = started.filter(
        (j) => !(/^research discovery: /.test(j.info.label) && !inProject(j.workingDirectory) && !inProject(j.info.program)),
      );
      note("systemDiscoveryJobs", started.map((j) => `${j.info.label} (cwd ${j.workingDirectory})`));
      assert.deepEqual(offending.map((j) => j.info.label), [], "no build, kernel, Lean, or project job started");
      note("refusedJobs", jobs.filter((j) => j.refusal).map((j) => `${j.info.label}: ${j.refusal.code}`));
      const tools = descendants(app.child.pid).filter((p) => TOOL_PROCESS.test(p.name));
      assert.deepEqual(tools, [], "no Lean/TeX/Docker process runs");
      assert.deepEqual(processesMentioning(join(dataDir, "research")), [], "no research helper or kernel runs");
      assert.deepEqual(processesMentioning(root).filter((p) => p.pid !== app.child.pid), [], "no process runs on the project");
      const now = await snapshotFiles(root);
      for (const [rel, bytes] of fixtureFiles) assert.ok(now.get(rel)?.equals(bytes), `${rel} unchanged`);
      assert.ok(!existsSync(join(root, ".lake")), "Lake never ran");

    });

    await step("1b", "Trust through the Research dialog; the LaTeX page, Lean, and lean_check follow by themselves", async () => {
      // Propagation is asserted directly: no surface's own Trust button is
      // pressed after the grant. Each check is recorded, so one run reports
      // every surface that did not follow.
      const opened = await invoke(page, "workspace_current");
      const research = page.getByTestId("research-panel");
      const lean = page.getByTestId("lean-panel");
      const texUntrusted = () => page.getByTestId("page-scroller").getByText("Workspace not trusted", { exact: true });
      // Trust through the shared dialog (from the Research panel).
      await runCommand(page, "Show Research", /Show Research/);
      await research.getByRole("button", { name: "Trust workspace…" }).click();
      await page.getByTestId("trust-dialog").waitFor();
      await page.waitForTimeout(300);
      await shot("01-trust-dialog-light");
      await page.getByRole("button", { name: "Trust workspace", exact: true }).click();
      await research.getByText("Workspace not trusted").waitFor({ state: "detached", timeout: 10_000 });
      assert.equal((await invoke(page, "trust_status", { root: opened.root })).trusted, true);
      const trustedAt = Date.now();
      const followed = (promise) => promise.then(() => true, () => false);

      // The LaTeX page view declined in 1a (its tab is still open) builds.
      await openFromTree(page, SUPPLEMENT);
      await pageEditor().waitFor({ timeout: 10_000 });
      const texLeft = await followed(texUntrusted().waitFor({ state: "detached", timeout: 15_000 }));
      softly(texLeft, "the LaTeX page view still shows 'Workspace not trusted' 15 s after the folder was trusted from the Research panel");
      if (texLeft) {
        timing("trust -> LaTeX view leaves untrusted", Date.now() - trustedAt);
        await settled(SUPPLEMENT, "supplement built with pdfLaTeX after trust", 120_000);
        timing("trust -> LaTeX pages", Date.now() - trustedAt);
        const pill = await followed(page.locator('[data-testid="build-status"][data-status="succeeded"]').waitFor({ timeout: 15_000 }));
        softly(pill, "the status bar build item does not show the LaTeX build after trust");
        await shot("01-trusted-tex-light");
      }
      await page.locator(`[data-editor-key="file:${SUPPLEMENT}"] .ui-editor-tab__close`).click().catch(() => {});

      // Lean followed the grant by itself: its state (read without asking Lean
      // to do anything) left Untrusted.
      const leanId = await docIdOf(LEMMA);
      assert.ok(leanId, `${LEMMA} is open`);
      const leanStatus = async () => (await invoke(page, "lean_documents")).find((d) => d.document === leanId)?.status.kind;
      const leanFollowed = await followed(
        waitFor(async () => (await leanStatus()) !== "untrusted", { timeout: 15_000, message: "Lean leaves Untrusted" }),
      );
      note("leanStatusAfterTrust", await leanStatus());
      softly(leanFollowed, "Lean still reports Untrusted 15 s after the folder was trusted from the Research panel");
      // lean_check (the path of the agent tool check_lean) on the file opened
      // while untrusted: it checks instead of reporting a stale refusal.
      const report = await invoke(page, "lean_check", {
        request: { target: { kind: "document", document: leanId }, goalsAt: [], timeoutMs: LEAN_TIMEOUT },
      });
      note("leanCheckAfterTrust", { status: report.status?.kind, checked: report.checked, note: report.note, ms: Date.now() - trustedAt });
      softly(
        report.status?.kind !== "untrusted" && !/is not trusted/.test(report.note ?? ""),
        `lean_check (the agent tool check_lean) reports the trusted folder as untrusted: ${JSON.stringify(report.status).slice(0, 300)}`,
      );
      softly(report.checked === true, `lean_check did not check after the grant: ${JSON.stringify(report).slice(0, 400)}`);
      // The Lean infoview (never clicked since 1a).
      await runCommand(page, "show lean infoview", /Show Lean Infoview/);
      await openInEditor(page, LEMMA);
      const panelLeft = await followed(
        waitFor(async () => (await lean.getAttribute("data-status")) !== "untrusted", { timeout: 15_000, message: "infoview" }),
      );
      note("leanPanelAfterTrust", await lean.getAttribute("data-status"));
      softly(panelLeft, "the Lean infoview still shows Untrusted after the folder was trusted from the Research panel");
    });

    // ===================================================================== 2
    let agentKey = null;
    await step("2", "Agent draft: edit, build, analysis, figure, Lean check; review, revert one file", async () => {
      if (!isDebug) {
        const refused = await invokeError(page, "agents_dev_load_script", { script: { turns: [] } });
        note("devScriptRefusal", refused.error ?? "accepted");
        note("providerConfigured", providerStatus.configured);
        note("verifiedParts", 0);
        notVerified(
          "agent draft",
          `release build: the scripted development provider does not exist (scriptedAvailable=false; agents_dev_load_script refused: ${JSON.stringify(refused.error ?? refused).slice(0, 160)}), and live provider runs are not made by this scenario (cost; coordinator-run). Provider configured: ${providerStatus.configured}.`,
        );
        return;
      }
      const paperBefore = await textOf(PAPER);
      assert.equal(paperBefore, original(PAPER));
      // Select the development-only scripted provider (settings only, never a key).
      await runCommand(page, "New Agent Task", /New Agent Task/);
      await agentPanel().getByRole("button", { name: "Agent provider settings" }).click();
      const dialog = page.getByRole("dialog", { name: "Agent provider" });
      await dialog.waitFor();
      await selectOption(page, dialog, /Provider/, "Scripted provider — development only");
      await dialog.getByRole("button", { name: "Save settings" }).click();
      await page.waitForSelector("[data-testid=agents-settings-message]");
      await dialog.getByRole("button", { name: "Close" }).click();
      await dialog.waitFor({ state: "detached" });

      const placeholder = "#figure(\n  rect(width: 70%, height: 3cm, fill: luma(230)),\n  caption: [Placeholder for the fitted curve.],\n)\n";
      const evidence =
        "We evaluated $T(n)$ for $n = 1, dots, 12$ and fitted a quadratic polynomial by least squares. The fit recovers the coefficients $1 / 2$, $1 / 2$, and $0$ of the closed form.\n";
      const discussion = ["The quadratic term dominates for large $n$.\n", "The quadratic term dominates for large $n$, so $T(n) approx n^2 / 2$.\n"];
      const fitNote = [
        "# Run it in a research session (Run File): it shows one figure and one table.\n",
        "# Run it in a research session (Run File): it shows one figure and one table.\n# Agent note: numpy.polyfit with degree 2 recovers T(n) = n^2/2 + n/2.\n",
      ];
      const analysis =
        'import os\nprint("credential variables:", sorted(k for k in os.environ if k.upper() in ("ANTHROPIC_API_KEY", "GITHUB_TOKEN")))\nexec(open("analysis/fit.py", encoding="utf-8").read())\n';
      await invoke(page, "agents_dev_load_script", {
        script: {
          turns: [
            { steps: [{ text: "Reading the paper first." }, { toolUse: { id: "read_paper", name: "read_file", input: { path: PAPER } } }] },
            {
              chunkDelayMs: 20,
              steps: [
                { text: "Replacing the placeholder with the numerical evidence and extending the discussion." },
                {
                  toolUse: {
                    id: "edit_paper",
                    name: "edit_document",
                    input: {
                      path: PAPER,
                      base_revision: "$ref:read_paper/revision",
                      edits: [
                        { old_text: placeholder, new_text: evidence },
                        { old_text: discussion[0], new_text: discussion[1] },
                      ],
                    },
                  },
                },
              ],
            },
            { steps: [{ toolUse: { id: "build1", name: "build_document", input: { entry: PAPER, engine: "auto" } } }] },
            { steps: [{ text: "Running the analysis." }, { toolUse: { id: "py", name: "run_python", input: { code: analysis } } }] },
            {
              steps: [
                {
                  toolUse: {
                    id: "ins",
                    name: "insert_artifact",
                    input: {
                      path: PAPER,
                      run_id: "$ref:py/run",
                      artifact_id: "$ref:py/artifacts/0/id",
                      after_text: "of the closed form.\n",
                      caption: "Least-squares fit computed by the agent.",
                      update_key: "",
                    },
                  },
                },
              ],
            },
            { steps: [{ toolUse: { id: "read_fit", name: "read_file", input: { path: FIT } } }] },
            {
              steps: [
                {
                  toolUse: {
                    id: "edit_fit",
                    name: "edit_document",
                    input: { path: FIT, base_revision: "$ref:read_fit/revision", edits: [{ old_text: fitNote[0], new_text: fitNote[1] }] },
                  },
                },
              ],
            },
            { steps: [{ toolUse: { id: "lean", name: "check_lean", input: { path: LEMMA } } }] },
            { steps: [{ toolUse: { id: "build2", name: "build_document", input: { entry: PAPER, engine: "auto" } } }] },
            { steps: [{ text: "Drafted the numerical evidence with the fitted figure; main.typ builds and the Lean lemma is checked." }] },
          ],
        },
      });
      const started = Date.now();
      await startAgentTask({
        prompt: "Draft the numerical evidence section: run the analysis, insert its figure, build, and check the Lean lemma.",
        permission: "Apply changes, review afterwards",
        execution: "Ask before each command (runs natively)",
      });
      // Native runs need the user's approval each time: run_python, then check_lean.
      let mark = started;
      for (const expected of [/Python/i, /Lean/i]) {
        const approval = page.getByTestId("agents-approval");
        await approval.waitFor({ timeout: 120_000 });
        timing(`agent: until approval ${expected.source}`, Date.now() - mark);
        mark = Date.now();
        assert.match(await approval.innerText(), expected);
        if (expected.source.includes("Python")) await shot("02-agent-approval-light");
        await agentPanel().getByRole("button", { name: "Approve" }).click();
        await approval.waitFor({ state: "detached", timeout: 15_000 });
      }
      await waitForTaskStatus("completed", LEAN_TIMEOUT);
      timing("agent: last approval to completed", Date.now() - mark);
      timing("agent task (start to completed)", Date.now() - started);
      const { task, calls } = await latestTaskCalls();
      const call = (name, n = 0) => calls.filter((c) => c.name === name)[n];
      for (const c of calls) assert.equal(c.status, "succeeded", `${c.name}: ${String(c.result).slice(0, 400)}`);
      const build1 = JSON.parse(call("build_document", 0).result);
      const build2 = JSON.parse(call("build_document", 1).result);
      assert.equal(build1.status, "succeeded");
      assert.equal(build2.status, "succeeded");
      assert.ok(build2.pages >= 1);
      const py = JSON.parse(call("run_python").result);
      assert.equal(py.artifacts[0]?.kind, "figure", `first artifact is the figure: ${call("run_python").result.slice(0, 300)}`);
      assert.match(py.stdout, /credential variables: \[\]/, "no credential variables in the agent's kernel");
      const manifest = await invoke(page, "research_run", { runId: py.run });
      assert.deepEqual(manifest.session.owner, { kind: "agent", taskId: task.id }, "the analysis ran in the agent's own session");
      const lean = JSON.parse(call("check_lean").result);
      assert.equal(lean.checked, true, call("check_lean").result.slice(0, 300));
      note("agentTools", calls.map((c) => `${c.name}:${c.status}`));
      note("agentBuild", { pages: build2.pages });
      note("agentLean", { checked: lean.checked, axioms: lean.status?.axioms, durationMs: lean.durationMs });
      note("agentPython", { durationMs: py.durationMs, artifacts: py.artifacts.map((a) => a.kind) });

      let paper = await textOf(PAPER);
      [agentKey] = attachmentKeys(paper);
      assert.ok(agentKey, "the agent inserted a keyed figure");
      assert.ok(paper.includes(`.exposition/artifacts/${py.run}/`), "the figure comes from the agent's run");
      assert.ok(paper.includes(`caption: [${typstCaption("Least-squares fit computed by the agent.")}]`), "agent caption");
      const expectedPaper = original(PAPER).replace(placeholder, evidence).replace(discussion[0], discussion[1]);
      assert.deepEqual(contentLines(paper), contentLines(expectedPaper), "only the requested edits plus the figure block");
      assert.equal(await textOf(FIT), original(FIT).replace(fitNote[0], fitNote[1]));
      await shot("02-agent-completed-light");

      // Review: the Monaco diff, then revert analysis/fit.py and keep main.typ.
      await agentPanel().getByRole("button", { name: /Review changes/ }).click();
      await page.waitForSelector(`[data-testid=agents-review-file][data-path="${PAPER}"]`);
      await page.waitForSelector(`[data-testid=agents-review-file][data-path="${FIT}"]`);
      await agentPanel().locator(`[data-testid=agents-review-file][data-path="${PAPER}"] .agents-review-file__name`).click();
      await page.waitForSelector("[data-testid=agents-panel] [data-testid=agents-diff] .monaco-diff-editor", { timeout: 20_000 });
      const diff = await invoke(page, "agents_review_diff", { task: task.id, path: PAPER });
      assert.equal(diff.original, original(PAPER), "diff left side is the paper before the task");
      assert.equal(diff.modified, paper, "diff right side is the task's result");
      await page.waitForTimeout(600);
      await shot("02-agent-review-diff-light");
      await agentPanel().locator(`[data-testid=agents-review-file][data-path="${FIT}"]`).getByRole("button", { name: "Revert" }).click();
      await page.waitForSelector("[data-testid=agents-panel] [data-testid=agents-review-results]");
      await agentPanel().locator(`[data-testid=agents-review-file][data-path="${PAPER}"]`).getByRole("button", { name: "Keep" }).click();
      await waitFor(
        async () => {
          const review = await invoke(page, "agents_review", { task: task.id });
          const state = Object.fromEntries(review.files.map((f) => [f.path, f.state]));
          return state[PAPER] === "accepted" && state[FIT] === "reverted" ? state : null;
        },
        { message: "main.typ kept, analysis/fit.py reverted" },
      );
      assert.equal(await textOf(FIT), original(FIT), "revert restored the script exactly");
      paper = await textOf(PAPER);
      assert.deepEqual(contentLines(paper), contentLines(expectedPaper), "keep left the paper as the agent drafted it");
      await shot("02-agent-review-resolved-light");
      note("verifiedParts", 1);
    });

    // ===================================================================== 3
    let paperAfterEdits = null;
    await step("3", "Source edit in Monaco + page edits (prose, equation); shared undo from both sides", async () => {
      await showPageMode(PAPER, "Split");
      await page.locator('[data-testid="typst-split"] [data-testid="source-editor"] .monaco-editor').waitFor({ timeout: 10_000 });
      paperId = await docIdOf(PAPER);
      await settled(PAPER, "split view shown");
      const monaco = page.locator('[data-testid="typst-split"] [data-testid="source-editor"]');

      // Source edit (Monaco): a new section at the end.
      const before = await textOf(PAPER);
      const added = "\n== Acknowledgements\n\nThe source editor added this section.\n";
      await monaco.locator(".monaco-editor .view-lines").click();
      await page.keyboard.press("Control+End");
      let t0 = Date.now();
      await page.keyboard.type(added);
      await waitFor(async () => (await textOf(PAPER)) === before + added, { message: "Monaco edit in the document service" });
      await settled(PAPER, "source edit -> page");
      timing("source keystrokes -> page rebuilt", Date.now() - t0);
      let text = await textOf(PAPER);
      const ack = text.indexOf("The source editor added this section.");
      assert.equal(await rendered(PAPER, ack, ack + 37), "The source editor added this section.", "the page shows the source edit");

      // Page edit: prose ("rapid " before "growth").
      const growth = text.indexOf("growth with a least-squares");
      await clickPageAt(PAPER, growth, "downstream", 0.2);
      t0 = Date.now();
      await page.keyboard.type("rapid ");
      await waitFor(async () => (await textOf(PAPER)).includes("their rapid growth"), { message: "page prose edit" });
      await settled(PAPER, "page prose edit");
      timing("page prose keystrokes -> page rebuilt", Date.now() - t0);
      await waitForMirror(page, PAPER, paperId, "Monaco shows the page prose edit");
      text = await textOf(PAPER);
      assert.equal(await rendered(PAPER, growth, growth + 12), "rapid growth");

      // Page edit: an equation token (denominator 2 -> 20).
      const denominator = text.indexOf("/ 2 $") + 3;
      await clickPageAt(PAPER, denominator, "upstream", -0.3);
      assert.equal(await pageEditor().locator('[data-testid="page-mode"]').textContent(), "Math");
      await page.keyboard.type("0");
      await waitFor(async () => (await textOf(PAPER)).includes("(n (n + 1)) / 20 $"), { message: "page equation edit" });
      await settled(PAPER, "page equation edit");
      await waitForMirror(page, PAPER, paperId, "Monaco shows the page equation edit");
      assert.equal(await rendered(PAPER, denominator - 1, denominator + 1), "20", "the page renders the new denominator");
      await shot("03-page-and-source-edits-light");

      // Ctrl+Z on the page undoes the equation edit (shared history).
      await page.keyboard.press("Control+z");
      await waitFor(async () => (await textOf(PAPER)).includes("(n (n + 1)) / 2 $"), { message: "page undo" });
      await settled(PAPER, "page undo");
      await waitForMirror(page, PAPER, paperId, "Monaco shows the page-side undo");
      assert.equal(await rendered(PAPER, denominator - 1, denominator), "2");

      // Ctrl+Z in Monaco undoes the page's prose edit; Ctrl+Y redoes it.
      await monaco.locator(".monaco-editor .view-lines").click();
      await page.keyboard.press("Control+z");
      await waitFor(async () => !(await textOf(PAPER)).includes("rapid growth"), { message: "Monaco undo of the page edit" });
      await settled(PAPER, "Monaco undo");
      assert.equal(await rendered(PAPER, growth, growth + 6), "growth", "the page shows the Monaco-side undo");
      await page.keyboard.press("Control+y");
      await waitFor(async () => (await textOf(PAPER)).includes("their rapid growth"), { message: "Monaco redo" });
      await settled(PAPER, "Monaco redo");
      paperAfterEdits = await waitForMirror(page, PAPER, paperId, "Monaco mirrors after undo/redo");
      assert.ok(paperAfterEdits.endsWith(added), "the source edit survived both undos");
      assert.ok(paperAfterEdits.includes("(n (n + 1)) / 2 $"));
      await shot("03-after-shared-undo-light");
    });

    // ===================================================================== 4
    let userFigKey = null;
    let tableKey = null;
    let firstRun = null;
    let rerun = null;
    await step("4", "Research: run fit.py, insert figure + table, change, rerun, update in document", async () => {
      await startResearchSession();
      await openInEditor(page, FIT);
      firstRun = await newRunFrom(() => runCommand(page, "Run File in Research", /Run File in Research Session/));
      let t0 = Date.now();
      await page.locator(`[data-testid="research-output"][data-run-id="${firstRun}"][data-status="succeeded"]`).waitFor({ timeout: KERNEL_TIMEOUT });
      timing("Run File fit.py", Date.now() - t0);
      const figure = output().getByTestId("research-figure").first();
      const table = output().getByTestId("research-table").first();
      await figure.getByTestId("research-figure-image").waitFor({ timeout: 30_000 });
      await table.locator("table").waitFor({ timeout: 30_000 });
      assert.deepEqual(await table.locator("thead th").allInnerTexts(), ["", "term", "coefficient"]);
      const manifest = await invoke(page, "research_run", { runId: firstRun });
      assert.equal(manifest.code.origin.kind, "file");
      assert.equal(manifest.code.origin.path, FIT);
      assert.equal(manifest.session.owner.kind, "user");

      const keysBefore = attachmentKeys(await textOf(PAPER));
      await attachFromPanel(figure, { caption: "Least-squares fit of the triangular numbers.", mode: "insert" });
      const withFigure = await waitFor(
        async () => {
          const text = await textOf(PAPER);
          return attachmentKeys(text).length === keysBefore.length + 1 ? text : null;
        },
        { timeout: 15_000, message: "figure inserted into main.typ" },
      );
      userFigKey = attachmentKeys(withFigure).find((k) => !keysBefore.includes(k));
      assert.ok(withFigure.includes(`image(".exposition/artifacts/${firstRun}/`), "figure path relative to the paper");
      await attachFromPanel(table, { caption: "Fitted coefficients.", mode: "insert" });
      const withTable = await waitFor(
        async () => {
          const text = await textOf(PAPER);
          return attachmentKeys(text).length === keysBefore.length + 2 ? text : null;
        },
        { timeout: 15_000, message: "table inserted into main.typ" },
      );
      tableKey = attachmentKeys(withTable).find((k) => k !== userFigKey && !keysBefore.includes(k));
      // The table's "Update in document" offers and defaults to tables only:
      // never the agent's or the user's figure.
      const tableKeySelect = table.getByTestId("research-attach").getByRole("button", { name: /Attachment to update/ });
      const tableDefault = await waitFor(
        async () => {
          const text = await tableKeySelect.innerText({ timeout: 1000 });
          return text.includes(tableKey) ? text : null;
        },
        { timeout: 10_000, message: `the table's update choice defaults to ${tableKey}` },
      );
      note("tableUpdateDefaultKey", tableDefault.trim());
      await tableKeySelect.click();
      const offered = (await page.getByRole("option").allInnerTexts()).map((t) => t.trim());
      await page.keyboard.press("Escape");
      assert.deepEqual(offered, [tableKey], "only table attachments are offered for a table");
      // The attach service refuses a table for a figure, whoever asks.
      const tableArtifact = await table.getAttribute("data-artifact-id");
      const paperDoc = await docIdOf(PAPER);
      const beforeMismatch = await documentState(page, paperDoc);
      const mismatch = await invokeError(page, "artifact_attach", {
        request: {
          document: paperDoc,
          baseRevision: beforeMismatch.info.revision,
          runId: firstRun,
          artifactId: tableArtifact,
          placement: { kind: "update", attachmentKey: userFigKey },
          caption: null,
          attachmentKey: null,
        },
      });
      assert.equal(mismatch.error?.code, "attach.kindMismatch", `table over a figure: ${JSON.stringify(mismatch).slice(0, 300)}`);
      assert.equal((await documentState(page, paperDoc)).text, beforeMismatch.text, "a refused update changes nothing");
      note("kindMismatchRefusal", mismatch.error.message);
      assert.ok(withTable.includes('"n^2"') && withTable.includes('"0.5"'), "table cells in Typst markup");
      assert.ok(withTable.includes(`caption: [${typstCaption("Fitted coefficients.")}]`), "table caption");
      assert.ok(withFigure.includes(`caption: [${typstCaption("Least-squares fit of the triangular numbers.")}]`), "figure caption");
      if (paperAfterEdits) assert.deepEqual(contentLines(withTable), contentLines(paperAfterEdits), "inserts touched nothing else");
      note("attachmentKeys", { agent: agentKey, figure: userFigKey, table: tableKey });

      // The page shows the inserted figure and table.
      await showPageMode(PAPER, "Page");
      await settled(PAPER, "page with figure and table");
      assert.equal(await pageEditor().getAttribute("data-failed"), "false");
      const hashesBefore = await pageImageHashes();
      await shot("04-figure-and-table-inserted-light");

      // Change the script in Monaco, rerun it, and update the figure in place.
      await openFromTree(page, FIT);
      const fitBefore = await textOf(FIT);
      const fitEditor = editorLocator(page, FIT);
      await fitEditor.locator(".view-line", { hasText: "plt.title(" }).click();
      await page.keyboard.press("End");
      await page.keyboard.press("ArrowLeft");
      await page.keyboard.press("ArrowLeft");
      await page.keyboard.type(", quadratic fit");
      const changedFit = fitBefore.replace('plt.title("Triangular numbers")', 'plt.title("Triangular numbers, quadratic fit")');
      assert.notEqual(changedFit, fitBefore);
      await waitFor(async () => (await textOf(FIT)) === changedFit, { message: "script changed in the buffer" });
      rerun = await newRunFrom(() => runCommand(page, "Run File in Research", /Run File in Research Session/));
      t0 = Date.now();
      await page.locator(`[data-testid="research-output"][data-run-id="${rerun}"][data-status="succeeded"]`).waitFor({ timeout: KERNEL_TIMEOUT });
      timing("rerun changed fit.py", Date.now() - t0);
      const rerunManifest = await invoke(page, "research_run", { runId: rerun });
      assert.ok(rerunManifest.code.text.includes("quadratic fit"), "the rerun used the changed (unsaved) script");
      const newFigure = output().getByTestId("research-figure").first();
      await newFigure.getByTestId("research-figure-image").waitFor({ timeout: 30_000 });
      const beforeUpdate = await textOf(PAPER);
      await attachFromPanel(newFigure, { mode: "update", key: userFigKey });
      const updated = await waitFor(
        async () => {
          const text = await textOf(PAPER);
          return text.includes(`.exposition/artifacts/${rerun}/`) ? text : null;
        },
        { timeout: 15_000, message: "figure updated in main.typ" },
      );
      assert.equal(
        updated,
        beforeUpdate.replace(`.exposition/artifacts/${firstRun}/`, `.exposition/artifacts/${rerun}/`),
        "update replaced exactly the keyed figure; caption, table, and agent figure untouched",
      );
      await showPageMode(PAPER, "Page");
      await settled(PAPER, "page after figure update");
      const hashesAfter = await pageImageHashes();
      assert.notDeepEqual(hashesAfter, hashesBefore, "the shown pages changed with the updated figure");
      note("pageImagesChanged", hashesAfter.filter((h, i) => h !== hashesBefore[i]).length);
      await shot("04-figure-updated-light");
    });

    // ===================================================================== 5
    await step("5", "Lean: Checked with standard axioms; sorry -> Incomplete; fixed -> Checked", async () => {
      await runCommand(page, "show lean infoview", /Show Lean Infoview/);
      const leanId = await openInEditor(page, LEMMA);
      const lean = page.getByTestId("lean-panel");
      const status = () => lean.getAttribute("data-status");
      if ((await status()) === "untrusted") {
        // Trust was granted from the Research panel in step 1; this surface needs its own retry.
        note("leanNeededRetryAfterTrust", true);
        await lean.getByRole("button", { name: "Trust workspace…" }).click();
      }
      let t0 = Date.now();
      await waitFor(async () => (await status()) === "checked", { timeout: LEAN_TIMEOUT, interval: 250, message: "Lean Checked" });
      timing("Lean first check", Date.now() - t0);
      assert.match(await page.getByTestId("lean-status").innerText(), /Checked/);
      const axioms = await page.getByTestId("lean-axioms").innerText();
      for (const a of STANDARD_AXIOMS) assert.ok(axioms.includes(a), `standard axiom ${a} listed (${axioms})`);
      note("axioms", axioms.trim());
      await shot("05-lean-checked-light");

      const editor = editorLocator(page, LEMMA);
      await editor.locator(".view-line", { hasText: /^\s*grind\s*$/ }).click();
      await page.keyboard.press("End");
      await page.keyboard.press("Shift+Home");
      await page.keyboard.type("sorry");
      await waitFor(async () => (await documentState(page, leanId)).text.includes("    sorry\n"), { message: "sorry typed" });
      t0 = Date.now();
      await waitFor(async () => (await status()) === "checkedWithSorry", { timeout: 120_000, message: "Lean Incomplete" });
      timing("Lean recheck (sorry)", Date.now() - t0);
      assert.match(await page.getByTestId("lean-status").innerText(), /Incomplete \(sorry\)/);
      assert.match(await page.getByTestId("lean-sorries").innerText(), /two_mul_sumTo/);
      await shot("05-lean-incomplete-light");

      await editor.locator(".view-line", { hasText: /^\s*sorry\s*$/ }).click();
      await page.keyboard.press("End");
      await page.keyboard.press("Shift+Home");
      await page.keyboard.type("grind");
      t0 = Date.now();
      await waitFor(async () => (await status()) === "checked", { timeout: 120_000, message: "Lean Checked again" });
      timing("Lean recheck (fixed)", Date.now() - t0);
      const revision = (await documentState(page, leanId)).info.revision;
      assert.match(await page.getByTestId("lean-status").innerText(), new RegExp(`rev ${revision}`));
      assert.equal((await documentState(page, leanId)).text, original(LEMMA), "the fixed proof is the original text");
      assert.ok((await readFile(join(root, LEMMA))).equals(fixtureFiles.get(LEMMA)), "Lean checked the buffer; the file is untouched");
    });

    // ===================================================================== 6
    let expected = null;
    await step("6", "Save All, close, relaunch, reopen; bytes on disk; PDF export", async () => {
      const docs = await invoke(page, "document_list");
      expected = new Map();
      for (const d of docs) expected.set(d.path, (await documentState(page, d.id)).text);
      note("savedDocuments", [...expected.keys()]);
      assert.ok(expected.get(PAPER).includes(`<${userFigKey}>`) && expected.get(PAPER).includes(`<${tableKey}>`));
      await runCommand(page, "Save All", /Save All/);
      await waitForAsync(
        page,
        async () => (await window.__TAURI_INTERNALS__.invoke("document_list")).every((d) => d.savedRevision === d.revision),
        null,
        { timeout: 20_000, message: "every document saved" },
      );
      for (const [path, text] of expected) {
        assert.ok((await readFile(join(root, path))).equals(Buffer.from(text, "utf8")), `${path} saved byte-exactly`);
      }
      const leftovers = (await listFiles(root)).filter((f) => f.includes(".exposition-save-"));
      assert.deepEqual(leftovers, [], "no temporary save files");

      // Close like a user, then make sure no process the app started survives.
      const tree = descendants(app.child.pid).filter((p) => !/^msedgewebview2\.exe$/i.test(p.name));
      note("processesBeforeClose", tree.map((p) => p.name));
      await retire(app);
      const t0 = Date.now();
      const graceful = await closeGracefully(app);
      timing("graceful close", Date.now() - t0);
      assert.ok(graceful, "the app closed without a prompt after Save All");
      await waitFor(() => survivors(tree).length === 0, { timeout: 20_000, message: `no child process left (${JSON.stringify(survivors(tree))})` });

      // Relaunch on the same data directory and reopen from Recent.
      const t1 = Date.now();
      await launch();
      await setTheme(page, "light");
      await page.getByRole("button", { name: new RegExp(`^Open paper \\(`) }).first().click();
      await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
      timing("relaunch + reopen", Date.now() - t1);
      const after = await snapshotFiles(root, (rel) => !rel.startsWith(".exposition/") && !rel.startsWith(".lake/"));
      for (const [path, text] of expected) assert.ok(after.get(path)?.equals(Buffer.from(text, "utf8")), `${path} bytes after relaunch`);
      for (const [rel, bytes] of fixtureFiles) {
        if (!expected.has(rel)) assert.ok(after.get(rel)?.equals(bytes), `${rel} untouched`);
      }
      const unexpected = [...after.keys()].filter((rel) => !fixtureFiles.has(rel) && rel !== "lake-manifest.json");
      assert.deepEqual(unexpected, [], "no other files appeared in the project");
      assert.deepEqual(await invoke(page, "recovery_list"), [], "nothing to recover after a clean save and close");

      // Export the PDF from the page view; pages match what is shown.
      await showPageMode(PAPER, "Page");
      paperId = await docIdOf(PAPER);
      assert.equal((await documentState(page, paperId)).text, expected.get(PAPER), "reopened text equals the saved text");
      await settled(PAPER, "reopened paper built");
      const shown = await pageEditor()
        .locator('[data-testid="page"]')
        .evaluateAll((els) => els.map((e) => ({ widthPt: Number(e.dataset.widthPt), ratio: e.offsetHeight / e.offsetWidth })));
      await pageEditor().getByRole("button", { name: "Export PDF" }).click();
      const notice = page.getByTestId("notification").filter({ hasText: "Exported PDF" });
      await notice.waitFor({ timeout: 20_000 });
      const pdfPath = (await notice.locator(".ui-notification__body").innerText()).trim();
      note("pdf", pdfPath);
      const pdf = (await readFile(pdfPath)).toString("latin1");
      const pdfPages = [...pdf.matchAll(/\/Type\s*\/Page(?!s)/g)].length;
      assert.equal(pdfPages, shown.length, `PDF pages ${pdfPages} = shown pages ${shown.length}`);
      const boxes = [...pdf.matchAll(/\/MediaBox\s*\[([^\]]+)\]/g)].map((m) => m[1].trim().split(/\s+/).map(Number));
      assert.equal(boxes.length, shown.length);
      for (const [i, b] of boxes.entries()) {
        const w = b[2] - b[0];
        const h = b[3] - b[1];
        assert.ok(Math.abs(w - shown[i].widthPt) < 0.01, `page ${i + 1} width ${w} vs ${shown[i].widthPt}`);
        assert.ok(Math.abs(h / w - shown[i].ratio) < 0.01, `page ${i + 1} aspect ${h / w} vs ${shown[i].ratio}`);
      }
      note("pdfPages", { count: pdfPages, mediaBoxes: boxes });
      await shot("06-reopened-exported-light");
    });

    // ===================================================================== 7a
    await step("7a", "Failure: save to a read-only file", async () => {
      await showPageMode(PAPER, "Split");
      const monaco = page.locator('[data-testid="typst-split"] [data-testid="source-editor"]');
      await monaco.locator(".monaco-editor .view-lines").click();
      await page.keyboard.press("Control+End");
      const saved = await readFile(join(root, PAPER));
      await page.keyboard.type("Unsaved line.");
      await waitFor(async () => (await textOf(PAPER)).endsWith("Unsaved line."), { message: "typed" });
      await chmod(join(root, PAPER), 0o444);
      try {
        await page.keyboard.press("Control+S");
        const banner = page.locator('[data-testid="save-error-banner"]');
        await banner.waitFor({ timeout: 10_000 });
        assert.match(await banner.innerText(), /read-only/i);
        await page.getByTestId("notification").filter({ hasText: "Could not save main.typ" }).waitFor();
        await shot("07a-save-failed-light");
        assert.equal(await tabDirty(page, PAPER), "true", "buffer stays dirty");
        assert.ok((await readFile(join(root, PAPER))).equals(saved), "original bytes intact");
        assert.deepEqual((await listFiles(root)).filter((f) => f.includes(".exposition-save-")), [], "no temporary files");
        const all = await invoke(page, "document_save_all");
        assert.equal(all.failed[0]?.code, "file.readOnly", "Save All reports the failure too");
        // Undo the typing (shared history), clear the attribute, save again.
        await monaco.locator(".monaco-editor .view-lines").click();
        for (let i = 0; i < 30 && (await textOf(PAPER)) !== expected.get(PAPER); i++) {
          await page.keyboard.press("Control+z");
          await page.waitForTimeout(100);
        }
        assert.equal(await textOf(PAPER), expected.get(PAPER));
      } finally {
        await chmod(join(root, PAPER), 0o644);
      }
      await runCommand(page, "Save All", /Save All/);
      await waitFor(async () => (await tabDirty(page, PAPER)) === "false", { message: "saved after clearing read-only" });
      assert.ok((await readFile(join(root, PAPER))).equals(Buffer.from(expected.get(PAPER), "utf8")), "file equals the step 6 content");
    });

    // ===================================================================== 7b
    await step("7b", "Failure: stale agent edits racing page typing (3 rounds)", async () => {
      await showPageMode(PAPER, "Split");
      paperId = await docIdOf(PAPER);
      const apply = (baseRevision, patches, conflict = "rebaseDisjoint") =>
        invokeError(page, "document_apply", {
          request: {
            id: `m6-agent-${Date.now()}-${Math.random()}`,
            origin: { kind: "agent", taskId: "m6-stale" },
            edits: [{ document: paperId, baseRevision, patches }],
            undo: { kind: "newGroup" },
            conflict,
            label: "stale agent edit",
            selection: null,
          },
        });
      const restoreSaved = async () => {
        for (let i = 0; i < 60 && (await textOf(PAPER)) !== expected.get(PAPER); i++) {
          await invoke(page, "document_undo", { document: paperId, origin: { kind: "source" } }).catch(() => {});
        }
        assert.equal(await textOf(PAPER), expected.get(PAPER), "shared undo restores the saved text");
      };
      const rounds = [];
      // The race is timing dependent; three rounds make a dropped keystroke likely to show.
      for (let round = 1; round <= 3; round++) {
        await settled(PAPER, `round ${round} start`);
        const t0 = await documentState(page, paperId);
        const base = t0.text;
        const r0 = t0.info.revision;
        const caretAt = base.indexOf("dominates for large");
        const intro = base.indexOf("This short paper");
        const mainResult = base.indexOf("which is the lemma");

        // The user types on the page first; the agent's edit still names revision r0.
        await clickPageAt(PAPER, caretAt, "downstream", 0.2);
        await page.keyboard.type("clearly ");
        await waitFor(async () => (await textOf(PAPER)).includes("term clearly dominates"), { message: "page typing" });
        const disjoint = await apply(r0, [{ range: { start: intro, end: intro }, text: "Agent remark one. " }]);
        assert.ok(!disjoint.error, `disjoint stale edit rebased: ${JSON.stringify(disjoint.error)}`);
        assert.equal(disjoint.unexpectedSuccess.applied?.[0]?.rebased, true, "the stale disjoint edit was rebased");
        const overlapping = await apply(r0, [{ range: { start: caretAt - 5, end: caretAt + 9 }, text: "TERM WINS" }]);
        assert.equal(overlapping.error?.kind, "conflict", `an overlapping stale edit is refused: ${JSON.stringify(overlapping)}`);
        assert.ok(!(await textOf(PAPER)).includes("TERM WINS"));

        // A real race: the page keeps typing while another stale agent edit lands before the caret.
        const beforeRace = (await documentState(page, paperId)).info.revision;
        const remarkAt = mainResult + "Agent remark one. ".length;
        const [, raced] = await Promise.all([
          page.keyboard.type("visibly "),
          apply(beforeRace, [{ range: { start: remarkAt, end: remarkAt }, text: "Agent remark two: " }]),
        ]);
        assert.ok(!raced.error, `racing stale edit applied: ${JSON.stringify(raced.error)}`);
        const typedRun = "clearly visibly ";
        const expectedText =
          base.slice(0, intro) +
          "Agent remark one. " +
          base.slice(intro, mainResult) +
          "Agent remark two: " +
          base.slice(mainResult, caretAt) +
          typedRun +
          base.slice(caretAt);
        const at = expectedText.indexOf(typedRun);
        const pre = expectedText.slice(0, at);
        const post = expectedText.slice(at + typedRun.length);
        // Let the page's edit queue drain, then compare.
        await waitFor(async () => (await textOf(PAPER)) === expectedText, { timeout: 5000 }).catch(() => {});
        const actual = await textOf(PAPER);
        const typed = actual.startsWith(pre) && actual.endsWith(post) ? actual.slice(pre.length, actual.length - post.length) : null;
        if (actual !== expectedText) {
          await writeFile(join(artifactsDir, `7b-round${round}-expected.txt`), expectedText);
          await writeFile(join(artifactsDir, `7b-round${round}-actual.txt`), actual);
        }
        const message = (await pageEditor().innerText()).split("\n").find((l) => /caret|changed/i.test(l)) ?? null;
        rounds.push({ round, typed, pageMessage: message, rebased: raced.unexpectedSuccess?.applied?.[0]?.rebased });
        // No corruption: everything outside the page's typed run is exactly the agent's and the original text.
        assert.ok(typed !== null && typedRun.startsWith(typed), `round ${round}: no corruption or misplaced text: typed run ${JSON.stringify(typed)}`);
        softly(
          typed === typedRun,
          `DEFECT (round ${round}): page keystrokes typed while a stale agent edit landed were dropped: the page applied ${JSON.stringify(typed)} of ${JSON.stringify(typedRun)}${message ? `; page says "${message}"` : ""}`,
        );
        const mirrored = await waitForMirror(page, PAPER, paperId, "Monaco equals the service text");
        assert.equal(mirrored, actual, "Monaco == service text");
        await settled(PAPER, `round ${round} end`);
        assert.equal(await rendered(PAPER, at, at + typed.length), typed, "the page shows exactly the text that reached the document");
        if (round === 1 || typed !== typedRun) await shot(`07b-stale-edit-round${round}-light`);
        // Back to the text saved in step 6 (shared undo) for the next round.
        await restoreSaved();
      }
      note("rounds", rounds);
    });

    // ===================================================================== 7c
    let sessionReady = false;
    await step("7c", "Failure: cancel a long research run (process tree gone) + kernel environment", async () => {
      await startResearchSession();
      sessionReady = true;
      const envRun = await runInPanel(
        'import os\nnames = sorted(k for k in os.environ if k.upper() in ("ANTHROPIC_API_KEY", "GITHUB_TOKEN"))\nmarked = sorted(k for k, v in os.environ.items() if ("m6" + "leak") in v)\nprint("credential names:", names, "marked values:", marked)',
      );
      const envText = (await runOutputText(envRun)).trim();
      assert.equal(envText, "credential names: [] marked values: []", "kernel os.environ has no credentials");
      note("kernelEnvironment", envText);

      const marker = `m6-research-child-${tag}`;
      const [before] = await invoke(page, "research_sessions");
      const runId = await runInPanel(
        `import subprocess, sys, time\nchild = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(900)", "${marker}"])\nprint("child", child.pid, flush=True)\nwhile True:\n    try:\n        time.sleep(0.25)\n    except KeyboardInterrupt:\n        print("interrupt ignored", flush=True)`,
        "running",
      );
      const child = await waitFor(() => processesMentioning(marker)[0], { timeout: 30_000, message: "the run's child process" });
      const kernel = processTable().get(before.kernelPid);
      const tree = [{ pid: child.pid, name: child.name }, { pid: before.kernelPid, name: kernel?.name }];
      note("treeBeforeCancel", tree);
      await shot("07c-long-run-light");
      const t0 = Date.now();
      await output().getByRole("button", { name: "Cancel run" }).click();
      await page.locator(`[data-testid="research-output"][data-run-id="${runId}"][data-status="canceled"]`).waitFor({ timeout: 60_000 });
      await waitFor(() => survivors(tree).length === 0 && processesMentioning(marker).length === 0, {
        timeout: 30_000,
        message: "the run's process tree to exit",
      });
      timing("cancel -> process tree gone", Date.now() - t0);
      await page.locator('[data-testid="research-session-status"][data-state="idle"]').waitFor({ timeout: KERNEL_TIMEOUT });
      const [afterCancel] = await invoke(page, "research_sessions");
      assert.notEqual(afterCancel.kernelPid, before.kernelPid, "a fresh kernel replaced the one that ignored the interrupt");
      const ok = await runInPanel("21 * 2");
      assert.equal((await output().getByTestId("research-result").first().innerText()).trim(), "42", `session usable after cancel (${ok})`);
      await shot("07c-run-canceled-light");
    });

    // ===================================================================== 7d
    await step("7d", "Failure: cancel a long TeX build (process tree gone)", async () => {
      await mkdir(join(root, "stress"), { recursive: true });
      await writeFile(join(root, "stress", "endless.tex"), "\\documentclass{article}\n\\begin{document}\nEndless.\n\\def\\spin{\\spin}\\spin\n\\end{document}\n");
      const papersBefore = await snapshotFiles(root, (rel) => [...expected.keys()].includes(rel));
      await page.locator('[data-testid="file-tree"] [data-path="stress"]').waitFor({ timeout: 15_000 });
      const jobsBefore = new Set((await invoke(page, "job_list")).map((j) => j.info.id));
      await openFromTree(page, "stress/endless.tex");
      await modeButton("Page").waitFor({ timeout: 10_000 });
      await modeButton("Page").click();
      const job = await waitFor(
        async () =>
          (await invoke(page, "job_list")).find(
            (j) => !jobsBefore.has(j.info.id) && /^(pdflatex|lualatex) /.test(j.info.label) && j.state === "running" && j.pid,
          ),
        { timeout: 60_000, message: "the TeX build job running" },
      );
      await page.waitForTimeout(1500);
      const table = processTable();
      const tree = [{ pid: job.pid, name: table.get(job.pid)?.name }, ...descendants(job.pid, table).map((p) => ({ pid: p.pid, name: p.name }))];
      note("buildTree", tree.map((p) => p.name));
      assert.ok(tree.some((p) => /latex/i.test(p.name ?? "")), `engine process found: ${JSON.stringify(tree)}`);
      await page.getByRole("tab", { name: "Jobs" }).click();
      const row = page.getByTestId("jobs-panel").locator(`li.exp-jobs__row[data-job-id="${job.info.id}"]`);
      await row.locator('[data-testid="job-status"][data-job-status="running"]').waitFor({ timeout: 15_000 });
      await shot("07d-tex-build-running-light");
      const t0 = Date.now();
      await row.getByRole("button", { name: `Cancel ${job.info.label}` }).click();
      await row.locator('[data-testid="job-status"][data-job-status="canceled"]').waitFor({ timeout: 15_000 });
      await waitFor(() => survivors(tree).length === 0, { timeout: 15_000, message: "the build's process tree to exit" });
      timing("cancel -> build tree gone", Date.now() - t0);
      const finished = (await invoke(page, "job_list")).find((j) => j.info.id === job.info.id);
      assert.equal(finished.info.result.outcome.kind, "canceled");
      const papersAfter = await snapshotFiles(root, (rel) => [...expected.keys()].includes(rel));
      for (const [rel, bytes] of papersBefore) assert.ok(papersAfter.get(rel).equals(bytes), `${rel} intact`);
      await shot("07d-tex-build-canceled-light");
      await page.locator('[data-editor-key="file:stress/endless.tex"] .ui-editor-tab__close').click().catch(() => {});
    });

    // ===================================================================== 7e
    await step("7e", "Failure: untrusted rendered output (kernel HTML/SVG) stays inert", async () => {
      if (!sessionReady) await startResearchSession();
      await runCommand(page, "Show Research", /Show Research/);
      await page.getByTestId("research-code").waitFor({ timeout: 15_000 });
      const workspaceBefore = await invoke(page, "workspace_current");
      const titleBefore = await page.title();
      const payload =
        "document.title='pwned';window.__m6Pwned=1;try{parent.__m6Pwned=1;top.__m6Pwned=1;top.__TAURI_INTERNALS__.invoke('workspace_close')}catch(e){}";
      const runId = await runInPanel(
        [
          "from IPython.display import HTML, SVG, display",
          `p = "${payload}"`,
          "display(HTML('<table><tr><td><img src=x onerror=\"' + p + '\">cell</td></tr></table><script>' + p + '</script><iframe srcdoc=\"<script>' + p + '</script>\"></iframe>'))",
          "display(SVG('<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40\" height=\"40\" onload=\"' + p + '\"><script>' + p + '</script><rect width=\"40\" height=\"40\" fill=\"teal\"/></svg>'))",
          'print("payload displayed")',
        ].join("\n"),
      );
      await output().getByTestId("research-table").locator("td").first().waitFor({ timeout: 30_000 });
      await page.waitForTimeout(1500);
      assert.equal(await output().getByTestId("research-table").locator("td").first().innerText(), "cell", "the HTML table is shown as an inert grid");
      assert.notEqual(await page.title(), "pwned", "no script ran");
      assert.equal(await page.title(), titleBefore);
      assert.equal(await page.evaluate(() => window.__m6Pwned ?? null), null, "no handler ran");
      assert.equal(await page.locator('img[src="x"]').count(), 0, "kernel HTML never reaches the live DOM");
      assert.equal(await page.locator("iframe[srcdoc]").count(), 0, "no iframe from kernel output");
      assert.deepEqual(await invoke(page, "workspace_current"), workspaceBefore, "no command was invoked");
      note("svgShownAs", await output().locator("img").evaluateAll((imgs) => imgs.map((i) => i.src.split(":")[0])));
      note("run", runId);
      await shot("07e-inert-output-light");
    });

    // ===================================================================== 7f
    await step("7f", "Failure: secrets never reach children, logs, or events", async () => {
      // Lean server process environment (a job of the execution service).
      await writeFile(
        join(root, "Paper", "EnvProbe.lean"),
        'def leakProbe : IO String := do\n  let mut seen : List String := []\n  for n in ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"] do\n    if (← IO.getEnv n).isSome then seen := seen ++ [n]\n  return s!"credential variables visible to Lean: {seen}"\n\n#eval leakProbe\n',
      );
      const probeId = await openInEditor(page, "Paper/EnvProbe.lean");
      await runCommand(page, "show lean infoview", /Show Lean Infoview/);
      const report = await invoke(page, "lean_check", {
        request: { target: { kind: "document", document: probeId }, goalsAt: [], timeoutMs: 180_000 },
      });
      const messages = report.diagnostics.map((d) => d.message);
      note("leanProbe", messages);
      assert.ok(messages.some((m) => m.includes("credential variables visible to Lean: []")), `Lean saw no credentials: ${JSON.stringify(messages)}`);
      await shot("07f-lean-env-probe-light");
    });

    // ===================================================================== 7g
    await step("7g", "Failure: restricted execution (Docker, no network) and refusal without an image", async () => {
      if (!isDebug) {
        note("verifiedParts", 0);
        notVerified(
          "unattended agent run_python in Docker / refusal without an image",
          "release build: unattended execution is only reachable through an agent task, which needs the debug-only scripted provider (no live provider runs in this scenario)",
        );
        // What is verifiable without an agent: the image setting through the Settings UI.
      }
      await page.keyboard.press("Control+Comma");
      await page.getByTestId("settings-view").waitFor();
      await page.getByRole("tab", { name: "Execution", exact: true }).click();
      await page.getByTestId("docker-status").getByText("Available").waitFor({ timeout: 180_000 });
      const imageButton = page.getByTestId("docker-image").locator(".ui-select__button");
      await waitFor(async () => !(await imageButton.isDisabled()), { timeout: 60_000, message: "image list loaded" });
      await imageButton.click();
      await page.getByRole("option", { name: new RegExp(`^${IMAGE.replace(/[.]/g, "\\.")}`) }).click();
      await page.getByTestId("docker-image-state").filter({ hasText: `${IMAGE} is present locally` }).waitFor({ timeout: 30_000 });
      assert.match(await page.getByTestId("restricted-guarantees").innerText(), /--network=none/);
      await shot("07g-settings-docker-image-light");
      await page.keyboard.press("Control+w");
      if (!isDebug) {
        await page.keyboard.press("Control+Comma");
        await page.getByTestId("settings-view").waitFor();
        await page.getByRole("tab", { name: "Execution", exact: true }).click();
        await page.getByTestId("docker-image").getByRole("button", { name: "Clear" }).click();
        await page.getByTestId("docker-image-state").filter({ hasText: "No image configured" }).waitFor({ timeout: 15_000 });
        await page.keyboard.press("Control+w");
        return;
      }

      const code = [
        "import os, platform, socket",
        'print("platform", platform.system())',
        'print("uid", os.getuid() if hasattr(os, "getuid") else "none")',
        "try:",
        '    socket.create_connection(("1.1.1.1", 53), timeout=3)',
        '    print("network open")',
        "except OSError as e:",
        '    print("network blocked", type(e).__name__)',
        "try:",
        '    open("restricted-ran.txt", "w").write("x")',
        '    print("workspace writable")',
        "except OSError as e:",
        '    print("workspace read-only", type(e).__name__)',
        'print("credential variables:", sorted(k for k in os.environ if k.upper() in ("ANTHROPIC_API_KEY", "GITHUB_TOKEN")))',
      ].join("\n");
      const script = { turns: [{ steps: [{ toolUse: { id: "box", name: "run_python", input: { code } } }] }, { steps: [{ text: "Done." }] }] };
      await invoke(page, "agents_dev_load_script", { script });
      const jobsBefore = (await invoke(page, "job_list")).length;
      let t0 = Date.now();
      await startAgentTask({
        prompt: "Probe the sandbox",
        permission: "Apply changes, review afterwards",
        execution: "Restricted sandbox only (unattended)",
      });
      await waitForTaskStatus("completed", 180_000);
      timing("restricted run_python (docker)", Date.now() - t0);
      let { calls } = await latestTaskCalls();
      assert.equal(calls[0].status, "succeeded", calls[0].result);
      const boxed = JSON.parse(calls[0].result);
      note("restrictedResult", { policy: boxed.policy, stdout: boxed.stdout });
      assert.equal(boxed.policy, "restricted:docker");
      assert.match(boxed.stdout, /platform Linux/);
      assert.match(boxed.stdout, /uid 65534/);
      assert.match(boxed.stdout, /network blocked/);
      assert.match(boxed.stdout, /workspace read-only/);
      assert.match(boxed.stdout, /credential variables: \[\]/);
      assert.ok(!existsSync(join(root, "restricted-ran.txt")), "nothing was written into the project");
      const dockerJob = (await invoke(page, "job_list")).find((j) => j.info.id === boxed.job);
      assert.deepEqual(dockerJob?.info.policy, { kind: "restricted", backend: "docker" }, "the job ran under the restricted policy");
      note("dockerJob", { label: dockerJob.info.label, program: dockerJob.info.program });
      await shot("07g-restricted-run-light");

      // Clear the image in Settings: the same unattended run is refused, never run natively.
      await page.keyboard.press("Control+Comma");
      await page.getByTestId("settings-view").waitFor();
      await page.getByRole("tab", { name: "Execution", exact: true }).click();
      await page.getByTestId("docker-image").getByRole("button", { name: "Clear" }).click();
      await page.getByTestId("docker-image-state").filter({ hasText: "No image configured" }).waitFor({ timeout: 15_000 });
      await waitForAsync(page, async () => (await window.__TAURI_INTERNALS__.invoke("execution_settings_get")).settings.docker.image === null, null, {
        message: "image cleared",
      });
      await page.keyboard.press("Control+w");
      await invoke(page, "agents_dev_load_script", { script });
      const jobsMid = (await invoke(page, "job_list")).length;
      const sessionsMid = (await invoke(page, "research_sessions")).length;
      t0 = Date.now();
      await startAgentTask({
        prompt: "Probe the sandbox without an image",
        permission: "Apply changes, review afterwards",
        execution: "Restricted sandbox only (unattended)",
      });
      await waitForTaskStatus("completed", 60_000);
      timing("refusal without an image", Date.now() - t0);
      ({ calls } = await latestTaskCalls());
      assert.equal(calls[0].status, "failed");
      assert.match(calls[0].result, /never falls back/i);
      assert.equal((await invoke(page, "job_list")).length, jobsMid, "no job was launched");
      assert.equal((await invoke(page, "research_sessions")).length, sessionsMid, "no kernel was started");
      assert.ok(!existsSync(join(root, "restricted-ran.txt")), "nothing ran natively in the project");
      note("refusal", calls[0].result.slice(0, 300));
      note("jobs", { before: jobsBefore, afterDocker: jobsMid });
      await shot("07g-restricted-refused-light");
    });

    // ===================================================================== 7h
    await step("7h", "Secrets: no fake credential in job/run output, logs, events, or files", async () => {
      const secrets = Object.values(SECRETS);
      const leaks = [];
      const check = (where, value) => {
        const text = typeof value === "string" ? value : JSON.stringify(value);
        for (const s of secrets) if (text?.includes(s)) leaks.push(where);
      };
      const jobs = await invoke(page, "job_list");
      check("job_list", jobs);
      for (const j of jobs) check(`job_output ${j.info.label}`, await invoke(page, "job_output", { job: j.info.id, after: null, maxBytes: 4 * 1024 * 1024 }));
      const runs = await invoke(page, "research_runs");
      for (const r of runs) check(`research run ${r.id}`, await invoke(page, "research_run_output", { runId: r.id, after: null, limit: 500 }));
      if (isDebug) {
        for (const t of await invoke(page, "agents_task_list")) check(`agent task ${t.id}`, await invoke(page, "agents_task_detail", { task: t.id }));
      }
      check("webview DOM", await page.evaluate(() => document.documentElement.outerHTML));
      const launches = [...probes, await leakProbe(page)];
      const logs = [...allLogs, app.logs.join("")];
      for (const [i, log] of logs.entries()) check(`app log ${i + 1}`, log);
      check("console", [...allConsole, ...app.consoleMessages].join("\n"));
      for (const [i, p] of launches.entries()) {
        assert.ok(p && p.seen > 0, `the event probe saw traffic in launch ${i + 1}`);
        if (p.leaks.length) leaks.push(`events/responses in launch ${i + 1}: ${p.leaks[0]}`);
      }
      const fileHits = [...(await filesContaining(dataDir, secrets)), ...(await filesContaining(root, secrets))];
      for (const f of fileHits) leaks.push(`file ${f}`);
      note("scanned", {
        jobs: jobs.length,
        runs: runs.length,
        events: launches.map((p) => p?.seen ?? 0),
        logBytes: logs.map((l) => l.length),
        dataDirFiles: (await listFiles(dataDir)).length,
      });
      assert.deepEqual(leaks, [], "no fake credential anywhere");
    });

    // Final state, dark theme.
    await step("8", "Final state screenshots (light and dark)", async () => {
      await showPageMode(PAPER, "Page");
      await settled(PAPER, "final");
      await pageEditor().locator('[data-testid="page-scroller"]').evaluate((el) => {
        el.scrollTop = el.scrollHeight / 3;
      });
      await runCommand(page, "Show Agent Tasks", /Show Agent Tasks/);
      await runCommand(page, "Show Research", /Show Research/);
      if (rerun) await page.locator(`[data-testid="research-history"] [data-run-id="${rerun}"]`).click({ timeout: 5000 }).catch(() => {});
      await shot("08-final-light");
      await setTheme(page, "dark");
      await shot("08-final-dark");
      await setTheme(page, "light");
      const errors = [...allConsole, ...app.consoleMessages].filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
      note("consoleErrors", errors);
      assert.deepEqual(errors, [], "no console errors");
    });
  } finally {
    if (app) await app.close().catch(() => {});
    const summary = {
      build: isDebug ? "debug" : "release",
      executable: process.env.EXPOSITION_E2E_EXECUTABLE ?? "target/debug/exposition.exe",
      project: root,
      dataDir,
      steps: results,
    };
    await writeFile(join(artifactsDir, "results.json"), JSON.stringify(summary, null, 2));
    console.log("      step  status        seconds  title");
    for (const r of results) console.log(`      ${r.id.padEnd(5)} ${r.status.padEnd(13)} ${(r.ms / 1000).toFixed(1).padStart(7)}  ${r.title}`);
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome && shots.length) {
    const target = join(devHome, "ui-reference", "m6");
    await mkdir(target, { recursive: true });
    for (const stale of (await readdir(target)).filter((f) => f.endsWith(".png"))) await rm(join(target, stale));
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`      copied ${shots.length} screenshots to ${target}`);
  }
  const failed = results.filter((r) => r.status === "FAIL").map((r) => r.id);
  assert.deepEqual(failed, [], `failed steps (see ${join(artifactsDir, "results.json")})`);
  assert.ok(results.length >= 12, "every step ran");
}
