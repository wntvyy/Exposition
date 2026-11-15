// Mathlib setup end to end through the app's own controls, with the real
// network, Lake, elan toolchain, and Mathlib cache.
//
// Opt-in (downloads Mathlib and its prebuilt cache: several GB, minutes):
//   EXPOSITION_MATHLIB_SETUP=1 npm run e2e -- mathlib-setup
// EXPOSITION_MATHLIB_SETUP_DIR=<absolute folder> chooses where the project is
// made (it must not exist or be empty); by default a temporary folder. The
// prepared project is left in place and its path is printed: run the
// `mathlib` scenario against it with
//   EXPOSITION_MATHLIB_PROJECT=<dir> EXPOSITION_MATHLIB_PACKAGE=<dir>/lean
//
// 1. Canceled setup: a new package is started from the dialog and canceled
//    while Lake resolves dependencies; everything it created is removed and
//    the project is byte for byte what it was.
// 2. Failed setup (mismatched toolchain): an existing package requires a
//    Mathlib release made for another Lean version; the setup fails with the
//    reason before any cache is fetched, removes what it downloaded, leaves
//    the configuration untouched, and the package is not ready.
// 3. New package: created and prepared from the dialog; the manifest pins the
//    Mathlib release of the pinned Lean; the starter file is Checked.
// 4. Reopen: a new app session on the same data folder needs no trust prompt,
//    no setup, and no download; the manifest is unchanged; the file is Checked.
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, openInEditor, waitFor } from "../workspace-helpers.mjs";

const seconds = (since) => Math.round((Date.now() - since) / 100) / 10;
const sha256 = async (path) => createHash("sha256").update(await readFile(path)).digest("hex");

