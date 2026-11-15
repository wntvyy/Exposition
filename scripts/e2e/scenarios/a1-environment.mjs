// A1 execution loop end to end in the real app: a trusted project with no
// usable environment → the setup notice for a host Python that lacks the
// kernel packages → Set up environment → the plan (folder, packages, PyPI,
// privileges, exact commands) → progress → ready → Run → output → edit the
// file → Rerun → Stop a long run → a failing package install leaves the
// environment ready → the environment choice persists across project reopen
// and an app restart → a workflow "Run again" with an unchanged plan starts
// without a second review, and a changed input opens the review.
//
// Needs a discovered Python outside the project and network access to
// pypi.org (otherwise it prints SKIPPED, unless EXPOSITION_REQUIRE_SETUP_TOOLS
// is set). Screenshots go to test-results/e2e/a1-environment and, when
// EXPOSITION_A1_SHOTS is set, are copied there too.
import { copyFile, cp, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { connect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { focusEditor, invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";

const SETUP_TIMEOUT = 15 * 60_000;
const KERNEL_TIMEOUT = 120_000;

function reachable(host, port, timeout = 5000) {
  return new Promise((resolve) => {
    const socket = connect({ host, port });
    const done = (ok) => {
      socket.destroy();
      resolve(ok);
    };
    socket.setTimeout(timeout, () => done(false));
    socket.once("connect", () => done(true));
    socket.once("error", () => done(false));
  });
}

function skip(reason) {
  const message = `SKIPPED: ${reason}`;
  if (process.env.EXPOSITION_REQUIRE_SETUP_TOOLS === "1") throw new Error(message);
  console.log(message);
}

const output = (page) => page.getByTestId("research-output").first();

async function newRunFinished(page, previous, status = "succeeded", timeout = KERNEL_TIMEOUT) {
  const runId = await waitFor(
    async () => {
      const id = (await output(page).count()) ? await output(page).getAttribute("data-run-id") : null;
      return id && id !== previous ? id : null;
    },
    { timeout: 60_000, message: "a new run" },
  );
  if (status) {
    await page.locator(`[data-testid="research-output"][data-run-id="${runId}"][data-status="${status}"]`).first().waitFor({ timeout });
  }
  return runId;
}

async function currentRun(page) {
  return (await output(page).count()) ? output(page).getAttribute("data-run-id") : null;
}

function pythonWorkflow(title) {
  return {
    schema: "exposition.workflow/1",
    title,
    description: "",
    preset: null,
    inputs: [{ id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }],
    steps: [
      {
        id: "run",
        label: "Summarize",
        operation: { id: "research.nativePython", version: 1 },
        parameters: { timeoutSeconds: { kind: "integer", value: 300 } },
        payload: {
          kind: "code",
          language: "python",
          source: 'import os\nrows = open(os.environ["EXPOSITION_INPUT_DATA"]).read().splitlines() if os.environ.get("EXPOSITION_INPUT_DATA") else []\nopen(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("rows\\n%d\\n" % len(rows))\nprint("rows", len(rows))\n',
        },
      },
    ],
    connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } }],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling: { execution: "trustedNative", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

export default async function a1Environment({ launchApp, artifactsDir, fixture, assert }) {
  if (!(await reachable("pypi.org", 443))) {
    skip("pypi.org is not reachable");
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-a1-env-"));
  const root = join(base, "project");
  const other = join(base, "other");
  const dataDir = join(base, "data");
  await cp(fixture("research-python"), root, { recursive: true });
  await mkdir(join(root, "data"), { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), "x\n1\n2\n3\n");
  await writeFile(join(root, "analysis", "loop.py"), 'print("version", 1)\n');
  await mkdir(other, { recursive: true });
  await writeFile(join(other, "notes.md"), "# Other project\n");
  await mkdir(dataDir, { recursive: true });

  const shotsDir = process.env.EXPOSITION_A1_SHOTS;
  if (shotsDir) await mkdir(shotsDir, { recursive: true });
  const shot = async (page, name) => {
    const dismiss = page.getByRole("button", { name: /^Dismiss notification/ });
    for (let i = 0; i < 10 && (await dismiss.count()) > 0; i++) await dismiss.first().click().catch(() => {});
    await page.waitForTimeout(250);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    if (shotsDir) await copyFile(path, join(shotsDir, `${name}.png`));
  };

  let app = await launchApp({ dataDir });
  try {
    let page = app.page;
    await page.setViewportSize({ width: 1500, height: 980 }).catch(() => {});
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    const workspace = await invoke(page, "workspace_current");
    await invoke(page, "trust_grant", { root: workspace.root });

    // Discovery: a host Python outside the project, preferring one without the kernel packages.
    const report = await invoke(page, "research_environments", { refresh: true });
    console.log(`environments: ${report.runtimes.map((r) => `${r.displayName}: ${r.availability.kind}`).join("; ")}`);
    const candidates = report.runtimes.filter((r) => r.language === "python" && r.executable && !r.projectFolder);
    const lacking = candidates.find((r) => r.availability.kind === "unavailable" && r.setup?.action === "createEnvironment");
    const basePython = lacking ?? candidates[0];
    if (!basePython) {
      skip("no Python interpreter was discovered");
      return;
    }
    console.log(`base: ${basePython.displayName} (${basePython.availability.kind})`);
    assert.ok(!report.runtimes.some((r) => r.projectFolder), "the project has no environment yet");

    await runCommand(page, "Show Research Panel", /Show Research Panel/);
    const panel = page.getByTestId("research-panel");
    await panel.waitFor({ timeout: 15_000 });
    await page.getByRole("separator", { name: "Resize panel" }).focus();
    for (let i = 0; i < 7; i++) await page.keyboard.press("ArrowUp");

    const envSelect = panel.getByRole("button", { name: /Research environment/ }).first();
    const choose = async (pattern) => {
      for (let attempt = 1; ; attempt++) {
        await envSelect.click();
        try {
          await page.getByRole("option", { name: pattern }).first().click({ timeout: 5_000 });
          return;
        } catch (error) {
          if (attempt >= 4) throw error;
          await page.keyboard.press("Escape").catch(() => {});
        }
      }
    };
    const escape = (s) => s.replace(/[()+.\\[\]?*^$|{}]/g, "\\$&");

    // ---- 1. the setup notice -------------------------------------------------
    await choose(new RegExp(`^${escape(basePython.displayName)}`));
    if (lacking) {
      const notice = panel.getByTestId("research-unavailable");
      await notice.waitFor({ timeout: 15_000 });
      assert.equal(await notice.getAttribute("data-setup"), "createEnvironment", "the notice offers Set up environment");
      assert.ok((await notice.innerText()).includes(lacking.availability.guidance.slice(0, 40)), "the notice shows the exact guidance");
      await shot(page, "a1-setup-notice");
      await panel.getByTestId("setup-open").click();
    } else {
      // Every host Python is ready: set up from the session bar action instead.
      await panel.getByTestId("setup-packages-open").click();
    }

    // ---- 2. the plan ---------------------------------------------------------
    const plan = page.getByTestId("setup-plan");
    await plan.waitFor({ timeout: 15_000 });
    await page.getByTestId("setup-summary").waitFor({ timeout: 30_000 });
    assert.match(await page.getByTestId("setup-folder").innerText(), /\.venv.*new, inside the project/);
    assert.equal((await page.getByTestId("setup-packages").innerText()).trim(), "ipykernel jupyter_client");
    const dialogText = await page.getByRole("dialog").innerText();
    assert.match(dialogText, /pypi\.org\/simple/);
    assert.match(dialogText, /account.s privileges/);
    await page.getByRole("button", { name: /^Commands/ }).click();
    const commandsText = await page.getByTestId("setup-commands").innerText();
    assert.match(commandsText, /-I -m venv/);
    assert.match(commandsText, /--require-virtualenv --only-binary=:all: --index-url https:\/\/pypi\.org\/simple ipykernel jupyter_client/);
    await shot(page, "a1-setup-plan");
    assert.ok(!existsSync(join(root, ".venv")), "nothing ran before Set up");
    await page.getByTestId("setup-start").click();

    // ---- 3. progress ---------------------------------------------------------
    const progress = panel.getByTestId("setup-progress");
    await progress.waitFor({ timeout: 30_000 });
    await waitFor(async () => (await progress.getAttribute("data-step")) === "1" && (await panel.getByTestId("setup-log").count()) > 0, {
      timeout: 120_000,
      interval: 250,
      message: "the install step with output",
    }).catch(() => {});
    if (await progress.count()) await shot(page, "a1-setup-progress");
    await progress.waitFor({ state: "detached", timeout: SETUP_TIMEOUT });
    const status = await invoke(page, "research_setup_status");
    console.log(`setup: ${status.state} ${status.message ?? ""} (${status.jobs.length} jobs)`);
    assert.equal(status.state, "succeeded", status.message ?? "");
    assert.ok(status.ready && status.environmentId, "the new environment is ready");
    const after = await invoke(page, "research_environments", { refresh: false });
    const venv = after.runtimes.find((r) => r.id === status.environmentId);
    assert.equal(venv?.availability.kind, "available");
    assert.equal(venv?.projectFolder, ".venv");
    await waitFor(async () => (await envSelect.innerText()).includes("project .venv"), { message: "the new environment is selected" });
    assert.equal(await invoke(page, "research_selection_get"), venv.id, "the choice is remembered for the project");

    // ---- 4. Run → output → edit → Rerun → Stop ------------------------------------
    await panel.getByTestId("research-code").fill("import sys\nprint(sys.prefix)");
    await panel.getByTestId("research-run").click();
    const first = await newRunFinished(page, null);
    const prefix = await output(page).getByTestId("research-stream").first().innerText();
    console.log(`sys.prefix: ${prefix.trim()}`);
    assert.match(prefix.toLowerCase(), /\.venv/, "the code ran in the project environment");

    await openInEditor(page, "analysis/loop.py");
    await runCommand(page, "Run File in Research", /Run File in Research Session/);
    const fileRun = await newRunFinished(page, first);
    assert.match(await output(page).getByTestId("research-stream").first().innerText(), /version 1/);
    await focusEditor(page, "analysis/loop.py", "end");
    await page.keyboard.type('print("version", 2)\n');
    await page.waitForTimeout(400);
    const rerun = panel.getByTestId("research-rerun");
    assert.equal(await rerun.getAttribute("data-origin"), "file");
    assert.equal(await rerun.getAttribute("aria-label"), "Rerun analysis/loop.py");
    await rerun.click();
    const rerunId = await newRunFinished(page, fileRun);
    const rerunText = (await output(page).getByTestId("research-stream").allInnerTexts()).join("\n");
    assert.match(rerunText, /version 2/, "Rerun used the edited (unsaved) source");
    const rerunManifest = await invoke(page, "research_run", { runId: rerunId });
    assert.equal(rerunManifest.code.origin.kind, "file");
    assert.ok(rerunManifest.unsavedInputs.includes("analysis/loop.py"), "the edited buffer was what ran");
    // Save, so switching projects later does not ask about unsaved changes.
    await focusEditor(page, "analysis/loop.py", "end");
    await page.keyboard.press("Control+s");
    await waitFor(async () => (await readFile(join(root, "analysis", "loop.py"), "utf8")).includes("version\", 2"), { message: "loop.py saved" });

    await panel.getByTestId("research-code").fill("import time\nfor i in range(600):\n    time.sleep(0.5)");
    await panel.getByTestId("research-run").click();
    const longRun = await newRunFinished(page, rerunId, "running");
    const stop = panel.getByTestId("research-stop");
    await waitFor(() => stop.isEnabled(), { message: "Stop enabled while code runs" });
    await shot(page, "a1-run-bar");
    await stop.click();
    await waitFor(async () => ["interrupted", "canceled"].includes(await output(page).getAttribute("data-status")), {
      timeout: 30_000,
      message: "the long run stops",
    });
    const stopped = await invoke(page, "research_run", { runId: longRun });
    console.log(`stopped run: ${stopped.record.status.kind}`);
    assert.ok(["interrupted", "canceled"].includes(stopped.record.status.kind));
    await waitFor(async () => !(await stop.isEnabled()), { message: "Stop is disabled once nothing runs" });

    // ---- 5. a failing install changes nothing ------------------------------------------
    await panel.getByTestId("setup-packages-open").click();
    await page.getByTestId("setup-summary").waitFor({ timeout: 30_000 });
    assert.match(await page.getByTestId("setup-folder").innerText(), /existing environment/);
    await page.getByLabel("Additional packages").fill("exposition-no-such-package-a1z");
    await waitFor(async () => (await page.getByTestId("setup-packages").innerText()).includes("exposition-no-such-package-a1z"), {
      message: "the plan lists the extra package",
    });
    await page.getByTestId("setup-start").click();
    const outcome = panel.getByTestId("setup-outcome");
    await outcome.waitFor({ timeout: SETUP_TIMEOUT });
    assert.equal(await outcome.getAttribute("data-state"), "failed");
    const outcomeText = await panel.getByTestId("setup-outcome-message").innerText();
    console.log(`failing install: ${outcomeText}`);
    assert.match(outcomeText, /Nothing was removed from \.venv/);
    await shot(page, "a1-setup-failed");
    const stillReady = await invoke(page, "research_environments", { refresh: true });
    assert.equal(stillReady.runtimes.find((r) => r.id === venv.id)?.availability.kind, "available", "still ready after the failed install");

    // ---- 6. the choice persists across project reopen and an app restart ---------------
    await invoke(page, "workspace_open", { path: other });
    await waitFor(async () => (await invoke(page, "research_workspace"))?.root?.toLowerCase().endsWith("other"), { message: "other project open" });
    await waitFor(async () => !(await envSelect.innerText()).includes("project .venv"), { timeout: 30_000, message: "the other project has its own choice" });
    await invoke(page, "workspace_open", { path: root });
    await waitFor(async () => (await envSelect.innerText()).includes("project .venv"), { timeout: 60_000, message: "the choice is back after reopening" });
    await app.close();
    app = await launchApp({ dataDir });
    page = app.page;
    await page.setViewportSize({ width: 1500, height: 980 }).catch(() => {});
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    await runCommand(page, "Show Research Panel", /Show Research Panel/);
    const panel2 = page.getByTestId("research-panel");
    await panel2.waitFor({ timeout: 15_000 });
    const envSelect2 = panel2.getByRole("button", { name: /Research environment/ }).first();
    await waitFor(async () => (await envSelect2.innerText()).includes("project .venv"), { timeout: 90_000, message: "the choice survives a restart" });
    console.log(`after restart: ${(await envSelect2.innerText()).trim()}`);

    // ---- 7. workflow Run again ----------------------------------------------------------
    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: pythonWorkflow("Count rows") } });
    const firstPlan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    assert.deepEqual(firstPlan.blockers, [], "the native plan can start in the trusted folder");
    const firstRun = await invoke(page, "workflow_run_start", { request: { plan: firstPlan.plan, approvalHash: firstPlan.approvalHash } });
    await waitFor(async () => (await invoke(page, "workflow_run_get", { run: firstRun.id })).state !== "running", { timeout: 180_000, interval: 500, message: "the first workflow run ends" });
    const firstDone = await invoke(page, "workflow_run_get", { run: firstRun.id });
    console.log(`workflow run 1: ${firstDone.state} ${firstDone.message ?? ""}`);
    assert.equal(firstDone.state, "succeeded");
    const runsOf = () => invoke(page, "workflow_runs", { workflow: saved.id });
    await runCommand(page, "Show Workflows", /Show Workflows/);
    await page.getByTestId("workflows-view").waitFor({ timeout: 15_000 });
    const item = page.getByTestId(`workflow-run-item-${firstRun.id}`).first();
    await item.waitFor({ timeout: 30_000 });
    const openFirst = async () => {
      await item.getByRole("button", { name: /^Open run/ }).click();
      const monitor = page.locator(`[data-testid="workflow-run"][data-run="${firstRun.id}"]`);
      await monitor.waitFor({ timeout: 15_000 });
      return monitor;
    };
    let monitor = await openFirst();
    const runsBefore = (await runsOf()).length;
    await monitor.getByTestId("workflow-run-again").click();
    await waitFor(async () => (await runsOf()).length > runsBefore, { timeout: 30_000, message: "Run again started a new run" });
    assert.equal(await page.getByTestId("workflow-plan-review").count(), 0, "no second review for an unchanged plan");
    const second = (await runsOf()).find((r) => r.id !== firstRun.id);
    await page.locator(`[data-testid="workflow-run"][data-run="${second.id}"]`).waitFor({ timeout: 15_000 });
    console.log(`Run again started ${second.id} directly (cause ${second.cause?.kind ?? "manual"})`);
    await waitFor(async () => (await runsOf()).every((r) => r.state !== "running"), { timeout: 180_000, interval: 500, message: "runs settle" });
    await shot(page, "a1-workflow-run-again");

    // A changed input makes the plan differ: Run again opens the review instead.
    await writeFile(join(root, "data", "obs.csv"), "x\n1\n2\n3\n4\n");
    await page.waitForTimeout(500);
    const runsNow = (await runsOf()).length;
    monitor = await openFirst();
    await monitor.getByTestId("workflow-run-again").click();
    await page.getByTestId("plan-review-notice").waitFor({ timeout: 30_000 });
    await page.getByTestId("workflow-plan-details").waitFor({ timeout: 30_000 });
    assert.match(await page.getByTestId("plan-review-notice").innerText(), /differs from the one this run was approved with/);
    await shot(page, "a1-workflow-run-again-review");
    assert.equal((await runsOf()).length, runsNow, "nothing started while the review is open");
    await page.getByRole("dialog").getByRole("button", { name: "Cancel" }).click();
    console.log("a1-environment: all steps passed");
  } finally {
    await app.close();
  }
}
