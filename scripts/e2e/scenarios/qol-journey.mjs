// QOL cross-feature journey (Exposition_Editor_Explorer_QOL.md §3: "repeat the
// ordinary create/rename/save/navigation/switch journey with the Graph page
// closed, then inspect the shared state"). Every user action goes through
// visible controls (Files toolbar and context menu, F2, Ctrl+F, Outline rows,
// preview Hide/Show, the page-bar project menu with the native folder dialog,
// the switch decision). IPC is used only for reading state (graph_status,
// graph_snapshot, document_list/document_state, workspace_current). The Graph
// page is never opened.
//  1. Open project "alpha" (native dialog): main.typ with a heading, a labelled
//     theorem and image("fig.png"), plus fig.png. Baseline graph.
//  2. New File (toolbar) with the default name's stem replaced → notes.txt;
//     New File (context menu) → draft.typ; type a heading + theorem unsaved.
//     Outline shows them labelled "Unsaved buffer"; Ctrl+F finds unsaved text;
//     main.typ's inline image preview shows and Hide/Show changes nothing.
//  3. F2-rename the open dirty draft.typ → chapter.typ (buffer, dirty state,
//     document id, selection, undo, tab key; Outline navigates); save; rename
//     notes.txt → notes.md (icon follows).
//  4. Open fig.png in the image tab, rename it in the tree; record what the
//     tab and main.typ's preview do (report only).
//  5. Unsaved edit in alpha; Open Folder → beta: Cancel changes nothing; Save
//     switches. beta's Outline/Problems/previews/graph show nothing of alpha,
//     also a few seconds later. Open Recent → alpha.
//  6. alpha's graph: stable identities across the renames, one resource per
//     path, no resources for the old names, no duplicate theorem, entity delta
//     = the genuinely new constructs, relation statuses kept, synchronized.
// Screenshots: test-results/e2e/qol-journey only (never ui-reference).
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { withNativeDialog } from "../native-dialog.mjs";
import { consoleErrors, dismissNotifications, fileRow, notifications, selectFiles } from "../p1-helpers.mjs";
import { editorLocator, invoke, openFromTree, tabLocator, waitFor } from "../workspace-helpers.mjs";

const ALPHA_MAIN = [
  "#let theorem = (name, body) => block[*Theorem* (#name). #body]",
  "",
  "= Alpha introduction",
  "",
  "Alpha text about groups.",
  "",
  '#theorem("Alpha result")[Every alpha group is a group.] <thm:alpha>',
  "",
  '#image("fig.png", width: 30%)',
  "",
].join("\n");

const BETA_MAIN = ["= Beta overview", "", "Beta text.", "", '#image("beta.png", width: 30%)', ""].join("\n");

const DRAFT_LINES = ["= Draft heading", "", '#theorem("Draft claim")[Every zebracorn is a draft.] <thm:draft>'];
const DRAFT_TEXT = DRAFT_LINES.join("\n");

/** Selection of the Monaco editor of `path` (UTF-16 offsets), from its React state. */
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
          return { start: model.getOffsetAt(s.getStartPosition()), end: model.getOffsetAt(s.getEndPosition()), line: s.startLineNumber };
        }
        hook = hook.next;
      }
      fiber = fiber.return;
    }
    return null;
  }, path);
}

/** Image bytes drawn by the webview's canvas. */
async function canvasPng(page, width, height, hue) {
  const dataUrl = await page.evaluate(
    ([w, h, c]) => {
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      ctx.fillStyle = `hsl(${c} 70% 55%)`;
      ctx.fillRect(0, 0, w, h);
      ctx.fillStyle = "white";
      ctx.font = `${Math.round(h / 5)}px sans-serif`;
      ctx.fillText(`${w}x${h}`, w / 10, h / 2);
      return canvas.toDataURL("image/png");
    },
    [width, height, hue],
  );
  return Buffer.from(dataUrl.split(",")[1], "base64");
}

