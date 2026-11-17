// QOL E/G — New File, Rename (F2) and OS drag-and-drop import in the real
// app, through visible controls, on disposable temporary projects. The Graph
// page is never opened: the graph is read through its commands only.
//  1. New File (toolbar): "Untitled.txt" suggested with its stem selected;
//     Enter creates it empty and focuses it in the editor.
//  2. New File with explicit names (.tex/.typ/.md/.py/.lean, a leading-dot
//     name) in a selected folder (Ctrl+N): created exactly as typed.
//  3. A colliding name keeps the dialog, the typed name and the existing file.
//  4. Rename an open DIRTY file with an extension change (F2 in the tree):
//     the disk bytes move unchanged, the tab keeps its place/dirty state/
//     document id/caret, undo still works, saving writes the new path only;
//     the graph keeps the resource's identity (no duplicate, no tombstone).
//     F2 in Monaco never opens the explorer's rename.
//  5. Case-only rename; a rename collision keeps everything.
//  6. Rename of an included chapter lists the manuscript that still uses it.
//  7. Injected graph-recording failure after a rename: the notification says
//     the graph has not recorded it; it is reconciled with the same identity.
//  8. Real OS drops (OLE DoDragDrop from a helper window, CF_HDROP file list)
//     onto a folder row and onto the tree (project folder): highlight seen,
//     copies made, originals untouched, a colliding name kept both; a link is
//     refused with a partial report; a larger batch shows progress and can be
//     canceled; Retry never duplicates; a project switch during an import
//     never redirects it.
//  9. What the native drop handler means for other drops (text dropped on
//     Monaco; an in-editor drag of selected text) — recorded as evidence.
// Screens → ui-reference/qol-files/.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { copyFile, mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

import { REPO_ROOT } from "../app.mjs";
import { consoleErrors, dismissNotifications, expandTo, fileRow, notifications, pressNotificationAction, selectFiles, settle, waitForNotification, platformSkip } from "../p1-helpers.mjs";
import { editorLocator, focusEditor, invoke, makeProject, openInEditor, openWorkspace, tabLocator, waitFor } from "../workspace-helpers.mjs";

const HELPER = join(REPO_ROOT, "scripts", "e2e", "os-drag.ps1");

/** Run the OS drag helper (asynchronously, so the page can be observed while it holds). */
function osDrag(args) {
  const q = (s) => `'${String(s).replace(/'/g, "''")}'`;
  const parts = [`& ${q(HELPER)}`, `-AppPid ${args.pid}`, `-X ${Math.round(args.x)}`, `-Y ${Math.round(args.y)}`, `-Mode ${args.mode ?? "files"}`];
  if (args.paths?.length) parts.push(`-Paths @(${args.paths.map(q).join(",")})`);
  if (args.text) parts.push(`-Text ${q(args.text)}`);
  if (args.x2 !== undefined) parts.push(`-X2 ${Math.round(args.x2)} -Y2 ${Math.round(args.y2)}`);
  parts.push(`-HoldMs ${args.holdMs ?? 1500}`);
  return new Promise((resolve, reject) => {
    const child = spawn("powershell", ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", parts.join(" ")], { stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    child.on("exit", (code) => {
      const line = out.trim().split(/\r?\n/).pop() ?? "";
      let result;
      try {
        result = JSON.parse(line);
      } catch {
        reject(new Error(`os-drag failed (${code}): ${out} ${err}`));
        return;
      }
      // The helper never drags onto another application's window.
      if (result.ok) resolve(result);
      else reject(new Error(`os-drag refused: ${line}`));
    });
  });
}

/** Physical client coordinates of an element's center (the window's client area is the webview). */
async function clientPoint(page, locator) {
  await locator.scrollIntoViewIfNeeded();
  await page.waitForTimeout(150);
  const box = await locator.boundingBox();
  const dpr = await page.evaluate(() => window.devicePixelRatio);
  return { x: (box.x + box.width / 2) * dpr, y: (box.y + box.height / 2) * dpr };
}

async function resources(page) {
  const snapshot = await invoke(page, "graph_snapshot", { query: { kinds: ["resource"], text: null, around: null, includeInactive: true, limit: 2000 } });
  return snapshot.entities;
}

