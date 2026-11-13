// M8: workflows run through the real scoped research sandbox (Docker), end to
// end in the real app.
//
// A. Numerical experiment preset (script + dataset + manuscript): reviewable
//    plan (inputs pinned by SHA-256, restricted containment, no blockers),
//    started with its approval hash; one step at a time; the Python batch sees
//    only its declared inputs (read-only), no network, no host environment,
//    non-root; outputs validated (hash, provenance); the figure retained with
//    computational evidence labelled restricted; a reviewed promotion applied.
// B. Agent-generated equivalent (scripted provider, debug build): proposed via
//    workflow_propose (saved, not run), customized by the user (new revision),
//    planned and run.
// C. Containment and contract failures: escaping/unsafe outputs refused and
//    never copied into the project; an undeclared import fails visibly; forged
//    evidence stays an ordinary file and cannot feed a formal-evidence port;
//    orphan steps and incompatible ports are not runnable; restricted Lean is
//    refused; a stale plan is refused; cancel removes the container.
// D. Restart without replay: the app is killed during a step; on relaunch the
//    run is interrupted, nothing is dispatched again, and it can be acknowledged.
//
// Requires Docker with the local image python:3.13-slim; without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_DOCKER=1.
// Observations: test-results/e2e/workflow-run/observations.json.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { hardKill } from "../app.mjs";
import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const RUN_TIMEOUT = 240_000;
const OBS = "x,y\n0,1\n1,3\n2,5\n3,7\n";
const PAPER = "= Results\n\nThe experiment is below.\n";

// The declared script: reads the declared CSV at its project path, probes the
// sandbox, writes table.csv, figure.svg, and dataset.csv (probe results).
const EXPERIMENT = `import csv, os, socket, sys
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
probes = []
def probe(k, v): probes.append((k, str(v)))
probe("paper_visible", os.path.exists("paper.typ"))
probe("helpers_visible", os.path.exists("helpers.py"))
try:
    open("data/obs.csv", "a").write("9,9\\n"); probe("input_writable", True)
except Exception as e:
    probe("input_writable", False)
try:
    socket.create_connection(("1.1.1.1", 53), timeout=3); probe("network", True)
except Exception:
    probe("network", False)
probe("uid", os.getuid())
probe("host_env", any(k in os.environ for k in ("USERPROFILE", "APPDATA", "LOCALAPPDATA", "ANTHROPIC_API_KEY", "PATHEXT")))
probe("platform", sys.platform)
with open(os.path.join(out, "dataset.csv"), "w", newline="") as f:
    w = csv.writer(f); w.writerow(["probe", "value"]); w.writerows(probes)
`;

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

