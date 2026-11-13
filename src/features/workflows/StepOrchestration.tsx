// Form sections for M9 step orchestration, shown in a step's card (the
// Form view and the canvas inspector): the composite reference (child title,
// pinned revision, its steps read-only, "Use latest revision" as an explicit
// parent edit), a bounded condition (source limited to upstream outputs a
// fixed predicate can read; verdict set / JSON comparison / presence;
// negate), and a bounded repeat (forEach over ≤ 20 values of a declared
// parameter, or until a predicate on the step's own output holds, ≤ 10
// iterations). Every edit is re-validated in Rust; its diagnostics appear in
// the card as for any other problem.
import { ChevronDown, ChevronRight, Plus, RefreshCw, X } from "lucide-react";
import { useState } from "react";

import type { OperationContract } from "../../bindings/OperationContract";
import type { ParameterSpec } from "../../bindings/ParameterSpec";
import type { ParamValue } from "../../bindings/ParamValue";
import type { PortType } from "../../bindings/PortType";
import { Banner, Button, Checkbox, IconButton, Select, TextField } from "../../ui/components";
import type { CompareOp, ConditionTest, ConditionValue, StepRepeat, WorkflowDefinition, WorkflowStep } from "./contract";
import { conditionSources, contractOf, outputLabel, PORT_TYPE_LABELS } from "./model";
import {
  compareOpsFor,
  defaultParamValue,
  defaultTest,
  describeCondition,
  describeRepeat,
  isComposite,
  KNOWN_VERDICTS,
  MAX_FOR_EACH_VALUES,
  MAX_UNTIL_ITERATIONS,
  numberProblem,
  pointerProblem,
  setCompositeRevision,
  setCondition,
  setRepeat,
  TEST_KIND_LABELS,
  testKindsFor,
  type TestKind,
} from "./orchestration";
import type { CompositeInfo } from "./useComposites";
import { compositeKey } from "./orchestration";

// ----- predicate --------------------------------------------------------------------------------

const VALUE_KINDS: { id: ConditionValue["kind"]; label: string }[] = [
  { id: "number", label: "Number" },
  { id: "text", label: "Text" },
  { id: "boolean", label: "True or false" },
];

/** The fixed predicate form (no expression language): kinds limited to what the source type allows. */
export function TestFields({
  test,
  type,
  optional,
  idPrefix,
  label,
  onChange,
}: {
  test: ConditionTest;
  type: PortType | null;
  optional: boolean;
  idPrefix: string;
  label: string;
  onChange: (test: ConditionTest) => void;
}) {
  const kinds = testKindsFor(type, optional);
  if (!kinds.includes(test.kind)) kinds.push(test.kind);
  const pointerHint = test.kind === "jsonCompare" ? pointerProblem(test.pointer) : null;
  const numberHint = test.kind === "jsonCompare" && test.value.kind === "number" ? numberProblem(test.value.value) : null;
  return (
    <div className="xp-wf__form" data-testid={`${idPrefix}-test`} data-kind={test.kind}>
      <Select<TestKind>
        label={label}
        value={test.kind}
        onChange={(kind) => onChange(defaultTest(kind))}
        options={kinds.map((k) => ({ id: k, label: TEST_KIND_LABELS[k] }))}
        data-testid={`${idPrefix}-kind`}
      />
      {test.kind === "verdictIn" && (
        <fieldset className="xp-wf__fieldset xp-wf__wide" data-testid={`${idPrefix}-verdicts`}>
          <legend className="ui-field__label">Exact Lean verdicts that count</legend>
          <div className="xp-wf__inline">
            {KNOWN_VERDICTS.map((v) => (
              <Checkbox
                key={v}
                isSelected={test.verdicts.includes(v)}
                onChange={(on) => onChange({ ...test, verdicts: on ? [...test.verdicts, v] : test.verdicts.filter((x) => x !== v) })}
              >
                {v}
              </Checkbox>
            ))}
          </div>
        </fieldset>
      )}
      {test.kind === "jsonCompare" && (
        <>
          <TextField
            label="JSON pointer (RFC 6901)"
            code
            value={test.pointer}
            placeholder="/summary/slope"
            description="Empty compares the whole document. The dataset must be JSON of at most 1 MiB."
            errorMessage={pointerHint ?? undefined}
            isInvalid={pointerHint !== null}
            onChange={(pointer) => onChange({ ...test, pointer })}
            data-testid={`${idPrefix}-pointer`}
          />
          <Select<CompareOp>
            label="Comparison"
            value={test.op}
            onChange={(op) => onChange({ ...test, op })}
            options={compareOpsFor(test.value.kind).map((o) => ({ id: o.id, label: `${o.symbol}  ${o.label}` }))}
            data-testid={`${idPrefix}-op`}
          />
          <Select<ConditionValue["kind"]>
            label="Value type"
            value={test.value.kind}
            onChange={(kind) =>
              onChange({
                ...test,
                // Text and true/false compare for equality only.
                op: compareOpsFor(kind).some((o) => o.id === test.op) ? test.op : "eq",
                value: kind === "boolean" ? { kind, value: true } : { kind, value: kind === "number" ? "0" : "" },
              })
            }
            options={VALUE_KINDS}
          />
          {test.value.kind === "boolean" ? (
            <Select<string>
              label="Value"
              value={String(test.value.value)}
              onChange={(v) => onChange({ ...test, value: { kind: "boolean", value: v === "true" } })}
              options={[
                { id: "true", label: "true" },
                { id: "false", label: "false" },
              ]}
            />
          ) : (
            <TextField
              label="Value"
              code={test.value.kind === "number"}
              value={test.value.value}
              errorMessage={numberHint ?? undefined}
              isInvalid={numberHint !== null}
              onChange={(value) => onChange({ ...test, value: { kind: test.value.kind as "number" | "text", value } })}
              data-testid={`${idPrefix}-value`}
            />
          )}
        </>
      )}
    </div>
  );
}

