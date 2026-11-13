// R-m9 (independent review of M9) against the built app: real IPC, real
// workspace watcher, real trigger engine, real Lean server (v4.35.0).
//
// A. A trigger may watch a Lean source (FormalSource) input. Lean elaboration
//    runs `#eval` IO natively, so a saved change to the watched file runs
//    arbitrary code with the user's privileges, unattended, although the
//    standing plan never showed that code (R-m9 F2: DEFECT when the marker
//    file the edited Lean source writes appears outside the project).
// B. The run-manifest export takes a destination path from the webview and
//    writes new .json files into the app data folder and the project's
//    `.exposition` records, which the bundle export refuses (R-m9 F3).
//    The exported manifest is checked for absolute user paths (held).
// C. Re-approving a trigger opens the standing-plan dialog with "allow native
//    steps" already ticked from the earlier approval (R-m9 F4).
// D. The plan binds the saved Lean file while the Lean service checks the
//    document buffer. Held at 8fd980b: a manual or triggered run over an
//    unsaved buffer is refused (workflow.inputChanged) and does not execute
//    the buffer's code again (DEFECT if it does). Control (pre-existing, not
//    M9): the live Lean server elaborates an unsaved buffer edit by itself,
//    so buffer code runs natively before any workflow is involved.
//
// Product defects are reported as `DEFECT` lines (the scenario still passes);
// held checks fail the scenario. Requires Lean through elan with
// leanprover/lean4:v4.35.0; without it the scenario reports SKIPPED unless
// EXPOSITION_REQUIRE_LEAN=1.
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, waitFor } from "../workspace-helpers.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const WATCHED = "LeanBasic/Watched.lean";
const CLEAN = "namespace Watched\n\ntheorem two : 2 = 2 := rfl\n\nend Watched\n";

function workflow() {
  return {
    schema: "exposition.workflow/1",
    title: "Audit on save",
    description: "",
    preset: null,
    inputs: [
      { id: "proof", label: "Proof file", portType: "formalSource", source: { kind: "resource", path: WATCHED, version: { kind: "atRunStart" } } },
    ],
    steps: [{ id: "audit", label: "Audit the proof", operation: { id: "lean.audit", version: 1 }, parameters: {}, payload: null }],
    connections: [{ kind: "data", from: { kind: "input", input: "proof" }, to: { step: "audit", port: "source" } }],
    outcomes: [{ id: "evidence", kind: "auditEvidence", label: "Audit evidence", from: { step: "audit", port: "evidence" }, optional: false }],
    ceiling: { execution: "trustedNative", modelRequests: false, retainResults: true, proposeChanges: false },
  };
}

