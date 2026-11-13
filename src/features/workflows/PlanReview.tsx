// Plan review: everything a workflow run will do, shown in full before the
// user starts it. The plan is resolved by Rust (`workflow_run_plan`) for one
// saved revision; "Start run" sends exactly that plan's id and approval hash.
// Rust recomputes the plan and refuses a stale approval
// (`workflow.planStale`), which is shown inline with "Review the new plan".
// Nothing here starts a run by itself.
import { Copy, Play, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useId, useState } from "react";

import type { ParamValue } from "../../bindings/ParamValue";
import type { PortSource } from "../../bindings/PortSource";
import type { StepPayload } from "../../bindings/StepPayload";
import type { WorkflowId } from "../../bindings/WorkflowId";
import type { WorkflowPlanStep } from "../../bindings/WorkflowPlanStep";
import type { WorkflowRun } from "../../bindings/WorkflowRun";
import type { WorkflowRunPlan } from "../../bindings/WorkflowRunPlan";
import { errorMessage, isCommandError } from "../../lib/ipc";
import { Badge, Banner, Button, Dialog, ErrorState, IconButton, LoadingState } from "../../ui/components";
import { workflowRunCommands } from "./ipc";
import { PORT_TYPE_LABELS } from "./model";
import { CONTAINMENT_VIEW, EFFECT_LABELS, formatBytes, formatSeconds, formatTime, shortHash, STALE_PLAN_CODES } from "./runModel";

const EXECUTION_CEILING: Record<string, string> = {
  none: "No code runs",
  restricted: "Restricted container only",
  trustedNative: "Trusted native execution allowed (your account's privileges)",
};

/** Full text with line numbers (read-only). Line numbers are not part of the copied text. */
export function CodeListing({ text, label, testId }: { text: string; label: string; testId?: string }) {
  const lines = text.split("\n");
  return (
    <pre className="xp-wf-code" aria-label={label} data-testid={testId} tabIndex={0}>
      {lines.map((line, i) => (
        <span key={i} className="xp-wf-code__line" data-line={i + 1}>
          {line}
          {"\n"}
        </span>
      ))}
    </pre>
  );
}

export function FullHash({ hash, label }: { hash: string; label: string }) {
  const hex = hash.replace(/^sha256:/, "");
  return (
    <span className="xp-wf-hash">
      <code title={hex} data-full={hex}>
        {shortHash(hex)}
      </code>
      <IconButton
        icon={Copy}
        size="sm"
        aria-label={`Copy SHA-256 of ${label}`}
        onPress={() => void navigator.clipboard?.writeText(hex).catch(() => undefined)}
      />
    </span>
  );
}

function paramText(value: ParamValue): string {
  switch (value.kind) {
    case "boolean":
      return value.value ? "yes" : "no";
    case "integer":
      return String(value.value);
    default:
      return value.value;
  }
}

function PayloadView({ step, payload }: { step: WorkflowPlanStep; payload: StepPayload }) {
  if (payload.kind === "code") {
    return (
      <div className="xp-wf-plan__payload">
        <div className="xp-wf-plan__label">
          Code ({payload.language}, {payload.source.split("\n").length} lines) — runs exactly as shown
        </div>
        <CodeListing text={payload.source} label={`Code of step ${step.label}`} testId={`plan-code-${step.step}`} />
      </div>
    );
  }
  return (
    <div className="xp-wf-plan__payload">
      <div className="xp-wf-plan__label">Instructions sent to the model provider — exactly as shown</div>
      <CodeListing text={payload.text} label={`Instructions of step ${step.label}`} testId={`plan-prompt-${step.step}`} />
    </div>
  );
}