// ----- condition --------------------------------------------------------------------------------

export function ConditionSection({
  draft,
  contracts,
  step,
  onChange,
}: {
  draft: WorkflowDefinition;
  contracts: OperationContract[];
  step: WorkflowStep;
  onChange: (def: WorkflowDefinition) => void;
}) {
  const condition = step.condition ?? null;
  if (isComposite(step)) {
    return (
      <div className="xp-wf__multi xp-wf-orch" data-testid={`condition-${step.id}`} data-enabled="false">
        <p className="xp-wf__muted">
          A step that runs a saved workflow cannot carry a condition; put conditions on the steps inside that workflow.
        </p>
        {condition && (
          <div>
            <Button size="sm" icon={X} onPress={() => onChange(setCondition(draft, step.id, null))}>
              Remove the condition
            </Button>
          </div>
        )}
      </div>
    );
  }
  const sources = conditionSources(draft, contracts, step.id);
  const current = condition ? sources.find((o) => o.step === condition.source.step && o.port === condition.source.port) : undefined;
  const sourceKey = condition ? `${condition.source.step}.${condition.source.port}` : null;
  const enable = (on: boolean) => {
    if (!on) return onChange(setCondition(draft, step.id, null));
    const first = sources[0];
    if (!first) return;
    const kind = testKindsFor(first.type, first.optional)[0] ?? "outputPresent";
    onChange(setCondition(draft, step.id, { source: { step: first.step, port: first.port }, test: defaultTest(kind), negate: false }));
  };
  return (
    <div className="xp-wf__multi xp-wf-orch" data-testid={`condition-${step.id}`} data-enabled={String(condition !== null)}>
      <Checkbox isSelected={condition !== null} isDisabled={!condition && sources.length === 0} onChange={enable}>
        Run this step only when a condition holds
      </Checkbox>
      {!condition && sources.length === 0 && (
        <p className="xp-wf__muted">
          A condition tests an earlier step's formal evidence (its verdict), a JSON dataset, or an optional output. No such output can
          come before this step yet.
        </p>
      )}
      {condition && (
        <>
          <div className="xp-wf__form">
            <Select<string>
              label="Tests the output"
              value={sourceKey}
              onChange={(key) => {
                const o = sources.find((x) => x.key === key);
                if (!o) return;
                const kinds = testKindsFor(o.type, o.optional);
                const test = kinds.includes(condition.test.kind) ? condition.test : defaultTest(kinds[0] ?? "outputPresent");
                onChange(setCondition(draft, step.id, { ...condition, source: { step: o.step, port: o.port }, test }));
              }}
              options={[
                ...(current || !sourceKey ? [] : [{ id: sourceKey, label: `${outputLabel(draft, contracts, condition.source.step, condition.source.port)} (not an earlier output)` }]),
                ...sources.map((o) => ({ id: o.key, label: o.label, description: o.type ? PORT_TYPE_LABELS[o.type] + (o.optional ? " · optional" : "") : undefined })),
              ]}
              data-testid={`condition-source-${step.id}`}
            />
            <Checkbox isSelected={condition.negate} onChange={(negate) => onChange(setCondition(draft, step.id, { ...condition, negate }))}>
              Negate: run when this does not hold
            </Checkbox>
          </div>
          <TestFields
            test={condition.test}
            type={current?.type ?? null}
            optional={current?.optional ?? false}
            idPrefix={`condition-${step.id}`}
            label="Holds when"
            onChange={(test) => onChange(setCondition(draft, step.id, { ...condition, test }))}
          />
          <p className="xp-wf__muted" data-testid={`condition-summary-${step.id}`}>
            {describeCondition(condition, (s, p) => `“${outputLabel(draft, contracts, s, p)}”`)} An unreadable source fails the step.
          </p>
        </>
      )}
    </div>
  );
}

