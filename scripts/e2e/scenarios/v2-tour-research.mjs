// UI standard V2 tour of the research area: the main surfaces populated, in
// all four themes (Light, Dark, Ocean, Sage), for visual review.
//
//  1. Research page: an explorer full of long file names with the supplied
//     file icons (sources, data, notes), a file selected (inspector + source).
//  2. A research run with output and an error (Python; skipped when no usable
//     Python environment is discovered).
//  3. Jobs panel: a canceled probe job with its stdout/stderr log next to a
//     job refused for an untrusted folder (debug build: dev probe commands).
//  4. Workflows: the plan review of a numerical-experiment workflow, and its
//     run with a pending manuscript change under review (Docker with the
//     local image python:3.13-slim; skipped without it, like workflow-run).
//  5. Lean: a checked file with evidence recorded (Lean 4.35 through elan;
//     skipped without it, like review-m4-lean-ui). Editing the file makes the
//     evidence stale, which the graph shows.
//  6. Graph: every node category (structure, document, data, math, formal,
//     reference, workflow) with a selection, keyboard focus on another node,
//     search-dimmed nodes, the Mathematics dependencies (stale evidence badge)
//     and verification table.
//  7. Narrow window: the graph inspector as a drawer.
//  8. The Equation panel with a draft (Typst, Split view).
//
// Shots: test-results/e2e/v2-tour-research/<name>-<theme>.png and, with
// EXPOSITION_DEV_HOME set, $EXPOSITION_DEV_HOME/ui-reference/v2/research/.
//
// Requires a debug build: it calls the debug-only probe command
// graph_reconcile.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { copyFile, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { equationSession, makeEquationProject, openDocumentIn } from "../equation-helpers.mjs";
import { consoleErrors } from "../p1-helpers.mjs";
import { runCommand } from "../palette.mjs";
import { applyTheme, shootThemes, THEMES } from "../theme-helpers.mjs";
import { invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";

const AREA = "research";
const WIDE = { width: 1440, height: 900 };
const NARROW = { width: 900, height: 640 };
const KERNEL_TIMEOUT = 180_000;
const RUN_TIMEOUT = 240_000;
const IMAGE = "python:3.13-slim";
const LEAN_TOOLCHAIN = "leanprover--lean4---v4.35.0";

const OBS = "x,y\n0,1\n1,3\n2,5\n3,7\n";
const PAPER = "= Arrhenius kinetics\n\nThe corrected rate constants follow the Arrhenius law.\n";
const NOTES = "# Notes\n\nOpen questions about the catalyst batches.\n";
const LEAN = "lean/OddSums.lean";
const DECLARATION = "oddSum_eq_sq";
const LEAN_TEXT = `def oddSum : Nat → Nat
  | 0 => 0
  | n + 1 => oddSum n + (2 * n + 1)

theorem oddSum_eq_sq (n : Nat) : oddSum n = n * n := by
  induction n with
  | zero => rfl
  | succ k ih => simp [oddSum, ih, Nat.succ_mul, Nat.mul_succ]; omega
`;
// The workflow's declared script (restricted container): table, figure, dataset.
const EXPERIMENT = `import csv, os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
rows = list(csv.DictReader(open("data/obs.csv")))
xs = [float(r["x"]) for r in rows]; ys = [float(r["y"]) for r in rows]
slope = (ys[-1] - ys[0]) / (xs[-1] - xs[0])
print("slope", slope)
with open(os.path.join(out, "table.csv"), "w", newline="") as f:
    w = csv.writer(f); w.writerow(["x", "y", "fit"])
    for x, y in zip(xs, ys): w.writerow([x, y, ys[0] + slope * (x - xs[0])])
pts = " ".join(f"{20 + 40 * x},{180 - 20 * y}" for x, y in zip(xs, ys))
open(os.path.join(out, "figure.svg"), "w").write(
    f'<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200"><polyline points="{pts}" fill="none" stroke="black"/></svg>')
with open(os.path.join(out, "dataset.csv"), "w", newline="") as f:
    w = csv.writer(f); w.writerow(["quantity", "value"]); w.writerow(["slope", slope])
`;
// A run with output and then an error.
const ERROR_RUN = `values = [1.5, 2.25, 4.0]
print("mean of", len(values), "values:", sum(values) / len(values))
print("normalising by the batch with the longest calibration record")
import math
math.log(-1)
`;

const PNG_1x1 = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", "base64");
const NOTEBOOK = JSON.stringify({ cells: [{ cell_type: "code", metadata: {}, source: ["print('rates')"], outputs: [], execution_count: null }], metadata: {}, nbformat: 4, nbformat_minor: 5 });

/** Research files with long names (group, name, content). */
const RESEARCH_FILES = [
  ["sources", "bootstrap_confidence_intervals_for_arrhenius_rate_constants_v2.py", "import numpy as np\n\n# Bootstrap the fitted activation energy.\nrng = np.random.default_rng(7)\n"],
  ["sources", "kinetics_model_with_temperature_dependent_prefactor.jl", "k(T; A=1.0, Ea=53.4) = A * exp(-Ea / (8.314e-3 * T))\n"],
  ["sources", "mixed_effects_regression_of_activation_energies_by_batch.R", "fit <- lm(log(k) ~ I(1 / T), data = rates)\n"],
  ["sources", "exploratory_rate_constant_analysis_notebook_2026-11.ipynb", NOTEBOOK],
  ["data", "calibrated_rate_constants_by_temperature_and_catalyst_batch.csv", "T,k,batch\n300,0.012,A\n310,0.021,A\n320,0.035,B\n"],
  ["data", "instrument_metadata_and_calibration_coefficients.json", '{"instrument": "UV-2600", "slope": 0.998, "intercept": 0.002}\n'],
  ["data", "arrhenius_plot_of_corrected_rate_constants_all_batches.png", PNG_1x1],
  ["data", "supplementary_measurement_protocol_revision_3.pdf", "%PDF-1.4\n%%EOF\n"],
];
const SELECTED_SOURCE = RESEARCH_FILES[0][1];

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 30_000 });
    return true;
  } catch {
    return false;
  }
}

