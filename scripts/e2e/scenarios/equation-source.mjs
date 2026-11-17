// Visual equation editor from the source editor, in real documents of every
// supported language (LaTeX with pdfLaTeX, plus a LuaLaTeX build check; Typst;
// Markdown print profile), with the pages beside the source (Split):
//  1. Ctrl+Shift+M in text opens a new inline equation; a nested fraction with
//     a root and a superscript is built from templates and typing only; Enter
//     applies ONE transaction; the compiled pages update.
//  2. Undo/redo in Monaco revert and restore it; save writes it to disk;
//     closing and reopening the file shows it.
//  3. Editing an existing (labelled, referenced) equation changes only its
//     content: labels and references stay.
//  4. Applying an unchanged draft and cancelling a changed one write nothing.
//  5. An external edit to the equation during a draft is a conflict: nothing
//     is written, the draft stays.
//  6. Symbol search by name, synonym, glyph, and command finds the relation,
//     and Enter inserts it at the retained caret.
//  7. A display matrix with an added row compiles.
//  8. Unsupported source (a custom macro/function) stays byte-for-byte.
//  9. LaTeX: a symbol needing amssymb adds \usepackage{amssymb} in the same
//     transaction (one undo removes both).
// Screenshots go to test-results/e2e/equation-source (reference shots in both
// themes also to $EXPOSITION_DEV_HOME/ui-reference). Requires Typst, TeX
// (pdflatex, lualatex) as for page-typst/page-latex.
import { copyFile, mkdir, readFile } from "node:fs/promises";
import { basename, join } from "node:path";

import { closeTab } from "../workspace-helpers.mjs";
import { runCommand } from "../palette.mjs";
import { equationSession, LANGUAGES, makeEquationProject, openDocumentIn, PATTERNS, squash } from "../equation-helpers.mjs";

