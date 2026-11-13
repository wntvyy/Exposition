// M8: the Formal audit workflow preset, end to end with the real Lean server
// and axiom audit (leanprover/lean4:v4.35.0).
//
// - Untrusted folder: the plan is blocked (Lean is trusted native only).
// - Trusted: the plan names the native step and its privileges; the run
//   executes the existing complete audit of one Lean file; its FormalEvidence
//   output carries the exact per-declaration verdicts (Checked / Checked with
//   axioms / Incomplete) and the file digest; the association step proposes
//   evidence for review and records nothing by itself.
// - Deciding the proposal records FormalCheck evidence only for the accepted
//   declarations, through the Lean service.
// - A customized workflow naming a declaration the audit did not list fails
//   (workflow.declarationNotAudited); nothing is recorded.
//
// Requires Lean through elan with leanprover/lean4:v4.35.0; without it the
// scenario reports SKIPPED unless EXPOSITION_REQUIRE_LEAN=1.
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { cp, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, waitFor } from "../workspace-helpers.mjs";

const TOOLCHAIN_DIR = "leanprover--lean4---v4.35.0";
const MIXED = "LeanBasic/Mixed.lean";
const MIXED_TEXT = `namespace LeanBasic

axiom myAx : 1 = 2

theorem usesAx : 1 = 2 := myAx

theorem clean : 2 = 2 := rfl

theorem notDone : 3 = 3 := by
  sorry

end LeanBasic
`;

export default async function workflowFormal({ launchApp, artifactsDir, assert, fixture }) {
  const elanHome = process.env.ELAN_HOME || join(homedir(), ".elan");
  if (!existsSync(join(elanHome, "toolchains", TOOLCHAIN_DIR, "bin"))) {
    const message = `SKIPPED: Lean toolchain ${TOOLCHAIN_DIR} is not installed through elan`;
    if (process.env.EXPOSITION_REQUIRE_LEAN === "1") throw new Error(message);
    console.log(message);
    return;
  }
  const base = await mkdtemp(join(tmpdir(), "xp-workflow-formal-"));
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
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 600)}`);
  };
  const app = await launchApp({ dataDir });
  const { page } = app;
  const finish = async (runId, timeout = 600_000) => {
    let run;
    await waitFor(async () => {
      run = await invoke(page, "workflow_run_get", { run: runId });
      return run.state !== "running";
    }, { timeout, interval: 500, message: `run ${runId} finishes` });
    return run;
  };
  const attempt = (run, step) => run.steps.find((s) => s.step === step)?.attempts.at(-1);
  const formalChecks = async () => {
    const math = await invoke(page, "graph_math_overview");
    return math.rows
      .filter((r) => r.entity.kind === "formalDeclaration" && r.evidence?.kind === "formalCheck")
      .map((r) => r.entity.label);
  };
  try {
    await page.setViewportSize({ width: 1440, height: 1000 }).catch(() => {});
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"] [data-path="lakefile.toml"]').waitFor({ timeout: 15_000 });

    const preset = await invoke(page, "workflow_from_preset", {
      request: { preset: "formalAudit", script: null, data: null, manuscript: null, leanFile: MIXED },
    });
    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: preset } });
    note("formal-preset-well-formed", saved.validation.state === "wellFormed", saved.validation.diagnostics.map((d) => d.code));

    const untrusted = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    note("untrusted-folder-blocks-lean", untrusted.blockers.some((b) => b.code === "workflow.untrustedFolder"), untrusted.blockers);

    await invoke(page, "trust_grant", { root: info.root });
    const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    const leanSha = createHash("sha256").update(await readFile(join(root, ...MIXED.split("/")))).digest("hex");
    note(
      "plan-names-native-step-and-privileges",
      plan.blockers.length === 0 &&
        plan.execution.nativeSteps.includes("audit") &&
        plan.steps[0].containment === "nativeAccount" &&
        plan.notices.some((n) => /privileges/i.test(n)) &&
        plan.inputs[0]?.sha256 === leanSha,
      { blockers: plan.blockers, execution: plan.execution, notices: plan.notices },
    );
    const before = await formalChecks();
    let run = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
    run = await finish(run.id);
    note("formal-run-succeeded", run.state === "succeeded", { state: run.state, steps: run.steps.map((s) => [s.step, s.state, s.attempts.at(-1)?.code, s.attempts.at(-1)?.message]) });
    const evidence = attempt(run, "audit")?.outputs.find((o) => o.port === "evidence")?.value;
    const verdict = (name) => evidence?.declarations?.find((d) => d.name === `LeanBasic.${name}`)?.verdict ?? "";
    note(
      "audit-output-exact-verdicts",
      evidence?.kind === "formalEvidence" && evidence.fileSha256 === leanSha && /axiom/i.test(verdict("usesAx")) && /^checked$/i.test(verdict("clean")) && !/^checked$/i.test(verdict("notDone")),
      { verdict: evidence?.verdict, declarations: evidence?.declarations?.map((d) => [d.name, d.verdict, d.axioms]) },
    );
    const proposal = attempt(run, "associate")?.outputs[0]?.value;
    const afterRun = await formalChecks();
    note(
      "association-proposed-not-recorded",
      proposal?.kind === "associationProposal" && proposal.status === "pending" && afterRun.length === before.length,
      { proposal, before, afterRun },
    );
    const decided = await invoke(page, "workflow_association_decide", {
      decision: { run: run.id, step: "associate", accept: ["LeanBasic.clean"] },
    });
    const decidedValue = attempt(decided, "associate")?.outputs[0]?.value;
    const afterDecide = await formalChecks();
    note(
      "decision-records-only-accepted-evidence",
      decidedValue?.status === "accepted" && afterDecide.some((l) => /clean/.test(l)) && !afterDecide.some((l) => /usesAx/.test(l)),
      { status: decidedValue?.status, afterDecide },
    );
    await page.screenshot({ path: join(artifactsDir, "workflow-formal-decided.png") }).catch(() => {});

    // Customized: a declaration the audit did not list.
    const custom = structuredClone(saved.definition);
    const assoc = custom.steps.find((s) => s.id === "associate");
    assoc.parameters = { declarations: { kind: "text", value: "LeanBasic.doesNotExist" } };
    custom.title = "Formal audit (customized)";
    const saved2 = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: custom } });
    const plan2 = await invoke(page, "workflow_run_plan", { id: saved2.id, revision: null });
    let run2 = await invoke(page, "workflow_run_start", { request: { plan: plan2.plan, approvalHash: plan2.approvalHash } });
    run2 = await finish(run2.id);
    const a2 = attempt(run2, "associate");
    note(
      "unaudited-declaration-refused",
      run2.state === "failed" && a2?.code === "workflow.declarationNotAudited" && (await formalChecks()).length === afterDecide.length,
      { state: run2.state, code: a2?.code, message: a2?.message },
    );
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