function leanAvailable() {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  return existsSync(join(elanHome, "toolchains", LEAN_TOOLCHAIN, "bin"));
}

async function setViewport(page, size) {
  await page.setViewportSize(size);
  await page.waitForFunction((w) => window.innerWidth === w, size.width);
  await page.waitForTimeout(150);
}

/** Wait until overlays and animations settle (reference screenshots). */
async function settle(page) {
  await page
    .waitForFunction(
      () =>
        !document.querySelector("[data-entering], [data-exiting]") &&
        document.getAnimations().every((a) => a.playState !== "running" || a.effect?.getTiming().iterations === Infinity),
      null,
      { timeout: 5000 },
    )
    .catch(() => {});
  await page.waitForTimeout(200);
}

/** Dismiss visible notifications (they cover the bottom-right corner of every page). */
async function dismissNotifications(page) {
  for (const button of await page.getByRole("button", { name: /^Dismiss notification/ }).all()) await button.click().catch(() => {});
}

/** Propose and apply typed graph operations (as the user). */
async function change(page, operations, summary = "v2 tour setup") {
  const status = await invoke(page, "graph_status");
  const proposal = await invoke(page, "graph_propose_change", { request: { baseRevision: status.revision, operations, summary } });
  if (!proposal.valid) throw new Error(`proposal invalid: ${JSON.stringify(proposal.diagnostics)}`);
  const outcome = await invoke(page, "graph_apply_change", { request: { proposal: proposal.id, expectedRevision: proposal.baseRevision } });
  if (outcome.kind !== "applied") throw new Error(`apply ${outcome.kind}: ${JSON.stringify(outcome.diagnostics)}`);
  return outcome;
}

const create = (tempId, kind, label) => ({ op: "createEntity", tempId, kind, roles: [], label, key: null, attributes: {}, binding: null });
const rel = (kind, from, to) => ({
  op: "addRelation",
  kind,
  from: { kind: "temp", tempId: from },
  to: { kind: "temp", tempId: to },
  evidenceKind: null,
  evidence: [],
  note: null,
});

async function snapshotOf(page, kinds) {
  return invoke(page, "graph_snapshot", { query: { kinds, text: null, around: null, includeInactive: false, limit: 500 } });
}

const showPage = async (page, id) => {
  await page.locator(`[data-page-button="${id}"]`).click();
  await page.locator(`[data-page="${id}"]:not([hidden])`).first().waitFor({ timeout: 15_000 });
};

