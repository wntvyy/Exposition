// Stabilization (lane st/state): open workflow views stay current with
// changes made elsewhere, in the real app with real restricted runs.
//
// T. D1 (verifier): an open Formal Audit draft in an untrusted folder becomes
//    reviewable when the folder is trusted from the Lean panel — no save.
// A. XR-K-03: already-open Python, R, and Julia workflow editors (no images
//    configured) become reviewable, and their step text says the runner is
//    available, when Settings › Execution sets each image — no save, the
//    revision unchanged; clearing the Python image blocks it again. The
//    Research page's Restricted research row follows every language, and
//    Refresh environments re-reads it after an out-of-band settings change.
// B. XR-E-03: the mounted Workflows list shows a draft made with its own New
//    Draft, then its new title and revision after a save in the editor.
// C. XR-E-02: an open composite parent offers "Use latest revision (2)" after
//    its child is saved in another tab; the pin moves only when chosen, as an
//    unsaved edit that Save makes a revision.
// D. XR-K-02: File: Close Folder while a restricted run is running: the run
//    is canceled (workflow.projectClosed, containers removed) and the open run
//    tab says its project is not open (no Cancel run); reopening the project
//    from Recent shows the canceled state in the same tab.
// E. E16: a visible project switch (File: New Project…) — the mounted
//    Workflows list shows the new project's drafts and runs only, the run tab
//    is unavailable; switching back (Close Folder, Recent) shows them again.
//
// Fixture workflows are saved through IPC (setup); every checked behavior is
// driven through visible controls. Requires Docker with python:3.13-slim,
// r-base:4.6.2 and julia:1.13 present locally (never pulled); otherwise
// SKIPPED unless EXPOSITION_REQUIRE_DOCKER=1.
// Observations: test-results/e2e/workflow-state-refresh/observations.json;
// screens copied to $EXPOSITION_DEV_HOME/ui-reference/workflows-state/.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { alertDialog, invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";

const IMAGES = { python: "python:3.13-slim", r: "r-base:4.6.2", julia: "julia:1.13" };
const OBS = "x,y\n0,1\n1,3\n2,5\n";
const STEP_WAIT = 20_000;
const LEAN = "theorem two_eq : 1 + 1 = 2 := rfl\n";

function dockerImagesPresent() {
  try {
    for (const image of Object.values(IMAGES)) execFileSync("docker", ["image", "inspect", image], { stdio: "ignore", timeout: 30_000 });
    return true;
  } catch {
    return false;
  }
}

function managedContainers() {
  try {
    return execFileSync("docker", ["ps", "-aq", "--filter", "label=org.exposition.managed"], { encoding: "utf8", timeout: 30_000 })
      .split(/\s+/)
      .filter(Boolean);
  } catch {
    return ["<docker ps failed>"];
  }
}

const dataInput = { id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } };
const ceiling = { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false };

/** A one-step restricted batch workflow in `language` over the declared data. */
function batchWorkflow(title, operation, language, source, timeoutSeconds = 120) {
  return {
    schema: "exposition.workflow/1",
    title,
    description: "",
    preset: null,
    inputs: [dataInput],
    steps: [
      {
        id: "run",
        label: "Run the code",
        operation: { id: operation, version: 1 },
        parameters: { timeoutSeconds: { kind: "integer", value: timeoutSeconds } },
        payload: { kind: "code", language, source },
      },
    ],
    connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } }],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling,
  };
}

const SLOW_PY = `import os, time
time.sleep(25)
open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("k,v\\nslow,1\\n")
`;
const QUICK_PY = `import os
open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("k,v\\nfit,1\\n")
`;
const R_CODE = `write.csv(data.frame(k = "r", v = 1), file.path(Sys.getenv("EXPOSITION_OUTPUT_DIR"), "dataset.csv"), row.names = FALSE)\n`;
const JL_CODE = `open(joinpath(ENV["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w") do io; write(io, "k,v\\njulia,1\\n"); end\n`;

