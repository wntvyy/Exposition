// Exact-page editing of LaTeX documents in the real app, with both the
// pdfLaTeX and the LuaLaTeX target (selected through .exposition/project.json):
// the shared trust prompt before any build, then on the real PDF pages: click
// in a word and type, replace a display-math exponent and insert "+ 1",
// Backspace inside a ligature, select and delete, a deliberate error that
// keeps the last good pages (stale) and lists the problem, the not-editable
// affordance on generated text (section number, \ref), and Split mode where
// Ctrl+Z in Monaco reverts a page edit. Records edit-to-new-page latency.
// Screenshots go to test-results/e2e/page-latex (reference shots in both
// themes also to $EXPOSITION_DEV_HOME/ui-reference).
//
// Requires TeX (pdflatex, lualatex, bibtex) discoverable by the execution service.
import { copyFile, cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

import { openFromTree } from "../workspace-helpers.mjs";

const PATH = "main.tex";

export default async function pageLatex({ launchApp, artifactsDir, assert, fixture }) {
  const app = await launchApp();
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const references = [];
  const latency = [];
  const invoke = (cmd, args = {}) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
  const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
  const editor = page.locator('[data-testid="page-editor"]');

  /** Reference screenshot in both themes (UI_STANDARD), returning to light. */
  const referenceShots = async (name) => {
    for (const theme of ["dark", "light"]) {
      await page.keyboard.press("Control+Shift+P");
      const palette = page.getByRole("textbox", { name: "Command" });
      await palette.waitFor({ state: "visible", timeout: 5000 });
      await palette.fill(`color theme ${theme}`);
      await page.locator(".wb-palette__item").first().waitFor();
      await page.keyboard.press("Enter");
      await palette.waitFor({ state: "detached", timeout: 5000 });
      await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
      await page.waitForTimeout(150);
      const file = join(artifactsDir, `${name}-${theme}.png`);
      await page.screenshot({ path: file });
      references.push(file);
    }
    await page.locator('[data-testid="page-input"]').focus();
  };

  async function project(engine) {
    const root = join(await mkdtemp(join(tmpdir(), `exposition-latex-${engine}-`)), "paper");
    await cp(fixture("latex-basic"), root, { recursive: true });
    await mkdir(join(root, ".exposition"), { recursive: true });
    await writeFile(
      join(root, ".exposition", "project.json"),
      JSON.stringify({
        version: 1,
        entry: PATH,
        targets: [
          { name: "pdf", format: "latex", engine: "pdfLatex", entry: PATH },
          { name: "lua", format: "latex", engine: "luaLatex", entry: PATH },
        ],
        defaultTarget: engine === "pdfLatex" ? "pdf" : "lua",
      }),
    );
    return root;
  }

  async function run(engine, { declineFirst }) {
    const tag = engine === "pdfLatex" ? "pdf" : "lua";
    const engineJobs = async () => (await invoke("job_list")).filter((j) => /^(pdflatex|lualatex) /.test(j.info.label));
    const jobsBefore = (await engineJobs()).length;
    const root = await project(engine);
    const info = await invoke("workspace_open", { path: root });
    await page.locator(`[data-testid="file-tree"] [data-path="${PATH}"]`).waitFor({ timeout: 10_000 });
    await openFromTree(page, PATH);
    await modeButton("Page").waitFor({ timeout: 10000 });
    await modeButton("Page").click();
    await editor.waitFor({ timeout: 10000 });

    // --- Untrusted: the shared trust prompt appears instead of a build.
    const dialog = page.getByRole("alertdialog", { name: "Trust this workspace?" });
    await dialog.waitFor({ timeout: 15000 });
    assert.ok((await dialog.getByTestId("trust-root").innerText()).endsWith("paper"));
    assert.equal((await engineJobs()).length, jobsBefore, "no engine job before trust");
    if (declineFirst) {
      await page.waitForTimeout(400);
      await shot(`${tag}-00-trust-prompt`);
      await dialog.getByRole("button", { name: /Don.t trust/ }).click();
      await dialog.waitFor({ state: "detached" });
      await page.getByTestId("page-scroller").getByText("Workspace not trusted", { exact: true }).waitFor({ timeout: 5000 });
      assert.equal((await engineJobs()).length, jobsBefore, "declined: still no engine job");
      assert.equal((await invoke("trust_status", { root: info.root })).trusted, false);
      // The status item reports the refusal, not a failed build.
      await page.locator('[data-testid="build-status"][data-status="untrusted"]').waitFor({ timeout: 5000 });
      await shot(`${tag}-01-untrusted`);
      await page.getByRole("button", { name: "Trust workspace…" }).click();
      await dialog.waitFor();
    }
    await dialog.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await dialog.waitFor({ state: "detached" });

    const doc = (await invoke("document_list")).find((d) => d.path === PATH).id;
    const state = () => invoke("document_state", { document: doc });
    const text = async () => (await state()).text;
    const build = () => editor.getAttribute("data-build");
    /** Wait until the shown build covers the current revision (not stale). */
    const settled = async (label, since = Date.now()) => {
      await page.waitForFunction(
        () => {
          const el = document.querySelector('[data-testid="page-editor"]');
          const img = document.querySelector('[data-testid="page"] img');
          return el?.dataset.stale === "false" && el.dataset.build && img?.complete;
        },
        null,
        { timeout: 60000 },
      );
      const ms = Date.now() - since;
      latency.push({ engine, label, ms });
      return ms;
    };
    /** Screen point of a source offset (center of the caret line), via the page map. */
    const point = async (offset, affinity = "downstream", dx = 0) => {
      const g = await invoke("page_caret", { build: await build(), path: PATH, offset, affinity });
      assert.ok(g, `no geometry for offset ${offset}`);
      const el = page.locator(`[data-testid="page"][data-page="${g.page}"]`);
      await el.evaluate((pageEl, top) => {
        const scroller = pageEl.closest('[data-testid="page-scroller"]');
        const scale = pageEl.getBoundingClientRect().width / Number(pageEl.dataset.widthPt);
        const y = pageEl.getBoundingClientRect().top + top * scale;
        const view = scroller.getBoundingClientRect();
        scroller.scrollTop += y - (view.top + view.height / 2);
      }, g.top);
      const box = await el.boundingBox();
      const scale = box.width / Number(await el.getAttribute("data-width-pt"));
      return { x: box.x + (g.x + dx) * scale, y: box.y + ((g.top + g.bottom) / 2) * scale, g, scale };
    };
    const waitCaretOffset = async (offset) => {
      try {
        await page.waitForFunction(
          (o) => document.querySelector('[data-testid="page-caret"]')?.getAttribute("data-offset") === String(o),
          offset,
          { timeout: 10000 },
        );
      } catch (error) {
        const actual = await page.locator('[data-testid="page-caret"]').getAttribute("data-offset").catch(() => null);
        throw new Error(`caret offset ${actual}, expected ${offset}`, { cause: error });
      }
    };
    /** Wait until the document text satisfies `predicate`. */
    const waitText = async (predicate, what) => {
      const deadline = Date.now() + 10000;
      while (!predicate(await text())) {
        if (Date.now() > deadline) throw new Error(`source never satisfied: ${what}`);
        await new Promise((r) => setTimeout(r, 50));
      }
    };

    await settled("first build (cold, after trust)");
    // The automatic full build (bibliography, cross-references) follows the
    // cold draft and reflows lines ("??" → "2"); interact after it.
    const fullStart = Date.now();
    for (;;) {
      const status = await invoke("tex_status", { target: null });
      if (status?.last?.mode === "full" && status.last.automatic && !status.running && !status.pending) break;
      if (Date.now() - fullStart > 60000) throw new Error("no automatic full build");
      await new Promise((r) => setTimeout(r, 100));
    }
    await settled("automatic full build", fullStart);
    const engineJob = (await engineJobs()).slice(jobsBefore).find((j) => j.info.result?.outcome.kind === "exited");
    assert.ok(engineJob, `the ${engine} engine ran as a supervised job`);
    await shot(`${tag}-02-opened`);
    const original = await text();

    // 1. Click inside "baseline" (between "base" and "line") and type.
    const at = original.indexOf("baseline") + 4;
    const p1 = await point(at, "downstream", 0.3);
    await page.mouse.click(p1.x, p1.y);
    await waitCaretOffset(at);
    let started = Date.now();
    await page.keyboard.type("QZ");
    await waitCaretOffset(at + 2);
    assert.equal(await text(), original.slice(0, at) + "QZ" + original.slice(at), "source changed exactly at the click");
    await settled("type 2 chars → new page", started);
    const typed = await invoke("page_selection", { build: await build(), path: PATH, start: at, end: at + 2 });
    assert.equal(typed.text, "QZ", "the new page shows the typed text where it was typed");
    await shot(`${tag}-03-typed`);

    // 2. Display math: select the exponent of c^2 and type 3; insert "+ 1" after x_i.
    let source = await text();
    const two = source.indexOf("c^2 + \\sum") + 2;
    // Click on the left part of the raised exponent glyph itself.
    const a1 = await point(two, "downstream", 1);
    await page.mouse.click(a1.x, a1.y);
    await waitCaretOffset(two);
    await page.locator(`[data-testid="page-mode"]`, { hasText: "Math" }).waitFor({ timeout: 5000 });
    // Extend over the (small, raised) exponent with the keyboard.
    await page.keyboard.press("Shift+ArrowRight");
    await waitCaretOffset(two + 1);
    started = Date.now();
    await page.keyboard.type("3");
    await waitText((t) => t.includes("m c^3 + \\sum"), "exponent replaced");
    await settled("exponent edit → new page", started);
    assert.equal((await invoke("page_selection", { build: await build(), path: PATH, start: two, end: two + 1 })).text, "3");
    source = await text();
    const xi = source.indexOf("x_i\n") + 3;
    const p2 = await point(xi, "upstream", -0.1);
    await page.mouse.click(p2.x, p2.y);
    await waitCaretOffset(xi);
    started = Date.now();
    await page.keyboard.type("+ 1");
    await waitText((t) => t.includes("x_i+ 1\n"), "+ 1 inserted in display math");
    await settled("display insert → new page", started);
    const plus = await invoke("page_selection", { build: await build(), path: PATH, start: xi, end: xi + 3 });
    assert.equal(plus.text, "+1", "the display shows x_i + 1");
    await shot(`${tag}-04-math`);

    // 3. Backspace inside the "ffi" ligature of "affine" (before the i).
    source = await text();
    const lig = source.indexOf("ine, office");
    const p3 = await point(lig, "upstream", 0);
    await page.mouse.click(p3.x, p3.y);
    await waitCaretOffset(lig);
    await page.keyboard.press("Backspace");
    await waitText((t) => t.includes("tests: afine, office"), "one f deleted");
    await settled("ligature backspace → new page");
    const fi = await invoke("page_selection", { build: await build(), path: PATH, start: lig - 2, end: lig });
    assert.equal(fi.text, "fi", "the remaining letters form the fi ligature");

    // 4. Select "emphasized " with click + shift-click and delete it.
    source = await text();
    const emph = source.indexOf("emphasized words");
    // (Each point is computed right before its click: computing one scrolls.)
    const s1 = await point(emph, "downstream", 0.2);
    await page.mouse.click(s1.x, s1.y);
    await waitCaretOffset(emph);
    const s2 = await point(emph + 11, "downstream", 0.2);
    await page.keyboard.down("Shift");
    await page.mouse.click(s2.x, s2.y);
    await page.keyboard.up("Shift");
    await waitCaretOffset(emph + 11);
    await page.waitForSelector(".page-editor__selection", { timeout: 5000 });
    await shot(`${tag}-05-selection`);
    await page.keyboard.press("Backspace");
    await waitText((t) => t.includes("\\emph{words}"), "selection deleted");
    await settled("selection delete → new page");

    // 5. Generated text is not editable: the section number and the \ref.
    source = await text();
    const intro = source.indexOf("Introduction");
    const g1 = await point(intro, "downstream", -18);
    await page.mouse.click(g1.x, g1.y);
    await page.locator('[data-testid="page-message"]', { hasText: "Generated" }).waitFor({ timeout: 5000 });
    const ref = source.indexOf("~\\ref{sec:results}");
    const g2 = await point(ref, "upstream", 6);
    await page.mouse.click(g2.x, g2.y);
    await page.locator('[data-testid="page-message"]', { hasText: "Generated" }).waitFor({ timeout: 5000 });
    await shot(`${tag}-06-generated`);

    // 6. A deliberate error keeps the last good pages (stale) and lists the problem.
    const goodBuild = await build();
    const errAt = source.indexOf("Inline mathematics");
    await invoke("document_apply", {
      request: {
        id: `e2e-error-${tag}`,
        origin: { kind: "source" },
        edits: [{ document: doc, baseRevision: (await state()).info.revision, patches: [{ range: { start: errAt, end: errAt }, text: "\\nosuchmacro " }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await page.waitForFunction(() => document.querySelector('[data-testid="page-editor"]')?.dataset.failed === "true", null, {
      timeout: 60000,
    });
    // The shown pages are the last good build (an automatic full build may
    // have replaced `goodBuild` before the error), not the failed one.
    const afterError = await invoke("tex_status", { target: null });
    assert.equal(afterError.last.result.status.kind, "failed");
    assert.equal(await build(), afterError.lastGood.result.id, "last good pages stay visible");
    assert.notEqual(afterError.last.result.id, afterError.lastGood.result.id);
    assert.ok(goodBuild <= afterError.lastGood.result.id || goodBuild.length < afterError.lastGood.result.id.length);
    assert.equal(await editor.getAttribute("data-stale"), "true");
    await page.getByRole("tab", { name: "Problems" }).click();
    await page.waitForSelector('[data-testid="problem"]');
    const problem = await page.locator('[data-testid="problem"]').first().textContent();
    assert.ok(/Undefined control sequence/.test(problem), `problem listed: ${problem}`);
    await shot(`${tag}-07-error-stale`);
    if (declineFirst) await referenceShots("page-latex-stale");
    await page.locator('[data-testid="page-input"]').focus();
    await page.keyboard.press("Control+z");
    await page.waitForFunction(() => document.querySelector('[data-testid="page-editor"]')?.dataset.failed === "false", null, {
      timeout: 60000,
    });
    await settled("recovered from error");

    // 7. Split mode: a page edit shows in Monaco; Ctrl+Z in Monaco reverts it.
    await modeButton("Split").click();
    await page.locator('[data-testid="typst-split"] [data-testid="source-editor"] .monaco-editor').waitFor({ timeout: 10000 });
    await page.locator('[data-testid="typst-split"] [data-testid="page-editor"]').waitFor();
    await settled("split view shown");
    source = await text();
    // (In the first paragraph, which Monaco shows without scrolling.)
    const busyAt = source.indexOf("official workflows");
    const p8 = await point(busyAt, "downstream", 0.2);
    await page.mouse.click(p8.x, p8.y);
    await waitCaretOffset(busyAt);
    await page.keyboard.type("Busy ");
    await waitCaretOffset(busyAt + 5);
    await page.waitForFunction(
      () =>
        /Busy\s*official/.test(
          (document.querySelector('[data-testid="typst-split"] .monaco-editor .view-lines')?.textContent ?? "").replace(
            / /g,
            " ",
          ),
        ),
      null,
      { timeout: 10000 },
    );
    await settled("split page edit → new page");
    await shot(`${tag}-08-split`);
    if (declineFirst) await referenceShots("page-latex-split");
    await page.locator('[data-testid="typst-split"] .monaco-editor .view-lines').click();
    await page.keyboard.press("Control+z");
    await waitText((t) => !t.includes("Busy official"), "Monaco undo reverted the page edit");
    await settled("after Monaco undo");
    const undone = await invoke("page_selection", { build: await build(), path: PATH, start: busyAt, end: busyAt + 5 });
    assert.equal(undone.text, "offic", "the page shows the source-side undo");
    await modeButton("Page").click();
    await editor.waitFor();
    await settled("page view after split");
    if (declineFirst) await referenceShots("page-latex-editing");

    // Edit-to-new-page latency as displayed in the app (status pill).
    const pill = (await page.locator('[data-testid="page-build-status"]').textContent()) ?? "";
    latency.push({ engine, label: "status pill after last edit", text: pill.trim() });
    const configuration = (await invoke("tex_status", { target: null }))?.lastGood?.result.configuration;
    assert.ok(configuration?.compiler.includes(engine === "pdfLatex" ? "pdfTeX" : "LuaHBTeX"), JSON.stringify(configuration));
    await invoke("workspace_close");
  }

  try {
    await run("pdfLatex", { declineFirst: true });
    await run("luaLatex", { declineFirst: false });
    for (const t of latency) console.log(`      ${t.engine} ${t.label}: ${t.ms ?? t.text}${t.ms !== undefined ? " ms" : ""}`);
    await writeFile(join(artifactsDir, "latency.json"), JSON.stringify(latency, null, 2));
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
