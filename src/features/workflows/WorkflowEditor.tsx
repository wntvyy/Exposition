// Workflow editor (editor tab `workflow:<id>`): a typed canvas and a form
// over the same draft. Steps come from the registered operations; port
// bindings offer only compatible sources; outcomes, parameters, code and
// prompt payloads are edited in the form (the canvas's inspector). Every
// change is validated in Rust (live diagnostics) and saving creates a new
// immutable revision. Saving never runs anything: a saved, well-formed
// revision runs only after "Review and run…" shows its full plan and the
// user presses "Start run".
//
// M9: a step may run another saved workflow at a pinned revision (composite;
// its ports come from the child), carry a bounded condition, and repeat
// (forEach / until); the workflow may allow up to 4 steps at once. Any of
// these makes the draft schema exposition.workflow/2. Triggers are enabled
// only from this editor's Triggers section, through a standing-plan review.
import { FileDown, LayoutGrid, ListTree, Play, Plus, Save, X } from "lucide-react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";

import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import type { EditorProps } from "../../app/registry";
import type { ExecutionCeiling } from "../../bindings/ExecutionCeiling";
import type { OperationContract } from "../../bindings/OperationContract";
import type { OutcomeKind } from "../../bindings/OutcomeKind";
import type { ParameterSpec } from "../../bindings/ParameterSpec";
import type { ParamValue } from "../../bindings/ParamValue";
import type { PortSpec } from "../../bindings/PortSpec";
import type { PortType } from "../../bindings/PortType";
import type { WorkflowDiagnostic } from "../../bindings/WorkflowDiagnostic";
import type { WorkflowInput } from "../../bindings/WorkflowInput";
import type { WorkflowSummary } from "../../bindings/WorkflowSummary";
import type { WorkflowValidation } from "../../bindings/WorkflowValidation";
import { COMPOSITE_OPERATION, type SavedWorkflow, type WorkflowDefinition, type WorkflowStep } from "./contract";
import { errorMessage } from "../../lib/ipc";
import {
  Banner,
  Button,
  Checkbox,
  confirmUnsaved,
  ErrorState,
  IconButton,
  LoadingState,
  SegmentedControl,
  Select,
  StatusPill,
  TextField,
  UnavailableState,
} from "../../ui/components";
import { useWorkspace } from "../workspace/store";
import { CodePayloadEditor } from "./CodePayloadEditor";
import { exportBundle } from "./actions";
import { workflowCommands } from "./ipc";
import {
  addCompositeStep,
  isComposite,
  isDerivedContract,
  MAX_PARALLEL,
  maxParallelOf,
  normalizeSchema,
  oneLine,
  setMaxParallel,
  textValueProblem,
  withComposites,
} from "./orchestration";
import { PlanReviewDialog } from "./PlanReview";
import { CONTAINMENT_VIEW, EFFECT_LABELS } from "./runModel";
import { openRun, RunHistory } from "./RunView";
import { CompositeSection, ConditionSection, RepeatSection } from "./StepOrchestration";
import { TriggersSection } from "./TriggersPanel";
import { useCompositeChildren, type CompositeInfo } from "./useComposites";
import { WorkflowCanvas, type CanvasSelectionItem } from "./WorkflowCanvas";
import {
  addInput,
  addOutcome,
  addStep,
  bindingsInto,
  compatibleSources,
  contractOf,
  diagnosticsFor,
  downstreamOf,
  FILE_INPUT_TYPES,
  generalDiagnostics,
  OUTCOME_LABELS,
  outcomeCandidates,
  parseSourceKey,
  PORT_TYPE_LABELS,
  prerequisitesOf,
  removeInput,
  removeStep,
  sameDefinition,
  setBindings,
  setPrerequisites,
  sourceKey,
  updateStep,
  WORKFLOW_KEY_PREFIX,
} from "./model";

const EXECUTION_LABELS: Record<ExecutionCeiling, string> = {
  none: "No code runs",
  restricted: "Restricted container only",
  trustedNative: "Trusted native execution allowed (your account's privileges)",
};

const UNBOUND = "__unbound";

function Diagnostics({ items, testId }: { items: WorkflowDiagnostic[]; testId?: string }) {
  if (items.length === 0) return null;
  return (
    <ul className="xp-wf__diagnostics" data-testid={testId}>
      {items.map((d, i) => (
        <li key={i} data-severity={d.severity} data-code={d.code} data-rule={d.rule ?? undefined}>
          <StatusPill status={d.severity === "error" ? "failed" : d.severity === "warning" ? "warning" : "idle"} label={d.severity === "error" ? "Problem" : d.severity === "warning" ? "Note" : "Info"} />
          <span>
            {d.message}
            {d.rule ? <span className="xp-wf__muted"> (rule {d.rule})</span> : null}
          </span>
        </li>
      ))}
    </ul>
  );
}

