// Research end to end in the real app (M4): open a project, refuse kernels
// until the workspace is trusted, trust it through the shared dialog, start a
// Python session, run panel input / a file / an editor selection, see text,
// a matplotlib figure, and an inert pandas table, insert the figure into a
// Typst document and the table into Markdown, rerun with changed code and
// update the figure in place, interrupt a long run, recover from a killed
// kernel, run R and Julia in further sessions, and leave no processes.
//
// Needs Python with ipykernel/jupyter_client (+ matplotlib, pandas); R
// (IRkernel) and Julia (IJulia) are exercised when discovery reports them.
// Screenshots (light and dark) go to test-results/e2e/research-python and,
// when EXPOSITION_DEV_HOME is set, to $EXPOSITION_DEV_HOME/ui-reference/research/.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, cp, mkdir, mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { editorLocator, invoke, invokeError, openInEditor, waitFor } from "../workspace-helpers.mjs";

const KERNEL_TIMEOUT = 120_000;

async function runCommand(page, query, expectedTitle) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByRole("textbox", { name: "Command" });
  await input.waitFor({ state: "visible", timeout: 5000 });
  await input.fill(query);
  const first = page.locator(".wb-palette__item").first();
  await first.waitFor({ state: "visible" });
  const title = await first.innerText();
  if (!expectedTitle.test(title)) throw new Error(`palette top match for "${query}" was "${title}"`);
  await page.keyboard.press("Enter");
  await input.waitFor({ state: "detached", timeout: 5000 });
}

/** Command lines of processes that mention `marker` (Windows process table). */
function processesMentioning(marker) {
  if (process.platform !== "win32") return [];
  const out = execFileSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "@(Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($env:XP_MARKER) } | Select-Object -ExpandProperty CommandLine) | ConvertTo-Json -Compress",
    ],
    { encoding: "utf8", env: { ...process.env, XP_MARKER: marker } },
  ).trim();
  if (!out) return [];
  const parsed = JSON.parse(out);
  return Array.isArray(parsed) ? parsed : [parsed];
}

function killPid(pid) {
  if (process.platform === "win32") execFileSync("taskkill", ["/PID", String(pid), "/F"], { stdio: "ignore" });
  else process.kill(pid, "SIGKILL");
}

const output = (page) => page.getByTestId("research-output");

/** Run code from the panel input and wait for the new run to finish with `status`. */
async function runInPanel(page, code, status = "succeeded", timeout = KERNEL_TIMEOUT) {
  const previous = (await output(page).count()) ? await output(page).getAttribute("data-run-id") : null;
  const area = page.getByTestId("research-code");
  await area.fill(code);
  await area.focus();
  await page.keyboard.press("Control+Enter");
  const runId = await waitFor(
    async () => {
      const id = (await output(page).count()) ? await output(page).getAttribute("data-run-id") : null;
      return id && id !== previous ? id : null;
    },
    { timeout: 30_000, message: "a new run" },
  );
  await page.locator(`[data-testid="research-output"][data-run-id="${runId}"][data-status="${status}"]`).waitFor({ timeout });
  return runId;
}

async function attach(page, { document, caption, mode }) {
  await page.getByRole("button", { name: "Insert or update in document…" }).first().click();
  const group = page.getByTestId("research-attach").first();
  await group.waitFor();
  const docSelect = group.getByRole("button", { name: /Document/ }).first();
  await docSelect.waitFor();
  if (!(await docSelect.innerText()).includes(document)) {
    await docSelect.click();
    await page.getByRole("option", { name: document }).click();
  }
  if (caption !== undefined) await group.getByRole("textbox", { name: "Caption" }).fill(caption);
  if (mode === "insert") {
    await group.getByRole("button", { name: "Insert into document" }).click();
  } else {
    const update = group.getByRole("button", { name: "Update in document" });
    await update.waitFor();
    await update.click();
  }
}

