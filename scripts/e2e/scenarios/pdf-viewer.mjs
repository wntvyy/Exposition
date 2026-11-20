// QC qc/pdf in the real app (Exposition_QOL_and_Consistency_Assignment.md §2.5):
// project PDF files open from the Files tree in a read-only editor tab.
//  1. A multi-page PDF opens in a tab (no text/binary buffer, no mode switch,
//     never dirty): page pictures from blob: URLs, page count and size.
//  2. Page navigation (Next / Previous, End key), Fit width / Actual size /
//     zoom (toolbar and keys); only pages near the viewport are in the DOM.
//  3. Opening the same file again reuses its tab and its view; other PDFs get
//     their own tabs (single page, name with a space, mixed page sizes).
//  4. Active content (open/launch action, document JavaScript, link
//     annotation, external file reference) stays inert: no request, no link
//     or frame in the DOM, nothing launched.
//  5. Damaged, disguised, empty, and encrypted files show concise states;
//     paths outside the project and other file types are refused by the
//     commands themselves.
//  6. Replacing the open file on disk updates the view; an in-app rename keeps
//     the tab (re-keyed in place); renaming/deleting on disk shows "PDF not
//     found" and the file coming back shows it again; in-app delete is handled.
//  7. Viewing writes nothing and starts no job.
//  8. Switching projects while a large PDF loads leaves no view of the
//     previous project; tokens of the previous project are refused.
// Screenshots: test-results/e2e/pdf-viewer and, with EXPOSITION_DEV_HOME set,
// $EXPOSITION_DEV_HOME/ui-reference/v2/pdf (four themes).
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { consoleErrors, selectFiles, waitForNotification } from "../p1-helpers.mjs";
import { shootThemes, applyTheme } from "../theme-helpers.mjs";
import { invoke, invokeError, openFromTree, tabLocator, waitFor } from "../workspace-helpers.mjs";

/** A PDF from numbered object bodies (object 1 is the catalog) with a classic xref. */
function pdfFromObjects(objects, trailerExtra = "") {
  let out = "%PDF-1.5\n";
  const offsets = [];
  objects.forEach((body, i) => {
    offsets.push(out.length);
    out += `${i + 1} 0 obj\n${body}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const offset of offsets) out += `${String(offset).padStart(10, "0")} 00000 n \n`;
  out += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R ${trailerExtra} >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(out, "latin1");
}

/** One page per [width, height] (points); page i has its own colored blocks. */
function pdfWithPages(sizes, { catalogExtra = "", pageExtra = "", extraObjects = [], hue = 0 } = {}) {
  const n = sizes.length;
  const kids = sizes.map((_, i) => `${3 + 2 * i} 0 R`).join(" ");
  const objects = [`<< /Type /Catalog /Pages 2 0 R ${catalogExtra} >>`, `<< /Type /Pages /Kids [${kids}] /Count ${n} >>`];
  sizes.forEach(([w, h], i) => {
    const c = ((i + hue) % 5) / 5;
    const content = [
      `${(0.15 + 0.7 * c).toFixed(2)} 0.35 ${(0.85 - 0.7 * c).toFixed(2)} rg 36 ${h - 120} ${w - 72} 84 re f`,
      `0.2 0.2 0.2 rg 36 ${h - 160} ${Math.round((w - 72) * 0.6)} 12 re f`,
      ...Array.from({ length: 12 }, (_, k) => `0.55 0.55 0.55 rg 36 ${h - 200 - k * 22} ${w - 72 - ((k * 37 + i * 53) % 140)} 8 re f`),
      `0.9 0.6 0.1 rg 36 36 ${24 + 18 * (i % 20)} 24 re f`,
    ].join("\n");
    objects.push(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ${w} ${h}] /Contents ${4 + 2 * i} 0 R /Resources << >> ${pageExtra} >>`);
    objects.push(`<< /Length ${content.length} >>\nstream\n${content}\nendstream`);
  });
  objects.push(...extraObjects);
  return pdfFromObjects(objects);
}

