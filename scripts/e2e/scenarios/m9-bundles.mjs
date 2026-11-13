// M9 composites and declarative interchange against the built app (real IPC).
//
// 1. Project A: a reusable child workflow and a schema /2 parent that runs it
//    at a pinned revision as a composite step. The registry lists
//    workflow.composite; validation expands the child (plan step "sub/fit"),
//    and editing the child makes revision 2 without changing the parent.
//    Recursion (a new revision including itself) is reported.
// 2. Export the parent as a workflow bundle to a .json file the user chose:
//    root + child with their ids/revisions, definitions only (no trust,
//    approvals, triggers, validation, or origin). Locations inside the
//    project's .exposition records or a non-.json name are refused.
// 3. Hostile bundles are refused and save nothing: a trust/approval/trigger
//    claim anywhere, a key smuggled into a tagged unit variant, a child that
//    runs the root (recursion), an extra unused child, an oversized file.
// 4. Project B: importing the bundle saves the child first and the parent with
//    its reference rewritten to the new child id/revision; both are imported
//    drafts with fresh ids; nothing runs.
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { invoke, invokeError } from "../workspace-helpers.mjs";

const CODE = `import os
open(os.path.join(os.environ["EXPOSITION_OUTPUT_DIR"], "dataset.csv"), "w").write("x\\n1\\n")
`;

function child(code = CODE) {
  return {
    schema: "exposition.workflow/1",
    title: "Fit",
    description: "A reusable fit.",
    preset: null,
    inputs: [{ id: "data", label: "Data", portType: "dataset", source: { kind: "unset" } }],
    steps: [
      {
        id: "fit",
        label: "Fit the data",
        operation: { id: "research.pythonBatch", version: 1 },
        parameters: {},
        payload: { kind: "code", language: "python", source: code },
      },
    ],
    connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "fit", port: "data" } }],
    outcomes: [{ id: "fitted", kind: "transformedDataset", label: "Fitted", from: { step: "fit", port: "dataset" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
  };
}

function parent(childId, revision) {
  return {
    schema: "exposition.workflow/2",
    title: "Parent",
    description: "",
    preset: null,
    inputs: [{ id: "data", label: "Observations", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "atRunStart" } } }],
    steps: [
      {
        id: "sub",
        label: "Run the fit",
        operation: { id: "workflow.composite", version: 1 },
        parameters: {},
        payload: null,
        composite: { workflow: childId, revision },
      },
    ],
    connections: [{ kind: "data", from: { kind: "input", input: "data" }, to: { step: "sub", port: "data" } }],
    outcomes: [{ id: "out", kind: "transformedDataset", label: "Result", from: { step: "sub", port: "fitted" }, optional: false }],
    ceiling: { execution: "restricted", modelRequests: false, retainResults: false, proposeChanges: false },
    execution: { maxParallel: 2 },
  };
}

