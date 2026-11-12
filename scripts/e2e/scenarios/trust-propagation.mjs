// Trust propagation: a grant or revoke made in one view reaches every view and
// service that caches trust, with no restart and no click on each view's own
// Trust button. Trust is a security boundary, so revocation is checked as
// closely as the grant: trusted work stops and every surface says untrusted.
//
//   1 open the shared paper project (tests/fixtures/m6-paper) untrusted: the
//     Lean infoview is Untrusted, the LaTeX page view's prompt is declined,
//     Research shows its banner
//   2 trust from the LaTeX page view's own button: the Research banner goes,
//     the Lean infoview leaves Untrusted by itself, lean_check (the agent's
//     check_lean) checks, and the page builds
//   3 start a research session and run code (the kernel works)
//   4 revoke from Settings > Trusted folders: the Lean server stops and the
//     infoview says Untrusted, lean_check refuses, the research session is
//     detached and its kernel is gone, Research shows its banner, the LaTeX
//     page says "Workspace not trusted" over its last pages, the status bar
//     says so too, and no trust prompt opens by itself
//   5 trust again from the Lean infoview: Research, the page, and Lean follow
//
// Needs Lean 4.35.0 through elan, TeX Live (pdflatex), and Python with
// ipykernel/jupyter_client.
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { waitForAsync } from "../wait.mjs";
import { invoke, openFromTree, openInEditor, waitFor } from "../workspace-helpers.mjs";

const LEMMA = "Paper/Gauss.lean";
const SUPPLEMENT = "supplement.tex";
const LEAN_TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const KERNEL_TIMEOUT = 120_000;
const LEAN_TIMEOUT = 300_000;
/** How long a surface may take to follow a trust change. */
const FOLLOW = 15_000;

function processTable() {
  const out = execFileSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine | ConvertTo-Json -Compress",
    ],
    { encoding: "utf8", maxBuffer: 128 * 1024 * 1024 },
  );
  return JSON.parse(out).map((p) => ({ pid: p.ProcessId, parent: p.ParentProcessId, name: p.Name, commandLine: p.CommandLine ?? "" }));
}

/** Descendants of `rootPid` whose image name matches `name`. */
function descendantsNamed(rootPid, name) {
  const table = processTable();
  const out = [];
  const queue = [rootPid];
  while (queue.length) {
    const parent = queue.shift();
    for (const p of table) {
      if (p.parent === parent && !out.some((o) => o.pid === p.pid)) {
        out.push(p);
        queue.push(p.pid);
      }
    }
  }
  return out.filter((p) => name.test(p.name));
}

const processesMentioning = (marker) => processTable().filter((p) => p.commandLine.toLowerCase().includes(marker.toLowerCase()));

async function runCommand(page, query, expected) {
  await page.keyboard.press("Control+Shift+P");
  const input = page.getByRole("textbox", { name: "Command" });
  await input.waitFor({ state: "visible", timeout: 5000 });
  await input.fill(query);
  const first = page.locator(".wb-palette__item").first();
  await first.waitFor({ state: "visible" });
  const title = await first.innerText();
  if (!expected.test(title)) throw new Error(`palette top match for "${query}" was "${title}"`);
  await page.keyboard.press("Enter");
  await input.waitFor({ state: "detached", timeout: 5000 });
}

async function dismissNotifications(page) {
  if ((await page.locator('[role="dialog"], [role="alertdialog"]').count()) > 0) return;
  const dismiss = page.getByRole("button", { name: /^Dismiss notification/ });
  for (let i = 0; i < 12 && (await dismiss.count()) > 0; i++) await dismiss.first().click().catch(() => {});
}