function TextArea(props: { label: string; value: string; onChange: (v: string) => void; testId?: string }) {
  const id = useId();
  return (
    <div className="ui-field">
      <label className="ui-field__label" htmlFor={id}>
        {props.label}
      </label>
      <textarea id={id} className="ui-input xp-wf__textarea" rows={4} value={props.value} onChange={(e) => props.onChange(e.target.value)} data-testid={props.testId} />
    </div>
  );
}

function ParameterField({
  spec,
  value,
  repeated,
  onChange,
}: {
  spec: ParameterSpec;
  value: ParamValue | undefined;
  /** The step repeats for each value of this parameter (set in Repeat below). */
  repeated?: boolean;
  onChange: (v: ParamValue | null) => void;
}) {
  const label = spec.required ? `${spec.label} (required)` : spec.label;
  if (repeated) {
    return (
      <div className="ui-field">
        <span className="ui-field__label">{label}</span>
        <span className="xp-wf__muted">Varies: one instance per value (see Repeat below).</span>
      </div>
    );
  }
  switch (spec.kind.kind) {
    case "boolean":
      return (
        <Checkbox isSelected={value?.kind === "boolean" && value.value} onChange={(v) => onChange({ kind: "boolean", value: v })}>
          {label}
        </Checkbox>
      );
    case "integer": {
      const { min, max } = spec.kind;
      return (
        <TextField
          label={`${label} — ${min}–${max}`}
          value={value?.kind === "integer" ? String(value.value) : ""}
          onChange={(v) => {
            const n = Number.parseInt(v, 10);
            onChange(v.trim() === "" || Number.isNaN(n) ? null : { kind: "integer", value: n });
          }}
        />
      );
    }
    default: {
      const text = value?.kind === "text" ? value.value : "";
      const problem = spec.kind.kind === "text" ? textValueProblem(text, spec.kind.maxLen) : null;
      return (
        <TextField
          label={label}
          code={spec.kind.kind === "path"}
          value={text}
          isInvalid={problem !== null}
          errorMessage={problem ?? undefined}
          onChange={(v) => onChange(v === "" ? null : { kind: "text", value: oneLine(v) })}
        />
      );
    }
  }
}

function PortBinding({
  draft,
  contracts,
  step,
  port,
  onChange,
}: {
  draft: WorkflowDefinition;
  contracts: OperationContract[];
  step: WorkflowStep;
  port: PortSpec;
  onChange: (def: WorkflowDefinition) => void;
}) {
  const options = compatibleSources(draft, contracts, step.id, port.name);
  const bound = bindingsInto(draft, step.id, port.name);
  const accepts = port.types.map((t) => PORT_TYPE_LABELS[t]).join(" or ");
  const label = `${port.label} (${accepts})${port.required ? "" : " — optional"}`;
  if (!port.multiple) {
    const current = bound[0] ? sourceKey(bound[0]) : UNBOUND;
    return (
      <Select
        label={label}
        value={current}
        onChange={(key) => {
          const source = key === UNBOUND ? null : parseSourceKey(String(key));
          onChange(setBindings(draft, step.id, port.name, source ? [source] : []));
        }}
        options={[{ id: UNBOUND, label: "Not bound" }, ...options.map((o) => ({ id: o.key, label: o.label }))]}
        data-testid={`bind-${step.id}-${port.name}`}
      />
    );
  }
  const boundKeys = bound.map(sourceKey);
  const remaining = options.filter((o) => !boundKeys.includes(o.key));
  return (
    <div className="xp-wf__multi">
      <span className="ui-field__label">{label}</span>
      <ul className="xp-wf__chips">
        {bound.map((b) => (
          <li key={sourceKey(b)}>
            {options.find((o) => o.key === sourceKey(b))?.label ?? sourceKey(b)}
            <IconButton icon={X} size="sm" aria-label={`Unbind ${sourceKey(b)}`} onPress={() => onChange(setBindings(draft, step.id, port.name, bound.filter((x) => sourceKey(x) !== sourceKey(b))))} />
          </li>
        ))}
        {bound.length === 0 && <li className="xp-wf__muted">Nothing bound</li>}
      </ul>
      {remaining.length > 0 && (
        <Select
          aria-label={`Add a source to ${port.label}`}
          placeholder="Add a compatible source…"
          value={null}
          onChange={(key) => {
            const source = parseSourceKey(String(key));
            if (source) onChange(setBindings(draft, step.id, port.name, [...bound, source]));
          }}
          options={remaining.map((o) => ({ id: o.key, label: o.label }))}
        />
      )}
    </div>
  );
}

