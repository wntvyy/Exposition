// Workflow-canvas subgraph copy/paste: internal ids and edges remapped,
// external references kept only within the same workflow, outcomes never
// copied, positions offset, and nothing run.
import { describe, expect, it } from "vitest";

import type { WorkflowDefinition } from "./contract";
import { copySubgraph, pasteMessage, pasteSubgraph } from "./subgraph";

const step = (id: string, label: string, extra: object = {}) => ({ id, label, operation: { id: "python.batch", version: 1 }, parameters: { n: { kind: "integer", value: 3 } }, payload: null, ...extra });

const def = {
  schema: "exposition.workflow/2",
  title: "Fit",
  description: "",
  preset: null,
  inputs: [{ id: "data", label: "Data", portType: "dataset", source: { kind: "resource", path: "data/obs.csv", version: { kind: "current" } } }],
  steps: [step("load", "Load"), step("fit", "Fit", { condition: { source: { step: "load", port: "table" }, test: { kind: "outputPresent" }, negate: false } }), step("plot", "Plot")],
  connections: [
    { kind: "data", from: { kind: "input", input: "data" }, to: { step: "load", port: "source" } },
    { kind: "data", from: { kind: "step", step: "load", port: "table" }, to: { step: "fit", port: "table" } },
    { kind: "data", from: { kind: "step", step: "fit", port: "result" }, to: { step: "plot", port: "values" } },
    { kind: "prerequisite", before: "load", after: "plot" },
  ],
  outcomes: [{ id: "fig", kind: "figure", label: "Figure", from: { step: "plot", port: "figure" }, optional: false }],
  ceiling: { kind: "restricted" },
} as unknown as WorkflowDefinition;

const positions = { "step:load": { x: 0, y: 0 }, "step:fit": { x: 200, y: 0 }, "step:plot": { x: 400, y: 0 }, "input:data": { x: -200, y: 0 } };

describe("copy and paste of a canvas selection", () => {
  it("remaps internal ids, edges and conditions; offsets positions; never copies outcomes", () => {
    const fragment = copySubgraph(def, "wf-1", ["step:load", "step:fit", "outcome:fig"], positions)!;
    expect(fragment.steps.map((s) => s.id)).toEqual(["load", "fit"]);
    const pasted = pasteSubgraph(def, "wf-other", fragment);
    const newSteps = pasted.draft.steps.slice(3);
    expect(newSteps.map((s) => s.id)).toEqual(["load-copy", "fit-copy"]);
    expect(newSteps.map((s) => s.label)).toEqual(["Load (copy)", "Fit (copy)"]);
    // Configuration copied as-is.
    expect(newSteps[0]!.parameters).toEqual(def.steps[0]!.parameters);
    // Internal edge and condition remapped.
    expect(pasted.draft.connections).toContainEqual({ kind: "data", from: { kind: "step", step: "load-copy", port: "table" }, to: { step: "fit-copy", port: "table" } });
    expect(newSteps[1]!.condition?.source.step).toBe("load-copy");
    // Into another workflow, the binding from the outside input is dropped and reported (ids are not shared across workflows).
    expect(pasted.draft.connections.filter((c) => c.kind === "data" && c.to.step === "load-copy")).toEqual([]);
    expect(pasted.dropped.some((d) => d.includes("input that was not copied"))).toBe(true);
    // Outcomes untouched; originals untouched.
    expect(pasted.draft.outcomes).toEqual(def.outcomes);
    expect(pasted.draft.steps.slice(0, 3)).toEqual(def.steps);
    expect(pasted.positions).toEqual({ "step:load-copy": { x: 32, y: 32 }, "step:fit-copy": { x: 232, y: 32 } });
    expect(pasteMessage(pasted)).toMatch(/^Pasted 2 steps with new ids, offset slightly\. Nothing runs/);
  });

  it("within the same workflow, external references that still exist are kept explicitly", () => {
    const fragment = copySubgraph(def, "wf-1", ["step:fit", "step:plot"], positions)!;
    const pasted = pasteSubgraph(def, "wf-1", fragment);
    // fit's binding from `load` (outside the selection, present here) stays; the condition on load stays.
    expect(pasted.draft.connections).toContainEqual({ kind: "data", from: { kind: "step", step: "load", port: "table" }, to: { step: "fit-copy", port: "table" } });
    expect(pasted.draft.steps.find((s) => s.id === "fit-copy")?.condition?.source.step).toBe("load");
    // The prerequisite load → plot is kept for the copy too; internal fit → plot remapped.
    expect(pasted.draft.connections).toContainEqual({ kind: "prerequisite", before: "load", after: "plot-copy" });
    expect(pasted.draft.connections).toContainEqual({ kind: "data", from: { kind: "step", step: "fit-copy", port: "result" }, to: { step: "plot-copy", port: "values" } });
    expect(pasted.dropped).toEqual([]);
  });

  it("repeated paste allocates fresh ids each time", () => {
    const fragment = copySubgraph(def, "wf-1", ["step:plot"], positions)!;
    const once = pasteSubgraph(def, "wf-1", fragment).draft;
    const twice = pasteSubgraph(once, "wf-1", fragment).draft;
    expect(twice.steps.map((s) => s.id)).toEqual(["load", "fit", "plot", "plot-copy", "plot-copy2"]);
    expect(new Set(twice.steps.map((s) => s.label)).size).toBe(5);
  });

  it("nothing selected (or only outcomes) copies nothing", () => {
    expect(copySubgraph(def, "wf-1", ["outcome:fig"], positions)).toBeNull();
  });
});
