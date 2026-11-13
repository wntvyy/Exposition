// Run monitor (editor tab `workflow-run:<id>`): the run's state, usage
// against its budgets, every step with its attempts (service operation,
// timings, failure code/message, bounded log), validated outputs per port,
// refused outputs, outcomes, and reviewable proposals. Live through the
// `workflow-run` event (the run is re-read on each event for it).
//
// Outputs are shown inertly: images (SVG included) only through <img> from a
// blob URL of the research artifact bytes; CSV/TSV as plain cells; anything
// else by path and hash. Markup is never inserted into the page.
//
// M9: several steps (and repeat instances) may be in flight at once — the
// "Running now" list shows them; repeated steps list their instances with the
// parameter value or iteration and state; a step skipped by its own condition
// is "Condition not met" with its reason (distinct from Skipped: an earlier
// step did not produce what it needs); unproduced outcomes give their reason;
// the run states its cause; each attempt shows its recorded environment.
// Export manifest writes the run manifest; Reproduce opens a new plan pinned
// to this run's inputs, with environment drift notices, approved like any.
import { Ban, Check, CircleSlash, Cpu, Eye, FileDown, FileWarning, History, RotateCcw, Square, X, Zap } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import type { EditorProps } from "../../app/registry";
import type { PromotionPreview } from "../../bindings/PromotionPreview";
import type { WorkflowOutputValue } from "../../bindings/WorkflowOutputValue";
import type { WorkflowStepOutput } from "../../bindings/WorkflowStepOutput";
import type {
  WorkflowEnvironment,
  WorkflowManifestExport,
  WorkflowPlanStep,
  WorkflowRun,
  WorkflowRunCause,
  WorkflowRunStep,
  WorkflowRunSummary,
  WorkflowStepAttempt,
  WorkflowStepInstance,
} from "./contract";
import { commands, errorMessage } from "../../lib/ipc";
import {
  Badge,
  Banner,
  Button,
  Checkbox,
  EmptyState,
  ErrorState,
  LoadingState,
  ProgressBar,
  Section,
  Spinner,
  StatusPill,
} from "../../ui/components";
import { researchCommands } from "../research/ipc";
import { PreviewView } from "../research/Promotion";
import { workflowEvents, workflowRunCommands } from "./ipc";
import { PORT_TYPE_LABELS } from "./model";
import { describeRepeat, describeTest } from "./orchestration";
import { FullHash, PlanDetails, PlanReviewDialog } from "./PlanReview";
import {
  ATTEMPT_STATE_VIEW,
  auditedDeclarations,
  CONTAINMENT_VIEW,
  describeCause,
  environmentFacts,
  filePreviewKind,
  formatBytes,
  formatDuration,
  formatTime,
  instanceCounts,
  instanceLabel,
  parseDelimited,
  PROPOSAL_STATUS_VIEW,
  RUN_STATE_VIEW,
  runningNow,
  shortHash,
  STEP_STATE_VIEW,
  usageRows,
  verdictTone,
  WORKFLOW_RUN_KEY_PREFIX,
  workflowRunKey,
} from "./runModel";

export function openRun(run: { id: string; title: string }) {
  editors.open({ key: workflowRunKey(run.id), title: `Run: ${run.title}` });
}

// ----- outputs -----------------------------------------------------------------------------

type Bytes = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; bytes: ArrayBuffer };

function useArtifactBytes(runId: string | null, artifact: string | null): Bytes | null {
  const [state, setState] = useState<Bytes | null>(runId && artifact ? { kind: "loading" } : null);
  useEffect(() => {
    if (!runId || !artifact) return;
    let live = true;
    setState({ kind: "loading" });
    commands.researchArtifact(runId, artifact).then(
      (bytes) => live && setState({ kind: "ready", bytes }),
      (e: unknown) => live && setState({ kind: "error", message: errorMessage(e) }),
    );
    return () => {
      live = false;
    };
  }, [runId, artifact]);
  return state;
}

/** An image through <img> from a blob URL (an SVG in <img> is inert: no scripts, no external loads). */
function ImagePreview({ bytes, mediaType, alt }: { bytes: ArrayBuffer; mediaType: string; alt: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    const next = URL.createObjectURL(new Blob([bytes], { type: mediaType }));
    setUrl(next);
    return () => URL.revokeObjectURL(next);
  }, [bytes, mediaType]);
  return url ? <img className="xp-wf-output__image" src={url} alt={alt} data-testid="workflow-output-image" /> : null;
}