function StepCard({
  draft,
  contracts,
  step,
  diagnostics,
  composites,
  onChange,
}: {
  draft: WorkflowDefinition;
  contracts: OperationContract[];
  step: WorkflowStep;
  diagnostics: WorkflowDiagnostic[];
  composites: ReadonlyMap<string, CompositeInfo>;
  onChange: (def: WorkflowDefinition) => void;
}) {
  const contract = contractOf(contracts, step);
  const forEachParameter = step.repeat?.kind === "forEach" ? step.repeat.parameter : null;
  // The operation declares the language of its code (python, r, julia).
  const codeLanguage = contract?.payload?.kind === "code" ? contract.payload.language : "python";
  const downstream = downstreamOf(draft, step.id);
  const prereqs = prerequisitesOf(draft, step.id);
  return (
    <li
      className="xp-wf__card"
      data-testid={`workflow-step-${step.id}`}
      data-operation={step.operation.id}
      data-composite={isComposite(step) || undefined}
      data-conditional={step.condition ? "true" : undefined}
      data-repeat={step.repeat?.kind}
    >
      <div className="xp-wf__card-header">
        <TextField className="xp-wf__grow" label="Step" value={step.label} onChange={(v) => onChange(updateStep(draft, step.id, { label: v }))} />
        <IconButton icon={X} aria-label={`Remove step ${step.label}`} onPress={() => onChange(removeStep(draft, step.id))} />
      </div>
      {contract ? (
        <>
          {!isDerivedContract(contract) && (
            <p className="xp-wf__muted">
              <strong>{contract.title}</strong> (v{contract.version}) — {contract.purpose}
            </p>
          )}
          <p className="xp-wf__muted">
            Runs through: {contract.runner.title} ({CONTAINMENT_VIEW[contract.runner.containment].label}) —{" "}
            {contract.runner.available ? "available" : `unavailable: ${contract.runner.availability}`}. Effects:{" "}
            {contract.effects.map((e) => EFFECT_LABELS[e].toLowerCase()).join("; ")}.
          </p>
          <CompositeSection draft={draft} step={step} infos={composites} onChange={onChange} />
          <div className="xp-wf__form">
            {contract.inputs.map((port) => (
              <PortBinding key={port.name} draft={draft} contracts={contracts} step={step} port={port} onChange={onChange} />
            ))}
            {contract.parameters.map((spec) => (
              <ParameterField
                key={spec.name}
                spec={spec}
                repeated={forEachParameter === spec.name}
                value={step.parameters[spec.name]}
                onChange={(v) => {
                  const parameters = { ...step.parameters };
                  if (v === null) delete parameters[spec.name];
                  else parameters[spec.name] = v;
                  onChange(updateStep(draft, step.id, { parameters }));
                }}
              />
            ))}
          </div>
          {contract.payload?.kind === "prompt" && (
            <TextArea
              label="Instructions for the agent (read-only review; it cannot run code or change files)"
              value={step.payload?.kind === "prompt" ? step.payload.text : ""}
              onChange={(text) => onChange(updateStep(draft, step.id, { payload: { kind: "prompt", text } }))}
              testId={`prompt-${step.id}`}
            />
          )}
          {contract.payload?.kind === "code" && (
            <div className="xp-wf__payload">
              <Checkbox
                isSelected={step.payload?.kind === "code"}
                onChange={(on) =>
                  onChange(updateStep(draft, step.id, { payload: on ? { kind: "code", language: codeLanguage, source: "" } : null }))
                }
              >
                Write the code here{contract.payload.unlessInput ? ` (instead of binding “${contract.payload.unlessInput}”)` : ""}
              </Checkbox>
              {step.payload?.kind === "code" && (
                <CodePayloadEditor
                  label={`Code for ${step.label}`}
                  language={step.payload.language}
                  value={step.payload.source}
                  onChange={(source) => onChange(updateStep(draft, step.id, { payload: { kind: "code", language: codeLanguage, source } }))}
                />
              )}
            </div>
          )}
          <ConditionSection draft={draft} contracts={contracts} step={step} onChange={onChange} />
          <RepeatSection draft={draft} contracts={contracts} step={step} onChange={onChange} />
          {draft.steps.length > 1 && (
            <div className="xp-wf__multi">
              <span className="ui-field__label">Runs after (explicit prerequisites; no data)</span>
              <div className="xp-wf__inline">
                {draft.steps
                  .filter((s) => s.id !== step.id && !downstream.has(s.id))
                  .map((s) => (
                    <Checkbox
                      key={s.id}
                      isSelected={prereqs.includes(s.id)}
                      onChange={(on) => onChange(setPrerequisites(draft, step.id, on ? [...prereqs, s.id] : prereqs.filter((p) => p !== s.id)))}
                    >
                      {s.label}
                    </Checkbox>
                  ))}
              </div>
            </div>
          )}
        </>
      ) : (
        <Banner tone="danger" title="Unknown operation">
          {step.operation.id} v{step.operation.version} is not a registered operation. Remove this step.
        </Banner>
      )}
      <Diagnostics items={diagnostics} />
    </li>
  );
}

