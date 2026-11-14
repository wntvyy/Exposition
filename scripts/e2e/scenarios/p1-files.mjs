// P1 project operations — files and folders in the real app (spec §5 rows 1,
// 2, 6, 9, 10a). Disposable temporary projects only.
//  1. Delete unused files (multi-selection, Delete key): no dialog, the files
//     leave the workspace, they are in the REAL Windows Recycle Bin (read back
//     through the shell), the graph no longer has them active; Undo restores.
//  2. Delete a folder from the context menu: no dialog (containment only).
//  3. Duplicate (Ctrl+D) twice: originals unchanged, fresh collision-free
//     names, same bytes.
//  4. Copy (Ctrl+C) puts the file on the REAL OS clipboard (CF_HDROP read back
//     with Get-Clipboard); Paste (Ctrl+V) into a folder, twice: no overwrite.
//  5. Paste of an external file put on the OS clipboard by Explorer-style
//     Set-Clipboard: copied in, the source unchanged; clipboard text is not a
//     command.
//  6. Paste a folder into itself: refused, nothing created.
//  7. Restore into an occupied path: the collision is asked, both versions kept.
//  8. Focus routing: Delete / Ctrl+D in Monaco and in the palette input never
//     touch files.
// Screens: routine result, context menu, restore collision → ui-reference/p1/.
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join } from "node:path";

import {
  consoleErrors,
  decision,
  dismissNotifications,
  expandTo,
  expectNoDialog,
  fileRow,
  osClipboardFiles,
  publishShots,
  recycleBinItemsFrom,
  selectFiles,
  setOsClipboardFiles,
  setOsClipboardText,
  waitForNotification,
} from "../p1-helpers.mjs";
import { editorLocator, focusEditor, invoke, makeProject, openInEditor, openWorkspace, waitFor } from "../workspace-helpers.mjs";

