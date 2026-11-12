// Mathematics view, "Associate a Lean declaration" (M7 lane D) against the real
// Lean audit: a declaration admitted with FormalCheck evidence by the Lean
// service is associated with an informal theorem from the Graph page's
// Mathematics inspector. The result is shown as a reviewed correspondence
// (never as equivalence); the verdict and freshness columns stay driven by the
// evidence (exact verdict, freshness never "current").
//
// Requires Lean through elan with leanprover/lean4:v4.35.0; without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_LEAN=1. Screens go to
// test-results/e2e/graph-lean-association/ and, with EXPOSITION_DEV_HOME set,
// to $EXPOSITION_DEV_HOME/ui-reference/graph/.
import { existsSync } from "node:fs";
import { copyFile, cp, mkdir, mkdtemp } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, openInEditor, waitFor } from "../workspace-helpers.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const DEFS = "LeanBasic/Defs.lean";
const DECLARATION = "LeanBasic.double_zero";

async function change(page, operations) {
  const status = await invoke(page, "graph_status");
  const proposal = await invoke(page, "graph_propose_change", { request: { baseRevision: status.revision, operations, summary: null } });
  if (!proposal.valid) throw new Error(`proposal invalid: ${JSON.stringify(proposal.diagnostics)}`);
  const outcome = await invoke(page, "graph_apply_change", { request: { proposal: proposal.id, expectedRevision: proposal.baseRevision } });
  if (outcome.kind !== "applied") throw new Error(`apply ${outcome.kind}`);
  return outcome;
}

export default async function graphLeanAssociation({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-graph-lean-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("lean-basic"), root, {
    recursive: true,
    filter: (src) => !/[\\/]\.lake([\\/]|$)/.test(src) && !src.endsWith("lake-manifest.json"),
  });
  await mkdir(dataDir, { recursive: true });
  const shots = [];
  const app = await launchApp({ dataDir, env: { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-graph-lean-${Date.now()}` } });
  const { page } = app;
  const shot = async (name) => {
    const path = join(artifactsDir, `${name}.png`);
    await page.screenshot({ path });
    shots.push(path);
  };
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await runCommand(page, "color theme light", /Color Theme: Light/);
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="lakefile.toml"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: info.root });
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    const defsDoc = await openInEditor(page, DEFS);
    await waitFor(async () => (await page.getByTestId("lean-panel").getAttribute("data-status")) === "checked", {
      timeout: 300_000,
      interval: 250,
      message: "Defs.lean checked",
    });

    // The Lean service records FormalCheck evidence and admits the declaration entity.
    const recorded = await invoke(page, "lean_record_evidence", { document: defsDoc, declarations: [DECLARATION] });
    assert.equal(recorded.recorded.length, 1, `evidence recorded: ${JSON.stringify(recorded)}`);
    const theorem = (await change(page, [{ op: "createEntity", tempId: "t", kind: "theorem", roles: [], label: "Doubling zero (paper)", key: null, attributes: {}, binding: null }])).created[0].id;

    // Graph → Mathematics → Verification → the theorem: no association, no verdict.
    await page.keyboard.press("Control+4");
    await page.getByTestId("graph-page").waitFor();
    await page.getByRole("radio", { name: "Mathematics" }).click();
    await page.getByRole("radio", { name: "Verification" }).click();
    const grid = page.getByRole("grid", { name: "Verification" });
    const row = grid.locator(`[data-entity-id="${theorem}"]`);
    await row.waitFor({ timeout: 15_000 });
    assert.equal(await row.getAttribute("data-verdict"), "none");
    await row.click();
    const button = page.getByTestId("graph-associate-button");
    await button.waitFor();
    assert.equal(await button.isDisabled(), false, "a formal declaration exists to associate");
    await button.click();
    const dialog = page.getByTestId("graph-associate-dialog");
    await dialog.waitFor();
    assert.match(await page.getByTestId("graph-associate-note").innerText(), /not a machine proof that they are equivalent/);
    const select = dialog.locator(".ui-select", { has: page.locator(".ui-field__label", { hasText: "Formal declaration" }) });
    // Indexing admits every declaration of the project; choose the audited one.
    await select.getByRole("button").click();
    await page.getByRole("option", { name: DECLARATION, exact: true }).click();
    assert.match(await select.innerText(), new RegExp(DECLARATION.replace(".", "\\.")), "the audited declaration is chosen");
    await page.getByRole("textbox", { name: "Note (optional)" }).fill("Both state double 0 = 0");
    await shot("graph-lean-associate-dialog-light");
    await page.getByRole("button", { name: "Record reviewed association" }).click();
    await dialog.waitFor({ state: "detached", timeout: 15_000 });

    // Shown as a reviewed correspondence; verdict and freshness come from the evidence.
    await waitFor(async () => /Reviewed association/.test(await row.innerText()), { timeout: 15_000, message: "formal association column" });
    await waitFor(async () => (await row.getAttribute("data-verdict")) === "checked", { timeout: 15_000, message: "verdict from the evidence" });
    assert.notEqual(await row.getAttribute("data-freshness"), "current", "freshness is never shown current (dependencies are not fingerprinted)");
    const formalSection = page.getByTestId("graph-formal-section");
    await formalSection.getByTestId("graph-formalization").first().waitFor({ timeout: 15_000 });
    assert.match(await page.getByTestId("graph-correspondence-note").innerText(), /not a machine proof/);
    const math = await invoke(page, "graph_math_overview");
    const mathRow = math.rows.find((r) => r.entity.id === theorem);
    assert.equal(mathRow.formalAssociation, "reviewed");
    assert.equal(mathRow.verdict, "checked");
    for (const dismiss of await page.getByRole("button", { name: /^Dismiss notification/ }).all()) await dismiss.click().catch(() => {});
    await page.getByRole("region", { name: "Notifications" }).waitFor({ state: "detached" }).catch(() => {});
    await page.getByTestId("graph-correspondence-note").scrollIntoViewIfNeeded();
    await shot("graph-lean-associated-light");
    await runCommand(page, "color theme dark", /Color Theme: Dark/);
    await shot("graph-lean-associated-dark");

    const errors = app.consoleMessages.filter((m) => m.startsWith("error:") || m.startsWith("pageerror:"));
    assert.deepEqual(errors, [], "no console errors");
  } finally {
    await app.close();
  }
  const devHome = process.env.EXPOSITION_DEV_HOME;
  if (devHome) {
    const target = join(devHome, "ui-reference", "graph");
    await mkdir(target, { recursive: true });
    for (const path of shots) await copyFile(path, join(target, path.split(/[\\/]/).pop()));
    console.log(`copied ${shots.length} screenshots to ${target}`);
  }
}