function dockerAvailable() {
  try {
    execFileSync("docker", ["image", "inspect", IMAGE], { stdio: "ignore", timeout: 30_000 });
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

/** A one-step restricted Python workflow over the declared data, realizing a dataset outcome. */
function pythonWorkflow(title, code, { outcomePort = "dataset", extraSteps = [], extraConnections = [], ceiling } = {}) {
  return {
    schema: "exposition.workflow/1",
    title,
    description: "",
    preset: null,
    inputs: [{ id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }],
    steps: [
      { id: "run", label: "Run the code", operation: { id: "research.pythonBatch", version: 1 }, parameters: { timeoutSeconds: { kind: "integer", value: 600 } }, payload: { kind: "code", language: "python", source: code } },
      ...extraSteps,
    ],
    connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } }, ...extraConnections],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: outcomePort }, optional: false }],
    ceiling: ceiling ?? { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

export default async function workflowRun({ launchApp, artifactsDir, assert }) {
  if (!dockerAvailable()) {
    const message = `SKIPPED: Docker with the local image ${IMAGE} is not available`;
    if (process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-workflow-run-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "data"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), OBS);
  await writeFile(join(root, "paper.typ"), PAPER);
  await writeFile(join(root, "experiment.py"), EXPERIMENT);
  await writeFile(join(root, "helpers.py"), "VALUE = 42\n");

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 600)}`);
  };
  const shots = [];
  let app = await launchApp({ dataDir });
  let page = app.page;
  const shot = async (name) => {
    await page.waitForTimeout(250);
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path }).catch(() => {});
    shots.push(path);
  };
  const fileHash = async (rel) => sha256(await readFile(join(root, ...rel.split("/"))));
  const save = async (definition, id = null, expectedRevision = null) =>
    invoke(page, "workflow_save", { request: { id, expectedRevision, definition } });
  const finish = async (runId, timeout = RUN_TIMEOUT) => {
    let run;
    await waitFor(async () => {
      run = await invoke(page, "workflow_run_get", { run: runId });
      return run.state !== "running";
    }, { timeout, interval: 500, message: `run ${runId} finishes` });
    return run;
  };
  const planAndStart = async (saved) => {
    const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    if (plan.blockers.length) return { plan, run: null };
    const run = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
    return { plan, run };
  };
  const stepOf = (run, id) => run.steps.find((s) => s.step === id);
  const lastAttempt = (run, id) => stepOf(run, id)?.attempts.at(-1);

  try {
    await page.setViewportSize({ width: 1600, height: 1000 }).catch(() => {});
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    const debugBuild = (await invoke(page, "app_info")).debugBuild;
    const set = await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
    note("docker-image-configured", set.settings.docker.image === IMAGE, set.settings.docker);
    await invoke(page, "research_space_enable").catch(() => {});

    // Operations now report real availability.
    const ops = await invoke(page, "workflow_operations");
    const batchOp = ops.find((o) => o.id === "research.pythonBatch");
    note("restricted-python-runner-available", batchOp?.runner.available === true, batchOp?.runner);

    // ---- A. Numerical experiment preset ------------------------------------
    const preset = await invoke(page, "workflow_from_preset", {
      request: { preset: "numericalExperiment", script: "experiment.py", data: "data/obs.csv", manuscript: "paper.typ", leanFile: null },
    });
    const savedA = await save(preset);
    note("preset-saved-runnable", savedA.validation.state === "wellFormed" && savedA.validation.runnable === true, {
      state: savedA.validation.state,
      runnable: savedA.validation.runnable,
      reason: savedA.validation.runnableReason,
      diagnostics: savedA.validation.diagnostics.map((d) => d.code),
    });
    const planA = await invoke(page, "workflow_run_plan", { id: savedA.id, revision: null });
    const inputHashes = Object.fromEntries(planA.inputs.map((i) => [i.path, i.sha256]));
    note("plan-no-blockers", planA.blockers.length === 0, planA.blockers);
    note("plan-steps-in-order", planA.steps.map((s) => s.step).join(",") === "run,retain,promote", planA.steps.map((s) => s.step));
    note("plan-restricted-containment", planA.steps[0].containment === "restrictedContainer" && planA.steps[0].effects.includes("executeRestricted"), planA.steps[0]);
    note(
      "plan-inputs-pinned-by-hash",
      inputHashes["experiment.py"] === (await fileHash("experiment.py")) && inputHashes["data/obs.csv"] === (await fileHash("data/obs.csv")),
      planA.inputs,
    );
    note("plan-binds-image-no-native", planA.execution.restrictedImage === IMAGE && planA.execution.nativeSteps.length === 0, planA.execution);
    const badStart = await invokeError(page, "workflow_run_start", { request: { plan: planA.plan, approvalHash: "0".repeat(64) } });
    note("start-needs-the-approved-hash", /planStale|approval/i.test(JSON.stringify(badStart)), badStart);
    const reused = await invokeError(page, "workflow_run_start", { request: { plan: planA.plan, approvalHash: planA.approvalHash } });
    note("a-refused-plan-cannot-be-reused", /planStale/.test(JSON.stringify(reused)), reused);
    const planA2 = await invoke(page, "workflow_run_plan", { id: savedA.id, revision: null });
    note("same-content-same-approval-hash", planA2.approvalHash === planA.approvalHash && planA2.plan !== planA.plan, { a: planA.approvalHash, b: planA2.approvalHash });
    const obsBefore = await fileHash("data/obs.csv");
    let runA = await invoke(page, "workflow_run_start", { request: { plan: planA2.plan, approvalHash: planA2.approvalHash } });
    note("run-started", runA.state === "running", runA.state);
    runA = await finish(runA.id);
    note("run-succeeded", runA.state === "succeeded", { state: runA.state, code: runA.code, message: runA.message, steps: runA.steps.map((s) => [s.step, s.state, s.attempts.at(-1)?.code, s.attempts.at(-1)?.message]) });
    const attemptsA = runA.steps.map((s) => s.attempts.map((a) => [a.intentAtMs, a.finishedAtMs]));
    const sequential = attemptsA.flat().every((a, i, all) => i === 0 || (all[i - 1][1] ?? Infinity) <= a[0]);
    note("one-step-at-a-time", sequential && runA.steps.every((s) => s.attempts.length === 1), attemptsA);
    const runAttempt = lastAttempt(runA, "run");
    const outputs = Object.fromEntries((runAttempt?.outputs ?? []).map((o) => [o.port, o]));
    const fig = outputs.figure?.value;
    note("figure-output-validated", fig?.kind === "file" && fig.path.endsWith("figure.svg") && fig.sha256 === (await fileHash(fig.path)), fig);
    note("provenance-names-the-attempt", outputs.figure?.provenance.run === runA.id && outputs.figure?.provenance.step === "run" && outputs.figure?.provenance.attempt === 1, outputs.figure?.provenance);
    const probesFile = outputs.dataset?.value?.path;
    const probes = probesFile ? Object.fromEntries((await readFile(join(root, ...probesFile.split("/")), "utf8")).trim().split(/\r?\n/).slice(1).map((l) => l.split(","))) : {};
    note("sandbox-sees-only-declared-inputs", probes.paper_visible === "False" && probes.helpers_visible === "False", probes);
    note("sandbox-inputs-read-only", probes.input_writable === "False" && (await fileHash("data/obs.csv")) === obsBefore, probes.input_writable);
    note("sandbox-no-network", probes.network === "False", probes.network);
    note("sandbox-non-root-linux-no-host-env", probes.uid !== "0" && probes.platform === "linux" && probes.host_env === "False", probes);
    const retained = lastAttempt(runA, "retain")?.outputs[0]?.value;
    note("figure-retained", retained?.kind === "retained" && retained.entity?.startsWith("ent-"), retained);
    if (retained?.entity) {
      const detail = await invoke(page, "graph_entity", { entity: retained.entity });
      const text = JSON.stringify(detail);
      note("retained-evidence-labelled-restricted", /computational/i.test(text) && /restricted/i.test(text) && !/trusted native/i.test(text), text.slice(0, 800));
    }
    const promo = lastAttempt(runA, "promote")?.outputs[0]?.value;
    note("promotion-proposed-not-applied", promo?.kind === "promotionProposal" && promo.status === "pending" && (await readFile(join(root, "paper.typ"), "utf8")) === PAPER, promo);
    if (promo?.promotion) {
      const preview = await invoke(page, "research_promotion_preview", { promotion: promo.promotion });
      note("promotion-review-shows-exact-markup", !!preview && preview.path === "paper.typ" && preview.edits.some((e) => e.text.includes("figure.svg")), preview?.edits);
      const applied = await invoke(page, "research_promotion_approve", { promotion: promo.promotion });
      const text = preview ? (await invoke(page, "document_state", { document: preview.document })).text : "";
      note("reviewed-promotion-applied-to-the-document", applied.graph.kind === "recorded" && text.includes("figure.svg") && text !== PAPER, { graph: applied.graph, text: text.slice(0, 300) });
      const after = await invoke(page, "workflow_run_get", { run: runA.id });
      note("run-shows-promotion-accepted", lastAttempt(after, "promote")?.outputs[0]?.value?.status === "accepted", lastAttempt(after, "promote")?.outputs[0]?.value);
    }

    // UI: the run is visible in the workflow's run history (lane U test ids).
    try {
      const runs = await invoke(page, "workflow_runs", { workflow: savedA.id });
      note("run-listed", runs.some((r) => r.id === runA.id && r.state === "succeeded"), runs.map((r) => [r.id, r.state]));
    } catch (e) {
      note("run-listed", false, String(e));
    }

    // ---- B. Agent-generated, then customized --------------------------------
    if (debugBuild) {
      await invoke(page, "agents_provider_update", {
        settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
      });
      const AGENT_CODE = 'import os\nopen(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("k,v\\nagent,1\\n")\n';
      await invoke(page, "agents_dev_load_script", {
        script: {
          turns: [
            {
              steps: [
                {
                  toolUse: {
                    id: "wf",
                    name: "workflow_propose",
                    input: {
                      workflow_id: "",
                      expected_revision: -1,
                      title: "Agent experiment",
                      description: "Transform the observations.",
                      preset: "",
                      inputs: [{ id: "data", label: "Observations", port_type: "dataset", path: "data/obs.csv", pinned_hash: "" }],
                      steps: [{ id: "run", label: "Run", operation: "research.pythonBatch", version: 1, parameters: [], payload: { kind: "code", language: "python", source: AGENT_CODE } }],
                      connections: [{ kind: "data", from_step: "", from_port: "", from_input: "data", to_step: "run", to_port: "data" }],
                      outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", step: "run", port: "dataset", optional: false }],
                      ceiling: { execution: "restricted", model_requests: false, retain_results: false, propose_changes: false },
                    },
                  },
                },
              ],
            },
            { steps: [{ text: "Saved the workflow draft for your review." }] },
          ],
        },
      });
      const task = await invoke(page, "agents_task_create", {
        request: { workspaceRoot: info.root, consent: "scopeContent", prompt: "Propose an experiment workflow", scope: { kind: "workspace" }, permission: "reviewEdits", execution: "restricted", budgets: null },
      });
      const answered = new Set();
      await waitFor(async () => {
        const detail = await invoke(page, "agents_task_detail", { task: task.id });
        for (const approval of detail.approvals) {
          if (answered.has(approval.id)) continue;
          answered.add(approval.id);
          await invoke(page, "agents_approval_resolve", { task: task.id, approval: approval.id, approved: approval.tool === "workflow_propose" });
        }
        return !["queued", "running", "awaitingApproval"].includes(detail.summary.status.kind);
      }, { timeout: 90_000, interval: 300, message: "agent task finishes" }).catch((e) => note("agent-task-finished", false, String(e)));
      const agentDraft = (await invoke(page, "workflow_list")).find((w) => w.title === "Agent experiment");
      const runsBeforeCustomize = (await invoke(page, "workflow_runs", { workflow: null })).length;
      note("agent-draft-saved-not-run", !!agentDraft && agentDraft.origin?.kind === "agent" && runsBeforeCustomize === 1, { agentDraft, runsBeforeCustomize });
      if (agentDraft) {
        const got = await invoke(page, "workflow_get", { id: agentDraft.id, revision: null });
        const customized = structuredClone(got.definition);
        customized.steps[0].payload.source = AGENT_CODE.replace("agent,1", "customized,2");
        const savedB = await save(customized, got.id, got.revision);
        note("customized-new-revision", savedB.revision === got.revision + 1, { from: got.revision, to: savedB.revision });
        const { plan: planB, run } = await planAndStart(savedB);
        note("customized-plan-shows-full-code", planB.steps[0].payload?.source?.includes("customized,2"), planB.steps[0].payload);
        const runB = run ? await finish(run.id) : null;
        const dataOut = runB && lastAttempt(runB, "run")?.outputs.find((o) => o.port === "dataset")?.value;
        const content = dataOut?.path ? await readFile(join(root, ...dataOut.path.split("/")), "utf8") : "";
        note("agent-generated-customized-workflow-ran", runB?.state === "succeeded" && content.includes("customized,2"), { state: runB?.state, blockers: planB.blockers, content });
      }
    } else {
      note("agent-generated-workflow (release build: covered by live scenarios)", true, "skipped in release");
    }

    // ---- C. Containment and contract failures -------------------------------
    const ESCAPE = `import os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