const A4 = [595, 842];
const viewerOf = (page, path) => page.locator(`[data-testid="pdf-viewer"][data-path="${path}"]`);

async function openPdf(page, path) {
  await openFromTree(page, path);
  const v = viewerOf(page, path);
  await v.waitFor({ timeout: 15000 });
  await waitFor(async () => (await v.getAttribute("data-status")) !== "loading", { timeout: 30000, message: `${path} loaded` });
  return v;
}

const pageReady = (v, n) =>
  waitFor(async () => (await v.locator(`[data-testid="pdf-page"][data-page="${n}"]`).getAttribute("data-state").catch(() => null)) === "ready", {
    timeout: 30000,
    message: `page ${n} rendered`,
  });

export default async function pdfViewer({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-pdf-"));
  const root = join(base, "project");
  const other = join(base, "other");
  const outside = join(base, "outside");
  const marker = join(base, "launched.txt");
  const app = await launchApp();
  const { page } = app;
  const requests = [];
  page.on("request", (r) => requests.push(r.url()));
  const shot = async (name) => {
    await page.screenshot({ path: join(artifactsDir, `pdf-${name}.png`) });
  };

  try {
    await page.setViewportSize({ width: 1440, height: 900 }).catch(() => {});
    // ----- fixture -----
    for (const dir of [join(root, "docs"), join(root, "export"), join(root, "bad"), join(other, "docs"), outside]) {
      await mkdir(dir, { recursive: true });
    }
    const paper = pdfWithPages([A4, A4, A4, A4, A4]);
    const paperReplaced = pdfWithPages([[842, 595], [842, 595]], { hue: 3 });
    const single = pdfWithPages([A4], { hue: 2 });
    const exported = pdfWithPages([A4, [842, 595], [300, 400]], { hue: 1 });
    const markerArg = marker.replace(/\\/g, "\\\\");
    const active = pdfWithPages([A4], {
      catalogExtra: "/OpenAction 5 0 R /Names << /JavaScript << /Names [(a) 6 0 R] >> >> /AA << /WC 6 0 R >>",
      pageExtra: "/Annots [7 0 R] /AA << /O 5 0 R >>",
      extraObjects: [
        `<< /S /Launch /F (cmd.exe) /Win << /F (cmd.exe) /P (/c echo launched > ${markerArg}) >> /Next 6 0 R >>`,
        "<< /S /JavaScript /JS (app.launchURL\\('https://example.invalid/js', true\\); window.__pdfScript = 1;) >>",
        "<< /Type /Annot /Subtype /Link /Rect [0 0 595 842] /A << /S /URI /URI (https://example.invalid/link) >> >>",
      ],
    });
    const big = pdfWithPages(Array.from({ length: 1500 }, () => [200, 120]));
    await writeFile(join(root, "docs", "paper.pdf"), paper);
    await writeFile(join(root, "single.pdf"), single);
    await writeFile(join(root, "export", "out one.pdf"), exported);
    await writeFile(join(root, "active.pdf"), active);
    await writeFile(join(root, "big.pdf"), big);
    await writeFile(join(root, "bad", "damaged.pdf"), Buffer.concat([Buffer.from("%PDF-1.7\n"), Buffer.alloc(600, 7)]));
    await writeFile(join(root, "bad", "fake.pdf"), "TOKEN=not-a-pdf\n");
    await writeFile(join(root, "bad", "empty.pdf"), "");
    await writeFile(
      join(root, "bad", "locked.pdf"),
      pdfFromObjects(
        [
          "<< /Type /Catalog /Pages 2 0 R >>",
          "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
          "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << >> >>",
          "<< /Length 0 >>\nstream\n\nendstream",
          "<< /Filter /Custom.Handler /V 4 /R 4 /Length 128 >>",
        ],
        "/Encrypt 5 0 R /ID [<00112233445566778899aabbccddeeff> <00112233445566778899aabbccddeeff>]",
      ),
    );
    await writeFile(join(root, "notes.md"), "# Notes\n");
    await writeFile(join(outside, "out.pdf"), single);
    await writeFile(join(other, "big.pdf"), pdfWithPages([A4, A4], { hue: 4 }));
    await writeFile(join(other, "readme.md"), "# Other\n");
    if (process.platform === "win32") {
      execFileSync("cmd", ["/c", "mklink", "/J", join(root, "escape"), outside], { stdio: "ignore" });
    }
    const untouched = { "single.pdf": single, "export/out one.pdf": exported, "active.pdf": active, "bad/damaged.pdf": null };
    untouched["bad/damaged.pdf"] = await readFile(join(root, "bad", "damaged.pdf"));

    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="notes.md"]').waitFor({ timeout: 15000 });
    const jobsBefore = (await invoke(page, "job_list")).length;

    // ----- 1. a multi-page PDF opens in a read-only tab -----
    const v = await openPdf(page, "docs/paper.pdf");
    assert.equal(await v.getAttribute("data-status"), "ready");
    assert.equal(await v.getAttribute("data-pages"), "5");
    assert.equal(await v.getAttribute("data-page"), "1");
    assert.equal(await v.getAttribute("data-fit"), "true");
    assert.match(await v.locator('[data-testid="pdf-facts"]').textContent(), /^5 pages · PDF · [\d.]+ (B|KiB|MiB)$/);
    assert.equal(await v.locator('[data-testid="pdf-page-indicator"]').textContent(), "Page 1 of 5");
    await pageReady(v, 1);
    const tab = tabLocator(page, "docs/paper.pdf");
    assert.equal(await tab.locator("[data-file-type]").getAttribute("data-file-type"), "pdf");
    assert.equal(await tab.getAttribute("data-dirty"), null, "a PDF tab is never dirty");
    assert.equal(await v.locator("textarea, input, .monaco-editor, [contenteditable=true]").count(), 0, "no editing surface");
    assert.equal(await page.locator(".wb-editor-group .ui-segmented__item:visible").count(), 0, "no Source/Page/Split switch");
    assert.equal(await page.locator('[data-testid="page-editor"]:visible').count(), 0, "not the page editor");
    assert.match(await v.locator('[data-testid="pdf-page"][data-page="1"] img').getAttribute("src"), /^blob:/);
    const sharp = await v.locator('[data-testid="pdf-page"][data-page="1"] img').evaluate(async (img) => {
      await img.decode().catch(() => {});
      return { natural: img.naturalWidth, shown: img.width };
    });
    assert.ok(sharp.natural >= sharp.shown, `the page is rendered at least at its shown size (${JSON.stringify(sharp)})`);
    const mounted = await v.locator('[data-testid="pdf-page"]').count();
    assert.ok(mounted >= 1 && mounted < 5, `only pages near the viewport are mounted (${mounted})`);
    await shootThemes(page, { outDir: artifactsDir, area: "pdf", name: "pdf-fit-width" });
    await applyTheme(page, "light");

    // ----- 2. navigation and zoom -----
    const indicator = v.locator('[data-testid="pdf-page-indicator"]');
    const next = v.getByRole("button", { name: "Next page" });
    const previous = v.getByRole("button", { name: "Previous page" });
    assert.ok(await previous.isDisabled(), "Previous is disabled on the first page");
    await next.click();
    await waitFor(async () => (await indicator.textContent()) === "Page 2 of 5", { message: "page 2" });
    await pageReady(v, 2);
    await next.click();
    await waitFor(async () => (await v.getAttribute("data-page")) === "3", { message: "page 3" });
    await previous.click();
    await waitFor(async () => (await v.getAttribute("data-page")) === "2", { message: "back to page 2" });
    const stage = v.locator(".xp-pdf-viewer__stage");
    await stage.focus();
    await page.keyboard.press("End");
    await waitFor(async () => (await v.getAttribute("data-page")) === "5", { message: "End scrolls to the last page" });
    await pageReady(v, 5);
    assert.ok(await next.isDisabled(), "Next is disabled on the last page");
    await shot("last-page");
    await previous.click();
    await waitFor(async () => Number(await v.getAttribute("data-page")) < 5, { message: "Previous leaves the end" });
    await stage.focus();
    await page.keyboard.press("Home");
    await waitFor(async () => (await v.getAttribute("data-page")) === "1", { message: "Home scrolls to the first page" });

    const fitZoom = Number(await v.getAttribute("data-zoom"));
    assert.ok(fitZoom > 0.3 && fitZoom <= 4, `fit width zoom ${fitZoom}`);
    const fitsWidth = await stage.evaluate((el) => el.scrollWidth <= el.clientWidth + 1);
    assert.ok(fitsWidth, "fit width needs no horizontal scrolling");
    await v.getByRole("button", { name: "Actual size" }).click();
    assert.equal(await v.getAttribute("data-zoom"), "1");
    assert.equal(await v.getAttribute("data-fit"), "false");
    assert.equal(await v.locator('[data-testid="pdf-zoom"]').textContent(), "100%");
    const width100 = await v.locator('[data-testid="pdf-page"][data-page="1"]').evaluate((el) => el.getBoundingClientRect().width);
    assert.ok(Math.abs(width100 - 595 * (96 / 72)) <= 1, `an A4 page is 793 px wide at 100% (${width100})`);
    await v.getByRole("button", { name: "Zoom in" }).click();
    assert.equal(await v.getAttribute("data-zoom"), "1.25");
    await stage.focus();
    await page.keyboard.press("-");
    await page.keyboard.press("-");
    assert.equal(await v.getAttribute("data-zoom"), "0.75");
    await page.keyboard.press("0");
    assert.equal(await v.getAttribute("data-zoom"), "1");
    for (let i = 0; i < 4; i++) await v.getByRole("button", { name: "Zoom in" }).click();
    assert.equal(await v.getAttribute("data-zoom"), "3");
    await pageReady(v, 1);
    await page.waitForTimeout(600);
    await shot("zoom-300");
    await stage.focus();
    await page.keyboard.press("Shift+Digit1");
    await waitFor(async () => (await v.getAttribute("data-fit")) === "true", { message: "Shift+1 fits the width" });
    await v.getByRole("button", { name: "Zoom out" }).click();
    assert.equal(await v.getAttribute("data-fit"), "false");
    await v.getByRole("button", { name: "Fit width" }).click();
    assert.equal(await v.getAttribute("data-fit"), "true");

    // ----- 3. reopening reuses the tab and the view; other PDFs get their own -----
    const token = await v.getAttribute("data-view");
    const tabKeys = () => page.locator("[data-editor-key]").evaluateAll((els) => els.map((e) => e.getAttribute("data-editor-key")));
    await openFromTree(page, "docs/paper.pdf");
    await page.waitForTimeout(400);
    assert.deepEqual(await tabKeys(), ["file:docs/paper.pdf"], "one tab for the file");
    assert.equal(await v.getAttribute("data-view"), token, "the open view is kept, not loaded again");
    const one = await openPdf(page, "single.pdf");
    assert.equal(await one.getAttribute("data-pages"), "1");
    assert.equal(await one.locator('[data-testid="pdf-page-indicator"]').textContent(), "Page 1 of 1");
    assert.match(await one.locator('[data-testid="pdf-facts"]').textContent(), /^1 page · PDF · /);
    assert.ok(await one.getByRole("button", { name: "Next page" }).isDisabled());
    await pageReady(one, 1);
    await shot("single-page");
    const exp = await openPdf(page, "export/out one.pdf");
    assert.equal(await exp.getAttribute("data-pages"), "3");
    await pageReady(exp, 1);
    await exp.getByRole("button", { name: "Next page" }).click();
    await pageReady(exp, 2);
    const sizes = await exp.locator('[data-testid="pdf-page"]').evaluateAll((els) =>
      els.map((e) => [e.dataset.page, e.getBoundingClientRect().width > e.getBoundingClientRect().height]),
    );
    assert.ok(sizes.some(([n, wide]) => n === "1" && !wide) && sizes.some(([n, wide]) => n === "2" && wide), `mixed page sizes keep their shapes (${JSON.stringify(sizes)})`);
    await shot("mixed-sizes");
    await openFromTree(page, "single.pdf");
    await openFromTree(page, "docs/paper.pdf");
    await viewerOf(page, "docs/paper.pdf").waitFor();
    assert.deepEqual(await tabKeys(), ["file:docs/paper.pdf", "file:single.pdf", "file:export/out one.pdf"], "each file has one tab");

    // ----- 4. active content stays inert -----
    const act = await openPdf(page, "active.pdf");
    assert.equal(await act.getAttribute("data-status"), "ready");
    await pageReady(act, 1);
    await page.waitForTimeout(700);
    const inert = await page.evaluate(() => ({
      script: window.__pdfScript,
      markup: document.body.innerHTML.includes("example.invalid"),
      active: document.querySelectorAll('[data-testid="pdf-viewer"] a, iframe, object, embed').length,
    }));
    assert.deepEqual(inert, { script: undefined, markup: false, active: 0 }, "nothing from the PDF reaches the DOM but pictures");
    assert.deepEqual(requests.filter((u) => u.includes("example.invalid")), [], "no request for the PDF's URLs");
    assert.ok(!existsSync(marker), "the launch action was not run");
    // Clicking the page (over the link annotation) does nothing.
    await act.locator('[data-testid="pdf-page"][data-page="1"]').click();
    await page.waitForTimeout(300);
    assert.deepEqual(requests.filter((u) => u.includes("example.invalid")), [], "the link is not clickable");
    assert.equal(await page.locator('[role="dialog"]:visible, [role="alertdialog"]:visible').count(), 0);

    // ----- 5. bad files and refused paths -----
    for (const [path, problem, title] of [
      ["bad/damaged.pdf", "damaged", "Can't show this PDF"],
      ["bad/fake.pdf", "damaged", "Can't show this PDF"],
      ["bad/empty.pdf", "empty", "No pages to show"],
      ["bad/locked.pdf", "encrypted", "Can't show an encrypted PDF"],
    ]) {
      const bad = await openPdf(page, path);
      assert.equal(await bad.getAttribute("data-status"), problem, path);
      assert.ok((await bad.locator('[data-testid="pdf-viewer-state"]').textContent()).includes(title), `${path}: ${title}`);
      assert.equal(await bad.locator("textarea, .monaco-editor").count(), 0, `${path}: never an editable buffer`);
      assert.equal(await bad.locator('[data-testid="pdf-page"]').count(), 0);
      if (path === "bad/damaged.pdf") await shootThemes(page, { outDir: artifactsDir, area: "pdf", name: "pdf-damaged" });
      if (path === "bad/locked.pdf") await shot("encrypted");
    }
    await applyTheme(page, "light");
    // A path that is not a project path at all is refused before the command runs (a string, no code).
    const refused = async (path) => {
      const outcome = await invokeError(page, "media_pdf_open", { path });
      return "unexpectedSuccess" in outcome ? "no error" : (outcome.error?.code ?? "invalid path");
    };
    assert.equal(await refused("notes.md"), "pdf.notPdf");
    assert.equal(await refused("missing.pdf"), "path.notFound");
    assert.notEqual(await refused("../outside/out.pdf"), "no error", "a path above the project is refused");
    assert.notEqual(await refused(join(outside, "out.pdf")), "no error", "an absolute path is refused");
    if (process.platform === "win32") assert.equal(await refused("escape/out.pdf"), "path.outsideWorkspace");
    assert.equal((await invokeError(page, "media_pdf_page", { token: 999999, page: 0, scale: 1 })).error?.code, "pdf.closed");

    // ----- 6. replace, rename, delete -----
    await tabLocator(page, "docs/paper.pdf").click();
    const shown = viewerOf(page, "docs/paper.pdf");
    await waitFor(async () => (await shown.getAttribute("data-status")) === "ready", { message: "paper shown again" });
    const before = await shown.getAttribute("data-view");
    await writeFile(join(root, "docs", "paper.pdf"), paperReplaced);
    await waitFor(async () => (await shown.getAttribute("data-pages")) === "2", { timeout: 20000, message: "replaced file shown" });
    assert.notEqual(await shown.getAttribute("data-view"), before, "a new view of the new bytes");
    assert.match(await shown.locator('[data-testid="pdf-facts"]').textContent(), /^2 pages · PDF · /);
    await pageReady(shown, 1);
    await shot("replaced");

    // In-app rename (F2 in the tree): the tab stays in place under the new name.
    const keysBefore = await tabKeys();
    await selectFiles(page, ["docs/paper.pdf"]);
    await page.keyboard.press("F2");
    const nameInput = page.getByTestId("file-name-dialog").getByRole("textbox");
    await nameInput.waitFor();
    await nameInput.press("Control+A");
    await page.keyboard.type("renamed.pdf");
    await page.keyboard.press("Enter");
    await waitFor(async () => existsSync(join(root, "docs", "renamed.pdf")) && !existsSync(join(root, "docs", "paper.pdf")), { message: "renamed on disk" });
    await waitFor(async () => (await tabKeys()).includes("file:docs/renamed.pdf"), { message: "tab re-keyed" });
    assert.deepEqual(await tabKeys(), keysBefore.map((k) => (k === "file:docs/paper.pdf" ? "file:docs/renamed.pdf" : k)), "the tab kept its place under the new key");
    const renamed = viewerOf(page, "docs/renamed.pdf");
    await renamed.waitFor({ timeout: 15000 });
    await waitFor(async () => (await renamed.getAttribute("data-status")) === "ready", { timeout: 15000, message: "renamed file shown" });
    assert.equal(await renamed.getAttribute("data-pages"), "2");
    assert.equal(await page.locator('[data-testid="pdf-viewer"][data-path="docs/paper.pdf"]').count(), 0);
    await pageReady(renamed, 1);

    // Renamed and deleted on disk (outside the app).
    await rename(join(root, "docs", "renamed.pdf"), join(root, "docs", "elsewhere.pdf"));
    await waitFor(async () => (await renamed.getAttribute("data-status")) === "missing", { timeout: 20000, message: "renamed away on disk" });
    assert.ok((await renamed.locator('[data-testid="pdf-viewer-state"]').textContent()).includes("PDF not found"));
    await shot("missing");
    await rename(join(root, "docs", "elsewhere.pdf"), join(root, "docs", "renamed.pdf"));
    await waitFor(async () => (await renamed.getAttribute("data-status")) === "ready", { timeout: 20000, message: "back on disk" });
    await rm(join(root, "docs", "renamed.pdf"));
    await waitFor(async () => (await renamed.getAttribute("data-status")) === "missing", { timeout: 20000, message: "deleted on disk" });
    assert.equal(await renamed.locator('[data-testid="pdf-page"]').count(), 0, "no page of the deleted file stays");
    await writeFile(join(root, "docs", "renamed.pdf"), paper);
    await waitFor(async () => (await renamed.getAttribute("data-pages")) === "5", { timeout: 20000, message: "restored on disk" });

    // In-app delete of the open PDF (Delete in the tree).
    await selectFiles(page, ["docs/renamed.pdf"]);
    await page.keyboard.press("Delete");
    await waitForNotification(page, /^Moved .* to the (Recycle Bin|Trash)$/);
    await waitFor(async () => !existsSync(join(root, "docs", "renamed.pdf")), { message: "deleted" });
    await waitFor(
      async () => {
        const left = page.locator('[data-testid="pdf-viewer"][data-path="docs/renamed.pdf"]');
        return (await left.count()) === 0 || (await left.getAttribute("data-status")) === "missing";
      },
      { timeout: 20000, message: "the deleted PDF is no longer shown" },
    );
    const afterDelete = (await tabKeys()).includes("file:docs/renamed.pdf") ? "tab kept, PDF not found" : "tab closed";
    console.log(`in-app delete of an open PDF: ${afterDelete}`);

    // Closing a tab removes its view.
    await tabLocator(page, "single.pdf").click();
    await viewerOf(page, "single.pdf").waitFor();
    await tabLocator(page, "single.pdf").locator(".ui-editor-tab__close").click();
    await waitFor(async () => (await page.locator('[data-testid="pdf-viewer"][data-path="single.pdf"]').count()) === 0, { message: "tab closed" });
    assert.ok(!(await tabKeys()).includes("file:single.pdf"));

    // ----- 7. nothing written, nothing built -----
    for (const [path, bytes] of Object.entries(untouched)) {
      assert.ok((await readFile(join(root, ...path.split("/")))).equals(bytes), `${path} unchanged`);
    }
    assert.equal((await invoke(page, "job_list")).length, jobsBefore, "viewing PDFs starts no job");

    // ----- 8. project switch while a large PDF loads -----
    await tabLocator(page, "export/out one.pdf").click();
    const kept = viewerOf(page, "export/out one.pdf");
    await waitFor(async () => (await kept.getAttribute("data-status")) === "ready", { message: "export shown" });
    const oldToken = Number(await kept.getAttribute("data-view"));
    await openFromTree(page, "big.pdf");
    // Do not wait for it: switch at once.
    await invoke(page, "workspace_open", { path: other });
    await page.locator('[data-testid="file-tree"] [data-path="readme.md"]').waitFor({ timeout: 15000 });
    await waitFor(
      async () => {
        const views = await page.locator('[data-testid="pdf-viewer"]').evaluateAll((els) =>
          els.map((e) => [e.dataset.path, e.dataset.status, e.dataset.pages ?? null]),
        );
        return views.every(([path, status, pages]) => status !== "ready" || (path === "big.pdf" && pages === "2"));
      },
      { timeout: 20000, message: "no view of the previous project" },
    );
    await page.waitForTimeout(1500);
    const stale = await page.locator('[data-testid="pdf-viewer"]').evaluateAll((els) =>
      els.filter((e) => e.dataset.pages === "1500" || e.dataset.pages === "3").map((e) => e.dataset.path),
    );
    assert.deepEqual(stale, [], "a late result of the previous project is never shown");
    const oldPage = await invokeError(page, "media_pdf_page", { token: oldToken, page: 0, scale: 1 });
    assert.ok(["pdf.closed", "workspace.changed"].includes(oldPage.error?.code), `a view of the previous project is refused (${JSON.stringify(oldPage)})`);
    const there = await openPdf(page, "big.pdf");
    assert.equal(await there.getAttribute("data-pages"), "2", "the same path shows the open project's file");
    await pageReady(there, 1);
    await shot("other-project");

    // A long PDF in the first project: opens, and only nearby pages are mounted.
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="notes.md"]').waitFor({ timeout: 15000 });
    const long = await openPdf(page, "big.pdf");
    assert.equal(await long.getAttribute("data-pages"), "1500");
    await pageReady(long, 1);
    assert.ok((await long.locator('[data-testid="pdf-page"]').count()) <= 12, "at most 12 pages mounted");
    await long.locator(".xp-pdf-viewer__stage").focus();
    await page.keyboard.press("End");
    await waitFor(async () => (await long.getAttribute("data-page")) === "1500", { message: "end of the long PDF" });
    await pageReady(long, 1500);
    assert.ok((await long.locator('[data-testid="pdf-page"]').count()) <= 12);
    await shot("long");

    const errors = consoleErrors(app).filter((m) => !/example\.invalid|net::ERR/.test(m));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await app.close();
  }
  await rm(base, { recursive: true, force: true }).catch(() => {});
}