export default async function workflowStateRefresh({ launchApp, artifactsDir, assert }) {
  if (!dockerImagesPresent()) {
    const message = `SKIPPED: Docker with the local images ${Object.values(IMAGES).join(", ")} is not available`;
    if (process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-wf-state-"));
  const alpha = join(base, "alpha");
  const beta = join(base, "beta");
  const dataDir = join(base, "data");
  await mkdir(join(alpha, "data"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(alpha, "data", "obs.csv"), OBS);
  await mkdir(join(alpha, "lean"), { recursive: true });
  await writeFile(join(alpha, "lean", "Two.lean"), LEAN);

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 600)}`);
  };
  const shots = [];
  const app = await launchApp({ dataDir });
  const page = app.page;
  const shot = async (name) => {
    await page.waitForTimeout(250);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path }).catch(() => {});
    shots.push(path);
  };
  const visible = (testId) => page.locator(`[data-testid="${testId}"]:visible`).first();
  const tab = (key) => page.locator(`[data-editor-key="${key}"]`).first();
  const until = async (message, check, timeout = STEP_WAIT) =>
    waitFor(check, { timeout, interval: 250, message }).then(
      () => true,
      () => false,
    );
  const section = async (name, body) => {
    try {
      await body();
    } catch (e) {
      note(`${name}-section-ran`, false, String(e?.stack ?? e).slice(0, 800));
      await shot(`failure-${name}`).catch(() => {});
      await page.keyboard.press("Escape").catch(() => {});
    }
  };
  const openSettingsExecution = async () => {
    await runCommand(page, "Open Settings", /Preferences: Open Settings/);
    await page.getByRole("tab", { name: "Execution", exact: true }).click();
    await page.getByRole("tabpanel", { name: "Execution" }).waitFor();
  };
  /** Choose `image` in the Settings › Execution image row `rowTestId` (visible controls). */
  const chooseImage = async (rowTestId, image) => {
    const button = page.getByTestId(rowTestId).locator(".ui-select__button").first();
    await until("image list loaded", async () => !(await button.isDisabled()));
    await button.click();
    await page.getByRole("option", { name: new RegExp(`^${image.replace(/[.]/g, "\\.")}`) }).first().click();
    await page.getByTestId(`${rowTestId}-state`).filter({ hasText: `${image} is present locally` }).waitFor({ timeout: STEP_WAIT });
  };
  const editorState = async () => {
    const editor = visible("workflow-editor");
    return {
      revision: await editor.getAttribute("data-revision"),
      canReview: await visible("workflow-status").getAttribute("data-can-review"),
      step: (await visible("workflow-step-run").innerText().catch(() => "")).replace(/\s+/g, " "),
    };
  };
  const showEditor = async (id) => {
    await tab(`workflow:${id}`).click();
    await page.locator(`[data-testid="workflow-editor"][data-workflow="${id}"]:visible`).waitFor({ timeout: STEP_WAIT });
  };
  const restrictedRow = () => page.locator('[data-testid="research-execution-modes"]:visible [data-mode="restricted"]').first();
  const languageAvailable = async (key) => page.locator(`[data-testid="research-restricted-${key}"]:visible`).first().getAttribute("data-available").catch(() => null);

  let saved = {};
  let runId = null;
  // Evidence: the availability reads the UI makes (Tauri IPC requests) and their answers;
  // no workflow_save happens in section A.
  const ipcLog = [];
  const t0 = Date.now();
  page.on("requestfinished", async (request) => {
    const cmd = /ipc\.localhost\/([a-z_]+)/.exec(request.url())?.[1];
    if (!cmd || !/^workflow_(operations|validate|save|get|list)$|^execution_settings_update$/.test(cmd)) return;
    const at = Date.now() - t0;
    let body = null;
    try {
      body = await (await request.response())?.json();
    } catch {}
    const python = cmd === "workflow_operations" && Array.isArray(body) ? body.find((o) => o.id === "research.pythonBatch")?.runner.available : undefined;
    const runnable = cmd === "workflow_validate" ? body?.runnable : cmd === "workflow_get" ? body?.validation?.runnable : undefined;
    const image = cmd === "execution_settings_update" ? body?.settings?.docker : undefined;
    ipcLog.push({ at, cmd, python, runnable, image });
  });
  try {
    await page.setViewportSize({ width: 1600, height: 1000 }).catch(() => {});
    await invoke(page, "workspace_open", { path: alpha });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    await invoke(page, "research_space_enable").catch(() => {});
    const settings0 = await invoke(page, "execution_settings_get");
    note("setup-no-images", !settings0.settings.docker.image && !settings0.settings.docker.rImage && !settings0.settings.docker.juliaImage, settings0.settings.docker);
    const save = (definition) => invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition } });
    saved.py = await save(batchWorkflow("Slow Python", "research.pythonBatch", "python", SLOW_PY));
    saved.r = await save(batchWorkflow("R batch", "research.rBatch", "r", R_CODE));
    saved.jl = await save(batchWorkflow("Julia batch", "research.juliaBatch", "julia", JL_CODE));
    saved.child = await save({ ...batchWorkflow("Fit model", "research.pythonBatch", "python", QUICK_PY), inputs: [{ ...dataInput, id: "obs" }], connections: [{ kind: "data", from: { kind: "input", input: "obs" }, to: { step: "run", port: "data" } }] });
    saved.parent = await save({
      schema: "exposition.workflow/2",
      title: "Study",
      description: "",
      preset: null,
      inputs: [dataInput],
      steps: [{ id: "comp", label: "Fit model", operation: { id: "workflow.composite", version: 1 }, parameters: {}, payload: null, composite: { workflow: saved.child.id, revision: 1 } }],
      connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "comp", port: "obs" } }],
      outcomes: [{ id: "fitted", kind: "transformedDataset", label: "Fitted values", from: { step: "comp", port: "out" }, optional: false }],
      ceiling,
    });
    note(
      "setup-fixtures-well-formed",
      ["py", "r", "jl", "child"].every((k) => saved[k].validation.state === "wellFormed" && !saved[k].validation.runnable),
      Object.fromEntries(Object.entries(saved).map(([k, s]) => [k, { state: s.validation.state, runnable: s.validation.runnable, codes: s.validation.diagnostics.map((d) => d.code) }])),
    );

    // ---- T. D1: an open workflow follows a folder trusted from the Lean panel ---------------
    await section("trust", async () => {
      await runCommand(page, "Show Lean Infoview", /Show Lean Infoview/);
      await openInEditor(page, "lean/Two.lean");
      const lp = page.getByTestId("lean-panel");
      await until("Lean panel untrusted", async () => (await lp.getAttribute("data-status")) === "untrusted", 30_000);
      await runCommand(page, "New Formal Audit Draft", /New Formal Audit Draft/);
      const editor = visible("workflow-editor");
      await editor.waitFor({ timeout: STEP_WAIT });
      const auditId = await editor.getAttribute("data-workflow");
      saved.audit = { id: auditId };
      const reason = async () => (await visible("workflow-run-reason").innerText().catch(() => "")).replace(/\s+/g, " ");
      const before = { canReview: await visible("workflow-status").getAttribute("data-can-review"), reason: await reason() };
      note("audit-blocked-untrusted", before.canReview === "false" && /not trusted/.test(before.reason), before);
      // Trust from the Lean panel of the Lean tab, then back to the still-open workflow tab.
      await page.locator('[data-editor-key="file:lean/Two.lean"]').first().click();
      await lp.getByRole("button", { name: "Trust workspace…" }).click();
      await alertDialog(page).getByRole("button", { name: "Trust workspace", exact: true }).click();
      await until("trusted", async () => (await invoke(page, "trust_status", { root: (await invoke(page, "workspace_current")).root })).trusted, 10_000);
      await showEditor(auditId);
      const ready = await until("audit reviewable after trust", async () => (await visible("workflow-status").getAttribute("data-can-review")) === "true");
      const stored = await invoke(page, "workflow_get", { id: auditId, revision: null });
      note("audit-reviewable-after-trust-without-save", ready && stored.revision === 1, { ready, revision: stored.revision, reason: await reason() });
      await shot("audit-after-trust");
    });

    // ---- A. XR-K-03: open editors follow the restricted images ------------------------------
    await section("images", async () => {
      const firstRead = ipcLog.length;
      await runCommand(page, "Show Workflows", /Show Workflows/);
      await page.getByTestId("workflows-view").waitFor({ timeout: 15_000 });
      for (const key of ["py", "r", "jl"]) {
        await tab("workflows:").click();
        await page.getByTestId(`workflow-open-${saved[key].id}`).click();
        await showEditor(saved[key].id);
        const before = await editorState();
        note(`${key}-blocked-without-image`, before.canReview === "false" && /unavailable: Restricted execution is unavailable: no .*image is configured/.test(before.step), before);
      }
      await showEditor(saved.py.id);
      await shot("editor-python-no-image");

      // Settings › Execution: the Python image; back to the still-open editors.
      await openSettingsExecution();
      await chooseImage("docker-image", IMAGES.python);
      await showEditor(saved.py.id);
      const pyReady = await until("Python editor reviewable", async () => (await editorState()).canReview === "true");
      const py = await editorState();
      note("py-reviewable-after-image-without-save", pyReady && py.revision === "1" && /— available\./.test(py.step) && !/no container image/.test(py.step), py);
      const pyStored = await invoke(page, "workflow_get", { id: saved.py.id, revision: null });
      note("py-revision-unchanged", pyStored.revision === 1, pyStored.revision);
      await shot("editor-python-image-configured");
      await showEditor(saved.r.id);
      note("r-still-blocked-after-python-image", (await editorState()).canReview === "false", await editorState());

      await openSettingsExecution();
      await chooseImage("docker-r-image", IMAGES.r);
      await chooseImage("docker-julia-image", IMAGES.julia);
      for (const key of ["r", "jl"]) {
        await showEditor(saved[key].id);
        const ready = await until(`${key} editor reviewable`, async () => (await editorState()).canReview === "true");
        const state = await editorState();
        note(`${key}-reviewable-after-language-image`, ready && state.revision === "1" && /— available\./.test(state.step), state);
      }

      // Research: the restricted row names and follows every language.
      await page.getByRole("toolbar", { name: "Pages" }).getByRole("button", { name: "Research", exact: true }).click();
      await page.locator('[data-testid="research-page"][data-state="enabled"]').waitFor({ timeout: 15_000 });
      const rowReady = await until("research row current", async () =>
        (await languageAvailable("python")) === "true" && (await languageAvailable("r")) === "true" && (await languageAvailable("julia")) === "true",
      );
      const rowText = (await restrictedRow().innerText()).replace(/\s+/g, " ");
      note("research-row-all-languages-available", rowReady && /Python, R, and Julia/.test(rowText), rowText);
      const mode = await page.getByTestId("research-execution-mode").first().getAttribute("aria-label");
      note("research-label-names-python-r-julia", /workflow batch steps in Python, R, and Julia/.test(mode) && /Trusted native/.test(mode), mode);
      await shot("research-restricted-row");
      // An out-of-band change (not through Settings): Refresh environments re-reads the row.
      await invoke(page, "execution_settings_update", { change: { kind: "languageImage", language: "julia", image: null } });
      await page.waitForTimeout(1000);
      const staleBefore = await languageAvailable("julia");
      await page.locator('button[aria-label="Refresh environments"]:visible').first().click();
      const refreshed = await until("julia unavailable after refresh", async () => (await languageAvailable("julia")) === "false");
      note("refresh-environments-rereads-restricted-row", refreshed, { before: staleBefore, after: await languageAvailable("julia"), text: (await restrictedRow().innerText()).replace(/\s+/g, " ") });
      await page.getByRole("toolbar", { name: "Pages" }).getByRole("button", { name: "Editor", exact: true }).click();

      // Clearing the Python image blocks the open editor again.
      await openSettingsExecution();
      await page.getByTestId("docker-image").getByRole("button", { name: "Clear" }).click();
      await page.getByTestId("docker-image-state").filter({ hasText: "No image configured" }).waitFor({ timeout: STEP_WAIT });
      await showEditor(saved.py.id);
      const blocked = await until("Python editor blocked again", async () => (await editorState()).canReview === "false");
      const cleared = await editorState();
      note("py-blocked-again-after-clear", blocked && /no container image is configured/.test(cleared.step), cleared);
      await openSettingsExecution();
      await chooseImage("docker-image", IMAGES.python);
      const saves = ipcLog.slice(firstRead).filter((l) => l.cmd === "workflow_save");
      note("images-section-saved-nothing", saves.length === 0, { saves: saves.length, reads: ipcLog.length - firstRead });
    });

    // ---- B. XR-E-03: the mounted list shows new and saved drafts ----------------------------
    await section("list", async () => {
      await tab("workflows:").click();
      await page.getByTestId("workflow-new").click();
      await page.getByTestId("workflow-new-numericalExperiment").click();
      const editor = page.locator('[data-testid="workflow-editor"]:visible').first();
      await until("new draft editor", async () => {
        const id = await editor.getAttribute("data-workflow").catch(() => null);
        return id !== null && !Object.values(saved).some((s) => s.id === id);
      });
      const newId = await editor.getAttribute("data-workflow");
      await tab("workflows:").click();
      const row = page.locator(`[data-testid="workflow-drafts"] [data-workflow="${newId}"]`);
      const listed = await until("new draft listed", async () => (await row.count()) > 0, 10_000);
      note("list-shows-own-new-draft", listed, { newId });
      await showEditor(newId);
      await visible("workflow-title").locator("input").fill("XR E09 parent");
      await visible("workflow-save").click();
      await until("revision 2", async () => (await visible("workflow-editor").getAttribute("data-revision")) === "2");
      await tab("workflows:").click();
      const renamed = await until("renamed draft listed", async () => /XR E09 parent/.test(await row.innerText().catch(() => "")) && /Revision 2/.test(await row.innerText().catch(() => "")), 10_000);
      note("list-shows-saved-title-and-revision", renamed, await row.innerText().catch(() => "missing"));
      await shot("workflows-list-after-save");
    });

    // ---- C. XR-E-02: the open parent discovers the child's new revision ---------------------
    await section("composite", async () => {
      await tab("workflows:").click();
      await page.getByTestId(`workflow-open-${saved.child.id}`).click();
      await showEditor(saved.child.id);
      await tab("workflows:").click();
      await page.getByTestId(`workflow-open-${saved.parent.id}`).click();
      await showEditor(saved.parent.id);
      await visible("composite-revision-comp").waitFor({ timeout: STEP_WAIT });
      note("parent-pins-child-1", (await visible("composite-revision-comp").innerText()) === "1" && (await page.locator('[data-testid="composite-use-latest-comp"]:visible').count()) === 0, "");

      await showEditor(saved.child.id);
      await visible("workflow-editor").getByLabel("Description", { exact: true }).fill("Child revision 2");
      await visible("workflow-save").click();
      await until("child revision 2", async () => (await visible("workflow-editor").getAttribute("data-revision")) === "2");

      await showEditor(saved.parent.id);
      const offered = await until("Use latest revision offered", async () => /Use latest revision \(2\)/.test(await visible("composite-use-latest-comp").innerText().catch(() => "")), 10_000);
      const pin = await visible("composite-revision-comp").innerText();
      const stored = await invoke(page, "workflow_get", { id: saved.parent.id, revision: null });
      note("parent-offers-latest-without-reopen", offered && pin === "1" && stored.revision === 1, { offered, pin, revision: stored.revision });
      await shot("composite-parent-offers-latest");
      await visible("composite-use-latest-comp").click();
      await until("pin 2", async () => (await visible("composite-revision-comp").innerText()) === "2");
      const unsaved = await invoke(page, "workflow_get", { id: saved.parent.id, revision: null });
      note("use-latest-is-an-unsaved-edit", unsaved.revision === 1 && unsaved.definition.steps[0].composite.revision === 1, unsaved.revision);
      await visible("workflow-save").click();
      await until("parent revision 2", async () => (await visible("workflow-editor").getAttribute("data-revision")) === "2");
      const after = await invoke(page, "workflow_get", { id: saved.parent.id, revision: null });
      note("save-pins-child-2", after.revision === 2 && after.definition.steps[0].composite.revision === 2, { revision: after.revision, pin: after.definition.steps[0].composite });
    });

    // ---- D. XR-K-02: Close Folder during a restricted run, then reopen ----------------------
    await section("close", async () => {
      // A freshly opened editor (independent of section A's refresh).
      await tab(`workflow:${saved.py.id}`).locator(".ui-editor-tab__close").click();
      await tab("workflows:").click();
      await page.getByTestId(`workflow-open-${saved.py.id}`).click();
      await showEditor(saved.py.id);
      await until("Python editor reviewable", async () => (await editorState()).canReview === "true");
      await visible("workflow-review-run").click();
      await page.getByTestId("workflow-plan-review").getByTestId("workflow-plan-details").waitFor({ timeout: 30_000 });
      await page.getByTestId("workflow-start-run").click();
      const monitor = visible("workflow-run");
      await monitor.waitFor({ timeout: 30_000 });
      runId = await monitor.getAttribute("data-run");
      const containerUp = await until("restricted container running", async () => managedContainers().length > 0, 60_000);
      note("run-started-in-container", containerUp && (await monitor.getAttribute("data-state")) === "running" && (await visible("workflow-run-cancel").count()) === 1, { runId, containers: managedContainers().length });
      await shot("run-running");

      await runCommand(page, "Close Folder", /File: Close Folder/);
      await until("no folder open", async () => /No folder open/.test(await page.getByTestId("pagebar-project").innerText()));
      await tab(`workflow-run:${runId}`).click().catch(() => {});
      const detached = await until("run tab unavailable", async () => (await page.locator('[data-testid="workflow-run-detached"]:visible').count()) === 1, 10_000);
      await page.waitForTimeout(2000);
      const cancelShown = await page.locator('[data-testid="workflow-run-cancel"]:visible').count();
      const runningShown = await page.locator('[data-testid="workflow-run"][data-state="running"]:visible').count();
      note("closed-project-run-tab-unavailable-no-cancel", detached && cancelShown === 0 && runningShown === 0, {
        detached,
        cancelShown,
        runningShown,
        text: (await page.locator('[data-testid="workflow-run-detached"]:visible').innerText().catch(() => "")).replace(/\s+/g, " "),
      });
      await shot("run-project-closed");
      const cleaned = await until("containers removed", async () => managedContainers().length === 0, 60_000);
      note("containers-removed-after-close", cleaned, managedContainers());

      // Reopen from Recent (Files view); the same tab shows the final state.
      await page.getByRole("button", { name: /^Open alpha/ }).click();
      await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
      await tab(`workflow-run:${runId}`).click();
      const canceled = await until("run tab shows canceled", async () => (await page.locator('[data-testid="workflow-run"][data-state="canceled"]:visible').count()) === 1);
      const record = await invoke(page, "workflow_run_get", { run: runId });
      note("reopened-run-tab-shows-canceled", canceled && (await page.locator('[data-testid="workflow-run-cancel"]:visible').count()) === 0, { canceled, state: record.state, code: record.code });
      note("run-record-canceled-by-project-close", record.state === "canceled" && record.code === "workflow.projectClosed", { state: record.state, code: record.code });
      await shot("run-reopened-canceled");
    });

    // ---- E. E16: a visible project switch with the Workflows list mounted -------------------
    await section("switch", async () => {
      await tab("workflows:").click();
      const runRow = page.locator(`[data-testid="workflow-run-item-${runId}"]:visible`);
      await until("alpha run listed", async () => (await runRow.count()) === 1);
      note("alpha-recent-runs-listed", (await runRow.count()) === 1, runId);

      await runCommand(page, "File: New Project", /File: ?New Project…$/);
      await page.getByTestId("new-project").waitFor({ timeout: 15_000 });
      await page.getByTestId("project-title").locator("input").fill("Beta");
      await page.getByTestId("project-location").locator("input").fill(beta);
      await page.waitForSelector('[data-testid="project-destination-status"][data-ok="true"]', { timeout: STEP_WAIT });
      await page.waitForSelector('[data-testid="project-preview"][data-valid="true"]', { timeout: STEP_WAIT });
      await page.getByTestId("project-create").click();
      await until("beta open", async () => /beta/i.test(await page.getByTestId("pagebar-project").innerText()), 30_000);

      await tab("workflows:").click();
      await page.getByTestId("workflows-view").waitFor({ timeout: STEP_WAIT });
      const switched = await until("beta list shown", async () =>
        (await page.locator(`[data-testid="workflow-drafts"] [data-workflow="${saved.py.id}"]`).count()) === 0 &&
        (await runRow.count()) === 0 &&
        (await page.getByText("No runs yet").count()) > 0,
      );
      note("switched-list-shows-only-new-project", switched, { alphaRunsShown: await runRow.count(), alphaDraftsShown: await page.locator(`[data-testid="workflow-drafts"] [data-workflow="${saved.py.id}"]`).count() });
      await shot("workflows-list-after-switch");
      await tab(`workflow-run:${runId}`).click();
      note("switched-run-tab-unavailable", await until("run tab unavailable", async () => (await page.locator('[data-testid="workflow-run-detached"]:visible').count()) === 1, 10_000), "");

      await runCommand(page, "Close Folder", /File: Close Folder/);
      await page.getByRole("button", { name: /^Open alpha/ }).click();
      await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
      await tab("workflows:").click();
      note("switch-back-lists-alpha-runs", await until("alpha run listed again", async () => (await runRow.count()) === 1), "");
    });

    note("diag-ipc-full-log", true, ipcLog);
    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    note("no-console-errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    if (process.env.EXPOSITION_DEV_HOME && shots.length) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "workflows-state");
      await mkdir(dir, { recursive: true });
      for (const shotPath of shots) if (existsSync(shotPath)) await cp(shotPath, join(dir, shotPath.split(/[\\/]/).pop()));
    }
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