open(os.path.join(out, "dataset.csv"), "w").write("a,b\\n1,2\\n")
os.symlink("/etc/passwd", os.path.join(out, "table.csv"))
os.symlink("/", os.path.join(out, "up"))
os.mkdir(os.path.join(out, "sub")); open(os.path.join(out, "sub", "x.csv"), "w").write("1")
os.mkfifo(os.path.join(out, "fifo"))
open(os.path.join(out, "figure.png"), "w").write("<script>alert(1)</script> not a png")
open(os.path.join(out, "extra.csv"), "w").write("1\\n"); os.link(os.path.join(out, "extra.csv"), os.path.join(out, "extra2.csv"))
open(os.path.join(out, ".hidden"), "w").write("1")
`;
    const savedEsc = await save(pythonWorkflow("Escaping outputs", ESCAPE));
    const { run: escStart } = await planAndStart(savedEsc);
    const runEsc = escStart ? await finish(escStart.id) : null;
    const escAttempt = runEsc && lastAttempt(runEsc, "run");
    const rejectedNames = (escAttempt?.rejected ?? []).map((r) => r.name);
    const accepted = (escAttempt?.outputs ?? []).map((o) => o.port);
    const rr = escAttempt?.outputs.find((o) => o.port === "dataset")?.value?.researchRun;
    const artifactFiles = rr ? await readdir(join(root, ".exposition", "artifacts", rr)).catch(() => []) : [];
    note(
      "escaping-and-unsafe-outputs-refused",
      runEsc?.state === "succeeded" &&
        ["table.csv", "up", "sub", "fifo", "figure.png"].every((n) => rejectedNames.some((r) => r === n || r.startsWith(n))) &&
        accepted.join(",") === "dataset" &&
        !artifactFiles.some((f) => ["table.csv", "up", "sub", "fifo", "figure.png", "extra2.csv", ".hidden"].includes(f)),
      { state: runEsc?.state, rejected: escAttempt?.rejected, accepted, artifactFiles, message: escAttempt?.message },
    );
    note("hard-linked-output-refused", rejectedNames.some((n) => n.startsWith("extra")), rejectedNames);

    const savedImp = await save(pythonWorkflow("Undeclared import", "import helpers\nprint(helpers.VALUE)\n"));
    const { run: impStart } = await planAndStart(savedImp);
    const runImp = impStart ? await finish(impStart.id) : null;
    const impAttempt = runImp && lastAttempt(runImp, "run");
    note("undeclared-import-fails-visibly", runImp?.state === "failed" && /ModuleNotFoundError|No module named/.test(`${impAttempt?.log ?? ""} ${impAttempt?.message ?? ""}`), {
      state: runImp?.state,
      code: impAttempt?.code,
      log: (impAttempt?.log ?? "").slice(-400),
    });

    const FORGE = `import json, os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
