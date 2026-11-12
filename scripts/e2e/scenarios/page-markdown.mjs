// Exact-page editing of a Markdown document (print profile
// exposition-markdown-print/1) in the real app: click in a paragraph and type
// characters that need Markdown escapes (exact Markdown patch, page shows the
// text), a line-start escape, change an exponent inside inline math, reflow
// from a large insertion, select and delete, undo, an unsupported-math marker
// visible on the page and listed in Problems, Split mode where Ctrl+Z in
// Monaco reverts a page edit, PDF export matching the pages, in-app latency.
// Opens a temporary copy of tests/fixtures/markdown-basic through the real
// workspace flow. Screenshots go to test-results/e2e/page-markdown (and
// reference shots to $EXPOSITION_DEV_HOME/ui-reference).
import { copyFile, cp, mkdir, mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

import { openFromTree } from "../workspace-helpers.mjs";
import { waitForAsync } from "../wait.mjs";
import { runCommand } from "../palette.mjs";

const PATH = "main.md";

export default async function pageMarkdown({ launchApp, artifactsDir, assert, fixture }) {
  const workspace = join(await mkdtemp(join(tmpdir(), "exposition-markdown-")), "project");
  await cp(fixture("markdown-basic"), workspace, { recursive: true });
  const app = await launchApp();
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const references = [];
  /** Reference screenshot in both themes (UI_STANDARD), returning to light. */
  const referenceShots = async (name) => {
    await page.evaluate(() =>
      document.querySelectorAll('[data-testid="page-scroller"]').forEach((s) => {
        s.scrollLeft = 0;
      }),
    );
    for (const theme of ["dark", "light"]) {
      await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
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
    await invoke("workspace_open", { path: workspace });
    await page.locator(`[data-testid="file-tree"] [data-path="${PATH}"]`).waitFor({ timeout: 10000 });
    await openFromTree(page, PATH);
    const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
    await modeButton("Page").waitFor({ timeout: 10000 });
    await modeButton("Page").click();
    await page.locator('[data-testid="page-editor"]').waitFor({ timeout: 10000 });
    const doc = (await invoke("document_list")).find((d) => d.path === PATH).id;
    const state = () => invoke("document_state", { document: doc });
    const text = async () => (await state()).text;
    const textIs = (expected) =>
      waitForAsync(page,
        async ([d, t]) => (await window.__TAURI_INTERNALS__.invoke("document_state", { document: d })).text === t,
        [doc, expected],
        { timeout: 10000 },
      );

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
    /** Wait for a build newer than `previous` to be shown and settled. */
    const rebuilt = async (previous, label) => {
      await page.waitForFunction(
        (p) => {
          const b = document.querySelector('[data-testid="page-editor"]')?.dataset.build;
          return b && b !== p;
        },
        previous,
        { timeout: 15000 },
      );
      return settled(label);
    };
    const selection = async (start, end) => (await invoke("page_selection", { build: await build(), path: PATH, start, end })).text;

    /** Screen point of a source offset (center of the caret line), via the page map. */
    const point = async (offset, affinity = "downstream", dx = 0) => {
      const g = await invoke("page_caret", { build: await build(), path: PATH, offset, affinity });
      assert.ok(g, `no geometry for offset ${offset}`);
      const el = page.locator(`[data-testid="page"][data-page="${g.page}"]`);
      // Scroll the target into the middle of the page view (both axes: the
      // split view is narrower than the zoomed page).
      await el.evaluate(
        (pageEl, [top, left]) => {
          const scroller = pageEl.closest('[data-testid="page-scroller"]');
          const scale = pageEl.getBoundingClientRect().width / Number(pageEl.dataset.widthPt);
          const view = scroller.getBoundingClientRect();
          scroller.scrollTop += pageEl.getBoundingClientRect().top + top * scale - (view.top + view.height / 2);
          scroller.scrollLeft += pageEl.getBoundingClientRect().left + left * scale - (view.left + view.width / 2);
        },
        [g.top, g.x],
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
    const clickAt = async (offset, affinity = "downstream", dx = 0.3) => {
      const p = await point(offset, affinity, dx);
      await page.mouse.click(p.x, p.y);
      await waitCaretOffset(offset);
    };
    /** Type one character at a time; returns the caret after it. */
    const typeChars = async (chars) => {
      for (const ch of chars) await page.keyboard.type(ch);
    };

    await settled("initial build");
    await shot("01-opened");
    const original = await text();

    // 1. Click inside "second" and type text that needs Markdown escapes:
    //    `$` and backticks are escaped, the rest is typed as is.
    const at = original.indexOf("second paragraph") + 3;
    await clickAt(at);
    assert.equal(await page.locator('[data-testid="page-mode"]').textContent(), "Text");
    const beforeTyping = await build();
    const typed = "a $5 `x` & #1 ";
    await typeChars(typed);
    const escaped = "a \\$5 \\`x\\` & #1 ";
    await waitCaretOffset(at + escaped.length);
    const afterTyping = await text();
    assert.equal(afterTyping, original.slice(0, at) + escaped + original.slice(at), "exact Markdown patch at the click");
    await rebuilt(beforeTyping, "rebuild after typing");
    assert.equal(await selection(at, at + escaped.length), typed, "the page renders the typed text where it was typed");
    await shot("02-typed-escapes");

    // 2. A line-start marker typed at the start of a paragraph is escaped.
    let source = await text();
    const final = source.indexOf("Final paragraph");
    await clickAt(final);
    await typeChars("# ");
    await waitCaretOffset(final + 3);
    source = await text();
    assert.equal(source.slice(final, final + 18), "\\# Final paragraph", "line-start `#` escaped");
    await settled("rebuild after line-start escape");
    assert.equal(await selection(final, final + 8), "# Final", "rendered as text, not a heading");

    // 2b. A list marker typed at the start of a paragraph, then a space, is
    //     escaped (prints literally) rather than refused (R-m23 F11).
    source = await text();
    const quote = source.indexOf("A block quote");
    await clickAt(quote);
    let beforeMarker = await build();
    await typeChars("1. ");
    await waitCaretOffset(quote + 4);
    source = await text();
    assert.equal(source.slice(quote - 2, quote + 17), "> 1\\. A block quote", "line-start `1. ` escaped in a quote");
    await rebuilt(beforeMarker, "rebuild after list-marker escape");
    assert.equal(await selection(quote, quote + 8), "1. A bl", "rendered literally, not a list");

    // 2c. Appending words at the end of a paragraph (R-m23 F1): every
    //     keystroke after the trailing space goes in.
    source = await text();
    const paragraphEnd = source.indexOf("print literally.") + "print literally.".length;
    await clickAt(paragraphEnd, "upstream", -0.3);
    beforeMarker = await build();
    await typeChars(" More words.");
    await waitCaretOffset(paragraphEnd + 12);
    source = await text();
    assert.equal(source.slice(paragraphEnd - 16, paragraphEnd + 13), "print literally. More words.\n", "appended at the paragraph end");
    await rebuilt(beforeMarker, "rebuild after appending");
    assert.equal(await selection(paragraphEnd - 10, paragraphEnd + 12), "literally. More words.", "the page shows the appended words");

    // 3. Change an exponent inside inline math: select the `2` of `\alpha^2`
    //    (click at its right edge, Shift+Left) and type 3.
    source = await text();
    const two = source.indexOf("\\alpha^2") + 7;
    await clickAt(two + 1, "upstream", -0.3);
    assert.equal(await page.locator('[data-testid="page-mode"]').textContent(), "Math");
    await page.keyboard.press("Shift+ArrowLeft");
    await waitCaretOffset(two);
    await page.keyboard.type("3");
    await waitCaretOffset(two + 1);
    source = await text();
    assert.ok(source.includes("$\\alpha^3 + \\beta_1 \\le \\sqrt{x}$"), "the exponent changed in the Markdown");
    await settled("rebuild after math edit");
    assert.equal(await selection(two, two + 1), "3", "the page shows the new exponent");
    await shot("03-exponent");
    await referenceShots("markdown-page-editing");

    // 4. Select "snake_case, " with click + shift-click and delete; undo.
    source = await text();
    const snake = source.indexOf("snake_case, ");
    await clickAt(snake, "downstream", 0.2);
    const s2 = await point(snake + 12, "downstream", 0.2);
    await page.keyboard.down("Shift");
    await page.mouse.click(s2.x, s2.y);
    await page.keyboard.up("Shift");
    await waitCaretOffset(snake + 12);
    await page.waitForSelector(".page-editor__selection", { timeout: 5000 });
    await shot("04-selection");
    const beforeDelete = await text();
    await page.keyboard.press("Backspace");
    await textIs(beforeDelete.slice(0, snake) + beforeDelete.slice(snake + 12));
    await settled("rebuild after delete");
    await shot("05-deleted");
    await page.keyboard.press("Control+z");
    await textIs(beforeDelete);
    await settled("rebuild after undo");

    // 4b. Content the page cannot edit says why when clicked (R-m23 F13):
    //     the code block (verbatim) and the task box (generated).
    const clickRect = async (rect) => {
      const el = page.locator(`[data-testid="page"][data-page="${rect.page}"]`);
      await el.evaluate(
        (pageEl, [top, left]) => {
          const scroller = pageEl.closest('[data-testid="page-scroller"]');
          const scale = pageEl.getBoundingClientRect().width / Number(pageEl.dataset.widthPt);
          const view = scroller.getBoundingClientRect();
          scroller.scrollTop += pageEl.getBoundingClientRect().top + top * scale - (view.top + view.height / 2);
          scroller.scrollLeft += pageEl.getBoundingClientRect().left + left * scale - (view.left + view.width / 2);
        },
        [rect.y + rect.height / 2, rect.x + rect.width / 2],
      );
      const box = await el.boundingBox();
      const scale = box.width / Number(await el.getAttribute("data-width-pt"));
      await page.mouse.click(box.x + (rect.x + rect.width / 2) * scale, box.y + (rect.y + rect.height / 2) * scale);
    };
    const messageShows = (needle) =>
      page.waitForFunction(
        (n) => (document.querySelector('[data-testid="page-message"]')?.textContent ?? "").includes(n),
        needle,
        { timeout: 5000 },
      );
    source = await text();
    const fence = source.indexOf("```rust");
    const fenceEnd = source.indexOf("```", fence + 3) + 3;
    const codeRects = (await invoke("page_selection", { build: await build(), path: PATH, start: fence, end: fenceEnd })).rects;
    assert.ok(codeRects.length >= 1, "the code block is on the page map");
    const codeRect = codeRects[1] ?? codeRects[0];
    await clickRect(codeRect);
    await messageShows("Raw/verbatim text");
    await shot("05b-code-not-editable");
    const taskAt = source.indexOf("[x]");
    const taskRect = (await invoke("page_selection", { build: await build(), path: PATH, start: taskAt, end: taskAt + 3 })).rects[0];
    assert.ok(taskRect, "the task box is on the page map");
    await clickRect(taskRect);
    await messageShows("Generated content");
    assert.equal(await text(), source, "clicking non-editable content changes nothing");

    // 5. Reflow: a large insertion in the first paragraph moves later
    //    content (the table, the figure caption) down or onto a new page.
    source = await text();
    const geometry = async (needle) => {
      const s = await text();
      const o = s.indexOf(needle);
      const r = await invoke("page_selection", { build: await build(), path: PATH, start: o, end: o + needle.length });
      return r.rects[0];
    };
    const tableBefore = await geometry("circle constant");
    const captionBefore = await geometry("A small test figure");
    const beforeInsertion = await build();
    const insertAt = source.indexOf("Characters such as");
    await clickAt(insertAt, "downstream", 0.2);
    const long = "This inserted passage is long enough to push the following material further down the page. ".repeat(8);
    await page.keyboard.insertText(long);
    await waitForAsync(page,
      async ([d]) => (await window.__TAURI_INTERNALS__.invoke("document_state", { document: d })).text.includes("push the following"),
      [doc],
      { timeout: 10000 },
    );
    await rebuilt(beforeInsertion, "rebuild after large insertion");
    const tableAfter = await geometry("circle constant");
    const captionAfter = await geometry("A small test figure");
    assert.ok(tableAfter.page > tableBefore.page || tableAfter.y > tableBefore.y + 20, `table moved: ${JSON.stringify([tableBefore, tableAfter])}`);
    assert.ok(
      captionAfter.page > captionBefore.page || captionAfter.y > captionBefore.y + 20,
      `figure moved: ${JSON.stringify([captionBefore, captionAfter])}`,
    );
    await shot("06-reflowed");

    // 6. Unsupported mathematics: a visible marker on the page, listed in Problems.
    source = await text();
    const unsupportedAt = source.indexOf("Final paragraph") - 3;
    const construct = "\\color{red}{x}";
    const beforeUnsupported = await build();
    await invoke("document_apply", {
      request: {
        id: "e2e-unsupported",
        origin: { kind: "source" },
        edits: [
          {
            document: doc,
            baseRevision: (await state()).info.revision,
            patches: [{ range: { start: unsupportedAt, end: unsupportedAt }, text: `Bad $${construct} + y$ math. ` }],
          },
        ],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await rebuilt(beforeUnsupported, "rebuild with unsupported math");
    source = await text();
    const markerAt = source.indexOf(construct);
    assert.equal(await selection(markerAt, markerAt + construct.length), construct, "marker text on the page");
    // Bring the marker into view for the screenshots.
    const markerRect = (
      await invoke("page_selection", { build: await build(), path: PATH, start: markerAt, end: markerAt + construct.length })
    ).rects[0];
    await page.locator(`[data-testid="page"][data-page="${markerRect.page}"]`).evaluate((pageEl, top) => {
      const scroller = pageEl.closest('[data-testid="page-scroller"]');
      const scale = pageEl.getBoundingClientRect().width / Number(pageEl.dataset.widthPt);
      const view = scroller.getBoundingClientRect();
      scroller.scrollTop += pageEl.getBoundingClientRect().top + top * scale - (view.top + view.height / 2);
      scroller.scrollLeft = 0;
    }, markerRect.y);
    await page.getByRole("tab", { name: "Problems" }).click();
    await page.waitForSelector('[data-testid="problem"]');
    const problems = await page.locator('[data-testid="problem"]').allTextContents();
    assert.ok(problems.some((p) => p.includes("\\color")), `problem listed: ${problems.join(" | ")}`);
    await shot("07-unsupported-marker");
    await referenceShots("markdown-unsupported-math");

    // 7. Split mode: the page and Monaco share one document; Ctrl+Z in Monaco
    //    reverts a page edit.
    await modeButton("Split").click();
    await page.locator('[data-testid="typst-split"] [data-testid="source-editor"] .monaco-editor').waitFor({ timeout: 10000 });
    await page.locator('[data-testid="typst-split"] [data-testid="page-editor"]').waitFor();
    await settled("split view shown");
    // Zoom out until the page fits the narrower pane (a zoomed page wider
    // than the pane is centred and its left part cannot be scrolled to).
    for (let i = 0; i < 6; i++) {
      const fits = await page.evaluate(() => {
        const scroller = document.querySelector('[data-testid="typst-split"] [data-testid="page-scroller"]');
        const pageEl = scroller?.querySelector('[data-testid="page"]');
        return Boolean(pageEl) && pageEl.getBoundingClientRect().width <= scroller.getBoundingClientRect().width;
      });
      if (fits) break;
      await page.locator('[data-testid="typst-split"]').getByRole("button", { name: "Zoom out" }).click();
      await page.waitForTimeout(200);
    }
    source = await text();
    const splitAt = source.indexOf("Introduction");
    await clickAt(splitAt, "downstream", 0.2);
    await page.keyboard.type("Busy ");
    await waitCaretOffset(splitAt + 5);
    const sawInMonaco = await page
      .waitForFunction(
        () =>
          (document.querySelector('[data-testid="typst-split"] .monaco-editor .view-lines')?.textContent ?? "")
            .replace(/ /g, " ")
            .includes("Busy Introduction"),
        null,
        { timeout: 10000 },
      )
      .then(() => true, () => false);
    assert.ok(sawInMonaco, "page edit visible in Monaco");
    await settled("rebuild after split typing");
    await shot("08-split");
    await referenceShots("markdown-page-split");
    await page.locator('[data-testid="typst-split"] .monaco-editor .view-lines').click();
    await page.keyboard.press("Control+z");
    await waitForAsync(page,
      async ([d]) => !(await window.__TAURI_INTERNALS__.invoke("document_state", { document: d })).text.includes("Busy Introduction"),
      [doc],
      { timeout: 10000 },
    );
    await settled("rebuild after Monaco undo");
    assert.equal(await selection(splitAt, splitAt + 12), "Introduction", "the page shows the source-side undo");
    await modeButton("Page").click();
    await page.locator('[data-testid="page-editor"]').waitFor();
    await settled("page view after split");

    // 8. Export PDF from the shown build: same page count and sizes.
    const pages = await page.locator('[data-testid="page"]').count();
    const exported = await invoke("typesetting_export_pdf", { build: await build(), destination: null });
    assert.ok(exported.path.endsWith("main.pdf"), exported.path);
    const pdf = (await readFile(exported.path)).toString("latin1");
    // Page objects only (not /Pages or /PageLabel), and the page tree's count.
    const pdfPages = [...pdf.matchAll(/\/Type\s*\/Page(?![A-Za-z])/g)].length;
    assert.equal(pdfPages, pages, "PDF page count equals rendered pages");
    const counts = [...pdf.matchAll(/\/Type\s*\/Pages[^>]*?\/Count\s+(\d+)|\/Count\s+(\d+)[^>]*?\/Type\s*\/Pages/g)];
    if (counts.length) assert.equal(Number(counts[0][1] ?? counts[0][2]), pages, "page tree count");
    const boxes = [...pdf.matchAll(/\/MediaBox\[([^\]]+)\]/g)].map((m) => m[1].trim().split(/\s+/).map(Number));
    const shown = await page.$$eval('[data-testid="page"]', (els) => els.map((e) => Number(e.dataset.widthPt)));
    assert.equal(boxes.length, pages, "one media box per page");
    for (const [i, b] of boxes.entries()) assert.ok(Math.abs(b[2] - b[0] - shown[i]) < 0.01, "PDF page width matches");

    // 9. In-app latency: one keystroke on the page until the new page is
    //    shown (wall clock), and the page editor's own edit-to-render figure.
    source = await text();
    const latencyAt = source.indexOf("line break.") + "line break".length;
    await clickAt(latencyAt, "upstream", -0.3);
    const keystroke = [];
    const pills = [];
    for (let i = 0; i < 8; i++) {
      const previous = await build();
      const started = Date.now();
      await page.keyboard.type("z");
      await rebuilt(previous, `latency keystroke ${i + 1}`);
      keystroke.push(Date.now() - started);
      pills.push(await page.locator('[data-testid="page-build-status"]').textContent());
    }
    assert.ok((await text()).includes("line breakzzzzzzzz."), "keystrokes reached the source");
    const median = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];
    const service = (await invoke("typesetting_build", { entry: PATH })).timings;
    await shot("09-final");

    console.log(`      pages ${pages}; export ${exported.path}`);
    console.log(
      `      keystroke -> new page shown: median ${median(keystroke)} ms (${keystroke.join(", ")}); ` +
        `editor edit-to-render: ${pills.map((p) => p.replace(/[^0-9]/g, "")).filter(Boolean).join(", ")} ms`,
    );
    console.log(
      `      service build (unchanged rebuild): total ${service.totalMs.toFixed(1)} ms, translate+compile ${service.compileMs.toFixed(1)} ms, page map ${service.mapMs.toFixed(1)} ms`,
    );
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