async function tree(dir, prefix = "") {
  const out = [];
  for (const entry of (await readdir(dir, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.name === ".exposition") continue;
    if (entry.isDirectory()) out.push(`${rel}/`, ...(await tree(join(dir, entry.name), rel)));
    else out.push(`${rel}:${await sha256(join(dir, entry.name))}`);
  }
  return out;
}

export default async function mathlibSetup({ launchApp, artifactsDir, assert }) {
  if (process.env.EXPOSITION_MATHLIB_SETUP !== "1") {
    console.log("SKIPPED: set EXPOSITION_MATHLIB_SETUP=1 to run the Mathlib setup scenario (downloads several GB)");
    return;
  }
  const base = process.env.EXPOSITION_MATHLIB_SETUP_DIR || (await mkdtemp(join(tmpdir(), "xp-mathlib-setup-")));
  await mkdir(base, { recursive: true });
  assert.deepEqual(await readdir(base), [], `${base} must be empty`);
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "mismatch"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await writeFile(join(root, "paper.typ"), "= Odd sums\n\nThe sum of the first $n$ odd numbers is $n^2$.\n");
  // An existing package whose Mathlib release was made for another Lean version.
  const mismatchLakefile = 'name = "Mismatch"\n\n[[require]]\nname = "mathlib"\nscope = "leanprover-community"\nrev = "v4.34.0"\n\n[[lean_lib]]\nname = "Mismatch"\n';
  await writeFile(join(root, "mismatch", "lakefile.toml"), mismatchLakefile);
  await writeFile(join(root, "mismatch", "lean-toolchain"), "leanprover/lean4:v4.35.0\n");
  await writeFile(join(root, "mismatch", "Mismatch.lean"), "import Mathlib.Tactic.Ring\n");
  const initial = await tree(root);

  const observations = [];
  const timings = {};
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 900)}`);
  };
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-mathlib-setup-${Date.now()}` };
  let app = await launchApp({ dataDir, env });
  let page = app.page;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) }).catch(() => {});
  const finished = async (timeout) => {
    let status;
    await waitFor(
      async () => {
        status = await invoke(page, "lean_setup_status");
        return status && status.state !== "running";
      },
      { timeout, interval: 1000, message: "the setup ends" },
    );
    return status;
  };
  let manifestDigest = null;
  try {
    await page.setViewportSize({ width: 1440, height: 1000 }).catch(() => {});
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: info.root });

    // ---- 1. canceled ---------------------------------------------------------
    await runCommand(page, "set up mathlib", /Set Up Mathlib/);
    const dialog = page.getByRole("dialog", { name: "Set up Mathlib" });
    await dialog.waitFor({ timeout: 10_000 });
    await dialog.getByRole("button", { name: /Package/ }).click();
    await page.getByRole("option", { name: "New package…", exact: true }).click();
    await dialog.getByRole("textbox", { name: "Folder" }).fill("lean");
    await dialog.getByRole("textbox", { name: "Library name" }).fill("OddSums");
    const plan = page.getByTestId("lean-setup-plan");
    await waitFor(async () => (await plan.getAttribute("data-state")) === "ready", { timeout: 20_000, message: "the new-package plan" });
    await page.getByTestId("lean-setup-start").click();
    const progress = page.getByTestId("lean-setup-progress");
    await progress.waitFor({ timeout: 30_000 });
    await waitFor(async () => (await progress.getAttribute("data-phase")) === "step" && existsSync(join(root, "lean", "lakefile.toml")), {
      timeout: 60_000,
      message: "the first step runs",
    });
    await shot("mathlib-setup-progress-light");
    await page.waitForTimeout(3000);
    await page.getByTestId("lean-setup-cancel").click();
    const canceled = await finished(120_000);
    const outcome = page.getByTestId("lean-setup-outcome");
    await outcome.waitFor({ timeout: 15_000 });
    note(
      "canceled-setup-removes-what-it-created",
      canceled.state === "canceled" && !canceled.ready && !existsSync(join(root, "lean")) && /removed/.test(canceled.message ?? "") && JSON.stringify(await tree(root)) === JSON.stringify(initial),
      { state: canceled.state, message: canceled.message, removed: canceled.removed },
    );
    note("canceled-outcome-shown", (await outcome.getAttribute("data-state")) === "canceled", await outcome.innerText());
    await shot("mathlib-setup-canceled-light");
    const afterCancel = await invoke(page, "lean_package_inspect", { folder: "lean" });
    note("canceled-package-is-not-a-package", afterCancel.readiness.kind === "notAPackage", afterCancel.readiness);
    await page.getByTestId("lean-setup-close").click();
    await dialog.waitFor({ state: "detached", timeout: 10_000 });

    // ---- 2. failed: mismatched toolchain --------------------------------------
    const mismatchPlan = await invoke(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "mismatch" } });
    note(
      "existing-package-plan-resolves-once-and-keeps-the-pin",
      !mismatchPlan.pinnedByManifest && mismatchPlan.mathlibRevision === "v4.34.0" && mismatchPlan.steps[0].args.join(" ") === "--keep-toolchain update" && mismatchPlan.keeps.includes("lean-toolchain"),
      { steps: mismatchPlan.steps.map((s) => s.args.join(" ")), keeps: mismatchPlan.keeps },
    );
    let started = Date.now();
    await invoke(page, "lean_setup_start", { plan: mismatchPlan });
    const failed = await finished(1_800_000);
    timings.mismatchSeconds = seconds(started);
    note(
      "mismatched-toolchain-fails-before-the-cache-with-the-reason",
      failed.state === "failed" &&
        !failed.ready &&
        failed.step === 0 &&
        /built for Lean leanprover\/lean4:v4\.34\.0/.test(failed.message ?? "") &&
        /pins leanprover\/lean4:v4\.35\.0/.test(failed.message ?? ""),
      { state: failed.state, step: failed.step, message: failed.message, seconds: timings.mismatchSeconds },
    );
    note(
      "failed-setup-leaves-configuration-and-removes-downloads",
      (await readFile(join(root, "mismatch", "lakefile.toml"), "utf8")) === mismatchLakefile &&
        (await readFile(join(root, "mismatch", "lean-toolchain"), "utf8")) === "leanprover/lean4:v4.35.0\n" &&
        !existsSync(join(root, "mismatch", ".lake")) &&
        !existsSync(join(root, "mismatch", "lake-manifest.json")) &&
        JSON.stringify(await tree(root)) === JSON.stringify(initial),
      { removed: failed.removed },
    );
    const afterFailure = await invoke(page, "lean_package_inspect", { folder: "mismatch" });
    note("failed-package-is-not-ready", afterFailure.readiness.kind === "dependenciesUnresolved", afterFailure.readiness);

    // ---- 3. a new package, through the dialog ----------------------------------
    await runCommand(page, "set up mathlib", /Set Up Mathlib/);
    await dialog.waitFor({ timeout: 10_000 });
    await page.getByTestId("lean-setup-retry").click();
    await dialog.getByRole("button", { name: /Package/ }).click();
    await page.getByRole("option", { name: "New package…", exact: true }).click();
    await dialog.getByRole("textbox", { name: "Folder" }).fill("lean");
    await dialog.getByRole("textbox", { name: "Library name" }).fill("OddSums");
    await waitFor(async () => (await plan.getAttribute("data-state")) === "ready", { timeout: 20_000, message: "the new-package plan" });
    const planned = await invoke(page, "lean_setup_plan", { request: { kind: "newMathlibPackage", folder: "lean", name: "OddSums" } });
    started = Date.now();
    await page.getByTestId("lean-setup-start").click();
    await progress.waitFor({ timeout: 30_000 });
    // While it runs, a Lean file of the package is held back, not checked half-prepared.
    const done = await finished(3_000_000);
    timings.newPackageSeconds = seconds(started);
    await outcome.waitFor({ timeout: 15_000 });
    await shot("mathlib-setup-ready-light");
    const manifest = JSON.parse(await readFile(join(root, "lean", "lake-manifest.json"), "utf8"));
    const mathlib = manifest.packages.find((p) => p.name === "mathlib");
    manifestDigest = await sha256(join(root, "lean", "lake-manifest.json"));
    const pin = (await readFile(join(root, "lean", "lean-toolchain"), "utf8")).trim();
    note(
      "new-package-is-ready",
      done.state === "succeeded" && done.ready && done.inspection?.readiness.kind === "ready" && (await outcome.getAttribute("data-state")) === "succeeded",
      { state: done.state, message: done.message, seconds: timings.newPackageSeconds, steps: planned.steps.map((s) => s.args.join(" ")) },
    );
    note(
      "manifest-pins-the-mathlib-release-of-the-pinned-lean",
      pin === planned.toolchain &&
        mathlib?.inputRev === planned.mathlibRevision &&
        `leanprover/lean4:${mathlib.inputRev}` === pin &&
        /^[0-9a-f]{40}$/.test(mathlib.rev) &&
        done.inspection.mathlibRevision === mathlib.rev &&
        done.inspection.manifestDigest === manifestDigest &&
        (await readFile(join(root, "lean", ".lake", "packages", "mathlib", "lean-toolchain"), "utf8")).trim() === pin,
      { pin, inputRev: mathlib?.inputRev, rev: mathlib?.rev, manifestDigest },
    );
    note(
      "only-the-planned-files-were-written",
      JSON.stringify((await readdir(join(root, "lean"))).sort()) === JSON.stringify([".gitignore", ".lake", "OddSums", "OddSums.lean", "lake-manifest.json", "lakefile.toml", "lean-toolchain"].sort()) &&
        JSON.stringify((await tree(root)).filter((l) => !l.startsWith("lean/"))) === JSON.stringify(initial),
      await readdir(join(root, "lean")),
    );
    await page.getByTestId("lean-setup-close").click();
    await dialog.waitFor({ state: "detached", timeout: 10_000 });
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    started = Date.now();
    await openInEditor(page, "lean/OddSums/Basic.lean");
    const panel = page.getByTestId("lean-panel");
    await waitFor(async () => !["checking", "notStarted", "unavailable"].includes(await panel.getAttribute("data-status")), { timeout: 900_000, interval: 500, message: "the starter settles" });
    timings.starterCheckSeconds = seconds(started);
    note("starter-file-is-checked", (await panel.getAttribute("data-status")) === "checked", { status: await panel.getAttribute("data-status"), seconds: timings.starterCheckSeconds });
    await shot("mathlib-starter-checked-light");
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }

  // ---- 4. reopen: same data folder, no prompt, no setup, no download ------------
  if (manifestDigest) {
    app = await launchApp({ dataDir, env });
    page = app.page;
    try {
      await page.setViewportSize({ width: 1440, height: 1000 }).catch(() => {});
      const info = await invoke(page, "workspace_open", { path: root });
      await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });
      const trust = await invoke(page, "trust_status", { path: info.root });
      note("trust-is-reused-after-reopen", trust.trusted === true, trust);
      const again = await invokeError(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "lean" } });
      note("reopened-package-needs-no-setup", again.error?.code === "lean.setup.alreadyPrepared", again.error ?? again);
      await runCommand(page, "show lean infoview", /Show Lean Infoview/);
      const started = Date.now();
      await openInEditor(page, "lean/OddSums/Basic.lean");
      const panel = page.getByTestId("lean-panel");
      await waitFor(async () => !["checking", "notStarted", "unavailable"].includes(await panel.getAttribute("data-status")), { timeout: 900_000, interval: 500, message: "the starter settles after reopening" });
      timings.reopenCheckSeconds = seconds(started);
      const jobs = await invoke(page, "job_list");
      const labels = jobs.map((j) => j.info?.spec?.label ?? j.spec?.label ?? "");
      note(
        "reopen-checks-without-update-or-download",
        (await panel.getAttribute("data-status")) === "checked" &&
          (await sha256(join(root, "lean", "lake-manifest.json"))) === manifestDigest &&
          (await invoke(page, "lean_setup_status")) === null &&
          !labels.some((l) => /Mathlib setup|update|cache/.test(l)),
        { status: await panel.getAttribute("data-status"), seconds: timings.reopenCheckSeconds, jobs: labels },
      );
      note("timings", true, timings);
    } finally {
      await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
      await app.close();
    }
  }
  console.log(`Prepared project: ${root}\nPrepared package: ${join(root, "lean")}`);
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
