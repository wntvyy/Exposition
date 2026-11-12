// M7 lane B: exact, version-bound Lean evidence in the project graph, against
// the real Lean server and axiom audit (leanprover/lean4:v4.35.0).
//
// - Associate an audited declaration with an informal claim through the Lean
//   infoview ("Associate with…"): FormalCheck evidence is recorded from the Lean
//   service's own audit (statement, axioms, file + declaration verdict,
//   toolchain, digest, revision; dependencies unavailable, no fingerprints),
//   and the reviewed `formalizes` association is a separate accepted relation.
// - Freshness is never "current"; after an edit the old record is history
//   (stale) and new evidence can be recorded for the new revision.
// - Verdict distinctions are preserved (Checked / Checked with axioms /
//   Incomplete), and a declaration the audit did not list gets no evidence.
// - The Mathematics view's IPC associates an existing declaration entity.
// - Restricted Lean execution stays refused (agent task in restricted mode).
//
// Requires Lean through elan with leanprover/lean4:v4.35.0 installed; without it
// the scenario reports SKIPPED unless EXPOSITION_REQUIRE_LEAN=1.
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { runCommand } from "../palette.mjs";
import { editorLocator, invoke, invokeError, openInEditor, waitFor } from "../workspace-helpers.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const DEFS = "LeanBasic/Defs.lean";
const MIXED = "LeanBasic/Mixed.lean";
const MIXED_TEXT = `namespace LeanBasic

axiom myAx : 1 = 2

theorem usesAx : 1 = 2 := myAx

theorem clean : 2 = 2 := rfl

theorem notDone : 3 = 3 := by
  sorry

end LeanBasic
`;

