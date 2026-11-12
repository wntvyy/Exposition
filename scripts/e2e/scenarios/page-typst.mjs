// Exact-page editing of a Typst document in the real app: click in a word and
// type, reflow across a line, edit inside a display equation, select and
// delete, undo, large insertion moving the figure/footnote, failed build with
// last-good pages, IME, Split mode with the shared Monaco model and undo,
// PDF export. Opens a temporary copy of the fixture project through the real
// workspace flow (open folder, file tree, mode switcher). Screenshots go to
// test-results/e2e/page-typst (and reference shots to $EXPOSITION_DEV_HOME).
import { copyFile, cp, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

import { openFromTree, openWorkspace } from "../workspace-helpers.mjs";
import { waitForAsync } from "../wait.mjs";

const PATH = "main.typ";

export default async function pageTypst({ launchApp, artifactsDir, assert, fixture }) {
  const workspace = join(await mkdtemp(join(tmpdir(), "exposition-typst-")), "project");
  await cp(fixture("typst-basic"), workspace, { recursive: true });
  const app = await launchApp();
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const references = [];
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
  const invoke = (cmd, args = {}) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
  const timings = [];

  try {
    // Open the folder and the file through the workbench, then switch to Page mode.
    await openWorkspace(page, workspace);
    await openFromTree(page, PATH);
    const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
    await modeButton("Page").waitFor({ timeout: 10000 });
    await modeButton("Page").click();
    await page.locator('[data-testid="page-editor"]').waitFor({ timeout: 10000 });
    const doc = (await invoke("document_list")).find((d) => d.path === PATH).id;
    const state = () => invoke("document_state", { document: doc });
    const text = async () => (await state()).text;

    const editor = page.locator('[data-testid="page-editor"]');
    /** Wait until the shown build covers the current revision (not stale). */
    const settled = async (label) => {
      const started = Date.now();
      await waitForAsync(page,
        async (path) => {
          const el = document.querySelector('[data-testid="page-editor"]');
          const img = document.querySelector('[data-testid="page"] img');
          if (!(el?.dataset.stale === "false" && el.dataset.build && img?.complete)) return false;
          // The shown build must cover the document's current revision, not
          // merely be "not stale" before the newest edit has been noticed.
          const docs = await window.__TAURI_INTERNALS__.invoke("document_list");
          const doc = docs.find((d) => d.path === path);
          return !!doc && el.dataset.buildRevision === String(doc.revision);
        },
        PATH,
        { timeout: 15000 },
      );
      timings.push({ label, ms: Date.now() - started });
      return editor.getAttribute("data-build");
    };
    const build = () => editor.getAttribute("data-build");

    /** Screen point of a source offset (center of the caret line), via the page map. */
    const point = async (offset, affinity = "downstream", dx = 0) => {
      const g = await invoke("page_caret", { build: await build(), path: PATH, offset, affinity });
      assert.ok(g, `no geometry for offset ${offset}`);
      const el = page.locator(`[data-testid="page"][data-page="${g.page}"]`);
      // Scroll the target into the middle of the page view first (vertically
      // and, when the page is wider than the view, horizontally).
      await el.evaluate(
        (pageEl, [top, x]) => {
          const scroller = pageEl.closest('[data-testid="page-scroller"]');
          const scale = pageEl.getBoundingClientRect().width / Number(pageEl.dataset.widthPt);
          const view = scroller.getBoundingClientRect();
          const y = pageEl.getBoundingClientRect().top + top * scale;
          scroller.scrollTop += y - (view.top + view.height / 2);
          const left = pageEl.getBoundingClientRect().left + x * scale;
          scroller.scrollLeft += left - (view.left + view.width / 2);
        },
        [g.top, g.x + dx],
      );
      const box = await el.boundingBox();
      const scale = box.width / Number(await el.getAttribute("data-width-pt"));
      return { x: box.x + (g.x + dx) * scale, y: box.y + ((g.top + g.bottom) / 2) * scale, g, scale };
    };
    const caret = async () => {
      const c = page.locator('[data-testid="page-caret"]');
      await c.waitFor();
      return { offset: Number(await c.getAttribute("data-offset")), box: await c.boundingBox() };
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

    await settled("initial build");
    await shot("01-opened");
    const original = await text();

    // 1. Click inside "explains" (between "exp" and "lains") and type.
    const at = original.indexOf("explains") + 3;
    const p1 = await point(at, "downstream", 0.6);
    await page.mouse.click(p1.x, p1.y);
    await waitCaretOffset(at);
    const before = await caret();
    await page.keyboard.type("XY");
    await waitCaretOffset(at + 2);
    const afterTyping = await text();
    assert.equal(afterTyping, original.slice(0, at) + "XY" + original.slice(at), "source changed exactly at the click");
    await settled("rebuild after typing");
    const sel = await invoke("page_selection", { build: await build(), path: PATH, start: at, end: at + 2 });
    assert.equal(sel.text, "XY", "the page renders the typed text where it was typed");
    const after = await caret();
    assert.ok(after.box.x > before.box.x, "caret moved right after the typed text");
    await shot("02-typed-in-word");

    // 2. Type after the last word of the paragraph's first line until the
    //    text no longer fits and reflows; the caret follows onto the next line.
    let source = await text();
    const paragraph = source.indexOf("The first paragraph");
    const lineEnd = await invoke("page_navigate", {
      build: await build(),
      path: PATH,
      offset: paragraph,
      affinity: "downstream",
      motion: "lineEnd",
      goalX: null,
    });
    // The line ends after a (trimmed) space: the last word ends just before it.
    const lastWordEnd = source[lineEnd.caret.offset - 1] === " " ? lineEnd.caret.offset - 1 : lineEnd.caret.offset;
    const p2 = await point(lastWordEnd, "upstream", -0.3);
    await page.mouse.click(p2.x, p2.y);
    await waitCaretOffset(lastWordEnd);
    const startTop = (await caret()).box.y;
    let typed = "";
    let reflowed = false;
    for (const ch of " wonderfully") {
      await page.keyboard.type(ch);
      typed += ch;
      await waitCaretOffset(lastWordEnd + typed.length);
      await settled(`reflow typing ${typed.length}`);
      const c = await caret();
      if (c.box.y > startTop + 5) {
        reflowed = true;
        break;
      }
    }
    assert.ok(reflowed, "typing at the line end reflowed the caret onto the next line");
    source = await text();
    assert.equal(source.slice(lastWordEnd, lastWordEnd + typed.length + 1), `${typed} `, "typed text is in the source at the caret");
    await shot("03-reflowed");

    // 3. Edit inside the display equation: after the denominator 2, type 0.
    const denominator = source.indexOf("/ 2 $") + 3;
    const p3 = await point(denominator, "upstream", -0.3);
    await page.mouse.click(p3.x, p3.y);
    await waitCaretOffset(denominator);
    assert.equal(await page.locator('[data-testid="page-mode"]').textContent(), "Math");
    await page.keyboard.type("0");
    await waitCaretOffset(denominator + 1);
    source = await text();
    assert.ok(source.includes("$ sum_(k=1)^n k = (n (n + 1)) / 20 $"), "math edited inside $...$");
    await settled("rebuild after math edit");
    const mathSel = await invoke("page_selection", { build: await build(), path: PATH, start: denominator - 1, end: denominator + 1 });
    assert.equal(mathSel.text, "20", "the denominator renders as 20");
    await shot("04-math-edit");
    await referenceShots("page-editing");

    // 4. Select "plain " with click + shift-click and delete it; then undo.
    const plain = source.indexOf("plain prose");
    const s1 = await point(plain, "downstream", 0.2);
    const s2 = await point(plain + 6, "downstream", 0.2);
    await page.mouse.click(s1.x, s1.y);
    await waitCaretOffset(plain);
    await page.keyboard.down("Shift");
    await page.mouse.click(s2.x, s2.y);
    await page.keyboard.up("Shift");
    await waitCaretOffset(plain + 6);
    await page.waitForSelector(".page-editor__selection", { timeout: 5000 });
    assert.equal(await page.locator('[data-testid="page-caret"]').getAttribute("data-anchor"), String(plain));
    await shot("05-selection");
    const beforeDelete = await text();
    await page.keyboard.press("Backspace");
    await waitForAsync(page,
      async ([d]) => !(await window.__TAURI_INTERNALS__.invoke("document_state", { document: d })).text.includes("plain prose"),
      [doc],
      { timeout: 10000 },
    );
    source = await text();
    assert.equal(source, beforeDelete.slice(0, plain) + beforeDelete.slice(plain + 6), "selection deleted from source");
    await settled("rebuild after delete");
    await shot("06-deleted");
    await page.keyboard.press("Control+z");
    await waitForAsync(page,
      async ([d, t]) => (await window.__TAURI_INTERNALS__.invoke("document_state", { document: d })).text === t,
      [doc, beforeDelete],
      { timeout: 10000 },
    );
    await settled("rebuild after undo");
    await shot("07-undone");

    // 5. A large insertion moves the figure and the footnote.
    source = await text();
    const caption = source.indexOf("A gray rectangle");
    const footnote = source.indexOf("A footnote with");
    const geometry = async (offset) => {
      const r = await invoke("page_selection", { build: await build(), path: PATH, start: offset, end: offset + 6 });
      return r.rects[0];
    };
    const captionBefore = await geometry(caption);
    const footnoteBefore = await geometry(footnote);
    const insertAt = source.indexOf("The second paragraph");
    const p5 = await point(insertAt, "downstream", 0.2);
    await page.mouse.click(p5.x, p5.y);
    await waitCaretOffset(insertAt);
    const long = "This inserted passage is long enough to push the following material further down the page. ".repeat(18);
    await page.keyboard.insertText(long);
    await waitForAsync(page,
      async ([d]) => (await window.__TAURI_INTERNALS__.invoke("document_state", { document: d })).text.includes("push the following"),
      [doc],
      { timeout: 10000 },
    );
    await settled("rebuild after large insertion");
    source = await text();
    const captionAfter = await geometry(source.indexOf("A gray rectangle"));
    const footnoteAfter = await geometry(source.indexOf("A footnote with"));
    assert.ok(
      captionAfter.page > captionBefore.page || captionAfter.y > captionBefore.y + 20,
      `figure moved: ${JSON.stringify([captionBefore, captionAfter])}`,
    );
    assert.ok(
      footnoteAfter.page !== footnoteBefore.page || Math.abs(footnoteAfter.y - footnoteBefore.y) > 1,
      `footnote moved: ${JSON.stringify([footnoteBefore, footnoteAfter])}`,
    );
    const pages = await page.locator('[data-testid="page"]').count();
    await shot("08-large-insertion");

    // 6. A deliberate error keeps the last good pages (stale) and lists the problem.
    const goodBuild = await build();
    const errAt = source.indexOf("Exposition Basics");
    await invoke("document_apply", {
      request: {
        id: "e2e-error",
        origin: { kind: "source" },
        edits: [{ document: doc, baseRevision: (await state()).info.revision, patches: [{ range: { start: errAt, end: errAt }, text: "#nosuch " }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await page.waitForFunction(() => document.querySelector('[data-testid="page-editor"]')?.dataset.failed === "true", null, {
      timeout: 10000,
    });
    assert.equal(await build(), goodBuild, "last good pages stay visible");
    await page.getByRole("tab", { name: "Problems" }).click();
    await page.waitForSelector('[data-testid="problem"]');
    const problem = await page.locator('[data-testid="problem"]').first().textContent();
    assert.ok(problem.includes("nosuch"), `problem listed: ${problem}`);
    await shot("09-failed-build-stale");
    await referenceShots("page-stale-output");
    // Fix it from the page view's undo (shared history) and recover.
    await page.locator('[data-testid="page-input"]').focus();
    await page.keyboard.press("Control+z");
    await page.waitForFunction(() => document.querySelector('[data-testid="page-editor"]')?.dataset.failed === "false", null, {
      timeout: 10000,
    });
    await settled("recovered");

    // 7. IME composition: shown while composing, inserted once on commit,
    //    including an astral character (UTF-16 surrogate pair).
    source = await text();
    const imeAt = source.indexOf("setting");
    const p7 = await point(imeAt, "downstream", 0.2);
    await page.mouse.click(p7.x, p7.y);
    await waitCaretOffset(imeAt);
    const beforeIme = await text();
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Input.imeSetComposition", { text: "u", selectionStart: 1, selectionEnd: 1 });
    await cdp.send("Input.imeSetComposition", { text: "ü", selectionStart: 1, selectionEnd: 1 });
    await page.waitForSelector(".page-editor__composition");
    assert.equal(await page.locator(".page-editor__composition").textContent(), "ü", "composition shown at the caret");
    assert.equal(await text(), beforeIme, "nothing is inserted while composing");
    await shot("11-ime-composing");
    await cdp.send("Input.insertText", { text: "über𝔸 " });
    await waitCaretOffset(imeAt + "über𝔸 ".length);
    source = await text();
    assert.equal(source, beforeIme.slice(0, imeAt) + "über𝔸 " + beforeIme.slice(imeAt), "committed composition inserted once");
    assert.equal(await page.locator(".page-editor__composition").count(), 0, "composition cleared");
    await settled("rebuild after IME commit");
    const imeSel = await invoke("page_selection", { build: await build(), path: PATH, start: imeAt, end: imeAt + 6 });
    assert.equal(imeSel.text, "über𝔸", "committed text renders at the caret");
    await cdp.detach();

    // 8. Split mode: the source (Monaco) and page views share one document.
    //    Typing on the page appears in Monaco; Ctrl+Z in Monaco undoes it.
    await modeButton("Split").click();
    await page.locator('[data-testid="typst-split"] [data-testid="source-editor"] .monaco-editor').waitFor({ timeout: 10000 });
    await page.locator('[data-testid="typst-split"] [data-testid="page-editor"]').waitFor();
    await settled("split view shown");
    source = await text();
    const splitAt = source.indexOf("Offices");
    const p8 = await point(splitAt, "downstream", 0.2);
    await page.mouse.click(p8.x, p8.y);
    await waitCaretOffset(splitAt);
    await page.keyboard.type("Busy ");
    await waitCaretOffset(splitAt + 5);
    const monacoText = () =>
      page.evaluate(() => document.querySelector('[data-testid="typst-split"] .monaco-editor .view-lines')?.textContent ?? "");
    const sawInMonaco = await page
      .waitForFunction(
        () =>
          (document.querySelector('[data-testid="typst-split"] .monaco-editor .view-lines')?.textContent ?? "")
            .replace(/ /g, " ")
            .includes("Busy Offices"),
        null,
        { timeout: 10000 },
      )
      .then(() => true, () => false);
    assert.ok(sawInMonaco, `page edit visible in Monaco: ${(await monacoText()).slice(0, 200)}`);
    await settled("rebuild after split typing");
    await shot("12-split");
    await referenceShots("page-split");

    // Regression: a zoomed page wider than its pane must stay fully reachable
    // by horizontal scrolling (a centered flex item used to overflow to the
    // left, out of scroll range).
    const splitPage = page.locator('[data-testid="typst-split"] [data-testid="page-editor"]');
    const zoomIns = 4;
    for (let i = 0; i < zoomIns; i++) await splitPage.getByRole("button", { name: "Zoom in" }).click();
    const reach = await splitPage.evaluate((root) => {
      const scroller = root.querySelector(".page-editor__scroller");
      const pageEl = root.querySelector(".page-editor__page");
      scroller.scrollLeft = 0;
      const atStart = pageEl.getBoundingClientRect().left - scroller.getBoundingClientRect().left;
      scroller.scrollLeft = scroller.scrollWidth;
      const atEnd = scroller.getBoundingClientRect().right - pageEl.getBoundingClientRect().right;
      return {
        wider: pageEl.getBoundingClientRect().width > scroller.clientWidth,
        atStart,
        atEnd,
      };
    });
    assert.ok(reach.wider, "zoomed page is wider than the split pane");
    assert.ok(reach.atStart >= -1, `left edge of a wide page is reachable (offset ${reach.atStart})`);
    assert.ok(reach.atEnd >= -1, `right edge of a wide page is reachable (offset ${reach.atEnd})`);
    for (let i = 0; i < zoomIns; i++) await splitPage.getByRole("button", { name: "Zoom out" }).click();
    await splitPage.evaluate((root) => {
      root.querySelector(".page-editor__scroller").scrollLeft = 0;
    });
    await page.locator('[data-testid="typst-split"] .monaco-editor .view-lines').click();
    await page.keyboard.press("Control+z");
    await waitForAsync(page,
      async ([d]) => !(await window.__TAURI_INTERNALS__.invoke("document_state", { document: d })).text.includes("Busy Offices"),
      [doc],
      { timeout: 10000 },
    );
    await settled("rebuild after Monaco undo");
    const undone = await invoke("page_selection", { build: await build(), path: PATH, start: splitAt, end: splitAt + 7 });
    assert.equal(undone.text, "Offices", "the page shows the source-side undo");
    await modeButton("Page").click();
    await page.locator('[data-testid="page-editor"]').waitFor();
    await settled("page view after split");

    // 9. Export PDF from the shown build: same page count and sizes.
    const exported = await invoke("typesetting_export_pdf", { build: await build(), destination: null });
    const pdf = (await readFile(exported.path)).toString("latin1");
    const pdfPages = [...pdf.matchAll(/\/Type\/Page(?!s)/g)].length;
    assert.equal(pdfPages, pages, "PDF page count equals rendered pages");
    const boxes = [...pdf.matchAll(/\/MediaBox\[([^\]]+)\]/g)].map((m) => m[1].trim().split(/\s+/).map(Number));
    const shown = await page.$$eval('[data-testid="page"]', (els) => els.map((e) => Number(e.dataset.widthPt)));
    for (const [i, b] of boxes.entries()) assert.ok(Math.abs(b[2] - b[0] - shown[i]) < 0.01, "PDF page width matches");
    await shot("10-final");

    // 10. Regressions from the R-m23 review, in the real app.
    const runCommand = async (query) => {
      await page.keyboard.press("Control+Shift+P");
      const palette = page.getByRole("textbox", { name: "Command" });
      await palette.waitFor({ state: "visible", timeout: 5000 });
      await palette.fill(query);
      // Wait until the filtered list ranks the command first, then run it
      // (Enter on a list that is still filtering does nothing).
      await page.waitForFunction(
        (q) => (document.querySelector(".wb-palette__item")?.textContent ?? "").toLowerCase().includes(q.toLowerCase()),
        query,
        { timeout: 5000 },
      );
      for (let i = 0; i < 3; i++) {
        await page.keyboard.press("Enter");
        if (await palette.waitFor({ state: "detached", timeout: 1500 }).then(() => true, () => false)) return;
      }
      throw new Error(`the command palette did not run "${query}"`);
    };
    const applySource = async (at, insert) =>
      invoke("document_apply", {
        request: {
          id: `e2e-src-${Date.now()}`,
          origin: { kind: "source" },
          edits: [{ document: doc, baseRevision: (await state()).info.revision, patches: [{ range: { start: at, end: at }, text: insert }] }],
          undo: { kind: "newGroup" },
          conflict: "reject",
          label: null,
          selection: null,
        },
      });
    await page.evaluate(async () => {
      window.__pageTypstBuilds = [];
      const internals = window.__TAURI_INTERNALS__;
      const handler = internals.transformCallback((event) => {
        window.__pageTypstBuilds.push({ id: event.payload.result.id, entry: event.payload.result.target.entry });
      });
      await internals.invoke("plugin:event|listen", { event: "typesetting:build", target: { kind: "Any" }, handler });
    });

    // F1: append words at the end of a paragraph (after the space every
    // keystroke used to be refused).
    {
      source = await text();
      const endAt = source.indexOf("marker.") + "marker.".length;
      const pe = await point(endAt, "upstream", -0.2);
      await page.mouse.click(pe.x, pe.y);
      await waitCaretOffset(endAt);
      await page.keyboard.type(" More words.");
      await waitCaretOffset(endAt + " More words.".length);
      assert.ok((await text()).includes("marker. More words.\n"), "F1: words appended at a paragraph end");
      await settled("F1 appended at paragraph end");
    }

    // F3: text produced by a #let macro is not editable on the page and a
    // click on it never edits the definition.
    {
      await applySource(0, "#let product = [Exposition Studio]\n");
      source = await text();
      await applySource(source.indexOf("More words.") + "More words.".length, " Made with #product today.");
      await settled("F3 macro added");
      source = await text();
      const defStart = source.indexOf("[Exposition Studio]");
      const rendered = await invoke("page_selection", { build: await build(), path: PATH, start: defStart, end: defStart + 19 });
      assert.ok(rendered.rects.length > 0, "F3: the macro output is rendered on the page");
      const r = rendered.rects[0];
      const hit = await invoke("page_hit_test", { build: await build(), page: r.page, x: r.x + r.width * 0.3, y: r.y + r.height / 2 });
      assert.equal(hit.nonEditable, "code", "F3: macro output is reported as not editable");
      const el = page.locator(`[data-testid="page"][data-page="${r.page}"]`);
      const box = await el.boundingBox();
      const scale = box.width / Number(await el.getAttribute("data-width-pt"));
      await page.mouse.click(box.x + (r.x + r.width * 0.3) * scale, box.y + (r.y + r.height / 2) * scale);
      await page.waitForFunction(() => (document.querySelector('[data-testid="page-message"]')?.textContent ?? "").includes("edit in source"), null, { timeout: 5000 });
      await page.keyboard.type("Z");
      await page.waitForTimeout(300);
      assert.ok((await text()).includes("#let product = [Exposition Studio]\n"), "F3: the definition is never edited from the page");
      await settled("F3 checked");
    }

    // F4: opening the page view adopts the newest build even when older
    // builds of the entry exist, and survives their eviction.
    {
      await modeButton("Source").click();
      await page.locator('[data-testid="page-editor"]').waitFor({ state: "detached", timeout: 10000 });
      for (let i = 0; i < 16; i++) await writeFile(join(workspace, `other${i}.typ`), `Other ${i}.\n`);
      await invoke("typesetting_build", { entry: PATH });
      for (let i = 0; i < 7; i++) await invoke("typesetting_build", { entry: `other${i}.typ` });
      await modeButton("Page").click();
      await page.locator('[data-testid="page-editor"]').waitFor();
      await settled("F4 page view reopened");
      await page.waitForTimeout(800);
      const newest = await page.evaluate((p) => window.__pageTypstBuilds.filter((b) => b.entry === p).map((b) => b.id).at(-1), PATH);
      assert.equal(await build(), newest, "F4: the page view shows the newest build of its entry");
      for (let i = 7; i < 16; i++) await invoke("typesetting_build", { entry: `other${i}.typ` });
      const before = app.consoleMessages.length;
      source = await text();
      const at = source.indexOf("Offices");
      const pf = await point(at, "downstream", 0.2);
      await page.mouse.click(pf.x, pf.y);
      await waitCaretOffset(at);
      const errs = app.consoleMessages.slice(before).filter((m) => m.startsWith("pageerror") || m.startsWith("error"));
      assert.deepEqual(errs, [], "F4: clicking after other builds places the caret without errors");
    }

    // F5: closing one of two views of the document keeps the other rebuilding.
    {
      await runCommand("Split Editor Right");
      await page.waitForFunction(() => document.querySelectorAll('[data-testid="page-editor"]').length === 2, null, { timeout: 10000 });
      await runCommand("Close Right Editor Group");
      await page.waitForFunction(() => document.querySelectorAll('[data-testid="page-editor"]').length === 1, null, { timeout: 10000 });
      await page.waitForTimeout(300);
      source = await text();
      await applySource(source.indexOf("Offices"), "Still ");
      const revision = (await state()).info.revision;
      await page.waitForFunction(
        (r) => {
          const el = document.querySelector('[data-testid="page-editor"]');
          return el?.dataset.buildRevision === String(r) && el.dataset.stale === "false";
        },
        revision,
        { timeout: 10000 },
      );
      await shot("13-r-m23-regressions");
    }

    console.log(`      pages ${pages}; export ${exported.path}`);
    for (const t of timings) console.log(`      ${t.label}: ${t.ms} ms`);
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
