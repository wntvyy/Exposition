// Workspace files end to end in the real app: open a folder, browse the tree,
// open files in Monaco, type, save with Ctrl+S, verify exact bytes on disk
// (LF, CRLF, UTF-8 BOM, UTF-16), undo/redo through the shared document
// service, mixed line endings, unavailable files, close/reopen, and rejection of
// traversal and junction escapes.
import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

import {
  alertDialog,
  closeTab,
  documentState,
  editorLocator,
  focusEditor,
  invoke,
  invokeError,
  makeProject,
  monacoState,
  openFromTree,
  openInEditor,
  openWorkspace,
  tabDirty,
  waitFor,
  waitForMirror,
  waitForText,
} from "../workspace-helpers.mjs";

export default async function workspaceFiles({ launchApp, artifactsDir, assert, fixture }) {
  const project = await makeProject(fixture);
  const app = await launchApp({ dataDir: project.dataDir });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  try {
    // --- open folder + tree -------------------------------------------------
    const info = await openWorkspace(page, project.root);
    assert.equal(info.name, "project");
    const tree = page.locator('[data-testid="file-tree"]');
    const topLevel = await tree.locator('[data-level="1"]').evaluateAll((rows) => rows.map((r) => r.dataset.path));
    for (const hidden of [".git", ".lake", "node_modules", "target"]) assert.ok(!topLevel.includes(hidden), `${hidden} hidden`);
    assert.deepEqual(
      topLevel.filter((p) => p !== "escape"),
      ["analysis", "chapters", "encoding", "main.typ", "notes.md", "README.txt"],
      "directories first, then files, case-insensitive",
    );
    const escapeRow = tree.locator('[data-path="escape"]');
    assert.equal(await escapeRow.getAttribute("aria-disabled"), "true", "junction escaping the folder is unavailable");
    await shot("01-tree");

    // --- open, type, save (LF) ---------------------------------------------
    const mainPath = join(project.root, "main.typ");
    const mainOriginal = await readFile(mainPath);
    const mainId = await openInEditor(page, "main.typ");
    await focusEditor(page, "main.typ", "end");
    await page.keyboard.type("Typed in Monaco.");
    await waitForText(page, mainId, `${mainOriginal.toString("utf8")}Typed in Monaco.`);
    await waitFor(async () => (await tabDirty(page, "main.typ")) === "true", { message: "dirty tab" });
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await tabDirty(page, "main.typ")) === "false", { message: "clean after save" });
    assert.deepEqual(await readFile(mainPath), Buffer.concat([mainOriginal, Buffer.from("Typed in Monaco.")]));
    await shot("02-saved-main");

    // --- undo/redo from Monaco go through the shared service ---------------
    await page.keyboard.type(" abc");
    const typed = `${mainOriginal.toString("utf8")}Typed in Monaco. abc`;
    await waitForText(page, mainId, typed);
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, `${mainOriginal.toString("utf8")}Typed in Monaco.`, "service text after Ctrl+Z");
    let state = await documentState(page, mainId);
    assert.ok(state.info.canRedo, "the service recorded the undo");
    // Monaco applies the service's change event and repaints asynchronously.
    await waitFor(async () => !(await editorLocator(page, "main.typ").locator(".view-lines").innerText()).includes("abc"), {
      message: "Monaco shows the undone text",
    });
    await page.keyboard.press("Control+Y");
    await waitForText(page, mainId, typed, "service text after Ctrl+Y");
    await page.keyboard.press("Control+Shift+Z"); // nothing more to redo; must not corrupt
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, `${mainOriginal.toString("utf8")}Typed in Monaco.`);
    state = await documentState(page, mainId);
    assert.equal(state.info.revision > state.info.savedRevision, true, "undo is a new revision");

    // --- edits from another origin reach Monaco without echo -------------------
    const before = await documentState(page, mainId);
    await invoke(page, "document_apply", {
      request: {
        id: "agent-e2e-1",
        origin: { kind: "agent", taskId: "e2e" },
        edits: [{ document: mainId, baseRevision: before.info.revision, patches: [{ range: { start: 0, end: 0 }, text: "// agent\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: "agent edit",
        selection: null,
      },
    });
    const viewText = async () =>
      (await editorLocator(page, "main.typ").locator(".view-lines").innerText()).replaceAll(" ", " ");
    await waitFor(async () => (await viewText()).startsWith("// agent"), { message: "agent edit shown in Monaco" });
    await new Promise((r) => setTimeout(r, 300));
    const afterAgent = await documentState(page, mainId);
    assert.equal(afterAgent.info.revision, before.info.revision + 1, "no echo transaction from the view");
    // Typing after a remote edit lands where the caret is (end of document).
    await page.keyboard.type("!");
    await waitForText(page, mainId, `// agent\n${before.text}!`);
    // Shared undo: the typed char, then the agent edit.
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, `// agent\n${before.text}`);
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, before.text, "undo reverts the agent edit through the shared history");

    // --- IME composition commits as one undo group -------------------------------
    const cdp = await page.context().newCDPSession(page);
    const composeBase = (await documentState(page, mainId)).text;
    await page.keyboard.type(" ");
    await waitForText(page, mainId, `${composeBase} `);
    await cdp.send("Input.imeSetComposition", { text: "k", selectionStart: 1, selectionEnd: 1 });
    await cdp.send("Input.imeSetComposition", { text: "かん", selectionStart: 2, selectionEnd: 2 });
    await cdp.send("Input.imeSetComposition", { text: "漢字", selectionStart: 2, selectionEnd: 2 });
    await cdp.send("Input.insertText", { text: "漢字" });
    await waitForText(page, mainId, `${composeBase} 漢字`, "composed text committed");
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, `${composeBase} `, "one undo removes the whole composition");
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, composeBase);

    // --- IME composition while an agent edits elsewhere (R-m1 finding) ----------
    // Remote revisions are held until the composition ends, then applied in order.
    await page.keyboard.press("Control+End");
    const imeBase = await waitForMirror(page, "main.typ", mainId);
    await cdp.send("Input.imeSetComposition", { text: "か", selectionStart: 1, selectionEnd: 1 });
    await waitFor(async () => (await monacoState(page, "main.typ"))?.text === `${imeBase}か`, {
      message: "composition shown in Monaco",
    });
    // Composition updates are not sent until the composition ends.
    const atComposition = await documentState(page, mainId);
    assert.equal(atComposition.text, imeBase, "partial composition text is not committed");
    await invoke(page, "document_apply", {
      request: {
        id: "agent-during-ime",
        origin: { kind: "agent", taskId: "e2e" },
        edits: [{ document: mainId, baseRevision: atComposition.info.revision, patches: [{ range: { start: 0, end: 0 }, text: "R:" }] }],
        undo: { kind: "newGroup" },
        conflict: "rebaseDisjoint",
        label: null,
        selection: null,
      },
    });
    await new Promise((r) => setTimeout(r, 200));
    await cdp.send("Input.imeSetComposition", { text: "かん", selectionStart: 2, selectionEnd: 2 });
    await cdp.send("Input.insertText", { text: "漢" });
    const afterIme = await waitForMirror(page, "main.typ", mainId, "Monaco and the service agree after IME + agent edit");
    assert.equal(afterIme, `R:${imeBase}漢`, "composed text committed once, agent edit kept, nothing else changed");
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, `R:${imeBase}`, "undo removes the composition");
    await page.keyboard.press("Control+Z");
    await waitForText(page, mainId, imeBase, "undo removes the agent edit");
    await cdp.detach();

    // --- close (clean, after saving) and reopen ----------------------------
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await tabDirty(page, "main.typ")) === "false", { message: "saved" });
    await closeTab(page, "main.typ");
    await waitFor(async () => (await invoke(page, "document_list")).every((d) => d.path !== "main.typ"), {
      message: "document closed in the service",
    });
    const reopenedId = await openInEditor(page, "main.typ");
    assert.equal((await documentState(page, reopenedId)).text, (await readFile(mainPath, "utf8")));
    assert.equal((await documentState(page, reopenedId)).text, `${mainOriginal.toString("utf8")}Typed in Monaco.`);

    // --- CRLF file: Enter inserts CRLF; bytes preserved ---------------------
    const crlfPath = join(project.root, "encoding", "crlf.tex");
    const crlfOriginal = await readFile(crlfPath);
    const crlfId = await openInEditor(page, "encoding/crlf.tex");
    assert.equal(await editorLocator(page, "encoding/crlf.tex").getAttribute("data-eol"), "crlf");
    await focusEditor(page, "encoding/crlf.tex", "end");
    await page.keyboard.type("% first");
    await page.keyboard.press("Enter");
    await page.keyboard.type("% second");
    await waitForText(page, crlfId, `${crlfOriginal.toString("utf8")}% first\r\n% second`);
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await tabDirty(page, "encoding/crlf.tex")) === "false", { message: "crlf saved" });
    assert.deepEqual(await readFile(crlfPath), Buffer.concat([crlfOriginal, Buffer.from("% first\r\n% second")]));

    // --- UTF-8 BOM file ------------------------------------------------------
    const bomPath = join(project.root, "encoding", "bom.md");
    const bomOriginal = await readFile(bomPath);
    const bomId = await openInEditor(page, "encoding/bom.md");
    await focusEditor(page, "encoding/bom.md", "start");
    await page.keyboard.type("Ü ");
    await waitForText(page, bomId, `Ü ${bomOriginal.subarray(3).toString("utf8")}`);
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await tabDirty(page, "encoding/bom.md")) === "false", { message: "bom saved" });
    assert.deepEqual(
      await readFile(bomPath),
      Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from("Ü "), bomOriginal.subarray(3)]),
    );

    // --- UTF-16 LE file -----------------------------------------------------
    const u16Path = join(project.root, "encoding", "utf16le.txt");
    const u16Id = await openInEditor(page, "encoding/utf16le.txt");
    await focusEditor(page, "encoding/utf16le.txt", "start");
    await page.keyboard.type(">");
    await waitForText(page, u16Id, ">UTF-16 LE text: αβγ\r\n");
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await tabDirty(page, "encoding/utf16le.txt")) === "false", { message: "utf16 saved" });
    assert.deepEqual(
      await readFile(u16Path),
      Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(">UTF-16 LE text: αβγ\r\n", "utf16le")]),
    );
    await shot("03-encodings");

    // --- mixed line endings: read-only until explicitly normalized ---------
    const mixedPath = join(project.root, "encoding", "mixed.txt");
    const mixedOriginal = await readFile(mixedPath);
    const mixedId = await openInEditor(page, "encoding/mixed.txt");
    const mixedEditor = editorLocator(page, "encoding/mixed.txt");
    await mixedEditor.locator('[data-testid="mixed-eol-banner"]').waitFor();
    const mixedRevision = (await documentState(page, mixedId)).info.revision;
    await focusEditor(page, "encoding/mixed.txt", "end");
    await page.keyboard.type("ignored");
    await new Promise((r) => setTimeout(r, 300));
    const mixedState = await documentState(page, mixedId);
    assert.equal(mixedState.info.revision, mixedRevision, "read-only view sent no edits");
    assert.equal(mixedState.text, mixedOriginal.toString("utf8"), "mixed text unchanged");
    await shot("04-mixed-eol");
    await mixedEditor.getByRole("button", { name: "Normalize to LF" }).click();
    await waitForText(page, mixedId, "line one\nline two\nline three\n", "normalized text");
    await mixedEditor.locator('[data-testid="mixed-eol-banner"]').waitFor({ state: "detached" });
    assert.deepEqual(await readFile(mixedPath), mixedOriginal, "normalizing does not write the file by itself");
    await focusEditor(page, "encoding/mixed.txt", "end");
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await readFile(mixedPath, "utf8")) === "line one\nline two\nline three\n", {
      message: "normalized file saved",
    });
    // Undo of the normalization is one step back to the mixed text.
    await page.keyboard.press("Control+Z");
    await waitForText(page, mixedId, mixedOriginal.toString("utf8"), "undo normalization");

    // --- unavailable files are never opened as text ------------------------
    await openFromTree(page, "encoding/binary.bin");
    const binary = page.locator('[data-testid="source-editor-unavailable"][data-path="encoding/binary.bin"]');
    await binary.waitFor();
    assert.match(await binary.innerText(), /binary/i);
    await openFromTree(page, "encoding/latin1.txt");
    await page
      .locator('[data-testid="source-editor-unavailable"][data-path="encoding/latin1.txt"]')
      .getByText(/not valid UTF-8/)
      .waitFor();
    assert.deepEqual(await readFile(join(project.root, "encoding", "latin1.txt")), Buffer.from("caf\xe9 au lait\n", "latin1"));
    await shot("05-unavailable");

    // --- close a dirty editor: Cancel keeps it, Don't Save discards -------
    await openFromTree(page, "notes.md");
    const notesId = await waitFor(() => editorLocator(page, "notes.md").getAttribute("data-document-id"));
    await editorLocator(page, "notes.md").locator(".monaco-editor").waitFor();
    await focusEditor(page, "notes.md", "end");
    await page.keyboard.type("- three");
    await waitFor(async () => (await tabDirty(page, "notes.md")) === "true", { message: "notes dirty" });
    await closeTab(page, "notes.md");
    const prompt = alertDialog(page);
    await prompt.waitFor();
    assert.match(await prompt.innerText(), /Save changes to notes\.md\?/);
    await shot("06-close-prompt");
    await prompt.getByRole("button", { name: "Cancel" }).click();
    await prompt.waitFor({ state: "detached" });
    assert.equal(await tabDirty(page, "notes.md"), "true", "cancel keeps the dirty editor");
    await closeTab(page, "notes.md");
    await prompt.getByRole("button", { name: "Don't Save" }).click();
    await waitFor(async () => (await invoke(page, "document_list")).every((d) => d.id !== notesId), {
      message: "discarded document closed",
    });
    assert.equal(await readFile(join(project.root, "notes.md"), "utf8"), "# Notes\n\n- one\n- two\n");

    // --- path security through the commands --------------------------------
    const traversal = await invokeError(page, "document_open", { path: "../outside/secret.txt" });
    assert.ok(traversal.error, "traversal rejected");
    assert.match(String(traversal.error), /traverse/i);
    const absolute = await invokeError(page, "document_open", { path: join(project.outside, "secret.txt") });
    assert.ok(absolute.error, "absolute path rejected");
    const junction = await invokeError(page, "document_open", { path: "escape/secret.txt" });
    assert.equal(junction.error?.code, "path.outsideWorkspace", JSON.stringify(junction));
    const junctionList = await invokeError(page, "workspace_list_dir", { path: "escape" });
    assert.equal(junctionList.error?.code, "path.outsideWorkspace");
    const missing = await invokeError(page, "document_open", { path: "missing.typ" });
    assert.equal(missing.error?.code, "path.notFound");

    // --- one document per physical file (R-m1 finding) ---------------------------
    const lowerDoc = await invoke(page, "document_open", { path: "main.typ" });
    const upperDoc = await invoke(page, "document_open", { path: "MAIN.TYP" });
    assert.equal(upperDoc.state.info.id, lowerDoc.state.info.id, "case variant returns the open document");
    assert.equal(upperDoc.state.info.path, "main.typ");
    execFileSync("cmd", ["/c", "mklink", "/J", join(project.root, "chapters-link"), join(project.root, "chapters")], {
      stdio: "ignore",
    });
    await page.locator('[data-testid="file-tree"] [data-path="chapters-link"]').waitFor({ timeout: 10_000 });
    await openFromTree(page, "chapters-link/intro.tex");
    // The alias tab becomes the canonical file's tab.
    await page.locator('[data-editor-key="file:chapters/intro.tex"]').waitFor();
    await waitFor(async () => (await page.locator('[data-editor-key="file:chapters-link/intro.tex"]').count()) === 0, {
      message: "alias tab replaced",
    });
    const introDocs = (await invoke(page, "document_list")).filter((d) => d.path?.endsWith("intro.tex"));
    assert.deepEqual(introDocs.map((d) => d.path), ["chapters/intro.tex"], "one document for the file");

    // --- NUL characters: opened as text with a notice, saved, reopened -----------
    await writeFile(join(project.root, "control-chars.txt"), "a\0b\n");
    await page.locator('[data-testid="file-tree"] [data-path="control-chars.txt"]').waitFor({ timeout: 10_000 });
    const nulId = await openInEditor(page, "control-chars.txt");
    await editorLocator(page, "control-chars.txt").locator('[data-testid="nul-banner"]').waitFor();
    await focusEditor(page, "control-chars.txt", "end");
    await page.keyboard.type("c");
    await waitForText(page, nulId, "a\0b\nc");
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await readFile(join(project.root, "control-chars.txt"), "utf8")) === "a\0b\nc", { message: "nul saved" });
    await closeTab(page, "control-chars.txt");
    await waitFor(async () => (await invoke(page, "document_list")).every((d) => d.id !== nulId), { message: "nul closed" });
    assert.equal((await invoke(page, "document_open", { path: "control-chars.txt" })).state.text, "a\0b\nc", "reopens as text");

    // --- too-broad folders cannot be opened (R-m1 finding) ------------------------
    const systemRoot = process.env.SystemRoot ?? "C:\\Windows";
    const broad = await invokeError(page, "workspace_open", { path: systemRoot });
    assert.equal(broad.error?.code, "workspace.tooBroad", JSON.stringify(broad));
    const stillOpen = await invoke(page, "workspace_current");
    assert.equal(stillOpen.name, "project", "the open project stays open");
    assert.ok((await invokeError(page, "document_open", { path: "win.ini" })).error, "no system file reachable");
    await shot("07-final");

    // --- switching folders closes the previous folder's documents and tabs ------
    const other = join(project.base, "other");
    await mkdir(other, { recursive: true });
    await writeFile(join(other, "other.md"), "# Other\n");
    const otherInfo = await invoke(page, "workspace_open", { path: other });
    assert.equal(otherInfo.name, "other");
    await page.locator('[data-testid="file-tree"] [data-path="other.md"]').waitFor();
    await waitFor(async () => (await page.locator('[data-editor-key^="file:"]').count()) === 0, {
      message: "previous folder's tabs closed",
    });
    assert.deepEqual(await invoke(page, "document_list"), []);
    const recent = await invoke(page, "workspace_recent");
    assert.deepEqual(recent.slice(0, 2).map((p) => p.name), ["other", "project"], "recent projects, newest first");

    const errors = app.consoleMessages.filter((m) => m.startsWith("pageerror") || m.startsWith("error"));
    assert.deepEqual(errors, [], "no page errors");
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await app.close();
  }
}
