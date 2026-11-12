// Pure edits of a workflow draft for the form editor. The editor offers only
// type-compatible choices, but Rust remains the validator: every edit is
// re-validated there and saved drafts carry its diagnostics.
import type { OperationContract } from "../../bindings/OperationContract";
import type { OutcomeKind } from "../../bindings/OutcomeKind";
import type { PortSource } from "../../bindings/PortSource";
import type { PortType } from "../../bindings/PortType";
import type { WorkflowConnection } from "../../bindings/WorkflowConnection";
import type { WorkflowDefinition } from "../../bindings/WorkflowDefinition";
import type { WorkflowDiagnostic } from "../../bindings/WorkflowDiagnostic";
import type { WorkflowStep } from "../../bindings/WorkflowStep";

export const WORKFLOW_KEY_PREFIX = "workflow:";

export const workflowKey = (id: string) => `${WORKFLOW_KEY_PREFIX}${id}`;

/** Mirrors the backend's outcome rule for building choices (display only). */
export const OUTCOME_ACCEPTS: Record<OutcomeKind, PortType[]> = {
  transformedDataset: ["dataset", "retainedArtifact"],
  retainedFigure: ["retainedArtifact"],
  retainedTable: ["retainedArtifact"],
  investigationReport: ["reviewReport"],
  auditEvidence: ["formalEvidence"],
  reviewableProposal: ["associationProposal", "promotionProposal"],
  manuscriptPromotion: ["promotionProposal"],
};

export const OUTCOME_LABELS: Record<OutcomeKind, string> = {
  transformedDataset: "Transformed dataset",
  retainedFigure: "Retained figure",
  retainedTable: "Retained table",
  investigationReport: "Investigation report",
  auditEvidence: "Audit evidence",
  reviewableProposal: "Reviewable proposal",
  manuscriptPromotion: "Manuscript promotion",
};

export const PORT_TYPE_LABELS: Record<PortType, string> = {
  codeSource: "Code",
  dataset: "Dataset",
  figure: "Figure",
  table: "Table",
  document: "Document",
  formalSource: "Lean source",
  formalEvidence: "Formal evidence",
  reviewReport: "Review report",
  retainedArtifact: "Retained artifact",
  promotionProposal: "Promotion proposal",
  associationProposal: "Association proposal",
};

/** Types an external input (a project file) may have. */
export const FILE_INPUT_TYPES: PortType[] = ["codeSource", "dataset", "figure", "table", "document", "formalSource"];

export function contractOf(contracts: OperationContract[], step: WorkflowStep): OperationContract | undefined {
  return contracts.find((c) => c.id === step.operation.id && c.version === step.operation.version);
}

export const sourceKey = (s: PortSource) => (s.kind === "step" ? `step:${s.step}.${s.port}` : `input:${s.input}`);

export function parseSourceKey(key: string): PortSource | null {
  const step = /^step:([^.]+)\.(.+)$/.exec(key);
  if (step?.[1] && step[2]) return { kind: "step", step: step[1], port: step[2] };
  const input = /^input:(.+)$/.exec(key);
  return input?.[1] ? { kind: "input", input: input[1] } : null;
}

/** Steps reachable from `stepId` along data and prerequisite connections. */
export function downstreamOf(def: WorkflowDefinition, stepId: string): Set<string> {
  const out = new Set<string>();
  const queue = [stepId];
  while (queue.length) {
    const current = queue.shift() as string;
    for (const c of def.connections) {
      const next =
        c.kind === "data" && c.from.kind === "step" && c.from.step === current
          ? c.to.step
          : c.kind === "prerequisite" && c.before === current
            ? c.after
            : null;
      if (next && !out.has(next)) {
        out.add(next);
        queue.push(next);
      }
    }
  }
  return out;
}

export interface SourceOption {
  key: string;
  label: string;
  source: PortSource;
  type: PortType;
}

/**
 * Compatible sources for a step's input port: external inputs and other
 * steps' outputs whose type the port accepts, excluding the step itself and
 * anything downstream of it (which would form a cycle).
 */
export function compatibleSources(
  def: WorkflowDefinition,
  contracts: OperationContract[],
  stepId: string,
  port: string,
): SourceOption[] {
  const step = def.steps.find((s) => s.id === stepId);
  const contract = step && contractOf(contracts, step);
  const accepts = contract?.inputs.find((p) => p.name === port)?.types ?? [];
  const downstream = downstreamOf(def, stepId);
  const options: SourceOption[] = [];
  for (const input of def.inputs) {
    if (accepts.includes(input.portType)) {
      const source: PortSource = { kind: "input", input: input.id };
      options.push({ key: sourceKey(source), label: `Input: ${input.label}`, source, type: input.portType });
    }
  }
  for (const other of def.steps) {
    if (other.id === stepId || downstream.has(other.id)) continue;
    const c = contractOf(contracts, other);
    for (const out of c?.outputs ?? []) {
      const type = out.types[0];
      if (type && accepts.includes(type)) {
        const source: PortSource = { kind: "step", step: other.id, port: out.name };
        options.push({ key: sourceKey(source), label: `${other.label} → ${out.label}`, source, type });
      }
    }
  }
  return options;
}

