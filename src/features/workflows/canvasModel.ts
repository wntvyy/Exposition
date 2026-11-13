// Pure projection of a workflow draft onto the canvas, and the canvas's
// connection rules. The canvas is a projection of the definition, never a
// whiteboard: positions are presentation only (kept per workflow in
// localStorage, never in the definition); a drawn connection becomes a data
// binding only when the port types are compatible (`compatibleSources`, the
// same rule the form uses); Rust re-validates every edit.
import type { OperationContract } from "../../bindings/OperationContract";
import type { PortSource } from "../../bindings/PortSource";
import type { PortType } from "../../bindings/PortType";
import type { WorkflowDefinition } from "../../bindings/WorkflowDefinition";
import type { WorkflowDiagnostic } from "../../bindings/WorkflowDiagnostic";
import {
  bindingsInto,
  compatibleSources,
  contractOf,
  downstreamOf,
  OUTCOME_ACCEPTS,
  OUTCOME_LABELS,
  PORT_TYPE_LABELS,
  prerequisitesOf,
  setBindings,
  setPrerequisites,
  sourceKey,
} from "./model";

export type CanvasSelectionItem =
  | { kind: "step"; id: string }
  | { kind: "input"; id: string }
  | { kind: "outcome"; id: string }
  | { kind: "edge"; id: string };

export const nodeIdOf = {
  input: (id: string) => `input:${id}`,
  step: (id: string) => `step:${id}`,
  outcome: (id: string) => `outcome:${id}`,
};

export function parseNodeId(nodeId: string): { kind: "input" | "step" | "outcome"; id: string } | null {
  const m = /^(input|step|outcome):(.+)$/.exec(nodeId);
  return m ? { kind: m[1] as "input" | "step" | "outcome", id: m[2]! } : null;
}

/** Handle ids: `in:<port>` / `out:<port>` on steps, `out` on inputs, `in` on outcomes, `pre-in`/`pre-out` for prerequisites. */
export const PRE_IN = "pre-in";
export const PRE_OUT = "pre-out";

export interface WfEdge {
  id: string;
  kind: "data" | "prerequisite" | "annotation" | "outcome";
  source: string;
  sourceHandle: string;
  target: string;
  targetHandle: string;
  label: string;
  /** Accessible description. */
  description: string;
  portType: PortType | null;
  /** Diagnostics referencing this connection. */
  problems: string[];
}

function sourceNodeAndHandle(from: PortSource): { node: string; handle: string } {
  return from.kind === "input" ? { node: nodeIdOf.input(from.input), handle: "out" } : { node: nodeIdOf.step(from.step), handle: `out:${from.port}` };
}

function sourceType(def: WorkflowDefinition, contracts: OperationContract[], from: PortSource): PortType | null {
  if (from.kind === "input") return def.inputs.find((i) => i.id === from.input)?.portType ?? null;
  const step = def.steps.find((s) => s.id === from.step);
  const c = step && contractOf(contracts, step);
  return c?.outputs.find((o) => o.name === from.port)?.types[0] ?? null;
}

function sourceLabel(def: WorkflowDefinition, from: PortSource): string {
  if (from.kind === "input") return def.inputs.find((i) => i.id === from.input)?.label ?? from.input;
  const step = def.steps.find((s) => s.id === from.step);
  return `${step?.label ?? from.step} → ${from.port}`;
}