export default async function qolJourney({ launchApp, artifactsDir, assert }) {
  if (process.platform !== "win32") {
    console.log("  skip qol-journey: the native dialog driver needs Windows");
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-journey-"));
  const alpha = join(base, "alpha");
  const beta = join(base, "beta");
  const dataDir = join(base, "data");
  await mkdir(alpha, { recursive: true });
  await mkdir(beta, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(alpha, "main.typ"), ALPHA_MAIN);
  await writeFile(join(beta, "main.typ"), BETA_MAIN);

  const evidence = { base, steps: [], graph: {}, observed: {} };
  const failures = [];
  /** Record a check; a failure is reported at the end (later steps still run). */
  const check = (step, name, ok, detail = null) => {
    evidence.steps.push({ step, name, ok: Boolean(ok), detail });
    console.log(`  ${ok ? "ok  " : "FAIL"} [${step}] ${name}${detail === null ? "" : ` ${JSON.stringify(detail)}`}`);
    if (!ok) failures.push(`[${step}] ${name}: ${JSON.stringify(detail)}`);
  };
  const observe = (key, value) => {
    evidence.observed[key] = value;
    console.log(`  note [${key}] ${JSON.stringify(value)}`);
  };

  const app = await launchApp({ dataDir });
  const { page } = app;
  const pid = app.child.pid;
  const shot = async (name) => {
    await page.waitForTimeout(300);
    await page.screenshot({ path: join(artifactsDir, `${name}.png`) }).catch(() => {});
  };

  // ---- read-only state helpers ----
  const status = () => invoke(page, "graph_status");
  const snapshot = async () => {
    const s = await invoke(page, "graph_snapshot", { query: { kinds: null, text: null, around: null, includeInactive: true, limit: 2000 } });
    assert.ok(!s.truncated, "graph snapshot complete");
    return s;
  };
  const synchronized = (label, timeout = 60_000) =>
    waitFor(
      async () => {
        const s = await status();
        return s.state.kind === "attached" && s.freshness.state === "synchronized" && s.freshness.pending === 0 ? s : null;
      },
      { timeout, message: `graph synchronized (${label})` },
    );
  const liveResources = (snap, key) => snap.entities.filter((e) => e.kind === "resource" && e.key === key && e.lifecycle !== "tombstoned");
  const anyResources = (snap, key) => snap.entities.filter((e) => e.kind === "resource" && e.key === key);
  const waitResource = (key, timeout = 30_000) =>
    waitFor(async () => liveResources(await snapshot(), key)[0] ?? null, { timeout, message: `resource ${key}` });
  const kindCounts = (snap) => {
    const counts = {};
    for (const e of snap.entities) if (e.lifecycle === "active") counts[e.kind] = (counts[e.kind] ?? 0) + 1;
    return counts;
  };
  const docByPath = async (path) => (await invoke(page, "document_list")).find((d) => d.path === path) ?? null;
  const docState = async (path) => {
    const doc = await docByPath(path);
    return doc ? invoke(page, "document_state", { document: doc.id }) : null;
  };
  const tabKeys = () => page.locator("[data-editor-key]").evaluateAll((els) => [...new Set(els.map((e) => e.getAttribute("data-editor-key")))]);
  const treeIcon = (path) => fileRow(page, path).locator("[data-file-type]").first().getAttribute("data-file-type");
  const tabIcon = (path) => tabLocator(page, path).locator("[data-file-type]").first().getAttribute("data-file-type");
  const outline = page.locator('[data-testid="outline-view"]');
  const outlineRows = () =>
    page.$$eval('[data-testid="outline-tree"] [role="row"]', (els) =>
      els.map((e) => `${"  ".repeat(Number(e.getAttribute("aria-level") ?? 1) - 1)}${e.querySelector(".ui-tree-item__title")?.textContent ?? ""}`),
    );
  const outlineRow = (title) =>
    page.locator('[data-testid="outline-tree"] [role="row"]', { has: page.locator(".ui-tree-item__title", { hasText: title }) }).first();
  const previews = (path) =>
    editorLocator(page, path)
      .locator('[data-testid="source-image-preview"]')
      .evaluateAll((els) => els.map((e) => [e.dataset.written, e.dataset.state]));
  const nameField = async () => {
    const input = page.getByTestId("file-name-dialog").getByRole("textbox");
    await input.waitFor();
    return input;
  };
  const typeName = async (name) => {
    const input = await nameField();
    await input.press("Control+A");
    await page.keyboard.type(name);
    await page.keyboard.press("Enter");
  };
  const renamed = (from, to) =>
    waitFor(async () => (await notifications(page)).find((n) => n.title === `Renamed “${from}” to “${to}”`) ?? null, {
      timeout: 15_000,
      message: `rename notification ${from} → ${to}`,
    });
  const focusEnd = async (path) => {
    await editorLocator(page, path).locator(".view-line").first().click();
    await page.keyboard.press("Control+End");
  };

  // ---- project menu helpers (page bar) ----
  const label = async () => (await page.getByTestId("pagebar-project").innerText()).trim();
  const decision = page.getByTestId("project-switch-decision");
  const currentRoot = async () => (await invoke(page, "workspace_current"))?.root ?? null;
  const isAt = async (root) => ((await currentRoot()) ?? "").toLowerCase() === root.toLowerCase();
  // graph_status reports the root as a Windows verbatim path (\\?\C:\...).
  const samePath = (a, b) => (a ?? "").replace(/^\\\\\?\\/, "").toLowerCase() === b.toLowerCase();
  const waitAt = async (root, name) => {
    await waitFor(() => isAt(root), { timeout: 20_000, message: `${name} open` });
    await waitFor(async () => (await label()) === name, { timeout: 10_000, message: `page bar shows ${name}` });
    await fileRow(page, "main.typ").waitFor({ timeout: 15_000 });
  };
  const menuItem = async (name) => {
    await page.getByTestId("pagebar-project").click();
    const item = page.getByRole("menuitem", { name });
    await item.waitFor({ timeout: 5_000 });
    return item;
  };
  const openViaDialog = (path) =>
    withNativeDialog({ title: "Open Folder", path, pid }, async () => (await menuItem(/^Open Folder/)).click());
  const openRecent = async (name) => {
    await (await menuItem(/^Open Recent/)).click();
    const item = page.getByRole("menu", { name: "Open Recent" }).getByRole("menuitem", { name: new RegExp(`^${name}`) });
    await item.waitFor({ timeout: 5_000 });
    await item.click();
  };

  try {
    await page.setViewportSize({ width: 1440, height: 900 }).catch(() => {});
    await page.getByTestId("workbench").waitFor({ timeout: 15_000 });
    // Fixture images drawn by the webview's canvas (setup, before any project opens).
    await writeFile(join(alpha, "fig.png"), await canvasPng(page, 320, 200, 210));
    await writeFile(join(beta, "beta.png"), await canvasPng(page, 200, 200, 30));

    // ======== 1. open alpha, baseline ===============================================
    const opened = await openViaDialog(alpha);
    check(1, "native dialog opened alpha", opened.found && opened.closed, opened);
    await waitAt(alpha, "alpha");
    await synchronized("alpha opened");
    const mainRes = await waitResource("main.typ");
    const figRes = await waitResource("fig.png");
    const snap0 = await snapshot();
    const status0 = await status();
    const theorems0 = snap0.entities.filter((e) => e.kind === "theorem" && e.lifecycle === "active");
    evidence.graph.baseline = {
      revision: status0.revision,
      entities: status0.entities,
      relations: status0.relations,
      kinds: kindCounts(snap0),
      mainTyp: mainRes.id,
      figPng: figRes.id,
      theorems: theorems0.map((e) => ({ id: e.id, key: e.key, label: e.label })),
      relationList: snap0.relations.map((r) => ({ id: r.id, kind: r.kind, from: r.from, to: r.to, status: r.status })),
    };
    observe("baseline", { entities: status0.entities, relations: status0.relations, kinds: kindCounts(snap0), theorems: evidence.graph.baseline.theorems });
    check(1, "alpha theorem indexed once", theorems0.length === 1, evidence.graph.baseline.theorems);
    const alphaThm = theorems0[0];

    // ======== 2. New File ×2, unsaved structure, Outline, Find, previews =================
    // (a) toolbar → suggested "Untitled.txt" with the stem selected; type the stem.
    await page.getByTestId("files-new-file").click();
    const input = await nameField();
    await waitFor(async () => (await input.inputValue()) === "Untitled.txt", { message: "suggested name" });
    const sel = await waitFor(async () => {
      const s = await input.evaluate((el) => [el.selectionStart, el.selectionEnd]);
      return s[0] === 0 && s[1] === 8 ? s : null;
    }, { message: "stem selected" }).catch(() => null);
    check(2, "New File suggests Untitled.txt with the stem selected", Boolean(sel), sel);
    await page.keyboard.type("notes");
    await page.keyboard.press("Enter");
    await waitFor(async () => existsSync(join(alpha, "notes.txt")), { message: "notes.txt created" });
    await editorLocator(page, "notes.txt").locator(".monaco-editor").waitFor({ timeout: 15_000 });
    check(2, "notes.txt created from the default name and opened", existsSync(join(alpha, "notes.txt")) && (await tabKeys()).includes("file:notes.txt"));
    check(2, "notes.txt icon is text", (await treeIcon("notes.txt")) === "text", await treeIcon("notes.txt"));

    // (b) context menu on a tree row → New File… → draft.typ (in the project folder).
    await fileRow(page, "main.typ").locator(".ui-tree-item__title").click({ button: "right" });
    const contextMenu = page.getByTestId("files-context-menu");
    await contextMenu.waitFor({ timeout: 5_000 });
    await contextMenu.getByRole("menuitem", { name: /^New File/ }).click();
    await typeName("draft.typ");
    await waitFor(async () => existsSync(join(alpha, "draft.typ")), { message: "draft.typ created" });
    const draftEditor = editorLocator(page, "draft.typ");
    await draftEditor.locator(".monaco-editor").waitFor({ timeout: 15_000 });
    const draftDocId = await waitFor(() => draftEditor.getAttribute("data-document-id"), { message: "draft document id" });
    check(2, "draft.typ created in the project folder via the context menu", existsSync(join(alpha, "draft.typ")));
    // The graph indexes the new (empty) file before anything is typed or renamed.
    const draftRes = await waitResource("draft.typ");
    const notesRes = await waitResource("notes.txt");
    evidence.graph.created = { draftTyp: draftRes.id, notesTxt: notesRes.id };

    // Type a heading and a theorem (insertText: no auto-closing brackets).
    await focusEnd("draft.typ");
    for (const [i, line] of DRAFT_LINES.entries()) {
      if (i > 0) await page.keyboard.press("Enter");
      if (line) await page.keyboard.insertText(line);
    }
    await waitFor(async () => (await docState("draft.typ"))?.text === DRAFT_TEXT, { message: `draft text (${JSON.stringify((await docState("draft.typ"))?.text)})` });
    check(2, "draft.typ dirty", (await tabLocator(page, "draft.typ").getAttribute("data-dirty")) === "true");
    check(2, "draft.typ unsaved on disk (still empty)", (await readFile(join(alpha, "draft.typ"), "utf8")) === "");

    // Outline (auxiliary panel tab) shows the unsaved structure.
    await page.getByRole("tablist", { name: "Auxiliary panel views" }).getByRole("tab", { name: "Outline" }).click();
    await outline.waitFor({ timeout: 10_000 });
    const draftRows = await waitFor(async () => {
      const rows = await outlineRows();
      return (await outline.getAttribute("data-path")) === "draft.typ" && rows.some((r) => r.includes("Draft claim")) ? rows : null;
    }, { timeout: 15_000, message: "draft outline" }).catch(async () => outlineRows());
    const unsavedBadge = await page.locator('[data-testid="outline-unsaved"]').textContent({ timeout: 5_000 }).catch(() => null);
    check(2, "Outline lists the unsaved heading and theorem", JSON.stringify(draftRows) === JSON.stringify(["Draft heading", "  Theorem (Draft claim)"]), draftRows);
    check(2, "Outline is labelled Unsaved buffer", /^Unsaved buffer \(revision \d+\)$/.test(unsavedBadge ?? ""), unsavedBadge);
    await shot("02-draft-outline-unsaved");

    // Ctrl+F finds a word that exists only in the unsaved buffer.
    const beforeFind = await docState("draft.typ");
    await focusEnd("draft.typ");
    await page.keyboard.press("Control+F");
    const findWidget = draftEditor.locator(".find-widget.visible");
    await findWidget.waitFor({ timeout: 5_000 });
    await page.keyboard.insertText("zebracorn");
    const count = await waitFor(async () => {
      const t = (await draftEditor.locator(".find-widget .matchesCount").textContent()) ?? "";
      return /1 of 1/.test(t) ? t : null;
    }, { message: "Find counts the unsaved word" }).catch(async () => draftEditor.locator(".find-widget .matchesCount").textContent());
    check(2, "Find (Ctrl+F) finds the unsaved word", /1 of 1/.test(count ?? ""), count);
    await shot("02-find-unsaved");
    await page.keyboard.press("Escape");
    await findWidget.waitFor({ state: "hidden", timeout: 5_000 }).catch(() => {});
    const afterFind = await docState("draft.typ");
    check(2, "Find changed no text or revision", afterFind.text === beforeFind.text && afterFind.info.revision === beforeFind.info.revision);

    // Inline image preview in main.typ; Hide/Show changes nothing.
    await openFromTree(page, "main.typ");
    const mainEditor = editorLocator(page, "main.typ");
    await mainEditor.locator(".monaco-editor").waitFor({ timeout: 10_000 });
    const mainPreviews = await waitFor(async () => {
      const p = await previews("main.typ");
      return p.length === 1 && p[0][1] === "ready" ? p : null;
    }, { timeout: 15_000, message: "main.typ preview" }).catch(() => previews("main.typ"));
    check(2, "main.typ shows the fig.png preview", JSON.stringify(mainPreviews) === JSON.stringify([["fig.png", "ready"]]), mainPreviews);
    await shot("02-main-preview");
    const mainBefore = await docState("main.typ");
    await mainEditor.locator('[aria-label="Hide preview of fig.png"]').click();
    const hidden = await waitFor(async () => (await previews("main.typ"))[0]?.[1] === "hidden", { message: "preview hidden" }).catch(() => false);
    const mainHidden = await docState("main.typ");
    check(2, "Hide preview: hidden, text/revision/undo/dirty unchanged", hidden &&
      mainHidden.text === mainBefore.text &&
      mainHidden.info.revision === mainBefore.info.revision &&
      mainHidden.info.canUndo === mainBefore.info.canUndo &&
      (await tabLocator(page, "main.typ").getAttribute("data-dirty")) !== "true", { hidden, revision: [mainBefore.info.revision, mainHidden.info.revision] });
    await mainEditor.locator('[aria-label="Show preview of fig.png"]').click();
    await waitFor(async () => (await previews("main.typ"))[0]?.[1] === "ready", { message: "preview shown again" }).catch(() => {});
    check(2, "Show preview again leaves main.typ unchanged", (await docState("main.typ")).info.revision === mainBefore.info.revision);

    // ======== 3. rename the open dirty draft.typ → chapter.typ ==========================
    await tabLocator(page, "draft.typ").click();
    await draftEditor.locator(".monaco-editor").waitFor();
    const selBefore = await monacoSelection(page, "draft.typ");
    const tabsBefore = await tabKeys();
    const draftBefore = await docState("draft.typ");
    await selectFiles(page, ["draft.typ"]);
    await page.keyboard.press("F2");
    const renameInput = await nameField();
    await waitFor(async () => (await renameInput.inputValue()) === "draft.typ", { message: "rename shows the current name" });
    await typeName("chapter.typ");
    await renamed("draft.typ", "chapter.typ");
    const chapterEditor = editorLocator(page, "chapter.typ");
    await chapterEditor.locator(".monaco-editor").waitFor({ timeout: 10_000 });
    const tabsAfter = await tabKeys();
    const chapterState = await docState("chapter.typ");
    check(3, "renamed on disk; the unsaved text was not written", !existsSync(join(alpha, "draft.typ")) && (await readFile(join(alpha, "chapter.typ"), "utf8")) === "");
    check(3, "tab re-keyed in place", JSON.stringify(tabsAfter) === JSON.stringify(tabsBefore.map((k) => (k === "file:draft.typ" ? "file:chapter.typ" : k))), { tabsBefore, tabsAfter });
    check(3, "same document, buffer kept, still dirty", (await chapterEditor.getAttribute("data-document-id")) === draftDocId &&
      chapterState?.text === DRAFT_TEXT &&
      (await tabLocator(page, "chapter.typ").getAttribute("data-dirty")) === "true", { doc: await chapterEditor.getAttribute("data-document-id"), draftDocId });
    const selAfter = await monacoSelection(page, "chapter.typ");
    check(3, "selection kept", JSON.stringify(selAfter) === JSON.stringify(selBefore), { selBefore, selAfter });
    check(3, "undo history kept (canUndo, same revision)", chapterState.info.canUndo === true && chapterState.info.revision === draftBefore.info.revision, chapterState.info);
    check(3, "icon is Typst in tree and tab", (await treeIcon("chapter.typ")) === "typst" && (await tabIcon("chapter.typ")) === "typst", [await treeIcon("chapter.typ"), await tabIcon("chapter.typ")]);
    // Undo one step and redo it (shared history across the rename).
    await focusEnd("chapter.typ");
    await page.keyboard.press("Control+Z");
    const undone = await waitFor(async () => {
      const t = (await docState("chapter.typ")).text;
      return t !== DRAFT_TEXT ? t : null;
    }, { message: "undo after rename" }).catch(() => null);
    await page.keyboard.press("Control+Y");
    const redone = await waitFor(async () => (await docState("chapter.typ")).text === DRAFT_TEXT, { message: "redo after rename" }).catch(() => false);
    check(3, "undo then redo across the rename", undone !== null && redone, { undone });
    // Outline follows the renamed document and navigates to the theorem.
    const chapterRows = await waitFor(async () => {
      const rows = await outlineRows();
      return (await outline.getAttribute("data-path")) === "chapter.typ" && rows.some((r) => r.includes("Draft claim")) ? rows : null;
    }, { timeout: 15_000, message: "chapter outline" }).catch(async () => ({ path: await outline.getAttribute("data-path"), rows: await outlineRows() }));
    check(3, "Outline follows the rename", Array.isArray(chapterRows), chapterRows);
    await focusEnd("chapter.typ");
    await page.keyboard.press("Control+Home");
    await outlineRow("Theorem (Draft claim)").click();
    const thmAt = DRAFT_TEXT.indexOf('#theorem("Draft claim")');
    const nav = await waitFor(async () => {
      const s = await monacoSelection(page, "chapter.typ");
      return s && s.start === thmAt ? s : null;
    }, { message: "outline navigation" }).catch(() => monacoSelection(page, "chapter.typ"));
    check(3, "Outline click puts the caret on the theorem's line", nav?.start === thmAt && nav?.line === 3, nav);
    await shot("03-renamed-dirty-outline");
    // Save.
    await focusEnd("chapter.typ");
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await readFile(join(alpha, "chapter.typ"), "utf8")) === DRAFT_TEXT, { message: "chapter.typ saved" });
    await waitFor(async () => (await tabLocator(page, "chapter.typ").getAttribute("data-dirty")) !== "true", { message: "clean after save" });
    check(3, "saved to the new path only", !existsSync(join(alpha, "draft.typ")) && (await readFile(join(alpha, "chapter.typ"), "utf8")) === DRAFT_TEXT);
    await waitFor(async () => (await page.locator('[data-testid="outline-unsaved"]').count()) === 0, { message: "unsaved label gone" }).catch(() => {});
    check(3, "Outline no longer says Unsaved buffer after save", (await page.locator('[data-testid="outline-unsaved"]').count()) === 0);
    await dismissNotifications(page);

    // notes.txt → notes.md (extension change).
    await selectFiles(page, ["notes.txt"]);
    await page.keyboard.press("F2");
    await typeName("notes.md");
    await renamed("notes.txt", "notes.md");
    await waitFor(async () => (await tabKeys()).includes("file:notes.md"), { message: "notes tab re-keyed" }).catch(() => {});
    const notesIcons = [await treeIcon("notes.md").catch(() => null), await tabIcon("notes.md").catch(() => null)];
    check(3, "notes.txt → notes.md: file moved, tab re-keyed, icon markdown", existsSync(join(alpha, "notes.md")) && !existsSync(join(alpha, "notes.txt")) &&
      (await tabKeys()).includes("file:notes.md") && !(await tabKeys()).includes("file:notes.txt") && notesIcons.every((i) => i === "markdown"), { notesIcons, tabs: await tabKeys() });
    await dismissNotifications(page);

    // ======== 4. image tab, then rename the image in the tree (report) ===================
    await openFromTree(page, "fig.png");
    const viewer = page.locator('[data-testid="image-viewer"][data-path="fig.png"]');
    await viewer.waitFor({ timeout: 10_000 });
    await waitFor(async () => (await viewer.getAttribute("data-status")) !== "loading", { message: "fig.png loaded" });
    check(4, "fig.png opens in the image tab", (await viewer.getAttribute("data-status")) === "ready", await viewer.getAttribute("data-status"));
    await selectFiles(page, ["fig.png"]);
    await page.keyboard.press("F2");
    await nameField();
    const refsListed = await page.getByTestId("rename-references").textContent({ timeout: 3_000 }).catch(() => null);
    await typeName("figure.png");
    const figNote = await renamed("fig.png", "figure.png").catch(() => null);
    await page.waitForTimeout(2_500);
    const viewers = await page.locator('[data-testid="image-viewer"]').evaluateAll((els) =>
      els.map((e) => ({ path: e.getAttribute("data-path"), status: e.getAttribute("data-status"), text: (e.textContent ?? "").replace(/\s+/g, " ").slice(0, 120) })),
    );
    const tabsAfterFig = await tabKeys();
    await shot("04-image-tab-after-rename");
    await tabLocator(page, "main.typ").click();
    await page.waitForTimeout(2_000);
    const mainPreviewAfter = await previews("main.typ");
    await shot("04-main-preview-after-rename");
    observe("image-rename", { referencesInDialog: refsListed, notification: figNote, viewers, tabs: tabsAfterFig, mainPreview: mainPreviewAfter });
    check(4, "figure.png on disk", existsSync(join(alpha, "figure.png")) && !existsSync(join(alpha, "fig.png")));
    await dismissNotifications(page);

    // ======== 5. unsaved edit; switch to beta (Cancel, then Save); back ==================
    await focusEnd("main.typ");
    await page.keyboard.insertText("\nUnsaved alpha remark.");
    await waitFor(async () => (await tabLocator(page, "main.typ").getAttribute("data-dirty")) === "true", { message: "main.typ dirty" });
    const editedMain = await waitFor(async () => {
      const t = (await docState("main.typ")).text;
      return t.endsWith("Unsaved alpha remark.") ? t : null;
    }, { message: "the edit reached the document" }).catch(async () => (await docState("main.typ")).text);
    check(5, "one unsaved edit", editedMain.endsWith("Unsaved alpha remark."), editedMain.slice(-40));
    await page.getByRole("tab", { name: "Problems" }).click().catch(() => {});
    const alphaProblems = await page.locator('[data-testid="problem"]').allTextContents().catch(() => []);
    observe("alpha-problems-before-switch", alphaProblems);
    await page.getByRole("tablist", { name: "Auxiliary panel views" }).getByRole("tab", { name: "Outline" }).click();

    const dialog1 = await openViaDialog(beta);
    await decision.waitFor({ timeout: 10_000 });
    const decisionTitle = await decision.getByRole("heading").innerText();
    const decisionButtons = await decision.getByRole("button").allInnerTexts();
    await shot("05-decision");
    await decision.getByRole("button", { name: "Cancel" }).click();
    await decision.waitFor({ state: "detached" });
    await page.waitForTimeout(800);
    const afterCancel = {
      dialogClosed: dialog1.closed,
      stillAlpha: await isAt(alpha),
      label: await label(),
      bufferKept: (await docState("main.typ")).text === editedMain,
      dirty: await tabLocator(page, "main.typ").getAttribute("data-dirty"),
      diskUnchanged: !(await readFile(join(alpha, "main.typ"), "utf8")).includes("Unsaved alpha remark."),
    };
    check(5, "Cancel changes nothing", afterCancel.dialogClosed && afterCancel.stillAlpha && afterCancel.label === "alpha" &&
      afterCancel.bufferKept && afterCancel.dirty === "true" && afterCancel.diskUnchanged, { decisionTitle, decisionButtons, afterCancel });

    // Record what the Outline, previews, image tabs and Problems show once beta is the project.
    await page.evaluate(() => {
      window.__journey = { rows: new Set(), outlinePaths: new Set(), previews: new Set(), viewers: new Set(), problems: new Set(), since: 0 };
      const read = () => {
        const j = window.__journey;
        const projectLabel = document.querySelector('[data-testid="pagebar-project"]')?.textContent?.trim() ?? "";
        if (projectLabel !== "beta") return;
        if (!j.since) j.since = performance.now();
        for (const r of document.querySelectorAll('[data-testid="outline-tree"] [role="row"] .ui-tree-item__title')) j.rows.add(r.textContent);
        for (const o of document.querySelectorAll('[data-testid="outline-view"]')) j.outlinePaths.add(o.getAttribute("data-path"));
        for (const p of document.querySelectorAll('[data-testid="source-image-preview"]')) j.previews.add(`${p.dataset.written}|${p.dataset.state}`);
        for (const v of document.querySelectorAll('[data-testid="image-viewer"]')) j.viewers.add(v.getAttribute("data-path"));
        for (const p of document.querySelectorAll('[data-testid="problem"]')) j.problems.add(p.textContent);
      };
      new MutationObserver(read).observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true });
    });
    const dialog2 = await openViaDialog(beta);
    await decision.waitFor({ timeout: 10_000 });
    await decision.getByRole("button", { name: /^Save( All)?$/ }).click();
    await waitAt(beta, "beta");
    check(5, "Save switched to beta and wrote alpha's edit", dialog2.closed && (await readFile(join(alpha, "main.typ"), "utf8")) === editedMain);
    const betaStatus = await synchronized("beta opened");
    check(5, "graph is beta's", samePath(betaStatus.root, beta), betaStatus.root);
    const betaSnap = await snapshot();
    const alphaKeys = ["chapter.typ", "notes.md", "figure.png", "fig.png", "draft.typ", "notes.txt"];
    const leaked = betaSnap.entities.filter((e) => alphaKeys.includes(e.key) || /Alpha result|Draft claim|thm:alpha|thm:draft/.test(`${e.label} ${e.key}`));
    check(5, "no alpha entity in beta's graph", leaked.length === 0, leaked.map((e) => [e.kind, e.key, e.label]));
    // Open beta's own main.typ; its outline and preview.
    await openFromTree(page, "main.typ");
    await editorLocator(page, "main.typ").locator(".monaco-editor").waitFor({ timeout: 10_000 });
    const betaRows = await waitFor(async () => {
      const rows = await outlineRows();
      return (await outline.getAttribute("data-path")) === "main.typ" && rows.length > 0 ? rows : null;
    }, { timeout: 15_000, message: "beta outline" }).catch(() => outlineRows());
    const betaPreviews = await waitFor(async () => {
      const p = await previews("main.typ");
      return p.length && p.every(([, s]) => s !== "loading" && s !== "idle") ? p : null;
    }, { timeout: 15_000, message: "beta previews" }).catch(() => previews("main.typ"));
    check(5, "beta's outline is its own", JSON.stringify(betaRows) === JSON.stringify(["Beta overview"]), betaRows);
    check(5, "beta's preview is its own", JSON.stringify(betaPreviews) === JSON.stringify([["beta.png", "ready"]]), betaPreviews);
    await page.getByRole("tab", { name: "Problems" }).click().catch(() => {});
    await page.waitForTimeout(500);
    const betaProblems = await page.locator('[data-testid="problem"]').allTextContents().catch(() => []);
    await shot("05-beta");
    // A few more seconds for any late answer from alpha.
    await page.waitForTimeout(5_000);
    const seen = await page.evaluate(() => {
      const j = window.__journey;
      return { recording: j.since > 0, rows: [...j.rows], outlinePaths: [...j.outlinePaths], previews: [...j.previews], viewers: [...j.viewers], problems: [...j.problems] };
    });
    observe("beta-seen-after-switch", { ...seen, betaProblems });
    const alphaish = /Alpha|Draft|fig\.png|figure\.png|chapter\.typ|notes\.md|zebracorn/;
    check(5, "nothing of alpha shown in beta (outline rows/paths, previews, image tabs, problems)", seen.recording &&
      ![...seen.rows, ...seen.outlinePaths, ...seen.previews, ...seen.viewers, ...seen.problems, ...betaProblems].some((s) => alphaish.test(s ?? "")), seen);
    check(5, "alpha's tabs closed with the switch", !(await tabKeys()).some((k) => /chapter\.typ|notes\.md|fig\.png|figure\.png/.test(k)), await tabKeys());
    await page.getByRole("tablist", { name: "Auxiliary panel views" }).getByRole("tab", { name: "Outline" }).click().catch(() => {});

    await openRecent("alpha");
    const backDecision = await decision.waitFor({ timeout: 2_000 }).then(() => true, () => false);
    if (backDecision) await decision.getByRole("button", { name: "Cancel" }).click();
    check(5, "back to alpha asks nothing (beta is clean)", !backDecision);
    await waitAt(alpha, "alpha");

    // ======== 6. alpha's graph after the journey ========================================
    const status6 = await synchronized("alpha reopened");
    check(6, "graph is alpha's and synchronized", samePath(status6.root, alpha) && status6.freshness.state === "synchronized", { root: status6.root, freshness: status6.freshness });
    await page.waitForTimeout(2_500); // let late watcher events land, then read again
    await synchronized("alpha settled");
    const snap6 = await snapshot();
    const status6b = await status();
    const live = (key) => liveResources(snap6, key);
    const one = (key) => live(key).length === 1;
    check(6, "main.typ keeps its resource id", one("main.typ") && live("main.typ")[0].id === mainRes.id, live("main.typ").map((e) => e.id));
    check(6, "chapter.typ resource id = draft.typ id (rename kept identity)", one("chapter.typ") && live("chapter.typ")[0].id === draftRes.id, { draft: draftRes.id, chapter: live("chapter.typ").map((e) => e.id) });
    check(6, "notes.md resource id = notes.txt id", one("notes.md") && live("notes.md")[0].id === notesRes.id, { notesTxt: notesRes.id, notesMd: live("notes.md").map((e) => e.id) });
    check(6, "figure.png resource id = fig.png id", one("figure.png") && live("figure.png")[0].id === figRes.id, { fig: figRes.id, figure: live("figure.png").map((e) => e.id) });
    const oldNames = Object.fromEntries(["draft.typ", "notes.txt", "fig.png"].map((k) => [k, anyResources(snap6, k).map((e) => [e.id, e.lifecycle])]));
    check(6, "no resources (live or tombstoned) for the old names", Object.values(oldNames).every((l) => l.length === 0), oldNames);
    const byKey = {};
    for (const e of snap6.entities.filter((x) => x.kind === "resource" && x.lifecycle !== "tombstoned")) (byKey[e.key] ??= []).push(e.id);
    const dupPaths = Object.entries(byKey).filter(([, ids]) => ids.length > 1);
    check(6, "exactly one live resource per path", dupPaths.length === 0, dupPaths);
    const theorems6 = snap6.entities.filter((e) => e.kind === "theorem" && e.lifecycle === "active").map((e) => ({ id: e.id, key: e.key, label: e.label }));
    const alphaThms = theorems6.filter((t) => t.id === alphaThm.id);
    const draftThms = theorems6.filter((t) => /Draft claim|thm:draft/.test(`${t.label} ${t.key}`));
    check(6, "alpha's theorem keeps its id, no duplicate", alphaThms.length === 1 && theorems6.filter((t) => /Alpha result|thm:alpha/.test(`${t.label} ${t.key}`)).length === 1, theorems6);
    check(6, "the saved chapter theorem exists once", draftThms.length === 1, theorems6);
    const kinds0 = kindCounts(snap0);
    const kinds6 = kindCounts(snap6);
    const delta = {};
    for (const k of new Set([...Object.keys(kinds0), ...Object.keys(kinds6)])) {
      const d = (kinds6[k] ?? 0) - (kinds0[k] ?? 0);
      if (d) delta[k] = d;
    }
    check(6, "entity delta = 2 resources (notes.md, chapter.typ) + 1 theorem", JSON.stringify(delta) === JSON.stringify({ resource: 2, theorem: 1 }) ||
      JSON.stringify(delta) === JSON.stringify({ theorem: 1, resource: 2 }), { delta, before: kinds0, after: kinds6 });
    const baselineIds = new Set(snap0.entities.map((e) => e.id));
    const newEntities = snap6.entities.filter((e) => !baselineIds.has(e.id)).map((e) => ({ id: e.id, kind: e.kind, key: e.key, label: e.label, lifecycle: e.lifecycle }));
    check(6, "no outline/page/preview/image-tab entities", !newEntities.some((e) => /page|outline|preview|heading|Draft heading|Alpha introduction|viewer/i.test(`${e.kind} ${e.key} ${e.label}`)), newEntities);
    const rel6 = new Map(snap6.relations.map((r) => [r.id, r]));
    const relChanges = evidence.graph.baseline.relationList.map((r) => {
      const now = rel6.get(r.id);
      return { id: r.id, kind: r.kind, before: r.status, after: now?.status ?? "(gone)", endpoints: now ? (now.from === r.from && now.to === r.to ? "same" : "changed") : "-" };
    });
    observe("relations", { before: snap0.relations.length, after: snap6.relations.length, changes: relChanges.filter((c) => c.before !== c.after || c.endpoints !== "same") });
    check(6, "relation statuses of surviving relations unchanged", relChanges.filter((c) => c.after !== "(gone)").every((c) => c.before === c.after), relChanges);
    evidence.graph.final = { revision: status6b.revision, entities: status6b.entities, relations: status6b.relations, kinds: kinds6, theorems: theorems6, newEntities, relations6: snap6.relations.map((r) => ({ id: r.id, kind: r.kind, from: r.from, to: r.to, status: r.status })), entities6: snap6.entities.map((e) => ({ id: e.id, kind: e.kind, key: e.key, label: e.label, lifecycle: e.lifecycle })) };
    observe("final", { entities: status6b.entities, relations: status6b.relations, delta, newEntities });

    const errors = consoleErrors(app);
    observe("console-errors", errors.slice(0, 20));
    check("all", "no console errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  assert.deepEqual(failures, [], `${failures.length} journey check(s) failed`);
}