function InputRow({
  draft,
  input,
  diagnostics,
  onChange,
}: {
  draft: WorkflowDefinition;
  input: WorkflowInput;
  diagnostics: WorkflowDiagnostic[];
  onChange: (def: WorkflowDefinition) => void;
}) {
  const update = (patch: Partial<WorkflowInput>) =>
    onChange({ ...draft, inputs: draft.inputs.map((i) => (i.id === input.id ? { ...i, ...patch } : i)) });
  const path = input.source.kind === "resource" ? input.source.path : "";
  const version = input.source.kind === "resource" ? input.source.version : null;
  return (
    <li className="xp-wf__card" data-testid={`workflow-input-${input.id}`}>
      <div className="xp-wf__card-header">
        <TextField className="xp-wf__grow" label="Input" value={input.label} onChange={(label) => update({ label })} />
        <IconButton icon={X} aria-label={`Remove input ${input.label}`} onPress={() => onChange(removeInput(draft, input.id))} />
      </div>
      <div className="xp-wf__form">
        <Select<PortType>
          label="Type"
          value={input.portType}
          onChange={(portType) => update({ portType })}
          options={FILE_INPUT_TYPES.map((t) => ({ id: t, label: PORT_TYPE_LABELS[t] }))}
        />
        <TextField
          label="Project file"
          code
          value={path}
          placeholder="e.g. analysis/run.py"
          onChange={(p) =>
            update({
              source: p.trim() === "" ? { kind: "unset" } : { kind: "resource", path: p.trim(), version: version ?? { kind: "atRunStart" } },
            })
          }
          data-testid={`input-path-${input.id}`}
        />
        {version && (
          <Select
            label="Version"
            value={version.kind}
            onChange={(kind) =>
              update({
                source: { kind: "resource", path, version: kind === "pinned" ? { kind: "pinned", contentHash: "" } : { kind: "atRunStart" } },
              })
            }
            options={[
              { id: "atRunStart", label: "The version current when a run starts (recorded by the run)" },
              { id: "pinned", label: "Pinned to an exact content hash" },
            ]}
          />
        )}
        {version?.kind === "pinned" && (
          <TextField
            label="SHA-256 content hash"
            code
            value={version.contentHash}
            onChange={(contentHash) => update({ source: { kind: "resource", path, version: { kind: "pinned", contentHash } } })}
          />
        )}
      </div>
      <Diagnostics items={diagnostics} />
    </li>
  );
}

/**
 * Load a workflow by id. Some views (the Research inspector) open a workflow
 * by its graph entity id; resolve that through the workflow list.
 */
async function resolveWorkflow(id: string): Promise<SavedWorkflow> {
  try {
    return await workflowCommands.get(id);
  } catch (error) {
    const unknown = typeof error === "object" && error !== null && "code" in error && (error as { code: unknown }).code === "workflow.unknown";
    if (!unknown) throw error;
    const match = (await workflowCommands.list()).find((w) => w.entity === id);
    if (!match) throw error;
    return workflowCommands.get(match.id);
  }
}

type EditorView = "form" | "canvas";
const VIEW_KEY = "exposition.workflows.view";

function readView(): EditorView {
  try {
    return localStorage.getItem(VIEW_KEY) === "canvas" ? "canvas" : "form";
  } catch {
    return "form";
  }
}

function originNote(saved: SavedWorkflow): string | null {
  switch (saved.origin.kind) {
    case "agent":
      return `Proposed by agent task ${saved.origin.taskId}.`;
    case "preset":
      return null;
    default:
      return null;
  }
}

