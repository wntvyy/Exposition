// P1 project operations — adversarial checks through the IPC directly (spec
// §5 row 11): the backend refuses invalid access regardless of the UI route.
//  - traversal, absolute and junction-escaping paths are refused; nothing
//    outside the project changes;
//  - external paths are only paste sources (never delete/duplicate targets);
//  - protected app infrastructure (.exposition stores) is refused;
//  - a tampered plan hash or an unknown plan applies nothing;
//  - the webview cannot act as an agent task;
//  - ordinary manual deletion needs no execution trust (the folder is not trusted).
import { readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join } from "node:path";

import { consoleErrors, platformSkip } from "../p1-helpers.mjs";
import { invoke, invokeError, makeProject, openWorkspace, waitFor } from "../workspace-helpers.mjs";

const user = { kind: "user" };
const del = (targets, permanent = false) => ({ kind: { kind: "delete", permanent }, targets, actor: user });

export default async function p1Security({ launchApp, artifactsDir, assert, fixture }) {
  const skip = platformSkip();
  if (skip) {
    console.log(`SKIPPED: p1-security ${skip}`);
    return;
  }
  const project = await makeProject(fixture);
  const { root, outside } = project;
  await writeFile(join(root, "victim.txt"), "keep me\n");
  const evidence = [];
  const app = await launchApp({ dataDir: project.dataDir });
  const { page } = app;
  const planOrError = async (request) => {
    const r = await invokeError(page, "project_op_plan", { request });
    return "unexpectedSuccess" in r ? { plan: r.unexpectedSuccess } : { error: r.error };
  };
  const refusedOrRejected = (r, what) => {
    if (r.error) {
      evidence.push(`${what}: rejected ${JSON.stringify(r.error).slice(0, 160)}`);
      return true;
    }
    const ok = r.plan.effects.length === 0 && r.plan.refusals.length > 0;
    evidence.push(`${what}: ${ok ? "refused" : "NOT refused"} ${JSON.stringify(r.plan.refusals.map((x) => x.code))}`);
    return ok;
  };
  try {
    await openWorkspace(page, root);
    const trust = await invoke(page, "trust_status", { root }).catch(() => null);
    evidence.push(`trust: ${JSON.stringify(trust?.kind ?? trust)}`);

    // Traversal / absolute / junction escape as delete targets.
    for (const path of ["../outside/secret.txt", "C:/Windows/win.ini", "escape/secret.txt"]) {
      assert.ok(refusedOrRejected(await planOrError(del([{ kind: "path", path }])), `delete ${path}`), `delete ${path} refused`);
    }
    assert.equal(await readFile(join(outside, "secret.txt"), "utf8"), "outside the workspace\n", "outside file untouched");
    // External paths are paste sources only.
    for (const kind of [del([{ kind: "external", path: join(outside, "secret.txt") }]), { kind: { kind: "duplicate" }, targets: [{ kind: "external", path: join(outside, "secret.txt") }], actor: user }]) {
      assert.ok(refusedOrRejected(await planOrError(kind), `${kind.kind.kind} external`), "external target refused");
    }
    // Paste into a junction that leaves the project, and into protected infrastructure.
    const pasteInto = (folder) => ({ kind: { kind: "paste", destination: { kind: "folder", folder } }, targets: [{ kind: "path", path: "victim.txt" }], actor: user });
    assert.ok(refusedOrRejected(await planOrError(pasteInto("escape")), "paste into escape/"), "paste through a junction refused");
    assert.ok(!existsSync(join(outside, "victim.txt")), "nothing written outside");
    assert.ok(refusedOrRejected(await planOrError(del([{ kind: "path", path: ".exposition/graph.sqlite3" }], true)), "delete graph store"), "protected store refused");
    assert.ok(refusedOrRejected(await planOrError(pasteInto(".exposition")), "paste into .exposition"), "protected destination refused");

    // The junction itself may be removed as a link; its target must never be touched.
    const link = await planOrError(del([{ kind: "path", path: "escape" }]));
    if (link.plan && link.plan.effects.length) {
      evidence.push(`delete the junction itself: ${link.plan.effects[0].description}`);
      const applied = await invoke(page, "project_op_apply", { apply: { plan: link.plan.plan, planHash: link.plan.planHash, choices: {} } });
      evidence.push(`junction delete outcome: ${applied.outcomes[0]?.status} ${applied.outcomes[0]?.code ?? ""}`);
      assert.equal(await readFile(join(outside, "secret.txt"), "utf8"), "outside the workspace\n", "the junction's target is untouched after deleting the link");
    } else {
      evidence.push(`delete the junction itself: refused ${JSON.stringify(link.error ?? link.plan?.refusals)}`);
    }

    // Tampered plan hash / unknown plan.
    const good = await invoke(page, "project_op_plan", { request: del([{ kind: "path", path: "victim.txt" }]) });
    const tampered = await invokeError(page, "project_op_apply", { apply: { plan: good.plan, planHash: `${good.planHash}x`, choices: {} } });
    assert.ok("error" in tampered, "a tampered hash applies nothing");
    evidence.push(`tampered hash: ${JSON.stringify(tampered.error).slice(0, 120)}`);
    const unknown = await invokeError(page, "project_op_apply", { apply: { plan: "plan-does-not-exist", planHash: good.planHash, choices: {} } });
    assert.ok("error" in unknown, "an unknown plan applies nothing");
    assert.ok(existsSync(join(root, "victim.txt")), "victim.txt still there after tampering");

    // The webview cannot act as an agent task.
    const asAgent = await planOrError({ ...del([{ kind: "path", path: "victim.txt" }]), actor: { kind: "agent", task: "task-forged" } });
    const agentRefused = asAgent.error || asAgent.plan.effects.length === 0;
    evidence.push(`agent actor from the webview: ${asAgent.error ? "rejected" : `effects ${asAgent.plan.effects.length}`}`);
    assert.ok(agentRefused, "a forged agent actor is refused");

    // Manual deletion needs no execution trust.
    const plan = await invoke(page, "project_op_plan", { request: del([{ kind: "path", path: "victim.txt" }]) });
    const result = await invoke(page, "project_op_apply", { apply: { plan: plan.plan, planHash: plan.planHash, choices: {} } });
    assert.equal(result.outcomes[0]?.status, "succeeded", "manual deletion works in an untrusted folder");
    await waitFor(async () => !existsSync(join(root, "victim.txt")), { message: "victim.txt deleted" });

    assert.deepEqual(consoleErrors(app), [], "no console errors");
  } finally {
    await writeFile(join(artifactsDir, "evidence.json"), JSON.stringify(evidence, null, 2)).catch(() => {});
    await app.close();
  }
}
