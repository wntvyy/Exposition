// M9 lane D: environment capture, run manifests, and reproductions through
// the real app's IPC (debug build) with the real restricted Docker adapter.
//
// Prints `ok:` / `DEFECT:` / `info:` lines, writes findings.json, and fails
// if any check failed. Run: `npm run e2e -- workflow-repro`. Requires Docker
// with the local image python:3.13-slim (never pulled); without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_DOCKER=1.
//
//   A. A restricted run records its environment: image name + identity, the
//      image's platform, interpreter, package list + digest, and the seed the
//      code saw (EXPOSITION_SEED).
//   B. workflow_run_manifest returns canonical JSON (sorted keys, identical
//      twice) with inputs/outputs/environments, without logs, secrets, or the
//      project path; workflow_run_manifest_export writes exactly those bytes
//      and refuses to replace a file that is not a run manifest.
//   C. workflow_run_reproduce_plan pins every input to the recorded digest,
//      shows no drift for the same image, and starting its plan with the
//      normal workflow_run_start records cause reproduce with the original's
//      seed and output digest.
//   D. A changed input blocks the reproduction (workflow.inputChanged); a run
//      of another project cannot be exported or reproduced.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const OBS = "x\n1\n2\n3\n";
const CODE = `import json, os, sys
with open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.json"), "w") as f:
    json.dump({"seed": os.environ.get("EXPOSITION_SEED"), "python": ".".join(map(str, sys.version_info[:3])), "rows": len(open("data/obs.csv").read().split())}, f)
`;

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 60_000 });
    return true;
  } catch {
    return false;
  }
}

function workflow(title) {
  return {
    schema: "exposition.workflow/1",
    title,
    description: "",
    preset: null,
    inputs: [{ id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }],
    steps: [{ id: "fit", label: "Fit", operation: { id: "research.pythonBatch", version: 1 }, parameters: {}, payload: { kind: "code", language: "python", source: CODE } }],
    connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "fit", port: "data" } }],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "fit", port: "dataset" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

function sortedEverywhere(value) {
  if (Array.isArray(value)) return value.every(sortedEverywhere);
  if (value && typeof value === "object") {
    const keys = Object.keys(value);
    return keys.every((k, i) => i === 0 || keys[i - 1] < k) && keys.every((k) => sortedEverywhere(value[k]));
  }
  return true;
}