async function liveResource(page, key) {
  return (await resources(page)).find((e) => e.key === key && e.lifecycle !== "tombstoned") ?? null;
}

/** Record every notification shown from now on (routine results fade after a few seconds). */
async function recordNotifications(page) {
  await page.evaluate(() => {
    if (window.__qolNotes) return;
    window.__qolNotes = [];
    const seen = new Map();
    const read = () => {
      for (const n of document.querySelectorAll('[data-testid="notification"]')) {
        const entry = {
          id: n.getAttribute("data-notification-id"),
          tone: n.getAttribute("data-tone"),
          title: n.querySelector(".ui-notification__title")?.textContent ?? "",
          body: n.querySelector(".ui-notification__body")?.textContent ?? "",
          actions: [...n.querySelectorAll(".ui-notification__actions button")].map((b) => b.textContent),
        };
        const key = `${entry.id}|${entry.title}|${entry.body}`;
        if (!seen.has(key)) {
          seen.set(key, true);
          window.__qolNotes.push(entry);
        }
      }
    };
    new MutationObserver(read).observe(document.body, { childList: true, subtree: true, characterData: true });
    read();
  });
}

/** A notification shown since recording started (even if it already faded). */
async function shownNotification(page, title, { timeout = 60_000 } = {}) {
  return waitFor(
    async () => (await page.evaluate(() => window.__qolNotes ?? [])).find((n) => title.test(n.title)) ?? null,
    { timeout, message: `notification ${title}` },
  );
}

async function nameField(page) {
  const input = page.getByTestId("file-name-dialog").getByRole("textbox");
  await input.waitFor();
  return input;
}

async function typeName(page, name) {
  const input = await nameField(page);
  await input.press("Control+A");
  await page.keyboard.type(name);
  await page.keyboard.press("Enter");
}

async function caret(page, path) {
  return editorLocator(page, path).locator(".cursors-layer .cursor").first().evaluate((c) => `${c.style.top}|${c.style.left}`);
}

