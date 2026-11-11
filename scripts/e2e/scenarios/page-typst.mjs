// Exact-page editing of a Typst document in the real app: click in a word and
// type, reflow across a line, edit inside a display equation, select and
// delete, undo, large insertion moving the figure/footnote, failed build with
// last-good pages, PDF export. Screenshots go to test-results/e2e/page-typst.
//
// Uses the debug-only `__expositionDev.openPage` harness (dev_open_document)
// until the workbench shell and workspace file opening are integrated.
import { cp, mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PATH = "main.typ";

export default async function pageTypst({ launchApp, artifactsDir, assert, fixture }) {
  const workspace = await mkdtemp(join(tmpdir(), "exposition-typst-"));
  await cp(fixture("typst-basic"), workspace, { recursive: true });
  const app = await launchApp();
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const invoke = (cmd, args = {}) => page.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args]);
  const timings = [];

  try {
    await page.waitForFunction(() => window.__expositionDev !== undefined, null, { timeout: 15000 });
    const info = await page.evaluate((p) => window.__expositionDev.openPage(p), join(workspace, PATH));
    const doc = info.id;
    const state = () => invoke("document_state", { document: doc });
    const text = async () => (await state()).text;

    const editor = page.locator('[data-testid="page-editor"]');
    /** Wait until the shown build covers the current revision (not stale). */
    const settled = async (label) => {
      const started = Date.now();
      await page.waitForFunction(
        () => {
          const el = document.querySelector('[data-testid="page-editor"]');
          const img = document.querySelector('[data-testid="page"] img');
          return el?.dataset.stale === "false" && el.dataset.build && img?.complete;
        },
        null,
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

    // 2. Type at the end of the first line until it reflows; the caret follows.
    let source = await text();
    const lineEndOffset = source.indexOf("long enough ") + "long enough".length;
    const p2 = await point(lineEndOffset, "upstream", -0.2);
    await page.mouse.click(p2.x, p2.y);
    await waitCaretOffset(lineEndOffset);
    const startTop = (await caret()).box.y;
    let typed = "";
    let reflowed = false;
    for (const ch of " wonderfully extended") {
      await page.keyboard.type(ch);
      typed += ch;
      await waitCaretOffset(lineEndOffset + typed.length);
      await settled(`reflow typing ${typed.length}`);
      const c = await caret();
      if (c.box.y > startTop + 5) {
        reflowed = true;
        break;
      }
    }
    assert.ok(reflowed, "typing at the line end reflowed the caret onto the next line");
    source = await text();
    assert.ok(source.includes(`long enough${typed} to wrap`), "typed text is in the source at the caret");
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

    // 4. Select "plain " with click + shift-click and delete it; then undo.
    const plain = source.indexOf("plain prose");
    const s1 = await point(plain, "downstream", 0.2);
    const s2 = await point(plain + 6, "downstream", 0.2);
    await page.mouse.click(s1.x, s1.y);
    await waitCaretOffset(plain);
    await page.mouse.click(s2.x, s2.y, { modifiers: ["Shift"] });
    await waitCaretOffset(plain + 6);
    await page.waitForSelector(".page-editor__selection");
    await shot("05-selection");
    const beforeDelete = await text();
    await page.keyboard.press("Backspace");
    await page.waitForFunction(
      async ([d]) => !(await window.__TAURI_INTERNALS__.invoke("document_state", { document: d })).text.includes("plain prose"),
      [doc],
      { timeout: 10000 },
    );
    source = await text();
    assert.equal(source, beforeDelete.slice(0, plain) + beforeDelete.slice(plain + 6), "selection deleted from source");
    await settled("rebuild after delete");
    await shot("06-deleted");
    await page.keyboard.press("Control+z");
    await page.waitForFunction(
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
    const long = "This inserted passage is long enough to push the following material further down the page. ".repeat(6);
    await page.keyboard.insertText(long);
    await page.waitForFunction(
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
    await page.waitForSelector('[data-testid="problem"]');
    const problem = await page.locator('[data-testid="problem"]').first().textContent();
    assert.ok(problem.includes("nosuch"), `problem listed: ${problem}`);
    await shot("09-failed-build-stale");
    // Fix it from the page view's undo (shared history) and recover.
    await page.locator('[data-testid="page-input"]').focus();
    await page.keyboard.press("Control+z");
    await page.waitForFunction(() => document.querySelector('[data-testid="page-editor"]')?.dataset.failed === "false", null, {
      timeout: 10000,
    });
    await settled("recovered");

    // 7. Export PDF from the shown build: same page count and sizes.
    const exported = await invoke("typesetting_export_pdf", { build: await build(), destination: null });
    const pdf = (await readFile(exported.path)).toString("latin1");
    const pdfPages = [...pdf.matchAll(/\/Type\/Page(?!s)/g)].length;
    assert.equal(pdfPages, pages, "PDF page count equals rendered pages");
    const boxes = [...pdf.matchAll(/\/MediaBox\[([^\]]+)\]/g)].map((m) => m[1].trim().split(/\s+/).map(Number));
    const shown = await page.$$eval('[data-testid="page"]', (els) => els.map((e) => Number(e.dataset.widthPt)));
    for (const [i, b] of boxes.entries()) assert.ok(Math.abs(b[2] - b[0] - shown[i]) < 0.01, "PDF page width matches");
    await shot("10-final");

    console.log(`      pages ${pages}; export ${exported.path}`);
    for (const t of timings) console.log(`      ${t.label}: ${t.ms} ms`);
    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    assert.deepEqual(errors, [], "no console errors");
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await app.close();
  }
}
