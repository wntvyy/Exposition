// R-m8 independent review: workflow runs through the real restricted Docker
// adapter in the real app (debug build).
//
// Prints `ok:` / `DEFECT:` / `info:` lines, writes findings.json, and fails
// if any DEFECT was reproduced. Run: `npm run e2e -- review-m8-runs`.
// Requires Docker with the local image python:3.13-slim (never pulled);
// without it the scenario reports SKIPPED unless EXPOSITION_REQUIRE_DOCKER=1.
//
// Sections:
//   A. R-m8-F1: the approval binds the restricted image only by its tag. The
//      tag is re-pointed to a different local image after the plan was
//      approved; starting the approved plan must be refused as stale (or the
//      run refused), never executed in an image the user did not approve.
//   B. R-m8-F5: the plan review dialog of the Numerical experiment preset (its
//      code comes from the script input) must show the complete code that
//      will run; the run monitor labels the step as it actually ran.
//   C. Coordinator lead (SyncedBatchRunner::sync repoints the shared research
//      service): a run in project A, then the user switches to project B and
//      starts a research session there; workflow listing/validation in B must
//      not repoint or detach B's research, and A's run must not touch B.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const IMAGE = "python:3.13-slim";
const APPROVED = "xp-m8-review:approved";
const SWAPPED = "xp-m8-review:swapped";
const RUN_TIMEOUT = 240_000;
const OBS = "x,y\n0,1\n1,3\n2,5\n";
const MARKER_LINE = "# R-M8-SCRIPT-MARKER: this exact line must be visible in the plan review";
const EXPERIMENT = `${MARKER_LINE}
import csv, os
out = os.environ["EXPOSITION_OUTPUT_DIR"]
rows = list(csv.DictReader(open("data/obs.csv")))
with open(os.path.join(out, "figure.svg"), "w") as f:
    f.write('<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="5" height="5"/></svg>')
with open(os.path.join(out, "table.csv"), "w") as f:
    f.write("n\\n%d\\n" % len(rows))
`;

function docker(args, opts = {}) {
  return (execFileSync("docker", args, { encoding: "utf8", timeout: 120_000, ...opts }) ?? "").trim();
}