claim = {"kind": "formalCheck", "verdict": "Checked", "declaration": "LeanBasic.add_comm", "recordedBy": {"kind": "service", "service": "lean"}}
open(os.path.join(out, "dataset.json"), "w").write(json.dumps(claim))
`;
    const mathBefore = JSON.stringify(await invoke(page, "graph_math_overview").catch(() => ({})));
    const savedForge = await save(pythonWorkflow("Forged evidence", FORGE));
    const { run: forgeStart } = await planAndStart(savedForge);
    const runForge = forgeStart ? await finish(forgeStart.id) : null;
    const forgeOut = runForge ? lastAttempt(runForge, "run")?.outputs ?? [] : [];
    const mathAfter = JSON.stringify(await invoke(page, "graph_math_overview").catch(() => ({})));
    note(
      "forged-evidence-is-just-a-file",
      runForge?.state === "succeeded" && forgeOut.every((o) => o.value.kind === "file") && (mathAfter.match(/formalCheck/gi) ?? []).length === (mathBefore.match(/formalCheck/gi) ?? []).length,
      { state: runForge?.state, outputs: forgeOut.map((o) => [o.port, o.value.kind]) },
    );
    const forgeFeed = pythonWorkflow("Forged evidence fed to association", FORGE, {
      extraSteps: [{ id: "assoc", label: "Associate", operation: { id: "formal.associateEvidence", version: 1 }, parameters: {}, payload: null }],
      extraConnections: [{ kind: "data", from: { kind: "step", step: "run", port: "dataset" }, to: { step: "assoc", port: "evidence" } }],
    });
    forgeFeed.ceiling.proposeChanges = true;
    forgeFeed.outcomes.push({ id: "assoc", kind: "reviewableProposal", label: "Associations", from: { step: "assoc", port: "proposal" }, optional: false });
    const vForge = await invoke(page, "workflow_validate", { definition: forgeFeed });
    note("dataset-cannot-stand-in-for-formal-evidence", vForge.runnable === false && vForge.diagnostics.some((d) => d.code === "workflow.serviceOwnedType" && d.rule === 6), vForge.diagnostics.map((d) => d.code));

    const orphan = pythonWorkflow("Orphan step", "pass\n", {
      extraSteps: [{ id: "lonely", label: "Lonely", operation: { id: "research.pythonBatch", version: 1 }, parameters: {}, payload: { kind: "code", language: "python", source: "pass\n" } }],
    });
    const vOrphan = await invoke(page, "workflow_validate", { definition: orphan });
    const incompatible = pythonWorkflow("Incompatible ports", "pass\n", {
      extraSteps: [{ id: "audit", label: "Audit", operation: { id: "lean.audit", version: 1 }, parameters: {}, payload: null }],
      extraConnections: [{ kind: "data", from: { kind: "step", step: "run", port: "table" }, to: { step: "audit", port: "source" } }],
    });
    const vIncompat = await invoke(page, "workflow_validate", { definition: incompatible });
    const savedOrphan = await save(orphan);
    const planOrphan = await invoke(page, "workflow_run_plan", { id: savedOrphan.id, revision: null });
    note(
      "orphan-and-incompatible-not-runnable",
      vOrphan.runnable === false &&
        vOrphan.diagnostics.some((d) => d.code === "workflow.orphanStep") &&
        vIncompat.diagnostics.some((d) => d.code === "workflow.incompatibleTypes") &&
        planOrphan.blockers.some((b) => b.code === "workflow.notWellFormed"),
      { orphan: vOrphan.diagnostics.map((d) => d.code), incompatible: vIncompat.diagnostics.map((d) => d.code), blockers: planOrphan.blockers.map((b) => b.code) },
    );
    const leanRestricted = {
      ...pythonWorkflow("Restricted Lean", "pass\n"),
      inputs: [{ id: "lean", label: "Lean", portType: "formalSource", source: { kind: "resource", path: "Proof.lean", version: { kind: "atRunStart" } } }],
      steps: [{ id: "audit", label: "Audit", operation: { id: "lean.audit", version: 1 }, parameters: {}, payload: null }],
      connections: [{ kind: "data", from: { kind: "input", input: "lean" }, to: { step: "audit", port: "source" } }],
      outcomes: [{ id: "ev", kind: "auditEvidence", label: "Evidence", from: { step: "audit", port: "evidence" }, optional: false }],
      ceiling: { execution: "restricted", modelRequests: false, retainResults: true, proposeChanges: false },
    };
    await writeFile(join(root, "Proof.lean"), "theorem t : 1 = 1 := rfl\n");
    const vLean = await invoke(page, "workflow_validate", { definition: leanRestricted });
    note("restricted-lean-refused", vLean.runnable === false && vLean.diagnostics.some((d) => d.code === "workflow.effectNotAllowed"), vLean.diagnostics.map((d) => [d.code, d.message]));

    // Stale plan: the input changes between review and start.
    const savedStale = await save(pythonWorkflow("Stale plan", 'import os\nopen(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("a\\n1\\n")\n'));
    const planStale = await invoke(page, "workflow_run_plan", { id: savedStale.id, revision: null });
    await writeFile(join(root, "data", "obs.csv"), `${OBS}4,9\n`);
    const stale = await invokeError(page, "workflow_run_start", { request: { plan: planStale.plan, approvalHash: planStale.approvalHash } });
    note("stale-plan-refused", /workflow\.planStale/.test(JSON.stringify(stale)), stale);
    await writeFile(join(root, "data", "obs.csv"), OBS);

    // Cancel: the container is removed.
    const savedSleep = await save(pythonWorkflow("Long step", "import time\ntime.sleep(300)\n"));
    const { run: sleepStart } = await planAndStart(savedSleep);
    if (sleepStart) {
      await waitFor(async () => lastAttempt(await invoke(page, "workflow_run_get", { run: sleepStart.id }), "run")?.state === "dispatched", { timeout: 60_000, message: "step dispatched" });
      await waitFor(() => managedContainers().length > 0, { timeout: 60_000, message: "container started" });
      const t0 = Date.now();
      await invoke(page, "workflow_run_cancel", { run: sleepStart.id });
      const canceled = await finish(sleepStart.id, 60_000);
      await waitFor(() => managedContainers().length === 0, { timeout: 30_000, message: "container removed" }).catch(() => {});
      note("cancel-stops-and-removes-container", canceled.state === "canceled" && stepOf(canceled, "run").state === "canceled" && managedContainers().length === 0, {
        state: canceled.state,
        ms: Date.now() - t0,
        containers: managedContainers(),
      });
    } else {
      note("cancel-stops-and-removes-container", false, "could not start");
    }

    // ---- D. Restart without replay ------------------------------------------
    const { run: crashStart } = await planAndStart(savedSleep);
    let crashRunId = crashStart?.id;
    if (crashStart) {
      await waitFor(async () => lastAttempt(await invoke(page, "workflow_run_get", { run: crashRunId }), "run")?.state === "dispatched", { timeout: 60_000, message: "step dispatched" });
      await shot("workflow-run-before-crash");
      hardKill(app.child.pid);
      await app.browser.close().catch(() => {});
      await waitFor(() => app.child.exitCode !== null || app.child.signalCode !== null, { message: "app exit" });
      const manifestsAtKill = (await readdir(join(root, ".exposition", "runs")).catch(() => [])).length;
      app = await launchApp({ dataDir });
      page = app.page;
      await page.setViewportSize({ width: 1600, height: 1000 }).catch(() => {});
      await invoke(page, "workspace_open", { path: root });
      await page.waitForTimeout(5000);
      const after = await invoke(page, "workflow_run_get", { run: crashRunId });
      const manifestsLater = (await readdir(join(root, ".exposition", "runs")).catch(() => [])).length;
      await waitFor(() => managedContainers().length === 0, { timeout: 60_000, message: "stale container swept" }).catch(() => {});
      note(
        "restart-interrupted-not-replayed",
        after.state === "interrupted" && stepOf(after, "run").state === "interrupted" && stepOf(after, "run").attempts.length === 1 && manifestsLater === manifestsAtKill,
        { state: after.state, step: stepOf(after, "run").state, attempts: stepOf(after, "run").attempts.map((a) => a.state), manifestsAtKill, manifestsLater, containers: managedContainers() },
      );
      const acked = await invoke(page, "workflow_run_acknowledge", { run: crashRunId });
      note("interrupted-run-acknowledged", acked.acknowledged === true && acked.state === "interrupted", { acknowledged: acked.acknowledged });
    } else {
      note("restart-interrupted-not-replayed", false, "could not start");
    }

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    note("no-console-errors", errors.length === 0, errors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    if (process.env.EXPOSITION_DEV_HOME && shots.length) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "workflows");
      await mkdir(dir, { recursive: true });
      for (const shotPath of shots) if (existsSync(shotPath)) await cp(shotPath, join(dir, shotPath.split(/[\\/]/).pop()));
    }
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
