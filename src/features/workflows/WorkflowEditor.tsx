// Workflow editor (editor tab `workflow:<id>`): a typed canvas and a form
// over the same draft. Steps come from the registered operations; port
// bindings offer only compatible sources; outcomes, parameters, code and
// prompt payloads are edited in the form (the canvas's inspector). Every
// change is validated in Rust (live diagnostics) and saving creates a new
// immutable revision. Saving never runs anything: a saved, well-formed
// revision runs only after "Review and run…" shows its full plan and the
// user presses "Start run".
import { LayoutGrid, ListTree, Play, Plus, Save, X } from "lucide-react";
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
import type { SavedWorkflow } from "../../bindings/SavedWorkflow";
import type { WorkflowDefinition } from "../../bindings/WorkflowDefinition";
import type { WorkflowDiagnostic } from "../../bindings/WorkflowDiagnostic";
import type { WorkflowInput } from "../../bindings/WorkflowInput";
import type { WorkflowStep } from "../../bindings/WorkflowStep";
import type { WorkflowValidation } from "../../bindings/WorkflowValidation";
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
import { workflowCommands } from "./ipc";
import { PlanReviewDialog } from "./PlanReview";
import { openRun, RunHistory } from "./RunView";
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

function ParameterField({ spec, value, onChange }: { spec: ParameterSpec; value: ParamValue | undefined; onChange: (v: ParamValue | null) => void }) {
  const label = spec.required ? `${spec.label} (required)` : spec.label;
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
    default:
      return (
        <TextField
          label={label}
          code={spec.kind.kind === "path"}
          value={value?.kind === "text" ? value.value : ""}
          onChange={(v) => onChange(v === "" ? null : { kind: "text", value: v })}
        />
      );
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
  onChange,
}: {
  draft: WorkflowDefinition;
  contracts: OperationContract[];
  step: WorkflowStep;
  diagnostics: WorkflowDiagnostic[];
  onChange: (def: WorkflowDefinition) => void;
}) {
  const contract = contractOf(contracts, step);
  const downstream = downstreamOf(draft, step.id);
  const prereqs = prerequisitesOf(draft, step.id);
  return (
    <li className="xp-wf__card" data-testid={`workflow-step-${step.id}`} data-operation={step.operation.id}>
      <div className="xp-wf__card-header">
        <TextField className="xp-wf__grow" label="Step" value={step.label} onChange={(v) => onChange(updateStep(draft, step.id, { label: v }))} />
        <IconButton icon={X} aria-label={`Remove step ${step.label}`} onPress={() => onChange(removeStep(draft, step.id))} />
      </div>
      {contract ? (
        <>
          <p className="xp-wf__muted">
            <strong>{contract.title}</strong> (v{contract.version}) — {contract.purpose}
          </p>
          <p className="xp-wf__muted">
            Runs through: {contract.runner.title} — {contract.runner.availability} Effects: {contract.effects.join(", ")}.
          </p>
          <div className="xp-wf__form">
            {contract.inputs.map((port) => (
              <PortBinding key={port.name} draft={draft} contracts={contracts} step={step} port={port} onChange={onChange} />
            ))}
            {contract.parameters.map((spec) => (
              <ParameterField
                key={spec.name}
                spec={spec}
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
                  onChange(updateStep(draft, step.id, { payload: on ? { kind: "code", language: "python", source: "" } : null }))
                }
              >
                Write the code here{contract.payload.unlessInput ? ` (instead of binding “${contract.payload.unlessInput}”)` : ""}
              </Checkbox>
              {step.payload?.kind === "code" && (
                <CodePayloadEditor
                  label={`Code for ${step.label}`}
                  value={step.payload.source}
                  onChange={(source) => onChange(updateStep(draft, step.id, { payload: { kind: "code", language: "python", source } }))}
                />
              )}
            </div>
          )}
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
  const [draft, setDraft] = useState<WorkflowDefinition | null>(null);
  const [contracts, setContracts] = useState<OperationContract[]>([]);
  const [validation, setValidation] = useState<WorkflowValidation | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [newStep, setNewStep] = useState<string | null>(null);
  const [newInput, setNewInput] = useState<PortType>("dataset");
  const [newOutcome, setNewOutcome] = useState<OutcomeKind>("retainedFigure");
  const [reviewOpen, setReviewOpen] = useState(false);
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
      const [workflow, ops] = await Promise.all([workflowCommands.get(id), workflowCommands.operations()]);
      setSaved(workflow);
      setDraft(workflow.definition);
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

  const dirty = Boolean(draft && saved && !sameDefinition(draft, saved.definition));
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  useEffect(() => editors.setDirty(resource.key, dirty), [dirty, resource.key]);

  // Live validation in Rust (debounced).
  useEffect(() => {
    if (!draft) return;
    const timer = setTimeout(() => {
      workflowCommands.validate(draft).then(setValidation, (e: unknown) => setSaveError(errorMessage(e)));
    }, 200);
    return () => clearTimeout(timer);
  }, [draft]);

  const save = useCallback(async (): Promise<boolean> => {
    if (!draft || !saved) return false;
    setSaving(true);
    setSaveError(null);
    try {
      const result = await workflowCommands.save({ id: saved.id, expectedRevision: saved.revision, definition: draft });
      setSaved(result);
      setDraft(result.definition);
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
    () => (draft ? outcomeCandidates(draft, contracts, newOutcome) : []),
    [draft, contracts, newOutcome],
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

  const addStepRow = (
    <div className="xp-wf__row">
      <Select
        label="New step"
        placeholder="Choose an operation…"
        value={newStep}
        onChange={(k) => setNewStep(String(k))}
        options={contracts.map((c) => ({ id: `${c.id}@${c.version}`, label: c.title, description: c.purpose }))}
      />
      <Button
        icon={Plus}
        isDisabled={!newStep}
        onPress={() => {
          const contract = contracts.find((c) => `${c.id}@${c.version}` === newStep);
          if (contract) {
            const next = addStep(draft, contract);
            setDraft(next);
            const added = next.steps[next.steps.length - 1];
            if (added) setSelection({ kind: "step", id: added.id });
          }
        }}
        data-testid="workflow-add-step"
      >
        Add Step
      </Button>
    </div>
  );

  const outcomesSection = (
    <section className="xp-wf__group" aria-label="Outcomes">
      <h3 className="xp-wf__group-title">Outcomes</h3>
      <p className="xp-wf__muted">What the workflow produces. Every step must lead to one; a manuscript promotion is an optional destination.</p>
      <ul className="xp-wf__cards">
        {draft.outcomes.map((o) => {
          const options = outcomeCandidates(draft, contracts, o.kind);
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
        <Diagnostics items={generalDiagnostics(diagnostics)} testId="workflow-diagnostics" />

        {view === "canvas" ? (
          <>
            <section className="xp-wf__group" aria-label="Workflow canvas">
              <WorkflowCanvas
                workflowId={saved.id}
                draft={draft}
                contracts={contracts}
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
                  <StepCard draft={draft} contracts={contracts} step={selectedStep} diagnostics={diagnosticsFor(diagnostics, { step: selectedStep.id })} onChange={setDraft} />
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
              <p className="xp-wf__muted">Each step is a registered operation. Data reaches a step only through a compatible binding; there are no loops, conditions, or nested workflows.</p>
              <ul className="xp-wf__cards">
                {draft.steps.map((step) => (
                  <StepCard key={step.id} draft={draft} contracts={contracts} step={step} diagnostics={diagnosticsFor(diagnostics, { step: step.id })} onChange={setDraft} />
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