export function edgesOf(def: WorkflowDefinition, contracts: OperationContract[], diagnostics: WorkflowDiagnostic[]): WfEdge[] {
  const stepLabel = (id: string) => def.steps.find((s) => s.id === id)?.label ?? id;
  const edges: WfEdge[] = [];
  def.connections.forEach((c, index) => {
    const problems = diagnostics.filter((d) => d.connection === index).map((d) => d.message);
    if (c.kind === "data") {
      const s = sourceNodeAndHandle(c.from);
      const type = sourceType(def, contracts, c.from);
      edges.push({
        id: `c${index}`,
        kind: "data",
        source: s.node,
        sourceHandle: s.handle,
        target: nodeIdOf.step(c.to.step),
        targetHandle: `in:${c.to.port}`,
        label: type ? PORT_TYPE_LABELS[type] : "data",
        description: `${sourceLabel(def, c.from)} feeds ${stepLabel(c.to.step)} → ${c.to.port}`,
        portType: type,
        problems,
      });
    } else if (c.kind === "prerequisite") {
      edges.push({
        id: `c${index}`,
        kind: "prerequisite",
        source: nodeIdOf.step(c.before),
        sourceHandle: PRE_OUT,
        target: nodeIdOf.step(c.after),
        targetHandle: PRE_IN,
        label: "runs before (no data)",
        description: `${stepLabel(c.before)} runs before ${stepLabel(c.after)} (prerequisite, no data)`,
        portType: null,
        problems,
      });
    } else {
      edges.push({
        id: `c${index}`,
        kind: "annotation",
        source: nodeIdOf.step(c.from),
        sourceHandle: PRE_OUT,
        target: nodeIdOf.step(c.to),
        targetHandle: PRE_IN,
        label: `annotation (no data)${c.label ? `: ${c.label}` : ""}`,
        description: `Annotation from ${stepLabel(c.from)} to ${stepLabel(c.to)}; it carries no data and schedules nothing`,
        portType: null,
        problems,
      });
    }
  });
  for (const o of def.outcomes) {
    const step = def.steps.find((s) => s.id === o.from.step);
    const c = step && contractOf(contracts, step);
    const type = c?.outputs.find((p) => p.name === o.from.port)?.types[0] ?? null;
    edges.push({
      id: `o:${o.id}`,
      kind: "outcome",
      source: nodeIdOf.step(o.from.step),
      sourceHandle: `out:${o.from.port}`,
      target: nodeIdOf.outcome(o.id),
      targetHandle: "in",
      label: type ? PORT_TYPE_LABELS[type] : "outcome",
      description: `${step?.label ?? o.from.step} → ${o.from.port} realizes the outcome ${o.label}`,
      portType: type,
      problems: diagnostics.filter((d) => d.outcome === o.id).map((d) => d.message),
    });
  }
  return edges;
}

export interface Point {
  x: number;
  y: number;
}

/** Estimated node height (ports stack vertically). */
export function nodeHeight(def: WorkflowDefinition, contracts: OperationContract[], nodeId: string): number {
  const parsed = parseNodeId(nodeId);
  if (parsed?.kind !== "step") return 84;
  const step = def.steps.find((s) => s.id === parsed.id);
  const c = step && contractOf(contracts, step);
  return 84 + 26 * ((c?.inputs.length ?? 0) + (c?.outputs.length ?? 0));
}

export const COLUMN_WIDTH = 380;
const ROW_GAP = 32;

/**
 * Deterministic layered layout by dependency order: inputs in the first
 * column, each step one column right of its deepest producer or
 * prerequisite, outcomes after the last step column.
 */
export function autoLayout(def: WorkflowDefinition, contracts: OperationContract[]): Map<string, Point> {
  const depth = new Map<string, number>();
  const visiting = new Set<string>();
  const depthOf = (id: string): number => {
    const known = depth.get(id);
    if (known !== undefined) return known;
    if (visiting.has(id)) return 1; // cycle: Rust reports it; keep the layout finite
    visiting.add(id);
    let d = 1;
    for (const c of def.connections) {
      if (c.kind === "data" && c.to.step === id && c.from.kind === "step") d = Math.max(d, depthOf(c.from.step) + 1);
      if (c.kind === "prerequisite" && c.after === id) d = Math.max(d, depthOf(c.before) + 1);
    }
    visiting.delete(id);
    depth.set(id, d);
    return d;
  };
  for (const s of def.steps) depthOf(s.id);
  const maxStep = Math.max(0, ...def.steps.map((s) => depth.get(s.id) ?? 1));
  const columns: string[][] = [];
  const put = (col: number, id: string) => {
    (columns[col] ??= []).push(id);
  };
  for (const i of def.inputs) put(0, nodeIdOf.input(i.id));
  for (const s of def.steps) put(depth.get(s.id) ?? 1, nodeIdOf.step(s.id));
  for (const o of def.outcomes) put(maxStep + 1, nodeIdOf.outcome(o.id));
  const positions = new Map<string, Point>();
  columns.forEach((ids, col) => {
    let y = 0;
    for (const id of ids ?? []) {
      positions.set(id, { x: col * COLUMN_WIDTH, y });
      y += nodeHeight(def, contracts, id) + ROW_GAP;
    }
  });
  return positions;
}

