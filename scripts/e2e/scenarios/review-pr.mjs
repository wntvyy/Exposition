// R-pr (independent review of e6eca9d..fa6c16e) against the built app: real
// IPC from the webview, the real Lean service, the real execution service,
// and the installed Lean toolchain. No model is used.
//
// A. The Mathlib setup commands over IPC: what an unprepared package is taken
//    for, the build-from-source choice on a package that is not resolved, a
//    manifest changed between review and start, a start in an untrusted
//    folder.
// B. The setup actually run, briefly: a new package is started from IPC and
//    canceled as soon as its first command runs (`lake update`, the first
//    seconds of a clone; nothing large is downloaded). While it runs, the
//    user adds a file of their own in the new library folder. Afterwards:
//    what was removed, what stayed, whether anything keeps writing.
//
// Product defects are reported as `DEFECT` lines (the scenario still passes);
// held checks fail the scenario.
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, invokeError, openInEditor, waitFor } from "../workspace-helpers.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const REV = "56a005855ea17e0b0958f73ec801ae140b0a1679";
const manifest = (batteriesUrl, batteriesRev) =>
  JSON.stringify({
    version: "1.3.0",
    packagesDir: ".lake/packages",
    packages: [
      { url: "https://github.com/leanprover-community/mathlib4", type: "git", rev: REV, name: "mathlib", inputRev: "v4.35.0" },
      { url: batteriesUrl, type: "git", rev: batteriesRev, name: "batteries", inputRev: "main" },
    ],
  });