export function WorkflowEditor({ resource }: EditorProps) {
  const id = resource.key.slice(WORKFLOW_KEY_PREFIX.length);
  const [saved, setSaved] = useState<SavedWorkflow | null>(null);
  const [draft, setDraftState] = useState<WorkflowDefinition | null>(null);
  const [contracts, setContracts] = useState<OperationContract[]>([]);
  const [validation, setValidation] = useState<WorkflowValidation | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [newStep, setNewStep] = useState<string | null>(null);
  const [newChild, setNewChild] = useState<string | null>(null);
  const [others, setOthers] = useState<WorkflowSummary[]>([]);
  const [newInput, setNewInput] = useState<PortType>("dataset");
  const [newOutcome, setNewOutcome] = useState<OutcomeKind>("retainedFigure");
  const [reviewOpen, setReviewOpen] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const [view, setViewState] = useState<EditorView>(readView);
  const [selection, setSelection] = useState<CanvasSelectionItem | null>(null);
  const reasonId = useId();
  // A draft belongs to the project it was opened in.
  const [projectRoot, setProjectRoot] = useState<string | null>(null);
  const currentRoot = useWorkspace((s) => s.info?.root ?? null);

  const setView = (next: EditorView) => {
    setViewState(next);
    try {
      localStorage.setItem(VIEW_KEY, next);
    } catch {
      // Presentation preference only.
    }
  };

  const load = useCallback(async () => {
    try {
      const [workflow, ops] = await Promise.all([resolveWorkflow(id), workflowCommands.operations()]);
      setSaved(workflow);
      setDraftState(workflow.definition);
      setValidation(workflow.validation);
      setContracts(ops);
      setProjectRoot(useWorkspace.getState().info?.root ?? null);
      setLoadError(null);
      editors.setTitle(resource.key, workflow.definition.title || "Workflow draft");
    } catch (error) {
      setLoadError(errorMessage(error));
    }
  }, [id, resource.key]);

  useEffect(() => {
    void load();
  }, [load]);

  // Every edit goes through here: a draft using any v2 field declares exposition.workflow/2.
  const savedSchema = saved?.definition.schema;
  const setDraft = useCallback(
    (next: WorkflowDefinition) => setDraftState(savedSchema ? normalizeSchema(next, savedSchema) : next),
    [savedSchema],
  );
  const composites = useCompositeChildren(draft);
  // Registered contracts plus the ports derived for each composite reference.
  const effective = useMemo(() => withComposites(contracts, composites.definitions), [contracts, composites.definitions]);
  const composingStep = newStep === `${COMPOSITE_OPERATION}@1`;
  useEffect(() => {
    if (!composingStep || !saved) return;
    workflowCommands.list().then(
      (list) => setOthers(list.filter((w) => w.id !== saved.id)),
      () => setOthers([]),
    );
  }, [composingStep, saved]);

  const dirty = Boolean(draft && saved && !sameDefinition(draft, saved.definition));
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  useEffect(() => editors.setDirty(resource.key, dirty), [dirty, resource.key]);

  // Live validation in Rust (debounced).
  useEffect(() => {
    if (!draft) return;
    const timer = setTimeout(() => {
      workflowCommands.validate(draft, saved?.id ?? null).then(setValidation, (e: unknown) => setSaveError(errorMessage(e)));
    }, 200);
    return () => clearTimeout(timer);
  }, [draft, saved?.id]);

  const save = useCallback(async (): Promise<boolean> => {
    if (!draft || !saved) return false;
    setSaving(true);
    setSaveError(null);
    try {
      const result = await workflowCommands.save({ id: saved.id, expectedRevision: saved.revision, definition: draft });
      setSaved(result);
      setDraftState(result.definition);
      setValidation(result.validation);
      editors.setTitle(resource.key, result.definition.title || "Workflow draft");
      notify({
        tone: result.unchanged ? "info" : "success",
        title: result.unchanged ? "No changes to save" : `Saved revision ${result.revision}`,
        body: `${result.definition.title}: ${result.validation.state === "wellFormed" ? "well-formed" : `incomplete (${result.validation.errors} problem${result.validation.errors === 1 ? "" : "s"})`}. ${result.validation.runnable ? "Use Review and run… to review its plan; saving runs nothing." : result.validation.runnableReason}`,
      });
      return true;
    } catch (error) {
      setSaveError(errorMessage(error));
      return false;
    } finally {
      setSaving(false);
    }
  }, [draft, saved, resource.key]);

  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(
    () =>
      editors.registerCloseGuard(resource.key, async () => {
        if (!dirtyRef.current) return true;
        const choice = await confirmUnsaved(saved?.definition.title || "this workflow draft");
        if (choice === "save") return saveRef.current();
        return choice === "discard";
      }),
    [resource.key, saved?.definition.title],
  );

  const diagnostics = validation?.diagnostics ?? [];
  const candidates = useMemo(
    () => (draft ? outcomeCandidates(draft, effective, newOutcome) : []),
    [draft, effective, newOutcome],
  );

  if (loadError) {
    return (
      <div className="xp-wf">
        <ErrorState title="Could not open this workflow draft" description={loadError} actions={<Button onPress={() => void load()}>Retry</Button>} />
      </div>
    );
  }
  if (!draft || !saved) return <LoadingState title="Loading the workflow draft…" />;
  if (projectRoot !== null && currentRoot !== projectRoot) {
    return (
      <div className="xp-wf" data-testid="workflow-detached">
        <UnavailableState
          title="This draft's project is not open"
          description={`“${saved.definition.title}” belongs to ${projectRoot}. Reopen that project to edit or save it.`}
          actions={<Button onPress={() => void editors.close(resource.key)}>Close</Button>}
        />
      </div>
    );
  }

  const errors = diagnostics.filter((d) => d.severity === "error").length;
  const runnable = validation?.runnable ?? false;
  // Why "Review and run…" is unavailable (null: it is available for the saved revision).
  const runBlock = dirty
    ? "Save the draft first: a run uses a saved revision, never unsaved edits."
    : validation?.state !== "wellFormed"
      ? `This revision is incomplete (${errors} problem${errors === 1 ? "" : "s"}): fix ${errors === 1 ? "it" : "them"} and save.`
      : !runnable
        ? (validation?.runnableReason ?? "This revision cannot run.")
        : null;
  const origin = originNote(saved);

  const inputsSection = (
    <section className="xp-wf__group" aria-label="Inputs">
      <h3 className="xp-wf__group-title">Inputs</h3>
      <p className="xp-wf__muted">Explicit project files the workflow reads, versioned when a run is reviewed or pinned to exact content.</p>
      <ul className="xp-wf__cards">
        {draft.inputs.map((input) => (
          <InputRow key={input.id} draft={draft} input={input} diagnostics={diagnosticsFor(diagnostics, { input: input.id })} onChange={setDraft} />
        ))}
      </ul>
      <div className="xp-wf__row">
        <Select<PortType> label="New input type" value={newInput} onChange={setNewInput} options={FILE_INPUT_TYPES.map((t) => ({ id: t, label: PORT_TYPE_LABELS[t] }))} />
        <Button icon={Plus} onPress={() => setDraft(addInput(draft, newInput))}>
          Add Input
        </Button>
      </div>
    </section>
  );

  const child = composingStep ? others.find((w) => w.id === newChild) : undefined;
  const addStepRow = (
    <div className="xp-wf__row">
      <Select
        label="New step"
        placeholder="Choose an operation…"
        value={newStep}
        onChange={(k) => setNewStep(String(k))}
        options={contracts
          .filter((c) => !isDerivedContract(c))
          .map((c) => ({ id: `${c.id}@${c.version}`, label: c.title, description: c.purpose }))}
        data-testid="workflow-new-step"
      />
      {composingStep && (
        <Select
          label="Workflow to run"
          placeholder={others.length ? "Choose a saved workflow…" : "No other saved workflows"}
          value={newChild}
          onChange={(k) => setNewChild(String(k))}
          options={others.map((w) => ({ id: w.id, label: w.title, description: `Latest revision ${w.revision}${w.state === "wellFormed" ? "" : " · incomplete"}` }))}
          data-testid="workflow-new-composite"
        />
      )}
      <Button
        icon={Plus}
        isDisabled={!newStep || (composingStep && !child)}
        onPress={() => {
          let next: WorkflowDefinition | null = null;
          if (composingStep && child) next = addCompositeStep(draft, { workflow: child.id, revision: child.revision }, child.title);
          else {
            const contract = contracts.find((c) => `${c.id}@${c.version}` === newStep);
            if (contract) next = addStep(draft, contract);
          }
          if (next) {
            setDraft(next);
            const added = next.steps[next.steps.length - 1];
            if (added) setSelection({ kind: "step", id: added.id });
          }
        }}
        data-testid="workflow-add-step"
      >
        Add Step
      </Button>
      {composingStep && (
        <p className="xp-wf__muted xp-wf__wide">
          The step runs the chosen workflow at its latest saved revision, pinned: later edits of that workflow never change this step
          until you choose “Use latest revision”. Composites may nest 3 deep and expand to at most 100 steps.
        </p>
      )}
    </div>
  );

  const outcomesSection = (
    <section className="xp-wf__group" aria-label="Outcomes">
      <h3 className="xp-wf__group-title">Outcomes</h3>
      <p className="xp-wf__muted">What the workflow produces. Every step must lead to one; a manuscript promotion is an optional destination.</p>
      <ul className="xp-wf__cards">
        {draft.outcomes.map((o) => {
          const options = outcomeCandidates(draft, effective, o.kind);
          return (
            <li key={o.id} className="xp-wf__card" data-testid={`workflow-outcome-${o.id}`}>
              <div className="xp-wf__card-header">
                <TextField
                  className="xp-wf__grow"
                  label={OUTCOME_LABELS[o.kind]}
                  value={o.label}
                  onChange={(label) => setDraft({ ...draft, outcomes: draft.outcomes.map((x) => (x.id === o.id ? { ...x, label } : x)) })}
                />
                <IconButton
                  icon={X}
                  aria-label={`Remove outcome ${o.label}`}
                  onPress={() => setDraft({ ...draft, outcomes: draft.outcomes.filter((x) => x.id !== o.id) })}
                />
              </div>
              <div className="xp-wf__form">
                <Select
                  label="Produced by"
                  value={`${o.from.step}.${o.from.port}`}
                  onChange={(key) => {
                    const c = options.find((x) => x.key === key);
                    if (c) setDraft({ ...draft, outcomes: draft.outcomes.map((x) => (x.id === o.id ? { ...x, from: { step: c.step, port: c.port } } : x)) });
                  }}
                  options={
                    options.some((c) => c.key === `${o.from.step}.${o.from.port}`)
                      ? options.map((c) => ({ id: c.key, label: c.label }))
                      : [{ id: `${o.from.step}.${o.from.port}`, label: `${o.from.step} → ${o.from.port} (incompatible)` }, ...options.map((c) => ({ id: c.key, label: c.label }))]
                  }
                />
                <Checkbox
                  isSelected={o.optional}
                  onChange={(optional) => setDraft({ ...draft, outcomes: draft.outcomes.map((x) => (x.id === o.id ? { ...x, optional } : x)) })}
                >
                  Optional destination
                </Checkbox>
              </div>
              <Diagnostics items={diagnosticsFor(diagnostics, { outcome: o.id })} />
            </li>
          );
        })}
      </ul>
      <div className="xp-wf__row">
        <Select<OutcomeKind>
          label="New outcome"
          value={newOutcome}
          onChange={setNewOutcome}
          options={(Object.keys(OUTCOME_LABELS) as OutcomeKind[]).map((k) => ({ id: k, label: OUTCOME_LABELS[k] }))}
        />
        <Button icon={Plus} isDisabled={candidates.length === 0} onPress={() => candidates[0] && setDraft(addOutcome(draft, newOutcome, { step: candidates[0].step, port: candidates[0].port }))}>
          Add Outcome
        </Button>
      </div>
    </section>
  );

  const selectedStep = selection?.kind === "step" ? draft.steps.find((s) => s.id === selection.id) : undefined;
  const selectedInput = selection?.kind === "input" ? draft.inputs.find((i) => i.id === selection.id) : undefined;

  return (
    <div className="xp-wf" data-testid="workflow-editor" data-workflow={saved.id} data-revision={saved.revision} data-view={view}>
      <div className={`xp-wf__page${view === "canvas" ? " xp-wf__page--wide" : ""}`}>
        <header className="xp-wf__header">
          <div className="xp-wf__grow">
            <h2 className="xp-wf__title">{draft.title || "Untitled workflow"}</h2>
            <p className="xp-wf__muted">
              Revision {saved.revision} of {saved.revisions.length}
              {saved.imported ? " · imported (untrusted, never run automatically)" : ""}
              {draft.preset ? ` · from the ${draft.preset} preset` : ""}
              {dirty ? " · unsaved changes" : ""}
            </p>
          </div>
          <StatusPill
            status={validation?.state === "wellFormed" ? "idle" : "warning"}
            label={validation?.state === "wellFormed" ? "Well-formed draft" : "Incomplete draft"}
            detail={errors ? `${errors} problem${errors === 1 ? "" : "s"}` : undefined}
          />
          <SegmentedControl<EditorView>
            aria-label="Editor view"
            value={view}
            onChange={setView}
            options={[
              { id: "form", label: "Form", icon: ListTree },
              { id: "canvas", label: "Canvas", icon: LayoutGrid },
            ]}
          />
          <Button
            variant="ghost"
            icon={FileDown}
            isPending={exporting}
            isDisabled={exporting}
            onPress={async () => {
              setExporting(true);
              setExportError(null);
              const result = await exportBundle(saved.id, saved.revision);
              setExporting(false);
              if (result && !result.ok) setExportError(result.error);
            }}
            data-testid="workflow-export"
          >
            Export revision {saved.revision}…
          </Button>
          <Button variant={dirty ? "primary" : "secondary"} icon={Save} isPending={saving} isDisabled={saving} onPress={() => void save()} data-testid="workflow-save">
            Save Revision
          </Button>
          <Button
            variant={dirty ? "secondary" : "primary"}
            icon={Play}
            isDisabled={runBlock !== null}
            aria-describedby={runBlock ? reasonId : undefined}
            onPress={() => setReviewOpen(true)}
            data-testid="workflow-review-run"
          >
            Review and run…
          </Button>
        </header>
        <div data-testid="workflow-status" data-runnable={String(runnable)} data-state={validation?.state ?? "incomplete"} data-can-review={String(runBlock === null)}>
          {runBlock ? (
            <Banner tone={dirty ? "info" : "warning"} title={dirty ? "Unsaved changes" : "Cannot run this revision"}>
              <span id={reasonId} data-testid="workflow-run-reason">
                {runBlock}
              </span>{" "}
              Saving keeps every revision; drafts may be incomplete and are saved with their problems listed.
            </Banner>
          ) : (
            <Banner tone="info" title={`Revision ${saved.revision} can run`}>
              Review and run… shows everything the run will read, execute, and send. Nothing runs until you press Start run.
            </Banner>
          )}
        </div>
        {origin && (
          <div data-testid="workflow-origin" data-origin={saved.origin.kind}>
            <Banner tone="info" title="Agent proposal">
              {origin} It is a draft like any other: review and edit it, save it, and it runs only from a plan you review and start.
            </Banner>
          </div>
        )}
        {saveError && (
          <Banner tone="danger" title="Not saved" live>
            {saveError}
          </Banner>
        )}
        {exportError && (
          <div data-testid="workflow-export-error">
            <Banner tone="danger" title="Not exported" live>
              {exportError}
            </Banner>
          </div>
        )}
        <Diagnostics items={generalDiagnostics(diagnostics)} testId="workflow-diagnostics" />

        {view === "canvas" ? (
          <>
            <section className="xp-wf__group" aria-label="Workflow canvas">
              <WorkflowCanvas
                workflowId={saved.id}
                draft={draft}
                contracts={effective}
                composites={composites.infos}
                diagnostics={diagnostics}
                selection={selection}
                onSelectionChange={setSelection}
                onChange={setDraft}
              />
            </section>
            {addStepRow}
            <section className="xp-wf__group" aria-label="Inspector" data-testid="workflow-inspector">
              <h3 className="xp-wf__group-title">Inspector</h3>
              {selectedStep ? (
                <ul className="xp-wf__cards">
                  <StepCard
                    draft={draft}
                    contracts={effective}
                    step={selectedStep}
                    diagnostics={diagnosticsFor(diagnostics, { step: selectedStep.id })}
                    composites={composites.infos}
                    onChange={setDraft}
                  />
                </ul>
              ) : selectedInput ? (
                <ul className="xp-wf__cards">
                  <InputRow draft={draft} input={selectedInput} diagnostics={diagnosticsFor(diagnostics, { input: selectedInput.id })} onChange={setDraft} />
                </ul>
              ) : (
                <p className="xp-wf__muted">Select a step or an input on the canvas to edit it here. The Form view shows every part of the workflow.</p>
              )}
            </section>
          </>
        ) : (
          <>
            <section className="xp-wf__group" aria-label="General">
              <h3 className="xp-wf__group-title">General</h3>
              <div className="xp-wf__form">
                <TextField label="Title" value={draft.title} onChange={(title) => setDraft({ ...draft, title })} data-testid="workflow-title" />
                <Select<ExecutionCeiling>
                  label="Most this workflow may run"
                  value={draft.ceiling.execution}
                  onChange={(execution) => setDraft({ ...draft, ceiling: { ...draft.ceiling, execution } })}
                  options={(Object.keys(EXECUTION_LABELS) as ExecutionCeiling[]).map((k) => ({ id: k, label: EXECUTION_LABELS[k] }))}
                />
                <Select<number>
                  label="Steps that may run at the same time"
                  value={maxParallelOf(draft)}
                  onChange={(n) => setDraft(setMaxParallel(draft, n))}
                  options={Array.from({ length: MAX_PARALLEL }, (_, i) => ({
                    id: i + 1,
                    label: i === 0 ? "1 — one at a time" : String(i + 1),
                    description: i === 0 ? "Each step waits for the previous one" : "Only steps that do not depend on each other; global limits still apply",
                  }))}
                  description="Independent steps and repeat instances run side by side up to this limit and the application's global limits (4 steps in flight, 2 containers, 2 native, 2 model, 1 Lean)."
                  data-testid="workflow-max-parallel"
                />
                <div className="xp-wf__wide">
                  <TextArea label="Description" value={draft.description} onChange={(description) => setDraft({ ...draft, description })} />
                </div>
                <div className="xp-wf__inline xp-wf__wide">
                  <Checkbox isSelected={draft.ceiling.modelRequests} onChange={(v) => setDraft({ ...draft, ceiling: { ...draft.ceiling, modelRequests: v } })}>
                    May send declared inputs to the model provider
                  </Checkbox>
                  <Checkbox isSelected={draft.ceiling.retainResults} onChange={(v) => setDraft({ ...draft, ceiling: { ...draft.ceiling, retainResults: v } })}>
                    May retain results and record evidence
                  </Checkbox>
                  <Checkbox isSelected={draft.ceiling.proposeChanges} onChange={(v) => setDraft({ ...draft, ceiling: { ...draft.ceiling, proposeChanges: v } })}>
                    May propose reviewable changes
                  </Checkbox>
                </div>
              </div>
            </section>

            {inputsSection}

            <section className="xp-wf__group" aria-label="Steps">
              <h3 className="xp-wf__group-title">Steps</h3>
              <p className="xp-wf__muted">
                Each step is a registered operation or another saved workflow (pinned revision). Data reaches a step only through a compatible
                binding. A step may run only when a fixed condition on an earlier output holds, and may repeat a bounded number of times; there
                is no expression language and no open-ended loop.
              </p>
              <ul className="xp-wf__cards">
                {draft.steps.map((step) => (
                  <StepCard
                    key={step.id}
                    draft={draft}
                    contracts={effective}
                    step={step}
                    diagnostics={diagnosticsFor(diagnostics, { step: step.id })}
                    composites={composites.infos}
                    onChange={setDraft}
                  />
                ))}
              </ul>
              {addStepRow}
            </section>

            {outcomesSection}

            {validation && validation.plan.length > 0 && (
              <section className="xp-wf__group" aria-label="Order">
                <h3 className="xp-wf__group-title">Order of steps</h3>
                <ol className="xp-wf__plan" data-testid="workflow-plan">
                  {validation.plan.map((p) => (
                    <li key={p.step}>
                      <strong>{p.label}</strong> — {p.operation} via {p.runner}. Effects: {p.effects.join(", ")}.
                    </li>
                  ))}
                </ol>
              </section>
            )}
          </>
        )}

        <RunHistory workflow={saved.id} />
        <TriggersSection workflow={saved.id} revision={saved.revision} definition={saved.definition} reviewBlock={runBlock} />
      </div>
      <PlanReviewDialog
        workflow={saved.id}
        revision={saved.revision}
        isOpen={reviewOpen}
        onOpenChange={setReviewOpen}
        onStarted={(run) => openRun(run)}
      />
    </div>
  );
}