export type ConnectResult = { ok: true; draft: WorkflowDefinition; message: string } | { ok: false; message: string };

export interface ConnectRequest {
  source: string;
  sourceHandle: string | null;
  target: string;
  targetHandle: string | null;
}

/**
 * A connection drawn on the canvas. Accepted only when compatible: an
 * output into an input port of the same type family (no cycles, no second
 * source on a single port), a step output into an outcome it can realize,
 * or a prerequisite between two steps (no cycles). Otherwise the reason.
 */
export function connect(def: WorkflowDefinition, contracts: OperationContract[], request: ConnectRequest): ConnectResult {
  // React Flow may report a drag from an input back to an output reversed; normalize.
  let { source, sourceHandle, target, targetHandle } = request;
  if (sourceHandle?.startsWith("in") || sourceHandle === PRE_IN) {
    [source, target] = [target, source];
    [sourceHandle, targetHandle] = [targetHandle, sourceHandle];
  }
  const from = parseNodeId(source);
  const to = parseNodeId(target);
  if (!from || !to || !sourceHandle || !targetHandle) return { ok: false, message: "Refused: connect an output (right side) to an input (left side)." };
  const stepLabel = (id: string) => def.steps.find((s) => s.id === id)?.label ?? id;

  // Prerequisite: step → step, no data.
  if (sourceHandle === PRE_OUT || targetHandle === PRE_IN) {
    if (sourceHandle !== PRE_OUT || targetHandle !== PRE_IN || from.kind !== "step" || to.kind !== "step") {
      return { ok: false, message: "Refused: a prerequisite connects the bottom of one step to the top of another." };
    }
    if (from.id === to.id) return { ok: false, message: "Refused: a step cannot run after itself." };
    if (downstreamOf(def, to.id).has(from.id)) {
      return { ok: false, message: `Refused: “${stepLabel(from.id)}” already runs after “${stepLabel(to.id)}”; this would form a cycle.` };
    }
    const existing = prerequisitesOf(def, to.id);
    if (existing.includes(from.id)) return { ok: false, message: `“${stepLabel(to.id)}” already runs after “${stepLabel(from.id)}”.` };
    return {
      ok: true,
      draft: setPrerequisites(def, to.id, [...existing, from.id]),
      message: `“${stepLabel(to.id)}” now runs after “${stepLabel(from.id)}” (prerequisite, no data).`,
    };
  }

  if (!sourceHandle.startsWith("out")) return { ok: false, message: "Refused: connect an output (right side) to an input (left side)." };
  const portSource: PortSource | null =
    from.kind === "input" ? { kind: "input", input: from.id } : from.kind === "step" ? { kind: "step", step: from.id, port: sourceHandle.slice(4) } : null;
  if (!portSource) return { ok: false, message: "Refused: an outcome has no output." };
  const type = sourceType(def, contracts, portSource);
  const typeLabel = type ? PORT_TYPE_LABELS[type] : "unknown type";

  if (to.kind === "outcome") {
    const outcome = def.outcomes.find((o) => o.id === to.id);
    if (!outcome || portSource.kind !== "step") return { ok: false, message: "Refused: an outcome is realized by a step's output, not by an external input." };
    const accepts = OUTCOME_ACCEPTS[outcome.kind];
    if (!type || !accepts.includes(type)) {
      return {
        ok: false,
        message: `Refused: ${OUTCOME_LABELS[outcome.kind]} needs ${accepts.map((t) => PORT_TYPE_LABELS[t]).join(" or ")}, but “${sourceLabel(def, portSource)}” produces ${typeLabel}.`,
      };
    }
    return {
      ok: true,
      draft: { ...def, outcomes: def.outcomes.map((o) => (o.id === outcome.id ? { ...o, from: { step: portSource.step, port: portSource.port } } : o)) },
      message: `“${outcome.label}” is now realized by ${sourceLabel(def, portSource)}.`,
    };
  }

  if (to.kind !== "step" || !targetHandle.startsWith("in:")) return { ok: false, message: "Refused: connect an output (right side) to an input (left side)." };
  const port = targetHandle.slice(3);
  const step = def.steps.find((s) => s.id === to.id);
  const contract = step && contractOf(contracts, step);
  const spec = contract?.inputs.find((p) => p.name === port);
  if (!step || !spec) return { ok: false, message: "Refused: that input port is not part of the step's operation." };
  if (portSource.kind === "step" && portSource.step === step.id) return { ok: false, message: "Refused: a step cannot feed its own input." };
  const key = sourceKey(portSource);
  const bound = bindingsInto(def, step.id, port);
  if (bound.some((b) => sourceKey(b) === key)) return { ok: false, message: `“${spec.label}” of “${step.label}” is already connected to ${sourceLabel(def, portSource)}.` };
  const options = compatibleSources(def, contracts, step.id, port);
  if (!options.some((o) => o.key === key)) {
    if (portSource.kind === "step" && downstreamOf(def, step.id).has(portSource.step)) {
      return { ok: false, message: `Refused: “${stepLabel(portSource.step)}” runs after “${step.label}”; this connection would form a cycle.` };
    }
    return {
      ok: false,
      message: `Refused: “${spec.label}” of “${step.label}” accepts ${spec.types.map((t) => PORT_TYPE_LABELS[t]).join(" or ")}, but “${sourceLabel(def, portSource)}” is ${typeLabel}.`,
    };
  }
  if (!spec.multiple && bound.length > 0) {
    return {
      ok: false,
      message: `Refused: “${spec.label}” of “${step.label}” takes one source and already has one. Select its connection and press Delete first.`,
    };
  }
  return {
    ok: true,
    draft: setBindings(def, step.id, port, [...bound, portSource]),
    message: `Connected ${sourceLabel(def, portSource)} to “${spec.label}” of “${step.label}”.`,
  };
}

