// Copy and paste of a workflow-canvas selection (P1 spec §2, decision 4).
//
// Copy takes the selected steps and inputs with their registered operation
// configuration (operation, parameters, payload, composite pin, repeat) and
// the connections among them. Paste inserts them into the open DRAFT with new
// ids, remaps every internal connection, offsets positions a little, and
// never runs anything; Save makes the revision, and an incomplete result
// stays a draft until valid.
// - Connections to items outside the selection are kept only when pasting
//   into the same workflow they were copied from and the item still exists
//   there (source-workflow ids are never treated as another workflow's ids);
//   otherwise they are dropped and reported.
// - Outcomes are not copied: copied outputs never attach to existing
//   consumers by themselves.
// - Composite pins (another saved workflow at a revision) stay explicit.
import type { WorkflowConnection } from "../../bindings/WorkflowConnection";
import type { WorkflowInput } from "../../bindings/WorkflowInput";
import type { WorkflowDefinition, WorkflowStep } from "./contract";
import { nodeIdOf, parseNodeId, type Point } from "./canvasModel";

export const FRAGMENT_SCHEMA = "exposition.workflow-fragment/1";

export interface WorkflowFragment {
  schema: typeof FRAGMENT_SCHEMA;
  /** Workflow the selection was copied from. */
  workflow: string;
  steps: WorkflowStep[];
  inputs: WorkflowInput[];
  /** Connections touching the selection (internal ones and external references). */
  connections: WorkflowConnection[];
  /** Node positions (by node id) when copied. */
  positions: Record<string, Point>;
}

export const PASTE_OFFSET: Point = { x: 32, y: 32 };

const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;

/** The fragment for the selected canvas nodes (`step:…`, `input:…`; outcomes are ignored). */
export function copySubgraph(def: WorkflowDefinition, workflowId: string, nodeIds: string[], positions: Record<string, Point>): WorkflowFragment | null {
  const stepIds = new Set<string>();
  const inputIds = new Set<string>();
  for (const nodeId of nodeIds) {
    const parsed = parseNodeId(nodeId);
    if (parsed?.kind === "step") stepIds.add(parsed.id);
    if (parsed?.kind === "input") inputIds.add(parsed.id);
  }
  const steps = def.steps.filter((s) => stepIds.has(s.id));
  const inputs = def.inputs.filter((i) => inputIds.has(i.id));
  if (steps.length === 0 && inputs.length === 0) return null;
  const connections = def.connections.filter((c) => {
    if (c.kind === "data") return stepIds.has(c.to.step);
    if (c.kind === "prerequisite") return stepIds.has(c.after) || stepIds.has(c.before);
    return stepIds.has(c.from) || stepIds.has(c.to);
  });
  const nodePositions: Record<string, Point> = {};
  for (const id of [...[...stepIds].map(nodeIdOf.step), ...[...inputIds].map(nodeIdOf.input)]) {
    const p = positions[id];
    if (p) nodePositions[id] = { ...p };
  }
  return { schema: FRAGMENT_SCHEMA, workflow: workflowId, steps: clone(steps), inputs: clone(inputs), connections: clone(connections), positions: nodePositions };
}

/** A fresh id from `base` that is free in `taken` (ids are `[A-Za-z0-9_-]{1,32}`). */
function freshId(base: string, taken: Set<string>): string {
  const stem = base.replace(/[^A-Za-z0-9_-]/g, "").replace(/-copy\d*$/, "").slice(0, 24) || "item";
  for (let n = 1; ; n++) {
    const id = `${stem}-copy${n === 1 ? "" : n}`;
    if (!taken.has(id)) {
      taken.add(id);
      return id;
    }
  }
}

function freshLabel(label: string, taken: Set<string>): string {
  if (!taken.has(label)) return label;
  for (let n = 1; ; n++) {
    const next = n === 1 ? `${label} (copy)` : `${label} (copy ${n})`;
    if (!taken.has(next)) return next;
  }
}

export interface PasteResult {
  draft: WorkflowDefinition;
  /** New node ids (canvas) of the pasted items. */
  added: string[];
  /** Positions for the pasted nodes (copied positions + offset). */
  positions: Record<string, Point>;
  /** Plain-language notes about what was not carried over. */
  dropped: string[];
  /** Counts for the message. */
  steps: number;
  inputs: number;
}