export default async function equationSource({ launchApp, artifactsDir, assert, fixture }) {
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

  async function run(language, { engine = "pdfLatex", short = false } = {}) {
    const tag = language === "latex" ? `${language}-${engine}` : language;
    const { path } = LANGUAGES[language];
    const root = await makeEquationProject(fixture("equations"), language, engine);
    await page.evaluate((p) => window.__TAURI_INTERNALS__.invoke("workspace_open", { path: p }), root);
    await openDocumentIn(page, path, "Split");
    const s = equationSession(page, path);
    await s.settled("opened");
    await s.showPanel();
    const original = await s.text();

    // 1. A new inline equation from the source caret: nested fraction, root, superscript.
    await s.monacoCaret("InlineTarget.");
    let before = await s.revision();
    await page.keyboard.press("Control+Shift+M");
    await s.draft("insert");
    assert.match(await page.getByTestId("equation-target").innerText(), /New inline equation at line \d+/);
    assert.match(await page.getByTestId("equation-target").innerText(), /from the source/);
    await s.template("Fraction");
    await s.mathType("1");
    await page.keyboard.press("Tab");
    await s.template("Square root");
    await s.mathType("x^2");
    await shot(`${tag}-01-nested-draft`);
    if (language === "latex" && engine === "pdfLatex") await referenceShots("equation-panel-draft");
    await page.keyboard.press("Enter");
    const withFraction = await s.waitText((t) => PATTERNS.nestedFraction[language].test(squash(t)), "nested fraction written");
    assert.equal(await s.revision(), before + 1, "one transaction");
    const at = original.indexOf("InlineTarget.") + "InlineTarget.".length;
    assert.ok(
      withFraction.startsWith(original.slice(0, at)) && withFraction.endsWith(original.slice(at)),
      "only an insertion at the caret; nothing else changed",
    );
    const inserted = withFraction.slice(at, withFraction.length - (original.length - at));
    assert.ok(!inserted.includes("\n"), `an inline construct: ${inserted}`);
    await s.settled("nested fraction → new pages");
    assert.ok((await s.rendered(at, at + inserted.length)).trim().length > 0, "the compiled page shows the new equation");
    await shot(`${tag}-02-nested-applied`);
    if (short) return page.evaluate(() => window.__TAURI_INTERNALS__.invoke("workspace_close"));

    // 2. Undo/redo from the source editor, save, reopen.
    await page.locator('[data-testid="source-editor"] .view-line', { hasText: "InlineTarget." }).first().click();
    await page.keyboard.press("Control+z");
    await s.waitText((t) => t === original, "Monaco undo removed the equation");
    await page.keyboard.press("Control+y");
    await s.waitText((t) => t === withFraction, "Monaco redo restored the equation");
    await page.keyboard.press("Control+s");
    const deadline = Date.now() + 10000;
    while ((await readFile(join(root, path), "utf8")) !== withFraction) {
      if (Date.now() > deadline) throw new Error("the saved file does not contain the equation");
      await new Promise((r) => setTimeout(r, 100));
    }
    await closeTab(page, path);
    await openDocumentIn(page, path, "Split", { trustPrompt: false });
    assert.equal(await s.text(), withFraction, "reopened document shows the equation");
    await s.settled("reopened");

    // 3. Edit the existing labelled equation: only its content changes.
    const energyLine = language === "typst" ? "$ E = m c^2 $" : "E = m c^2";
    await s.monacoCaret(energyLine, language === "typst" ? 3 : null);
    before = await s.revision();
    await page.keyboard.press("Control+Shift+M");
    await s.draft("edit");
    assert.match(await page.getByTestId("equation-target").innerText(), /Editing equation at line \d+/);
    await page.keyboard.press("End");
    await s.mathType("+1");
    await page.keyboard.press("Enter");
    const edited = await s.waitText((t) => PATTERNS.energyPlusOne.test(squash(t)), "energy equation edited");
    assert.equal(await s.revision(), before + 1, "one transaction");
    if (language === "latex") {
      assert.ok(edited.includes("\\label{eq:energy}") && edited.includes("\\eqref{eq:energy}") && edited.includes("\\ref{sec:eq}"));
      assert.ok(edited.includes("\\begin{equation}") && edited.includes("\\end{equation}"));
    } else if (language === "typst") {
      assert.ok(edited.includes("$ <energy>") && edited.includes("@energy"), "label and reference kept");
    }
    await s.settled("edited equation → new pages");
    await shot(`${tag}-03-edited`);

    // 4. Unchanged apply and cancel write nothing.
    before = await s.revision();
    await s.monacoCaret("Existing inline math", "Existing inline math $".length + 1);
    await page.keyboard.press("Control+Shift+M");
    await s.draft("edit");
    assert.equal(await s.view.getAttribute("data-unchanged"), "true");
    await page.keyboard.press("Enter");
    await page.locator('[data-testid="equation-view"][data-state="idle"]').waitFor();
    await s.monacoCaret("Existing inline math", "Existing inline math $".length + 1);
    await page.keyboard.press("Control+Shift+M");
    await s.draft("edit");
    await s.mathType("z");
    await page.keyboard.press("Escape");
    await page.locator('[data-testid="equation-view"][data-state="idle"]').waitFor();
    await page.waitForTimeout(300);
    assert.equal(await s.revision(), before, "unchanged apply and cancel wrote nothing");

    // 5. An external edit to the equation during a draft: conflict, nothing written.
    await s.monacoCaret("Existing inline math", "Existing inline math $".length + 1);
    await page.keyboard.press("Control+Shift+M");
    await s.draft("edit");
    await s.mathType("q");
    const current = await s.text();
    const inside = current.indexOf("$a + b$") + 2;
    await s.invoke("document_apply", {
      request: {
        id: `e2e-eq-external-${tag}`,
        origin: { kind: "external" },
        edits: [{ document: await s.docId(), baseRevision: await s.revision(), patches: [{ range: { start: inside, end: inside }, text: "c" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await page.locator('[data-testid="equation-view"][data-conflict="changed"]').waitFor({ timeout: 10000 });
    assert.ok(await page.getByTestId("equation-apply").isDisabled(), "Apply is disabled during a conflict");
    await shot(`${tag}-04-conflict`);
    if (language === "latex" && engine === "pdfLatex") await referenceShots("equation-panel-conflict");
    const afterExternal = await s.text();
    assert.ok(afterExternal.includes("$ac + b$"), "only the external change is in the document");
    await page.getByTestId("equation-cancel").click();
    await page.waitForTimeout(300);
    assert.equal(await s.text(), afterExternal, "the conflicted draft wrote nothing");

    // 6. Symbol search by name, synonym, glyph, command; Enter inserts at the retained caret.
    await s.monacoCaret("SymbolTarget.");
    await page.keyboard.press("Control+Shift+M");
    await s.draft("insert");
    await s.mathType("x");
    for (const query of ["less than or equal", "leq", "≤", "<="]) {
      const name = await s.search(query);
      assert.match(name ?? "", /less.*(than)?.*equal/i, `"${query}" finds the relation first (got ${name})`);
    }
    await page.keyboard.press("Enter"); // in the search field: inserts the first result
    await s.mathType("y");
    await s.apply();
    await s.waitText((t) => PATTERNS.leq[language].test(t.slice(t.indexOf("SymbolTarget."))), "≤ inserted");
    const symbolLine = (await s.text()).split("\n").find((l) => l.startsWith("SymbolTarget."));
    assert.ok(/x.*(leq?|lt\.eq|<=|≤).*y/.test(symbolLine), `inserted at the retained caret: ${symbolLine}`);
    await s.settled("symbol → new pages");

    // 7. A display matrix with an added row compiles.
    await s.monacoCaret("DisplayTarget.");
    await page.keyboard.press("Control+Shift+M");
    await s.draft("insert");
    await page.getByRole("radio", { name: "Display" }).click();
    await s.template("Matrix (2 × 2)");
    await s.mathType("a");
    await page.keyboard.press("Tab");
    await s.mathType("b");
    await page.keyboard.press("Tab");
    await s.mathType("c");
    await page.keyboard.press("Tab");
    await s.mathType("d");
    await page.getByTestId("equation-grid-actions").getByRole("button", { name: "Row below" }).click();
    await s.mathType("e");
    await page.keyboard.press("Enter"); // in a cell: ambiguous, does not apply
    assert.equal(await s.view.getAttribute("data-state"), "draft");
    await page.keyboard.press("Control+Enter");
    await s.waitText((t) => PATTERNS.matrix[language].test(t), "matrix written");
    await s.settled("matrix → new pages");
    await shot(`${tag}-05-matrix`);

    // 8. Unsupported source (custom macro/function) is preserved exactly.
    if (language !== "markdown") {
      const macro = language === "latex" ? "\\vect{v}" : "vect(v)";
      await s.monacoCaret("A vector", "A vector $".length + 1);
      await page.keyboard.press("Control+Shift+M");
      await s.draft("edit");
      await page.getByTestId("equation-opaque").waitFor();
      await page.keyboard.press("End");
      await s.mathType("+1");
      await page.keyboard.press("Enter");
      const kept = await s.waitText((t) => /\+w\+1/.test(squash(t)), "edit next to the unsupported part");
      assert.ok(kept.includes(`$${macro}`), `"${macro}" kept byte-for-byte`);
      await s.settled("unsupported kept → new pages");
    }

    // 9. LaTeX: a symbol that needs amssymb adds the package in the same transaction.
    if (language === "latex") {
      await s.monacoCaret("SymbolTarget.");
      await page.keyboard.press("Control+Shift+M");
      await s.draft("insert");
      await s.search("lesssim");
      await page.keyboard.press("Enter");
      await page.getByTestId("equation-requirement").waitFor();
      assert.match(await page.getByTestId("equation-requirement").innerText(), /amssymb/);
      before = await s.revision();
      const withoutPackage = await s.text();
      await s.apply();
      const withPackage = await s.waitText((t) => t.includes("\\usepackage{amssymb}"), "package added");
      assert.equal(await s.revision(), before + 1, "equation and package in one transaction");
      assert.ok(withPackage.indexOf("\\usepackage{amssymb}") < withPackage.indexOf("\\begin{document}"));
      await s.settled("amssymb symbol → new pages");
      await page.locator('[data-testid="source-editor"] .view-line', { hasText: "SymbolTarget." }).first().click();
      await page.keyboard.press("Control+z");
      await s.waitText((t) => t === withoutPackage, "one undo removes the equation and the package");
    }

    await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("workspace_close"));
  }

  try {
    await run("latex", { engine: "pdfLatex" });
    await run("latex", { engine: "luaLatex", short: true });
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
