// Mathlib end to end in the real app, against a real, prepared Mathlib
// package: the editor's language server, the infoview, the Lean check IPC,
// the agent's check_lean, the Formal audit workflow, and formal evidence all
// resolve the same package root, toolchain, and dependencies; the audit
// reports transitive axioms (`sorryAx`, a custom axiom) for a file that
// imports Mathlib; a manifest or source change makes recorded evidence stale.
//
// Opt-in: needs a Mathlib package that is already prepared (several GB, made
// with the app's Set Up Mathlib or with `lake update` + `lake exe cache get`).
// Name it with EXPOSITION_MATHLIB_PACKAGE=<absolute folder>; without it the
// scenario reports SKIPPED. The folder is opened in place: the scenario
// writes `ExpositionMathlibProbe*.lean` files into it and removes them again,
// and it restores `lake-manifest.json` byte for byte after the freshness
// check. It never runs `lake update` and downloads nothing.
//
// Optional: EXPOSITION_MATHLIB_PROJECT=<absolute folder> is an Exposition
// project that contains the package in a subfolder (the package folder must
// then be inside it); the project folder is opened instead of the package.
//
// Requires a debug build: it calls the debug-only probe commands lean_check,
// lean_package_inspect.
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, relative, resolve, sep } from "node:path";

import { runCommand } from "../palette.mjs";
import { invoke, invokeError, openInEditor, waitFor } from "../workspace-helpers.mjs";

const ODD = "ExpositionMathlibProbeOdd.lean";
const MIXED = "ExpositionMathlibProbeMixed.lean";
const ODD_TEXT = `import Mathlib.Algebra.BigOperators.Intervals
import Mathlib.Tactic.Ring

open Finset

/-- The sum of the first \`n\` odd numbers is \`n ^ 2\`. -/
theorem expositionProbe_sum_odd (n : ℕ) : ∑ i ∈ range n, (2 * i + 1) = n ^ 2 := by
  induction n with
  | zero => simp
  | succ n ih =>
    rw [sum_range_succ, ih]
    ring
`;
const MIXED_TEXT = `import Mathlib.Algebra.BigOperators.Intervals
import Mathlib.Tactic.Ring

open Finset

axiom expositionProbe_ax : ∀ n : ℕ, ∑ i ∈ range n, (2 * i + 1) = n ^ 2

theorem expositionProbe_uses_ax (n : ℕ) : ∑ i ∈ range n, (2 * i + 1) = n ^ 2 :=
  expositionProbe_ax n

theorem expositionProbe_not_done (n : ℕ) : ∑ i ∈ range n, (2 * i + 1) = n ^ 2 := by
  sorry

theorem expositionProbe_clean (a b : ℕ) : (a + b) ^ 2 = a ^ 2 + 2 * a * b + b ^ 2 := by
  ring
`;

const seconds = (since) => Math.round((Date.now() - since) / 100) / 10;