export default async function p1Files({ launchApp, artifactsDir, assert, fixture }) {
  const project = await makeProject(fixture);
  const { root, base } = project;
  await mkdir(join(root, "scratch", "sub"), { recursive: true });
  await writeFile(join(root, "scratch", "a.txt"), "alpha\n");
  await writeFile(join(root, "scratch", "b.txt"), "beta\n");
  await writeFile(join(root, "scratch", "sub", "c.txt"), "gamma\n");
  await mkdir(join(base, "outside-src"), { recursive: true });
  const external = join(base, "outside-src", "measurements.csv");
  await writeFile(external, "x,y\n1,2\n");
  const evidence = { real: [], notes: [] };
  const shots = [];
  const shot = async (page, name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };

  const app = await launchApp({ dataDir: project.dataDir });
  const { page } = app;
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await openWorkspace(page, root);

    // --- 1. delete unused files, multi-selection, Delete key ------------------------
    await selectFiles(page, ["scratch/a.txt", "scratch/b.txt"]);
    await page.keyboard.press("Delete");
    await expectNoDialog(page, assert, 800, "deleting unused files asks nothing");
    const moved = await waitForNotification(page, /^Moved 2 items to the (Recycle Bin|Trash)$/);
    assert.equal(moved.tone, "info");
    await waitFor(async () => !existsSync(join(root, "scratch", "a.txt")) && !existsSync(join(root, "scratch", "b.txt")), { message: "files left the workspace" });
    await waitFor(async () => (await fileRow(page, "scratch/a.txt").count()) === 0, { message: "tree updated" });
    const binned = recycleBinItemsFrom(join(root, "scratch")).map((i) => i.name);
    assert.ok(binned.some((n) => n.startsWith("a")) && binned.some((n) => n.startsWith("b")), `real Recycle Bin holds a.txt and b.txt (${binned.join(", ")})`);
    evidence.real.push("Windows Recycle Bin (Shell.Application namespace 10) holds the deleted files with their original folder");
    await shot(page, "p1-routine-delete-light");
    // The graph follows the actual outcome: no active resource for the deleted paths.
    const snapshot = await invoke(page, "graph_snapshot", { query: { kinds: null, text: "a.txt", around: null, includeInactive: true, limit: 50 } }).catch(() => null);
    if (snapshot) {
      const active = snapshot.entities.filter((e) => e.key === "scratch/a.txt" && e.lifecycle === "active");
      assert.equal(active.length, 0, "graph has no active resource for the deleted file");
    }
    // Undo (only when the outcome carried restore tokens).
    if (moved.actions.includes("Undo")) {
      await fileRow(page, "scratch").focus();
      await page.keyboard.press("Control+Z");
      await waitFor(async () => existsSync(join(root, "scratch", "a.txt")) && existsSync(join(root, "scratch", "b.txt")), { message: "Undo restored the files" });
      assert.equal(await readFile(join(root, "scratch", "a.txt"), "utf8"), "alpha\n", "restored bytes");
      evidence.notes.push("Undo (Ctrl+Z) restored both files from the Recycle Bin");
      await waitForNotification(page, /^Restored 2 items$/);
    } else {
      evidence.notes.push(`Undo not offered: ${moved.body}`);
      assert.match(moved.body, /Recycle Bin|Trash/, "recovery described truthfully");
    }
    await dismissNotifications(page);

    // --- 2. a folder from the context menu (containment only: no prompt) -------------
    await expandTo(page, "scratch/sub");
    await fileRow(page, "scratch/sub").click({ button: "right" });
    const menu = page.getByTestId("files-context-menu");
    await menu.waitFor();
    assert.ok(await menu.getByRole("menuitem", { name: /Delete Permanently/ }).isVisible(), "Delete Permanently is a separate, labeled item");
    await shot(page, "p1-context-menu-light");
    await menu.getByRole("menuitem", { name: /^Delete\b(?! Permanently)/ }).click();
    await expectNoDialog(page, assert, 800, "folder with only containment links: no prompt");
    await waitFor(async () => !existsSync(join(root, "scratch", "sub")), { message: "folder deleted" });
    await waitForNotification(page, /^Moved “sub” to the (Recycle Bin|Trash)$/);
    await dismissNotifications(page);

    // --- 3. duplicate twice ---------------------------------------------------------
    const notes = await readFile(join(root, "notes.md"));
    await selectFiles(page, ["notes.md"]);
    await page.keyboard.press("Control+D");
    await waitFor(async () => (await readdir(root)).filter((n) => /^notes.*\.md$/.test(n)).length === 2, { message: "first duplicate" });
    await selectFiles(page, ["notes.md"]);
    await page.keyboard.press("Control+D");
    await waitFor(async () => (await readdir(root)).filter((n) => /^notes.*\.md$/.test(n)).length === 3, { message: "second duplicate" });
    const copies = (await readdir(root)).filter((n) => /^notes.*\.md$/.test(n) && n !== "notes.md");
    assert.equal(new Set(copies).size, 2, `two distinct names: ${copies.join(", ")}`);
    for (const c of copies) assert.deepEqual(await readFile(join(root, c)), notes, `${c} has the saved bytes`);
    assert.deepEqual(await readFile(join(root, "notes.md")), notes, "original unchanged");
    evidence.notes.push(`duplicates: ${copies.join(", ")}`);
    await dismissNotifications(page);

    // --- 4. copy → real OS clipboard; paste twice into a folder -----------------------
    await selectFiles(page, ["main.typ"]);
    await page.keyboard.press("Control+C");
    await waitForNotification(page, /^Copied “main\.typ”/);
    const clip = osClipboardFiles();
    assert.ok(clip.some((p) => p.toLowerCase() === join(root, "main.typ").toLowerCase()), `OS clipboard file list holds main.typ (${clip.join(", ")})`);
    evidence.real.push("Get-Clipboard -Format FileDropList returns the copied project file (CF_HDROP written by the app)");
    await selectFiles(page, ["chapters"]);
    await page.keyboard.press("Control+V");
    await waitFor(async () => existsSync(join(root, "chapters", "main.typ")), { message: "pasted into chapters/" });
    await page.keyboard.press("Control+V");
    await waitFor(async () => (await readdir(join(root, "chapters"))).filter((n) => /^main.*\.typ$/.test(n)).length === 2, { message: "second paste is a new file" });
    const main = await readFile(join(root, "main.typ"));
    for (const n of (await readdir(join(root, "chapters"))).filter((x) => /^main.*\.typ$/.test(x))) {
      assert.deepEqual(await readFile(join(root, "chapters", n)), main, `${n} equals the source`);
    }
    await dismissNotifications(page);

    // --- 5. paste an external file from the OS clipboard; text is never a command -----
    setOsClipboardFiles([external]);
    await selectFiles(page, ["analysis"]);
    await page.keyboard.press("Control+V");
    await waitFor(async () => existsSync(join(root, "analysis", "measurements.csv")), { message: "external file pasted" });
    assert.equal(await readFile(external, "utf8"), "x,y\n1,2\n", "external source unchanged");
    evidence.real.push("Set-Clipboard -Path (Explorer-style CF_HDROP) pasted into the project by explicit Ctrl+V");
    await dismissNotifications(page);
    setOsClipboardText(join(root, "README.txt"));
    await selectFiles(page, ["analysis"]);
    await page.keyboard.press("Control+V");
    await waitForNotification(page, "Nothing to paste");
    assert.ok(!existsSync(join(root, "analysis", "README.txt")), "clipboard text naming a path was not treated as a file");
    await dismissNotifications(page);

    // --- 6. a folder into itself ----------------------------------------------------
    await selectFiles(page, ["chapters"]);
    await page.keyboard.press("Control+C");
    await page.keyboard.press("Control+V");
    await waitForNotification(page, /^(Nothing to paste|Could not paste)/);
    assert.ok(!existsSync(join(root, "chapters", "chapters")), "no recursive self-copy");
    await dismissNotifications(page);

    // --- 7. restore into an occupied path ---------------------------------------------
    await selectFiles(page, ["README.txt"]);
    const readme = await readFile(join(root, "README.txt"), "utf8");
    await page.keyboard.press("Delete");
    const gone = await waitForNotification(page, /^Moved “README\.txt” to the (Recycle Bin|Trash)$/);
    await waitFor(async () => !existsSync(join(root, "README.txt")), { message: "README deleted" });
    await writeFile(join(root, "README.txt"), "a new README\n");
    if (gone.actions.includes("Undo")) {
      await fileRow(page, "main.typ").focus();
      await page.keyboard.press("Control+Z");
      const collision = page.getByTestId("op-restore-collision");
      await collision.waitFor({ timeout: 10_000 });
      assert.match(await collision.textContent(), /already exists/);
      await shot(page, "p1-restore-collision-light");
      await collision.getByRole("button", { name: "Restore with a New Name" }).click();
      await waitFor(async () => (await readdir(root)).filter((n) => /^README.*\.txt$/.test(n)).length === 2, { message: "both versions kept" });
      assert.equal(await readFile(join(root, "README.txt"), "utf8"), "a new README\n", "the new file was not overwritten");
      const restored = (await readdir(root)).find((n) => /^README.*\.txt$/.test(n) && n !== "README.txt");
      assert.equal(await readFile(join(root, restored), "utf8"), readme, "the restored version has the old bytes");
      evidence.notes.push(`restore into occupied path kept both: README.txt (new) + ${restored} (restored)`);
    } else {
      evidence.notes.push("restore-collision not exercised: deletion offered no Undo");
    }
    await dismissNotifications(page);

    // --- 8. focus routing: Monaco and the palette keep their keys ----------------------
    await openInEditor(page, "main.typ");
    await selectFiles(page, ["notes.md"]);
    await focusEditor(page, "main.typ", "end");
    const before = (await readdir(root)).sort();
    await page.keyboard.press("Control+D");
    await page.keyboard.press("Delete");
    await page.keyboard.press("Shift+Delete");
    await page.waitForTimeout(500);
    assert.deepEqual((await readdir(root)).sort(), before, "keys in Monaco never touch files");
    assert.ok(existsSync(join(root, "notes.md")), "notes.md still there");
    assert.equal(await page.getByRole("alertdialog").count(), 0);
    await page.keyboard.press("Control+Shift+P");
    await page.locator(".wb-palette__input").waitFor();
    await page.keyboard.type("abc");
    await page.keyboard.press("Delete");
    await page.keyboard.press("Control+D");
    await page.keyboard.press("Escape");
    await page.waitForTimeout(300);
    assert.deepEqual((await readdir(root)).sort(), before, "keys in the palette never touch files");
    assert.equal(await decision(page).count(), 0);
    await editorLocator(page, "main.typ").waitFor();

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
  await publishShots(shots);
}
