// QOL q/media in the real app (Exposition_Editor_Explorer_QOL.md §2 A, F, H):
//  A. Images open from the Files tree in a read-only tab: PNG (path with a
//     space), JPEG, SVG; Fit / Actual size / zoom; drag to pan; a hostile SVG
//     (script, onload, external image, iframe) stays inert with no network
//     request; damaged, unsupported (TIFF, text named .png) and missing files
//     show concise states, never an editable buffer; replacing, renaming, and
//     deleting the open image on disk updates the tab; viewing never writes.
//  F. File-type icons in the Files tree and on editor tabs (data-file-type).
//  H. Inline previews in Markdown, LaTeX, and Typst source: literal references
//     resolve (spaces, extension search), references in comments, code, and
//     verbatim and dynamic/remote ones are ignored, missing/outside ones show
//     one-line states; an unsaved reference edit previews at once and Undo
//     removes only the typed text; per-image Hide/Show and document-wide
//     Hide/Show (palette) never change the text, revision, dirty state, or undo;
//     Monaco Find still counts matches; no build job starts.
//  Graph: graph_status revision/entity/relation counts are unchanged by all
//  viewing and previewing.
// Screenshots: test-results/e2e/qol-media and, with EXPOSITION_DEV_HOME set,
// $EXPOSITION_DEV_HOME/ui-reference/media.
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, mkdtemp, readFile, readdir, rename, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { editorLocator, invoke, openFromTree, tabLocator, waitFor } from "../workspace-helpers.mjs";

const HOSTILE_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="240" height="120" onload="window.top.__svgOnload = 1">' +
  '<script>window.top.__svgScript = 1; fetch("https://example.invalid/script")</script>' +
  '<rect width="240" height="120" fill="#3b6fd8"/><circle cx="60" cy="60" r="40" fill="#f4b400"/>' +
  '<image href="https://example.invalid/track.png" x="120" y="20" width="80" height="80"/>' +
  '<foreignObject x="0" y="0" width="10" height="10"><iframe xmlns="http://www.w3.org/1999/xhtml" src="https://example.invalid/frame"></iframe></foreignObject>' +
  "</svg>";

const NOTES_MD = [
  "# Notes",
  "",
  "The plot:",
  "",
  "![A plot](figs/plot%20one.png)",
  "",
  "A vector drawing <img src=\"figs/vector.svg\" alt=\"vector\"> inline.",
  "",
  "```",
  "![in code](figs/photo.jpg)",
  "```",
  "",
  "<!-- ![in a comment](figs/photo.jpg) -->",
  "",
  "![remote](https://example.invalid/remote.png)",
  "",
  "![gone](figs/missing.png)",
  "",
  "![outside](figs/escape/out.png)",
  "",
  "End of the plot notes.",
  "",
].join("\n");

const PAPER_TEX = [
  "\\documentclass{article}",
  "\\usepackage{graphicx}",
  "\\begin{document}",
  "% \\includegraphics{figs/plot one.png}",
  "\\begin{verbatim}",
  "\\includegraphics{figs/vector.svg}",
  "\\end{verbatim}",
  "\\begin{figure}",
  "  \\includegraphics[width=4cm]{figs/photo}",
  "  \\caption{A photo}",
  "\\end{figure}",
  "\\includegraphics{\\figdir/x.png}",
  "\\end{document}",
  "",
].join("\n");

const DOC_TYP = [
  "= Typst",
  "// #image(\"figs/photo.jpg\")",
  "#figure(image(\"figs/plot one.png\", width: 50%), caption: [Plot])",
  "#let p = \"figs/photo.jpg\"",
  "#image(p)",
  "// The document does not compile: previews must not depend on a build.",
  "#this-function-does-not-exist()",
  "",
].join("\n");

function consoleErrors(app) {
  return app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
}

