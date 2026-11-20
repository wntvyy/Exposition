// The guards of the Mathlib setup against the built app: real IPC from the
// webview, the real Lean service, the real execution service, and the
// installed Lean toolchain. No model is used.
//
// A. The setup commands over IPC, nothing runs: a Mathlib requirement
//    written with TOML literal strings is seen (the Lean server does not
//    start there), the plan names where every dependency is fetched from, a
//    manifest changed between review and start is a stale plan, a `mathlib`
//    from another repository is planned with a warning that names the
//    address (in the plan, under its hash, and in the dialog), the
//    build-from-source choice does
//    not start Lake on a package that is not resolved, and an untrusted
//    folder starts nothing.
// B. The setup actually run, briefly: a new package in a folder that does
//    not exist yet is started from IPC and canceled as soon as its first
//    command runs (`lake update`, the first seconds of a clone; nothing
//    large is downloaded). While it runs, the user adds a file of their own
//    in the new library folder. Afterwards: everything the setup created is
//    removed, the user's file is kept and named in the outcome, nothing else
//    changed, and nothing keeps writing.
//
// Self-skips without Lean 4.35 installed through elan (set
// EXPOSITION_REQUIRE_LEAN=1 to fail instead).
//
// Requires a debug build: it calls the debug-only probe command
// lean_package_inspect.
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, openInEditor, waitFor } from "../workspace-helpers.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const REV = "56a005855ea17e0b0958f73ec801ae140b0a1679";
const MATHLIB_URL = "https://github.com/leanprover-community/mathlib4";
const FORK_URL = "https://github.com/attacker-example/mathlib4";
const BATTERIES_URL = "https://github.com/leanprover-community/batteries";
const BATTERIES_REV = "66e469188fe07963e5cf666c7d4433f45b8131c5";
const manifest = (mathlibUrl, batteriesUrl, batteriesRev) =>
  JSON.stringify({
    version: "1.3.0",
    packagesDir: ".lake/packages",
    packages: [
      { url: mathlibUrl, type: "git", rev: REV, name: "mathlib", inputRev: "v4.35.0" },
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

export default async function mathlibSetupGuard({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-mathlib-guard-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await mkdir(join(root, "literal", "Literal"), { recursive: true });
  await mkdir(join(root, "pinned"), { recursive: true });
  await mkdir(dataDir, { recursive: true });
  await cp(fixture("lean-mathlib"), join(root, "formal"), { recursive: true });
  await writeFile(join(root, "paper.typ"), "= Paper\n");
  // The same requirement as the fixture's, written with TOML literal strings.
  await writeFile(join(root, "literal", "lakefile.toml"), "name = 'Literal'\n\n[[require]]\nname = 'mathlib'\nscope = 'leanprover-community'\nrev = 'v4.35.0'\n\n[[lean_lib]]\nname = 'Literal'\n");
  await writeFile(join(root, "literal", "lean-toolchain"), "leanprover/lean4:v4.35.0\n");
  await writeFile(join(root, "literal", "Literal", "Basic.lean"), "theorem literal : 1 + 1 = 2 := rfl\n");
  // An existing package whose manifest pins its dependencies (not downloaded).
  await writeFile(join(root, "pinned", "lakefile.toml"), 'name = "Pinned"\n\n[[require]]\nname = "mathlib"\nscope = "leanprover-community"\nrev = "v4.35.0"\n\n[[lean_lib]]\nname = "Pinned"\n');
  await writeFile(join(root, "pinned", "lean-toolchain"), "leanprover/lean4:v4.35.0\n");
  await writeFile(join(root, "pinned", "lake-manifest.json"), manifest(MATHLIB_URL, BATTERIES_URL, BATTERIES_REV));

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 900)}`);
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
      "A-literal-string-requirement-is-seen",
      literal.usesMathlib && literal.readiness.kind === "dependenciesUnresolved" && literal.mathlibInputRevision === "v4.35.0",
      { usesMathlib: literal.usesMathlib, readiness: literal.readiness },
    );

    // The plan names every source with its address and commit.
    const reviewed = await invoke(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "pinned" } });
    const sources = Object.fromEntries(reviewed.sources.map((s) => [s.name, s]));
    note(
      "A-plan-names-every-source-with-address-and-commit",
      reviewed.sources.length === 2 &&
        sources.mathlib?.url === MATHLIB_URL &&
        sources.mathlib?.rev === REV &&
        sources.batteries?.url === BATTERIES_URL &&
        sources.batteries?.rev === BATTERIES_REV &&
        typeof reviewed.manifestDigest === "string" &&
        typeof reviewed.lakefileDigest === "string",
      reviewed.sources,
    );
    // Untrusted: a reviewed plan does not start.
    const untrusted = await invokeError(page, "lean_setup_start", { plan: reviewed });
    note("A-untrusted-folder-starts-nothing", untrusted.error?.code === "lean.untrusted", untrusted.error ?? untrusted);
    // Another dependency redirected after the review: a stale plan (checked before trust), and the new plan says so.
    await writeFile(join(root, "pinned", "lake-manifest.json"), manifest(MATHLIB_URL, "https://github.com/attacker-example/batteries", "0000000000000000000000000000000000000001"));
    const again = await invoke(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "pinned" } });
    const swapped = await invokeError(page, "lean_setup_start", { plan: reviewed });
    note(
      "A-changed-manifest-is-a-stale-plan",
      swapped.error?.code === "lean.setup.planStale" &&
        again.planHash !== reviewed.planHash &&
        again.sources.some((s) => s.url === "https://github.com/attacker-example/batteries" && s.rev === "0000000000000000000000000000000000000001"),
      { start: swapped.error?.code ?? swapped, samePlanHash: again.planHash === reviewed.planHash, sources: again.sources },
    );
    // The lakefile edited after the review: stale as well.
    await writeFile(join(root, "pinned", "lakefile.toml"), 'name = "Pinned"\n\n[[require]]\nname = "mathlib"\nscope = "leanprover-community"\nrev = "v4.35.0"\n\n[[lean_lib]]\nname = "Pinned"\n# edited\n');
    const edited = await invokeError(page, "lean_setup_start", { plan: again });
    note("A-changed-lakefile-is-a-stale-plan", edited.error?.code === "lean.setup.planStale", edited.error ?? edited);
    // `mathlib` taken from another repository: planned, and the plan says so
    // with exactly that address. The fact is under the plan hash.
    const officialNow = await invoke(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "pinned" } });
    await writeFile(join(root, "pinned", "lake-manifest.json"), manifest(FORK_URL, BATTERIES_URL, BATTERIES_REV));
    const fork = await invoke(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder: "pinned" } });
    note(
      "A-mathlib-from-a-fork-is-planned-and-named",
      fork.mathlibSource?.official === false &&
        fork.mathlibSource?.url === FORK_URL &&
        fork.sources.some((s) => s.name === "mathlib" && s.url === FORK_URL) &&
        fork.summary.includes(`Mathlib comes from ${FORK_URL}, not the official repository`) &&
        fork.planHash !== officialNow.planHash &&
        officialNow.mathlibSource?.official === true &&
        officialNow.mathlibSource?.url === MATHLIB_URL,
      { mathlibSource: fork.mathlibSource, official: officialNow.mathlibSource, summary: fork.summary },
    );
    const hidden = await invokeError(page, "lean_setup_start", { plan: { ...fork, mathlibSource: officialNow.mathlibSource } });
    note("A-a-fork-plan-edited-to-official-is-stale", hidden.error?.code === "lean.setup.planStale", hidden.error ?? hidden);
    // The plan reviewed for the official repository does not start on the fork's manifest.
    const official = await invokeError(page, "lean_setup_start", { plan: officialNow });
    note("A-official-plan-does-not-start-on-a-fork", official.error?.code === "lean.setup.planStale", official.error ?? official);
    // The gate sees a fork as Mathlib, as before.
    const forked = await invoke(page, "lean_package_inspect", { folder: "pinned" });
    note(
      "A-a-fork-is-still-a-mathlib-package",
      forked.usesMathlib && forked.mathlibRevision === REV && forked.readiness.kind === "dependenciesMissing",
      { usesMathlib: forked.usesMathlib, readiness: forked.readiness },
    );
    // The dialog shows the warning with the Sources table and marks the row; one primary action.
    await page.setViewportSize({ width: 1440, height: 1000 }).catch(() => {});
    await runCommand(page, "set up mathlib", /Set Up Mathlib/);
    const dialog = page.getByRole("dialog", { name: "Set up Mathlib" });
    await dialog.getByRole("button", { name: /Package/ }).click();
    await page.getByRole("option", { name: /pinned/ }).click();
    const planView = page.getByTestId("lean-setup-plan");
    await waitFor(async () => (await planView.getAttribute("data-plan-hash")) === fork.planHash, { timeout: 20_000, message: "the fork's plan in the dialog" });
    const warning = (await page.getByTestId("lean-setup-fork-warning").innerText()).replace(/\s+/g, " ");
    const row = page.locator('[data-testid="lean-setup-sources"] [data-source="mathlib"]');
    note(
      "A-dialog-warns-about-the-fork-with-its-address",
      warning.includes("Mathlib is not from the official repository") &&
        warning.includes(`Mathlib comes from ${FORK_URL}, not the official repository`) &&
        /its cache program runs on this computer with your account.s privileges/.test(warning) &&
        warning.includes("The prebuilt cache may not cover a fork") &&
        (await row.getAttribute("data-official")) === "false" &&
        /Not official/.test(await row.innerText()) &&
        (await dialog.getByTestId("lean-setup-start").count()) === 1 &&
        (await page.getByRole("dialog").count()) === 1,
      { warning, row: await row.innerText() },
    );
    await page.screenshot({ path: join(artifactsDir, "mathlib-setup-plan-fork-light.png") });
    if (process.env.EXPOSITION_DEV_HOME) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "research");
      await mkdir(dir, { recursive: true });
      await cp(join(artifactsDir, "mathlib-setup-plan-fork-light.png"), join(dir, "mathlib-setup-plan-fork-light.png"));
    }
    await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
    await dialog.waitFor({ state: "hidden", timeout: 10_000 });
    note("A-no-setup-ran", (await invoke(page, "lean_setup_status")) === null, await invoke(page, "lean_setup_status"));

    // Trusted: the build-from-source choice does not start Lake on a package
    // whose dependencies are not resolved, in either spelling.
    await invoke(page, "trust_grant", { root: info.root });
    const doc = await openInEditor(page, "formal/OddSums/Basic.lean");
    const state = async (id) => (await invoke(page, "lean_documents")).find((d) => d.document === id);
    await waitFor(async () => (await state(doc))?.status.kind === "unavailable", { timeout: 60_000, interval: 250, message: "the unprepared package is not started" });
    await invoke(page, "lean_allow_local_build", { document: doc });
    const literalDoc = await openInEditor(page, "literal/Literal/Basic.lean");
    await waitFor(async () => (await state(literalDoc))?.status.kind === "unavailable", { timeout: 60_000, interval: 250, message: "the literal-string package is not started" });
    await new Promise((r) => setTimeout(r, 4000));
    note(
      "A-build-from-source-does-not-lift-unresolved-dependencies",
      (await state(doc))?.status.kind === "unavailable" && !existsSync(join(root, "formal", ".lake")) && !existsSync(join(root, "formal", "lake-manifest.json")),
      { status: (await state(doc))?.status, lake: existsSync(join(root, "formal", ".lake")) },
    );
    note(
      "A-literal-string-package-starts-no-server",
      (await state(literalDoc))?.status.kind === "unavailable" &&
        /Set Up Mathlib/.test((await state(literalDoc))?.status.reason ?? "") &&
        !existsSync(join(root, "literal", ".lake")) &&
        !existsSync(join(root, "literal", "lake-manifest.json")),
      { status: (await state(literalDoc))?.status, lake: existsSync(join(root, "literal", ".lake")) },
    );
    const bogus = await invokeError(page, "lean_allow_local_build", { document: "doc-does-not-exist" });
    note("A-build-from-source-needs-a-lean-document", !!bogus.error, bogus.error ?? bogus);
    note("A-cancel-of-an-unknown-setup-is-false", (await invoke(page, "lean_setup_cancel", { setupId: "0".repeat(32) })) === false, "");

    // ---- B. a real setup, canceled as soon as its first command runs ---------------
    const before = await tree(root);
    const plan = await invoke(page, "lean_setup_plan", { request: { kind: "newMathlibPackage", folder: "fresh", name: "Formal" } });
    note("B-plan-creates-in-the-named-folder", plan.packageRoot.replace(/\\/g, "/").endsWith("/project/fresh") && plan.createsPackage && plan.steps[0].args.join(" ") === "--keep-toolchain update", { packageRoot: plan.packageRoot, steps: plan.steps.map((s) => s.args.join(" ")) });
    // The package folder does not exist yet (the dialog's default case).
    const first = await invokeError(page, "lean_setup_start", { plan });
    note("B-a-new-package-in-a-new-folder-starts", "unexpectedSuccess" in first && !existsSync(join(root, "fresh", "lakefile.lean")), first.error ?? "started");
    if (!("unexpectedSuccess" in first)) throw new Error(`the setup did not start: ${JSON.stringify(first.error)}`);
    setupId = first.unexpectedSuccess.id;
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
    note(
      "B-the-users-own-file-survives-cleanup",
      existsSync(mine) && JSON.stringify(status.kept) === JSON.stringify(["Formal/Mine.lean"]),
      { kept: status.kept, removed: status.removed },
    );
    note(
      "B-the-outcome-says-what-was-kept",
      /Kept, because they were added or changed while it ran: `Formal\/Mine\.lean`/.test(status.message ?? "") &&
        !/The new package's files were removed; nothing else/.test(status.message ?? ""),
      status.message,
    );
    // Nothing keeps writing after cleanup, and nothing else changed.
    const after1 = await tree(root);
    await new Promise((r) => setTimeout(r, 5000));
    const after2 = await tree(root);
    note("B-nothing-writes-after-cleanup", JSON.stringify(after1) === JSON.stringify(after2), { extra: after2.filter((p) => !after1.includes(p)).slice(0, 10) });
    const added = after2.filter((p) => !before.includes(p));
    note("B-what-the-setup-created-is-removed", JSON.stringify(added) === JSON.stringify(["fresh/", "fresh/Formal/", "fresh/Formal/Mine.lean"]), { added: added.slice(0, 20) });
    const missing = before.filter((p) => !after2.includes(p));
    note("B-nothing-else-was-removed", missing.length === 0, { missing });
    const fresh = await invoke(page, "lean_package_inspect", { folder: "fresh" });
    note("B-the-canceled-package-is-not-ready", fresh.readiness.kind === "notAPackage", fresh.readiness);
    const texts = [status.message ?? "", ...(status.log ?? [])].join("\n");
    note("B-status-shows-no-credential", !/sk-ant-|ANTHROPIC_API_KEY/.test(texts), "");
  } finally {
    if (setupId) await invoke(page, "lean_setup_cancel", { setupId }).catch(() => {});
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2)).catch(() => {});
    await app.close();
    await rm(base, { recursive: true, force: true }).catch(() => {});
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((o) => o.id), [], `checks failed: ${JSON.stringify(failed).slice(0, 1500)}`);
  console.log(`mathlib-setup-guard: ${observations.length} ok`);
}