function PlanStepView({ plan, step, index }: { plan: WorkflowRunPlan; step: WorkflowPlanStep; index: number }) {
  const containment = CONTAINMENT_VIEW[step.containment];
  const sourceText = (from: PortSource) => {
    if (from.kind === "input") {
      const input = plan.inputs.find((i) => i.id === from.input);
      return input ? `Input “${input.label}” (${input.path})` : `Input ${from.input}`;
    }
    const producer = plan.steps.find((s) => s.step === from.step);
    return `${producer?.label ?? from.step} → ${from.port}`;
  };
  const params = Object.entries(step.parameters);
  return (
    <li className="xp-wf-plan__step" data-testid={`plan-step-${step.step}`} data-containment={step.containment} data-available={String(step.available)}>
      <div className="xp-wf-plan__step-head">
        <span className="xp-wf-plan__index" aria-hidden="true">
          {index + 1}
        </span>
        <div className="xp-wf__grow">
          <strong>{step.label}</strong>
          <div className="xp-wf__muted">
            {step.operationTitle} ({step.operation.id} v{step.operation.version}) · runner: {step.runnerTitle}
          </div>
        </div>
        <Badge tone={containment.tone} title={containment.detail}>
          {containment.label}
        </Badge>
      </div>
      <p className="xp-wf__muted xp-wf-plan__containment">{containment.detail}</p>
      {!step.available && (
        <Banner tone="warning" title="Runner not available">
          {step.availability}
        </Banner>
      )}
      <dl className="xp-wf-facts">
        <dt>Effects</dt>
        <dd>
          <ul className="xp-wf-plan__effects">
            {step.effects.map((e) => (
              <li key={e} data-effect={e}>
                {EFFECT_LABELS[e]}
              </li>
            ))}
          </ul>
        </dd>
        <dt>Reads</dt>
        <dd>
          {step.bindings.length === 0 ? (
            "Nothing bound"
          ) : (
            <ul className="xp-wf-plan__effects">
              {step.bindings.map((b, i) => (
                <li key={i}>
                  <code>{b.port}</code> ← {sourceText(b.from)}
                </li>
              ))}
            </ul>
          )}
        </dd>
        {step.prerequisites.length > 0 && (
          <>
            <dt>Runs after</dt>
            <dd>{step.prerequisites.map((p) => plan.steps.find((s) => s.step === p)?.label ?? p).join(", ")} (no data)</dd>
          </>
        )}
        <dt>Parameters</dt>
        <dd>
          {params.length === 0
            ? "None"
            : params.map(([name, value]) => (
                <span key={name} className="xp-wf-plan__param">
                  <code>{name}</code> = <code>{paramText(value)}</code>
                </span>
              ))}
        </dd>
        <dt>Limits</dt>
        <dd>
          At most {formatSeconds(step.timeLimitSeconds)} and {formatBytes(step.maxOutputBytes)} of output
        </dd>
      </dl>
      {step.payload && <PayloadView step={step} payload={step.payload} />}
    </li>
  );
}