/** Image bytes drawn by the webview's own canvas (PNG/JPEG encoders). */
async function canvasImage(page, type, width, height, hue) {
  const dataUrl = await page.evaluate(
    ([t, w, h, c]) => {
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      const g = ctx.createLinearGradient(0, 0, w, h);
      g.addColorStop(0, `hsl(${c} 70% 55%)`);
      g.addColorStop(1, `hsl(${(c + 120) % 360} 70% 45%)`);
      ctx.fillStyle = g;
      ctx.fillRect(0, 0, w, h);
      ctx.fillStyle = "white";
      ctx.font = `${Math.round(h / 6)}px sans-serif`;
      ctx.fillText(`${w}×${h}`, w / 10, h / 2);
      return canvas.toDataURL(t, 0.9);
    },
    [type, width, height, hue],
  );
  return Buffer.from(dataUrl.split(",")[1], "base64");
}

async function graphCounts(page) {
  await waitFor(
    async () => {
      const s = await invoke(page, "graph_status");
      return s.state.kind === "attached" && s.freshness.state === "synchronized" && s.freshness.pending === 0;
    },
    { timeout: 30000, message: "graph synchronized" },
  );
  const s = await invoke(page, "graph_status");
  return { revision: s.revision, entities: s.entities, relations: s.relations };
}

/** Click into the first text line of a source editor (not a preview) and move the caret. */
async function focusEditor(page, path, where) {
  await editorLocator(page, path).locator(".view-line").first().click();
  await page.keyboard.press(where === "end" ? "Control+End" : "Control+Home");
}

const viewer = (page) => page.locator('[data-testid="image-viewer"]:visible');

async function openImage(page, path) {
  await openFromTree(page, path);
  const v = page.locator(`[data-testid="image-viewer"][data-path="${path}"]`);
  await v.waitFor({ timeout: 10000 });
  await waitFor(async () => (await v.getAttribute("data-status")) !== "loading", { message: `${path} loaded` });
  return v;
}