export default async function leanEvidence({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-lean-evidence-"));
  const root = join(base, "project");
  const dataDir = join(base, "data");
  await cp(fixture("lean-basic"), root, {
    recursive: true,
    filter: (src) => !/[\\/]\.lake([\\/]|$)/.test(src) && !src.endsWith("lake-manifest.json"),
  });
  await writeFile(join(root, ...MIXED.split("/")), MIXED_TEXT);
  await mkdir(dataDir, { recursive: true });

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)}`);
  };
  const env = { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-lean-evidence-${Date.now()}` };
  const app = await launchApp({ dataDir, env });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) });
  try {
    await page.setViewportSize({ width: 1440, height: 1000 }).catch(() => {});
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="lakefile.toml"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: info.root });
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);
    const defsDoc = await openInEditor(page, DEFS);
    const panel = page.getByTestId("lean-panel");
    await waitFor(async () => (await panel.getAttribute("data-status")) === "checked", {
      timeout: 300_000,
      interval: 250,
      message: "Defs.lean checked",
    });

    // ---- the service's audit record ---------------------------------------
    const audit = await invoke(page, "lean_audit_record", { document: defsDoc });
    const zero = audit.declarations.find((d) => d.name === "LeanBasic.double_zero");
    note("audit-record-from-service", audit.status.kind === "checked" && zero?.verdict === "checked" && audit.contentHash.length === 64, {
      revision: audit.revision,
      statement: zero?.statement,
      axioms: zero?.axioms,
    });

    // ---- associate through the infoview ------------------------------------
    const row = panel.locator('[data-testid="lean-checked-declaration"][data-name="LeanBasic.double_zero"]');
    await row.getByRole("button", { name: "Associate with…" }).click();
    const dialog = page.getByTestId("lean-associate-dialog");
    await dialog.waitFor();
    await page.getByRole("textbox", { name: "Or create a claim" }).fill("Doubling zero gives zero");
    await page.getByRole("button", { name: "Create claim" }).click();
    await waitFor(async () => (await page.getByRole("button", { name: /Informal statement/ }).innerText()).includes("Doubling zero"), {
      message: "the new claim is selected",
    });
    await page.getByRole("textbox", { name: "Note (optional)" }).fill("Both state double 0 = 0");
    await page.getByRole("button", { name: "Record reviewed association" }).click();
    await dialog.waitFor({ state: "detached", timeout: 15_000 });
    const association = row.getByTestId("lean-decl-association");
    await association.waitFor({ timeout: 15_000 });
    const assocText = await association.innerText();
    note("association-is-reviewed-correspondence", (await association.getAttribute("data-status")) === "accepted" && /not a proof of equivalence/.test(assocText), assocText);
    const latest = row.getByTestId("lean-evidence-latest");
    await latest.waitFor();
    note("evidence-shown-freshness-unknown", (await latest.getAttribute("data-freshness")) === "unknown", await latest.innerText());
    await shot("lean-evidence-associated-light");

    // ---- the graph records -------------------------------------------------
    const math = await invoke(page, "graph_math_overview");
    const claimRow = math.rows.find((r) => r.entity.label === "Doubling zero gives zero");
    const declRow = math.rows.find((r) => r.entity.key === "LeanBasic.double_zero");
    const evidence = claimRow?.evidence;
    const formal = evidence?.formal;
    note(
      "formal-evidence-exact",
      evidence?.kind === "formalCheck" &&
        evidence.recordedBy.kind === "service" &&
        formal.declaration === "LeanBasic.double_zero" &&
        formal.statement === zero.statement &&
        JSON.stringify(formal.axioms) === JSON.stringify(zero.axioms) &&
        formal.fileVerdict === "checked" &&
        formal.declarationVerdict === "checked" &&
        formal.sourceDigest === audit.contentHash &&
        formal.revision === audit.revision &&
        /leanprover\/lean4:v4\.35\.0/.test(formal.toolchain) &&
        formal.dependencyFingerprints === null &&
        formal.dependencies.kind === "unavailable" &&
        formal.dependencies.reason === "the Lean audit does not report declaration dependencies",
      formal,
    );
    note(
      "informal-correspondence-separate-from-verdict",
      claimRow?.formalAssociation === "reviewed" && claimRow?.verdict === "checked" && claimRow?.freshness?.kind !== "current" && claimRow?.informal === "noProof",
      { association: claimRow?.formalAssociation, verdict: claimRow?.verdict, freshness: claimRow?.freshness, informal: claimRow?.informal },
    );
    note("declaration-entity-anchored", declRow?.binding?.path === DEFS, declRow?.binding);

    // ---- the Mathematics view's IPC: associate an existing declaration entity
    const graph0 = await invoke(page, "graph_status");
    const proposal = await invoke(page, "graph_propose_change", {
      request: {
        baseRevision: graph0.revision,
        operations: [{ op: "createEntity", tempId: "t", kind: "theorem", roles: [], label: "Theorem 1 (paper)", key: null, attributes: {}, binding: null }],
        summary: null,
      },
    });
    const applied = await invoke(page, "graph_apply_change", { request: { proposal: proposal.id, expectedRevision: graph0.revision } });
    const theorem = applied.created[0].id;
    const viaEntity = await invoke(page, "lean_associate_declaration", {
      request: { declaration: { kind: "entity", entity: declRow.entity.id }, target: theorem, note: null },
    });
    note("associate-by-entity-ipc", viaEntity.association.status === "accepted" && viaEntity.evidence === null, viaEntity.association);
    const wrongWay = await invokeError(page, "lean_associate_declaration", {
      request: { declaration: { kind: "entity", entity: theorem }, target: declRow.entity.id, note: null },
    });
    note("only-declarations-formalize", Boolean(wrongWay.error), wrongWay.error);

    // ---- a declaration the audit did not list gets no evidence ------------
    const missing = await invoke(page, "lean_record_evidence", { document: defsDoc, declarations: ["LeanBasic.not_there"] });
    note("unlisted-declaration-refused", missing.recorded.length === 0 && /did not report/.test(missing.refused[0]?.reason ?? ""), missing.refused);

    // ---- historical freshness after an edit --------------------------------
    const before = await invoke(page, "document_state", { document: defsDoc });
    await invoke(page, "document_apply", {
      request: {
        id: `edit-${Date.now()}`,
        origin: { kind: "source" },
        edits: [{ document: defsDoc, baseRevision: before.info.revision, patches: [{ range: { start: before.text.length, end: before.text.length }, text: "\n-- edited after the association\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    await waitFor(
      async () => {
        const s = await panel.getAttribute("data-status");
        const rev = Number(await panel.getAttribute("data-revision"));
        return s === "checked" && rev > before.info.revision;
      },
      { timeout: 300_000, interval: 250, message: "the edited revision checked" },
    );
    await waitFor(async () => (await latest.getAttribute("data-freshness")) === "stale", { timeout: 15_000, message: "evidence becomes history" });
    const history = await row.getByTestId("lean-evidence-history").innerText();
    note("historical-after-edit", /changed since revision/.test(history), history);
    await shot("lean-evidence-history-light");
    await row.getByRole("button", { name: "Record evidence" }).click();
    await waitFor(async () => (await latest.getAttribute("data-freshness")) === "unknown", { timeout: 15_000, message: "new evidence for the new revision" });
    const views = await invoke(page, "lean_declaration_evidence", { document: defsDoc });
    const zeroView = views.find((v) => v.name === "LeanBasic.double_zero");
    note(
      "old-evidence-kept-as-history",
      zeroView.evidence.length === 2 && zeroView.evidence[0].freshness.kind === "unknown" && zeroView.evidence[1].freshness.kind === "stale",
      zeroView.evidence.map((e) => `${e.evidence.formal.revision}:${e.freshness.kind}`),
    );

    // ---- verdict distinctions (Incomplete file; per-declaration verdicts) --
    const mixedDoc = await openInEditor(page, MIXED);
    await waitFor(async () => (await panel.getAttribute("data-status")) === "checkedWithSorry", {
      timeout: 300_000,
      interval: 250,
      message: "Mixed.lean incomplete",
    });
    const audited = panel.getByTestId("lean-audited-declarations");
    await audited.waitFor({ timeout: 15_000 });
    const verdicts = await audited.locator('[data-testid="lean-audited-declaration"]').evaluateAll((els) =>
      Object.fromEntries(els.map((e) => [e.dataset.name, e.dataset.verdict])),
    );
    note(
      "per-declaration-verdicts-preserved",
      verdicts["LeanBasic.clean"] === "checked" && verdicts["LeanBasic.usesAx"] === "checkedWithAxioms" && verdicts["LeanBasic.notDone"] === "incomplete",
      verdicts,
    );
    const recorded = await invoke(page, "lean_record_evidence", {
      document: mixedDoc,
      declarations: ["LeanBasic.clean", "LeanBasic.usesAx", "LeanBasic.notDone"],
    });
    const mixedViews = await invoke(page, "lean_declaration_evidence", { document: mixedDoc });
    const byName = Object.fromEntries(mixedViews.map((v) => [v.name, v.evidence[0]?.evidence.formal]));
    note(
      "file-and-declaration-verdicts-recorded",
      recorded.recorded.length === 3 &&
        byName["LeanBasic.clean"]?.fileVerdict === "incomplete" &&
        byName["LeanBasic.clean"]?.declarationVerdict === "checked" &&
        byName["LeanBasic.usesAx"]?.declarationVerdict === "checkedWithAxioms" &&
        byName["LeanBasic.usesAx"]?.axioms.some((a) => a.includes("myAx")) &&
        byName["LeanBasic.notDone"]?.declarationVerdict === "incomplete",
      Object.fromEntries(Object.entries(byName).map(([k, f]) => [k, `${f?.fileVerdict}/${f?.declarationVerdict} ${f?.axioms?.join(",")}`])),
    );
    await shot("lean-evidence-verdicts-light");
    await runCommand(page, "color theme dark", /Color Theme: Dark/);
    await page.waitForTimeout(300);
    await shot("lean-evidence-verdicts-dark");
    await runCommand(page, "color theme light", /Color Theme: Light/);

    // ---- restricted Lean execution stays refused --------------------------
    const appInfo = await invoke(page, "app_info");
    if (appInfo.debugBuild) {
      await invoke(page, "agents_provider_update", {
        settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
      });
      await invoke(page, "agents_dev_load_script", {
        script: {
          turns: [
            { steps: [{ toolUse: { id: "lean", name: "check_lean", input: { path: DEFS } } }] },
            { steps: [{ text: "The check was refused." }] },
          ],
        },
      });
      const evidenceBefore = (await invoke(page, "lean_declaration_evidence", { document: defsDoc })).reduce((n, v) => n + v.evidence.length, 0);
      const task = await invoke(page, "agents_task_create", {
        request: {
          prompt: "Check Defs.lean",
          workspaceRoot: info.root,
          scope: { kind: "workspace" },
          permission: "autoApply",
          execution: "restricted",
          consent: "scopeContent",
          budgets: null,
        },
      });
      const detail = await waitFor(
        async () => {
          const d = await invoke(page, "agents_task_detail", { task: task.id });
          return ["completed", "failed", "canceled", "stopped"].includes(d.summary.status.kind) ? d : null;
        },
        { timeout: 60_000, message: "restricted agent task finished" },
      );
      const call = detail.transcript.find((t) => t.kind === "toolCall" && t.name === "check_lean");
      const evidenceAfter = (await invoke(page, "lean_declaration_evidence", { document: defsDoc })).reduce((n, v) => n + v.evidence.length, 0);
      note(
        "restricted-lean-refused",
        call?.isError === true && /natively|restricted/i.test(call.result ?? "") && evidenceAfter === evidenceBefore,
        { status: call?.status, result: call?.result?.slice(0, 200) },
      );
    } else {
      note("restricted-lean-refused", true, "SKIPPED in a release build (scripted provider is debug-only); covered by exposition-agents tests");
    }

    const errors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    note("no-console-errors", errors.length === 0, errors.slice(0, 5));
    assert.ok(editorLocator(page, DEFS));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    if (process.env.EXPOSITION_DEV_HOME) {
      const dir = join(process.env.EXPOSITION_DEV_HOME, "ui-reference", "research");
      await mkdir(dir, { recursive: true });
      for (const name of ["lean-evidence-associated-light", "lean-evidence-history-light", "lean-evidence-verdicts-light", "lean-evidence-verdicts-dark"]) {
        const from = join(artifactsDir, `${name}.png`);
        if (existsSync(from)) await cp(from, join(dir, `${name}.png`));
      }
    }
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