/** The plan in full (also shown read-only in the run monitor as "the plan exactly as approved"). */
export function PlanDetails({ plan }: { plan: WorkflowRunPlan }) {
  const { execution, ceiling, budgets } = plan;
  const stepNames = (ids: string[]) => ids.map((id) => `“${plan.steps.find((s) => s.step === id)?.label ?? id}”`).join(", ");
  return (
    <div className="xp-wf-plan" data-testid="workflow-plan-details" data-plan={plan.plan} data-approval={plan.approvalHash}>
      <dl className="xp-wf-facts">
        <dt>Project</dt>
        <dd data-testid="plan-project">{plan.project}</dd>
        <dt>Workflow</dt>
        <dd>
          {plan.title} · revision <span data-testid="plan-revision">{plan.revision}</span> · definition <FullHash hash={plan.definitionHash} label="the definition" />
        </dd>
        <dt>Most it may do</dt>
        <dd>
          {EXECUTION_CEILING[ceiling.execution] ?? ceiling.execution}
          {ceiling.modelRequests ? "; may send declared inputs to the model provider" : "; no model requests"}
          {ceiling.retainResults ? "; may keep results and record evidence" : ""}
          {ceiling.proposeChanges ? "; may propose reviewable changes" : ""}
        </dd>
        <dt>Execution</dt>
        <dd>
          {execution.restrictedBackend
            ? `Restricted backend ${execution.restrictedBackend}${execution.restrictedImage ? ` with image ${execution.restrictedImage}` : " (no image configured)"}`
            : "No restricted backend configured"}
          {" · "}
          {execution.trustedFolder ? "folder trusted for native execution" : "folder not trusted for native execution"}
          {execution.nativeSteps.length > 0 && ` · runs natively with your account's privileges: ${stepNames(execution.nativeSteps)}`}
          {execution.modelSteps.length > 0 && ` · sends material to the model provider: ${stepNames(execution.modelSteps)}`}
        </dd>
        <dt>Budgets</dt>
        <dd data-testid="plan-budgets">
          {budgets.maxSteps} steps · {formatSeconds(budgets.maxTotalSeconds)} in total · {formatBytes(budgets.maxOutputBytes)} of output · {budgets.maxModelRequests} model request
          {budgets.maxModelRequests === 1 ? "" : "s"}
        </dd>
        <dt>Plan</dt>
        <dd>
          Made {formatTime(plan.createdAtMs)}; start it before {formatTime(plan.expiresAtMs)} · approval <FullHash hash={plan.approvalHash} label="the approval" />
        </dd>
      </dl>

      {plan.notices.length > 0 && (
        <ul className="xp-wf-plan__notices" data-testid="plan-notices">
          {plan.notices.map((n, i) => (
            <li key={i}>
              <Banner tone="info" title="Note">
                {n}
              </Banner>
            </li>
          ))}
        </ul>
      )}

      <h3 className="xp-wf-plan__heading">Inputs, exactly as the run will read them</h3>
      {plan.inputs.length === 0 ? (
        <p className="xp-wf__muted">No external inputs.</p>
      ) : (
        <table className="xp-wf-table" data-testid="plan-inputs">
          <thead>
            <tr>
              <th scope="col">Input</th>
              <th scope="col">Path</th>
              <th scope="col">SHA-256</th>
              <th scope="col">Size</th>
              <th scope="col">Version</th>
            </tr>
          </thead>
          <tbody>
            {plan.inputs.map((input) => (
              <tr key={input.id} data-testid={`plan-input-${input.id}`}>
                <th scope="row">
                  {input.label} <span className="xp-wf__muted">({PORT_TYPE_LABELS[input.portType]})</span>
                </th>
                <td>
                  <code>{input.path}</code>
                </td>
                <td>
                  <FullHash hash={input.sha256} label={input.path} />
                </td>
                <td>{formatBytes(input.bytes)}</td>
                <td>{input.pinned ? "Pinned" : "Current at review"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <h3 className="xp-wf-plan__heading">Steps, in the order they run (one at a time)</h3>
      <ol className="xp-wf-plan__steps">
        {plan.steps.map((step, i) => (
          <PlanStepView key={step.step} plan={plan} step={step} index={i} />
        ))}
      </ol>
    </div>
  );
}

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; plan: WorkflowRunPlan };

export interface PlanReviewDialogProps {
  workflow: WorkflowId;
  revision: number | null;
  isOpen: boolean;
  onOpenChange: (open: boolean) => void;
  onStarted: (run: WorkflowRun) => void;
}

/** "Review and run": fetches the plan when opened; starting requires an explicit press of "Start run". */
export function PlanReviewDialog({ workflow, revision, isOpen, onOpenChange, onStarted }: PlanReviewDialogProps) {
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  const [starting, setStarting] = useState(false);
  const [failure, setFailure] = useState<{ code: string; message: string } | null>(null);
  const reasonId = useId();

  const fetchPlan = useCallback(async () => {
    setLoad({ kind: "loading" });
    setFailure(null);
    try {
      setLoad({ kind: "ready", plan: await workflowRunCommands.plan(workflow, revision) });
    } catch (e) {
      setLoad({ kind: "error", message: errorMessage(e) });
    }
  }, [workflow, revision]);

  useEffect(() => {
    if (isOpen) void fetchPlan();
  }, [isOpen, fetchPlan]);

  const plan = load.kind === "ready" ? load.plan : null;
  const stale = failure !== null && STALE_PLAN_CODES.includes(failure.code);
  const blocked = (plan?.blockers.length ?? 0) > 0;

  const start = async () => {
    if (!plan || blocked) return;
    setStarting(true);
    setFailure(null);
    try {
      const run = await workflowRunCommands.start({ plan: plan.plan, approvalHash: plan.approvalHash });
      onOpenChange(false);
      onStarted(run);
    } catch (e) {
      setFailure({ code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
    } finally {
      setStarting(false);
    }
  };

  const startDisabledReason = !plan
    ? "The plan is not loaded."
    : blocked
      ? `This plan cannot start: ${plan.blockers.length} blocker${plan.blockers.length === 1 ? "" : "s"} listed above.`
      : stale
        ? "The reviewed plan is out of date. Review the new plan."
        : null;

  return (
    <Dialog
      title={plan ? `Review and run “${plan.title}”` : "Review and run"}
      size="lg"
      isOpen={isOpen}
      onOpenChange={onOpenChange}
      isDismissable={!starting}
      className="xp-wf-plan-dialog"
      footer={
        <>
          {startDisabledReason && plan && (
            <span className="xp-wf__muted xp-wf-plan__reason" id={reasonId} data-testid="plan-start-reason">
              {startDisabledReason}
            </span>
          )}
          <Button slot="close" isDisabled={starting}>
            Cancel
          </Button>
          <Button
            variant="primary"
            icon={Play}
            isPending={starting}
            isDisabled={starting || startDisabledReason !== null}
            aria-describedby={startDisabledReason && plan ? reasonId : undefined}
            onPress={() => void start()}
            data-testid="workflow-start-run"
          >
            Start run
          </Button>
        </>
      }
    >
      <div className="xp-wf-plan-body" data-testid="workflow-plan-review" data-state={load.kind}>
        {load.kind === "loading" && <LoadingState title="Resolving the plan…" />}
        {load.kind === "error" && (
          <ErrorState
            title="Could not make a run plan"
            description={load.message}
            actions={<Button onPress={() => void fetchPlan()}>Retry</Button>}
          />
        )}
        {plan && (
          <>
            <p className="xp-wf__muted">
              Starting approves exactly what is listed here: these inputs at these hashes, these steps with this code and these
              instructions, under these limits. If anything changes before the run starts, it is refused and you review again.
            </p>
            {failure && (
              <div data-testid="plan-start-error" data-code={failure.code}>
                {stale ? (
                  <Banner
                    tone="stale"
                    title="The plan changed since you reviewed it"
                    live
                    actions={
                      <Button size="sm" icon={RefreshCw} onPress={() => void fetchPlan()} data-testid="plan-review-new">
                        Review the new plan
                      </Button>
                    }
                  >
                    {failure.message} Nothing was started.
                  </Banner>
                ) : (
                  <Banner tone="danger" title="The run was not started" live>
                    {failure.message}
                  </Banner>
                )}
              </div>
            )}
            {blocked && (
              <div className="xp-wf-plan__blockers" data-testid="plan-blockers" role="group" aria-label="Why this plan cannot start">
                <Banner tone="danger" title={`Cannot start: ${plan.blockers.length} blocker${plan.blockers.length === 1 ? "" : "s"}`}>
                  <ul>
                    {plan.blockers.map((b, i) => (
                      <li key={i} data-code={b.code} data-step={b.step ?? undefined} data-input={b.input ?? undefined}>
                        {b.message}
                        {b.step ? <span className="xp-wf__muted"> (step {plan.steps.find((s) => s.step === b.step)?.label ?? b.step})</span> : null}
                        {b.input ? <span className="xp-wf__muted"> (input {plan.inputs.find((x) => x.id === b.input)?.label ?? b.input})</span> : null}
                      </li>
                    ))}
                  </ul>
                </Banner>
              </div>
            )}
            <PlanDetails plan={plan} />
          </>
        )}
      </div>
    </Dialog>
  );
}