export default async function reviewM9({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-review-m9-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("lean-basic"), root, {
    recursive: true,
    filter: (src) => !/[\\/]\.lake([\\/]|$)/.test(src) && !src.endsWith("lake-manifest.json"),
  });
  await writeFile(join(root, ...WATCHED.split("/")), CLEAN);
  await mkdir(dataDir, { recursive: true });
  // Outside the project: what the edited Lean source writes when it is elaborated.
  const marker = join(base, "outside", "written-by-watched-lean.txt");
  await mkdir(join(base, "outside"), { recursive: true });

  const observations = [];
  const note = (id, ok, detail, defect = false) => {
    observations.push({ id, ok: !!ok, defect, detail });
    const tag = ok ? "ok    " : defect ? "DEFECT" : "FAIL  ";
    console.log(`${tag} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 700)}`);
  };
  const app = await launchApp({ dataDir });
  const { page } = app;
  const call = (command, args) => invoke(page, command, args);
  const finish = async (runId, timeout = 600_000) => {
    let run;
    await waitFor(async () => {
      run = await call("workflow_run_get", { run: runId });
      return run.state !== "running";
    }, { timeout, interval: 500, message: `run ${runId} finishes` });
    return run;
  };
  const saveThroughApp = async (path, text) => {
    const opened = await call("document_open", { path });
    const doc = opened.state.info.id;
    const state = await call("document_state", { document: doc });
    await call("document_apply", {
      request: {
        id: `review-m9-${Date.now()}-${Math.random()}`,
        origin: { kind: "source" },
        edits: [{ document: doc, baseRevision: state.info.revision, patches: [{ range: { start: 0, end: state.text.length }, text }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await call("document_save", { document: doc, overwrite: false });
  };

  let saved = null;
  let trigger = null;
  let firedRun = null;
  try {
    await page.setViewportSize({ width: 1600, height: 1000 }).catch(() => {});
    const info = await call("workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="lakefile.toml"]').waitFor({ timeout: 15_000 });
    await call("trust_grant", { root: info.root });
    saved = await call("workflow_save", { request: { id: null, expectedRevision: null, definition: workflow() } });
    assert.equal(saved.validation.state, "wellFormed", JSON.stringify(saved.validation.diagnostics));

    // ---- A. a watched Lean source runs its code natively, unreviewed ----------------
    const review = await call("workflow_trigger_plan", { request: { workflow: saved.id, kind: { kind: "inputsChanged", inputs: ["proof"] } } });
    const leanInput = review.plan.inputs.find((i) => i.id === "proof");
    note("A-standing-plan-accepts-a-watched-lean-source", review.blockers.length === 0, { mayChange: review.mayChange, nativeSteps: review.nativeSteps });
    const warns = [...review.mayChange, ...review.notices].some((t) => /#eval|executes? (its|the) (code|source)|arbitrary|metaprogram/i.test(t));
    note(
      "A-standing-plan-says-the-watched-lean-file-is-executable-code",
      warns,
      { mayChange: review.mayChange, notices: review.notices, leanTextShown: leanInput?.text !== null && leanInput?.text !== undefined },
      true,
    );
    trigger = await call("workflow_trigger_enable", {
      request: {
        plan: review.plan.plan,
        standingHash: review.standingHash,
        settings: { debounceSeconds: 10, maxRunsPerHour: 12, maxRunsPerDay: 100, allowNative: true, maxModelRequestsPerDay: 0 },
      },
    });
    await waitFor(async () => (await call("workflow_trigger_list", { workflow: saved.id }))[0]?.listening, {
      timeout: 60_000,
      message: "the trigger listens (project indexed)",
    });
    note("A-trigger-enabled-and-listening", true, { id: trigger.id });
    const markerPath = marker.replace(/\\/g, "/");
    const smuggled = `${CLEAN}\n#eval IO.FS.writeFile "${markerPath}" "written by a watched Lean file, never reviewed"\n`;
    // Saved outside the editor (an external editor, a sync tool, git pull): the
    // file is not open in the app, so the live Lean server never sees it.
    await writeFile(join(root, ...WATCHED.split("/")), smuggled);
    await page.waitForTimeout(5000);
    note("A-control-nothing-elaborated-the-change-before-the-trigger-fired", !existsSync(marker), { markerBeforeFire: existsSync(marker) });
    assert.ok(!existsSync(marker), "the marker appeared before the trigger fired; the attribution would be wrong");
    const runs = await waitFor(async () => {
      const list = await call("workflow_runs", { workflow: saved.id });
      return list.length > 0 && list;
    }, { timeout: 90_000, interval: 250, message: "the triggered run" });
    firedRun = await finish(runs[0].id);
    const planInput = firedRun.plan.inputs.find((i) => i.id === "proof");
    note("A-triggered-run-finished", firedRun.state === "succeeded", {
      state: firedRun.state,
      code: firedRun.code,
      cause: firedRun.cause?.kind,
      leanTextInPlan: planInput?.text ?? null,
      verdict: firedRun.steps[0]?.attempts.at(-1)?.outputs[0]?.value?.verdict,
    });
    const written = existsSync(marker) ? await readFile(marker, "utf8") : null;
    note(
      "A-a-saved-change-to-a-watched-lean-file-ran-no-unreviewed-code",
      written === null,
      written === null
        ? "no marker"
        : `the edited Lean source wrote ${marker} (outside the project) during the unattended triggered audit: "${written}"`,
      true,
    );
    await call("workflow_trigger_disable", { trigger: trigger.id });

    // ---- B. manifest export as a webview write primitive -----------------------------
    const planted = [
      ["app-data", join(dataDir, "review-m9-planted.json")],
      ["project-records", join(root, ".exposition", "review-m9-planted.json")],
    ];
    for (const [where, path] of planted) {
      const outcome = await invokeError(page, "workflow_run_manifest_export", { run: firedRun.id, destination: path });
      const wrote = existsSync(path);
      note(`B-manifest-export-refuses-${where}`, !wrote, { destination: path, outcome: outcome.error ?? "written" }, true);
      if (wrote) await rm(path, { force: true });
    }
    const bundleRefusal = await invokeError(page, "workflow_bundle_export", {
      id: saved.id,
      revision: null,
      path: join(root, ".exposition", "review-m9-bundle.json"),
    });
    note("B-control-bundle-export-refuses-project-records", bundleRefusal.error?.code === "workflow.file", bundleRefusal.error);
    assert.equal(bundleRefusal.error?.code, "workflow.file");
    const manifest = await call("workflow_run_manifest", { run: firedRun.id });
    const lower = manifest.toLowerCase();
    const leaks = [base, homedir(), dataDir, root]
      .flatMap((p) => [p, p.replace(/\\/g, "/")])
      .filter((p) => lower.includes(p.toLowerCase()));
    note("B-manifest-has-no-absolute-user-paths", leaks.length === 0, { leaks, bytes: manifest.length });
    assert.equal(leaks.length, 0, `manifest leaks ${leaks.join(", ")}`);
    const exportTarget = join(base, "outside", "manifest.json");
    const exported = await call("workflow_run_manifest_export", { run: firedRun.id, destination: exportTarget });
    assert.equal(await readFile(exportTarget, "utf8"), manifest, "the export writes exactly the manifest");
    note("B-manifest-export-to-a-chosen-file", true, exported);

    // ---- D. the approval binds the saved file, Lean elaborates the open buffer --------
    // The plan (and a trigger's standing plan) binds the SHA-256 of the saved
    // Lean file; the Lean audit checks the document service's buffer. A run
    // over an unsaved buffer must be refused without executing the buffer's
    // code on the plan's behalf (the live server's own elaboration is the
    // control, removed before each run).
    const BUFFERED = "LeanBasic/Buffered.lean";
    await writeFile(join(root, ...BUFFERED.split("/")), CLEAN.replace(/Watched/g, "Buffered"));
    await mkdir(join(root, "data"), { recursive: true });
    await writeFile(join(root, "data", "notes.csv"), "note\nfirst\n");
    const withNotes = workflow();
    withNotes.title = "Audit with notes";
    withNotes.inputs = [
      { id: "proof", label: "Proof file", portType: "formalSource", source: { kind: "resource", path: BUFFERED, version: { kind: "atRunStart" } } },
      { id: "notes", label: "Notes", portType: "dataset", source: { kind: "resource", path: "data/notes.csv", version: { kind: "atRunStart" } } },
    ];
    const savedD = await call("workflow_save", { request: { id: null, expectedRevision: null, definition: withNotes } });
    note("D-workflow-well-formed", savedD.validation.state === "wellFormed", savedD.validation.diagnostics.map((d) => d.code));
    const editBuffer = async (target) => {
      const opened = await call("document_open", { path: BUFFERED });
      const doc = opened.state.info.id;
      const state = await call("document_state", { document: doc });
      const text = `${CLEAN.replace(/Watched/g, "Buffered")}\n#eval IO.FS.writeFile "${target.replace(/\\/g, "/")}" "unsaved buffer code"\n`;
      await call("document_apply", {
        request: {
          id: `review-m9-buffer-${Date.now()}`,
          origin: { kind: "source" },
          edits: [{ document: doc, baseRevision: state.info.revision, patches: [{ range: { start: 0, end: state.text.length }, text }] }],
          undo: { kind: "newGroup" },
          conflict: "reject",
          label: null,
          selection: null,
        },
      });
      return doc;
    };
    // Control: does the live Lean server (open documents in a trusted folder)
    // elaborate the unsaved buffer by itself, before any workflow runs?
    const markerLive = join(base, "outside", "live-server-buffer.txt");
    await editBuffer(markerLive);
    await page.waitForTimeout(15_000);
    const liveElaborated = existsSync(markerLive);
    note("D-control-live-lean-server-elaborates-unsaved-buffers", true, { liveElaborated });
    const markerD1 = join(base, "outside", "manual-run-buffer.txt");
    await editBuffer(markerD1);
    await page.waitForTimeout(15_000);
    const liveD1 = existsSync(markerD1);
    if (liveD1) await rm(markerD1, { force: true });
    note("D-control-marker-before-the-run", true, { writtenBeforeTheRun: liveD1, removed: liveD1 });
    const diskText = await readFile(join(root, ...BUFFERED.split("/")), "utf8");
    note("D-buffer-edit-left-the-saved-file-unchanged", !diskText.includes("#eval"), diskText.length);
    const planD = await call("workflow_run_plan", { id: savedD.id, revision: null });
    note("D-plan-binds-the-saved-file", planD.blockers.length === 0 && planD.inputs.find((i) => i.id === "proof")?.text === null, planD.blockers);
    const startedD = await call("workflow_run_start", { request: { plan: planD.plan, approvalHash: planD.approvalHash } });
    const runD = await finish(startedD.id);
    const attemptD = runD.steps[0]?.attempts.at(-1);
    note(
      "D-manual-run-executes-only-the-approved-text",
      !existsSync(markerD1),
      { marker: existsSync(markerD1) ? await readFile(markerD1, "utf8") : null, run: runD.state, code: attemptD?.code, message: attemptD?.message },
      true,
    );
    const markerD2 = join(base, "outside", "triggered-run-buffer.txt");
    await editBuffer(markerD2);
    await page.waitForTimeout(15_000);
    const liveD2 = existsSync(markerD2);
    if (liveD2) await rm(markerD2, { force: true });
    note("D-control-marker-before-the-trigger", true, { writtenBeforeTheTrigger: liveD2, removed: liveD2 });
    const reviewD =await call("workflow_trigger_plan", { request: { workflow: savedD.id, kind: { kind: "inputsChanged", inputs: ["notes"] } } });
    const triggerD = await call("workflow_trigger_enable", {
      request: {
        plan: reviewD.plan.plan,
        standingHash: reviewD.standingHash,
        settings: { debounceSeconds: 2, maxRunsPerHour: 12, maxRunsPerDay: 100, allowNative: true, maxModelRequestsPerDay: 0 },
      },
    });
    await waitFor(async () => (await call("workflow_trigger_list", { workflow: savedD.id }))[0]?.listening, { timeout: 60_000, message: "trigger D listens" });
    const before = (await call("workflow_runs", { workflow: savedD.id })).length;
    await saveThroughApp("data/notes.csv", "note\nfirst\nsecond\n");
    const triggeredD = await waitFor(async () => {
      const list = await call("workflow_runs", { workflow: savedD.id });
      return list.length > before && list[0];
    }, { timeout: 60_000, interval: 250, message: "the triggered run of D" });
    const runD2 = await finish(triggeredD.id);
    note(
      "D-triggered-run-executes-only-the-standing-plan-text",
      !existsSync(markerD2),
      {
        marker: existsSync(markerD2) ? await readFile(markerD2, "utf8") : null,
        run: runD2.state,
        cause: runD2.cause?.kind,
        code: runD2.steps[0]?.attempts.at(-1)?.code,
      },
      true,
    );
    await call("workflow_trigger_disable", { trigger: triggerD.id });

    // ---- C. re-approval pre-ticks native consent ------------------------------------
    await runCommand(page, "Show Workflows", /Show Workflows/);
    await page.getByTestId("workflows-view").waitFor({ timeout: 15_000 });
    await page.getByTestId(`workflow-open-${saved.id}`).click();
    await page.locator('[data-testid="workflow-triggers"]:visible').first().scrollIntoViewIfNeeded();
    await page.getByTestId(`trigger-reapprove-${trigger.id}`).click();
    await page.getByTestId("trigger-dialog").waitFor({ timeout: 15_000 });
    await page.getByTestId("trigger-review").click();
    const native = page.getByTestId("trigger-native");
    await native.waitFor({ timeout: 60_000 });
    const preTicked = (await native.getAttribute("data-allowed")) === "true";
    await page.screenshot({ path: join(artifactsDir, "reapprove-native.png") }).catch(() => {});
    note("C-reapproval-asks-for-native-consent-anew", !preTicked, { allowNativePreTicked: preTicked, nativeSteps: await native.getAttribute("data-native-steps") }, true);
    await page.keyboard.press("Escape").catch(() => {});
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2)).catch(() => {});
    if (trigger) await invoke(page, "workflow_trigger_disable", { trigger: trigger.id }).catch(() => {});
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok && !o.defect);
  assert.deepEqual(failed, [], `held checks failed: ${JSON.stringify(failed).slice(0, 800)}`);
  console.log(`review-m9: ${observations.filter((o) => o.ok).length} ok, ${observations.filter((o) => o.defect && !o.ok).length} DEFECT`);
}
