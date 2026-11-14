// Plan review: everything a workflow run will do, shown in full before the
// user starts it. The plan is resolved by Rust (`workflow_run_plan`) for one
// saved revision; "Start run" sends exactly that plan's id and approval hash.
// Rust recomputes the plan and refuses a stale approval
// (`workflow.planStale`), which is shown inline with "Review the new plan".
// Nothing here starts a run by itself.
//
// M9: composite steps arrive expanded (`a/b` ids, `compositePath` titles)
// and are grouped under their composite with their complete code/prompts;
// conditions and iteration bounds are stated in plain words; the workflow's
// max parallel and the application's global limits are listed. A
// reproduction plan (Reproduce in the run view) is reviewed here the same
// way, with its environment drift notices first.
import { Copy, Layers, Play, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useId, useState } from "react";

import type { PortSource } from "../../bindings/PortSource";
import type { StepPayload } from "../../bindings/StepPayload";
import type { WorkflowId } from "../../bindings/WorkflowId";
import type { ReviewPlan, WorkflowPlanStep, WorkflowRun, WorkflowRunPlan } from "./contract";
import { errorMessage, isCommandError } from "../../lib/ipc";
import { Badge, Banner, Button, Dialog, ErrorState, IconButton, KeyValueList, LoadingState, SimpleTable, type KeyValueItem } from "../../ui/components";
import { workflowRunCommands } from "./ipc";
import { PORT_TYPE_LABELS } from "./model";
import { DEFAULT_LIMITS, describeCondition, describeLimits, describeRepeat, paramText } from "./orchestration";
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

/** A step's position in its plan or run as a quiet badge (decorative: the list order says it). */
export function StepIndex({ index }: { index: number }) {
  return (
    <span className="xp-wf-index" aria-hidden="true">
      <Badge>{index + 1}</Badge>
    </span>
  );
}

function PayloadView({ step, payload }: { step: WorkflowPlanStep; payload: StepPayload }) {
  if (payload.kind === "code") {
    return (
      <div className="xp-wf__stack">
        <p className="ui-meta">
          Code ({payload.language}, {payload.source.split("\n").length} lines) — runs exactly as shown
        </p>
        <CodeListing text={payload.source} label={`Code of step ${step.label}`} testId={`plan-code-${step.step}`} />
      </div>
    );
  }
  return (
    <div className="xp-wf__stack">
      <p className="ui-meta">Instructions sent to the model provider — exactly as shown</p>
      <CodeListing text={payload.text} label={`Instructions of step ${step.label}`} testId={`plan-prompt-${step.step}`} />
    </div>
  );
}

/** "Label → port" of a plan step's output (plan steps carry no port labels). */
function planPortLabel(plan: WorkflowRunPlan, step: string, port: string): string {
  return `“${plan.steps.find((s) => s.step === step)?.label ?? step} → ${port}”`;
}