export default async function qolFiles({ launchApp, artifactsDir, assert, fixture }) {
  const skip = platformSkip();
  if (skip) {
    console.log(`SKIPPED: qol-files ${skip}`);
    return;
  }
  const project = await makeProject(fixture);
  const { root, base } = project;
  await writeFile(join(root, "scratch.txt"), "alpha\n");
  await writeFile(join(root, "paper.tex"), "\\documentclass{article}\n\\begin{document}\n\\input{chapters/intro}\n\\end{document}\n");
  // Dropped files live outside the project.
  const src = join(base, "drop-src");
  await mkdir(join(src, "bulk"), { recursive: true });
  await writeFile(join(src, "figure.png"), Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3]));
  await writeFile(join(src, "measurements.csv"), "x,y\n1,2\n3,4\n");
  await writeFile(join(src, "notes.md"), "# Notes from elsewhere\n");
  const big = Buffer.alloc(8 * 1024 * 1024, 7);
  const bulk = [];
  for (let i = 0; i < 60; i++) {
    const p = join(src, "bulk", `sample-${String(i).padStart(2, "0")}.dat`);
    await writeFile(p, big);
    bulk.push(p);
  }
  // A link among dropped items (a junction to a folder outside).
  const linkTarget = join(base, "link-target");
  await mkdir(linkTarget, { recursive: true });
  await writeFile(join(linkTarget, "inside.txt"), "behind a link\n");
  const { execFileSync } = await import("node:child_process");
  execFileSync("cmd", ["/c", "mklink", "/J", join(src, "linked-folder"), linkTarget], { stdio: "ignore" });
  const other = await makeProject(fixture);

  const evidence = { real: [], notes: [], graph: {}, drops: [] };
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await settle(page);
    await page.screenshot({ path });
    shots.push(path);
  };

  const app = await launchApp({ dataDir: project.dataDir });
  const { page } = app;
  const pid = app.child.pid;
  try {
    await openWorkspace(page, root);
    await recordNotifications(page);
    await waitFor(async () => (await liveResource(page, "scratch.txt")) !== null, { timeout: 20_000, message: "graph indexed scratch.txt" });

    // --- 1. New File with the default name --------------------------------------------
    await page.getByTestId("files-new-file").click();
    const input = await nameField(page);
    await waitFor(async () => (await input.inputValue()) === "Untitled.txt", { message: "suggested name" });
    const selection = await waitFor(async () => {
      const s = await input.evaluate((el) => [el.selectionStart, el.selectionEnd]);
      return s[0] === 0 && s[1] === 8 ? s : null;
    }, { message: "stem selected" });
    assert.deepEqual(selection, [0, 8], "the stem “Untitled” is selected");
    await shot(page, "qol-new-file-dialog-light");
    await page.keyboard.press("Enter");
    await waitFor(async () => existsSync(join(root, "Untitled.txt")), { message: "Untitled.txt created" });
    assert.equal((await readFile(join(root, "Untitled.txt"))).length, 0, "created empty");
    await editorLocator(page, "Untitled.txt").locator(".monaco-editor").waitFor({ timeout: 15_000 });
    await waitFor(() => page.evaluate(() => Boolean(document.activeElement?.closest('[data-testid="source-editor"][data-path="Untitled.txt"]'))), { message: "focus in the new file's editor" });
    evidence.notes.push("New File (toolbar) → Untitled.txt created empty, opened, focus in its Monaco editor");

    // --- 2. explicit names in a selected folder (Ctrl+N) --------------------------------
    for (const name of ["draft.tex", "notes.typ", "summary.md", "fit.py", "Basic.lean", ".latexmkrc", "data notes ü.txt"]) {
      await selectFiles(page, ["chapters"]);
      await page.keyboard.press("Control+N");
      await typeName(page, name);
      await waitFor(async () => existsSync(join(root, "chapters", name)), { message: `chapters/${name} created` });
      assert.ok(!existsSync(join(root, "chapters", `${name}.txt`)), `no .txt appended to ${name}`);
      await tabLocator(page, `chapters/${name}`).waitFor({ timeout: 15_000 });
    }
    evidence.notes.push("Ctrl+N in chapters/: draft.tex, notes.typ, summary.md, fit.py, Basic.lean, .latexmkrc, “data notes ü.txt” created exactly and opened");

    // --- 3. a colliding name keeps the dialog and the existing file ---------------------
    const notesBefore = await readFile(join(root, "notes.md"));
    await selectFiles(page, ["README.txt"]);
    await page.keyboard.press("Control+N");
    await typeName(page, "notes.md");
    await page.getByTestId("file-name-dialog").getByText(/already contains “notes\.md”/).waitFor();
    assert.equal(await (await nameField(page)).inputValue(), "notes.md", "the typed name stays");
    assert.deepEqual(await readFile(join(root, "notes.md")), notesBefore, "existing file unchanged");
    await shot(page, "qol-new-file-collision-light");
    await page.getByTestId("file-name-cancel").click();
    await page.getByTestId("file-name-dialog").waitFor({ state: "detached" });

    // --- 4. rename an open dirty file with an extension change ---------------------------
    const before = await liveResource(page, "scratch.txt");
    assert.ok(before, "scratch.txt has a resource");
    const docId = await openInEditor(page, "scratch.txt");
    await focusEditor(page, "scratch.txt", "start");
    await page.keyboard.type("dirty ");
    await waitFor(async () => (await tabLocator(page, "scratch.txt").getAttribute("data-dirty")) === "true", { message: "tab dirty" });
    const caretBefore = await caret(page, "scratch.txt");
    // F2 inside Monaco is Monaco's (no explorer rename dialog).
    await page.keyboard.press("F2");
    await page.waitForTimeout(400);
    assert.equal(await page.getByTestId("file-name-dialog").count(), 0, "F2 in Monaco does not open the explorer rename");
    await page.keyboard.press("Escape");
    const tabsBefore = await page.locator("[data-editor-key]").evaluateAll((els) => els.map((e) => e.getAttribute("data-editor-key")));
    await selectFiles(page, ["scratch.txt"]);
    await page.keyboard.press("F2");
    const renameInput = await nameField(page);
    await waitFor(async () => (await renameInput.inputValue()) === "scratch.txt", { message: "current name" });
    await typeName(page, "scratch.md");
    await waitForNotification(page, /^Renamed “scratch\.txt” to “scratch\.md”$/);
    assert.ok(!existsSync(join(root, "scratch.txt")) && existsSync(join(root, "scratch.md")), "renamed on disk");
    assert.equal(await readFile(join(root, "scratch.md"), "utf8"), "alpha\n", "the saved bytes moved; unsaved text not written");
    const tabsAfter = await page.locator("[data-editor-key]").evaluateAll((els) => els.map((e) => e.getAttribute("data-editor-key")));
    assert.deepEqual(tabsAfter, tabsBefore.map((k) => (k === "file:scratch.txt" ? "file:scratch.md" : k)), "the tab kept its place under the new key");
    assert.equal(await tabLocator(page, "scratch.md").getAttribute("data-dirty"), "true", "still unsaved");
    const renamedEditor = editorLocator(page, "scratch.md");
    await renamedEditor.locator(".monaco-editor").waitFor();
    assert.equal(await renamedEditor.getAttribute("data-document-id"), docId, "same document");
    assert.equal((await invoke(page, "document_state", { document: docId })).text, "dirty alpha\n", "buffer kept");
    assert.equal((await invoke(page, "document_state", { document: docId })).info.language, "markdown", "language follows the extension");
    assert.equal(await caret(page, "scratch.md"), caretBefore, "caret position kept");
    // Undo still reaches the edits made before the rename.
    await focusEditor(page, "scratch.md", "end");
    for (let i = 0; i < 6; i++) await page.keyboard.press("Control+Z");
    await waitFor(async () => (await invoke(page, "document_state", { document: docId })).text === "alpha\n", { message: "undo across the rename" });
    await page.keyboard.type("saved ");
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await readFile(join(root, "scratch.md"), "utf8")).includes("saved "), { message: "saved to the new path" });
    assert.ok(!existsSync(join(root, "scratch.txt")), "the old path is never recreated");
    // Graph identity (Graph page closed), also after the watcher's events.
    await page.waitForTimeout(2500);
    const after = await liveResource(page, "scratch.md");
    assert.equal(after?.id, before.id, "the resource kept its identity");
    const all = (await resources(page)).filter((e) => e.key === "scratch.txt" || e.key === "scratch.md");
    assert.equal(all.length, 1, `one resource for the renamed file (${JSON.stringify(all.map((e) => [e.key, e.lifecycle]))})`);
    evidence.graph.rename = { before: [before.id, before.key], after: [after.id, after.key], resourcesForBothPaths: all.length };
    const status = await invoke(page, "graph_status");
    evidence.graph.statusAfterRename = status.freshness?.state ?? status.state;
    await dismissNotifications(page);

    // --- 5. case-only rename; a rename collision keeps everything -------------------------
    const fitBefore = await liveResource(page, "chapters/fit.py").catch(() => null);
    await selectFiles(page, ["chapters/fit.py"]);
    await page.keyboard.press("F2");
    await typeName(page, "Fit.py");
    await waitForNotification(page, /^Renamed “fit\.py” to “Fit\.py”$/);
    assert.ok((await readdir(join(root, "chapters"))).includes("Fit.py"), "the name on disk changed case");
    assert.ok(!(await readdir(join(root, "chapters"))).includes("fit.py"));
    await page.waitForTimeout(1500);
    const fitAll = (await resources(page)).filter((e) => (e.key ?? "").toLowerCase() === "chapters/fit.py" && e.lifecycle !== "tombstoned");
    assert.equal(fitAll.length, 1, `one live resource after a case-only rename (${JSON.stringify(fitAll.map((e) => e.key))})`);
    if (fitBefore) assert.equal(fitAll[0].id, fitBefore.id, "case-only rename kept the identity");
    await dismissNotifications(page);
    await selectFiles(page, ["chapters/summary.md"]);
    await page.keyboard.press("F2");
    await typeName(page, "draft.tex");
    await page.getByTestId("file-name-dialog").getByText(/already contains “draft\.tex”/).waitFor();
    assert.ok(existsSync(join(root, "chapters", "summary.md")), "nothing renamed");
    await page.getByTestId("file-name-cancel").click();

    // --- 6. references are listed, not rewritten ------------------------------------------
    await waitFor(async () => (await liveResource(page, "paper.tex")) !== null, { message: "paper.tex indexed" });
    await selectFiles(page, ["chapters/intro.tex"]);
    await page.keyboard.press("F2");
    const refs = page.getByTestId("rename-references");
    await refs.waitFor({ timeout: 10_000 });
    assert.match(await refs.textContent(), /paper\.tex/, "the manuscript that includes it is named");
    await shot(page, "qol-rename-references-light");
    await typeName(page, "introduction.tex");
    const renamedIntro = await waitForNotification(page, /^Renamed “intro\.tex” to “introduction\.tex”$/);
    assert.equal(renamedIntro.tone, "warning", "a rename that leaves references says so");
    assert.match(await readFile(join(root, "paper.tex"), "utf8"), /\\input\{chapters\/intro\}/, "the manuscript was not rewritten");
    evidence.notes.push(`references after renaming intro.tex: ${renamedIntro.body}`);
    await dismissNotifications(page);

    // --- 7. injected graph-recording failure -------------------------------------------
    const notesTyp = await waitFor(() => liveResource(page, "chapters/notes.typ"), { timeout: 15_000, message: "notes.typ indexed" });
    await invoke(page, "files_dev_fail_next_graph_record");
    await selectFiles(page, ["chapters/notes.typ"]);
    await page.keyboard.press("F2");
    await typeName(page, "notes-2.typ");
    const pending = await waitForNotification(page, /^Renamed “notes\.typ” to “notes-2\.typ”$/);
    assert.equal(pending.tone, "warning");
    assert.match(pending.body, /has not recorded the rename yet/, "the pending graph state is stated");
    await shot(page, "qol-rename-graph-pending-light");
    const reconciled = await waitFor(() => liveResource(page, "chapters/notes-2.typ"), { timeout: 15_000, message: "reconciled after the failed record" });
    assert.equal(reconciled.id, notesTyp.id, "reconciled with the same identity");
    const notesAll = (await resources(page)).filter((e) => e.key === "chapters/notes.typ" || e.key === "chapters/notes-2.typ");
    assert.equal(notesAll.length, 1, "no duplicate after the retry");
    evidence.graph.injectedFailure = { id: notesTyp.id, after: reconciled.id, resources: notesAll.length };
    await dismissNotifications(page);

    // --- 8. real OS drops ------------------------------------------------------------
    // (a) two files onto the analysis/ folder row.
    const analysisRow = fileRow(page, "analysis");
    let target = await clientPoint(page, analysisRow);
    const dropA = osDrag({ pid, x: target.x, y: target.y, paths: [join(src, "figure.png"), join(src, "measurements.csv")], holdMs: 2500 });
    dropA.catch(() => {});
    const highlighted = await waitFor(() => page.locator('[data-testid="file-tree"] [data-path="analysis"][data-drop-target]').count(), { timeout: 8000, message: "folder highlighted during the drag" }).catch(() => 0);
    if (highlighted) await shot(page, "qol-drop-target-folder-light");
    const resultA = await dropA;
    evidence.drops.push({ drop: "figure.png + measurements.csv → analysis/", helper: resultA, highlighted: Boolean(highlighted) });
    assert.ok(resultA.onApp, `the drop point was on the app window (${JSON.stringify(resultA)})`);
    await waitFor(async () => existsSync(join(root, "analysis", "figure.png")) && existsSync(join(root, "analysis", "measurements.csv")), { timeout: 15_000, message: "dropped files copied" });
    assert.ok(highlighted, "the destination folder was highlighted during the drag");
    assert.deepEqual(await readFile(join(root, "analysis", "measurements.csv")), await readFile(join(src, "measurements.csv")), "same bytes");
    assert.ok(existsSync(join(src, "figure.png")) && existsSync(join(src, "measurements.csv")), "originals untouched");
    await waitForNotification(page, "Imported 2 items into analysis/");
    await waitFor(async () => (await liveResource(page, "analysis/measurements.csv")) !== null, { timeout: 15_000, message: "the copy has its own resource" });
    evidence.real.push("OLE DoDragDrop (CF_HDROP) from a helper window onto the app window imported into the highlighted folder");
    await dismissNotifications(page);

    // (b) onto the tree's empty space (project folder), a colliding name and a link.
    const tree = page.getByTestId("file-tree");
    const box = await tree.boundingBox();
    const dpr = await page.evaluate(() => window.devicePixelRatio);
    target = { x: (box.x + box.width / 2) * dpr, y: (box.y + box.height - 12) * dpr };
    const dropB = osDrag({ pid, x: target.x, y: target.y, paths: [join(src, "notes.md"), join(src, "linked-folder")], holdMs: 2000 });
    dropB.catch(() => {});
    const rootHighlighted = await waitFor(() => page.locator('[data-testid="file-tree"][data-drop-target]').count(), { timeout: 8000, message: "tree highlighted" }).catch(() => 0);
    if (rootHighlighted) await shot(page, "qol-drop-target-root-light");
    evidence.drops.push({ drop: "notes.md + junction → project folder", helper: await dropB, highlighted: Boolean(rootHighlighted) });
    const partial = await waitForNotification(page, /^Imported 1 of 2 items into the project folder$/);
    assert.equal(partial.tone, "warning");
    assert.match(partial.body, /“notes\.md” as “notes \(2\)\.md”/, "the colliding name kept both");
    assert.match(partial.body, /link/, "the link was refused with its reason");
    assert.ok(!partial.actions.includes("Retry"), "no Retry for a refusal that stays true (a link)");
    assert.deepEqual(await readFile(join(root, "notes.md")), notesBefore, "the existing notes.md was not overwritten");
    assert.equal(await readFile(join(root, "notes (2).md"), "utf8"), "# Notes from elsewhere\n");
    assert.ok(!existsSync(join(root, "linked-folder")), "nothing copied through the link");
    await shot(page, "qol-import-partial-light");
    await dismissNotifications(page);

    // (c) a larger batch: progress, Cancel, then Retry (no duplicates).
    await mkdir(join(root, "bulk-in"), { recursive: true });
    await waitFor(() => fileRow(page, "bulk-in").count(), { message: "bulk-in listed" });
    target = await clientPoint(page, fileRow(page, "bulk-in"));
    // Press the progress notification's Cancel as soon as it appears (a click on the visible button).
    await page.evaluate(() => {
      const press = () => {
        for (const n of document.querySelectorAll('[data-testid="notification"]')) {
          if (!/^Importing 60 items/.test(n.querySelector(".ui-notification__title")?.textContent ?? "")) continue;
          const cancel = [...n.querySelectorAll(".ui-notification__actions button")].find((b) => b.textContent === "Cancel");
          if (cancel) {
            window.__qolCanceled = true;
            cancel.click();
            return true;
          }
        }
        return false;
      };
      const observer = new MutationObserver(() => press() && observer.disconnect());
      observer.observe(document.body, { childList: true, subtree: true });
    });
    const dropC = osDrag({ pid, x: target.x, y: target.y, paths: bulk, holdMs: 300 });
    dropC.catch(() => {});
    await dropC;
    const ended = await shownNotification(page, /^(Import canceled: \d+ of 60 imported into bulk-in\/|Imported 60 items into bulk-in\/)$/);
    evidence.notes.push(`cancel pressed: ${await page.evaluate(() => Boolean(window.__qolCanceled))}`);
    const copied = () => readdir(join(root, "bulk-in"));
    const reported = /canceled: (\d+) of/.exec(ended.title);
    await waitFor(async () => (await copied()).length === (reported ? Number(reported[1]) : 60), { message: "files on disk match the report" });
    evidence.notes.push(`bulk import: ${ended.title}`);
    if (ended.actions.includes("Retry")) {
      await pressNotificationAction(page, ended.title, "Retry");
      // Retry sends only what was not imported (the backend also never copies a source twice for one import).
      const retried = await shownNotification(page, /^Imported \d+ items? into bulk-in\/$/);
      await waitFor(async () => (await copied()).length === 60, { timeout: 60_000, message: "the retry copied the rest" });
      evidence.notes.push(`retry: ${retried.title}`);
      evidence.notes.push("Retry after cancel completed the rest");
    }
    const names = await copied();
    assert.equal(names.length, 60, `exactly 60 files (${names.length})`);
    assert.ok(names.every((n) => !/\(\d+\)/.test(n)), "Retry never duplicated a completed copy");
    await dismissNotifications(page);

    // (d) a project switch during an import never redirects it.
    await mkdir(join(root, "switch-in"), { recursive: true });
    await waitFor(() => fileRow(page, "switch-in").count(), { message: "switch-in listed" });
    target = await clientPoint(page, fileRow(page, "switch-in"));
    await osDrag({ pid, x: target.x, y: target.y, paths: bulk, holdMs: 200 });
    if (await page.locator('[data-testid="notification"]', { hasText: "Importing 60 items" }).count()) await shot(page, "qol-import-progress-light");
    await invoke(page, "workspace_open", { path: other.root });
    // The final result (not the progress notification).
    const switched = await shownNotification(page, /^(Imported|Nothing was imported)\b.* into switch-in\/$/);
    const done = /^Imported (\d+) of/.exec(switched.title)?.[1] ?? (/^Imported 60 items/.test(switched.title) ? "60" : "0");
    const inOld = existsSync(join(root, "switch-in")) ? (await readdir(join(root, "switch-in"))).length : 0;
    assert.equal(inOld, Number(done), "the copies in the original project match the report");
    assert.ok(!existsSync(join(other.root, "switch-in")), "nothing was written into the newly opened project");
    assert.ok(!(await readdir(other.root)).some((n) => n.startsWith("sample-")), "no copy in the new project's folder");
    evidence.notes.push(`switch during import: ${switched.title}; ${inOld} copies in the original project's switch-in/`);
    if (inOld < 60) assert.match(switched.body, /another project was opened/, "skipped items say why");

    // --- 9. other drops with the native handler ---------------------------------------
    await openWorkspace(page, root);
    await openInEditor(page, "README.txt");
    const readmeEditor = editorLocator(page, "README.txt");
    const readmeId = await readmeEditor.getAttribute("data-document-id");
    const textBefore = (await invoke(page, "document_state", { document: readmeId })).text;
    target = await clientPoint(page, readmeEditor.locator(".view-lines"));
    const textDrop = await osDrag({ pid, x: target.x, y: target.y, mode: "text", text: "DROPPED-TEXT", holdMs: 600 });
    await page.waitForTimeout(800);
    const textAfter = (await invoke(page, "document_state", { document: readmeId })).text;
    evidence.drops.push({ drop: "plain text (CF_UNICODETEXT) onto Monaco", helper: textDrop, inserted: textAfter.includes("DROPPED-TEXT") });
    // An in-editor drag of selected text (Monaco's own mouse-based drag and drop).
    await focusEditor(page, "README.txt", "start");
    await page.keyboard.press("Shift+End");
    const line1 = readmeEditor.locator(".view-line").first();
    const lineBox = await line1.boundingBox();
    const from = { x: (lineBox.x + 12) * dpr, y: (lineBox.y + lineBox.height / 2) * dpr };
    const to = { x: (lineBox.x + 12) * dpr, y: (lineBox.y + lineBox.height * 2.5) * dpr };
    const moveDrag = await osDrag({ pid, x: from.x, y: from.y, mode: "mouse", x2: to.x, y2: to.y, holdMs: 300 });
    await page.waitForTimeout(600);
    const textMoved = (await invoke(page, "document_state", { document: readmeId })).text;
    evidence.drops.push({ drop: "in-editor drag of selected text (mouse)", helper: moveDrag, changed: textMoved !== textAfter });
    // Leave README.txt as it was on disk (nothing saved).
    assert.equal(await readFile(join(root, "README.txt"), "utf8"), textBefore, "nothing saved by these drops");

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const dir = join(devHome, "ui-reference", "qol-files");
    await mkdir(dir, { recursive: true });
    for (const p of shots) await copyFile(p, join(dir, p.split(/[\\/]/).pop()));
  }
}