export default async function v2TourResearch({ launchApp, artifactsDir, fixture, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-v2-tour-research-"));
  const root = join(base, "project");
  const outside = join(base, "outside");
  const untrusted = join(base, "untrusted-project");
  const dataDir = join(base, "data");
  for (const dir of [join(root, "data"), join(root, "lean"), outside, untrusted, dataDir]) await mkdir(dir, { recursive: true });
  await writeFile(join(root, "paper.typ"), PAPER);
  await writeFile(join(root, "notes.md"), NOTES);
  await writeFile(join(root, "experiment.py"), EXPERIMENT);
  await writeFile(join(root, "data", "obs.csv"), OBS);
  await writeFile(join(root, ...LEAN.split("/")), LEAN_TEXT);
  for (const [, name, content] of RESEARCH_FILES) await writeFile(join(outside, name), content);

  const skipped = [];
  const skip = (what, why) => {
    skipped.push(`${what}: ${why}`);
    console.log(`SKIPPED ${what}: ${why}`);
  };
  const shots = [];
  const published = process.env.EXPOSITION_DEV_HOME ? join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "v2", AREA) : null;

  const app = await launchApp({ dataDir });
  const { page } = app;
  /** Four-theme shot; `prepare` re-creates transient state after the theme switch (it opens the palette). */
  const shoot = async (name, prepare) => {
    const paths = await shootThemes(page, {
      outDir: artifactsDir,
      area: AREA,
      name,
      prepare: async (p, theme) => {
        await dismissNotifications(p);
        if (prepare) await prepare(p, theme);
        await settle(p);
      },
    });
    shots.push(...paths);
  };
  /**
   * Four-theme shot of a modal dialog: the palette cannot open over a modal,
   * so the dialog is closed before each theme switch and opened again.
   */
  const shootDialog = async (name, open, close) => {
    await mkdir(artifactsDir, { recursive: true });
    if (published) await mkdir(published, { recursive: true });
    for (const [theme] of THEMES) {
      await applyTheme(page, theme);
      await dismissNotifications(page);
      await open();
      await settle(page);
      const path = join(artifactsDir, `${name}-${theme}.png`);
      await page.screenshot({ path });
      if (published) await copyFile(path, join(published, `${name}-${theme}.png`));
      shots.push(path);
      await close();
    }
  };

  try {
    // Room for the bottom panel (Jobs), auxiliary panel hidden (persisted workbench layout).
    await page.evaluate(() =>
      localStorage.setItem(
        "exposition.workbench.layout",
        JSON.stringify({
          version: 1,
          sidebar: { visible: true, size: 260, activeView: null },
          panel: { visible: true, size: 380, activeView: "jobs.panel" },
          auxiliary: { visible: false, size: 340, activeView: null },
          editorSplit: 50,
        }),
      ),
    );
    await page.reload();
    await page.waitForFunction(() => (document.getElementById("root")?.childElementCount ?? 0) > 0);
    await page.getByTestId("workbench").waitFor({ timeout: 15_000 });
    await setViewport(page, WIDE);
    await applyTheme(page, "light");

    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: info.root });
    await waitFor(async () => (await invoke(page, "graph_status")).state.kind === "attached", { timeout: 15_000, message: "graph attached" });
    const debugBuild = (await invoke(page, "app_info")).debugBuild;

    // Graph items for every category (math, data, reference, structure); files add document/formal.
    const seeded = await change(page, [
      create("thm", "theorem", "Arrhenius bound on the corrected rate constants for every catalyst batch"),
      create("lem", "lemma", "Monotonicity of the rate in temperature"),
      create("def", "definition", "Activation energy"),
      create("hyp", "assumption", "Constant pre-exponential factor"),
      create("prf", "proof", "Proof of the Arrhenius bound"),
      create("col", "collection", "Kinetics study reading list"),
      create("exp", "experiment", "Temperature sweep of the catalyst batches"),
      create("fnd", "finding", "Activation energy is 53.4 kJ/mol"),
      create("ref", "reference", "Arrhenius 1889, Über die Reaktionsgeschwindigkeit"),
      rel("dependsOn", "thm", "lem"),
      rel("assumes", "thm", "hyp"),
      rel("dependsOn", "lem", "def"),
      rel("proofOf", "prf", "thm"),
      rel("memberOf", "lem", "col"),
      rel("references", "fnd", "thm"),
      rel("references", "thm", "ref"),
      rel("references", "exp", "thm"),
    ]);
    const ids = Object.fromEntries(seeded.created.map(({ tempId, id }) => [tempId, id]));

    // ---- 1. Research page: explorer with long names, a file selected -----------------
    const rp = page.locator('[data-page="research"]');
    await showPage(page, "research");
    await rp.locator('[data-testid="research-page"][data-state="disabled"]').waitFor({ timeout: 15_000 });
    await rp.getByRole("button", { name: "Enable Research" }).click();
    await rp.locator('[data-testid="research-page"][data-state="enabled"]').waitFor({ timeout: 15_000 });
    const paths = {};
    for (const [group, name] of RESEARCH_FILES) {
      const imported = await invoke(page, "research_space_import", { request: { group, source: join(outside, name), mode: "copy" } });
      paths[name] = imported.file.path;
    }
    const note = await invoke(page, "research_space_create_file", { request: { group: "notes", name: "methods_and_open_questions_for_the_kinetics_study.md" } });
    for (const path of [...Object.values(paths), note.path]) {
      await rp.locator(`[data-testid="research-tree"] [data-path="${path}"]`).first().waitFor({ timeout: 15_000 });
    }
    const sourcePath = paths[SELECTED_SOURCE];
    await rp.locator(`[data-testid="research-tree"] [data-path="${sourcePath}"]`).first().click();
    await rp.locator(`[data-testid="research-inspector"][data-kind="file"][data-path="${sourcePath}"]`).waitFor({ timeout: 15_000 });
    await rp.locator(`[data-testid="source-editor"][data-path="${sourcePath}"] .monaco-editor`).waitFor({ timeout: 15_000 });
    await shoot("research-explorer");

    // ---- 2. A run with output and an error -------------------------------------------
    const report = await invoke(page, "research_environments", { refresh: false });
    const python = report.runtimes.find((r) => r.language === "python" && r.availability.kind === "available");
    if (!python) {
      skip("research run", `no usable Python environment (${report.runtimes.map((r) => `${r.displayName}: ${r.availability.kind}`).join("; ")})`);
    } else {
      await rp.getByRole("button", { name: "Start session" }).click();
      await rp.locator('[data-testid="research-session-status"][data-state="idle"]').waitFor({ timeout: KERNEL_TIMEOUT });
      const code = rp.getByTestId("research-code");
      const runCode = async (text, status) => {
        const output = rp.getByTestId("research-output");
        const previous = (await output.count()) ? await output.getAttribute("data-run-id") : null;
        await code.fill(text);
        await code.focus();
        await page.keyboard.press("Control+Enter");
        const runId = await waitFor(
          async () => {
            const id = (await output.count()) ? await output.getAttribute("data-run-id") : null;
            return id && id !== previous ? id : null;
          },
          { timeout: 30_000, message: "a new research run" },
        );
        await rp.locator(`[data-testid="research-output"][data-run-id="${runId}"][data-status="${status}"]`).waitFor({ timeout: KERNEL_TIMEOUT });
        return runId;
      };
      await runCode('print("rates loaded")\nsum(range(10))', "succeeded");
      await runCode(ERROR_RUN, "failed");
      await rp.getByTestId("research-output").getByTestId("research-stream").first().waitFor();
      await rp.getByTestId("research-output").getByTestId("research-error-output").waitFor();
      assert.match(await rp.getByTestId("research-output").getByTestId("research-error-output").innerText(), /ValueError|math domain error/);
      await shoot("research-run-error");
    }

    // ---- 3. Jobs panel: a canceled job with its log, and a refused one ---------------
    if (!debugBuild) {
      skip("jobs", "the dev probe job needs a debug build");
    } else {
      await showPage(page, "editor");
      await runCommand(page, "Show Jobs", /Show Jobs/);
      const panel = page.getByTestId("jobs-panel");
      await panel.waitFor({ timeout: 15_000 });
      const refusedId = await invoke(page, "dev_start_workspace_job", { root: untrusted });
      await panel.locator(`li.exp-jobs__row[data-job-id="${refusedId}"] [data-testid="job-status"][data-job-status="failedToStart"]`).waitFor({ timeout: 15_000 });
      const jobId = await invoke(page, "dev_start_probe_job");
      const row = panel.locator(`li.exp-jobs__row[data-job-id="${jobId}"]`);
      await row.locator('[data-testid="job-status"][data-job-status="running"]').waitFor({ timeout: 30_000 });
      await row.locator(".exp-jobs__select").click();
      const output = page.getByTestId("job-output");
      await waitFor(async () => /tick \d+ parent=/.test(await output.innerText()) && (await output.locator('[data-stream="stderr"]').count()) > 0, {
        timeout: 30_000,
        message: "stdout and stderr in the job log",
      });
      const job = (await invoke(page, "job_list")).find((j) => j.info.id === jobId);
      await row.getByRole("button", { name: `Cancel ${job.info.label}` }).click();
      await row.locator('[data-testid="job-status"][data-job-status="canceled"]').waitFor({ timeout: 15_000 });
      await shoot("jobs");
    }

    await showPage(page, "editor");
    if (await page.getByTestId("jobs-panel").isVisible()) await runCommand(page, "Toggle Panel", /Toggle Panel/);

    // ---- 4. Workflows: plan review and a pending manuscript change -------------------
    // Saved in every case (a workflow item for the graph); reviewed and run with Docker.
    const preset = await invoke(page, "workflow_from_preset", {
      request: { preset: "numericalExperiment", script: "experiment.py", data: "data/obs.csv", manuscript: "paper.typ", leanFile: null },
    });
    if (!dockerAvailable()) {
      await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: preset } });
      skip("workflow plan review and pending approval", `Docker with the local image ${IMAGE} is not available`);
    } else {
      await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
      const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: preset } });
      await showPage(page, "editor");
      await runCommand(page, "Show Workflows", /Show Workflows/);
      await page.getByTestId("workflows-view").waitFor({ timeout: 15_000 });
      await page.getByTestId(`workflow-open-${saved.id}`).click();
      await page.locator('[data-testid="workflow-editor"]:visible').first().waitFor({ timeout: 15_000 });
      const status = page.getByTestId("workflow-status").first();
      await waitFor(async () => (await status.getAttribute("data-can-review")) === "true", { timeout: 30_000, message: "workflow reviewable" });
      const review = page.getByTestId("workflow-plan-review");
      await shootDialog(
        "workflow-plan-review",
        async () => {
          await page.getByTestId("workflow-review-run").first().click();
          await review.getByTestId("workflow-plan-details").waitFor({ timeout: 30_000 });
        },
        async () => {
          await page.keyboard.press("Escape");
          await review.waitFor({ state: "detached", timeout: 10_000 });
        },
      );

      // A run whose promotion step leaves a pending, reviewable manuscript change.
      const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
      assert.deepEqual(plan.blockers, [], "the numerical experiment can start");
      const started = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
      let run;
      await waitFor(
        async () => {
          run = await invoke(page, "workflow_run_get", { run: started.id });
          return run.state !== "running";
        },
        { timeout: RUN_TIMEOUT, interval: 500, message: "the workflow run ends" },
      );
      assert.equal(run.state, "succeeded", `workflow run: ${run.state} ${run.message ?? ""}`);
      await runCommand(page, "Show Workflows", /Show Workflows/);
      await page.getByTestId("workflows-view").waitFor({ timeout: 15_000 });
      const item = page.getByTestId(`workflow-run-item-${started.id}`).first();
      await item.waitFor({ timeout: 30_000 });
      await item.getByRole("button", { name: /^Open run/ }).click();
      const monitor = page.locator(`[data-testid="workflow-run"][data-run="${started.id}"]`);
      await monitor.waitFor({ timeout: 15_000 });
      const proposal = monitor.locator('[data-testid="workflow-promotion-proposal"][data-status="pending"]');
      await proposal.waitFor({ timeout: 15_000 });
      await proposal.getByTestId("workflow-promotion-review").click();
      await proposal.getByTestId("workflow-promotion-approve").waitFor({ timeout: 15_000 });
      await shoot("workflow-run-pending-approval", async () => {
        await proposal.evaluate((el) => el.scrollIntoView({ block: "start" }));
      });
    }
    if (!(await snapshotOf(page, ["workflow"])).entities.length) await change(page, [create("wf", "workflow", "Numerical experiment")]);
    // The experiment derives from the saved workflow (places the workflow beside the cluster).
    const workflowEntity = (await snapshotOf(page, ["workflow"])).entities[0].id;
    {
      const status = await invoke(page, "graph_status");
      const proposal = await invoke(page, "graph_propose_change", {
        request: {
          baseRevision: status.revision,
          operations: [
            { op: "addRelation", kind: "derivedFrom", from: { kind: "id", id: ids.exp }, to: { kind: "id", id: workflowEntity }, evidenceKind: null, evidence: [], note: null },
          ],
          summary: "v2 tour: experiment from workflow",
        },
      });
      if (proposal.valid) {
        await invoke(page, "graph_apply_change", { request: { proposal: proposal.id, expectedRevision: proposal.baseRevision } });
        ids.wf = workflowEntity;
      } else console.log(`experiment derivedFrom workflow not proposed: ${JSON.stringify(proposal.diagnostics)}`);
    }

    // ---- 5. Lean: a checked file, evidence recorded, then made stale -----------------
    if (!leanAvailable()) {
      skip("lean", `Lean toolchain ${LEAN_TOOLCHAIN} is not installed through elan`);
    } else {
      await showPage(page, "editor");
      await runCommand(page, "show lean infoview", /Show Lean Infoview/);
      const leanDoc = await openInEditor(page, LEAN);
      const panel = page.getByTestId("lean-panel");
      await waitFor(async () => (await panel.getAttribute("data-status")) === "checked", { timeout: 300_000, interval: 250, message: `${LEAN} checked` });
      const declRow = panel.locator(`[data-testid="lean-checked-declaration"][data-name="${DECLARATION}"]`);
      await declRow.waitFor({ timeout: 30_000 });
      await declRow.getByRole("button", { name: "Record evidence" }).click();
      const latest = declRow.getByTestId("lean-evidence-latest");
      await latest.waitFor({ timeout: 15_000 });
      await shoot("lean-checked");

      // Associate the declaration with the theorem, then change the Lean file: stale evidence.
      const declaration = await waitFor(
        async () => {
          await invoke(page, "graph_reconcile", { force: false });
          return (await snapshotOf(page, ["formalDeclaration"])).entities.find((e) => e.label === DECLARATION)?.id;
        },
        { timeout: 30_000, message: "the declaration entity" },
      );
      ids.decl = declaration;
      await invoke(page, "lean_associate_declaration", { request: { declaration: { kind: "entity", entity: declaration }, target: ids.thm, note: null } });
      const before = await invoke(page, "document_state", { document: leanDoc });
      await invoke(page, "document_apply", {
        request: {
          id: `v2-tour-${Date.now()}`,
          origin: { kind: "source" },
          edits: [{ document: leanDoc, baseRevision: before.info.revision, patches: [{ range: { start: before.text.length, end: before.text.length }, text: "\n-- a later edit\n" }] }],
          undo: { kind: "newGroup" },
          conflict: "reject",
          label: null,
          selection: null,
        },
      });
      await invoke(page, "document_save", { document: leanDoc, overwrite: false });
      await waitFor(
        async () => {
          await invoke(page, "graph_reconcile", { force: false });
          const d = await invoke(page, "graph_entity", { entity: declaration });
          return d.evidenceFreshness.some(([, f]) => f.kind === "stale");
        },
        { timeout: 60_000, interval: 500, message: "the declaration's evidence is stale" },
      );
    }

    // ---- 6. Graph: every category, selection, focus, search-dimmed nodes -------------
    const node = (id) => page.locator(`[data-testid="graph-canvas"] .react-flow__node[data-id="${id}"]`);
    await showPage(page, "graph");
    const canvas = page.getByTestId("graph-canvas");
    await canvas.waitFor({ timeout: 15_000 });
    for (const key of ["thm", "lem", "col", "exp", "ref"]) await node(ids[key]).waitFor({ timeout: 15_000 });
    await waitFor(async () => (await page.getByTestId("graph-save-state").first().getAttribute("data-state")) === "saved", { timeout: 15_000, message: "layout saved" });
    await canvas.getByRole("button", { name: "Fit to view" }).click();
    await settle(page);
    const categories = await canvas.locator(".ui-node-card").evaluateAll((els) => [...new Set(els.map((e) => e.getAttribute("data-category")))].sort());
    console.log(`graph categories on the canvas: ${categories.join(", ")}`);
    for (const category of ["structure", "document", "data", "math", "reference", "workflow"]) {
      assert.ok(categories.includes(category), `the canvas shows a ${category} node (${categories.join(", ")})`);
    }
    if (ids.decl) assert.ok(categories.includes("formal"), `the canvas shows a formal node (${categories.join(", ")})`);
    await dismissNotifications(page);
    /** Fit the view to the mathematical cluster with its neighbours of other categories, then select the theorem. */
    const frameCluster = async (select = "thm") => {
      // Every node of the cluster on screen before selecting them.
      await canvas.getByRole("button", { name: "Fit to view" }).click();
      await settle(page);
      const keys = ["fnd", "prf", "thm", "hyp", "lem", "def", "col", "ref", "exp", "wf"].filter((k) => ids[k]);
      await node(ids[keys[0]]).click();
      for (const k of keys.slice(1)) await node(ids[k]).click({ modifiers: ["Control"] });
      await canvas.getByRole("button", { name: "Fit to selection" }).click();
      await settle(page);
      // Clear the multi-selection on the background (the view stays), then select the theorem alone.
      await canvas.locator(".react-flow__pane").click({ position: { x: 8, y: 8 } });
      await node(ids[select]).click();
    };
    await frameCluster();
    const inspector = page.getByTestId("graph-entity-inspector");
    await inspector.waitFor({ timeout: 10_000 });
    assert.equal(await inspector.getAttribute("data-entity-id"), ids.thm);
    await shoot("graph-canvas-selected-focus", async () => {
      // Keyboard focus on another node (focus ring distinct from the selection outline).
      await node(ids.lem).focus();
    });
    const search = page.getByRole("searchbox", { name: "Search the graph" });
    await search.fill("rate");
    await settle(page);
    await shoot("graph-search-dimmed");
    await search.fill("");

    // Mathematics: dependencies (verdict and stale evidence badges) and the verification table.
    await page.getByRole("radio", { name: "Mathematics" }).click();
    const mathCanvas = page.getByTestId("graph-math-canvas");
    await mathCanvas.waitFor({ timeout: 10_000 });
    await mathCanvas.locator(`.react-flow__node[data-id="${ids.thm}"]`).waitFor({ timeout: 10_000 });
    await shoot("graph-math-dependencies");
    await page.getByRole("radio", { name: "Verification" }).click();
    const verification = page.getByRole("grid", { name: "Verification" });
    await verification.waitFor({ timeout: 10_000 });
    await verification.locator(`[data-entity-id="${ids.thm}"]`).click();
    await page.getByTestId("graph-entity-inspector").waitFor({ timeout: 10_000 });
    await shoot("graph-math-verification");
    await page.getByRole("radio", { name: "Dependencies" }).click();
    await page.getByRole("radio", { name: "Project" }).click();
    await canvas.waitFor({ timeout: 10_000 });

    // ---- 7. Narrow window: the inspector becomes a drawer -----------------------------
    // Framed while wide (a narrow window's drawer would cover nodes being selected); the view is kept.
    await frameCluster("lem");
    await setViewport(page, NARROW);
    await dismissNotifications(page);
    // Narrowing closes the inspector; a new selection opens it as a drawer.
    await canvas.locator(".react-flow__pane").click({ position: { x: 8, y: 8 } });
    await node(ids.lem).click();
    await page.locator('[data-region="graph-inspector"][data-narrow]').waitFor({ timeout: 10_000 });
    await shoot("graph-narrow-drawer");
    await setViewport(page, WIDE);

    // ---- 8. Equation panel with a draft (Typst, Split) -------------------------------
    for (const session of await invoke(page, "research_sessions")) await invoke(page, "research_session_shutdown", { sessionId: session.id });
    const eqRoot = await makeEquationProject(fixture("equations"), "typst");
    await showPage(page, "editor");
    await invoke(page, "workspace_open", { path: eqRoot });
    await openDocumentIn(page, "main.typ", "Split");
    const s = equationSession(page, "main.typ");
    await s.settled("opened");
    await s.showPanel();
    await s.monacoCaret("InlineTarget.");
    await page.keyboard.press("Control+Shift+M");
    await s.draft("insert");
    await s.template("Fraction");
    await s.mathType("1");
    await page.keyboard.press("Tab");
    await s.template("Square root");
    await s.mathType("x^2");
    await shoot("equation-panel-draft");
    await page.keyboard.press("Escape");

    await applyTheme(page, "light");
    if (skipped.length) console.log(`skipped parts: ${skipped.join(" | ")}`);
    console.log(`${shots.length} screenshots${published ? ` (also in ${published})` : ""}`);
    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await app.close();
  }
}