function PlanStepView({ plan, step, index }: { plan: WorkflowRunPlan; step: WorkflowPlanStep; index: number }) {
  const containment = CONTAINMENT_VIEW[step.containment];
  const path = step.compositePath ?? [];
  const sourceText = (from: PortSource) => {
    if (from.kind === "input") {
      const input = plan.inputs.find((i) => i.id === from.input);
      return input ? `Input “${input.label}” (${input.path})` : `Input ${from.input}`;
    }
    const producer = plan.steps.find((s) => s.step === from.step);
    return `${producer?.label ?? from.step} → ${from.port}`;
  };
  const params = Object.entries(step.parameters);
  const facts: (KeyValueItem | false)[] = [
    !!step.condition && {
      key: "condition",
      label: "Condition",
      value: <span data-testid={`plan-condition-${step.step}`}>{step.condition && describeCondition(step.condition, (s, p) => planPortLabel(plan, s, p))}</span>,
    },
    !!step.repeat && {
      key: "repeat",
      label: "Repeats",
      value: (
        <span data-testid={`plan-repeat-${step.step}`}>
          {step.repeat && describeRepeat(step.repeat, (port) => `its output “${port}”`, (name) => `“${name}”`)}
        </span>
      ),
    },
    {
      key: "effects",
      label: "Effects",
      value: (
        <ul className="xp-wf-bullets">
          {step.effects.map((e) => (
            <li key={e} data-effect={e}>
              {EFFECT_LABELS[e]}
            </li>
          ))}
        </ul>
      ),
    },
    {
      key: "reads",
      label: "Reads",
      value:
        step.bindings.length === 0 ? (
          "Nothing bound"
        ) : (
          <ul className="xp-wf-bullets">
            {step.bindings.map((b, i) => (
              <li key={i}>
                <code>{b.port}</code> ← {sourceText(b.from)}
              </li>
            ))}
          </ul>
        ),
    },
    step.prerequisites.length > 0 && {
      key: "after",
      label: "Runs after",
      value: `${step.prerequisites.map((p) => plan.steps.find((s) => s.step === p)?.label ?? p).join(", ")} (no data)`,
    },
    {
      key: "parameters",
      label: "Parameters",
      value:
        params.length === 0 ? (
          "None"
        ) : (
          <span className="xp-wf-params">
            {params.map(([name, value]) => (
              <span key={name}>
                <code>{name}</code> = <code>{paramText(value)}</code>
              </span>
            ))}
          </span>
        ),
    },
    {
      key: "limits",
      label: "Limits",
      value: (
        <span data-testid={`plan-limits-${step.step}`}>
          At most {formatSeconds(step.timeLimitSeconds)} and {formatBytes(step.maxOutputBytes)} of output
          {step.maxModelRequests ? `; up to ${step.maxModelRequests} model request${step.maxModelRequests === 1 ? "" : "s"}` : ""}
          {(plan.maxParallel ?? 1) > 1 ? " — reserved from the run's budgets before the step starts, so steps running together cannot exceed them" : ""}
        </span>
      ),
    },
  ];
  return (
    <li
      className="xp-wf-block xp-wf-block--numbered"
      data-testid={`plan-step-${step.step}`}
      data-containment={step.containment}
      data-available={String(step.available)}
      data-composite-path={path.length ? path.join(" › ") : undefined}
      data-conditional={step.condition ? "true" : undefined}
      data-repeat={step.repeat?.kind}
    >
      <div className="xp-wf-block__head">
        <StepIndex index={index} />
        <div className="xp-wf-block__lead">
          <span className="xp-wf-block__title">{step.label}</span>
          <span className="ui-meta">
            {path.length > 0 && <>Inside {path.map((t) => `“${t}”`).join(" › ")} · </>}
            {step.operationTitle} ({step.operation.id} v{step.operation.version}) · runner: {step.runnerTitle}
          </span>
        </div>
        <Badge tone={containment.tone} title={containment.detail}>
          {containment.label}
        </Badge>
      </div>
      <p className="ui-meta">{containment.detail}</p>
      {!step.available && (
        <Banner tone="warning" title="Runner not available">
          {step.availability}
        </Banner>
      )}
      <KeyValueList items={facts.filter((f): f is KeyValueItem => f !== false)} />
      {step.payload && <PayloadView step={step} payload={step.payload} />}
      {step.bindings.map((b, i) => {
        if (b.from.kind !== "input") return null;
        const inputId = b.from.input;
        const input = plan.inputs.find((x) => x.id === inputId);
        if (!input || input.text === null) return null;
        const lean = input.portType === "formalSource";
        return (
          <div className="xp-wf__stack" key={`script-${i}`}>
            <p className="ui-meta">
              {lean ? "Lean source" : "Code from"} <code>{input.path}</code> ({input.text.split("\n").length} lines,{" "}
              <FullHash hash={input.sha256} label={input.path} />) —{" "}
              {lean ? "the audit elaborates exactly this text (elaboration can run code)" : "runs exactly as shown"}
            </p>
            <CodeListing text={input.text} label={`Code of step ${step.label} from ${input.path}`} testId={`plan-script-${step.step}`} />
          </div>
        );
      })}
    </li>
  );
}

type StepGroup = { path: string[]; prefix: string; items: { step: WorkflowPlanStep; index: number }[] };

