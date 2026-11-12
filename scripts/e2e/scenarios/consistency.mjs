// M6 consistency gaps in the real app:
// - Settings files that are invalid at startup are set aside and the user is
//   told once, naming the kept file (application, execution, project settings);
//   hand-edited container limits/user are validated (never root).
// - Mixed line endings: the page view shows the read-only reason, page typing
//   and agent-origin edits are refused by the document service until the
//   file is normalized; undoing the normalization makes it read-only again.
// - Project settings: LaTeX targets in Settings > Project; the palette's
//   engine switch rebuilds the open page view with LuaLaTeX; the targets table
//   switches it back to pdfLaTeX.
// - Recent: "Remove from Recent"; files.reopenLastProject reopens the last
//   project on the next start, and leaves a notice when it is gone.
// Screenshots (light and dark) go to test-results/e2e/consistency/.
//
// Requires Typst (bundled) and TeX (pdflatex, lualatex) discoverable by the
// execution service.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readdir, readFile, rename, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { waitForAsync } from "../wait.mjs";
import { openFromTree } from "../workspace-helpers.mjs";

const VIEWPORT = { width: 1440, height: 900 };
const MIXED = "= Mixed\r\n\nSome text on the page.\n";

const invoke = (page, cmd, args = {}) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
const rejection = (page, cmd, args = {}) =>
  page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a).then(() => null, (e) => e), [cmd, args]);

