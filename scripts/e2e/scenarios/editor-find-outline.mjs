// Editor Find and Outline in the real app (QOL C + D), through visible
// controls only:
// - Find in a dirty Typst buffer with Ctrl+F (Monaco's Find widget): match
//   count includes unsaved text, next/previous, Escape returns focus to the
//   editor, the text/revision/dirty state are unchanged; palette "Edit: Find";
//   Ctrl+F on the Files tree does not open it.
// - Page view: Ctrl+F and the toolbar's "Find in Source" switch to Split, focus
//   the source, and open Find there (rendered page text is not searched).
// - Outline (auxiliary panel tab): rows in source order for .typ/.tex/.md with
//   nesting, unsaved-buffer label, coverage (partial for an environment
//   defined elsewhere); clicking selects the exact source range; editing and
//   undoing a heading updates the rows; switching documents follows the
//   active tab; compiled pages listed from the identified build, kept and
//   labelled "Showing pages from an earlier revision" after a failed build.
// - Graph entity/relation counts are unchanged by Find and Outline use.
// Screenshots: test-results/e2e/editor-find-outline (and, with
// EXPOSITION_DEV_HOME, $EXPOSITION_DEV_HOME/ui-reference/editor/).
import { copyFile, cp, mkdir, mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

import { runCommand } from "../palette.mjs";
import { waitForAsync } from "../wait.mjs";
import { editorLocator, openFromTree, openWorkspace, tabLocator, waitFor } from "../workspace-helpers.mjs";

/** Selection of the Monaco editor inside the source editor of `path` (UTF-16 offsets). */
function monacoSelection(page, path) {
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
            start: model.getOffsetAt(s.getStartPosition()),
            end: model.getOffsetAt(s.getEndPosition()),
            focused: editor.hasTextFocus(),
          };
        }
        hook = hook.next;
      }
      fiber = fiber.return;
    }
    return null;
  }, path);
}