// ----- repeat -----------------------------------------------------------------------------------

type RepeatKind = "once" | StepRepeat["kind"];

function ValueField({ spec, value, index, onChange }: { spec: ParameterSpec; value: ParamValue; index: number; onChange: (v: ParamValue) => void }) {
  const label = `Value ${index + 1}`;
  if (spec.kind.kind === "boolean") {
    return (
      <Select<string>
        label={label}
        value={value.kind === "boolean" ? String(value.value) : "true"}
        onChange={(v) => onChange({ kind: "boolean", value: v === "true" })}
        options={[
          { id: "true", label: "yes" },
          { id: "false", label: "no" },
        ]}
      />
    );
  }
  if (spec.kind.kind === "integer") {
    const { min, max } = spec.kind;
    const text = value.kind === "integer" ? String(value.value) : "";
    const n = Number.parseInt(text, 10);
    const out = Number.isNaN(n) || n < min || n > max;
    return (
      <TextField
        label={`${label} (${min}–${max})`}
        value={text}
        isInvalid={out}
        errorMessage={out ? `Between ${min} and ${max}.` : undefined}
        onChange={(v) => {
          const parsed = Number.parseInt(v, 10);
          onChange({ kind: "integer", value: Number.isNaN(parsed) ? min : parsed });
        }}
      />
    );
  }
  return (
    <TextField
      label={label}
      code={spec.kind.kind === "path"}
      value={value.kind === "text" ? value.value : ""}
      onChange={(v) => onChange({ kind: "text", value: v })}
    />
  );
}