export default async function workflowRepro({ launchApp, artifactsDir }) {
  if (!dockerAvailable()) {
    const message = `SKIPPED: Docker with the local image ${IMAGE} is not available`;
    if (process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const results = [];
  const expect = (id, ok, detail = "") => {
    results.push({ status: ok ? "ok" : "DEFECT", id, detail });
    console.log(`${ok ? "ok:" : "DEFECT:"} ${id}${detail ? ` — ${typeof detail === "string" ? detail : JSON.stringify(detail).slice(0, 600)}` : ""}`);
    return ok;
  };
  const base = await mkdtemp(join(tmpdir(), "xp-workflow-repro-"));
  const root = join(base, "project");
  const other = join(base, "other");
  for (const dir of [root, other]) {
    await mkdir(join(dir, "data"), { recursive: true });
    await writeFile(join(dir, "data", "obs.csv"), OBS);
  }
  const exportDir = join(base, "exports");
  await mkdir(exportDir, { recursive: true });
  const dataDir = join(base, "data");
  await mkdir(dataDir, { recursive: true });

  const app = await launchApp({ dataDir });
  const page = app.page;
  const finish = async (runId) => {
    let run;
    await waitFor(async () => {
      run = await invoke(page, "workflow_run_get", { run: runId });
      return run.state !== "running";
    }, { timeout: 240_000, interval: 500, message: `run ${runId} finishes` });
    return run;
  };
  const fileText = (rel) => readFile(join(root, ...rel.split("/")), "utf8");
  const outputOf = async (run) => {
    const value = run.steps[0].attempts.at(-1).outputs.find((o) => o.port === "dataset")?.value;
    return value ? { sha256: value.sha256, json: JSON.parse(await fileText(value.path)) } : null;
  };

  try {
    await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: workflow("Repro") } });
    const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    expect("A plan has no blockers", plan.blockers.length === 0, plan.blockers);
    const started = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
    const run = await finish(started.id);
    expect("A run succeeded", run.state === "succeeded", { state: run.state, message: run.message });

    // ---- A. environment ----------------------------------------------------
    const env = run.steps[0].attempts.at(-1).environment;
    const out = await outputOf(run);
    console.log(`info: environment ${JSON.stringify({ ...env, packages: env?.packages?.length })}`);
    expect("A environment names the image and the approved identity", env?.image?.name === IMAGE && env?.image?.id === plan.execution.restrictedImageId, env?.image);
    expect("A environment has the image's platform and interpreter", env?.os === "linux" && /^3\.13\./.test(env?.interpreter?.version ?? ""), [env?.os, env?.interpreter]);
    expect("A environment lists the image packages with a digest", env?.packages?.some((p) => p.name === "pip") && /^[0-9a-f]{64}$/.test(env?.packagesDigest ?? ""), env?.packagesDigest);
    expect("A the code saw the recorded seed", out?.json.seed === String(env?.seed) && out?.json.python === env?.interpreter?.version, { seen: out?.json, recorded: env?.seed });

    // ---- B. manifest ---------------------------------------------------------
    const json1 = await invoke(page, "workflow_run_manifest", { run: run.id });
    const json2 = await invoke(page, "workflow_run_manifest", { run: run.id });
    const manifest = JSON.parse(json1);
    await writeFile(join(artifactsDir, "manifest.json"), json1);
    expect("B manifest is identical twice and sorted at every level", json1 === json2 && sortedEverywhere(manifest));
    expect(
      "B manifest has revision, hashes, inputs, outputs, environment, budgets, cause",
      manifest.revision === run.revision && manifest.definitionHash === run.definitionHash && manifest.approvalHash === plan.approvalHash &&
        manifest.inputs[0]?.path === "data/obs.csv" && manifest.outputs[0]?.sha256 === out?.sha256 &&
        manifest.steps[0].attempts[0].environment?.seed === env?.seed && manifest.budgets && manifest.usage && manifest.cause?.kind === "manual",
      { inputs: manifest.inputs, outputs: manifest.outputs, cause: manifest.cause },
    );
    const lower = json1.toLowerCase();
    expect("B manifest has no project path, logs, or key", !lower.includes(root.toLowerCase().replaceAll("\\", "\\\\")) && !lower.includes(root.toLowerCase()) && !json1.includes("[stdout]") && !/sk-ant-/.test(json1));
    const dest = join(exportDir, "repro-manifest.json");
    const exported = await invoke(page, "workflow_run_manifest_export", { run: run.id, destination: dest });
    const written = await readFile(dest, "utf8");
    expect("B export writes exactly the manifest", written === json1 && exported.bytes === Buffer.byteLength(json1) && /^[0-9a-f]{64}$/.test(exported.sha256), exported);
    const foreign = join(exportDir, "package.json");
    await writeFile(foreign, '{"name": "keep me"}');
    const refused = await invokeError(page, "workflow_run_manifest_export", { run: run.id, destination: foreign });
    expect("B export refuses to replace a file that is not a run manifest", refused.error?.code === "workflow.invalidDestination" && (await readFile(foreign, "utf8")) === '{"name": "keep me"}', refused.error ?? "replaced");
    const relative = await invokeError(page, "workflow_run_manifest_export", { run: run.id, destination: "manifest.json" });
    expect("B export refuses a relative destination", !!relative.error, relative.error);
    // Exposition's own records are never an export destination (as for bundles).
    for (const [label, dest] of [
      ["the project's .exposition folder", join(root, ".exposition", "run-manifest.json")],
      ["the app data folder", join(dataDir, "run-manifest.json")],
    ]) {
      const res = await invokeError(page, "workflow_run_manifest_export", { run: run.id, destination: dest });
      expect(`B export refuses ${label}`, !!res.error && !existsSync(dest), res.error ?? `written to ${dest}`);
    }

    // ---- C. reproduce ----------------------------------------------------------
    const repro = await invoke(page, "workflow_run_reproduce_plan", { run: run.id });
    expect("C reproduction names the run and pins every input to the recorded digest", repro.reproduces === run.id && repro.plan.inputs.every((i) => i.pinned && i.sha256 === run.plan.inputs.find((r) => r.id === i.id)?.sha256), repro.plan.inputs);
    expect("C same image: no drift", repro.drift.length === 0, repro.drift);
    expect("C plan notices say what it reproduces and the seeds", repro.plan.notices.some((n) => n.includes(`reproduces run ${run.id}`)) && repro.plan.notices.some((n) => n.includes("EXPOSITION_SEED")), repro.plan.notices);
    const copyStarted = await invoke(page, "workflow_run_start", { request: { plan: repro.plan.plan, approvalHash: repro.plan.approvalHash } });
    const copy = await finish(copyStarted.id);
    const copyOut = await outputOf(copy);
    expect("C the reproduction records cause reproduce", copy.cause?.kind === "reproduce" && copy.cause?.of === run.id, copy.cause);
    expect("C the reproduction got the same seed and reproduced the output digest", copy.steps[0].attempts.at(-1).environment?.seed === env?.seed && copyOut?.sha256 === out?.sha256, { seed: copy.steps[0].attempts.at(-1).environment?.seed, out: copyOut?.sha256, original: out?.sha256 });
    const copyManifest = JSON.parse(await invoke(page, "workflow_run_manifest", { run: copy.id }));
    expect("C the reproduction's manifest records its cause", copyManifest.cause?.kind === "reproduce" && copyManifest.cause?.of === run.id, copyManifest.cause);

    // ---- D. refusals -------------------------------------------------------------
    await writeFile(join(root, "data", "obs.csv"), "x\n9\n");
    const changed = await invoke(page, "workflow_run_reproduce_plan", { run: run.id });
    const blocker = changed.plan.blockers.find((b) => b.code === "workflow.inputChanged");
    expect("D a changed input blocks the reproduction", blocker?.input === "data", changed.plan.blockers);
    const start = await invokeError(page, "workflow_run_start", { request: { plan: changed.plan.plan, approvalHash: changed.plan.approvalHash } });
    expect("D its start is refused", start.error?.code === "workflow.inputChanged", start.error ?? "started");
    await invoke(page, "workspace_open", { path: other });
    const foreignManifest = await invokeError(page, "workflow_run_manifest", { run: run.id });
    const foreignRepro = await invokeError(page, "workflow_run_reproduce_plan", { run: run.id });
    expect("D another project's run cannot be exported or reproduced", foreignManifest.error?.code === "workflow.unknownRun" && foreignRepro.error?.code === "workflow.unknownRun", [foreignManifest.error, foreignRepro.error]);
  } finally {
    await page.screenshot({ path: join(artifactsDir, "final.png") }).catch(() => {});
    await app.close();
    console.log(`info: project folders ${base}`);
  }
  await writeFile(join(artifactsDir, "findings.json"), JSON.stringify(results, null, 2));
  const defects = results.filter((r) => r.status === "DEFECT");
  if (defects.length) throw new Error(`${defects.length} check(s) failed: ${defects.map((d) => d.id).join("; ")}`);
}