export default async function qolMedia({ launchApp, artifactsDir, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-media-"));
  const root = join(base, "project");
  const other = join(base, "other");
  const outside = join(base, "outside");
  const shots = [];
  // No typesetting toolchain on PATH: previews must not depend on a build.
  const systemRoot = process.env.SystemRoot ?? "C:/Windows";
  const systemPath = [join(systemRoot, "System32"), systemRoot].join(";");
  const app = await launchApp(process.platform === "win32" ? { env: { Path: systemPath, PATH: systemPath } } : {});
  const { page } = app;
  const requests = [];
  page.on("request", (r) => requests.push(r.url()));
  const shot = async (name, { themes = ["light"] } = {}) => {
    for (const theme of themes) {
      if (themes.length > 1) {
        await runCommand(page, `color theme ${theme}`, theme === "dark" ? /Color Theme: Dark/ : /Color Theme: Light/);
        await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme);
        await page.waitForTimeout(200);
      }
      const file = join(artifactsDir, `media-${name}-${theme}.png`);
      await page.screenshot({ path: file });
      shots.push(file);
    }
  };

  try {
    await page.setViewportSize({ width: 1440, height: 900 }).catch(() => {});
    // ----- fixture (images drawn by the webview's canvas) -----
    await mkdir(join(root, "figs"), { recursive: true });
    await mkdir(join(other, "figs"), { recursive: true });
    await mkdir(outside, { recursive: true });
    const plot = await canvasImage(page, "image/png", 1600, 1200, 210);
    const plotReplaced = await canvasImage(page, "image/png", 800, 400, 20);
    const photo = await canvasImage(page, "image/jpeg", 640, 480, 120);
    const otherPlot = await canvasImage(page, "image/png", 300, 300, 300);
    await writeFile(join(root, "figs", "plot one.png"), plot);
    await writeFile(join(root, "figs", "photo.jpg"), photo);
    await writeFile(join(root, "figs", "vector.svg"), HOSTILE_SVG);
    await writeFile(join(root, "figs", "damaged.png"), Buffer.concat([plot.subarray(0, 40), Buffer.alloc(200, 7)]));
    await writeFile(join(root, "figs", "scan.tiff"), Buffer.from([0x49, 0x49, 0x2a, 0, 8, 0, 0, 0, 0, 0]));
    await writeFile(join(root, "figs", "fake.png"), "TOKEN=not-an-image\n");
    await writeFile(join(outside, "out.png"), photo);
    await writeFile(join(root, "notes.md"), NOTES_MD);
    await writeFile(join(root, "paper.tex"), PAPER_TEX);
    await writeFile(join(root, "doc.typ"), DOC_TYP);
    await writeFile(join(other, "figs", "plot one.png"), otherPlot);
    await writeFile(join(other, "readme.md"), "# Other\n");
    if (process.platform === "win32") {
      execFileSync("cmd", ["/c", "mklink", "/J", join(root, "figs", "escape"), outside], { stdio: "ignore" });
    }
    const imageFiles = ["plot one.png", "photo.jpg", "vector.svg", "damaged.png", "scan.tiff", "fake.png"];
    const before = Object.fromEntries(await Promise.all(imageFiles.map(async (f) => [f, await readFile(join(root, "figs", f))])));

    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="notes.md"]').waitFor({ timeout: 15000 });
    const baseline = await graphCounts(page);
    const tools = await invoke(page, "toolchains_list").catch(() => []);
    const latex = tools.filter((t) => /latex|tex/i.test(`${t.id} ${t.name} ${t.category}`));
    await writeFile(join(artifactsDir, "toolchains.json"), JSON.stringify(tools, null, 1));
    console.log(`LaTeX tools: ${latex.map((t) => `${t.name}=${t.status}`).join(", ") || "none listed"}`);

    // ----- F: file-type icons in the tree -----
    const iconOf = (path) =>
      page.locator(`[data-testid="file-tree"] [data-path="${path}"] [data-file-type]`).first().getAttribute("data-file-type");
    assert.equal(await iconOf("notes.md"), "markdown");
    assert.equal(await iconOf("paper.tex"), "tex");
    assert.equal(await iconOf("doc.typ"), "typst");

    // ----- A: PNG with a space in its name -----
    const png = await openImage(page, "figs/plot one.png");
    assert.equal(await iconOf("figs/plot one.png"), "image");
    assert.equal(await png.getAttribute("data-status"), "ready");
    assert.equal(await png.getAttribute("data-fit"), "true");
    assert.match(await png.locator('[data-testid="image-facts"]').textContent(), /^1600 × 1200 · PNG · [\d.]+ (KB|MB)$/);
    assert.equal(await tabLocator(page, "figs/plot one.png").locator("[data-file-type]").getAttribute("data-file-type"), "image");
    assert.equal(await tabLocator(page, "figs/plot one.png").getAttribute("data-dirty"), null, "an image tab is never dirty");
    assert.equal(await png.locator("textarea, .monaco-editor").count(), 0, "no text/binary buffer");
    assert.match(await png.locator('[data-testid="image-viewer-image"]').getAttribute("src"), /^blob:/);
    const fitZoom = Number(await png.getAttribute("data-zoom"));
    assert.ok(fitZoom < 1, `fit scales the 1600 px image down (${fitZoom})`);
    await shot("viewer-fit", { themes: ["dark", "light"] });
    await png.getByRole("button", { name: "Actual size" }).click();
    assert.equal(await png.getAttribute("data-zoom"), "1");
    assert.equal(await png.locator('[data-testid="image-zoom"]').textContent(), "100%");
    await png.getByRole("button", { name: "Zoom in" }).click();
    assert.equal(await png.getAttribute("data-zoom"), "1.25");
    // Keyboard on the focused image: "-" zooms out, Shift+1 fits.
    const stage = png.locator(".xp-image-viewer__stage");
    await stage.focus();
    await page.keyboard.press("-");
    assert.equal(await png.getAttribute("data-zoom"), "1");
    // Drag to pan.
    const scrollOf = () => stage.evaluate((el) => [el.scrollLeft, el.scrollTop]);
    const [x0, y0] = await scrollOf();
    const box = await stage.boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 - 200, box.y + box.height / 2 - 150, { steps: 6 });
    await page.mouse.up();
    const [x1, y1] = await scrollOf();
    assert.ok(x1 > x0 && y1 > y0, `dragging pans (${x0},${y0} -> ${x1},${y1})`);
    await shot("viewer-actual-size");
    await page.keyboard.press("Shift+Digit1");
    await waitFor(async () => (await png.getAttribute("data-fit")) === "true", { message: "Shift+1 fits" });
    await png.getByRole("button", { name: "Actual size" }).click();
    await png.getByRole("button", { name: "Fit" }).click();
    assert.equal(await png.getAttribute("data-fit"), "true");

    // ----- A: JPEG and hostile SVG -----
    const jpg = await openImage(page, "figs/photo.jpg");
    assert.match(await jpg.locator('[data-testid="image-facts"]').textContent(), /^640 × 480 · JPEG · /);
    const svg = await openImage(page, "figs/vector.svg");
    assert.equal(await svg.getAttribute("data-status"), "ready");
    assert.match(await svg.locator('[data-testid="image-facts"]').textContent(), /^240 × 120 · SVG · /);
    await page.waitForTimeout(500);
    const inert = await page.evaluate(() => ({
      script: window.__svgScript,
      onload: window.__svgOnload,
      markup: document.body.innerHTML.includes("example.invalid"),
      frames: document.querySelectorAll("iframe, object, embed, foreignObject").length,
    }));
    assert.deepEqual(inert, { script: undefined, onload: undefined, markup: false, frames: 0 }, "the SVG stays inert");
    await shot("viewer-svg");

    // ----- A: damaged, unsupported, and disguised files -----
    for (const [path, problem, title] of [
      ["figs/damaged.png", "damaged", "Can't show this image"],
      ["figs/scan.tiff", "unsupported", "Can't show this image format"],
      ["figs/fake.png", "unsupported", "Can't show this image format"],
    ]) {
      const v = await openImage(page, path);
      assert.equal(await v.getAttribute("data-status"), problem, path);
      assert.match(await v.locator('[data-testid="image-viewer-state"]').textContent(), new RegExp(title.replace(/'/g, "'")));
      assert.equal(await v.locator("textarea, .monaco-editor").count(), 0, `${path}: never an editable buffer`);
      if (path === "figs/scan.tiff") await shot("viewer-unsupported");
    }

    // ----- H: Markdown previews -----
    await openFromTree(page, "notes.md");
    const md = editorLocator(page, "notes.md");
    await md.locator(".monaco-editor").waitFor({ timeout: 10000 });
    const docId = await md.getAttribute("data-document-id");
    const previews = (editor) =>
      editor.locator('[data-testid="source-image-preview"]').evaluateAll((els) => els.map((e) => [e.dataset.written, e.dataset.state]));
    await waitFor(
      async () => {
        const p = await previews(md);
        return p.length === 4 && p[0][1] === "ready" && p[1][1] === "ready";
      },
      { timeout: 15000, message: "Markdown previews ready" },
    );
    assert.deepEqual(await previews(md), [
      ["figs/plot one.png", "ready"],
      ["figs/vector.svg", "ready"],
      ["figs/missing.png", "missing"],
      ["figs/escape/out.png", process.platform === "win32" ? "blocked" : "missing"],
    ]);
    const state0 = await invoke(page, "document_state", { document: docId });
    const unchanged = async (label) => {
      const s = await invoke(page, "document_state", { document: docId });
      assert.equal(s.text, NOTES_MD, `${label}: text unchanged`);
      assert.equal(s.info.revision, state0.info.revision, `${label}: revision unchanged`);
      assert.equal(s.info.canUndo, state0.info.canUndo, `${label}: undo unchanged`);
      assert.equal(await md.getAttribute("data-dirty"), "false", `${label}: not dirty`);
    };
    await unchanged("previews shown");
    await md.locator('[data-testid="source-image-preview"]').first().scrollIntoViewIfNeeded();
    await shot("previews-markdown", { themes: ["dark", "light"] });

    const jobsBefore = (await invoke(page, "job_list")).length;
    // Per-image Hide / Show.
    // Monaco keeps its line layer (and so the zones) out of the accessibility
    // tree; the buttons are found by their accessible name attribute.
    await md.locator('[aria-label="Hide preview of figs/plot one.png"]').click();
    await waitFor(async () => (await previews(md))[0]?.[1] === "hidden", { message: "hidden" });
    await shot("previews-hidden-one");
    await unchanged("one preview hidden");
    await md.locator('[aria-label="Show preview of figs/plot one.png"]').click();
    await waitFor(async () => (await previews(md))[0]?.[1] === "ready", { message: "shown again" });

    // Document-wide Hide / Show from the palette (focus the editor first).
    await focusEditor(page, "notes.md", "start");
    await runCommand(page, "Hide Image Previews in Source", /Hide Image Previews in Source/);
    await waitFor(async () => (await previews(md)).length === 0, { message: "all previews hidden" });
    await unchanged("all previews hidden");
    await focusEditor(page, "notes.md", "start");
    await runCommand(page, "Show Image Previews in Source", /Show Image Previews in Source/);
    await waitFor(async () => (await previews(md)).length === 4, { message: "previews back" });
    await unchanged("previews shown again");
    assert.equal((await invoke(page, "job_list")).length, jobsBefore, "showing/hiding previews starts no job");

    // Monaco Find still finds text around the previews.
    await focusEditor(page, "notes.md", "start");
    await page.keyboard.press("Control+F");
    await page.keyboard.insertText("plot");
    await waitFor(async () => /of 4/.test((await md.locator(".find-widget .matchesCount").textContent()) ?? ""), {
      message: "Find counts 4 matches",
    });
    await page.keyboard.press("Escape");
    await unchanged("after Find");

    // An unsaved reference edit previews at once; Undo removes only that edit.
    await focusEditor(page, "notes.md", "end");
    await page.keyboard.insertText("![new](figs/photo.jpg)");
    await waitFor(async () => (await previews(md)).some(([w, s]) => w === "figs/photo.jpg" && s === "ready"), {
      timeout: 15000,
      message: "preview of the unsaved reference",
    });
    assert.equal(await md.getAttribute("data-dirty"), "true");
    const typedText = `${NOTES_MD}![new](figs/photo.jpg)`;
    await waitFor(async () => (await invoke(page, "document_state", { document: docId })).text === typedText, {
      message: "the typed reference reached the document",
    });
    const typed = await invoke(page, "document_state", { document: docId });
    // Undo steps are the user's typing only (the grouper may split it); the
    // previews added none, so undoing the typing returns to the opened text.
    await page.waitForTimeout(1100);
    await md.locator(".view-line").last().click();
    for (let i = 0; i < 6; i++) {
      if ((await invoke(page, "document_state", { document: docId })).text === NOTES_MD) break;
      await page.keyboard.press("Control+Z");
      await page.waitForTimeout(300);
    }
    await waitFor(async () => (await invoke(page, "document_state", { document: docId })).text === NOTES_MD, {
      message: `undo restores the text (typed revision ${typed.info.revision})`,
    });
    assert.equal((await invoke(page, "document_state", { document: docId })).info.canUndo, state0.info.canUndo, "no undo step besides the typing");
    await waitFor(async () => !(await previews(md)).some(([w]) => w === "figs/photo.jpg"), { message: "preview gone" });

    // ----- H: LaTeX (extension search, comment/verbatim/macro ignored) -----
    await openFromTree(page, "paper.tex");
    const tex = editorLocator(page, "paper.tex");
    await tex.locator(".monaco-editor").waitFor({ timeout: 10000 });
    await waitFor(async () => {
      const p = await previews(tex);
      return p.length === 1 && p[0][1] === "ready";
    }, { timeout: 15000, message: "LaTeX preview" });
    assert.deepEqual(await previews(tex), [["figs/photo", "ready"]]);
    assert.equal(await tex.locator('[data-testid="source-image-preview"]').getAttribute("data-path"), "figs/photo.jpg");
    await shot("previews-latex");

    // ----- H: Typst (comment and variable path ignored) -----
    await openFromTree(page, "doc.typ");
    const typ = editorLocator(page, "doc.typ");
    await typ.locator(".monaco-editor").waitFor({ timeout: 10000 });
    await waitFor(async () => {
      const p = await previews(typ);
      return p.length === 1 && p[0][1] === "ready";
    }, { timeout: 15000, message: "Typst preview" });
    assert.deepEqual(await previews(typ), [["figs/plot one.png", "ready"]]);
    // Split mode: the page build fails, the source previews stay.
    await page.locator(".ui-segmented__item", { hasText: "Split" }).first().click();
    await waitFor(
      async () =>
        (await page.locator('[data-testid="page-editor"]:visible').getAttribute("data-failed").catch(() => null)) === "true",
      { timeout: 20000, message: "the Typst build fails" },
    );
    const typSplit = page.locator('[data-testid="source-editor"][data-path="doc.typ"]:visible');
    await waitFor(
      async () => {
        const p = await previews(typSplit);
        return p.length === 1 && p[0][1] === "ready";
      },
      { timeout: 15000, message: "Typst preview with a failed build" },
    );
    await shot("previews-typst-build-failed");

    // ----- Nothing written, nothing built, graph unchanged -----
    for (const f of imageFiles) assert.ok((await readFile(join(root, "figs", f))).equals(before[f]), `${f} unchanged`);
    const after = await graphCounts(page);
    assert.deepEqual(after, baseline, "viewing and previewing leave the graph unchanged");
    const external = requests.filter((u) => u.includes("example.invalid"));
    assert.deepEqual(external, [], "no remote request");

    // ----- A: replace, rename, delete the open image on disk -----
    await tabLocator(page, "figs/plot one.png").click();
    const shown = viewer(page);
    await writeFile(join(root, "figs", "plot one.png"), plotReplaced);
    await waitFor(async () => /^800 × 400 · PNG/.test((await shown.locator('[data-testid="image-facts"]').textContent()) ?? ""), {
      timeout: 15000,
      message: "replaced image shown",
    });
    await rename(join(root, "figs", "plot one.png"), join(root, "figs", "plot two.png"));
    await waitFor(async () => (await shown.getAttribute("data-status")) === "missing", { timeout: 15000, message: "renamed away" });
    await shot("viewer-missing");
    await rename(join(root, "figs", "plot two.png"), join(root, "figs", "plot one.png"));
    await waitFor(async () => (await shown.getAttribute("data-status")) === "ready", { timeout: 15000, message: "renamed back" });
    await rm(join(root, "figs", "plot one.png"));
    await waitFor(async () => (await shown.getAttribute("data-status")) === "missing", { timeout: 15000, message: "deleted" });
    // The Markdown preview of the deleted image follows too.
    await tabLocator(page, "notes.md").click();
    await waitFor(async () => (await previews(md))[0]?.[1] === "missing", { timeout: 15000, message: "preview of deleted image" });

    // ----- Project switch: never another project's image -----
    await writeFile(join(root, "figs", "plot one.png"), plot);
    await tabLocator(page, "figs/plot one.png").click();
    await waitFor(async () => /^1600 × 1200/.test((await shown.locator('[data-testid="image-facts"]').textContent()) ?? ""), {
      timeout: 15000,
      message: "restored",
    });
    await invoke(page, "workspace_open", { path: other });
    await waitFor(
      async () => {
        const v = page.locator('[data-testid="image-viewer"][data-path="figs/plot one.png"]');
        if ((await v.count()) === 0) return true; // tab closed with the project
        const facts = (await v.locator('[data-testid="image-facts"]').textContent().catch(() => "")) ?? "";
        return /^300 × 300/.test(facts) || (await v.getAttribute("data-status")) !== "ready";
      },
      { timeout: 15000, message: "no image of the previous project" },
    );

    const errors = consoleErrors(app).filter((m) => !/example\.invalid|Content Security Policy|net::ERR/.test(m));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await app.close();
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "media");
    await mkdir(target, { recursive: true });
    const names = shots.map((p) => p.split(/[\\/]/).pop());
    for (const stale of (await readdir(target)).filter((f) => f.endsWith(".png") && !names.includes(f))) await rm(join(target, stale));
    for (const [i, p] of shots.entries()) await copyFile(p, join(target, names[i]));
    console.log(`copied ${shots.length} screenshots to ${target}`);
  }
  await rm(base, { recursive: true, force: true }).catch(() => {});
}