export function RepeatSection({
  draft,
  contracts,
  step,
  onChange,
}: {
  draft: WorkflowDefinition;
  contracts: OperationContract[];
  step: WorkflowStep;
  onChange: (def: WorkflowDefinition) => void;
}) {
  const contract = contractOf(contracts, step);
  const repeat = step.repeat ?? null;
  if (isComposite(step) && !repeat) {
    return (
      <p className="xp-wf__muted xp-wf-orch" data-testid={`repeat-${step.id}`} data-kind="once">
        A step that runs a saved workflow runs once; repeat steps inside that workflow instead.
      </p>
    );
  }
  const params = contract?.parameters ?? [];
  const outputs = contract?.outputs ?? [];
  const kind: RepeatKind = repeat?.kind ?? "once";
  const options: { id: RepeatKind; label: string }[] = [
    { id: "once", label: "Run once" },
    ...(params.length ? [{ id: "forEach" as const, label: "Once for each value of a parameter" }] : []),
    ...(outputs.length ? [{ id: "until" as const, label: "Again until a condition on its output holds" }] : []),
  ];
  if (!options.some((o) => o.id === kind)) options.push({ id: kind, label: kind === "forEach" ? "For each value" : "Until" });

  const choose = (next: RepeatKind) => {
    if (next === "once") return onChange(setRepeat(draft, step.id, null));
    if (next === "forEach") {
      const spec = params[0];
      if (!spec) return;
      const existing = step.parameters[spec.name];
      return onChange(setRepeat(draft, step.id, { kind: "forEach", parameter: spec.name, values: [existing ?? defaultParamValue(spec, 0)] }));
    }
    const port = outputs[0];
    if (!port) return;
    const kinds = testKindsFor(port.types[0] ?? null, !port.required);
    onChange(
      setRepeat(draft, step.id, { kind: "until", port: port.name, test: defaultTest(kinds[0] ?? "outputPresent"), negate: false, maxIterations: 3, onExhausted: "fail" }),
    );
  };

  const own = (port: string) => `its output “${outputs.find((o) => o.name === port)?.label ?? port}”`;
  const paramLabel = (name: string) => `“${params.find((p) => p.name === name)?.label ?? name}”`;

  return (
    <div className="xp-wf__multi xp-wf-orch" data-testid={`repeat-${step.id}`} data-kind={kind}>
      <div className="xp-wf__form">
        <Select<RepeatKind> label="Repeat" value={kind} onChange={choose} options={options} data-testid={`repeat-kind-${step.id}`} />
      </div>
      {repeat?.kind === "forEach" && (
        <ForEachFields draft={draft} step={step} repeat={repeat} params={params} onChange={onChange} />
      )}
      {repeat?.kind === "until" && (
        <>
          <div className="xp-wf__form">
            <Select<string>
              label="Until its output"
              value={repeat.port}
              onChange={(port) => {
                const spec = outputs.find((o) => o.name === port);
                const kinds = testKindsFor(spec?.types[0] ?? null, spec ? !spec.required : false);
                const test = kinds.includes(repeat.test.kind) ? repeat.test : defaultTest(kinds[0] ?? "outputPresent");
                onChange(setRepeat(draft, step.id, { ...repeat, port, test }));
              }}
              options={outputs.map((o) => ({ id: o.name, label: o.label, description: o.types[0] ? PORT_TYPE_LABELS[o.types[0]] : undefined }))}
            />
            <Checkbox isSelected={repeat.negate} onChange={(negate) => onChange(setRepeat(draft, step.id, { ...repeat, negate }))}>
              Negate: stop when this does not hold
            </Checkbox>
          </div>
          <TestFields
            test={repeat.test}
            type={outputs.find((o) => o.name === repeat.port)?.types[0] ?? null}
            optional={!(outputs.find((o) => o.name === repeat.port)?.required ?? true)}
            idPrefix={`repeat-${step.id}`}
            label="Stops when"
            onChange={(test) => onChange(setRepeat(draft, step.id, { ...repeat, test }))}
          />
          <div className="xp-wf__form">
            <Select<number>
              label="At most (iterations)"
              value={repeat.maxIterations}
              onChange={(maxIterations) => onChange(setRepeat(draft, step.id, { ...repeat, maxIterations }))}
              options={Array.from({ length: MAX_UNTIL_ITERATIONS }, (_, i) => ({ id: i + 1, label: String(i + 1) }))}
              data-testid={`repeat-max-${step.id}`}
            />
            <Select<string>
              label="If it never holds"
              value={repeat.onExhausted}
              onChange={(v) => onChange(setRepeat(draft, step.id, { ...repeat, onExhausted: v === "useLast" ? "useLast" : "fail" }))}
              options={[
                { id: "fail", label: "Fail the step" },
                { id: "useLast", label: "Use the last iteration's output" },
              ]}
            />
          </div>
        </>
      )}
      {repeat && (
        <p className="xp-wf__muted" data-testid={`repeat-summary-${step.id}`}>
          {describeRepeat(repeat, own, paramLabel)}
        </p>
      )}
    </div>
  );
}

function ForEachFields({
  draft,
  step,
  repeat,
  params,
  onChange,
}: {
  draft: WorkflowDefinition;
  step: WorkflowStep;
  repeat: Extract<StepRepeat, { kind: "forEach" }>;
  params: ParameterSpec[];
  onChange: (def: WorkflowDefinition) => void;
}) {
  const spec = params.find((p) => p.name === repeat.parameter);
  const set = (values: ParamValue[]) => onChange(setRepeat(draft, step.id, { ...repeat, values }));
  return (
    <>
      <div className="xp-wf__form">
        <Select<string>
          label="For each value of"
          value={repeat.parameter}
          onChange={(parameter) => {
            const next = params.find((p) => p.name === parameter);
            if (next) onChange(setRepeat(draft, step.id, { kind: "forEach", parameter, values: [defaultParamValue(next, 0)] }));
          }}
          options={params.map((p) => ({ id: p.name, label: p.label }))}
          data-testid={`repeat-parameter-${step.id}`}
        />
      </div>
      {spec ? (
        <ol className="xp-wf-values" aria-label={`Values of ${spec.label}`} data-testid={`repeat-values-${step.id}`}>
          {repeat.values.map((v, i) => (
            <li key={i} className="xp-wf-values__item">
              <ValueField spec={spec} value={v} index={i} onChange={(nv) => set(repeat.values.map((x, j) => (j === i ? nv : x)))} />
              <IconButton
                icon={X}
                size="sm"
                aria-label={`Remove value ${i + 1}`}
                isDisabled={repeat.values.length <= 1}
                onPress={() => set(repeat.values.filter((_, j) => j !== i))}
              />
            </li>
          ))}
        </ol>
      ) : (
        <Banner tone="warning" title="Unknown parameter">
          {repeat.parameter} is not a parameter of this operation. Choose one above.
        </Banner>
      )}
      {spec && (
        <div className="xp-wf__row">
          <Button
            size="sm"
            icon={Plus}
            isDisabled={repeat.values.length >= MAX_FOR_EACH_VALUES}
            onPress={() => set([...repeat.values, defaultParamValue(spec, repeat.values.length)])}
            data-testid={`repeat-add-value-${step.id}`}
          >
            Add value
          </Button>
          <span className="xp-wf__muted">
            {repeat.values.length} of at most {MAX_FOR_EACH_VALUES}. Instances may run at the same time within the workflow's limit. Its
            outputs can feed only inputs that accept several sources.
          </span>
        </div>
      )}
    </>
  );
}