export default async function mathlib({ launchApp, artifactsDir, assert }) {
  const packageDir = process.env.EXPOSITION_MATHLIB_PACKAGE ? resolve(process.env.EXPOSITION_MATHLIB_PACKAGE) : null;
  if (!packageDir) {
    console.log("SKIPPED: set EXPOSITION_MATHLIB_PACKAGE to a prepared Mathlib package folder to run this scenario");
    return;
  }
  const root = process.env.EXPOSITION_MATHLIB_PROJECT ? resolve(process.env.EXPOSITION_MATHLIB_PROJECT) : packageDir;
  const folder = relative(root, packageDir).split(sep).join("/");
  assert.ok(!folder.startsWith(".."), "the package folder must be inside the project folder");
  for (const needed of ["lean-toolchain", "lake-manifest.json", ".lake/packages/mathlib"]) {
    assert.ok(existsSync(join(packageDir, needed)), `${packageDir} is not a prepared Mathlib package (${needed} is missing)`);
  }
  const inPackage = (name) => (folder ? `${folder}/${name}` : name);
  const manifestPath = join(packageDir, "lake-manifest.json");
  const manifestBytes = await readFile(manifestPath);
  const manifestDigest = createHash("sha256").update(manifestBytes).digest("hex");
  const pin = (await readFile(join(packageDir, "lean-toolchain"), "utf8")).trim();
  const mathlibRev = JSON.parse(manifestBytes.toString("utf8")).packages.find((p) => p.name === "mathlib")?.rev;
  await writeFile(join(packageDir, ODD), ODD_TEXT);
  await writeFile(join(packageDir, MIXED), MIXED_TEXT);
  const dataDir = join(await mkdtemp(join(tmpdir(), "xp-mathlib-")), "data");
  await mkdir(dataDir, { recursive: true });

  const observations = [];
  const note = (id, ok, detail) => {
    observations.push({ id, ok: !!ok, detail });
    console.log(`${ok ? "ok    " : "FAIL  "} ${id}: ${typeof detail === "string" ? detail : JSON.stringify(detail)?.slice(0, 900)}`);
  };
  const timings = {};
  const app = await launchApp({ dataDir, env: { EXPOSITION_CREDENTIAL_NAMESPACE: `e2e-mathlib-${Date.now()}` } });
  const { page } = app;
  const shot = (name) => page.screenshot({ path: join(artifactsDir, `${name}.png`) }).catch(() => {});
  try {
    await page.setViewportSize({ width: 1440, height: 1000 }).catch(() => {});
    const info = await invoke(page, "workspace_open", { path: root });
    await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
    await invoke(page, "trust_grant", { root: info.root });
    await runCommand(page, "show lean infoview", /Show Lean Infoview/);

    // ---- package context, before anything is checked ------------------------
    const hasSetup = !("error" in (await invokeError(page, "lean_package_inspect", { folder })));
    if (hasSetup) {
      const inspection = await invoke(page, "lean_package_inspect", { folder });
      note(
        "package-inspected-ready",
        inspection.readiness.kind === "ready" &&
          inspection.usesMathlib &&
          inspection.manifestDigest === manifestDigest &&
          inspection.mathlibRevision === mathlibRev &&
          inspection.toolchain === pin &&
          inspection.cachePresent,
        { readiness: inspection.readiness, toolchain: inspection.toolchain, mathlib: inspection.mathlibRevision, input: inspection.mathlibInputRevision },
      );
      // Reopen/rerun: a prepared package needs no setup, and none is offered.
      const again = await invokeError(page, "lean_setup_plan", { request: { kind: "prepareMathlibPackage", folder } });
      note("prepared-package-needs-no-download", again.error?.code === "lean.setup.alreadyPrepared", again.error ?? again);
      const overwrite = await invokeError(page, "lean_setup_plan", { request: { kind: "newMathlibPackage", folder, name: "Formal" } });
      note("existing-package-never-overwritten", overwrite.error?.code === "lean.setup.packageExists", overwrite.error ?? overwrite);
      const nested = await invokeError(page, "lean_setup_plan", {
        request: { kind: "newMathlibPackage", folder: inPackage("ExpositionProbeNested"), name: "Formal" },
      });
      note("nested-package-refused", nested.error?.code === "lean.setup.insidePackage", nested.error ?? nested);
    } else {
      note("package-inspected-ready", true, "SKIPPED: this build has no lean_package_inspect");
    }

    // ---- editor + infoview -----------------------------------------------------
    let started = Date.now();
    const oddDoc = await openInEditor(page, inPackage(ODD));
    const panel = page.getByTestId("lean-panel");
    await waitFor(async () => !["checking", "notStarted"].includes(await panel.getAttribute("data-status")), {
      timeout: 1_500_000,
      interval: 500,
      message: "the Mathlib theorem settles in the editor",
    });
    timings.editorFirstCheckSeconds = seconds(started);
    const oddState = (await invoke(page, "lean_documents")).find((d) => d.document === oddDoc);
    const errors = oddState.diagnostics.filter((d) => d.severity === "error");
    note("editor-checks-mathlib-theorem", oddState.status.kind === "checked" && errors.length === 0, {
      status: oddState.status.kind,
      reason: oddState.status.reason,
      seconds: timings.editorFirstCheckSeconds,
      diagnostics: oddState.diagnostics.map((d) => `${d.severity}: ${d.message.slice(0, 160)}`),
    });
    const sameDir = (a, b) => resolve(a ?? "").toLowerCase() === resolve(b ?? "").toLowerCase();
    note("editor-uses-package-root-and-pinned-toolchain", sameDir(oddState.projectRoot, packageDir) && oddState.toolchain?.name === pin, {
      projectRoot: oddState.projectRoot,
      toolchain: oddState.toolchain,
    });
    const projects = await invoke(page, "lean_projects");
    const project = projects.find((p) => sameDir(p.root, packageDir));
    note("one-lake-project-at-the-package-root", project?.kind.kind === "lake" && (project.path ?? "") === folder, projects.map((p) => [p.path, p.kind, p.server.kind]));
    await shot("mathlib-editor-checked-light");

    // ---- the audit record: declaration, statement, axioms ----------------------
    const audit = await invoke(page, "lean_audit_record", { document: oddDoc });
    const sumOdd = audit?.declarations.find((d) => d.name === "expositionProbe_sum_odd");
    note(
      "audit-reports-declaration-statement-and-standard-axioms",
      sumOdd?.verdict === "checked" &&
        sumOdd.kind === "theorem" &&
        /Finset\.range n/.test(sumOdd.statement) &&
        sumOdd.axioms.length > 0 &&
        sumOdd.axioms.every((a) => ["propext", "Classical.choice", "Quot.sound"].includes(a)),
      { statement: sumOdd?.statement, axioms: sumOdd?.axioms, verdict: sumOdd?.verdict, declarations: audit?.declarationCount },
    );

    // ---- manual check (the Lean check IPC), disk text ---------------------------
    started = Date.now();
    const mixed = await invoke(page, "lean_check", { request: { target: { kind: "path", path: inPackage(MIXED) }, goalsAt: [], timeoutMs: 600_000 } });
    timings.mixedCheckSeconds = seconds(started);
    note(
      "sorry-in-a-mathlib-file-is-never-checked",
      mixed.checked === false && mixed.status.kind === "checkedWithSorry" && sameDir(mixed.projectRoot, packageDir) && mixed.toolchain?.name === pin,
      { status: mixed.status.kind, checked: mixed.checked, seconds: timings.mixedCheckSeconds, projectRoot: mixed.projectRoot },
    );
    const mixedDoc = await openInEditor(page, inPackage(MIXED));
    await waitFor(async () => (await panel.getAttribute("data-status")) === "checkedWithSorry", { timeout: 900_000, interval: 500, message: "the mixed file settles" });
    const mixedAudit = await invoke(page, "lean_audit_record", { document: mixedDoc });
    const verdictOf = (name) => mixedAudit?.declarations.find((d) => d.name === name);
    note(
      "custom-axiom-and-sorry-reported-per-declaration",
      verdictOf("expositionProbe_uses_ax")?.verdict === "checkedWithAxioms" &&
        verdictOf("expositionProbe_uses_ax").axioms.some((a) => a.includes("expositionProbe_ax")) &&
        verdictOf("expositionProbe_not_done")?.verdict === "incomplete" &&
        verdictOf("expositionProbe_not_done").axioms.includes("sorryAx") &&
        verdictOf("expositionProbe_clean")?.verdict === "checked",
      mixedAudit?.declarations.map((d) => [d.name, d.verdict, d.axioms.join(",")]),
    );
    await shot("mathlib-mixed-verdicts-light");

    // ---- agent check_lean (scripted provider replaces only the model) ----------
    const appInfo = await invoke(page, "app_info");
    if (appInfo.debugBuild) {
      await invoke(page, "agents_provider_update", { settings: { provider: "scripted", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" } });
      await invoke(page, "agents_dev_load_script", {
        script: {
          turns: [
            {
              steps: [
                { toolUse: { id: "odd", name: "check_lean", input: { path: inPackage(ODD) } } },
                { toolUse: { id: "mixed", name: "check_lean", input: { path: inPackage(MIXED) } } },
              ],
            },
            { steps: [{ text: "The odd-sums theorem is checked; the mixed file is not." }] },
          ],
        },
      });
      started = Date.now();
      const task = await invoke(page, "agents_task_create", {
        request: { prompt: "Check the Mathlib files", workspaceRoot: info.root, scope: { kind: "workspace" }, permission: "reviewEdits", execution: "approveEach", consent: "scopeContent", budgets: null },
      });
      const approved = [];
      const detail = await waitFor(
        async () => {
          const d = await invoke(page, "agents_task_detail", { task: task.id });
          for (const approval of d.approvals ?? []) {
            if (!approved.includes(approval.id)) {
              approved.push(approval.id);
              await invoke(page, "agents_approval_resolve", { task: task.id, approval: approval.id, approved: true });
            }
          }
          return ["completed", "failed", "canceled", "stopped"].includes(d.summary.status.kind) ? d : null;
        },
        { timeout: 900_000, interval: 500, message: "the agent task finishes" },
      );
      timings.agentChecksSeconds = seconds(started);
      const calls = detail.transcript.filter((t) => t.kind === "toolCall" && t.name === "check_lean");
      const parse = (text) => {
        try {
          return JSON.parse(text ?? "");
        } catch {
          return { unparsed: (text ?? "").slice(0, 300) };
        }
      };
      const [oddResult, mixedResult] = calls.map((c) => parse(c.result));
      note(
        "agent-check-agrees-with-the-editor",
        calls.length === 2 &&
          calls[0].isError !== true &&
          oddResult.checked === true &&
          sameDir(oddResult.projectRoot, packageDir) &&
          oddResult.toolchain?.name === pin &&
          oddResult.contentHash === audit.contentHash &&
          calls[1].isError === true &&
          mixedResult.checked === false &&
          mixedResult.status?.kind === "checkedWithSorry",
        {
          approvals: approved.length,
          seconds: timings.agentChecksSeconds,
          odd: { checked: oddResult.checked, status: oddResult.status?.kind, root: oddResult.projectRoot, note: oddResult.note, unparsed: oddResult.unparsed },
          mixed: { checked: mixedResult.checked, status: mixedResult.status?.kind, note: mixedResult.note, unparsed: mixedResult.unparsed },
        },
      );
    } else {
      note("agent-check-agrees-with-the-editor", true, "SKIPPED in a release build (the scripted provider is debug-only)");
    }

    // ---- workflow: lean.audit + formal.associateEvidence ------------------------
    const preset = await invoke(page, "workflow_from_preset", { request: { preset: "formalAudit", script: null, data: null, manuscript: null, leanFile: inPackage(ODD) } });
    const saved = await invoke(page, "workflow_save", { request: { id: null, expectedRevision: null, definition: preset } });
    const plan = await invoke(page, "workflow_run_plan", { id: saved.id, revision: null });
    note("workflow-plan-has-no-blockers", plan.blockers.length === 0 && plan.execution.nativeSteps.includes("audit"), { blockers: plan.blockers, notices: plan.notices });
    started = Date.now();
    let run = await invoke(page, "workflow_run_start", { request: { plan: plan.plan, approvalHash: plan.approvalHash } });
    await waitFor(
      async () => {
        run = await invoke(page, "workflow_run_get", { run: run.id });
        return run.state !== "running";
      },
      { timeout: 900_000, interval: 500, message: "the workflow run finishes" },
    );
    timings.workflowSeconds = seconds(started);
    const attempt = (step) => run.steps.find((s) => s.step === step)?.attempts.at(-1);
    const value = attempt("audit")?.outputs.find((o) => o.port === "evidence")?.value;
    const wfDecl = value?.declarations?.find((d) => d.name === "expositionProbe_sum_odd");
    const environment = attempt("audit")?.environment;
    note(
      "workflow-audit-agrees-with-the-editor",
      run.state === "succeeded" && value?.kind === "formalEvidence" && value.fileSha256 === audit.contentHash && /^checked$/i.test(wfDecl?.verdict ?? "") && wfDecl.statement === sumOdd.statement,
      { state: run.state, seconds: timings.workflowSeconds, verdict: value?.verdict, decl: wfDecl, steps: run.steps.map((s) => [s.step, s.state, s.attempts.at(-1)?.code, s.attempts.at(-1)?.message]) },
    );
    note(
      "workflow-records-toolchain-and-manifest-digest",
      environment?.toolchain?.name === pin && environment.toolchain.manifestDigest === manifestDigest && JSON.stringify(environment).includes(mathlibRev),
      environment,
    );
    const decided = await invoke(page, "workflow_association_decide", { decision: { run: run.id, step: "associate", accept: ["expositionProbe_sum_odd"] } });
    const decidedValue = decided.steps.find((s) => s.step === "associate")?.attempts.at(-1)?.outputs[0]?.value;
    note("workflow-association-accepted-records-evidence", decidedValue?.status === "accepted", decidedValue);

    // ---- evidence: exact declaration, verdict, axioms, location, identity ------
    const views = await invoke(page, "lean_declaration_evidence", { document: oddDoc });
    const view = views.find((v) => v.name === "expositionProbe_sum_odd");
    const evidence = view?.evidence[0]?.evidence;
    const formal = evidence?.formal;
    const environmentPairs = Object.fromEntries(evidence?.environment ?? []);
    note(
      "evidence-exact",
      evidence?.kind === "formalCheck" &&
        evidence.recordedBy.kind === "service" &&
        formal.declaration === "expositionProbe_sum_odd" &&
        formal.statement === sumOdd.statement &&
        formal.declarationVerdict === "checked" &&
        formal.sourcePath === inPackage(ODD) &&
        formal.sourceDigest === audit.contentHash &&
        formal.toolchain === pin &&
        JSON.stringify(formal.axioms) === JSON.stringify(sumOdd.axioms),
      { formal, environment: evidence?.environment, inputs: evidence?.inputs },
    );
    note(
      "evidence-names-the-dependency-identity",
      environmentPairs["lake-manifest.json sha256"] === manifestDigest && environmentPairs["mathlib revision"] === mathlibRev,
      evidence?.environment,
    );
    note("evidence-freshness-never-current-on-compile-alone", view?.evidence[0]?.freshness.kind !== "current", view?.evidence[0]?.freshness);

    // ---- the graph: declarations of the file, not Mathlib's ---------------------
    const math = await invoke(page, "graph_math_overview");
    const formalRows = math.rows.filter((r) => r.entity.kind === "formalDeclaration");
    note(
      "graph-holds-only-the-project-declarations",
      formalRows.some((r) => r.entity.key === "expositionProbe_sum_odd" && r.verdict === "checked") && formalRows.length < 50,
      { formalDeclarations: formalRows.length, names: formalRows.map((r) => r.entity.key).slice(0, 12) },
    );

    // ---- a dependency change makes the evidence stale ---------------------------
    // The manifest gets one trailing newline (same pins, another digest), then
    // is restored byte for byte; nothing is resolved or downloaded.
    try {
      await writeFile(manifestPath, Buffer.concat([manifestBytes, Buffer.from("\n")]));
      const stale = await waitFor(
        async () => {
          const now = (await invoke(page, "lean_declaration_evidence", { document: oddDoc })).find((v) => v.name === "expositionProbe_sum_odd");
          return now?.evidence[0]?.freshness.kind === "stale" ? now.evidence[0].freshness : null;
        },
        { timeout: 20_000, interval: 500, message: "evidence becomes stale after the manifest changed" },
      ).catch((e) => ({ error: e.message }));
      note("manifest-change-makes-evidence-stale", Array.isArray(stale.reasons) && stale.reasons.some((r) => /lake-manifest\.json/.test(r)), stale);
    } finally {
      await writeFile(manifestPath, manifestBytes);
    }
    const restored = await waitFor(
      async () => {
        const now = (await invoke(page, "lean_declaration_evidence", { document: oddDoc })).find((v) => v.name === "expositionProbe_sum_odd");
        return now?.evidence[0]?.freshness.kind !== "stale" ? now.evidence[0].freshness : null;
      },
      { timeout: 20_000, interval: 500, message: "evidence is not stale once the manifest is restored" },
    ).catch((e) => ({ error: e.message }));
    note("restored-manifest-is-not-stale", !restored.error, restored);

    // ---- a source change makes the evidence history ------------------------------
    const before = await invoke(page, "document_state", { document: oddDoc });
    await invoke(page, "document_apply", {
      request: {
        id: `edit-${Date.now()}`,
        origin: { kind: "source" },
        edits: [{ document: oddDoc, baseRevision: before.info.revision, patches: [{ range: { start: before.text.length, end: before.text.length }, text: "\n-- edited after the evidence was recorded\n" }] }],
        undo: { kind: "newGroup" },
        conflict: "reject",
        label: null,
        selection: null,
      },
    });
    const afterEdit = await waitFor(
      async () => {
        const now = (await invoke(page, "lean_declaration_evidence", { document: oddDoc })).find((v) => v.name === "expositionProbe_sum_odd");
        return now?.evidence[0]?.freshness.kind === "stale" ? now.evidence[0].freshness : null;
      },
      { timeout: 20_000, interval: 500, message: "evidence becomes history after the edit" },
    ).catch((e) => ({ error: e.message }));
    note("source-change-makes-evidence-stale", Array.isArray(afterEdit.reasons), afterEdit);
    // The unsaved buffer is what the editor checks now (buffer, not disk).
    started = Date.now();
    await waitFor(
      async () => {
        const s = (await invoke(page, "lean_documents")).find((d) => d.document === oddDoc);
        return s.status.kind === "checked" && s.status.revision > before.info.revision;
      },
      { timeout: 900_000, interval: 500, message: "the edited buffer is checked" },
    );
    timings.recheckAfterEditSeconds = seconds(started);
    note("unsaved-buffer-rechecked", true, { seconds: timings.recheckAfterEditSeconds });

    note("timings", true, timings);
    const consoleErrors = app.consoleMessages.filter((m) => m.startsWith("error") || m.startsWith("pageerror"));
    note("no-console-errors", consoleErrors.length === 0, consoleErrors.slice(0, 5));
  } finally {
    await writeFile(join(artifactsDir, "observations.json"), JSON.stringify(observations, null, 2));
    await app.close();
    await writeFile(manifestPath, manifestBytes).catch(() => {});
    for (const name of [ODD, MIXED]) await rm(join(packageDir, name), { force: true });
  }
  const failed = observations.filter((o) => !o.ok);
  assert.deepEqual(failed.map((d) => d.id), [], `violations (see ${join(artifactsDir, "observations.json")})`);
}