export function bindingsInto(def: WorkflowDefinition, stepId: string, port: string): PortSource[] {
  return def.connections.flatMap((c) => (c.kind === "data" && c.to.step === stepId && c.to.port === port ? [c.from] : []));
}

export function setBindings(def: WorkflowDefinition, stepId: string, port: string, sources: PortSource[]): WorkflowDefinition {
  const kept = def.connections.filter((c) => !(c.kind === "data" && c.to.step === stepId && c.to.port === port));
  const added: WorkflowConnection[] = sources.map((from) => ({ kind: "data", from, to: { step: stepId, port } }));
  return { ...def, connections: [...kept, ...added] };
}

export function prerequisitesOf(def: WorkflowDefinition, stepId: string): string[] {
  return def.connections.flatMap((c) => (c.kind === "prerequisite" && c.after === stepId ? [c.before] : []));
}

export function setPrerequisites(def: WorkflowDefinition, stepId: string, before: string[]): WorkflowDefinition {
  const kept = def.connections.filter((c) => !(c.kind === "prerequisite" && c.after === stepId));
  return { ...def, connections: [...kept, ...before.map((b) => ({ kind: "prerequisite" as const, before: b, after: stepId }))] };
}

function uniqueId(prefix: string, taken: Set<string>): string {
  for (let n = 1; ; n++) {
    const id = `${prefix}${n}`;
    if (!taken.has(id)) return id;
  }
}

export function addStep(def: WorkflowDefinition, contract: OperationContract): WorkflowDefinition {
  const id = uniqueId("step", new Set(def.steps.map((s) => s.id)));
  const payload = contract.payload?.kind === "prompt" ? { kind: "prompt" as const, text: "" } : null;
  const step: WorkflowStep = {
    id,
    label: contract.title,
    operation: { id: contract.id, version: contract.version },
    parameters: {},
    payload,
  };
  return { ...def, steps: [...def.steps, step] };
}

/** Remove a step with its connections and the outcomes it realized. */
export function removeStep(def: WorkflowDefinition, stepId: string): WorkflowDefinition {
  return {
    ...def,
    steps: def.steps.filter((s) => s.id !== stepId),
    connections: def.connections.filter((c) => {
      if (c.kind === "data") return c.to.step !== stepId && !(c.from.kind === "step" && c.from.step === stepId);
      if (c.kind === "prerequisite") return c.before !== stepId && c.after !== stepId;
      return c.from !== stepId && c.to !== stepId;
    }),
    outcomes: def.outcomes.filter((o) => o.from.step !== stepId),
  };
}

export function updateStep(def: WorkflowDefinition, stepId: string, patch: Partial<WorkflowStep>): WorkflowDefinition {
  return { ...def, steps: def.steps.map((s) => (s.id === stepId ? { ...s, ...patch } : s)) };
}

export function addInput(def: WorkflowDefinition, portType: PortType): WorkflowDefinition {
  const id = uniqueId("input", new Set(def.inputs.map((i) => i.id)));
  return { ...def, inputs: [...def.inputs, { id, label: PORT_TYPE_LABELS[portType], portType, source: { kind: "unset" } }] };
}

export function removeInput(def: WorkflowDefinition, inputId: string): WorkflowDefinition {
  return {
    ...def,
    inputs: def.inputs.filter((i) => i.id !== inputId),
    connections: def.connections.filter((c) => !(c.kind === "data" && c.from.kind === "input" && c.from.input === inputId)),
  };
}

export interface OutcomeCandidate {
  key: string;
  label: string;
  step: string;
  port: string;
}

/** Outputs that can realize an outcome of `kind`. */
export function outcomeCandidates(def: WorkflowDefinition, contracts: OperationContract[], kind: OutcomeKind): OutcomeCandidate[] {
  const accepts = OUTCOME_ACCEPTS[kind];
  return def.steps.flatMap((s) => {
    const c = contractOf(contracts, s);
    return (c?.outputs ?? [])
      .filter((o) => o.types[0] !== undefined && accepts.includes(o.types[0]))
      .map((o) => ({ key: `${s.id}.${o.name}`, label: `${s.label} → ${o.label}`, step: s.id, port: o.name }));
  });
}

export function addOutcome(def: WorkflowDefinition, kind: OutcomeKind, from: { step: string; port: string }): WorkflowDefinition {
  const id = uniqueId("outcome", new Set(def.outcomes.map((o) => o.id)));
  return { ...def, outcomes: [...def.outcomes, { id, kind, label: OUTCOME_LABELS[kind], from, optional: false }] };
}

export function diagnosticsFor(
  diagnostics: WorkflowDiagnostic[],
  where: { step?: string; input?: string; outcome?: string },
): WorkflowDiagnostic[] {
  return diagnostics.filter(
    (d) =>
      (where.step !== undefined && d.step === where.step && d.outcome === null) ||
      (where.input !== undefined && d.input === where.input) ||
      (where.outcome !== undefined && d.outcome === where.outcome),
  );
}

/** Diagnostics not attached to a step, input, or outcome. */
export function generalDiagnostics(diagnostics: WorkflowDiagnostic[]): WorkflowDiagnostic[] {
  return diagnostics.filter((d) => d.step === null && d.input === null && d.outcome === null);
}

export function sameDefinition(a: WorkflowDefinition, b: WorkflowDefinition): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}
