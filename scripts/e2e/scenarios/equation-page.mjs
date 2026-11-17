// Visual equation editor from the exact-page view, in real documents of every
// supported language (LaTeX/pdfLaTeX, Typst, Markdown print profile), Page mode:
//  1. Click in text on a page, Ctrl+Shift+M: a new equation at that source
//     position (resolved in the current revision), applied as one transaction;
//     the pages show it.
//  2. With no draft open, a symbol chosen in the panel goes in at the page
//     caret directly (a new inline equation holding it).
//  3. Click inside an existing equation and use the page toolbar's Edit
//     Equation: the whole equation opens in the panel; only its content
//     changes (label and reference kept).
//  4. Ctrl+Z / Ctrl+Y on the page undo and redo the equation edit (shared
//     history with the source).
//  5. A cases block created from the page compiles.
//  6. An external edit to the equation during a draft: conflict, nothing written.
//  7. Save from the page; close and reopen: the document and pages show the edits.
// Screenshots go to test-results/e2e/equation-page (reference shots in both
// themes also to $EXPOSITION_DEV_HOME/ui-reference). Requires Typst and TeX.
import { copyFile, mkdir, readFile } from "node:fs/promises";
import { basename, join } from "node:path";

import { closeTab } from "../workspace-helpers.mjs";
import { runCommand } from "../palette.mjs";
import { equationSession, LANGUAGES, makeEquationProject, openDocumentIn, PATTERNS, squash } from "../equation-helpers.mjs";

