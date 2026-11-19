// Theme switching preserves work (UI standard V2 §5 and §11). With an unsaved
// edit open in the source editor and the Graph page mounted, switch through
// Light, Dark, Ocean, Sage, and Follow system with the real commands, then:
//  - the source editor was not remounted (a marker set on its DOM survives),
//    its text, dirty marker, selection, and Monaco's alternative version id
//    (any edit or undo entry changes it) are unchanged, and Undo still
//    removes the typed text;
//  - the document service's revision and text are unchanged;
//  - the project graph's revision and counts are unchanged and the canvas
//    node positions did not move;
//  - Monaco followed each theme (editor background = that theme's surface);
//  - the chosen theme persists across a relaunch (Ocean).
// Screenshots: test-results/e2e/v2-theme-preservation only.
import { execFileSync } from "node:child_process";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { applyTheme, THEMES } from "../theme-helpers.mjs";
import { documentState, editorLocator, focusEditor, invoke, makeProject, openInEditor, openWorkspace, tabDirty, waitFor, waitForMirror } from "../workspace-helpers.mjs";

// The editor surface per theme (tokens.ts `palettes[theme].surface`).
const SURFACE = { light: "rgb(255, 255, 255)", dark: "rgb(29, 30, 32)", ocean: "rgb(24, 31, 40)", sage: "rgb(251, 251, 251)" };
// No leading space: Monaco may drop a typed leading space on an empty last line.
const TYPED = "theme-preservation-check";

/** Monaco editor facts for `path`, through the SourceEditor's React state. */
function editorFacts(page, path) {
  return page.evaluate((p) => {
    const el = [...document.querySelectorAll('[data-testid="source-editor"]')].find((e) => e.dataset.path === p);
    if (!el) return null;
    const key = Object.keys(el).find((k) => k.startsWith("__reactFiber$"));
    let fiber = key ? el[key] : null;
    while (fiber) {
      let hook = fiber.memoizedState;
      while (hook && typeof hook === "object" && "next" in hook) {
        const editor = hook.memoizedState?.current;
        if (editor && typeof editor.getSelection === "function" && editor.getModel()) {
          const model = editor.getModel();
          const s = editor.getSelection();
          return {
            text: model.getValue(),
            version: model.getAlternativeVersionId(),
            selection: [s.startLineNumber, s.startColumn, s.endLineNumber, s.endColumn],
            marked: el.querySelector(".monaco-editor")?.dataset.v2Mark === "kept",
            background: getComputedStyle(el.querySelector(".monaco-editor .monaco-editor-background") ?? el).backgroundColor,
          };
        }
        hook = hook.next;
      }
      fiber = fiber.return;
    }
    return null;
  }, path);
}

function nodePositions(page) {
  return page.evaluate(() =>
    Object.fromEntries([...document.querySelectorAll(".react-flow__node")].map((n) => [n.dataset.id, n.style.transform])),
  );
}

/** Close like a user (WM_CLOSE) so WebView2 flushes localStorage (theme). */
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

export default async function v2ThemePreservation({ launchApp, artifactsDir, assert, fixture }) {
  const project = await makeProject(fixture);
  let app = await launchApp({ dataDir: project.dataDir });
  try {
    let { page } = app;
    await openWorkspace(page, project.root);

    // Graph page mounted once, so its canvas exists while themes change.
    await page.locator('[data-page-button="graph"]').click();
    await page.locator('[data-page="graph"]').waitFor({ timeout: 15_000 });
    await waitFor(async () => (await invoke(page, "graph_status")).revision > 0, { timeout: 30_000, message: "graph built" });
    await page.locator(".react-flow__node").first().waitFor({ timeout: 15_000 }).catch(() => {});
    await page.locator('[data-page-button="editor"]').click();

    // Unsaved edit in main.typ.
    const id = await openInEditor(page, "main.typ");
    const saved = (await documentState(page, id)).text;
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.type(TYPED);
    await waitForMirror(page, "main.typ", id);
    await page.evaluate(() => {
      const el = document.querySelector('[data-testid="source-editor"][data-path="main.typ"] .monaco-editor');
      if (el) el.dataset.v2Mark = "kept";
    });
    const before = await editorFacts(page, "main.typ");
    assert.ok(before?.marked, "marker set on the editor");
    assert.equal(before.text, saved + TYPED, "typed text is in the editor");
    assert.equal(await tabDirty(page, "main.typ"), "true", "the tab shows unsaved changes");
    const docBefore = await documentState(page, id);
    const graphBefore = await invoke(page, "graph_status");

    for (const [theme] of THEMES) {
      await applyTheme(page, theme);
      const facts = await editorFacts(page, "main.typ");
      assert.ok(facts?.marked, `${theme}: the editor was not remounted`);
      assert.equal(facts.text, before.text, `${theme}: text unchanged`);
      assert.equal(facts.version, before.version, `${theme}: no edit or undo entry added`);
      assert.deepEqual(facts.selection, before.selection, `${theme}: selection unchanged`);
      assert.equal(facts.background, SURFACE[theme], `${theme}: Monaco follows the theme`);
      assert.equal(await tabDirty(page, "main.typ"), "true", `${theme}: still unsaved`);
      await page.screenshot({ path: join(artifactsDir, `editor-${theme}.png`) });
    }
    await runCommand(page, "Color Theme: Follow System", /Color Theme: Follow System/);
    assert.ok((await editorFacts(page, "main.typ"))?.marked, "system: the editor was not remounted");

    // Graph page: positions and graph state unchanged across the switches.
    await page.locator('[data-page-button="graph"]').click();
    await page.locator('[data-page="graph"]').waitFor();
    const positionsBefore = await nodePositions(page);
    for (const [theme] of THEMES) await applyTheme(page, theme);
    await page.waitForTimeout(300);
    assert.deepEqual(await nodePositions(page), positionsBefore, "graph node positions unchanged");
    const graphAfter = await invoke(page, "graph_status");
    for (const field of ["revision", "entities", "relations"]) {
      assert.equal(graphAfter[field], graphBefore[field], `graph ${field} unchanged by theme switches`);
    }
    await page.screenshot({ path: join(artifactsDir, "graph-sage.png") });
    await page.locator('[data-page-button="editor"]').click();

    // The document service saw no change either; one Undo removes the typed text.
    const docAfter = await documentState(page, id);
    assert.equal(docAfter.info.revision, docBefore.info.revision, "document revision unchanged");
    assert.equal(docAfter.text, docBefore.text, "document text unchanged");
    // The undo history still holds the edit (Monaco may split typing into a few stops).
    await focusEditor(page, "main.typ", "end");
    for (let i = 0; i < 6 && (await editorFacts(page, "main.typ"))?.text !== saved; i++) {
      await page.keyboard.press("Control+Z");
      await page.waitForTimeout(100);
    }
    assert.equal((await editorFacts(page, "main.typ"))?.text, saved, "Undo removes the typed text after the theme switches");

    // Persistence across a relaunch.
    await applyTheme(page, "ocean");
    await page.waitForTimeout(300);
    await closeGracefully(app);
    app = await launchApp({ dataDir: project.dataDir });
    page = app.page;
    await page.waitForFunction(() => document.documentElement.dataset.theme === "ocean", null, { timeout: 15_000 });
    assert.equal(await page.evaluate(() => localStorage.getItem("exposition.theme")), JSON.stringify("ocean"), "Ocean persisted");
  } finally {
    await app.close();
  }
}