/** Paste a fragment into `def` (the open draft of `workflowId`). */
export function pasteSubgraph(def: WorkflowDefinition, workflowId: string, fragment: WorkflowFragment, offset: Point = PASTE_OFFSET): PasteResult {
  const sameWorkflow = fragment.workflow === workflowId;
  const stepIdsTaken = new Set(def.steps.map((s) => s.id));
  const inputIdsTaken = new Set(def.inputs.map((i) => i.id));
  const labelsTaken = new Set([...def.steps.map((s) => s.label), ...def.inputs.map((i) => i.label)]);
  const stepMap = new Map<string, string>();
  const inputMap = new Map<string, string>();
  for (const s of fragment.steps) stepMap.set(s.id, freshId(s.id, stepIdsTaken));
  for (const i of fragment.inputs) inputMap.set(i.id, freshId(i.id, inputIdsTaken));
  const dropped: string[] = [];
  const existingStep = (id: string) => sameWorkflow && def.steps.some((s) => s.id === id);
  const existingInput = (id: string) => sameWorkflow && def.inputs.some((i) => i.id === id);
  const label = (id: string) => fragment.steps.find((s) => s.id === id)?.label ?? id;

  // Steps: same configuration, new ids; a condition on an outside step is kept only when that step exists here.
  const steps: WorkflowStep[] = fragment.steps.map((s) => {
    const step: WorkflowStep = { ...clone(s), id: stepMap.get(s.id)!, label: freshLabel(s.label, labelsTaken) };
    labelsTaken.add(step.label);
    if (step.condition) {
      const source = step.condition.source.step;
      if (stepMap.has(source)) step.condition = { ...step.condition, source: { ...step.condition.source, step: stepMap.get(source)! } };
      else if (!existingStep(source)) {
        delete step.condition;
        dropped.push(`The condition of “${s.label}” (it tested a step that was not copied)`);
      }
    }
    return step;
  });
  const inputs: WorkflowInput[] = fragment.inputs.map((i) => {
    const input: WorkflowInput = { ...clone(i), id: inputMap.get(i.id)!, label: freshLabel(i.label, labelsTaken) };
    labelsTaken.add(input.label);
    return input;
  });

  const connections: WorkflowConnection[] = [];
  for (const c of fragment.connections) {
    if (c.kind === "data") {
      const to = stepMap.get(c.to.step);
      if (!to) continue;
      if (c.from.kind === "step") {
        const from = stepMap.get(c.from.step) ?? (existingStep(c.from.step) ? c.from.step : null);
        if (from) connections.push({ kind: "data", from: { kind: "step", step: from, port: c.from.port }, to: { step: to, port: c.to.port } });
        else dropped.push(`The binding of “${label(c.to.step)}” › ${c.to.port} from a step that was not copied`);
      } else {
        const from = inputMap.get(c.from.input) ?? (existingInput(c.from.input) ? c.from.input : null);
        if (from) connections.push({ kind: "data", from: { kind: "input", input: from }, to: { step: to, port: c.to.port } });
        else dropped.push(`The binding of “${label(c.to.step)}” › ${c.to.port} from an input that was not copied`);
      }
    } else if (c.kind === "prerequisite") {
      const before = stepMap.get(c.before) ?? (existingStep(c.before) ? c.before : null);
      const after = stepMap.get(c.after) ?? (existingStep(c.after) ? c.after : null);
      if (before && after && (stepMap.has(c.before) || stepMap.has(c.after))) connections.push({ kind: "prerequisite", before, after });
      else dropped.push(`A “runs before” link of “${label(stepMap.has(c.before) ? c.before : c.after)}” to a step that was not copied`);
    } else {
      const from = stepMap.get(c.from) ?? (existingStep(c.from) ? c.from : null);
      const to = stepMap.get(c.to) ?? (existingStep(c.to) ? c.to : null);
      if (from && to) connections.push({ ...c, from, to });
      else dropped.push(`An annotation of “${label(stepMap.has(c.from) ? c.from : c.to)}” to a step that was not copied`);
    }
  }

  const positions: Record<string, Point> = {};
  const added: string[] = [];
  for (const [old, next] of stepMap) {
    const p = fragment.positions[nodeIdOf.step(old)];
    added.push(nodeIdOf.step(next));
    if (p) positions[nodeIdOf.step(next)] = { x: p.x + offset.x, y: p.y + offset.y };
  }
  for (const [old, next] of inputMap) {
    const p = fragment.positions[nodeIdOf.input(old)];
    added.push(nodeIdOf.input(next));
    if (p) positions[nodeIdOf.input(next)] = { x: p.x + offset.x, y: p.y + offset.y };
  }
  return {
    draft: { ...def, steps: [...def.steps, ...steps], inputs: [...def.inputs, ...inputs], connections: [...def.connections, ...connections] },
    added,
    positions,
    dropped,
    steps: steps.length,
    inputs: inputs.length,
  };
}

/** The message after a paste: what was added, the offset, and what was not carried over. */
export function pasteMessage(result: PasteResult): string {
  const parts: string[] = [];
  if (result.steps) parts.push(`${result.steps} step${result.steps === 1 ? "" : "s"}`);
  if (result.inputs) parts.push(`${result.inputs} input${result.inputs === 1 ? "" : "s"}`);
  const head = `Pasted ${parts.join(" and ")} with new ids, offset slightly. Nothing runs; save to keep the change.`;
  return result.dropped.length ? `${head} Not carried over: ${result.dropped.join("; ")}.` : head;
}
