// Workspace files end to end in the real app: open a folder, browse the tree,
// open files in Monaco, type, save with Ctrl+S, verify exact bytes on disk
// (LF, CRLF, UTF-8 BOM, UTF-16), undo/redo through the shared document
// service, mixed line endings, unavailable files, close/reopen, and rejection of
// traversal and junction escapes.
import { readFile } from "node:fs/promises";
import { join } from "node:path";

import {
  documentState,
  editorLocator,
  focusEditor,
  invoke,
  invokeError,
  makeProject,
  openFromTree,
  openInEditor,
  openWorkspace,
  tabDirty,
  waitFor,
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
    for (const hidden of [".git", "node_modules", "target"]) assert.ok(!topLevel.includes(hidden), `${hidden} hidden`);
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
    const monacoText = await editorLocator(page, "main.typ").locator(".view-lines").innerText();
    assert.ok(!monacoText.includes("abc"), "Monaco shows the undone text");
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
    await cdp.detach();

    // --- close (clean, after saving) and reopen ----------------------------
    await page.keyboard.press("Control+S");
    await waitFor(async () => (await tabDirty(page, "main.typ")) === "false", { message: "saved" });
    await page.locator('[data-testid="editor-tab-close"]').first().click();
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
    await page.locator('[data-testid="source-editor-unavailable"]').waitFor();
    assert.match(await page.locator('[data-testid="source-editor-unavailable"]').innerText(), /binary/i);
    await openFromTree(page, "encoding/latin1.txt");
    await page.locator('[data-testid="source-editor-unavailable"]').getByText(/not valid UTF-8/).waitFor();
    assert.deepEqual(await readFile(join(project.root, "encoding", "latin1.txt")), Buffer.from("caf\xe9 au lait\n", "latin1"));
    await shot("05-unavailable");

    // --- close a dirty editor: Cancel keeps it, Don't Save discards -------
    await openFromTree(page, "notes.md");
    const notesId = await waitFor(() => editorLocator(page, "notes.md").getAttribute("data-document-id"));
    await editorLocator(page, "notes.md").locator(".monaco-editor").waitFor();
    await focusEditor(page, "notes.md", "end");
    await page.keyboard.type("- three");
    await waitFor(async () => (await tabDirty(page, "notes.md")) === "true", { message: "notes dirty" });
    const notesClose = page.locator('[data-testid="editor-tab"][data-key="file:notes.md"] + [data-testid="editor-tab-close"]');
    await notesClose.click();
    const prompt = page.locator('[data-testid="prompt"]');
    await prompt.waitFor();
    assert.match(await prompt.innerText(), /Save changes to notes\.md\?/);
    await shot("06-close-prompt");
    await prompt.locator('[data-prompt-button="cancel"]').click();
    await prompt.waitFor({ state: "detached" });
    assert.equal(await tabDirty(page, "notes.md"), "true", "cancel keeps the dirty editor");
    await notesClose.click();
    await prompt.locator('[data-prompt-button="discard"]').click();
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
    await shot("07-final");

    const errors = app.consoleMessages.filter((m) => m.startsWith("pageerror") || m.startsWith("error"));
    assert.deepEqual(errors, [], "no page errors");
  } catch (error) {
    await shot("failure").catch(() => {});
    throw error;
  } finally {
    await app.close();
  }
}
