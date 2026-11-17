// The built-in Numerical experiment preset, UNTOUCHED, in the real app
// (XR-E-01): created without a script, the run step starts with the inline
// Python template. The template must run as generated, in the restricted
// Docker runner:
//
// A. No data: the template's fallback series; the run succeeds, writes a
//    valid table.csv (summary statistics) and figure.svg, and the figure is
//    retained.
// B. A declared CSV input (and a manuscript): the template reads the CSV at
//    its project path; the statistics are those of the data; the figure is
//    retained and a reviewed promotion is proposed (not applied).
// Both: the escaped "\n" of the figure-writing line is intact in the saved
// revision and in the plan's full code (what the run review shows).
//
// Requires Docker with the local image python:3.13-slim; without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_DOCKER=1.
// Observations: test-results/e2e/workflow-preset-template/observations.json.
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const RUN_TIMEOUT = 240_000;
const DATA = "x\n1\n2\n3\n4\n";
const PAPER = "= Results\n\nThe experiment is below.\n";
/** The figure-writing line as it must read in Python: escaped newlines. */
const WRITE_LINE = 'f.write("\\n".join(svg) + "\\n")';

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 30_000 });
    return true;
  } catch {
    return false;
  }
}

export default async function workflowPresetTemplate({ launchApp, artifactsDir, assert }) {
  if (!dockerAvailable()) {
    const message = `SKIPPED: Docker with the local image ${IMAGE} is not available`;
    if (process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-preset-template-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), DATA);
  await writeFile(join(root, "paper.typ"), PAPER);

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 600)}`);
  };
  const app = await launchApp({ dataDir });
  const page = app.page;
  const read = async (rel) => readFile(join(root, ...rel.split("/")), "utf8");
  const finish = async (runId) => {
    let run;
    await waitFor(async () => {
      run = await invoke(page, "workflow_run_get", { run: runId });
      return run.state !== "running";
    }, { timeout: RUN_TIMEOUT, interval: 500, message: `run ${runId} finishes` });
    return run;
  };
  const lastAttempt = (run, id) => run.steps.find((s) => s.step === id)?.attempts.at(-1);
  const stats = (csv) => Object.fromEntries(csv.trim().split(/\r?\n/).slice(1).map((l) => l.split(",")));

  /** Create the preset as a person would (no edits), save, plan, run; check the template's outputs. */
  const runPreset = async (label, request) => {
    const definition = await invoke(page, "workflow_from_preset", {
      request: { preset: "numericalExperiment", script: null, data: null, manuscript: null, leanFile: null, ...request },
    });
    const template = definition.steps.find((s) => s.id === "run")?.payload?.source ?? "";
    note(`${label}-template-has-escaped-newlines`, template.includes(WRITE_LINE) && !/"\r?\n/.test(template.split("svg = [")[1] ?? "x"), template.slice(-200));
    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition } });
    note(`${label}-saved-runnable`, saved.validation.state === "wellFormed" && saved.validation.runnable === true, {
      state: saved.validation.state,
      runnable: saved.validation.runnable,
      reason: saved.validation.runnableReason,
      diagnostics: saved.validation.diagnostics.map((d) => d.code),
    });
    const stored = await invoke(page, "workflow_get", { id: saved.id, revision: null });
    const storedSource = stored.definition.steps.find((s) => s.id === "run")?.payload?.source ?? "";
    note(`${label}-saved-revision-keeps-escaped-newlines`, storedSource === template && storedSource.includes(WRITE_LINE), { revision: stored.revision });
    const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    const planSource = plan.steps.find((s) => s.step === "run")?.payload?.source ?? "";
    note(`${label}-plan-shows-the-full-code-with-escaped-newlines`, planSource === template && planSource.includes(WRITE_LINE), planSource.slice(-120));
    note(`${label}-plan-no-blockers`, plan.blockers.length === 0, plan.blockers);
    if (plan.blockers.length) return null;
    const started = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
    const run = await finish(started.id);
    const attempt = lastAttempt(run, "run");
    note(`${label}-run-succeeded`, run.state === "succeeded", {
      state: run.state,
      steps: run.steps.map((s) => [s.step, s.state, s.attempts.at(-1)?.code, s.attempts.at(-1)?.message]),
      log: (attempt?.log ?? "").slice(-400),
    });
    const outputs = Object.fromEntries((attempt?.outputs ?? []).map((o) => [o.port, o.value]));
    const table = outputs.table?.path ? await read(outputs.table.path) : "";
    const figure = outputs.figure?.path ? await read(outputs.figure.path) : "";
    note(`${label}-figure-is-valid-svg`, /^<svg xmlns="http:\/\/www\.w3\.org\/2000\/svg"[^>]*>\n/.test(figure) && figure.endsWith("</svg>\n") && /<polyline [^>]*points="[\d., ]+"\/>/.test(figure), figure.slice(0, 160));
    const retained = lastAttempt(run, "retain")?.outputs[0]?.value;
    note(`${label}-figure-retained`, retained?.kind === "retained" && retained.entity?.startsWith("ent-"), retained);
    return { run, table: stats(table), raw: table };
  };

  try {
    await page.setViewportSize({ width: 1600, height: 1000 }).catch(() => {});
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    const set = await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    note("docker-image-configured", set.settings.docker.image === IMAGE, set.settings.docker);
    await invoke(page, "research_space_enable").catch(() => {});

    // ---- A. No data: the fallback series -------------------------------------
    const a = await runPreset("fallback", {});
    if (a) note("fallback-table-statistics", a.table.count === "60" && ["mean", "std", "min", "max"].every((k) => Number.isFinite(Number(a.table[k]))), a.raw);

    // ---- B. Declared CSV input and a manuscript ------------------------------
    const b = await runPreset("csv", { data: "data/obs.csv", manuscript: "paper.typ" });
    if (b) {
      note("csv-table-statistics-are-the-data", b.table.count === "4" && b.table.mean === "2.5" && b.table.min === "1" && b.table.max === "4" && b.table.std === "1.11803", b.raw);
      const promo = lastAttempt(b.run, "promote")?.outputs[0]?.value;
      note("csv-promotion-proposed-not-applied", promo?.kind === "promotionProposal" && promo.status === "pending" && (await read("paper.typ")) === PAPER, promo);
      note("csv-input-unchanged", (await read("data/obs.csv")) === DATA, "data/obs.csv");
    }

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    note("no-console-errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