export default async function m9Bundles({ launchApp, assert }) {
  const base = await mkdtemp(join(tmpdir(), "xp-m9-bundles-"));
  const rootA = join(base, "a");
  const rootB = join(base, "b");
  const out = join(base, "out");
  const dataDir = join(base, "data");
  for (const dir of [join(rootA, "data"), join(rootB, "data"), out, dataDir]) await mkdir(dir, { recursive: true });
  for (const root of [rootA, rootB]) await writeFile(join(root, "data", "obs.csv"), "x\n1\n2\n");

  const app = await launchApp({ dataDir });
  const page = app.page;
  const call = (command, args) => invoke(page, command, args);
  const log = (step, detail) => console.log(`ok: ${step}${detail ? ` — ${JSON.stringify(detail).slice(0, 300)}` : ""}`);
  const save = (definition, id = null, expectedRevision = null) =>
    call("workflow_save", { request: { id, expectedRevision, definition } });

  // ---- 1. composites -------------------------------------------------------
  await call("workspace_open", { path: rootA });
  await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
  const operations = await call("workflow_operations");
  const composite = operations.find((o) => o.id === "workflow.composite");
  assert.ok(composite, "workflow.composite is registered");
  assert.equal(composite.runner.available, true);
  assert.deepEqual([composite.inputs, composite.outputs, composite.effects], [[], [], []]);
  log("the registry lists workflow.composite", composite.runner.availability);

  const c = await save(child());
  const p = await save(parent(c.id, 1));
  assert.equal(p.validation.state, "wellFormed", JSON.stringify(p.validation.diagnostics));
  assert.deepEqual(p.validation.plan.map((s) => s.step), ["sub/fit"]);
  assert.deepEqual(p.validation.plan[0].compositePath, ["Run the fit"]);
  assert.equal(p.validation.plan[0].runnerId, "restrictedPython");
  const validated = await call("workflow_validate", { definition: parent(c.id, 1) });
  assert.equal(validated.state, "wellFormed");
  assert.ok(!validated.diagnostics.some((d) => d.code === "workflow.compositeUnchecked"), "the editor's validation resolves composites");
  log("parent with a pinned composite is well-formed and expanded", p.validation.plan[0]);

  const c2 = await save(child("print(2)\n"), c.id, 1);
  assert.equal(c2.revision, 2);
  const pNow = await call("workflow_get", { id: p.id, revision: null });
  assert.equal(pNow.revision, 1);
  assert.deepEqual(pNow.definition.steps[0].composite, { workflow: c.id, revision: 1 });
  log("editing the child makes revision 2; the parent keeps revision 1");

  const selfish = await call("workflow_validate", { definition: parent(p.id, 1), id: p.id });
  assert.ok(selfish.diagnostics.some((d) => d.code === "workflow.compositeRecursive"), JSON.stringify(selfish.diagnostics));
  const deep = await call("workflow_validate", { definition: { ...parent(c.id, 1), execution: { maxParallel: 9 } } });
  assert.ok(deep.diagnostics.some((d) => d.code === "workflow.maxParallel"));
  log("recursion and maxParallel bounds are reported");

  // ---- 2. export -----------------------------------------------------------
  const target = join(out, "parent.json");
  const exported = await call("workflow_bundle_export", { id: p.id, revision: null, path: target });
  assert.equal(exported.children, 1);
  const text = await readFile(target, "utf8");
  const bundle = JSON.parse(text);
  assert.deepEqual(Object.keys(bundle).sort(), ["children", "root", "schema"]);
  assert.equal(bundle.schema, "exposition.workflow-bundle/1");
  assert.deepEqual([bundle.root.id, bundle.root.revision], [p.id, 1]);
  assert.deepEqual([bundle.children[0].id, bundle.children[0].revision], [c.id, 1]);
  for (const word of ["trust", "approval", "trigger", "runnable", "diagnostics", "origin"]) {
    assert.ok(!text.includes(word), `${word} in the bundle`);
  }
  log("exported a bundle with its pinned child", { bytes: exported.bytes });
  for (const bad of [join(rootA, ".exposition", "x.json"), join(out, "parent.txt"), "parent.json", join(out, "missing", "p.json")]) {
    const refused = await invokeError(page, "workflow_bundle_export", { id: p.id, revision: null, path: bad });
    assert.equal(refused.error?.code, "workflow.file", `${bad}: ${JSON.stringify(refused)}`);
  }
  log("export locations are checked");

  // ---- 3. hostile bundles --------------------------------------------------
  const listA = (await call("workflow_list")).length;
  const hostile = async (name, mutate) => {
    const b = JSON.parse(text);
    mutate(b);
    const path = join(out, `${name}.json`);
    await writeFile(path, typeof b === "string" ? b : JSON.stringify(b));
    const refused = await invokeError(page, "workflow_bundle_import", { path });
    assert.equal(refused.error?.code, "workflow.bundleInvalid", `${name}: ${JSON.stringify(refused)}`);
    assert.equal((await call("workflow_list")).length, listA, `${name}: nothing saved`);
    return refused.error.message;
  };
  await hostile("trust", (b) => (b.trust = { granted: true }));
  await hostile("approval", (b) => (b.root.approval = { hash: "0".repeat(64) }));
  await hostile("trigger", (b) => (b.children[0].definition.triggers = [{ kind: "interval", minutes: 15 }]));
  await hostile("step-approved", (b) => (b.root.definition.steps[0].approved = true));
  await hostile("unit-variant", (b) => (b.children[0].definition.inputs[0].source = { kind: "unset", trust: true }));
  const recursive = await hostile("recursion", (b) => (b.children[0].definition = { ...parent(b.root.id, 1), title: "Loop" }));
  assert.ok(recursive.includes("itself"), recursive);
  await hostile("unused", (b) => b.children.push({ ...b.children[0], id: "wf-0123456789abcdef0123456789abcdef" }));
  const huge = join(out, "huge.json");
  await writeFile(huge, `{"schema":"exposition.workflow-bundle/1","pad":"${"x".repeat(9 * 1024 * 1024)}"}`);
  const tooBig = await invokeError(page, "workflow_bundle_import", { path: huge });
  assert.equal(tooBig.error?.code, "workflow.bundleInvalid", JSON.stringify(tooBig).slice(0, 300));
  log("hostile bundles are refused and save nothing");

  // ---- 4. import into another project --------------------------------------
  await call("workspace_open", { path: rootB });
  await page.locator('[data-testid="file-tree"]').waitFor({ timeout: 15_000 });
  assert.equal((await call("workflow_list")).length, 0);
  const imported = await call("workflow_bundle_import", { path: target });
  assert.equal(imported.children.length, 1);
  const [ic] = imported.children;
  assert.deepEqual(ic.original, { workflow: c.id, revision: 1 });
  assert.notEqual(ic.saved.workflow, c.id);
  assert.notEqual(imported.root.id, p.id);
  assert.deepEqual(imported.root.definition.steps[0].composite, ic.saved);
  assert.equal(imported.root.imported, true);
  assert.equal(imported.root.validation.state, "wellFormed", JSON.stringify(imported.root.validation.diagnostics));
  assert.deepEqual(imported.root.validation.plan.map((s) => s.step), ["sub/fit"]);
  const listB = await call("workflow_list");
  assert.equal(listB.length, 2);
  assert.ok(listB.every((w) => w.imported), "imported drafts");
  for (const w of listB) assert.deepEqual(await call("workflow_runs", { workflow: w.id }), [], "nothing ran");
  log("imported into another project: child first, reference rewritten, drafts only", ic);
}