// ----- composite --------------------------------------------------------------------------------

function ChildSteps({ def, infos, depth }: { def: WorkflowDefinition; infos: ReadonlyMap<string, CompositeInfo>; depth: number }) {
  return (
    <ol className="xp-wf-children">
      {def.steps.map((s) => {
        const nested = s.composite ? infos.get(compositeKey(s.composite)) : undefined;
        return (
          <li key={s.id} data-child-step={s.id}>
            <span>{s.label}</span>{" "}
            <span className="xp-wf__muted">
              {s.composite
                ? `runs “${nested?.saved?.definition.title ?? s.composite.workflow}” revision ${s.composite.revision}`
                : `${s.operation.id} v${s.operation.version}`}
              {s.condition ? " · conditional" : ""}
              {s.repeat ? (s.repeat.kind === "forEach" ? ` · for each of ${s.repeat.values.length} values` : ` · until, at most ${s.repeat.maxIterations}`) : ""}
            </span>
            {nested?.saved && depth < 3 && <ChildSteps def={nested.saved.definition} infos={infos} depth={depth + 1} />}
          </li>
        );
      })}
    </ol>
  );
}

export function CompositeSection({
  draft,
  step,
  infos,
  onChange,
}: {
  draft: WorkflowDefinition;
  step: WorkflowStep;
  infos: ReadonlyMap<string, CompositeInfo>;
  onChange: (def: WorkflowDefinition) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  if (!isComposite(step)) return null;
  const ref = step.composite;
  if (!ref) {
    return (
      <Banner tone="warning" title="No workflow chosen">
        This step runs another saved workflow; remove it and add it again choosing the workflow.
      </Banner>
    );
  }
  const info = infos.get(compositeKey(ref));
  const title = info?.saved?.definition.title ?? ref.workflow;
  const newer = info?.latest !== null && info?.latest !== undefined && info.latest > ref.revision ? info.latest : null;
  return (
    <div className="xp-wf__multi xp-wf-orch" data-testid={`composite-${step.id}`} data-workflow={ref.workflow} data-revision={ref.revision}>
      <p className="xp-wf__muted">
        Runs the saved workflow <strong>“{title}”</strong> at revision <span data-testid={`composite-revision-${step.id}`}>{ref.revision}</span>{" "}
        (pinned). Its inputs and outcomes are this step's ports; its steps appear expanded in the plan review. Editing that workflow
        makes a new revision and never changes this pin by itself.
      </p>
      {info?.error && (
        <Banner tone="danger" title={`Revision ${ref.revision} could not be loaded`}>
          {info.error}
        </Banner>
      )}
      {newer !== null && (
        <Banner
          tone="info"
          title={`Revision ${newer} of “${title}” is newer`}
          actions={
            <Button size="sm" icon={RefreshCw} onPress={() => onChange(setCompositeRevision(draft, step.id, newer))} data-testid={`composite-use-latest-${step.id}`}>
              Use latest revision ({newer})
            </Button>
          }
        >
          This step still runs revision {ref.revision}. Using the latest is an edit of this workflow: review the ports, then save.
        </Banner>
      )}
      {info?.saved && (
        <div>
          <Button
            size="sm"
            variant="ghost"
            icon={expanded ? ChevronDown : ChevronRight}
            aria-expanded={expanded}
            onPress={() => setExpanded(!expanded)}
            data-testid={`composite-expand-${step.id}`}
          >
            {expanded ? "Hide" : "Show"} its {info.saved.definition.steps.length} step{info.saved.definition.steps.length === 1 ? "" : "s"} (read-only)
          </Button>
          {expanded && (
            <div data-testid={`composite-children-${step.id}`}>
              <ChildSteps def={info.saved.definition} infos={infos} depth={1} />
            </div>
          )}
        </div>
      )}
    </div>
  );
}