async function tree(dir, prefix = "") {
  const out = [];
  for (const entry of (await readdir(dir, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.name === ".exposition") continue;
    if (entry.isDirectory()) out.push(`${rel}/`, ...(await tree(join(dir, entry.name), rel)));
    else out.push(rel);
  }
  return out;
}

export default async function reviewPr({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-review-pr-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "literal"), { recursive: true });
  await mkdir(join(root, "pinned"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await cp(fixture("lean-mathlib"), join(root, "formal"), { recursive: true });
  await writeFile(join(root, "paper.typ"), "= Paper\n");
  // The same requirement as the fixture's, written with TOML literal strings.
  await writeFile(join(root, "literal", "lakefile.toml"), "name = 'Literal'\n\n[[require]]\nname = 'mathlib'\nscope = 'leanprover-community'\nrev = 'v4.35.0'\n\n[[lean_lib]]\nname = 'Literal'\n");
  await writeFile(join(root, "literal", "lean-toolchain"), "leanprover/lean4:v4.35.0\n");
  // An existing package whose manifest pins its dependencies (not downloaded).
  await writeFile(join(root, "pinned", "lakefile.toml"), 'name = "Pinned"\n\n[[require]]\nname = "mathlib"\nscope = "leanprover-community"\nrev = "v4.35.0"\n\n[[lean_lib]]\nname = "Pinned"\n');
  await writeFile(join(root, "pinned", "lean-toolchain"), "leanprover/lean4:v4.35.0\n");
  await writeFile(join(root, "pinned", "lake-manifest.json"), manifest("https://github.com/leanprover-community/batteries", "66e469188fe07963e5cf666c7d4433f45b8131c5"));

  const observations = [];
  const note = (id, ok, detail, defect = false) => {
    observations.push({ id, ok: !!ok, defect, detail });
    const tag = ok ? "ok    " : defect ? "DEFECT" : "FAIL  ";
    console.log(`${tag} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 900)}`);
  };
  const app = await launchApp({ dataDir });
  const { page } = app;
  let setupId = null;
  try {
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="paper.typ"]').waitFor({ timeout: 15_000 });

    // ---- A. over IPC, nothing runs ------------------------------------------------
    const quoted = await invoke(page, "lean_package_inspect", { folder: "formal" });
    const literal = await invoke(page, "lean_package_inspect", { folder: "literal" });
    note("A-fixture-requirement-is-seen", quoted.usesMathlib && quoted.readiness.kind === "dependenciesUnresolved", quoted.readiness);
    note(
      "A-literal-string-requirement-is-seen (R-pr-F3)",
      literal.usesMathlib,
      { usesMathlib: literal.usesMathlib, readiness: literal.readiness, note: "the Lean server would start here and Lake would clone and build Mathlib" },
      true,
    );

    // Untrusted: a reviewed plan does not start; a manifest changed after the
    // review should be a stale plan (checked before trust).
    const reviewed = await invoke(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "pinned" } });
    const untrusted = await invokeError(page, "lean_setup_start", { plan: reviewed });
    note("A-untrusted-folder-starts-nothing", untrusted.error?.code === "lean.untrusted", untrusted.error ?? untrusted);
    await writeFile(join(root, "pinned", "lake-manifest.json"), manifest("https://github.com/attacker-example/batteries", "0000000000000000000000000000000000000001"));
    const again = await invoke(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "pinned" } });
    const swapped = await invokeError(page, "lean_setup_start", { plan: reviewed });
    note(
      "A-changed-manifest-is-a-stale-plan (R-pr-F4)",
      swapped.error?.code === "lean.setup.planStale",
      { start: swapped.error?.code ?? swapped, samePlanHash: again.planHash === reviewed.planHash, planNamesTheSource: JSON.stringify(again).includes("attacker-example") },
      true,
    );
    note("A-no-setup-ran", (await invoke(page, "lean_setup_status")) === null, await invoke(page, "lean_setup_status"));

    // Trusted: the build-from-source choice does not start Lake on a package
    // whose dependencies are not resolved.
    await invoke(page, "trust_grant", { root: info.root });
    const doc = await openInEditor(page, "formal/OddSums/Basic.lean");
    const state = async () => (await invoke(page, "lean_documents")).find((d) => d.document === doc);
    await waitFor(async () => (await state())?.status.kind === "unavailable", { timeout: 60_000, interval: 250, message: "the unprepared package is not started" });
    await invoke(page, "lean_allow_local_build", { document: doc });
    await new Promise((r) => setTimeout(r, 4000));
    note(
      "A-build-from-source-does-not-lift-unresolved-dependencies",
      (await state())?.status.kind === "unavailable" && !existsSync(join(root, "formal", ".lake")) && !existsSync(join(root, "formal", "lake-manifest.json")),
      { status: (await state())?.status, lake: existsSync(join(root, "formal", ".lake")) },
    );
    const bogus = await invokeError(page, "lean_allow_local_build", { document: "doc-does-not-exist" });
    note("A-build-from-source-needs-a-lean-document", !!bogus.error, bogus.error ?? bogus);
    note("A-cancel-of-an-unknown-setup-is-false", (await invoke(page, "lean_setup_cancel", { setupId: "0".repeat(32) })) === false, "");

    // ---- B. a real setup, canceled as soon as its first command runs ---------------
    const before = await tree(root);
    const plan = await invoke(page, "lean_setup_plan", { request: { kind: "newMathlibPackage", folder: "fresh", name: "Formal" } });
    note("B-plan-creates-in-the-named-folder", plan.packageRoot.replace(/\\/g, "/").endsWith("/project/fresh") && plan.createsPackage && plan.steps[0].args.join(" ") === "--keep-toolchain update", { packageRoot: plan.packageRoot, steps: plan.steps.map((s) => s.args.join(" ")) });
    const started = await invoke(page, "lean_setup_start", { plan });
    setupId = started.id;
    await waitFor(async () => (await invoke(page, "lean_setup_status"))?.phase === "step", { timeout: 30_000, interval: 50, message: "the first command runs" });
    // The user's own file, added to the new library folder while the setup runs.
    const mine = join(root, "fresh", "Formal", "Mine.lean");
    await writeFile(mine, "theorem mine : 1 + 1 = 2 := rfl\n");
    const canceled = await invoke(page, "lean_setup_cancel", { setupId });
    let status;
    await waitFor(
      async () => {
        status = await invoke(page, "lean_setup_status");
        return status?.state !== "running";
      },
      { timeout: 120_000, interval: 250, message: "the canceled setup ends" },
    );
    setupId = null;
    note("B-cancel-ends-the-setup-as-canceled", canceled === true && status.state === "canceled" && status.ready === false, { state: status.state, message: status.message, removed: status.removed });
    const mineKept = existsSync(mine);
    note(
      "B-the-users-own-file-survives-cleanup (R-pr-F5)",
      mineKept,
      { kept: mineKept, message: status.message, removed: status.removed },
      true,
    );
    // Nothing keeps writing after cleanup, and nothing else changed.
    const after1 = await tree(root);
    await new Promise((r) => setTimeout(r, 5000));
    const after2 = await tree(root);
    note("B-nothing-writes-after-cleanup", JSON.stringify(after1) === JSON.stringify(after2), { extra: after2.filter((p) => !after1.includes(p)).slice(0, 10) });
    const leftovers = after2.filter((p) => !before.includes(p) && p !== "fresh/" && p !== "fresh/Formal/" && p !== "fresh/Formal/Mine.lean");
    note("B-what-the-setup-created-is-removed", leftovers.length === 0, { leftovers: leftovers.slice(0, 20) });
    const missing = before.filter((p) => !after2.includes(p));
    note("B-nothing-else-was-removed", missing.length === 0, { missing });
    const fresh = await invoke(page, "lean_package_inspect", { folder: "fresh" });
    note("B-the-canceled-package-is-not-ready", fresh.readiness.kind !== "ready", fresh.readiness);
    const texts = [status.message ?? "", ...(status.log ?? [])].join("\n");
    note("B-status-leaks-no-home-path-of-another-user-or-key", !/sk-ant-|ANTHROPIC_API_KEY/.test(texts), "");
  } finally {
    if (setupId) await invoke(page, "lean_setup_cancel", { setupId }).catch(() => {});
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2)).catch(() => {});
    await app.close();
    await rm(base, { recursive: true, force: true }).catch(() => {});
  }
  const failed = observations.filter((o) => !o.ok && !o.defect);
  assert.deepEqual(failed.map((o) => o.id), [], `held checks failed: ${JSON.stringify(failed).slice(0, 1200)}`);
  console.log(`review-pr: ${observations.filter((o) => o.ok).length} ok, ${observations.filter((o) => o.defect && !o.ok).length} DEFECT`);
}