export default async function trustPropagation({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  assert.ok(existsSync(join(elanHome, "toolchains", LEAN_TOOLCHAIN_DIR, "bin")), `Lean ${LEAN_TOOLCHAIN_DIR} is required (elan)`);
  const base = await mkdtemp(join(tmpdir(), "xp-trust-"));
  const root = join(base, "paper");
  const dataDir = join(base, "data");
  await cp(fixture("m6-paper"), root, { recursive: true });
  await mkdir(dataDir, { recursive: true });
  const app = await launchApp({ dataDir });
  const { page } = app;
  const timings = {};
  const shot = async (name) => {
    await dismissNotifications(page).catch(() => {});
    await page.waitForTimeout(250);
    await page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  };
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    const research = page.getByTestId("research-panel");
    const lean = page.getByTestId("lean-panel");
    const pageEditor = page.locator('[data-testid="page-editor"]:visible').first();
    const modeButton = (label) => page.locator(".ui-segmented__item:visible", { hasText: label }).first();
    const trustPrompt = page.getByRole("alertdialog", { name: "Trust this workspace?" });
    const researchBanner = () => research.getByText("Workspace not trusted", { exact: true });
    const leanStatus = () => lean.getAttribute("data-status");
    const leanCheck = (document, timeoutMs) =>
      invoke(page, "lean_check", { request: { target: { kind: "document", document }, goalsAt: [], timeoutMs } });
    /** The page view of the supplement shows pages built for the current revision. */
    const supplementBuilt = (timeout, message) =>
      waitForAsync(
        page,
        async (path) => {
          const el = [...document.querySelectorAll('[data-testid="page-editor"]')].find((e) => e.offsetParent !== null);
          if (!el) return false;
          const imgs = [...el.querySelectorAll('[data-testid="page"] img')];
          if (!(el.dataset.stale === "false" && el.dataset.build && imgs.length > 0 && imgs.every((i) => i.complete))) return false;
          if (el.querySelector(".page-editor__untrusted")) return false;
          const doc = (await window.__TAURI_INTERNALS__.invoke("document_list")).find((d) => d.path === path);
          return !!doc && el.dataset.buildRevision === String(doc.revision);
        },
        SUPPLEMENT,
        { timeout, message },
      );

    // ---- 1: untrusted everywhere ------------------------------------------
    const opened = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="main.typ"]').waitFor({ timeout: 15_000 });
    await runCommand(page, "Show Research", /Show Research/);
    await researchBanner().waitFor({ timeout: 15_000 });
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    const leanId = await openInEditor(page, LEMMA);
    await waitFor(async () => (await leanStatus()) === "untrusted", { timeout: 30_000, message: "Lean untrusted" });
    await openFromTree(page, SUPPLEMENT);
    await modeButton("Page").waitFor({ timeout: 10_000 });
    await modeButton("Page").click();
    await trustPrompt.waitFor({ timeout: 10_000 });
    await trustPrompt.getByRole("button", { name: /Don.t trust/ }).click();
    await trustPrompt.waitFor({ state: "detached" });
    await page.getByTestId("page-scroller").getByText("Workspace not trusted", { exact: true }).waitFor({ timeout: 10_000 });
    assert.equal((await leanCheck(leanId, 5_000)).status.kind, "untrusted", "lean_check refuses while untrusted");
    assert.deepEqual(descendantsNamed(app.child.pid, /^(lean|lake)\.exe$/i), [], "no Lean process while untrusted");

    // ---- 2: trust from the page view; every other surface follows --------
    await page.getByTestId("page-scroller").getByRole("button", { name: "Trust workspace…" }).click();
    await trustPrompt.waitFor();
    await trustPrompt.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await trustPrompt.waitFor({ state: "detached" });
    const grantedAt = Date.now();
    assert.equal((await invoke(page, "trust_status", { root: opened.root })).trusted, true);
    // lean_check right away (as an agent's check_lean would): it checks.
    const checked = await leanCheck(leanId, LEAN_TIMEOUT);
    assert.notEqual(checked.status.kind, "untrusted", `lean_check after the grant: ${JSON.stringify(checked.status)}`);
    assert.equal(checked.checked, true, `lean_check checks after the grant: ${JSON.stringify(checked).slice(0, 400)}`);
    timings.grantToLeanChecked = Date.now() - grantedAt;
    // Lean infoview (not touched since it showed Untrusted).
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    await openInEditor(page, LEMMA);
    await waitFor(async () => (await leanStatus()) !== "untrusted", { timeout: FOLLOW, message: "Lean infoview leaves Untrusted after a grant in the page view" });
    // Research banner.
    await runCommand(page, "Show Research", /Show Research/);
    await researchBanner().waitFor({ state: "detached", timeout: FOLLOW });
    // The page view built.
    await openFromTree(page, SUPPLEMENT);
    await pageEditor.waitFor({ timeout: 10_000 });
    await supplementBuilt(120_000, "supplement pages after the grant");
    await page.locator('[data-testid="build-status"][data-status="succeeded"]').waitFor({ timeout: FOLLOW });
    timings.grantToPages = Date.now() - grantedAt;
    await shot("02-trusted");

    // ---- 3: research session runs code -------------------------------------
    await runCommand(page, "Show Research", /Show Research/);
    await page.getByRole("button", { name: "Start session" }).click();
    await page.locator('[data-testid="research-session-status"][data-state="idle"]').waitFor({ timeout: KERNEL_TIMEOUT });
    const code = page.getByTestId("research-code");
    await code.fill("print('kernel ran', 6 * 7)");
    await code.focus();
    await page.keyboard.press("Control+Enter");
    await page.locator('[data-testid="research-output"][data-status="succeeded"]').first().waitFor({ timeout: KERNEL_TIMEOUT });
    const [session] = (await invoke(page, "research_sessions")).filter((s) => s.owner.kind === "user");
    assert.ok(session && session.state.kind === "idle", `a live user session: ${JSON.stringify(session?.state)}`);
    const researchDir = join(dataDir, "research");
    assert.ok(processesMentioning(researchDir).length > 0, "the research helper runs");
    const leanJob = (await invoke(page, "lean_documents")).find((d) => d.document === leanId)?.job;
    assert.ok(leanJob, "the Lean server runs as a job");
    assert.ok(descendantsNamed(app.child.pid, /^lean\.exe$/i).length > 0, "a Lean process runs");

    // ---- 4: revoke from Settings; trusted work stops, every surface says so --
    await runCommand(page, "Manage Trusted Folders", /Manage Trusted Folders/);
    const trustedRoot = (await invoke(page, "trust_status", { root: opened.root })).root;
    const row = page.getByTestId("trusted-folder").filter({ hasText: trustedRoot }).first();
    await row.waitFor({ timeout: 10_000 });
    await row.getByRole("button", { name: /^Revoke trust for / }).click();
    const revokeDialog = page.getByRole("alertdialog", { name: "Revoke trust for this folder?" });
    await revokeDialog.waitFor();
    await revokeDialog.getByRole("button", { name: "Revoke trust", exact: true }).click();
    await revokeDialog.waitFor({ state: "detached" });
    const revokedAt = Date.now();
    assert.equal((await invoke(page, "trust_status", { root: opened.root })).trusted, false);
    // The check seam refuses at once.
    const refused = await leanCheck(leanId, 5_000);
    assert.equal(refused.status.kind, "untrusted", `lean_check after the revoke: ${JSON.stringify(refused.status)}`);
    assert.equal(refused.checked, false);
    // The Lean server job ended and its processes are gone.
    await waitFor(
      async () => (await invoke(page, "job_list")).find((j) => j.info.id === leanJob)?.info.result,
      { timeout: FOLLOW, message: "the Lean server job finished after the revoke" },
    );
    await waitFor(() => descendantsNamed(app.child.pid, /^(lean|lake)\.exe$/i).length === 0, {
      timeout: FOLLOW,
      interval: 500,
      message: "no Lean process after the revoke",
    });
    // The research session is detached and its kernel is gone.
    await waitFor(
      async () => (await invoke(page, "research_sessions")).find((s) => s.id === session.id)?.state.kind === "detached",
      { timeout: FOLLOW, message: "research session detached after the revoke" },
    );
    await waitFor(() => processesMentioning(researchDir).length === 0, {
      timeout: FOLLOW,
      interval: 500,
      message: "no research helper or kernel after the revoke",
    });
    timings.revokeToWorkStopped = Date.now() - revokedAt;
    // Every view says untrusted again, and nothing prompts on its own.
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    await openInEditor(page, LEMMA);
    await waitFor(async () => (await leanStatus()) === "untrusted", { timeout: FOLLOW, message: "Lean infoview shows Untrusted after the revoke" });
    await runCommand(page, "Show Research", /Show Research/);
    await researchBanner().waitFor({ timeout: FOLLOW });
    await page.getByTestId("research-detached-reason").getByText(/trust/i).waitFor({ timeout: FOLLOW });
    await openFromTree(page, SUPPLEMENT);
    await pageEditor.waitFor({ timeout: 10_000 });
    await pageEditor.locator(".page-editor__untrusted").getByText("Workspace not trusted", { exact: true }).waitFor({ timeout: FOLLOW });
    assert.ok((await pageEditor.locator('[data-testid="page"] img').count()) > 0, "the last pages stay visible");
    assert.match(await page.getByTestId("page-build-status").innerText(), /Not trusted/);
    await page.locator('[data-testid="build-status"][data-status="untrusted"]').waitFor({ timeout: FOLLOW });
    await page.waitForTimeout(1500);
    assert.equal(await trustPrompt.count(), 0, "no trust prompt opens by itself after a revoke");
    timings.revokeToSurfaces = Date.now() - revokedAt;
    await shot("04-revoked");

    // ---- 5: trust again from the Lean infoview -------------------------------
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    await openInEditor(page, LEMMA);
    await lean.getByRole("button", { name: "Trust workspace…" }).click();
    await trustPrompt.waitFor();
    await trustPrompt.getByRole("button", { name: "Trust workspace", exact: true }).click();
    await trustPrompt.waitFor({ state: "detached" });
    await waitFor(async () => (await leanStatus()) !== "untrusted", { timeout: FOLLOW, message: "Lean leaves Untrusted" });
    await runCommand(page, "Show Research", /Show Research/);
    await researchBanner().waitFor({ state: "detached", timeout: FOLLOW });
    await openFromTree(page, SUPPLEMENT);
    await pageEditor.waitFor({ timeout: 10_000 });
    await supplementBuilt(120_000, "supplement pages after trusting again");
    assert.equal(await pageEditor.locator(".page-editor__untrusted").count(), 0);
    await shot("05-trusted-again");

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    assert.deepEqual(errors, [], "no console errors");
    console.log(`      timings (ms): ${JSON.stringify(timings)}`);
  } finally {
    await app.close();
  }
}