function dockerAvailable() {
  try {
    docker(["image", "inspect", IMAGE], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

function managedContainers() {
  try {
    return docker(["ps", "-aq", "--filter", "label=org.exposition.managed"]).split(/\s+/).filter(Boolean);
  } catch {
    return ["<docker ps failed>"];
  }
}

function recorder(artifactsDir) {
  const results = [];
  const log = (status, id, detail) => {
    results.push({ status, id, detail });
    const tag = status === "DEFECT" ? "DEFECT:" : status === "ok" ? "ok:" : "info:";
    console.log(`${tag} ${id}${detail ? ` — ${typeof detail === "string" ? detail : JSON.stringify(detail).slice(0, 700)}` : ""}`);
  };
  return {
    info: (id, detail = "") => log("info", id, detail),
    expect(id, ok, detail = "") {
      log(ok ? "ok" : "DEFECT", id, detail);
      return ok;
    },
    async finish() {
      await writeFile(join(artifactsDir, "findings.json"), JSON.stringify(results, null, 2));
      const defects = results.filter((r) => r.status === "DEFECT");
      if (defects.length) throw new Error(`${defects.length} defect(s) reproduced: ${defects.map((d) => d.id).join("; ")}`);
    },
  };
}

function pythonWorkflow(title, code, { input = true } = {}) {
  return {
    schema: "exposition.workflow/1",
    title,
    description: "",
    preset: null,
    inputs: input ? [{ id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }] : [],
    steps: [{ id: "run", label: "Run the code", operation: { id: "research.pythonBatch", version: 1 }, parameters: { timeoutSeconds: { kind: "integer", value: 600 } }, payload: { kind: "code", language: "python", source: code } }],
    connections: input ? [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "run", port: "data" } }] : [],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "run", port: "dataset" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

async function makeProject(base, name) {
  const root = join(base, name);
  await mkdir(join(root, "data"), { recursive: true });
  await writeFile(join(root, "data", "obs.csv"), OBS);
  await writeFile(join(root, "experiment.py"), EXPERIMENT);
  await writeFile(join(root, "paper.typ"), "= Results\n");
  return root;
}

export default async function reviewM8Runs({ launchApp, artifactsDir }) {
  if (!dockerAvailable()) {
    const message = `SKIPPED: Docker with the local image ${IMAGE} is not available`;
    if (process.env.EXPOSITION_REQUIRE_DOCKER === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const r = recorder(artifactsDir);
  const base = await mkdtemp(join(tmpdir(), "xp-review-m8-runs-"));
  const rootA = await makeProject(base, "projectA");
  const rootB = await makeProject(base, "projectB");
  const dataDir = join(base, "data");
  await mkdir(dataDir, { recursive: true });

  const app = await launchApp({ dataDir });
  const page = app.page;
  const finish = async (runId, timeout = RUN_TIMEOUT) => {
    let run;
    await waitFor(async () => {
      run = await invoke(page, "workflow_run_get", { run: runId });
      return run.state !== "running";
    }, { timeout, interval: 500, message: `run ${runId} finishes` });
    return run;
  };
  const save = (definition) => invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition } });
  const attempt = (run, step = "run") => run.steps.find((s) => s.step === step)?.attempts.at(-1);
  const fileText = async (root, rel) => readFile(join(root, ...rel.split("/")), "utf8").catch(() => "");

  try {
    await page.setViewportSize({ width: 1600, height: 1000 }).catch(() => {});
    await invoke(page, "workspace_open", { path: rootA });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });

    // ---- A. image re-tagged after approval ---------------------------------
    try {
      docker(["rm", "-f", "xp-m8-review-mk"], { stdio: "ignore" });
    } catch {
      // the helper container may not exist
    }
    try {
      docker(["tag", IMAGE, APPROVED]);
      const markerDir = await mkdtemp(join(tmpdir(), "xp-review-m8-marker-"));
      await writeFile(join(markerDir, "marker.txt"), "swapped image\n");
      docker(["create", "--name", "xp-m8-review-mk", IMAGE, "true"]);
      docker(["cp", join(markerDir, "marker.txt"), "xp-m8-review-mk:/swapped-marker"]);
      docker(["commit", "xp-m8-review-mk", SWAPPED]);
      docker(["rm", "xp-m8-review-mk"]);
      const approvedId = docker(["image", "inspect", "--format", "{{.Id}}", APPROVED]);
      const swappedId = docker(["image", "inspect", "--format", "{{.Id}}", SWAPPED]);
      r.info("A images", { approvedId, swappedId });

      const set = await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: APPROVED } });
      r.info("A configured image", set.settings.docker.image);
      const probe = `import os
open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("swapped_marker\\n%s\\n" % os.path.exists("/swapped-marker"))
`;
      const savedA = await save(pythonWorkflow("Image binding", probe));
      const planA = await invoke(page, "workflow_run_plan", { id: savedA.id, revision: null });
      r.expect("A plan has no blockers and names the approved image", planA.blockers.length === 0 && planA.execution.restrictedImage === APPROVED, { blockers: planA.blockers, execution: planA.execution });
      // The user approved planA. Now the tag points at another image.
      docker(["tag", SWAPPED, APPROVED]);
      r.info("A tag re-pointed", docker(["image", "inspect", "--format", "{{.Id}}", APPROVED]));
      await page.waitForTimeout(6000); // past the 5 s availability cache
      const started = await invokeError(page, "workflow_run_start", { request: { plan: planA.plan, approvalHash: planA.approvalHash } });
      if (started.error) {
        r.expect("R-m8-F1 A starting after the image changed is refused", true, started.error);
      } else {
        const runA = await finish(started.unexpectedSuccess.id);
        const out = attempt(runA)?.outputs.find((o) => o.port === "dataset")?.value;
        const content = out?.path ? await fileText(rootA, out.path) : "";
        const rr = out?.researchRun;
        const manifest = rr ? await invoke(page, "research_run", { runId: rr }).catch((e) => String(e)) : null;
        const recordedImage = JSON.stringify(manifest?.execution ?? manifest).slice(0, 400);
        r.expect(
          "R-m8-F1 A a plan approved for one image must not run in another image under the same tag",
          !(runA.state === "succeeded" && /True/.test(content)),
          { state: runA.state, output: content.trim(), recorded: recordedImage, approvedImageId: approvedId, ranImageId: swappedId },
        );
      }
    } catch (e) {
      r.expect("A image binding section ran", false, String(e));
    } finally {
      for (const tag of [APPROVED, SWAPPED]) {
        try {
          docker(["rmi", "-f", tag], { stdio: "ignore" });
        } catch {
          // already gone
        }
      }
      await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } }).catch(() => {});
    }

    // ---- B. plan review shows the code that runs; monitor labels -----------
    try {
      await invoke(page, "execution_settings_update", { change: { kind: "dockerImage", image: IMAGE } });
      await invoke(page, "research_space_enable").catch(() => {});
      // The user's own research session in the same project (the batch runner
      // re-points the shared research service before each batch).
      let userSession = null;
      try {
        const current = await invoke(page, "workspace_current");
        await invoke(page, "trust_grant", { root: current.root });
        const report = await invoke(page, "research_environments", { refresh: false });
        const python = report.runtimes.find((x) => x.language === "python" && x.availability.kind === "available");
        userSession = await invoke(page, "research_session_start", { request: { environmentId: python.id } });
        await waitFor(async () => (await invoke(page, "research_sessions")).some((s) => s.id === userSession.id && s.state?.kind === "idle"), { timeout: 90_000, message: "A session idle" });
      } catch (e) {
        r.info("B user research session in A could not start", String(e).slice(0, 300));
      }
      const preset = await invoke(page, "workflow_from_preset", {
        request: { preset: "numericalExperiment", script: "experiment.py", data: "data/obs.csv", manuscript: null, leanFile: null },
      });
      const savedB = await save(preset);
      const planB = await invoke(page, "workflow_run_plan", { id: savedB.id, revision: null });
      const runStep = planB.steps.find((s) => s.containment === "restrictedContainer");
      r.info("B preset plan", { steps: planB.steps.map((s) => [s.step, s.containment, s.payload?.kind ?? null, s.bindings.map((b) => b.port)]), blockers: planB.blockers });
      await runCommand(page, "Show Workflows", /Show Workflows/);
      await page.getByTestId("workflows-view").waitFor({ timeout: 15_000 });
      await page.getByTestId(`workflow-open-${savedB.id}`).click();
      const review = page.getByTestId("workflow-review-run");
      await review.waitFor({ timeout: 15_000 });
      await waitFor(async () => await review.isEnabled(), { timeout: 30_000, message: "Review and run enabled" });
      await review.click();
      const dialog = page.getByTestId("workflow-plan-review");
      await dialog.waitFor({ timeout: 30_000 });
      await page.getByTestId("workflow-plan-details").waitFor({ timeout: 30_000 });
      const dialogText = await dialog.innerText();
      await page.screenshot({ path: join(artifactsDir, "plan-review-numerical.png") }).catch(() => {});
      r.expect(
        "R-m8-F5 B the plan review shows the complete code the restricted step runs (script input)",
        dialogText.includes(MARKER_LINE),
        { scriptShownAs: (dialogText.match(/.*experiment\.py.*/g) ?? []).slice(0, 4), payload: runStep?.payload ?? null },
      );
      r.expect("B the restricted step is labelled Restricted container", /Restricted container/.test(dialogText) && !/sandboxed/i.test(dialogText), "");
      const start = page.getByTestId("workflow-start-run");
      if (await start.isEnabled()) {
        await start.click();
        const monitor = page.getByTestId("workflow-run").first();
        await monitor.waitFor({ timeout: 30_000 });
        await waitFor(async () => ["succeeded", "failed", "canceled", "interrupted"].includes(await monitor.getAttribute("data-state")), { timeout: RUN_TIMEOUT, interval: 500, message: "run monitor ends" });
        const state = await monitor.getAttribute("data-state");
        const stepText = await page.getByTestId(`workflow-run-step-${runStep?.step ?? "run"}`).first().innerText().catch(() => "");
        await page.screenshot({ path: join(artifactsDir, "run-monitor-numerical.png") }).catch(() => {});
        r.expect("B run monitor: run succeeded and the step says Restricted container", state === "succeeded" && /Restricted container/.test(stepText), { state, stepText: stepText.slice(0, 300) });
        if (userSession) {
          const sessions = await invoke(page, "research_sessions");
          const mine = sessions.find((s) => s.id === userSession.id);
          r.expect("B the user's research session in the same project survives the workflow batch", mine?.state?.kind === "idle", mine?.state ?? sessions.map((s) => [s.id, s.state]));
          await invoke(page, "research_session_shutdown", { sessionId: userSession.id }).catch(() => {});
        }
      } else {
        r.expect("B Start run enabled", false, await page.getByTestId("plan-start-reason").innerText().catch(() => ""));
      }
      await page.keyboard.press("Escape").catch(() => {});
    } catch (e) {
      r.expect("B UI section ran", false, String(e).slice(0, 500));
    }

    // ---- C. project switch while a run is active (sync lead) --------------
    try {
      const longRun = await save(pythonWorkflow("Long step", "import time\ntime.sleep(300)\n", { input: false }));
      const planLong = await invoke(page, "workflow_run_plan", { id: longRun.id, revision: null });
      const spare = await invoke(page, "workflow_run_plan", { id: longRun.id, revision: null });
      const run = await invoke(page, "workflow_run_start", { request: { plan: planLong.plan, approvalHash: planLong.approvalHash } });
      await waitFor(async () => attempt(await invoke(page, "workflow_run_get", { run: run.id }))?.state === "dispatched", { timeout: 60_000, message: "dispatched" });
      await waitFor(() => managedContainers().length > 0, { timeout: 60_000, message: "container started" });
      // Switch to B.
      const infoB = await invoke(page, "workspace_open", { path: rootB });
      await invoke(page, "trust_grant", { root: infoB.root });
      await waitFor(() => managedContainers().length === 0, { timeout: 30_000, message: "A's container stopped" }).catch(() => {});
      r.expect("C switching projects stops A's container", managedContainers().length === 0, managedContainers());
      const ws1 = await invoke(page, "research_workspace");
      let session = null;
      try {
        const report = await invoke(page, "research_environments", { refresh: false });
        const python = report.runtimes.find((x) => x.language === "python" && x.availability.kind === "available");
        session = await invoke(page, "research_session_start", { request: { environmentId: python.id } });
        await waitFor(async () => (await invoke(page, "research_sessions")).some((s) => s.id === session.id && ["idle", "busy"].includes(s.state?.kind ?? s.state)), { timeout: 90_000, message: "B session idle" });
      } catch (e) {
        r.info("C B research session could not start (kernel)", String(e).slice(0, 300));
      }
      for (let i = 0; i < 3; i++) {
        await invoke(page, "workflow_list");
        await invoke(page, "workflow_operations");
        await invoke(page, "workflow_validate", { definition: pythonWorkflow("probe", "pass\n") });
        await page.waitForTimeout(2000);
      }
      const ws2 = await invoke(page, "research_workspace");
      const sessions = await invoke(page, "research_sessions");
      r.expect(
        "C workflow listing/validation in B keeps research on B and B's session alive",
        ws1?.root === ws2?.root && /projectB/i.test(ws2?.root ?? "") && (!session || sessions.some((s) => s.id === session.id && !/detached|stopped|exited/i.test(JSON.stringify(s.state)))),
        { before: ws1?.root, after: ws2?.root, sessions: sessions.map((s) => [s.id, s.state]) },
      );
      const stale = await invokeError(page, "workflow_run_start", { request: { plan: spare.plan, approvalHash: spare.approvalHash } });
      r.expect("C A's plan cannot start while B is open", !!stale.error, stale.error ?? stale.unexpectedSuccess?.id);
      const bMeta = await readdir(join(rootB, ".exposition", "runs")).catch(() => []);
      const bArtifacts = await readdir(join(rootB, ".exposition", "artifacts")).catch(() => []);
      r.expect("C A's run wrote nothing into B", bMeta.length === 0 && bArtifacts.length === 0, { bMeta, bArtifacts });
      if (session) await invoke(page, "research_session_shutdown", { sessionId: session.id }).catch(() => {});
      await invoke(page, "workspace_open", { path: rootA });
      const runA = await invoke(page, "workflow_run_get", { run: run.id });
      r.expect("C A's run ended canceled (project closed), nothing resumed", runA.state === "canceled" && runA.code === "workflow.projectClosed" && runA.steps[0].attempts.length === 1, { state: runA.state, code: runA.code, attempts: runA.steps[0].attempts.map((a) => a.state) });
    } catch (e) {
      r.expect("C project switch section ran", false, String(e).slice(0, 500));
    }

    // ---- D. trusted native step: labels, then trust revoked mid-step -------
    try {
      const current = await invoke(page, "workspace_current");
      await invoke(page, "trust_grant", { root: current.root });
      const nativeDef = pythonWorkflow("Native step", 'import os, time\nopen(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("pid\\n%d\\n" % os.getpid())\nprint("started", flush=True)\ntime.sleep(120)\n', { input: false });
      nativeDef.steps[0].operation.id = "research.nativePython";
      nativeDef.ceiling.execution = "trustedNative";
      const savedN = await save(nativeDef);
      const planN = await invoke(page, "workflow_run_plan", { id: savedN.id, revision: null });
      r.expect(
        "D native plan names the native step and its privileges, binds no image",
        planN.blockers.length === 0 && planN.execution.nativeSteps.includes("run") && planN.steps[0].containment === "nativeAccount" && planN.notices.some((n) => /not a sandbox/i.test(n)) && planN.execution.restrictedImage === null,
        { blockers: planN.blockers, execution: planN.execution },
      );
      const runN = await invoke(page, "workflow_run_start", { request: { plan: planN.plan, approvalHash: planN.approvalHash } });
      await waitFor(async () => attempt(await invoke(page, "workflow_run_get", { run: runN.id }))?.state === "dispatched", { timeout: 60_000, message: "native dispatched" });
      await page.waitForTimeout(1500);
      const t0 = Date.now();
      await invoke(page, "trust_revoke", { root: current.root });
      const endN = await finish(runN.id, 60_000);
      const aN = attempt(endN);
      const rrN = aN?.operation?.id;
      const manifestN = rrN ? await invoke(page, "research_run", { runId: rrN }).catch((e) => ({ error: String(e) })) : null;
      r.expect(
        "D revoking trust cancels the native run and its process; the record says trusted native",
        endN.state === "canceled" && endN.code === "workflow.untrustedFolder" && Date.now() - t0 < 30_000 && manifestN?.execution?.kind === "trustedNative",
        { state: endN.state, code: endN.code, attempt: aN?.state, ms: Date.now() - t0, execution: manifestN?.execution ?? manifestN },
      );
      const pidText = rrN ? await fileText(current.root, `.exposition/artifacts/${rrN}/dataset.csv`) : "";
      const pid = Number(pidText.split(/\r?\n/)[1]);
      if (pid > 0) {
        await page.waitForTimeout(1000);
        const listing = execFileSync("tasklist", ["/FI", `PID eq ${pid}`, "/NH"], { encoding: "utf8" });
        r.expect("D the native batch process is gone after the revocation", !listing.includes(String(pid)), listing.trim().slice(0, 200));
      } else {
        r.info("D native batch pid not recorded (output of a canceled batch not registered)", pidText.slice(0, 100));
      }
      const planAfter = await invoke(page, "workflow_run_plan", { id: savedN.id, revision: null });
      r.expect("D untrusted folder blocks the native plan (no fallback to restricted)", planAfter.blockers.some((b) => b.code === "workflow.untrustedFolder") && planAfter.steps[0].containment === "nativeAccount", planAfter.blockers.map((b) => b.code));
    } catch (e) {
      r.expect("D native section ran", false, String(e).slice(0, 500));
    }

    // ---- E. hostile IPC arguments -------------------------------------------
    try {
      const calls = [
        ["workflow_run_get", { run: "wrun-../../etc" }],
        ["workflow_run_get", { run: "x".repeat(100_000) }],
        ["workflow_run_get", { run: 42 }],
        ["workflow_run_cancel", { run: "wrun-00000000000000000000000000000000" }],
        ["workflow_run_acknowledge", { run: "wplan-00000000000000000000000000000000" }],
        ["workflow_run_start", { request: { plan: "wplan-00000000000000000000000000000000", approvalHash: "0".repeat(64) } }],
        ["workflow_run_start", { request: { plan: "wplan-00000000000000000000000000000000", approvalHash: "0".repeat(64), trusted: true } }],
        ["workflow_run_start", { request: { plan: "wrun-00000000000000000000000000000000", approvalHash: "" } }],
        ["workflow_association_decide", { decision: { run: "wrun-00000000000000000000000000000000", step: "s".repeat(50_000), accept: Array.from({ length: 5000 }, (_, i) => `d${i}`) } }],
        ["workflow_association_decide", { decision: { run: "wrun-00000000000000000000000000000000", step: "associate", accept: [], extra: 1 } }],
        ["workflow_run_plan", { id: "wf-../../x", revision: null }],
        ["workflow_run_plan", { id: "wf-00000000000000000000000000000000", revision: -1 }],
        ["workflow_runs", { workflow: { kind: "x" } }],
      ];
      const outcomes = [];
      for (const [cmd, args] of calls) {
        const res = await invokeError(page, cmd, args);
        outcomes.push([cmd, res.error ? (res.error.code ?? String(res.error).slice(0, 80)) : "SUCCEEDED"]);
      }
      r.expect("E hostile run-command arguments are all refused", outcomes.every(([, o]) => o !== "SUCCEEDED"), outcomes);
      // Two approved plans started at the same moment: the active-run cap admits one.
      const sleeper = await save(pythonWorkflow("Concurrent", "import time\ntime.sleep(20)\n", { input: false }));
      const p1 = await invoke(page, "workflow_run_plan", { id: sleeper.id, revision: null });
      const p2 = await invoke(page, "workflow_run_plan", { id: sleeper.id, revision: null });
      const [s1, s2] = await Promise.all([p1, p2].map((p) => invokeError(page, "workflow_run_start", { request: { plan: p.plan, approvalHash: p.approvalHash } })));
      const startedIds = [s1, s2].filter((s) => s.unexpectedSuccess).map((s) => s.unexpectedSuccess.id);
      r.expect("E two simultaneous starts: exactly one run is admitted", startedIds.length === 1, [s1.error?.code ?? "started", s2.error?.code ?? "started"]);
      for (const id of startedIds) {
        await invoke(page, "workflow_run_cancel", { run: id });
        await finish(id, 60_000);
      }
      // A run of another project is invisible from here.
      const info = await invoke(page, "workspace_current");
      const runs = await invoke(page, "workflow_runs", { workflow: null });
      await invoke(page, "workspace_open", { path: rootB });
      const fromB = await invokeError(page, "workflow_run_get", { run: runs[0]?.id });
      const decideB = await invokeError(page, "workflow_association_decide", { decision: { run: runs[0]?.id, step: "run", accept: [] } });
      const ackB = await invokeError(page, "workflow_run_acknowledge", { run: runs[0]?.id });
      r.expect("E another project's run cannot be read, decided or acknowledged", !!fromB.error && !!decideB.error && !!ackB.error, [fromB.error?.code, decideB.error?.code, ackB.error?.code]);
      await invoke(page, "workspace_open", { path: info.root });
    } catch (e) {
      r.expect("E IPC section ran", false, String(e).slice(0, 500));
    }
  } finally {
    await page.screenshot({ path: join(artifactsDir, "final.png") }).catch(() => {});
    await app.close();
    if (existsSync(base)) r.info("project folders", base);
  }
  await r.finish();
}