async function waitFor(what, fn, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  let last;
  for (;;) {
    last = await fn();
    if (last) return last;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what} (last: ${JSON.stringify(last)})`);
    await new Promise((r) => setTimeout(r, 150));
  }
}

/** Close like a user (WM_CLOSE) so the app shuts down normally. */
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
  await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 15_000))]);
  await app.close();
}

function notification(page, title) {
  return page.getByTestId("notification").filter({ hasText: title });
}

async function dismissAll(page) {
  const dismiss = page.getByRole("button", { name: /^Dismiss notification/ });
  for (let n = await dismiss.count(); n > 0; n--) await dismiss.first().click();
}

async function settle(page) {
  await page.waitForFunction(
    () =>
      !document.querySelector("[data-entering], [data-exiting]") &&
      document.getAnimations().every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity),
    null,
    { timeout: 5000 },
  );
}

async function setTheme(page, theme) {
  await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
  await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
}

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

export default async function consistency({ launchApp, artifactsDir, assert, fixture }) {
  const evidence = {};
  const shots = [];
  /** Screenshot in light and dark (returns to light). */
  const bothThemes = async (page, name, { keepNotifications = false } = {}) => {
    if (!keepNotifications) await dismissAll(page);
    for (const theme of ["dark", "light"]) {
      await setTheme(page, theme);
      // The palette's own notifications (none) and focus rings settle first.
      await settle(page);
      const path = join(artifactsDir, `${name}-${theme}.png`);
      await page.screenshot({ path });
      shots.push(path);
    }
  };

  const base = await mkdtemp(join(tmpdir(), "xp-consistency-"));
  const dataDir = join(base, "data");
  const paper = join(base, "paper");
  const thesis = join(base, "thesis");
  await mkdir(join(dataDir, "execution"), { recursive: true });
  await mkdir(paper, { recursive: true });
  await writeFile(join(paper, "main.typ"), "= Paper\n\nHello world.\n");
  await writeFile(join(paper, "mixed.typ"), MIXED);
  await cp(fixture("latex-basic"), thesis, { recursive: true });
  await mkdir(join(thesis, ".exposition"), { recursive: true });
  const badProject = '{"version":1,"targets":[],"defaultTarget":"missing"}';
  await writeFile(join(thesis, ".exposition", "project.json"), badProject);
  // Invalid settings files, as a user's hand edit could leave them.
  const badApp = '{"version":1,"editor":{"fontSize":400}}';
  const badExecution = '{"docker":{"image":null,"memory":"lots","user":"0:0","pidsLimit":0}}';
  await writeFile(join(dataDir, "settings.json"), badApp);
  await writeFile(join(dataDir, "execution", "settings.json"), badExecution);
  const env = { EXPOSITION_TOOL_CACHE_DIR: join(artifactsDir, "..", "tool-cache") };

  // ---------------- First start ----------------
  let app = await launchApp({ dataDir, env });
  try {
    const { page } = app;
    await page.setViewportSize(VIEWPORT);

    // --- 3. Invalid settings files: set aside, defaults in use, told once.
    const appNotice = notification(page, "Invalid application settings were reset");
    const execNotice = notification(page, "Invalid execution settings were reset");
    await appNotice.waitFor({ timeout: 20_000 });
    await execNotice.waitFor({ timeout: 20_000 });
    const kept = (await readdir(dataDir)).filter((n) => n.startsWith("settings.json.invalid-"));
    const keptExec = (await readdir(join(dataDir, "execution"))).filter((n) => n.startsWith("settings.json.invalid-"));
    assert.equal(kept.length, 1, "application settings kept once");
    assert.equal(keptExec.length, 1, "execution settings kept once");
    assert.equal(await readFile(join(dataDir, kept[0]), "utf8"), badApp, "original application settings kept byte for byte");
    assert.equal(await readFile(join(dataDir, "execution", keptExec[0]), "utf8"), badExecution);
    const appText = await appNotice.innerText();
    const execText = await execNotice.innerText();
    assert.ok(appText.includes(kept[0]) && /font size/i.test(appText), `names the kept file: ${appText}`);
    assert.ok(execText.includes(keptExec[0]) && execText.includes("as root"), `names the kept file: ${execText}`);
    const execution = await invoke(page, "execution_settings_get");
    assert.deepEqual(
      { user: execution.settings.docker.user, memory: execution.settings.docker.memory, pids: execution.settings.docker.pidsLimit },
      { user: "65534:65534", memory: "2g", pids: 256 },
      "invalid container values fall back to defaults (never root)",
    );
    assert.equal((await invoke(page, "settings_get")).settings.editor.fontSize, 13);
    evidence.settingsWarnings = { app: appText, execution: execText, kept, keptExec };
    await bothThemes(page, "settings-reset-notifications", { keepNotifications: true });
    // The Execution page says it too.
    await page.keyboard.press("Control+Comma");
    await page.getByRole("tab", { name: "Execution", exact: true }).click();
    await page.getByTestId("execution-settings-warning").waitFor();
    await dismissAll(page);

    // --- 2. Turn on "Reopen the last project" in Settings > Project.
    await page.getByRole("tab", { name: "Project", exact: true }).click();
    const reopen = page.getByRole("switch", { name: "Reopen the last project" });
    await reopen.waitFor();
    assert.equal(await reopen.isChecked(), false, "off by default");
    await reopen.focus();
    await page.keyboard.press("Space");
    await waitForAsync(page, async () => (await window.__TAURI_INTERNALS__.invoke("settings_get")).settings.files.reopenLastProject, null, {
      message: "reopenLastProject saved",
    });
    await page.keyboard.press("Control+w");

    // --- 4. Mixed line endings: read-only for every view and origin.
    await invoke(page, "workspace_open", { path: paper });
    await page.locator('[data-testid="file-tree"] [data-path="mixed.typ"]').waitFor({ timeout: 10_000 });
    await openFromTree(page, "mixed.typ");
    const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
    await modeButton("Page").waitFor({ timeout: 10_000 });
    await modeButton("Page").click();
    const pageEditor = page.locator('[data-testid="page-editor"]');
    await pageEditor.waitFor();
    const readOnly = page.getByTestId("page-read-only");
    await readOnly.waitFor({ timeout: 10_000 });
    assert.match(await readOnly.getAttribute("data-reason"), /mixes line endings/);
    await waitFor("pages built", async () => (await pageEditor.getAttribute("data-build")) !== "", 60_000);
    const doc = (await invoke(page, "document_list")).find((d) => d.path === "mixed.typ");
    assert.equal(doc.lineEnding, "mixed");
    // Typing on the page is refused with the same reason, nothing is sent.
    await page.locator('[data-testid="page"]').first().click({ position: { x: 120, y: 120 } });
    await page.locator('[data-testid="page-input"]').focus();
    await page.keyboard.type("x");
    await page.getByTestId("page-message").filter({ hasText: "mixes line endings" }).waitFor({ timeout: 5000 });
    // An agent (or any other origin) is refused by the document service.
    const agentEdit = (revision) => ({
      request: {
        id: `e2e-agent-${revision}`,
        origin: { kind: "agent", taskId: "e2e" },
        edits: [{ document: doc.id, baseRevision: revision, patches: [{ range: { start: 0, end: 0 }, text: "// agent\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: "Agent edit",
        selection: null,
      },
    });
    const refused = await rejection(page, "document_apply", agentEdit(1));
    assert.equal(refused?.kind, "mixedLineEndings", `agent edit refused: ${JSON.stringify(refused)}`);
    let state = await invoke(page, "document_state", { document: doc.id });
    assert.equal(state.text, MIXED, "nothing changed");
    assert.equal(state.info.revision, 1);
    evidence.mixedRefusal = refused;
    await bothThemes(page, "page-mixed-read-only");
    // Normalize from the page view: editable again, by every origin.
    await readOnly.getByRole("button", { name: "Normalize to LF" }).click();
    await readOnly.waitFor({ state: "detached" });
    state = await invoke(page, "document_state", { document: doc.id });
    assert.equal(state.text, MIXED.replace(/\r\n/g, "\n"));
    assert.equal(state.info.lineEnding, "lf");
    await invoke(page, "document_apply", agentEdit(state.info.revision));
    // Undo the edit and the normalization: mixed and read-only again.
    await invoke(page, "document_undo", { document: doc.id, origin: { kind: "page" } });
    await invoke(page, "document_undo", { document: doc.id, origin: { kind: "page" } });
    state = await invoke(page, "document_state", { document: doc.id });
    assert.equal(state.text, MIXED);
    assert.equal(state.info.lineEnding, "mixed");
    await readOnly.waitFor({ timeout: 5000 });
    assert.equal((await rejection(page, "document_apply", agentEdit(state.info.revision)))?.kind, "mixedLineEndings");
    await invoke(page, "document_close", { document: doc.id });
    await pageEditor.waitFor({ state: "detached", timeout: 10_000 });

    // --- 7 + 3. A LaTeX project with an invalid project.json; the engine switch.
    await invoke(page, "workspace_open", { path: thesis });
    await page.locator('[data-testid="file-tree"] [data-path="main.tex"]').waitFor({ timeout: 10_000 });
    await openFromTree(page, "main.tex");
    await modeButton("Page").waitFor({ timeout: 10_000 });
    await modeButton("Page").click();
    const trustDialog = page.getByRole("alertdialog", { name: "Trust this workspace?" });
    await trustDialog.waitFor({ timeout: 15_000 });
    await trustDialog.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await trustDialog.waitFor({ state: "detached" });
    // The page view's build read project.json: set aside, and the user is told.
    const projectNotice = notification(page, "Invalid project settings were reset");
    await projectNotice.waitFor({ timeout: 20_000 });
    const keptProject = (await readdir(join(thesis, ".exposition"))).filter((n) => n.startsWith("project.json.invalid-"));
    assert.equal(keptProject.length, 1);
    assert.equal(await readFile(join(thesis, ".exposition", keptProject[0]), "utf8"), badProject);
    assert.ok((await projectNotice.innerText()).includes(keptProject[0]), "names the kept project.json");
    evidence.projectWarning = await projectNotice.innerText();

    const lastGood = async (engine) =>
      (await invoke(page, "tex_status", { target: { format: "latex", engine, entry: "main.tex" } }))?.lastGood?.result ?? null;
    /** The page view shows a successful build by `engine` (its compiler). */
    const shownBy = async (engine, compiler) =>
      waitFor(
        `page view built by ${engine}`,
        async () => {
          const good = await lastGood(engine);
          const shown = await pageEditor.getAttribute("data-build");
          return good && good.configuration?.compiler.includes(compiler) && shown === good.id ? good : null;
        },
        120_000,
      );
    const pdf = await shownBy("pdfLatex", "pdfTeX");
    evidence.pdfBuild = { id: pdf.id, compiler: pdf.configuration.compiler };

    // The palette switches the active file to LuaLaTeX; the open view rebuilds.
    await runCommand(page, "Build This File with LuaLaTeX", /LaTeX: Build This File with LuaLaTeX/);
    await notification(page, "main.tex now builds with LuaLaTeX").waitFor({ timeout: 10_000 });
    const lua = await shownBy("luaLatex", "LuaHBTeX");
    evidence.luaBuild = { id: lua.id, compiler: lua.configuration.compiler };
    const saved = JSON.parse(await readFile(join(thesis, ".exposition", "project.json"), "utf8"));
    assert.deepEqual(saved.targets, [{ name: "main", format: "latex", engine: "luaLatex", entry: "main.tex" }]);

    // Settings > Project lists the target; switch back to pdfLaTeX in the table.
    await runCommand(page, "Project Settings and LaTeX Targets", /Preferences: Project Settings and LaTeX Targets/);
    const targets = page.getByTestId("latex-targets");
    const row = targets.getByTestId("latex-target").filter({ hasText: "main" });
    await row.waitFor();
    assert.equal(await row.getAttribute("data-engine"), "luaLatex");
    await bothThemes(page, "settings-project");
    await targets.getByRole("button", { name: "Add target…" }).first().click();
    const addDialog = page.getByRole("dialog", { name: "Add LaTeX target" });
    await addDialog.waitFor();
    await bothThemes(page, "settings-project-add-target");
    await addDialog.getByRole("button", { name: "Cancel" }).click();
    await addDialog.waitFor({ state: "detached" });
    await row.getByRole("button", { name: /Engine of main/ }).click();
    await page.getByRole("option", { name: "pdfLaTeX" }).click();
    await waitFor("row shows pdfLaTeX", async () => (await row.getAttribute("data-engine")) === "pdfLatex");
    // Back to the page view: rebuilt by pdfLaTeX.
    await page.locator('[data-editor-key="file:main.tex"]').first().click();
    await pageEditor.waitFor();
    const back = await waitFor(
      "a newer pdfLaTeX build shown",
      async () => {
        const good = await lastGood("pdfLatex");
        return good && good.id !== pdf.id && (await pageEditor.getAttribute("data-build")) === good.id ? good : null;
      },
      120_000,
    );
    evidence.backToPdf = { id: back.id, compiler: back.configuration.compiler };

    // --- 7. Remove from Recent (Files view, no folder open).
    await runCommand(page, "Close Folder", /File: Close Folder/);
    const recent = page.getByTestId("recent-project");
    await waitFor("both folders listed", async () => (await recent.count()) === 2);
    await bothThemes(page, "recent-projects");
    await page.getByRole("button", { name: "Remove thesis from Recent" }).click();
    await waitFor("thesis removed", async () => (await recent.count()) === 1);
    const listed = await invoke(page, "workspace_recent");
    assert.deepEqual(listed.map((p) => p.name), ["paper"], "the service forgot it");
    assert.ok(existsSync(thesis), "the folder itself is untouched");
    // Open paper again from Recent: it is the last project at exit.
    await page.getByRole("button", { name: /^Open paper/ }).click();
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 10_000 });
    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await closeGracefully(app);
  }

  // ---------------- Second start: the last project reopens ----------------
  app = await launchApp({ dataDir, env });
  try {
    const { page } = app;
    await page.setViewportSize(VIEWPORT);
    const files = page.getByTestId("files-view");
    await files.waitFor({ timeout: 20_000 });
    assert.ok((await files.getAttribute("data-root")).endsWith("paper"), "reopened the last project");
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 10_000 });
    await page.waitForTimeout(1500);
    for (const title of ["Invalid application settings were reset", "Invalid execution settings were reset"]) {
      assert.equal(await notification(page, title).count(), 0, `${title}: told once, not again`);
    }
    assert.deepEqual(consoleErrors(app), [], "no console errors after reopening");
  } finally {
    await closeGracefully(app);
  }

  // ---------------- Third start: the last project is gone ----------------
  await rename(paper, `${paper}-moved`);
  app = await launchApp({ dataDir, env });
  try {
    const { page } = app;
    await page.setViewportSize(VIEWPORT);
    const notice = notification(page, "Could not reopen paper");
    await notice.waitFor({ timeout: 20_000 });
    evidence.reopenNotice = await notice.innerText();
    await page.getByTestId("files-empty").waitFor();
    await bothThemes(page, "reopen-failed", { keepNotifications: true });
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2));
    await app.close();
  }
  console.log(`      screenshots: ${shots.length} in ${artifactsDir}`);
}