export default async function researchPython({ launchApp, artifactsDir, fixture, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-research-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("research-python"), root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  const shots = [];
  const shot = async (page, name) => {
    // Reference screens: no transient notifications or open popovers.
    const dismiss = page.getByRole("button", { name: /^Dismiss notification/ });
    for (let i = 0; i < 10 && (await dismiss.count()) > 0; i++) await dismiss.first().click().catch(() => {});
    if ((await page.getByRole("listbox").count()) > 0) {
      await page.keyboard.press("Escape");
      await page.getByRole("listbox").waitFor({ state: "detached", timeout: 3000 }).catch(() => {});
    }
    await page.waitForTimeout(300);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };

  const app = await launchApp({ dataDir });
  try {
    const { page } = app;
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });

    // Untrusted workspace: no kernel may start.
    const refused = await invokeError(page, "research_session_start", {
      request: { environmentId: "python:any" },
    });
    assert.equal(refused.error?.code, "research.notTrusted", "kernels are refused in an untrusted workspace");

    await runCommand(page, "Show Research", /Show Research/);
    const panel = page.getByTestId("research-panel");
    await panel.waitFor({ timeout: 15_000 });
    // Give the panel room for output (keyboard resize, 5% per step) and hide the auxiliary region.
    await page.getByRole("separator", { name: "Resize panel" }).focus();
    for (let i = 0; i < 6; i++) await page.keyboard.press("ArrowUp");
    await runCommand(page, "Toggle Auxiliary Panel", /Toggle Auxiliary Panel/);
    await page.getByRole("button", { name: "Trust workspace…" }).click();
    await page.getByTestId("trust-dialog").waitFor();
    await page.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await page.getByRole("button", { name: "Trust workspace…" }).waitFor({ state: "detached", timeout: 10_000 });

    // Environments: real discovery.
    const report = await invoke(page, "research_environments", { refresh: false });
    const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    assert.ok(python, "a usable Python environment is discovered");
    const unavailable = report.runtimes.find((r) => r.availability.kind === "unavailable");
    const summary = report.runtimes.map((r) => `${r.displayName}: ${r.availability.kind}`);
    console.log(`environments: ${summary.join("; ")}`);

    if (unavailable) {
      const envSelect = page.getByRole("button", { name: /Research environment/ }).first();
      // The panel re-reads environments right after trust is granted, which can
      // close a picker that was just opened; reopen it until the option is chosen.
      const choose = async (pattern) => {
        for (let attempt = 1; ; attempt++) {
          await envSelect.click();
          try {
            await page.getByRole("option", { name: pattern }).click({ timeout: 5_000 });
            return;
          } catch (error) {
            if (attempt >= 4) throw error;
            await page.keyboard.press("Escape").catch(() => {});
          }
        }
      };
      await choose(new RegExp(`^${unavailable.displayName.replace(/[()+.]/g, "\\$&")}`));
      const notice = page.getByTestId("research-unavailable");
      await notice.waitFor();
      const text = await notice.innerText();
      assert.ok(text.includes(unavailable.availability.guidance.slice(0, 40)), "unavailable state shows exact guidance");
      await shot(page, "research-unavailable-light");
      await choose(new RegExp(`^${python.displayName.replace(/[()+.]/g, "\\$&")}$`));
    }

    await page.getByRole("button", { name: "Start session" }).click();
    await page.locator('[data-testid="research-session-status"][data-state="idle"]').waitFor({ timeout: KERNEL_TIMEOUT });
    const [session] = await invoke(page, "research_sessions");
    assert.equal(session.owner.kind, "user");

    // Text output.
    const textRun = await runInPanel(page, 'print("hello from research")\n6 * 7');
    const streamText = await output(page).getByTestId("research-stream").first().innerText();
    assert.equal(streamText.trim(), "hello from research");
    assert.equal((await output(page).getByTestId("research-result").first().innerText()).trim(), "42");

    // Run File from the editor: matplotlib figure.
    await openInEditor(page, "analysis/plot.py");
    await runCommand(page, "Run File in Research", /Run File in Research Session/);
    const fileRun = await waitFor(
      async () => {
        const id = await output(page).getAttribute("data-run-id");
        return id && id !== textRun ? id : null;
      },
      { timeout: 30_000, message: "the file run" },
    );
    await page.locator(`[data-testid="research-output"][data-run-id="${fileRun}"][data-status="succeeded"]`).waitFor({ timeout: KERNEL_TIMEOUT });
    const fileManifest = await invoke(page, "research_run", { runId: fileRun });
    assert.equal(fileManifest.code.origin.kind, "file");
    assert.equal(fileManifest.code.origin.path, "analysis/plot.py");
    const plotBytes = await readFile(join(root, "analysis", "plot.py"));
    const [plotInput] = fileManifest.record.inputs;
    assert.equal(plotInput.contentHash, `sha256:${createHash("sha256").update(plotBytes).digest("hex")}`, "input pinned by content hash");
    assert.equal(typeof plotInput.revision, "number", "the open buffer's revision is recorded");
    const image = output(page).getByTestId("research-figure-image");
    await image.waitFor({ timeout: 30_000 });
    await waitFor(() => image.evaluate((img) => img.complete && img.naturalWidth > 0), { message: "figure pixels" });
    assert.match(await image.getAttribute("src"), /^blob:/, "figures are shown from blob URLs");

    // Run Selection from the editor (third code line: `xs = list(range(10))`).
    await editorLocator(page, "analysis/plot.py").locator(".monaco-editor .view-lines").click();
    await page.keyboard.press("Control+Home");
    for (let i = 0; i < 3; i++) await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Home");
    await page.keyboard.press("Shift+End");
    await runCommand(page, "Run Selection in Research", /Run Selection in Research Session/);
    const selectionRun = await waitFor(
      async () => {
        const id = await output(page).getAttribute("data-run-id");
        return id && id !== fileRun ? id : null;
      },
      { timeout: 30_000, message: "the selection run" },
    );
    await page.locator(`[data-testid="research-output"][data-run-id="${selectionRun}"][data-status="succeeded"]`).waitFor({ timeout: KERNEL_TIMEOUT });
    const selectionManifest = await invoke(page, "research_run", { runId: selectionRun });
    assert.equal(selectionManifest.code.origin.kind, "selection");
    assert.equal(selectionManifest.code.text, "xs = list(range(10))");
    assert.equal(typeof selectionManifest.record.inputs[0].revision, "number", "selection pinned to a revision");

    // Pandas table, rendered as an inert grid.
    await runInPanel(page, 'import pandas as pd\npd.DataFrame({"n": [1, 2, 3], "square": [1, 4, 9]})');
    const table = output(page).getByTestId("research-table");
    await table.locator("table").waitFor({ timeout: 30_000 });
    assert.deepEqual(await table.locator("thead th").allInnerTexts(), ["", "n", "square"]);
    // Untrusted HTML: scripts and handlers never run in the app.
    await runInPanel(
      page,
      'from IPython.display import HTML, display\ndisplay(HTML("<table><tr><td><img src=x onerror=\\"document.title=\'pwned\'\\">cell</td></tr></table><script>document.title=\'pwned\'</script>"))',
    );
    await output(page).getByTestId("research-table").locator("td").first().waitFor();
    assert.equal(await output(page).getByTestId("research-table").locator("td").first().innerText(), "cell");
    assert.notEqual(await page.title(), "pwned", "kernel HTML never executes");
    assert.equal(await page.locator('img[src="x"]').count(), 0, "kernel HTML never reaches the live page");

    // Insert the figure into the Typst paper.
    const paperId = await openInEditor(page, "paper.typ");
    const paperBefore = (await invoke(page, "document_state", { document: paperId })).text;
    await page.locator(`[data-testid="research-history"] [data-run-id="${fileRun}"]`).click();
    await output(page).getByTestId("research-figure-image").waitFor();
    await attach(page, { document: "paper.typ", caption: "Squares of n", mode: "insert" });
    const inserted = await waitFor(
      async () => {
        const text = (await invoke(page, "document_state", { document: paperId })).text;
        return text.includes("// exposition:begin fig-") ? text : null;
      },
      { timeout: 15_000, message: "figure block in paper.typ" },
    );
    const key = inserted.match(/\/\/ exposition:begin (\S+)/)[1];
    assert.ok(inserted.includes(`image(".exposition/artifacts/${fileRun}/output-1.png", width: 80%)`), "figure path relative to the document");
    assert.ok(inserted.includes("caption: [Squares of n]"));
    assert.ok(inserted.startsWith(paperBefore), "insert appends without touching existing text");

    // Edit elsewhere in the paper, rerun with changed code, and update the figure in place.
    const edited = inserted.replace("small $n$", "small integers $n$");
    const state = await invoke(page, "document_state", { document: paperId });
    const at = state.text.indexOf("small $n$");
    await invoke(page, "document_apply", {
      request: {
        id: "e2e-edit",
        origin: { kind: "source" },
        edits: [{ document: paperId, baseRevision: state.info.revision, patches: [{ range: { start: at, end: at + "small $n$".length }, text: "small integers $n$" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    const cubesRun = await runInPanel(
      page,
      'import matplotlib.pyplot as plt\nxs = list(range(10))\nplt.figure(figsize=(4, 3))\nplt.plot(xs, [x ** 3 for x in xs], marker="s")\nplt.title("Cubes")\nplt.show()',
    );
    await output(page).getByTestId("research-figure-image").waitFor({ timeout: 30_000 });
    await attach(page, { document: "paper.typ", mode: "update" });
    const updated = await waitFor(
      async () => {
        const text = (await invoke(page, "document_state", { document: paperId })).text;
        return text.includes(cubesRun) ? text : null;
      },
      { timeout: 15_000, message: "updated figure block" },
    );
    assert.equal(
      updated,
      edited.replace(`.exposition/artifacts/${fileRun}/output-1.png`, `.exposition/artifacts/${cubesRun}/output-1.png`),
      "update replaces exactly the keyed block; other edits and the caption are preserved",
    );
    assert.ok(updated.includes(`<${key}>`));
    await invoke(page, "document_save", { document: paperId, overwrite: false });
    assert.equal(await readFile(join(root, "paper.typ"), "utf8"), updated, "saved Typst source matches");

    // Insert the table into Markdown notes.
    const notesId = await openInEditor(page, "notes.md");
    await runInPanel(page, 'import pandas as pd\npd.DataFrame({"n": [1, 2, 3], "cube": [1, 8, 27]})');
    await output(page).getByTestId("research-table").locator("table").waitFor({ timeout: 30_000 });
    await attach(page, { document: "notes.md", caption: "Cubes", mode: "insert" });
    const notes = await waitFor(
      async () => {
        const text = (await invoke(page, "document_state", { document: notesId })).text;
        return text.includes("<!-- exposition:begin tab-") ? text : null;
      },
      { timeout: 15_000, message: "table block in notes.md" },
    );
    assert.ok(notes.includes("|  | n | cube |\n| ---: | ---: | ---: |\n| 0 | 1 | 1 |"), notes);
    assert.ok(notes.includes("Table: Cubes"));
    await page.getByTestId("research-panel").scrollIntoViewIfNeeded();
    await shot(page, "research-light");

    // Provenance on disk.
    const manifest = JSON.parse(await readFile(join(root, ".exposition", "runs", `${cubesRun}.json`), "utf8"));
    assert.equal(manifest.format, "exposition.run/1");
    assert.equal(manifest.record.status.kind, "succeeded");
    assert.ok(manifest.record.environment.some((t) => t.name === "matplotlib"));
    assert.ok(manifest.record.outputs.some((a) => a.kind === "figure" && a.contentHash.startsWith("sha256:")));

    // Interrupt a long run.
    const area = page.getByTestId("research-code");
    await area.fill("import time\ntime.sleep(60)");
    await area.focus();
    await page.keyboard.press("Control+Enter");
    await page.locator('[data-testid="research-output"][data-status="running"]').waitFor({ timeout: 30_000 });
    await page.locator('[data-testid="research-session-status"][data-state="busy"]').waitFor({ timeout: 30_000 });
    await page.waitForTimeout(500);
    const interruptAt = Date.now();
    await page.getByRole("button", { name: "Interrupt", exact: true }).click();
    await page.locator('[data-testid="research-output"][data-status="interrupted"]').waitFor({ timeout: 20_000 });
    assert.ok(Date.now() - interruptAt < 15_000, "interrupt stops a blocking sleep promptly");

    // Kill the kernel: crash detected, restart recovers with a fresh kernel.
    const [live] = await invoke(page, "research_sessions");
    killPid(live.kernelPid);
    await page.getByTestId("research-crash-reason").waitFor({ timeout: 30_000 });
    assert.match(await page.getByTestId("research-crash-reason").innerText(), /kernel/i);
    await shot(page, "research-crashed-light");
    await page.getByRole("button", { name: "Restart with a fresh kernel" }).click();
    await page.locator('[data-testid="research-session-status"][data-state="idle"]').waitFor({ timeout: KERNEL_TIMEOUT });
    await runInPanel(page, "21 * 2");
    assert.equal((await output(page).getByTestId("research-result").first().innerText()).trim(), "42");

    // R and Julia in further user sessions (the webview cannot create agent
    // sessions: an owner it sends is ignored).
    for (const [language, code, expected] of [
      ["r", "x <- c(1, 2, 3)\ncat(sum(x), '\\n')", "6"],
      ["julia", "println(1 + 2)", "3"],
    ]) {
      const env = report.runtimes.find((r) => r.language === language && r.availability.kind === "available");
      if (!env) {
        console.log(`${language}: unavailable (${report.runtimes.find((r) => r.language === language)?.availability.guidance})`);
        continue;
      }
      const other = await invoke(page, "research_session_start", {
        request: { environmentId: env.id, owner: { kind: "agent", taskId: "forged" } },
      });
      assert.equal(other.owner.kind, "user", "webview sessions are always the user's");
      const run = await invoke(page, "research_execute", {
        request: { sessionId: other.id, source: { kind: "input", code } },
      });
      const done = await waitFor(
        async () => {
          const m = await invoke(page, "research_run", { runId: run.id });
          return m.record.status.kind !== "running" ? m : null;
        },
        { timeout: KERNEL_TIMEOUT, message: `${language} run` },
      );
      assert.equal(done.record.status.kind, "succeeded", `${language} run succeeded`);
      const page1 = await invoke(page, "research_run_output", { runId: run.id, after: null, limit: 100 });
      const text = page1.entries.filter((e) => e.item.kind === "stream").map((e) => e.item.text).join("");
      assert.equal(text.trim(), expected, `${language} output`);
      console.log(`${language}: ${env.displayName} ran "${code.split("
").pop()}" -> ${text.trim()}`);
      await invoke(page, "research_session_shutdown", { sessionId: other.id });
    }

    // Dark theme reference screen.
    await page.locator(`[data-testid="research-history"] [data-run-id="${cubesRun}"]`).click();
    await output(page).getByTestId("research-figure-image").waitFor();
    await runCommand(page, "color theme dark", /Color Theme: Dark/);
    await shot(page, "research-dark");
    await runCommand(page, "color theme light", /Color Theme: Light/);

    // Stop the session: no helper or kernel process may remain.
    await page.getByRole("button", { name: "Stop session" }).click();
    await page.locator('[data-testid="research-session-status"][data-state="none"]').waitFor({ timeout: 30_000 });
    // Helpers and kernels carry the research data directory in their command
    // lines (helper script, connection files); the webview's own profile does not.
    const researchDir = join(dataDir, "research");
    await waitFor(() => processesMentioning(researchDir).length === 0, { timeout: 20_000, message: "no research processes left" });
    assert.ok(info.root, "workspace opened");
    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await app.close();
  }

  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "research");
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${shots.length} screenshots to ${target}`);
  }
}
