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
import { errorMessage, isCommandError } from "../../lib/ipc";
import {
  Badge,
  Banner,
  Button,
  Checkbox,
  confirmUnsaved,
  EmptyState,
  ErrorState,
  IconButton,
  InspectorBody,
  LoadingState,
  PageHeader,
  PanelHeader,
  Section,
  SegmentedControl,
  Select,
  StateSizeProvider,
  StatusPill,
  StatusText,
  TextArea,
  TextField,
  UnavailableState,
} from "../../ui/components";
import { useWorkspace } from "../workspace/store";
import { CodePayloadEditor } from "./CodePayloadEditor";
import { exportBundle, WORKFLOWS_KEY } from "./actions";
import { useRunnerAvailabilityEpoch } from "./availability";
import { workflowsChanged } from "./changes";
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
  isWorkflowId,
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
    <ul className="xp-wf-diagnostics" data-testid={testId}>
      {items.map((d, i) => (
        <li key={i} data-severity={d.severity} data-code={d.code} data-rule={d.rule ?? undefined}>
          <StatusText status={d.severity === "error" ? "failed" : d.severity === "warning" ? "warning" : "idle"} label={d.severity === "error" ? "Problem" : d.severity === "warning" ? "Note" : "Info"} />
          <span>
            {d.message}
            {d.rule ? <span className="ui-meta"> (rule {d.rule})</span> : null}
          </span>
        </li>
      ))}
    </ul>
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
        <span className="ui-meta">Varies: one instance per value (see Repeat below).</span>
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
      <ul className="xp-wf-sources">
        {bound.map((b) => (
          <li key={sourceKey(b)}>
            <Badge>{options.find((o) => o.key === sourceKey(b))?.label ?? sourceKey(b)}</Badge>
            <IconButton icon={X} size="sm" aria-label={`Unbind ${sourceKey(b)}`} onPress={() => onChange(setBindings(draft, step.id, port.name, bound.filter((x) => sourceKey(x) !== sourceKey(b))))} />
          </li>
        ))}
        {bound.length === 0 && <li className="ui-meta">Nothing bound</li>}
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
      className="xp-wf-block"
      data-testid={`workflow-step-${step.id}`}
      data-operation={step.operation.id}
      data-composite={isComposite(step) || undefined}
      data-conditional={step.condition ? "true" : undefined}
      data-repeat={step.repeat?.kind}
    >
      <div className="xp-wf-block__fields">
        <TextField className="xp-wf__grow" label="Step" value={step.label} onChange={(v) => onChange(updateStep(draft, step.id, { label: v }))} />
        <IconButton icon={X} aria-label={`Remove step ${step.label}`} onPress={() => onChange(removeStep(draft, step.id))} />
      </div>
      {contract ? (
        <>
          {!isDerivedContract(contract) && (
            <p className="ui-meta">
              <span className="xp-wf-name">{contract.title}</span> (v{contract.version}) — {contract.purpose}
            </p>
          )}
          <p className="ui-meta">
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
              label="Instructions for the agent"
              description="Read-only review: it cannot run code or change files."
              value={step.payload?.kind === "prompt" ? step.payload.text : ""}
              onChange={(text) => onChange(updateStep(draft, step.id, { payload: { kind: "prompt", text } }))}
              rows={4}
              testId={`prompt-${step.id}`}
            />
          )}
          {contract.payload?.kind === "code" && (
            <div className="xp-wf__stack">
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
    <li className="xp-wf-block" data-testid={`workflow-input-${input.id}`}>
      <div className="xp-wf-block__fields">
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

/** Why a workflow tab shows no draft. */
type LoadFailure =
  /** The tab's key does not hold a workflow id (a stale or malformed reference); nothing was requested. */
  | { kind: "invalidReference" }
  /** No such workflow is saved in the open project (deleted, or it belongs to another project). */
  | { kind: "missing"; message: string }
  | { kind: "error"; message: string };

function loadFailure(error: unknown): LoadFailure {
  if (isCommandError(error) && error.code === "workflow.unknown") return { kind: "missing", message: error.message };
  // The IPC layer refuses a malformed id before the command runs (a plain string, no code).
  if (typeof error === "string" && error.startsWith("invalid args")) return { kind: "invalidReference" };
  return { kind: "error", message: errorMessage(error) };
}

const showWorkflows = () => editors.open({ key: WORKFLOWS_KEY, title: "Workflows" });

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
  const [loadError, setLoadError] = useState<LoadFailure | null>(null);
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
    // The key must hold the workflow's own id; anything else is not looked up or guessed.
    if (!isWorkflowId(id)) {
      setLoadError({ kind: "invalidReference" });
      return;
    }
    try {
      const [workflow, ops] = await Promise.all([workflowCommands.get(id), workflowCommands.operations()]);
      setSaved(workflow);
      setDraftState(workflow.definition);
      setValidation(workflow.validation);
      setContracts(ops);
      setProjectRoot(useWorkspace.getState().info?.root ?? null);
      setLoadError(null);
      editors.setTitle(resource.key, workflow.definition.title || "Workflow draft");
    } catch (error) {
      setLoadError(loadFailure(error));
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
  // Settings, trust, and project changes can make a runner (un)available while
  // this tab is open: re-read the runner catalog and the draft's runnability.
  const availability = useRunnerAvailabilityEpoch();
  const contractsEpoch = useRef(availability);
  useEffect(() => {
    if (contractsEpoch.current === availability) return;
    contractsEpoch.current = availability;
    let live = true;
    workflowCommands.operations().then(
      (ops) => {
        if (live) setContracts(ops);
      },
      () => {
        // Keep the catalog shown; the next change or reopening reads it again.
      },
    );
    return () => {
      live = false;
    };
  }, [availability]);

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

  // Live validation in Rust (debounced), again whenever runner availability
  // may have changed; only the newest answer is shown.
  const validationTicket = useRef(0);
  useEffect(() => {
    if (!draft) return;
    const timer = setTimeout(() => {
      const ticket = ++validationTicket.current;
      workflowCommands.validate(draft, saved?.id ?? null).then(
        (next) => {
          if (ticket === validationTicket.current) setValidation(next);
        },
        (e: unknown) => setSaveError(errorMessage(e)),
      );
    }, 200);
    return () => clearTimeout(timer);
  }, [draft, saved?.id, availability]);

  const save = useCallback(async (): Promise<boolean> => {
    if (!draft || !saved) return false;
    setSaving(true);
    setSaveError(null);
    try {
      const result = await workflowCommands.save({ id: saved.id, expectedRevision: saved.revision, definition: draft });
      // A validation still in flight is older than this answer.
      validationTicket.current++;
      setSaved(result);
      setDraftState(result.definition);
      setValidation(result.validation);
      if (!result.unchanged) workflowsChanged();
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
    const recovery = (
      <>
        <Button variant="primary" onPress={showWorkflows}>
          Show Workflows
        </Button>
        <Button onPress={() => void editors.close(resource.key)}>Close</Button>
      </>
    );
    if (loadError.kind === "invalidReference") {
      return (
        <div className="xp-wf" data-testid="workflow-invalid-reference">
          <UnavailableState
            title="This tab does not point to a saved workflow"
            description="Its reference is not a workflow id, so nothing was opened or changed. Open the workflow from the Workflows list."
            actions={recovery}
          />
        </div>
      );
    }
    if (loadError.kind === "missing") {
      // The backend says when it was deleted here; otherwise it was never saved
      // in this project (its "does not exist" would mislead for another project's).
      const deleted = /deleted/i.test(loadError.message);
      return (
        <div className="xp-wf" data-testid="workflow-missing">
          <UnavailableState
            title="This workflow is not in the open project"
            description={
              deleted
                ? `${loadError.message} A deleted workflow can be restored from Settings › Storage & History.`
                : "It is not saved in this project. A workflow of another project opens when that project is open."
            }
            actions={recovery}
          />
        </div>
      );
    }
    return (
      <div className="xp-wf" data-testid="workflow-load-error">
        <ErrorState title="Could not open this workflow draft" description={loadError.message} actions={<Button onPress={() => void load()}>Retry</Button>} />
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
    <Section title="Inputs">
      <div className="xp-wf__stack">
        <p className="ui-meta">Project files the workflow reads.</p>
        {draft.inputs.length > 0 && (
          <ul className="xp-wf-blocks">
            {draft.inputs.map((input) => (
              <InputRow key={input.id} draft={draft} input={input} diagnostics={diagnosticsFor(diagnostics, { input: input.id })} onChange={setDraft} />
            ))}
          </ul>
        )}
        <div className="xp-wf__row">
          <Select<PortType> label="New input type" value={newInput} onChange={setNewInput} options={FILE_INPUT_TYPES.map((t) => ({ id: t, label: PORT_TYPE_LABELS[t] }))} />
          <Button icon={Plus} onPress={() => setDraft(addInput(draft, newInput))}>
            Add Input
          </Button>
        </div>
      </div>
    </Section>
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
        <p className="ui-meta xp-wf__wide">
          The step runs the chosen workflow at its latest saved revision, pinned: later edits of that workflow never change this step
          until you choose “Use latest revision”. Composites may nest 3 deep and expand to at most 100 steps.
        </p>
      )}
    </div>
  );

  const outcomesSection = (
    <Section title="Outcomes">
      <div className="xp-wf__stack">
        <p className="ui-meta">What the workflow produces. Every step must lead to one.</p>
        {draft.outcomes.length > 0 && (
          <ul className="xp-wf-blocks">
            {draft.outcomes.map((o) => {
              const options = outcomeCandidates(draft, effective, o.kind);
              return (
                <li key={o.id} className="xp-wf-block" data-testid={`workflow-outcome-${o.id}`}>
                  <div className="xp-wf-block__fields">
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
        )}
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
      </div>
    </Section>
  );

  const selectedStep = selection?.kind === "step" ? draft.steps.find((s) => s.id === selection.id) : undefined;
  const selectedInput = selection?.kind === "input" ? draft.inputs.find((i) => i.id === selection.id) : undefined;
  const subtitle = `Revision ${saved.revision} of ${saved.revisions.length}${saved.imported ? " · imported (untrusted)" : ""}${dirty ? " · unsaved changes" : ""}`;

  return (
    <div className="xp-wf" data-testid="workflow-editor" data-workflow={saved.id} data-revision={saved.revision} data-view={view}>
      <PageHeader
        title={draft.title || "Untitled workflow"}
        subtitle={<span title={subtitle}>{subtitle}</span>}
        views={
          <SegmentedControl<EditorView>
            aria-label="Editor view"
            value={view}
            onChange={setView}
            options={[
              { id: "form", label: "Form", icon: ListTree },
              { id: "canvas", label: "Canvas", icon: LayoutGrid },
            ]}
          />
        }
        actions={
          <>
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
            <Button variant="secondary" icon={Save} isPending={saving} isDisabled={saving} onPress={() => void save()} data-testid="workflow-save">
              Save Revision
            </Button>
            <Button
              variant="primary"
              icon={Play}
              isDisabled={runBlock !== null}
              aria-describedby={runBlock ? reasonId : undefined}
              onPress={() => setReviewOpen(true)}
              data-testid="workflow-review-run"
            >
              Review and run…
            </Button>
          </>
        }
        status={
          <span data-testid="workflow-status" data-runnable={String(runnable)} data-state={validation?.state ?? "incomplete"} data-can-review={String(runBlock === null)}>
            <StatusPill
              status={validation?.state === "wellFormed" ? "idle" : "warning"}
              label={validation?.state === "wellFormed" ? "Well-formed draft" : "Incomplete draft"}
              detail={errors ? `${errors} problem${errors === 1 ? "" : "s"}` : undefined}
            />
          </span>
        }
      />
      <div className="xp-wf__body">
        <div className={`xp-wf__page${view === "canvas" ? " xp-wf__page--wide" : ""}`}>
          <StateSizeProvider value="compact">
            <div className="xp-wf__lead">
              {runBlock ? (
                <Banner tone={dirty ? "info" : "warning"} title={dirty ? "Unsaved changes" : "Cannot run this revision"}>
                  <span id={reasonId} data-testid="workflow-run-reason">
                    {runBlock}
                  </span>
                  {dirty && " Saving keeps earlier revisions, and an incomplete draft can be saved."}
                </Banner>
              ) : (
                <Banner tone="info" title={`Revision ${saved.revision} can run`}>
                  Review and run… shows everything the run will read, execute, and send. Nothing runs until you press Start run.
                </Banner>
              )}
              {origin && (
                <div data-testid="workflow-origin" data-origin={saved.origin.kind}>
                  <Banner tone="info" title="Agent proposal">
                    {origin} Like any draft, it runs only from a plan you review and start.
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
            </div>

            {view === "canvas" ? (
              <div className="xp-wf-canvas-view">
                <section className="xp-wf__stack" aria-label="Workflow canvas">
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
                  {addStepRow}
                </section>
                <section className="xp-wf-inspector" aria-label="Inspector" data-testid="workflow-inspector">
                  <PanelHeader title="Inspector" />
                  <InspectorBody>
                    {selectedStep ? (
                      <ul className="xp-wf-blocks">
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
                      <ul className="xp-wf-blocks">
                        <InputRow draft={draft} input={selectedInput} diagnostics={diagnosticsFor(diagnostics, { input: selectedInput.id })} onChange={setDraft} />
                      </ul>
                    ) : (
                      <EmptyState title="Select a step or an input on the canvas to edit it here." description="The Form view shows every part of the workflow." />
                    )}
                  </InspectorBody>
                </section>
              </div>
            ) : (
              <>
                <Section title="General">
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
                      }))}
                      description="Independent steps and repeat instances run side by side up to this limit and the application's global limits (4 steps in flight, 2 containers, 2 native, 2 model, 1 Lean)."
                      data-testid="workflow-max-parallel"
                    />
                    <div className="xp-wf__wide">
                      <TextArea label="Description" rows={4} value={draft.description} onChange={(description) => setDraft({ ...draft, description })} />
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
                </Section>

                {inputsSection}

                <Section title="Steps">
                  <div className="xp-wf__stack">
                    <p className="ui-meta">
                      Each step runs an operation or another saved workflow (at a pinned revision) and reads only the data bound to its
                      inputs. Conditions and repeats are optional and bounded.
                    </p>
                    {draft.steps.length > 0 && (
                      <ul className="xp-wf-blocks">
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
                    )}
                    {addStepRow}
                  </div>
                </Section>

                {outcomesSection}

                {validation && validation.plan.length > 0 && (
                  <Section title="Order of steps">
                    <ol className="xp-wf-order" data-testid="workflow-plan">
                      {validation.plan.map((p) => (
                        <li key={p.step}>
                          <span className="xp-wf-name">{p.label}</span> — {p.operation} via {p.runner}. Effects:{" "}
                          {p.effects.map((e) => (EFFECT_LABELS[e] ?? e).toLowerCase()).join("; ")}.
                        </li>
                      ))}
                    </ol>
                  </Section>
                )}
              </>
            )}

            <RunHistory workflow={saved.id} />
            <TriggersSection workflow={saved.id} revision={saved.revision} definition={saved.definition} reviewBlock={runBlock} />
          </StateSizeProvider>
        </div>
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