/** Consecutive steps with the same composite path form one group (top-level steps: an empty path). */
export function groupByComposite(steps: WorkflowPlanStep[]): StepGroup[] {
  const groups: StepGroup[] = [];
  steps.forEach((step, index) => {
    const path = step.compositePath ?? [];
    const last = groups[groups.length - 1];
    if (last && last.path.join("\u0000") === path.join("\u0000")) last.items.push({ step, index });
    else groups.push({ path, prefix: step.step.includes("/") ? step.step.slice(0, step.step.lastIndexOf("/")) : "", items: [{ step, index }] });
  });
  return groups;
}

type PlanInput = ReviewPlan["inputs"][number];

/** The plan in full (also shown read-only in the run monitor as "the plan exactly as approved"). */
export function PlanDetails({ plan }: { plan: ReviewPlan }) {
  const { execution, ceiling, budgets } = plan;
  const maxParallel = plan.maxParallel ?? 1;
  const limits = plan.limits ?? DEFAULT_LIMITS;
  const groups = groupByComposite(plan.steps);
  const drift = plan.drift ?? [];
  // R/Julia batches run in their own images; the default image matters only to the other restricted steps.
  const ownImage = new Set((execution.runnerImages ?? []).map((r) => r.runner));
  const defaultImageSteps = plan.steps.some((s) => s.containment === "restrictedContainer" && !ownImage.has(s.runner));
  const runnerTitle = (runner: string) => plan.steps.find((s) => s.runner === runner)?.runnerTitle ?? runner;
  // Drift is also summarized in the plan's notices; show each fact once.
  const notices = plan.notices.filter((n) => !drift.some((d) => d.message === n));
  const stepNames = (ids: string[]) => ids.map((id) => `“${plan.steps.find((s) => s.step === id)?.label ?? id}”`).join(", ");
  const facts: (KeyValueItem | false)[] = [
    { key: "project", label: "Project", value: <span data-testid="plan-project">{plan.project}</span> },
    {
      key: "workflow",
      label: "Workflow",
      value: (
        <>
          {plan.title} · revision <span data-testid="plan-revision">{plan.revision}</span> · definition{" "}
          <FullHash hash={plan.definitionHash} label="the definition" />
        </>
      ),
    },
    {
      key: "ceiling",
      label: "Most it may do",
      value: (
        <>
          {EXECUTION_CEILING[ceiling.execution] ?? ceiling.execution}
          {ceiling.modelRequests ? "; may send declared inputs to the model provider" : "; no model requests"}
          {ceiling.retainResults ? "; may keep results and record evidence" : ""}
          {ceiling.proposeChanges ? "; may propose reviewable changes" : ""}
        </>
      ),
    },
    {
      key: "execution",
      label: "Execution",
      value: (
        <div data-testid="plan-execution">
          {execution.restrictedBackend
            ? `Restricted backend ${execution.restrictedBackend}${
                defaultImageSteps ? (execution.restrictedImage ? ` with image ${execution.restrictedImage}` : " (no image configured)") : ""
              }`
            : "No restricted backend configured"}
          {defaultImageSteps && execution.restrictedImageId && (
            <span data-testid="plan-image-id">
              {" "}
              (exactly the image <FullHash hash={execution.restrictedImageId} label="the approved image" />; a re-tagged or updated image is refused)
            </span>
          )}
          {" · "}
          {execution.trustedFolder ? "folder trusted for native execution" : "folder not trusted for native execution"}
          {execution.nativeSteps.length > 0 && ` · runs natively with your account's privileges: ${stepNames(execution.nativeSteps)}`}
          {execution.modelSteps.length > 0 && ` · sends material to the model provider: ${stepNames(execution.modelSteps)}`}
          {execution.runnerImages && execution.runnerImages.length > 0 && (
            <ul className="xp-wf-bullets" data-testid="plan-runner-images" aria-label="Images of the R and Julia runners">
              {execution.runnerImages.map((r) => (
                <li key={r.runner} data-runner={r.runner}>
                  {runnerTitle(r.runner)} runs in{" "}
                  {r.imageId ? (
                    <>
                      <code>{r.image}</code>, exactly the image <FullHash hash={r.imageId} label={`the image of ${runnerTitle(r.runner)}`} />
                    </>
                  ) : (
                    <>
                      <code>{r.image}</code> (identity not recorded)
                    </>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      ),
    },
    {
      key: "concurrency",
      label: "Concurrency",
      value: (
        <span data-testid="plan-concurrency" data-max-parallel={maxParallel}>
          {maxParallel === 1
            ? "One step at a time."
            : `Up to ${maxParallel} independent steps (or repeat instances) at the same time; a step starts only after everything it reads or runs after has finished.`}{" "}
          {describeLimits(limits)}
        </span>
      ),
    },
    {
      key: "budgets",
      label: "Budgets",
      value: (
        <span data-testid="plan-budgets">
          {budgets.maxSteps} steps · {formatSeconds(budgets.maxTotalSeconds)} in total · {formatBytes(budgets.maxOutputBytes)} of output ·{" "}
          {budgets.maxModelRequests} model request
          {budgets.maxModelRequests === 1 ? "" : "s"}
        </span>
      ),
    },
    {
      key: "plan",
      label: "Plan",
      value: (
        <>
          Made {formatTime(plan.createdAtMs)}; start it before {formatTime(plan.expiresAtMs)} · approval{" "}
          <FullHash hash={plan.approvalHash} label="the approval" />
        </>
      ),
    },
    !!plan.reproduces && {
      key: "reproduces",
      label: "Reproduces",
      value: (
        <span data-testid="plan-reproduces">
          Run <code>{plan.reproduces}</code>: every input is pinned to the content that run read. The new run records it as its cause.
        </span>
      ),
    },
  ];
  return (
    <div className="xp-wf-plan" data-testid="workflow-plan-details" data-plan={plan.plan} data-approval={plan.approvalHash}>
      <KeyValueList items={facts.filter((f): f is KeyValueItem => f !== false)} />

      {drift.length > 0 && (
        <div data-testid="plan-drift" role="group" aria-label="Environment changes since the recorded run">
          <Banner tone="warning" title={`The environment changed since the recorded run (${drift.length})`}>
            <ul>
              {drift.map((d, i) => (
                <li key={i} data-aspect={d.aspect} data-step={d.step ?? undefined}>
                  {d.message}
                  {(d.recorded || d.current) && (
                    <span className="ui-meta">
                      {" "}
                      (recorded: <code>{d.recorded ?? "not recorded"}</code>; now: <code>{d.current ?? "unknown"}</code>)
                    </span>
                  )}
                </li>
              ))}
            </ul>
            Approving runs it in the current environment; the results may differ.
          </Banner>
        </div>
      )}

      {notices.length > 0 && (
        <div data-testid="plan-notices">
          <Banner tone="info" title={notices.length === 1 ? "Note" : "Notes"}>
            <ul>
              {notices.map((n, i) => (
                <li key={i}>{n}</li>
              ))}
            </ul>
          </Banner>
        </div>
      )}

      <h3 className="ui-section-title">Inputs, exactly as the run will read them</h3>
      {plan.inputs.length === 0 ? (
        <p className="ui-meta">No external inputs.</p>
      ) : (
        <div data-testid="plan-inputs">
          <SimpleTable<PlanInput>
            aria-label="Inputs, exactly as the run will read them"
            rows={plan.inputs}
            rowKey={(input) => input.id}
            rowProps={(input) => ({ "data-testid": `plan-input-${input.id}` })}
            columns={[
              {
                id: "input",
                title: "Input",
                render: (input) => (
                  <>
                    {input.label} <span className="ui-meta">({PORT_TYPE_LABELS[input.portType]})</span>
                  </>
                ),
              },
              { id: "path", title: "Path", render: (input) => <code className="ui-code">{input.path}</code> },
              { id: "sha", title: "SHA-256", render: (input) => <FullHash hash={input.sha256} label={input.path} /> },
              { id: "size", title: "Size", numeric: true, render: (input) => formatBytes(input.bytes) },
              { id: "version", title: "Version", render: (input) => (input.pinned ? "Pinned" : "Current at review") },
            ]}
          />
        </div>
      )}

      <h3 className="ui-section-title">
        {maxParallel === 1 ? "Steps, in the order they run (one at a time)" : `Steps, in dependency order (up to ${maxParallel} at a time)`}
      </h3>
      <ol className="xp-wf-blocks">
        {groups.map((g) =>
          g.path.length === 0 ? (
            g.items.map(({ step, index }) => <PlanStepView key={step.step} plan={plan} step={step} index={index} />)
          ) : (
            <li key={`group-${g.items[0]!.step.step}`} className="xp-wf-group" data-testid={`plan-group-${g.prefix}`} data-composite-path={g.path.join(" › ")}>
              <p className="xp-wf-group__label ui-group-label">
                <Layers size={14} aria-hidden="true" />
                <span>
                  Inside the workflow step {g.path.map((t) => `“${t}”`).join(" › ")} — {g.items.length} step{g.items.length === 1 ? "" : "s"}, shown in full
                </span>
              </p>
              <ol className="xp-wf-blocks xp-wf-group__steps">
                {g.items.map(({ step, index }) => (
                  <PlanStepView key={step.step} plan={plan} step={step} index={index} />
                ))}
              </ol>
            </li>
          ),
        )}
      </ol>
    </div>
  );
}

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; plan: ReviewPlan };

export interface PlanReviewDialogProps {
  workflow: WorkflowId;
  revision: number | null;
  isOpen: boolean;
  onOpenChange: (open: boolean) => void;
  onStarted: (run: WorkflowRun) => void;
  /**
   * Where the plan comes from (default: `workflow_run_plan` for the revision).
   * Reproduce passes `workflow_run_reproduce`; the plan is approved the same way.
   */
  loadPlan?: () => Promise<ReviewPlan>;
  /** Dialog title prefix (default "Review and run"). */
  heading?: string;
}

/** "Review and run": fetches the plan when opened; starting requires an explicit press of "Start run". */
export function PlanReviewDialog({ workflow, revision, isOpen, onOpenChange, onStarted, loadPlan, heading = "Review and run" }: PlanReviewDialogProps) {
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  const [starting, setStarting] = useState(false);
  const [failure, setFailure] = useState<{ code: string; message: string } | null>(null);
  const reasonId = useId();

  const fetchPlan = useCallback(async () => {
    setLoad({ kind: "loading" });
    setFailure(null);
    try {
      setLoad({ kind: "ready", plan: await (loadPlan ? loadPlan() : workflowRunCommands.plan(workflow, revision)) });
    } catch (e) {
      setLoad({ kind: "error", message: errorMessage(e) });
    }
  }, [workflow, revision, loadPlan]);

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
      title={plan ? `${heading} “${plan.title}”` : heading}
      size="lg"
      isOpen={isOpen}
      onOpenChange={onOpenChange}
      isDismissable={!starting}
      className="xp-wf-plan-dialog"
      footer={
        <>
          {startDisabledReason && plan && (
            <span className="ui-meta xp-wf-plan__reason" id={reasonId} data-testid="plan-start-reason">
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
        {load.kind === "loading" && <LoadingState title="Resolving the plan…" fill={false} />}
        {load.kind === "error" && (
          <ErrorState
            title="Could not make a run plan"
            description={load.message}
            fill={false}
            actions={<Button onPress={() => void fetchPlan()}>Retry</Button>}
          />
        )}
        {plan && (
          <>
            <p className="ui-meta">
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
              <div data-testid="plan-blockers" role="group" aria-label="Why this plan cannot start">
                <Banner tone="danger" title={`Cannot start: ${plan.blockers.length} blocker${plan.blockers.length === 1 ? "" : "s"}`}>
                  <ul>
                    {plan.blockers.map((b, i) => (
                      <li key={i} data-code={b.code} data-step={b.step ?? undefined} data-input={b.input ?? undefined}>
                        {b.message}
                        {b.step ? <span className="ui-meta"> (step {plan.steps.find((s) => s.step === b.step)?.label ?? b.step})</span> : null}
                        {b.input ? <span className="ui-meta"> (input {plan.inputs.find((x) => x.id === b.input)?.label ?? b.input})</span> : null}
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