export default async function editorFindOutline({ launchApp, artifactsDir, assert, fixture }) {
  const root = join(await mkdtemp(join(tmpdir(), "exposition-outline-")), "project");
  await cp(fixture("outline-basic"), root, { recursive: true });
  const app = await launchApp();
  const { page } = app;
  const references = [];
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const referenceShots = async (name) => {
    for (const theme of ["dark", "light"]) {
      await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
      await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
      await page.waitForTimeout(200);
      const file = join(artifactsDir, `${name}-${theme}.png`);
      await page.screenshot({ path: file });
      references.push(file);
    }
  };
  const invoke = (cmd, args = {}) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
  const docState = async (path) => {
    const doc = (await invoke("document_list")).find((d) => d.path === path);
    return invoke("document_state", { document: doc.id });
  };
  const findWidget = page.locator('[data-testid="source-editor"][data-path="main.typ"] .find-widget.visible');
  const matches = page.locator('[data-testid="source-editor"][data-path="main.typ"] .find-widget .matchesCount');
  const outline = page.locator('[data-testid="outline-view"]');
  const rows = async () =>
    page.$$eval('[data-testid="outline-tree"] [role="row"]', (els) =>
      els.map((e) => `${"  ".repeat(Number(e.getAttribute("aria-level") ?? 1) - 1)}${e.querySelector(".ui-tree-item__title")?.textContent ?? ""}`),
    );
  const row = (title) => page.locator('[data-testid="outline-tree"] [role="row"]', { has: page.locator(".ui-tree-item__title", { hasText: title }) }).first();
  const graphCounts = async () => {
    const s = await invoke("graph_status");
    return { entities: s.entities, relations: s.relations, state: s.freshness?.state };
  };
  const notes = [];
  /** Ctrl+Z (shared document history) until the text is `expected` again. */
  const undoUntil = async (path, expected) => {
    for (let i = 0; i < 20 && (await docState(path)).text !== expected; i++) {
      await page.keyboard.press("Control+Z");
      await page.waitForTimeout(150);
    }
    assert.equal((await docState(path)).text, expected, `undo restored ${path}`);
  };

  try {
    await openWorkspace(page, root);
    // Baseline graph state once the opened project is reconciled.
    const baseline = await waitFor(async () => {
      const c = await graphCounts();
      return c.state === "synchronized" && c.entities > 0 ? c : null;
    }, { timeout: 30_000, message: "graph synchronized" });

    // ---- C. Find in a dirty buffer -------------------------------------------
    await openFromTree(page, "main.typ");
    const editor = editorLocator(page, "main.typ");
    await editor.locator(".monaco-editor").waitFor();
    await editor.locator(".monaco-editor .view-lines").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("\nDirty line alpha.");
    await waitFor(async () => (await tabLocator(page, "main.typ").getAttribute("data-dirty")) === "true", { message: "main.typ dirty" });
    await waitFor(async () => (await docState("main.typ")).text.includes("Dirty line alpha."), { message: "edit applied" });
    const before = await docState("main.typ");
    const alphas = before.text.split("alpha").length - 1;
    assert.equal(alphas, 6, "fixture has five occurrences plus the unsaved one");

    await page.keyboard.press("Control+F");
    await findWidget.waitFor({ timeout: 5000 });
    await page.keyboard.type("alpha");
    await waitFor(async () => /of 6/.test((await matches.textContent()) ?? ""), { message: "6 matches incl. unsaved text" });
    const firstCount = await matches.textContent();
    await page.keyboard.press("Enter");
    await waitFor(async () => (await matches.textContent()) !== firstCount, { message: "next match" });
    const nextCount = await matches.textContent();
    await page.keyboard.press("Shift+Enter");
    await waitFor(async () => (await matches.textContent()) === firstCount, { message: "previous match" });
    notes.push(`find counts: ${firstCount} -> ${nextCount} -> ${firstCount}`);
    await referenceShots("find-dirty-buffer");
    await page.keyboard.press("Escape");
    await findWidget.waitFor({ state: "hidden", timeout: 5000 });
    assert.ok(await page.evaluate(() => !!document.activeElement?.closest('[data-testid="source-editor"] .monaco-editor')), "Escape returns focus to the editor");
    const after = await docState("main.typ");
    assert.equal(after.text, before.text, "Find did not change the text");
    assert.equal(after.info.revision, before.info.revision, "Find did not create a revision");
    assert.equal(await tabLocator(page, "main.typ").getAttribute("data-dirty"), "true", "still unsaved, not saved by Find");

    // Ctrl+F on the Files tree is not Find (the Files view keeps its keys).
    await page.locator('[data-testid="file-tree"] [data-path="notes.md"]').first().focus();
    assert.ok(await page.evaluate(() => !!document.activeElement?.closest('[data-testid="file-tree"]')), "a Files row has focus");
    await page.keyboard.press("Control+F");
    await page.waitForTimeout(300);
    assert.equal(await findWidget.count(), 0, "Ctrl+F in the Files view does not open the editor's Find");
    assert.ok(await page.evaluate(() => !!document.activeElement?.closest('[data-testid="file-tree"]')), "focus stays in the Files view");
    // The palette reaches Find from anywhere.
    await runCommand(page, "Edit: Find", /^Edit: Find/);
    await findWidget.waitFor({ timeout: 5000 });
    await page.keyboard.press("Escape");
    await findWidget.waitFor({ state: "hidden" });

    // ---- Page view: Find in Source ---------------------------------------------
    const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
    await modeButton("Page").click();
    const pageEditor = page.locator('[data-testid="page-editor"]');
    await pageEditor.waitFor();
    await waitForAsync(page, () => document.querySelectorAll('[data-testid="page"] img').length >= 2, null, { timeout: 30_000, message: "two pages built" });
    await page.locator('[data-testid="page-input"]').focus();
    await page.keyboard.press("Control+F");
    await page.locator('[data-testid="typst-split"]').waitFor({ timeout: 5000 });
    await findWidget.waitFor({ timeout: 5000 });
    assert.ok(await page.evaluate(() => !!document.activeElement?.closest(".find-widget")), "Find has focus in the source beside the pages");
    await shot("find-in-source-split");
    await referenceShots("find-in-source-split");
    await page.keyboard.press("Escape");
    await findWidget.waitFor({ state: "hidden" });
    assert.ok(await page.evaluate(() => !!document.activeElement?.closest('[data-testid="source-editor"] .monaco-editor')), "Escape returns to the source");
    // The toolbar route from the page-only view.
    await modeButton("Page").click();
    await pageEditor.waitFor();
    const findButton = page.getByRole("button", { name: "Find in Source" });
    await findButton.waitFor();
    await findButton.click();
    await page.locator('[data-testid="typst-split"]').waitFor({ timeout: 5000 });
    await findWidget.waitFor({ timeout: 5000 });
    await page.keyboard.press("Escape");
    assert.equal((await docState("main.typ")).text, before.text, "page routes changed nothing");

    // ---- D. Outline ----------------------------------------------------------
    // The Outline tab of the auxiliary panel (beside Lean and Dependencies).
    await page.getByRole("tablist", { name: "Auxiliary panel views" }).getByRole("tab", { name: "Outline" }).click();
    await outline.waitFor();
    await waitFor(async () => (await rows()).length >= 5, { message: "outline rows" });
    const typRows = await rows();
    assert.deepEqual(typRows, [
      "Introduction",
      "  Theorem (Main result)",
      "  Proof",
      "  Background",
      "    Lemma",
      "Results",
    ]);
    await page.locator('[data-testid="outline-unsaved"]').waitFor();
    assert.equal(await outline.getAttribute("data-coverage"), "complete");
    // Click a theorem: its first line is selected in the source.
    await row("Theorem (Main result)").click();
    const text = (await docState("main.typ")).text;
    const thmAt = text.indexOf('#theorem("Main result")');
    await waitFor(async () => (await monacoSelection(page, "main.typ"))?.start === thmAt, { message: "theorem selected" });
    const thmSel = await monacoSelection(page, "main.typ");
    // The construct's first line (the call; its label follows the construct).
    assert.equal(text.slice(thmSel.start, thmSel.end), '#theorem("Main result")[Every finite group is a group.]', "the theorem is selected");
    // The pages of the identified build.
    const pagesSection = page.locator('[data-testid="outline-pages"]');
    await waitFor(async () => (await pagesSection.getAttribute("data-state")) === "current", { timeout: 30_000, message: "pages current" });
    assert.equal(await page.locator('[data-testid="outline-page-list"] [role="row"]').count(), 2);
    await page.locator('[data-testid="outline-page-list"] [role="row"]', { hasText: "Page 2" }).click();
    await waitForAsync(page, () => {
      const scroller = document.querySelector('[data-testid="page-scroller"]');
      const p2 = document.querySelector('[data-testid="page"][data-page="1"]');
      if (!scroller || !p2) return false;
      const s = scroller.getBoundingClientRect();
      const r = p2.getBoundingClientRect();
      return scroller.scrollTop > 0 && Math.abs(r.top - s.top) < 40;
    }, null, { message: "page 2 scrolled into view" });
    await shot("outline-typst");
    await referenceShots("outline-typst");

    // Edit a heading (select it from the outline, type over it), then undo.
    await row("Results").click();
    const resultsAt = text.indexOf("= Results");
    await waitFor(async () => (await monacoSelection(page, "main.typ"))?.start === resultsAt, { message: "heading selected" });
    await page.keyboard.type("= Findings");
    await waitFor(async () => (await rows()).includes("Findings") && !(await rows()).includes("Results"), { message: "outline follows the edit" });
    await undoUntil("main.typ", text);
    await waitFor(async () => (await rows()).includes("Results") && !(await rows()).includes("Findings"), { message: "outline follows the undo" });

    // A failed build keeps the earlier pages, labelled. First the build of
    // the current (undone) revision.
    await waitFor(
      async () =>
        (await pagesSection.getAttribute("data-state")) === "current" &&
        (await pagesSection.getAttribute("data-build-revision")) === String((await docState("main.typ")).info.revision),
      { timeout: 30_000, message: "pages of the current revision" },
    );
    const goodBuild = await pagesSection.getAttribute("data-build");
    await editor.locator(".monaco-editor .view-lines").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("\n#undefined-function()");
    await waitFor(async () => (await pagesSection.getAttribute("data-state")) === "failed", { timeout: 30_000, message: "failed build keeps pages" });
    // The listed pages are those of the last successful build (an earlier
    // revision; typing may have produced a newer good build than `goodBuild`
    // before the error was complete), the same build the page view shows.
    const listedBuild = await pagesSection.getAttribute("data-build");
    const listedRevision = Number(await pagesSection.getAttribute("data-build-revision"));
    assert.ok(listedBuild && listedRevision < (await docState("main.typ")).info.revision, "pages from an earlier, successful build");
    assert.equal(listedBuild, await pageEditor.getAttribute("data-build"), "the page view shows the same build");
    notes.push(`failed build: listed ${listedBuild} (revision ${listedRevision}); good build before typing ${goodBuild}`);
    assert.match(await pagesSection.textContent(), /Showing pages from an earlier revision/);
    assert.equal(await page.locator('[data-testid="outline-page-list"] [role="row"]').count(), 2);
    await shot("outline-failed-build");
    await referenceShots("outline-failed-build");
    await undoUntil("main.typ", text);
    await waitFor(async () => (await pagesSection.getAttribute("data-state")) === "current", { timeout: 30_000, message: "pages current again" });

    // ---- Switch documents: LaTeX (partial coverage) and Markdown -------------
    await openFromTree(page, "paper.tex");
    await editorLocator(page, "paper.tex").locator(".monaco-editor").waitFor();
    await waitFor(async () => (await outline.getAttribute("data-path")) === "paper.tex" && (await rows()).length >= 6, { message: "paper.tex outline" });
    assert.deepEqual(await rows(), [
      "Introduction",
      "  Definition (Group)",
      "  Main statements",
      "    Theorem (Main result)",
      "    Proof",
      "Further results",
      "  Lemma",
    ]);
    assert.equal(await outline.getAttribute("data-coverage"), "partial", "an environment defined elsewhere is reported");
    assert.match(await page.locator('[data-testid="outline-coverage"]').textContent(), /Partial.*mytheorem/);
    assert.equal(await page.locator('[data-testid="outline-unsaved"]').count(), 0, "a saved file is not labelled unsaved");
    await row("Theorem (Main result)").click();
    const tex = (await docState("paper.tex")).text;
    await waitFor(async () => (await monacoSelection(page, "paper.tex"))?.start === tex.indexOf("\\begin{theorem}[Main result]"), { message: "LaTeX theorem selected" });
    await shot("outline-latex-partial");
    await referenceShots("outline-latex-partial");

    await openFromTree(page, "notes.md");
    await editorLocator(page, "notes.md").locator(".monaco-editor").waitFor();
    await waitFor(async () => (await outline.getAttribute("data-path")) === "notes.md" && (await rows()).length >= 5, { message: "notes.md outline" });
    assert.deepEqual(await rows(), ["Notes", "  Definitions", "    Definition (Ring)", "  Open questions", "    Conjecture"]);
    await row("Conjecture").click();
    const md = (await docState("notes.md")).text;
    await waitFor(async () => (await monacoSelection(page, "notes.md"))?.start === md.indexOf("::: {.conjecture"), { message: "Markdown conjecture selected" });

    // Back to the Typst tab: its outline again (never another document's).
    await tabLocator(page, "main.typ").click();
    await waitFor(async () => (await outline.getAttribute("data-path")) === "main.typ" && (await rows())[0] === "Introduction", { message: "main.typ outline again" });

    // ---- Graph unchanged by Find/Outline use ----------------------------------
    const afterCounts = await graphCounts();
    assert.equal(afterCounts.entities, baseline.entities, "no graph entities added by outline/find use");
    assert.equal(afterCounts.relations, baseline.relations, "no graph relations added by outline/find use");
    notes.push(`graph: ${JSON.stringify(baseline)} -> ${JSON.stringify(afterCounts)}`);

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    assert.deepEqual(errors, [], "no console errors");
    console.log(notes.join("\n"));
  } finally {
    if (process.env.EXPOSITION_DEV_HOME) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "editor");
      await mkdir(dir, { recursive: true });
      for (const file of references) await copyFile(file, join(dir, basename(file)));
    }
    await app.close();
  }
}