export default async function equationPage({ launchApp, artifactsDir, assert, fixture }) {
  const app = await launchApp();
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const references = [];
  const referenceShots = async (name) => {
    for (const theme of ["dark", "light"]) {
      await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
      await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
      await page.waitForTimeout(150);
      const file = join(artifactsDir, `${name}-${theme}.png`);
      await page.screenshot({ path: file });
      references.push(file);
    }
  };

  async function run(language) {
    const { path } = LANGUAGES[language];
    const root = await makeEquationProject(fixture, language);
    await page.evaluate((p) => window.__TAURI_INTERNALS__.invoke("workspace_open", { path: p }), root);
    await openDocumentIn(page, path, "Page");
    const s = equationSession(page, path);
    await s.settled("opened");
    await s.showPanel();
    const pageInput = page.locator('[data-testid="page-input"]');

    // 1. A new equation at a page caret in text.
    let source = await s.text();
    const inlineAt = source.indexOf("InlineTarget.") + "InlineTarget.".length;
    await s.clickPageAt(inlineAt, { affinity: "upstream", dx: -0.1 });
    let before = await s.revision();
    await page.keyboard.press("Control+Shift+M");
    await s.draft("insert");
    assert.match(await page.getByTestId("equation-target").innerText(), /from the page/);
    await s.template("Fraction");
    await s.mathType("1");
    await page.keyboard.press("Tab");
    await s.template("Square root");
    await s.mathType("x^2");
    await page.keyboard.press("Enter");
    const withFraction = await s.waitText((t) => PATTERNS.nestedFraction[language].test(squash(t)), "fraction written from the page");
    assert.equal(await s.revision(), before + 1, "one transaction");
    assert.ok(withFraction.startsWith(source.slice(0, inlineAt)) && withFraction.endsWith(source.slice(inlineAt)), "inserted exactly at the page caret");
    const insertedLength = withFraction.length - source.length;
    await s.settled("page insert → new pages");
    assert.ok((await s.rendered(inlineAt, inlineAt + insertedLength)).trim().length > 0, "the pages show the new equation");
    await shot(`${language}-01-page-insert`);
    if (language === "typst") await referenceShots("equation-page-insert");

    // 2. A single symbol at the page caret, without a draft.
    source = await s.text();
    const symbolAt = source.indexOf("SymbolTarget.") + "SymbolTarget.".length;
    await s.clickPageAt(symbolAt, { affinity: "upstream", dx: -0.1 });
    before = await s.revision();
    const name = await s.search("alpha");
    assert.match(name ?? "", /alpha/i);
    await page.locator('[data-testid="equation-symbol-grid"] [role="option"]').first().click();
    const withSymbol = await s.waitText((t) => PATTERNS.alpha[language].test(t.slice(symbolAt, symbolAt + 20)), "symbol inserted at the page caret");
    assert.equal(await s.revision(), before + 1);
    assert.ok(withSymbol.startsWith(source.slice(0, symbolAt)) && withSymbol.endsWith(source.slice(symbolAt)));
    assert.equal(await s.view.getAttribute("data-state"), "idle", "no draft was needed");
    await s.settled("symbol → new pages");

    // 3. Select an existing equation on the page and edit it in the panel.
    source = await s.text();
    const energy = source.indexOf("m c^2");
    await s.clickPageAt(energy, { dx: 1, mode: "Math" });
    before = await s.revision();
    await page.getByTestId("page-equation").click();
    await s.draft("edit");
    assert.match(await page.getByTestId("equation-target").innerText(), /Editing equation at line \d+/);
    await page.keyboard.press("End");
    await s.mathType("+1");
    await page.keyboard.press("Enter");
    const edited = await s.waitText((t) => /E=mc\^\{?2\}?\+1/.test(squash(t)), "energy equation edited from the page");
    assert.equal(await s.revision(), before + 1, "one transaction");
    if (language === "latex") assert.ok(edited.includes("\\label{eq:energy}") && edited.includes("\\eqref{eq:energy}"));
    if (language === "typst") assert.ok(edited.includes("$ <energy>") && edited.includes("@energy"));
    await s.settled("page edit → new pages");
    await shot(`${language}-02-page-edit`);

    // 4. Undo and redo on the page (the shared document history).
    await pageInput.focus();
    await page.keyboard.press("Control+z");
    await s.waitText((t) => t === source, "page undo reverted the equation edit");
    await page.keyboard.press("Control+y");
    await s.waitText((t) => t === edited, "page redo restored it");
    await s.settled("after page undo/redo");

    // 5. A cases block from the page compiles.
    source = await s.text();
    const displayAt = source.indexOf("DisplayTarget.") + "DisplayTarget.".length;
    await s.clickPageAt(displayAt, { affinity: "upstream", dx: -0.1 });
    await page.keyboard.press("Control+Shift+M");
    await s.draft("insert");
    await page.getByRole("radio", { name: "Display" }).click();
    await s.template("Cases");
    await s.mathType("1");
    await page.keyboard.press("Tab");
    await s.mathType("x");
    await page.keyboard.press("Tab");
    await s.mathType("0");
    await page.keyboard.press("Tab");
    await s.mathType("y");
    await page.keyboard.press("Control+Enter");
    await s.waitText((t) => PATTERNS.cases[language].test(t), "cases written");
    await s.settled("cases → new pages");
    await shot(`${language}-03-cases`);

    // 6. An external edit during a draft opened from the page: conflict, nothing written.
    source = await s.text();
    const sum = source.indexOf("$a + b$") + 1;
    await s.clickPageAt(sum, { dx: 0.5, mode: "Math" });
    await page.getByTestId("page-equation").click();
    await s.draft("edit");
    await s.mathType("q");
    await s.invoke("document_apply", {
      request: {
        id: `e2e-eq-page-external-${language}`,
        origin: { kind: "external" },
        edits: [{ document: await s.docId(), baseRevision: await s.revision(), patches: [{ range: { start: sum + 1, end: sum + 1 }, text: "c" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await page.locator('[data-testid="equation-view"][data-conflict="changed"]').waitFor({ timeout: 10000 });
    assert.ok(await page.getByTestId("equation-apply").isDisabled());
    const afterExternal = await s.text();
    await page.getByTestId("equation-cancel").click();
    await page.waitForTimeout(300);
    assert.equal(await s.text(), afterExternal, "the conflicted draft wrote nothing");
    await s.settled("after the external edit");

    // 7. Save from the page, close, reopen.
    const final = await s.text();
    await pageInput.focus();
    await page.keyboard.press("Control+s");
    const deadline = Date.now() + 10000;
    while ((await readFile(join(root, path), "utf8")) !== final) {
      if (Date.now() > deadline) throw new Error("the saved file does not match the document");
      await new Promise((r) => setTimeout(r, 100));
    }
    await closeTab(page, path);
    await openDocumentIn(page, path, "Page", { trustPrompt: false });
    assert.equal(await s.text(), final, "reopened document keeps every equation edit");
    await s.settled("reopened");
    await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("workspace_close"));
  }

  try {
    await run("latex");
    await run("typst");
    await run("markdown");
    if (process.env.EXPOSITION_DEV_HOME) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference");
      await mkdir(dir, { recursive: true });
      for (const file of references) await copyFile(file, join(dir, basename(file)));
    }
    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    assert.deepEqual(errors, [], "no console errors");
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await app.close();
  }
}