/** Remove a canvas edge from the definition (data, prerequisite, annotation); outcome links are not removable here. */
export function removeEdge(def: WorkflowDefinition, edgeId: string): WorkflowDefinition | null {
  const m = /^c(\d+)$/.exec(edgeId);
  if (!m) return null;
  const index = Number(m[1]);
  if (index < 0 || index >= def.connections.length) return null;
  return { ...def, connections: def.connections.filter((_, i) => i !== index) };
}

/** Diagnostics for one port of a step (by `port`), and the step's others. */
export function portDiagnostics(diagnostics: WorkflowDiagnostic[], step: string, port: string): WorkflowDiagnostic[] {
  return diagnostics.filter((d) => d.step === step && d.port === port);
}

const LAYOUT_PREFIX = "exposition.workflows.layout.";

/** Saved node positions of a workflow (presentation only; never part of the definition). */
export function loadPositions(workflowId: string): Record<string, Point> {
  try {
    const raw = localStorage.getItem(LAYOUT_PREFIX + workflowId);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (typeof parsed !== "object" || parsed === null) return {};
    const out: Record<string, Point> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      const p = v as Partial<Point> | null;
      if (p && Number.isFinite(p.x) && Number.isFinite(p.y)) out[k] = { x: p.x as number, y: p.y as number };
    }
    return out;
  } catch {
    return {};
  }
}

export function savePositions(workflowId: string, positions: Record<string, Point>): void {
  try {
    if (Object.keys(positions).length === 0) localStorage.removeItem(LAYOUT_PREFIX + workflowId);
    else localStorage.setItem(LAYOUT_PREFIX + workflowId, JSON.stringify(positions));
  } catch {
    // Presentation only: losing it re-arranges by dependency order.
  }
}