function TablePreview({ bytes, path }: { bytes: ArrayBuffer; path: string }) {
  const grid = useMemo(() => {
    const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
    return parseDelimited(text, path.toLowerCase().endsWith(".tsv") ? "\t" : ",", 100);
  }, [bytes, path]);
  if (grid.rows.length === 0) return <p className="xp-wf__muted">The table is empty.</p>;
  const [head, ...body] = grid.rows;
  return (
    <div className="xp-wf-output__table" data-testid="workflow-output-table">
      <table className="xp-wf-table">
        <thead>
          <tr>
            {head!.map((c, i) => (
              <th key={i} scope="col">
                {c}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {body.map((r, i) => (
            <tr key={i}>
              {r.map((c, j) => (
                <td key={j}>{c}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {grid.truncated && (
        <p className="xp-wf__muted">
          Showing {grid.rows.length} of {grid.totalRows} rows.
        </p>
      )}
    </div>
  );
}

function FilePreview({ value }: { value: Extract<WorkflowOutputValue, { kind: "file" }> }) {
  const kind = filePreviewKind(value.mediaType, value.path);
  const bytes = useArtifactBytes(kind === "none" ? null : value.researchRun, kind === "none" ? null : value.artifact);
  if (kind === "none") return null;
  if (!value.researchRun || !value.artifact) return <p className="xp-wf__muted">No preview: this file is not a research run artifact.</p>;
  if (!bytes || bytes.kind === "loading") return <Spinner size="sm" aria-label={`Loading a preview of ${value.path}`} />;
  if (bytes.kind === "error") return <p className="xp-wf__muted">No preview: {bytes.message}</p>;
  return kind === "image" ? (
    <ImagePreview bytes={bytes.bytes} mediaType={value.mediaType} alt={`Preview of ${value.path}`} />
  ) : (
    <TablePreview bytes={bytes.bytes} path={value.path} />
  );
}

function PromotionReview({
  run,
  value,
  onRun,
}: {
  run: WorkflowRun;
  value: Extract<WorkflowOutputValue, { kind: "promotionProposal" }>;
  onRun: (run: WorkflowRun) => void;
}) {
  const [open, setOpen] = useState(false);
  const [preview, setPreview] = useState<PromotionPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const pending = value.status === "pending";

  const load = async () => {
    setOpen(true);
    setError(null);
    try {
      const found = await workflowRunCommands.promotionPreview(value.promotion);
      if (!found) setError("This review is no longer pending: it was applied, discarded, or expired. Review a new run to prepare it again.");
      setPreview(found);
    } catch (e) {
      setError(errorMessage(e));
    }
  };
  const decide = async (approve: boolean) => {
    setBusy(true);
    setError(null);
    try {
      if (approve) {
        const outcome = await researchCommands.approvePromotion(value.promotion);
        notify({ tone: "success", title: `Inserted into ${outcome.path}`, body: `Attachment ${outcome.attachmentKey}. Undo is available in the document.` });
      } else {
        await researchCommands.cancelPromotion(value.promotion);
      }
      setOpen(false);
      setPreview(null);
      onRun(await workflowRunCommands.get(run.id));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="xp-wf-proposal" data-testid="workflow-promotion-proposal" data-promotion={value.promotion} data-status={value.status}>
      <div className="xp-wf__row">
        <span className="xp-wf__grow">
          Manuscript change for <code>{value.document}</code>
        </span>
        <Badge tone={PROPOSAL_STATUS_VIEW[value.status].tone} dashed={PROPOSAL_STATUS_VIEW[value.status].dashed}>
          {PROPOSAL_STATUS_VIEW[value.status].label}
        </Badge>
        {pending && !open && (
          <Button size="sm" icon={Eye} onPress={() => void load()} data-testid="workflow-promotion-review">
            Review the change…
          </Button>
        )}
      </div>
      {pending && value.expiresAtMs !== null && (
        <p className="xp-wf__muted" data-testid="workflow-promotion-expiry">
          The prepared change can be reviewed until {formatTime(value.expiresAtMs)}.
        </p>
      )}
      {value.status === "accepted" && value.attachmentKey !== null && (
        <p className="xp-wf__muted" data-testid="workflow-promotion-attachment">
          Inserted as attachment <code>{value.attachmentKey}</code>; undo it in the document.
        </p>
      )}
      {open && (
        <div className="xp-wf-proposal__review">
          {error && (
            <Banner tone="danger" title="The change could not be reviewed" live>
              {error} Nothing was changed.
            </Banner>
          )}
          {!preview && !error && <Spinner size="sm" aria-label="Loading the prepared change" />}
          {preview && <PreviewView preview={preview} />}
          <div className="xp-wf__row">
            <Button size="sm" variant="secondary" isDisabled={busy} onPress={() => void decide(false)}>
              Discard proposal
            </Button>
            <Button size="sm" variant="primary" icon={Check} isPending={busy} isDisabled={!preview || busy} onPress={() => void decide(true)} data-testid="workflow-promotion-approve">
              {preview?.update ? "Approve update" : "Approve insertion"}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

function AssociationReview({
  run,
  step,
  value,
  onRun,
}: {
  run: WorkflowRun;
  step: string;
  value: Extract<WorkflowOutputValue, { kind: "associationProposal" }>;
  onRun: (run: WorkflowRun) => void;
}) {
  const audited = useMemo(() => auditedDeclarations(run), [run]);
  const [selected, setSelected] = useState<string[]>([]);
  const [busy, setBusy] = useState<"accept" | "reject" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pending = value.status === "pending";
  const status = PROPOSAL_STATUS_VIEW[value.status];

  const decide = async (accept: string[], which: "accept" | "reject") => {
    setBusy(which);
    setError(null);
    try {
      onRun(await workflowRunCommands.decideAssociation({ run: run.id, step, accept }));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="xp-wf-proposal" data-testid="workflow-association-proposal" data-proposal={value.proposal} data-status={value.status}>
      <div className="xp-wf__row">
        <span className="xp-wf__grow">Associate audited declarations with their evidence</span>
        <Badge tone={status.tone} dashed={status.dashed}>
          {status.label}
        </Badge>
      </div>
      <p className="xp-wf__muted">
        Accepting records each selected declaration's audit evidence. The verdict is the Lean audit's own; an association is a
        reviewed correspondence, not a machine proof of anything else.
      </p>
      <ul className="xp-wf-checklist" data-testid="workflow-association-checklist" aria-label="Declarations to associate">
        {value.declarations.map((name) => {
          const facts = audited.get(name);
          return (
            <li key={name} data-declaration={name} data-verdict={facts?.verdict ?? "unknown"} data-associated={!pending ? String(value.accepted.includes(name)) : undefined}>
              {pending ? (
                <Checkbox isSelected={selected.includes(name)} onChange={(on) => setSelected(on ? [...selected, name] : selected.filter((n) => n !== name))}>
                  <code>{name}</code>
                </Checkbox>
              ) : (
                <code>{name}</code>
              )}
              {!pending &&
                (value.accepted.includes(name) ? (
                  <Badge tone="success">
                    Associated
                  </Badge>
                ) : (
                  <Badge tone="neutral" dashed>
                    Not associated
                  </Badge>
                ))}
              {facts ? (
                <Badge tone={verdictTone(facts.verdict)}>{facts.verdict}</Badge>
              ) : (
                <Badge tone="neutral" dashed>
                  Verdict not in this run
                </Badge>
              )}
              {facts && facts.axioms.length > 0 && <span className="xp-wf__muted">Axioms: {facts.axioms.join(", ")}</span>}
              {facts?.statement && <code className="xp-wf-checklist__statement">{facts.statement}</code>}
            </li>
          );
        })}
      </ul>
      {error && (
        <Banner tone="danger" title="Not recorded" live>
          {error}
        </Banner>
      )}
      {pending && (
        <div className="xp-wf__row">
          <Button size="sm" variant="secondary" icon={X} isPending={busy === "reject"} isDisabled={busy !== null} onPress={() => void decide([], "reject")} data-testid="workflow-association-reject">
            Reject all
          </Button>
          <Button
            size="sm"
            variant="primary"
            icon={Check}
            isPending={busy === "accept"}
            isDisabled={busy !== null || selected.length === 0}
            onPress={() => void decide(selected, "accept")}
            data-testid="workflow-association-accept"
          >
            Accept selected ({selected.length})
          </Button>
        </div>
      )}
    </div>
  );
}

function OutputValueView({ run, step, value, onRun }: { run: WorkflowRun; step: string; value: WorkflowOutputValue; onRun: (run: WorkflowRun) => void }) {
  switch (value.kind) {
    case "file":
      return (
        <>
          <div className="xp-wf-output__facts">
            <code>{value.path}</code> · {value.mediaType} · {formatBytes(value.bytes)} · <FullHash hash={value.sha256} label={value.path} />
          </div>
          <FilePreview value={value} />
        </>
      );
    case "retained":
      return (
        <div className="xp-wf-output__facts">
          Kept in Research: <code>{value.path}</code> · <FullHash hash={value.sha256} label={value.path} />
          {value.evidence ? " · evidence recorded" : ""}
        </div>
      );
    case "formalEvidence":
      return (
        <div>
          <div className="xp-wf-output__facts">
            Lean audit of <code>{value.file}</code> · <FullHash hash={value.fileSha256} label={value.file} /> · file verdict{" "}
            <Badge tone={verdictTone(value.verdict)}>{value.verdict}</Badge>
          </div>
          <ul className="xp-wf-checklist" aria-label="Audited declarations">
            {value.declarations.map((d) => (
              <li key={d.name}>
                <code>{d.name}</code> <Badge tone={verdictTone(d.verdict)}>{d.verdict}</Badge>
                {d.axioms.length > 0 && <span className="xp-wf__muted">Axioms: {d.axioms.join(", ")}</span>}
              </li>
            ))}
          </ul>
        </div>
      );
    case "report":
      return <pre className="xp-wf-log" aria-label="Report">{value.text}</pre>;
    case "promotionProposal":
      return <PromotionReview run={run} value={value} onRun={onRun} />;
    case "associationProposal":
      return <AssociationReview run={run} step={step} value={value} onRun={onRun} />;
  }
}

/** The repeat instance that produced an output (lane B adds `provenance.instance`; absent before M9). */
function provenanceInstance(provenance: object): number | null {
  const value = (provenance as { instance?: unknown }).instance;
  return typeof value === "number" ? value : null;
}

function OutputItem({ run, step, output, onRun }: { run: WorkflowRun; step: string; output: WorkflowStepOutput; onRun: (run: WorkflowRun) => void }) {
  const instance = provenanceInstance(output.provenance);
  return (
    <li className="xp-wf-output" data-testid={`workflow-output-${step}-${output.port}`} data-kind={output.value.kind} data-port={output.port}>
      <div className="xp-wf-output__head">
        <strong>{output.port}</strong> <Badge tone="neutral">{PORT_TYPE_LABELS[output.portType]}</Badge>
        <span className="xp-wf__muted">
          {" "}
          validated · from {output.provenance.operation.kind} {output.provenance.operation.id} (attempt {output.provenance.attempt}
          {instance !== null ? `, instance ${instance + 1}` : ""})
        </span>
      </div>
      <OutputValueView run={run} step={step} value={output.value} onRun={onRun} />
    </li>
  );
}

// ----- steps -------------------------------------------------------------------------------

/** The environment an attempt recorded (runner, image identity, interpreter/toolchain, packages, model, seed). */
export function EnvironmentView({ env, testId }: { env: WorkflowEnvironment; testId: string }) {
  const [packages, setPackages] = useState(false);
  return (
    <div className="xp-wf-env" data-testid={testId} data-runner={env.runner}>
      <dl className="xp-wf-facts">
        {environmentFacts(env).map((f) => (
          <div key={f.label} className="xp-wf-env__row" data-fact={f.label}>
            <dt>{f.label}</dt>
            <dd>
              {f.value}
              {f.hash && (
                <>
                  {" · "}
                  <FullHash hash={f.hash} label={`${f.label.toLowerCase()} of this attempt`} />
                </>
              )}
            </dd>
          </div>
        ))}
      </dl>
      {env.packages.length > 0 && (
        <div>
          <Button size="sm" variant="ghost" aria-expanded={packages} onPress={() => setPackages(!packages)}>
            {packages ? "Hide" : "Show"} the {env.packages.length} package{env.packages.length === 1 ? "" : "s"}
          </Button>
          {packages && (
            <ul className="xp-wf-env__packages" aria-label="Packages">
              {env.packages.map((p) => (
                <li key={p.name}>
                  <code>
                    {p.name} {p.version}
                  </code>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {env.notes.length > 0 && (
        <ul className="xp-wf__muted">
          {env.notes.map((n, i) => (
            <li key={i}>{n}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function AttemptView({
  run,
  step,
  attempt,
  onRun,
  scope,
}: {
  run: WorkflowRun;
  step: WorkflowRunStep;
  attempt: WorkflowStepAttempt;
  onRun: (run: WorkflowRun) => void;
  /** Test-id scope: the step id, or `<step>-i<index>` for an instance's attempt. */
  scope?: string;
}) {
  const view = ATTEMPT_STATE_VIEW[attempt.state];
  const id = scope ?? step.step;
  return (
    <div className="xp-wf-attempt" data-testid={`workflow-attempt-${id}-${attempt.attempt}`} data-state={attempt.state}>
      <div className="xp-wf__row">
        <span>Attempt {attempt.attempt}</span>
        <StatusPill size="sm" status={view.status} label={view.label} detail={formatDuration(attempt.dispatchedAtMs ?? attempt.intentAtMs, attempt.finishedAtMs)} />
        <span className="xp-wf__muted">
          {attempt.operation ? (
            <>
              {attempt.operation.kind} <code>{attempt.operation.id}</code>
            </>
          ) : (
            "no service operation yet"
          )}
        </span>
      </div>
      <p className="xp-wf__muted">
        Recorded {formatTime(attempt.intentAtMs)}
        {attempt.dispatchedAtMs !== null ? ` · dispatched ${formatTime(attempt.dispatchedAtMs)}` : ""}
        {attempt.finishedAtMs !== null ? ` · finished ${formatTime(attempt.finishedAtMs)}` : ""}
      </p>
      {(attempt.code || attempt.message) && (
        <div data-testid={`workflow-attempt-error-${id}`} data-code={attempt.code ?? undefined}>
          <Banner tone={attempt.state === "canceled" ? "canceled" : attempt.state === "interrupted" ? "warning" : "danger"} title={attempt.code ?? "Problem"}>
            {attempt.message}
          </Banner>
        </div>
      )}
      {attempt.log && (
        <pre className="xp-wf-log" tabIndex={0} aria-label={`Log of ${step.label}, attempt ${attempt.attempt}`} data-testid={`workflow-log-${id}`}>
          {attempt.log}
        </pre>
      )}
      {attempt.environment ? (
        <Section title="Environment" collapsible defaultExpanded={false}>
          <EnvironmentView env={attempt.environment} testId={`workflow-env-${id}-${attempt.attempt}`} />
        </Section>
      ) : (
        attempt.dispatchedAtMs !== null && (
          <p className="xp-wf__muted" data-testid={`workflow-env-missing-${id}-${attempt.attempt}`}>
            No environment was recorded for this attempt.
          </p>
        )
      )}
      {attempt.outputs.length > 0 && (
        <ul className="xp-wf-outputs" aria-label={`Outputs of ${step.label}`}>
          {attempt.outputs.map((o, i) => (
            <OutputItem key={`${o.port}-${i}`} run={run} step={step.step} output={o} onRun={onRun} />
          ))}
        </ul>
      )}
      {attempt.rejected.length > 0 && (
        <ul className="xp-wf-rejected" aria-label={`Refused outputs of ${step.label}`}>
          {attempt.rejected.map((r, i) => (
            <li key={i} className="xp-wf-rejected__item" data-testid={`workflow-rejected-${step.step}`} data-name={r.name}>
              <FileWarning size={14} aria-hidden="true" />
              <span>
                <strong>Refused:</strong> <code>{r.name}</code> — {r.reason}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Why a step did not run, in plain words (the runner's own reason first). */
export function notRunReason(step: WorkflowRunStep, planStep: WorkflowPlanStep | undefined, plan: WorkflowRun["plan"]): string | null {
  if (step.state === "conditionNotMet") {
    const own = step.reason ? ` ${step.reason}` : "";
    const c = planStep?.condition;
    const cond = c
      ? ` It runs only if ${describeTest(c.test, `“${plan.steps.find((x) => x.step === c.source.step)?.label ?? c.source.step} → ${c.source.port}”`, c.negate)}.`
      : "";
    return `Not run: its condition was not met.${own}${cond}`;
  }
  if (step.state === "skipped") return `Not run: an earlier step did not produce what it needs.${step.reason ? ` ${step.reason}` : ""}`;
  return step.reason ?? null;
}

function InstanceView({ run, step, instance, parameter, onRun }: { run: WorkflowRun; step: WorkflowRunStep; instance: WorkflowStepInstance; parameter?: string; onRun: (run: WorkflowRun) => void }) {
  const view = STEP_STATE_VIEW[instance.state];
  const scope = `${step.step}-i${instance.index}`;
  return (
    <li className="xp-wf-instance" data-testid={`workflow-instance-${step.step}-${instance.index}`} data-state={instance.state} data-index={instance.index}>
      <div className="xp-wf__row">
        <span className="xp-wf__grow">
          <strong>{instanceLabel(instance, parameter)}</strong>
          <span className="xp-wf__muted"> · instance {instance.index + 1}</span>
        </span>
        <StatusPill size="sm" status={view.status} label={view.label} />
      </div>
      {instance.reason && <p className="xp-wf__muted">{instance.reason}</p>}
      {instance.attempts.map((a) => (
        <AttemptView key={a.attempt} run={run} step={step} attempt={a} onRun={onRun} scope={scope} />
      ))}
    </li>
  );
}

function StepView({ run, step, index, onRun }: { run: WorkflowRun; step: WorkflowRunStep; index: number; onRun: (run: WorkflowRun) => void }) {
  const view = STEP_STATE_VIEW[step.state];
  const containment = CONTAINMENT_VIEW[step.containment];
  const planStep = run.plan.steps.find((p) => p.step === step.step);
  const path = planStep?.compositePath ?? [];
  const instances = step.instances ?? [];
  const parameter = planStep?.repeat?.kind === "forEach" ? planStep.repeat.parameter : undefined;
  const counts = instanceCounts(instances);
  const reason = notRunReason(step, planStep, run.plan);
  return (
    <li
      className="xp-wf-runstep"
      data-testid={`workflow-run-step-${step.step}`}
      data-state={step.state}
      data-instances={instances.length || undefined}
      data-composite-path={path.length ? path.join(" › ") : undefined}
    >
      <div className="xp-wf-plan__step-head">
        <span className="xp-wf-plan__index" aria-hidden="true">
          {index + 1}
        </span>
        <div className="xp-wf__grow">
          <strong>{step.label}</strong>
          <div className="xp-wf__muted">
            {path.length > 0 && <>Inside {path.map((t) => `“${t}”`).join(" › ")} · </>}
            {step.operationTitle} · runner: {step.runnerTitle}
          </div>
        </div>
        <Badge tone={containment.tone} title={containment.detail}>
          {containment.label}
        </Badge>
        <StatusPill status={view.status} label={view.label} />
      </div>
      {planStep?.repeat && (
        <p className="xp-wf__muted" data-testid={`workflow-run-repeat-${step.step}`}>
          {describeRepeat(planStep.repeat, (port) => `its output “${port}”`, (name) => `“${name}”`)}
        </p>
      )}
      {reason && (step.state === "conditionNotMet" || step.state === "skipped" || step.reason) && (
        <p className={step.state === "conditionNotMet" ? "xp-wf-reason xp-wf-reason--condition" : "xp-wf-reason"} data-testid={`workflow-step-reason-${step.step}`} data-state={step.state}>
          {reason}
        </p>
      )}
      {instances.length > 0 && (
        <div className="xp-wf-instances">
          <p className="xp-wf__muted" data-testid={`workflow-instances-summary-${step.step}`}>
            {instances.length} instance{instances.length === 1 ? "" : "s"}:{" "}
            {Object.entries(counts)
              .map(([state, n]) => `${n} ${STEP_STATE_VIEW[state as keyof typeof STEP_STATE_VIEW].label.toLowerCase()}`)
              .join(", ")}
          </p>
          <ol className="xp-wf-instances__list" aria-label={`Instances of ${step.label}`}>
            {instances.map((i) => (
              <InstanceView key={i.index} run={run} step={step} instance={i} parameter={parameter} onRun={onRun} />
            ))}
          </ol>
        </div>
      )}
      {step.attempts.length === 0
        ? instances.length === 0 && !reason && <p className="xp-wf__muted">Not dispatched.</p>
        : step.attempts.map((a) => <AttemptView key={a.attempt} run={run} step={step} attempt={a} onRun={onRun} />)}
    </li>
  );
}

// ----- run view ----------------------------------------------------------------------------

/** The run manifest (JSON text from the service), shown on request as plain text. */
function ManifestView({ run }: { run: WorkflowRun }) {
  const [state, setState] = useState<{ kind: "idle" } | { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; text: string }>({ kind: "idle" });
  const load = async () => {
    setState({ kind: "loading" });
    try {
      const raw = await workflowRunCommands.manifest(run.id);
      let text = raw;
      try {
        text = JSON.stringify(JSON.parse(raw), null, 2);
      } catch {
        // Shown as received.
      }
      setState({ kind: "ready", text });
    } catch (e) {
      setState({ kind: "error", message: errorMessage(e) });
    }
  };
  return (
    <div data-testid="workflow-run-manifest-view" data-state={state.kind}>
      <p className="xp-wf__muted">
        The revision, definition and approval hashes, every input and output digest, each attempt's environment, budgets and usage,
        and the cause. Export manifest writes the same JSON to a file.
      </p>
      {state.kind !== "ready" && (
        <Button size="sm" isPending={state.kind === "loading"} onPress={() => void load()} data-testid="workflow-run-manifest-show">
          Show the manifest
        </Button>
      )}
      {state.kind === "error" && (
        <Banner tone="danger" title="The manifest is unavailable" live>
          {state.message}
        </Banner>
      )}
      {state.kind === "ready" && (
        <pre className="xp-wf-log xp-wf-manifest" tabIndex={0} aria-label="Run manifest (JSON)">
          {state.text}
        </pre>
      )}
    </div>
  );
}

function CauseView({ cause, run }: { cause: WorkflowRunCause | undefined; run: WorkflowRun }) {
  const kind = cause?.kind ?? "manual";
  return (
    <div className="xp-wf-cause" data-testid="workflow-run-cause" data-kind={kind}>
      <span className="xp-wf__muted">
        {kind === "trigger" ? <Zap size={14} aria-hidden="true" /> : kind === "reproduce" ? <History size={14} aria-hidden="true" /> : null} {describeCause(cause)}.
      </span>
      {cause?.kind === "reproduce" && (
        <Button size="sm" variant="ghost" onPress={() => openRun({ id: cause.of, title: run.title })} data-testid="workflow-run-cause-open">
          Open the reproduced run
        </Button>
      )}
      {cause?.kind === "trigger" && cause.changed.length > 0 && (
        <ul className="xp-wf-cause__changes" aria-label="Watched inputs that changed">
          {cause.changed.map((c) => {
            const input = run.plan.inputs.find((i) => i.id === c.input);
            return (
              <li key={c.input} data-input={c.input}>
                {input ? (
                  <>
                    {input.label} (<code>{input.path}</code>)
                  </>
                ) : (
                  c.input
                )}
                : <code title={c.before ?? undefined}>{c.before ? shortHash(c.before) : "absent"}</code> →{" "}
                <code title={c.after ?? undefined}>{c.after ? shortHash(c.after) : "absent"}</code>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

export function RunMonitor({ run, onRun }: { run: WorkflowRun; onRun: (run: WorkflowRun) => void }) {
  const [busy, setBusy] = useState<"cancel" | "ack" | "manifest" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reviewOpen, setReviewOpen] = useState(false);
  const [reproduceOpen, setReproduceOpen] = useState(false);
  const [manifest, setManifest] = useState<WorkflowManifestExport | null>(null);
  const view = RUN_STATE_VIEW[run.state];
  const running = runningNow(run);
  const maxParallel = run.plan.maxParallel ?? 1;
  const reproducePlan = useCallback(async () => {
    const r = await workflowRunCommands.reproducePlan(run.id);
    return { ...r.plan, reproduces: r.reproduces, drift: r.drift };
  }, [run.id]);

  const exportManifest = async () => {
    setBusy("manifest");
    setError(null);
    try {
      const written = await workflowRunCommands.exportManifest(run.id);
      setManifest(written);
      notify({ tone: "success", title: "Run manifest exported", body: written.path });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const act = async (which: "cancel" | "ack") => {
    setBusy(which);
    setError(null);
    try {
      onRun(which === "cancel" ? await workflowRunCommands.cancel(run.id) : await workflowRunCommands.acknowledge(run.id));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="xp-wf__page xp-wf-run" data-testid="workflow-run" data-run={run.id} data-state={run.state} data-acknowledged={String(run.acknowledged)}>
      <header className="xp-wf__header">
        <div className="xp-wf__grow">
          <h2 className="xp-wf__title">{run.title}</h2>
          <p className="xp-wf__muted">
            Revision {run.revision} · started {formatTime(run.startedAtMs)}
            {run.finishedAtMs !== null ? ` · finished ${formatTime(run.finishedAtMs)} (${formatDuration(run.startedAtMs, run.finishedAtMs)})` : ""}
          </p>
          <CauseView cause={run.cause} run={run} />
        </div>
        <span data-testid="workflow-run-state" data-state={run.state} aria-live="polite">
          <StatusPill status={view.status} label={view.label} />
        </span>
        {run.state === "running" ? (
          <Button variant="secondary" icon={Square} isPending={busy === "cancel"} isDisabled={busy !== null} onPress={() => void act("cancel")} data-testid="workflow-run-cancel">
            Cancel run
          </Button>
        ) : (
          <>
            {run.state !== "interrupted" && (
              <Button variant="secondary" icon={RotateCcw} onPress={() => setReviewOpen(true)} data-testid="workflow-run-again">
                Review a new run…
              </Button>
            )}
            <Button variant="secondary" icon={History} onPress={() => setReproduceOpen(true)} data-testid="workflow-run-reproduce">
              Reproduce…
            </Button>
          </>
        )}
        {run.state !== "running" && (
          <Button
            variant="ghost"
            icon={FileDown}
            isPending={busy === "manifest"}
            isDisabled={busy !== null}
            onPress={() => void exportManifest()}
            data-testid="workflow-run-export-manifest"
          >
            Export manifest
          </Button>
        )}
      </header>

      {manifest && (
        <div data-testid="workflow-run-manifest" data-path={manifest.path}>
          <Banner tone="info" title="Run manifest exported">
            <code>{manifest.path}</code> · {formatBytes(manifest.bytes)} · <FullHash hash={manifest.sha256} label="the manifest" />. It lists the revision,
            definition and approval hashes, input and output digests, each attempt's environment, budgets and usage, and the cause.
          </Banner>
        </div>
      )}

      {run.state === "interrupted" && (
        <div data-testid="workflow-run-interrupted">
          <Banner
            tone="warning"
            title="The application stopped during this run"
            actions={
              <>
                {!run.acknowledged && (
                  <Button size="sm" icon={Check} isPending={busy === "ack"} isDisabled={busy !== null} onPress={() => void act("ack")} data-testid="workflow-run-acknowledge">
                    Mark as reviewed
                  </Button>
                )}
                <Button size="sm" icon={RotateCcw} onPress={() => setReviewOpen(true)} data-testid="workflow-run-review-new">
                  Review a new run
                </Button>
              </>
            }
          >
            Nothing was repeated automatically. A step marked “Outcome unknown” may have partly run; check its effects before
            starting a new run.{run.acknowledged ? " Reviewed." : ""}
          </Banner>
        </div>
      )}
      {run.state === "canceled" && (
        <Banner tone="canceled" title="This run was canceled">
          Steps after the cancellation were not run. Outputs already validated are listed below.
        </Banner>
      )}
      {run.state === "failed" && (
        <div data-testid="workflow-run-failure" data-code={run.code ?? undefined}>
          <Banner tone="danger" title={run.code ? `Run failed: ${run.code}` : "Run failed"}>
            {run.message ?? "A step failed; later steps were not run."}
          </Banner>
        </div>
      )}
      {error && (
        <Banner tone="danger" title="Not done" live>
          {error}
        </Banner>
      )}

      {run.state === "running" && (
        <section className="xp-wf__group" aria-label="Running now" data-testid="workflow-run-concurrent" data-count={running.length}>
          <h3 className="xp-wf__group-title">
            Running now ({running.length}
            {maxParallel > 1 ? ` of at most ${maxParallel} at once` : ""})
          </h3>
          {running.length === 0 ? (
            <p className="xp-wf__muted">Nothing is in flight: the next step waits for its inputs or a free slot.</p>
          ) : (
            <ul className="xp-wf-running">
              {running.map((r) => (
                <li key={`${r.step}-${r.instance ?? "s"}`} data-testid={`workflow-running-${r.step}${r.instance === null ? "" : `-${r.instance}`}`}>
                  <Cpu size={14} aria-hidden="true" />
                  <strong>{r.label}</strong>
                  {r.detail && <span> · {r.detail}</span>}
                  {r.sinceMs !== null && <span className="xp-wf__muted"> · since {formatTime(r.sinceMs)}</span>}
                </li>
              ))}
            </ul>
          )}
        </section>
      )}

      <section className="xp-wf__group" aria-label="Usage">
        <h3 className="xp-wf__group-title">Usage against the run's budgets</h3>
        <div className="xp-wf-usage" data-testid="workflow-run-usage">
          {usageRows(run).map((r) => (
            <ProgressBar
              key={r.label}
              label={r.label}
              value={r.limit > 0 ? Math.min(100, (r.used / r.limit) * 100) : 0}
              valueLabel={`${r.format(r.used)} of ${r.format(r.limit)}`}
              tone={r.limit > 0 && r.used >= r.limit ? "danger" : "accent"}
            />
          ))}
        </div>
      </section>

      <section className="xp-wf__group" aria-label="Steps">
        <h3 className="xp-wf__group-title">Steps</h3>
        <ol className="xp-wf-plan__steps" data-testid="workflow-run-steps">
          {run.steps.map((s, i) => (
            <StepView key={s.step} run={run} step={s} index={i} onRun={onRun} />
          ))}
        </ol>
      </section>

      <section className="xp-wf__group" aria-label="Outcomes">
        <h3 className="xp-wf__group-title">Outcomes</h3>
        <ul className="xp-wf-outputs" data-testid="workflow-run-outcomes">
          {run.outcomes.map((o) => (
            <li key={o.id} className="xp-wf-output" data-outcome={o.id} data-realized={String(o.value !== null)}>
              <div className="xp-wf-output__head">
                <strong>{o.label}</strong>
                {o.optional && <Badge tone="neutral">Optional</Badge>}{" "}
                {o.value ? (
                  <Badge tone="success" icon={Check}>
                    Produced
                  </Badge>
                ) : (
                  <Badge tone="neutral" dashed icon={o.optional ? CircleSlash : Ban}>
                    Not produced
                  </Badge>
                )}
              </div>
              {o.value && <span className="xp-wf__muted">{o.value.kind === "file" || o.value.kind === "retained" ? o.value.path : `${o.step} → ${o.port}`}</span>}
              {!o.value && o.reason && (
                <span className="xp-wf__muted" data-testid={`workflow-outcome-reason-${o.id}`}>
                  {o.reason}
                </span>
              )}
            </li>
          ))}
        </ul>
      </section>

      <Section title="The plan exactly as approved" collapsible defaultExpanded={false}>
        <PlanDetails plan={run.plan} />
      </Section>

      {run.state !== "running" && (
        <Section title="Run manifest" collapsible defaultExpanded={false}>
          <ManifestView run={run} />
        </Section>
      )}

      <PlanReviewDialog
        workflow={run.workflow}
        revision={run.revision}
        isOpen={reviewOpen}
        onOpenChange={setReviewOpen}
        onStarted={(next) => openRun(next)}
      />
      <PlanReviewDialog
        workflow={run.workflow}
        revision={run.revision}
        isOpen={reproduceOpen}
        onOpenChange={setReproduceOpen}
        onStarted={(next) => openRun(next)}
        loadPlan={reproducePlan}
        heading="Reproduce"
      />
    </div>
  );
}

/** Editor tab for one run, kept current by `workflow-run` events. */
export function WorkflowRunView({ resource }: EditorProps) {
  const id = resource.key.slice(WORKFLOW_RUN_KEY_PREFIX.length);
  const [run, setRun] = useState<WorkflowRun | null>(null);
  const [error, setError] = useState<string | null>(null);
  const latest = useRef(0);

  const refresh = useCallback(async () => {
    const ticket = ++latest.current;
    try {
      const next = await workflowRunCommands.get(id);
      if (ticket !== latest.current) return;
      setRun(next);
      setError(null);
      editors.setTitle(resource.key, `Run: ${next.title}`);
    } catch (e) {
      if (ticket === latest.current) setError(errorMessage(e));
    }
  }, [id, resource.key]);

  useEffect(() => {
    void refresh();
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void workflowEvents
      .run((event) => {
        if (event.run === id) void refresh();
      })
      .then((u) => {
        if (disposed) u();
        else unlisten = u;
      })
      // Outside the desktop app there are no events; the view still loads once.
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [id, refresh]);

  if (error && !run) {
    return (
      <div className="xp-wf">
        <ErrorState title="Could not open this run" description={error} actions={<Button onPress={() => void refresh()}>Retry</Button>} />
      </div>
    );
  }
  if (!run) return <LoadingState title="Loading the run…" />;
  return (
    <div className="xp-wf">
      <RunMonitor run={run} onRun={(next) => setRun(next)} />
    </div>
  );
}

// ----- history -----------------------------------------------------------------------------

/** Runs of a workflow (or of every workflow), newest first. */
export function RunHistory({ workflow, title = "Runs" }: { workflow: string | null; title?: string }) {
  const [runs, setRuns] = useState<WorkflowRunSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await workflowRunCommands.list(workflow);
      setRuns([...list].sort((a, b) => b.startedAtMs - a.startedAtMs));
      setError(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [workflow]);

  useEffect(() => {
    void refresh();
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void workflowEvents
      .run((event) => {
        if (workflow === null || event.workflow === workflow) void refresh();
      })
      .then((u) => {
        if (disposed) u();
        else unlisten = u;
      })
      // Outside the desktop app there are no events; the view still loads once.
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [workflow, refresh]);

  return (
    <section className="xp-wf__group" aria-label={title}>
      <h3 className="xp-wf__group-title">{title}</h3>
      {error && (
        <Banner tone="danger" title="Runs are unavailable" actions={<Button size="sm" onPress={() => void refresh()}>Retry</Button>}>
          {error}
        </Banner>
      )}
      {runs && runs.length === 0 && <EmptyState fill={false} title="No runs yet" description="Review and run a saved, well-formed revision to see its runs here." />}
      {runs && runs.length > 0 && (
        <ul className="xp-wf__cards" data-testid="workflow-runs">
          {runs.map((r) => {
            const view = RUN_STATE_VIEW[r.state];
            return (
              <li key={r.id} className="xp-wf__card xp-wf__row" data-testid={`workflow-run-item-${r.id}`} data-state={r.state}>
                <div className="xp-wf__grow">
                  <strong>{r.title}</strong>
                  <p className="xp-wf__muted">
                    Revision {r.revision} · {formatTime(r.startedAtMs)} · {r.stepsDone} of {r.stepsTotal} steps
                  </p>
                </div>
                {r.state === "interrupted" && !r.acknowledged && (
                  <Badge tone="warning" dashed>
                    Needs review
                  </Badge>
                )}
                {r.cause?.kind === "trigger" && (
                  <Badge tone="info" icon={Zap} title={`Started by trigger ${r.cause.trigger}`}>
                    Trigger
                  </Badge>
                )}
                {r.cause?.kind === "reproduce" && (
                  <Badge tone="info" icon={History} title={`Reproduction of run ${r.cause.of}`}>
                    Reproduction
                  </Badge>
                )}
                <StatusPill status={view.status} label={view.label} />
                <Button size="sm" onPress={() => openRun(r)} aria-label={`Open run of ${r.title} started ${formatTime(r.startedAtMs)}`}>
                  Open
                </Button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
