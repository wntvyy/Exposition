// Export PDF from a page view to a destination the person chooses (QC 2.4),
// in the real app with the real native save dialog (Windows IFileDialog,
// driven through UI Automation: scripts/e2e/native-dialog.ps1).
//
// Drives: the Export PDF toolbar button of the page view → the backend's
// "Export PDF" save dialog (its suggested name, Cancel, a typed destination,
// the system's "Confirm Save As" prompt answered No and Yes) → the
// notification. Checks the files on disk: a destination outside the project
// with spaces and non-ASCII characters, cancel and a refused overwrite write
// nothing, a failed export (a file that is not a PDF; a file another program
// holds open) leaves the existing file as it was, an export of a source with
// errors says it is the last successful build, the default destination is
// next to the entry, and the export changes no document, no project file,
// and no graph record. Over IPC: a caller-supplied destination outside the
// project and a build of another document are refused.
// Everything is in temporary folders.
import { spawn } from "node:child_process";
import { cp, mkdir, mkdtemp, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { withNativeDialog } from "../native-dialog.mjs";
import { waitForAsync } from "../wait.mjs";
import { invoke, invokeError, openFromTree, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const PATH = "main.typ";
const TITLE = "Export PDF";

/** Every file under `dir` (relative path → size and modification time). */
async function listing(dir, prefix = "") {
  const out = {};
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    // The graph database and its journals change on their own schedule.
    if (rel === ".exposition") continue;
    if (entry.isDirectory()) Object.assign(out, await listing(join(dir, entry.name), rel));
    else {
      const info = await stat(join(dir, entry.name));
      out[rel] = `${info.size}:${info.mtimeMs}`;
    }
  }
  return out;
}

const pdfPages = (bytes) => [...bytes.toString("latin1").matchAll(/\/Type\s*\/Page(?!s)/g)].length;

export default async function pageExport({ launchApp, artifactsDir, assert, fixture }) {
  if (process.platform !== "win32") throw new Error("page-export drives the Windows save dialog");
  const base = await mkdtemp(join(tmpdir(), "exposition-export-"));
  const workspace = join(base, "project");
  // Spaces and non-ASCII characters in the destination folder and file name.
  const outside = join(base, "Mes exports été 数学");
  await cp(fixture("typst-basic"), workspace, { recursive: true });
  await mkdir(outside, { recursive: true });
  const app = await launchApp();
  const { page } = app;
  const pid = app.child.pid;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  const notes = [];
  const note = (name, value) => notes.push({ name, value });
  let holder = null;

  try {
    await openWorkspace(page, workspace);
    await openFromTree(page, PATH);
    const modeButton = (label) => page.locator(".ui-segmented__item", { hasText: label }).first();
    await modeButton("Page").waitFor({ timeout: 10_000 });
    await modeButton("Page").click();
    const editor = page.locator('[data-testid="page-editor"]');
    await editor.waitFor({ timeout: 10_000 });
    const settled = () =>
      waitForAsync(
        page,
        async (path) => {
          const el = document.querySelector('[data-testid="page-editor"]');
          const img = document.querySelector('[data-testid="page"] img');
          if (!(el?.dataset.stale === "false" && el.dataset.build && img?.complete)) return false;
          const docs = await window.__TAURI_INTERNALS__.invoke("document_list");
          const doc = docs.find((d) => d.path === path);
          return !!doc && el.dataset.buildRevision === String(doc.revision);
        },
        PATH,
        { timeout: 20_000 },
      );
    await settled();
    const shownPages = await page.locator('[data-testid="page"]').count();
    const doc = (await invoke(page, "document_list")).find((d) => d.path === PATH);
    const exportButton = editor.getByRole("button", { name: TITLE });
    const notices = () =>
      page.locator('[data-testid="notification"]').evaluateAll((items) =>
        items.map((n) => ({
          title: n.querySelector(".ui-notification__title")?.textContent ?? "",
          body: n.querySelector(".ui-notification__body")?.textContent ?? "",
        })),
      );
    const dismissAll = async () => {
      for (const button of await page.locator('[data-testid="notification"] .ui-notification__dismiss').all()) {
        await button.click().catch(() => {});
      }
      await waitFor(async () => (await notices()).length === 0, { message: "notifications dismissed" });
    };
    const notice = (title) =>
      waitFor(async () => (await notices()).find((n) => n.title === title), { timeout: 20_000, message: `notification ${title}` });
    /** Press Export PDF and drive the save dialog. */
    const exportVia = (options) => withNativeDialog({ title: TITLE, save: true, pid, ...options }, () => exportButton.click());
    // The graph's counts, once the project's first reconciliation is done.
    const graphCounts = async () => {
      const status = await invoke(page, "graph_status");
      return { state: status.state, revision: status.revision, entities: status.entities, relations: status.relations };
    };
    let graphBefore = await graphCounts();
    await waitFor(
      async () => {
        await new Promise((r) => setTimeout(r, 1000));
        const now = await graphCounts();
        const stable = JSON.stringify(now) === JSON.stringify(graphBefore);
        graphBefore = now;
        return stable;
      },
      { timeout: 30_000, interval: 0, message: "the graph to settle" },
    );
    const projectBefore = await listing(workspace);

    // 1. Cancel: the dialog suggests `<entry stem>.pdf` in the entry's
    //    folder; nothing is written and nothing is reported.
    const canceled = await exportVia({ cancel: true });
    note("cancel", canceled);
    assert.ok(canceled.found && canceled.closed, `the save dialog opened and closed: ${JSON.stringify(canceled)}`);
    // Windows shows the suggested name without a known extension when the
    // person hides extensions; the PDF filter adds `.pdf` either way (step 6).
    assert.match(String(canceled.suggested), /^main(\.pdf)?$/, "the dialog suggests <entry stem>.pdf");
    assert.equal(String(canceled.folder).toLowerCase(), workspace.toLowerCase(), "the dialog starts in the entry's folder");
    await page.waitForTimeout(500);
    assert.deepEqual(await notices(), [], "a canceled export reports nothing");
    assert.deepEqual(await listing(workspace), projectBefore, "cancel wrote nothing in the project");
    assert.deepEqual(await readdir(outside), [], "cancel wrote nothing outside");

    // 2. A destination outside the project (spaces, non-ASCII).
    const target = join(outside, "Thèse finale (v2).pdf");
    const first = await exportVia({ path: target });
    note("outside", first);
    assert.ok(first.closed && !first.overwritePrompt, `saved without a prompt: ${JSON.stringify(first)}`);
    const exported = await notice("Exported PDF");
    assert.equal(exported.body, target, "the notification names the chosen file");
    const firstBytes = await readFile(target);
    assert.ok(firstBytes.subarray(0, 5).toString() === "%PDF-", "the chosen file is a PDF");
    assert.equal(pdfPages(firstBytes), shownPages, "the PDF has the pages shown");
    assert.deepEqual(await readdir(outside), ["Thèse finale (v2).pdf"], "only the chosen file was written (no temporary file)");
    // An output copy: no project file, no document, no graph record.
    assert.deepEqual(await listing(workspace), projectBefore, "the project's files are unchanged");
    const docAfter = (await invoke(page, "document_list")).find((d) => d.path === PATH);
    assert.deepEqual(docAfter, doc, "the document is unchanged (not saved, not modified)");
    assert.equal((await invoke(page, "document_list")).length, 1, "no document was opened for the export");
    await page.waitForTimeout(1500);
    assert.deepEqual(await graphCounts(), graphBefore, "the graph is unchanged (no entity for the exported file)");
    await shot("01-exported-outside");
    await dismissAll();

    // 3. The same destination again: the system asks; No keeps the file.
    const firstStat = await stat(target);
    const refused = await exportVia({ path: target, overwrite: "No" });
    note("overwrite-refused", refused);
    assert.equal(refused.overwritePrompt, true, `the system asked before replacing: ${JSON.stringify(refused)}`);
    assert.ok(refused.closed, "the dialog was closed after No");
    await page.waitForTimeout(500);
    assert.deepEqual(await notices(), [], "a refused overwrite reports nothing");
    assert.equal((await stat(target)).mtimeMs, firstStat.mtimeMs, "the file was not rewritten");
    assert.ok((await readFile(target)).equals(firstBytes), "the file is as it was");

    // 4. The source gets an error: the pages stay on the last successful
    //    build, the export says so, and Yes replaces the earlier export.
    const state = await invoke(page, "document_state", { document: doc.id });
    await invoke(page, "document_apply", {
      request: {
        id: `e2e-export-${Date.now()}`,
        origin: { kind: "source" },
        edits: [{ document: doc.id, baseRevision: state.info.revision, patches: [{ range: { start: 0, end: 0 }, text: "#nosuch\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await page.locator('[data-testid="page-build-status"]').filter({ hasText: /Build failed/ }).waitFor({ timeout: 20_000 });
    const lastGood = await editor.getAttribute("data-build");
    const replaced = await exportVia({ path: target, overwrite: "Yes" });
    note("overwrite-accepted", replaced);
    assert.equal(replaced.overwritePrompt, true, "the system asked before replacing");
    const outdated = await notice("Exported the last successful build");
    assert.ok(outdated.body.startsWith(target), `the notification names the file: ${outdated.body}`);
    assert.match(outdated.body, /does not include the latest changes/);
    const secondBytes = await readFile(target);
    assert.equal(pdfPages(secondBytes), shownPages, "the last successful build was exported");
    assert.equal(await editor.getAttribute("data-build"), lastGood, "the view still shows the last successful build");
    const dirty = (await invoke(page, "document_list")).find((d) => d.path === PATH);
    assert.notEqual(dirty.savedRevision, dirty.revision, "the unsaved source edit was not saved by the export");
    assert.ok((await readFile(join(workspace, PATH), "utf8")).indexOf("#nosuch") === -1, "main.typ on disk is unchanged");
    await shot("02-exported-last-good");
    await dismissAll();
    await invoke(page, "document_undo", { document: doc.id, origin: { kind: "source" } });
    await settled();

    // 5. Failures keep the existing file. A file that is not a PDF is never
    //    replaced, even after Yes in the system prompt.
    const notesFile = join(outside, "notes.pdf");
    await writeFile(notesFile, "my notes, not a PDF");
    const notPdf = await exportVia({ path: notesFile, overwrite: "Yes" });
    note("not-a-pdf", notPdf);
    const failed = await notice("PDF export failed");
    assert.match(failed.body, /not a PDF/, failed.body);
    assert.equal(await readFile(notesFile, "utf8"), "my notes, not a PDF");
    await dismissAll();
    //    A destination another program holds open (as a PDF viewer does).
    holder = spawn(
      "powershell.exe",
      ["-NoProfile", "-NonInteractive", "-Command", "$f = [System.IO.File]::Open($env:XP_HOLD, 'Open', 'Read', 'Read'); 'held'; Start-Sleep -Seconds 120; $f.Close()"],
      { env: { ...process.env, XP_HOLD: target }, stdio: ["ignore", "pipe", "ignore"], windowsHide: true },
    );
    await new Promise((resolve, reject) => {
      holder.stdout.on("data", (chunk) => String(chunk).includes("held") && resolve());
      holder.on("exit", () => reject(new Error("the file holder exited")));
      setTimeout(() => reject(new Error("the file holder did not start")), 30_000);
    });
    const busy = await exportVia({ path: target, overwrite: "Yes" });
    note("busy", busy);
    const busyNotice = await notice("PDF export failed");
    note("busy-message", busyNotice.body);
    assert.match(busyNotice.body, /another program/, busyNotice.body);
    assert.match(busyNotice.body, /The existing file was not changed\./, busyNotice.body);
    holder.kill();
    holder = null;
    assert.ok((await readFile(target)).equals(secondBytes), "the earlier export is intact after the failed one");
    assert.deepEqual((await readdir(outside)).sort(), ["Thèse finale (v2).pdf", "notes.pdf"].sort(), "no temporary file is left");
    await shot("03-export-failed");
    await dismissAll();

    // 6. The suggested destination: next to the entry, in the project.
    const inProject = await exportVia({});
    note("default", inProject);
    assert.match(String(inProject.suggested), /^main(\.pdf)?$/);
    const defaultNotice = await notice("Exported PDF");
    assert.equal(defaultNotice.body.toLowerCase(), join(workspace, "main.pdf").toLowerCase(), "exported next to the entry");
    assert.equal(pdfPages(await readFile(join(workspace, "main.pdf"))), shownPages);
    await dismissAll();

    // 7. Over IPC the destination is never a caller's choice outside the
    //    project, and a build is exported only as its own document.
    const build = await editor.getAttribute("data-build");
    const sneaky = join(outside, "from-ipc.pdf");
    const outsideIpc = await invokeError(page, "typesetting_export_pdf", { build, entry: PATH, destination: sneaky });
    assert.equal(outsideIpc.error?.code, "typesetting.export", JSON.stringify(outsideIpc));
    assert.match(outsideIpc.error.message, /outside the project/);
    await writeFile(join(workspace, "other.typ"), "= Other\n");
    const wrongEntry = await invokeError(page, "typesetting_export_pdf", { build, entry: "other.typ", destination: join(workspace, "other.pdf") });
    assert.equal(wrongEntry.error?.code, "typesetting.export", JSON.stringify(wrongEntry));
    assert.match(wrongEntry.error.message, /not of other\.typ/);
    const missingEntry = await invokeError(page, "typesetting_export_pdf", { build, destination: sneaky });
    assert.ok(missingEntry.error, "the entry is required");
    assert.deepEqual((await readdir(outside)).sort(), ["Thèse finale (v2).pdf", "notes.pdf"].sort(), "nothing was written outside over IPC");
    assert.ok(!(await readdir(workspace)).includes("other.pdf"), "the other document got no PDF");
  } finally {
    holder?.kill();
    await writeFile(join(artifactsDir, "notes.json"), JSON.stringify(notes, null, 2)).catch(() => {});
    await app.close();
  }
}
