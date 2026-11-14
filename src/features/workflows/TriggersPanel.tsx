// Triggers of one workflow (workflow editor, "Triggers" section). Triggers
// are off by default and app-local; only the user enables one, by approving
// a *standing plan* (`workflow_trigger_plan`): the exact revision, runners,
// image identity, trust and budgets, with only the watched inputs' saved
// content allowed to change. Settings (quiet period, rate limits, native
// steps, daily model requests) are chosen next to that review and checked by
// the backend when enabling (`workflow_trigger_enable` with the standing hash;
// `trigger.planStale` when the plan changed). Any other change suspends the
// trigger with a visible reason until it is reviewed again. Agents, imports,
// and generated graphs never enable one (no agent tool; the backend refuses).
import { Bell, BellOff, ChevronLeft, Plus, RefreshCw, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useId, useState } from "react";

import { errorMessage, isCommandError } from "../../lib/ipc";
import { Badge, Banner, Button, Checkbox, Dialog, EmptyState, ErrorState, LoadingState, Section, Select, TextField } from "../../ui/components";
import type {
  TriggerHistoryEvent,
  TriggerKind,
  TriggerState,
  WorkflowDefinition,
  WorkflowTrigger,
  WorkflowTriggerLimits,
  WorkflowTriggerPlan,
  WorkflowTriggerSettings,
} from "./contract";
import { workflowEvents, workflowTriggerCommands } from "./ipc";
import { PORT_TYPE_LABELS } from "./model";
import { FullHash, PlanDetails } from "./PlanReview";
import { openRun } from "./RunView";
import { formatSeconds, formatTime, shortHash } from "./runModel";

/** A standing plan that no longer matches what was reviewed: review it again. */
export const STALE_TRIGGER_CODES = ["trigger.planStale", "workflow.planStale"];
/** Inputs a trigger never watches (lane C refuses them): their content is code. */
const UNWATCHABLE: string[] = ["codeSource", "formalSource"];
/** The trigger store could not be written or read (`trigger.storeUnavailable`). */
const STORE_UNAVAILABLE = "trigger.storeUnavailable";
const STORE_TITLE = "Triggers cannot be saved right now";

/** Lane C's minimum interval (the plan's limits are authoritative once loaded). */
const MIN_INTERVAL_MINUTES = 15;

export const TRIGGER_STATE_VIEW: Record<TriggerState["kind"], { label: string; tone: "success" | "warning" | "neutral"; dashed: boolean }> = {
  active: { label: "Active", tone: "success", dashed: false },
  suspended: { label: "Suspended", tone: "warning", dashed: false },
  disabled: { label: "Off", tone: "neutral", dashed: true },
};

export function historyText(event: TriggerHistoryEvent): string {
  switch (event.kind) {
    case "enabled":
      return `Enabled after review (revision ${event.revision})`;
    case "fired":
      return `Started a run${event.changed.length ? ` after ${event.changed.length} watched input${event.changed.length === 1 ? "" : "s"} changed` : ""}`;
    case "queued":
      return "Queued one run (a run of this workflow was active)";
    case "rateLimited":
      return `Not started: rate limit reached until ${formatTime(event.untilMs)}`;
    case "startRefused":
      return `Not started: ${event.message} (${event.code})`;
    case "suspended":
      return `Suspended: ${event.message}`;
    case "disabled":
      return "Turned off";
  }
}

/** A suspension's title by its code; the backend's message (shown once, as is) says what to do. */
export function suspensionTitle(code: string): string {
  switch (code) {
    case "trigger.planChanged":
      return "Suspended: the approved plan no longer matches";
    case "trigger.trustRevoked":
      return "Suspended: folder trust was revoked";
    case "trigger.nativeNotAllowed":
      return "Suspended: native steps are not allowed";
    default:
      return "Suspended";
  }
}

export function describeKind(kind: TriggerKind, watched: { label: string; path: string }[]): string {
  const names = watched.map((w) => `${w.label} (${w.path})`).join(", ");
  if (kind.kind === "interval") return `Every ${formatSeconds(kind.minutes * 60)}${names ? `; may read new saved content of ${names}` : ""}`;
  return `When the saved content of ${names || "its watched inputs"} changes`;
}

export function describeSettings(t: WorkflowTriggerSettings): string {
  return (
    `Waits for ${formatSeconds(t.debounceSeconds)} without further changes · at most ${t.maxRunsPerHour} run${t.maxRunsPerHour === 1 ? "" : "s"} an hour and ` +
    `${t.maxRunsPerDay} a day · at most one queued follow-up (the latest change wins)` +
    (t.maxModelRequestsPerDay > 0 ? ` · at most ${t.maxModelRequestsPerDay} model requests a day` : "") +
    ` · ${t.allowNative ? "native steps allowed without asking (your account's privileges)" : "native steps not allowed"}`
  );
}

/** Problems with settings against the plan's limits (the backend checks them again). */
export function settingsProblems(s: WorkflowTriggerSettings, limits: WorkflowTriggerLimits, plan: Pick<WorkflowTriggerPlan, "nativeSteps" | "modelRequestsPerRun">): string[] {
  const problems: string[] = [];
  if (s.debounceSeconds < limits.minDebounceSeconds || s.debounceSeconds > limits.maxDebounceSeconds)
    problems.push(`The quiet period is ${formatSeconds(limits.minDebounceSeconds)} to ${formatSeconds(limits.maxDebounceSeconds)}.`);
  if (s.maxRunsPerHour < 1 || s.maxRunsPerHour > limits.maxRunsPerHour) problems.push(`Runs per hour: 1 to ${limits.maxRunsPerHour}.`);
  if (s.maxRunsPerDay < 1 || s.maxRunsPerDay > limits.maxRunsPerDay) problems.push(`Runs per day: 1 to ${limits.maxRunsPerDay}.`);
  if (s.maxRunsPerHour > s.maxRunsPerDay) problems.push(`Runs per hour cannot exceed runs per day (${s.maxRunsPerDay}).`);
  if (plan.modelRequestsPerRun > 0 && (s.maxModelRequestsPerDay < plan.modelRequestsPerRun || s.maxModelRequestsPerDay > limits.maxModelRequestsPerDay))
    problems.push(`Model requests per day: ${plan.modelRequestsPerRun} (one run) to ${limits.maxModelRequestsPerDay}.`);
  if (plan.nativeSteps.length > 0 && !s.allowNative) problems.push("This plan runs steps natively: allow them explicitly to enable the trigger.");
  return problems;
}

function NumberField({
  label,
  value,
  min,
  max,
  onChange,
  testId,
  description,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (n: number) => void;
  testId: string;
  description?: string;
}) {
  const [text, setText] = useState(String(value));
  useEffect(() => setText(String(value)), [value]);
  const n = Number(text);
  const invalid = !/^\d+$/.test(text.trim()) || n < min || n > max;
  return (
    <TextField
      label={`${label} (${min}–${max})`}
      value={text}
      isInvalid={invalid}
      errorMessage={invalid ? `A whole number from ${min} to ${max}.` : undefined}
      description={description}
      onChange={(v) => {
        setText(v);
        const parsed = Number(v);
        if (/^\d+$/.test(v.trim()) && parsed >= min && parsed <= max) onChange(parsed);
      }}
      data-testid={testId}
    />
  );
}

// ----- choose + review the standing plan -----------------------------------------------------------

type Phase = { kind: "choose" } | { kind: "loading" } | { kind: "error"; message: string } | { kind: "review"; standing: WorkflowTriggerPlan };

export interface TriggerDialogProps {
  workflow: string;
  revision: number;
  definition: WorkflowDefinition;
  /** Review again an existing trigger (suspended or off): its kind and settings. */
  initial?: { kind: TriggerKind; settings: WorkflowTriggerSettings };
  /** The workflow's current trigger, which enabling replaces (one trigger per workflow). */
  replaces?: WorkflowTrigger | null;
  isOpen: boolean;
  onOpenChange: (open: boolean) => void;
  onEnabled: (trigger: WorkflowTrigger) => void;
}

const NEW_KIND: TriggerKind = { kind: "inputsChanged", inputs: [] };

export function TriggerDialog({ workflow, revision, definition, initial, replaces = null, isOpen, onOpenChange, onEnabled }: TriggerDialogProps) {
  const [kind, setKind] = useState<TriggerKind>(initial?.kind ?? NEW_KIND);
  const [settings, setSettings] = useState<WorkflowTriggerSettings | null>(initial?.settings ?? null);
  const [phase, setPhase] = useState<Phase>({ kind: "choose" });
  const [enabling, setEnabling] = useState(false);
  const [failure, setFailure] = useState<{ code: string; message: string } | null>(null);
  const reasonId = useId();

  useEffect(() => {
    if (isOpen) {
      setKind(initial?.kind ?? NEW_KIND);
      setSettings(initial?.settings ?? null);
      setPhase({ kind: "choose" });
      setFailure(null);
    }
  }, [isOpen, initial]);

  const review = useCallback(async () => {
    setPhase({ kind: "loading" });
    setFailure(null);
    try {
      const standing = await workflowTriggerCommands.plan({ workflow, kind });
      // Native steps need explicit consent in every review: never carried over from
      // the trigger being reviewed again, an earlier review, or the defaults (R-m9 F4).
      setSettings((current) => ({ ...(current ?? standing.defaults), allowNative: false }));
      setPhase({ kind: "review", standing });
    } catch (e) {
      setPhase({ kind: "error", message: errorMessage(e) });
    }
  }, [workflow, kind]);

  const standing = phase.kind === "review" ? phase.standing : null;
  const stale = failure !== null && STALE_TRIGGER_CODES.includes(failure.code);
  const problems = standing && settings ? settingsProblems(settings, standing.limits, standing) : [];
  const watchedIds = kind.inputs;
  const chooseProblem =
    kind.kind === "inputsChanged" && watchedIds.length === 0
      ? "Choose at least one input to watch."
      : kind.kind === "interval" && kind.minutes < MIN_INTERVAL_MINUTES
        ? `The interval is at least ${MIN_INTERVAL_MINUTES} minutes.`
        : null;

  const enable = async () => {
    if (!standing || !settings || standing.blockers.length || problems.length) return;
    setEnabling(true);
    setFailure(null);
    try {
      const trigger = await workflowTriggerCommands.enable({ plan: standing.plan.plan, standingHash: standing.standingHash, settings });
      onOpenChange(false);
      onEnabled(trigger);
    } catch (e) {
      setFailure({ code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
    } finally {
      setEnabling(false);
    }
  };

  // Code and Lean sources are code (Lean elaboration runs code): never watched; a change needs a new review.
  const watchable = definition.inputs.filter((i) => !UNWATCHABLE.includes(i.portType));
  const unwatchable = definition.inputs.filter((i) => UNWATCHABLE.includes(i.portType));
  const enableReason = !standing
    ? null
    : standing.blockers.length
      ? `This standing plan cannot be enabled: ${standing.blockers.length} blocker${standing.blockers.length === 1 ? "" : "s"} listed above.`
      : stale
        ? "The plan changed since you reviewed it. Review it again."
        : problems.length
          ? (problems[0] ?? null)
          : null;
  const setInputs = (inputs: string[]) => setKind(kind.kind === "interval" ? { ...kind, inputs } : { kind: "inputsChanged", inputs });

  return (
    <Dialog
      title={standing ? "Review the standing plan" : "New trigger"}
      size="lg"
      isOpen={isOpen}
      onOpenChange={onOpenChange}
      isDismissable={!enabling}
      className="xp-wf-plan-dialog"
      footer={
        <>
          {enableReason && (
            <span className="xp-wf__muted xp-wf-plan__reason" id={reasonId} data-testid="trigger-enable-reason">
              {enableReason}
            </span>
          )}
          <Button slot="close" isDisabled={enabling}>
            Cancel
          </Button>
          {phase.kind === "review" || phase.kind === "error" ? (
            <>
              <Button icon={ChevronLeft} isDisabled={enabling} onPress={() => setPhase({ kind: "choose" })} data-testid="trigger-back">
                Change what it watches
              </Button>
              <Button
                variant="primary"
                icon={ShieldCheck}
                isPending={enabling}
                isDisabled={enabling || !standing || enableReason !== null}
                aria-describedby={enableReason ? reasonId : undefined}
                onPress={() => void enable()}
                data-testid="trigger-enable"
              >
                {replaces ? "Replace trigger" : "Enable trigger"}
              </Button>
            </>
          ) : (
            <Button variant="primary" isDisabled={chooseProblem !== null || phase.kind === "loading"} onPress={() => void review()} data-testid="trigger-review">
              Review standing plan…
            </Button>
          )}
        </>
      }
    >
      <div className="xp-wf-plan-body" data-testid="trigger-dialog" data-phase={phase.kind}>
        {replaces && (
          <div data-testid="trigger-replaces">
            <Banner tone="warning" title="This replaces the current trigger">
              This workflow already has a trigger ({TRIGGER_STATE_VIEW[replaces.state.kind].label.toLowerCase()}). Enabling this one replaces it: a
              workflow has one trigger.
            </Banner>
          </div>
        )}
        {phase.kind === "choose" && (
          <>
            <p className="xp-wf__muted">
              A trigger starts runs of the latest saved revision (now {revision}) by itself, only under the standing plan you review next.
              It never fires when the project opens or for changes made while it was closed, and it stops when the project closes or
              trust is revoked.
            </p>
            <div className="xp-wf__form">
              <Select<TriggerKind["kind"]>
                label="Starts a run"
                value={kind.kind}
                onChange={(k) => setKind(k === "interval" ? { kind: "interval", minutes: 60, inputs: kind.inputs } : { kind: "inputsChanged", inputs: kind.inputs })}
                options={[
                  { id: "inputsChanged", label: "When watched inputs are saved with new content" },
                  { id: "interval", label: `On a fixed interval (at least ${MIN_INTERVAL_MINUTES} minutes)` },
                ]}
                data-testid="trigger-kind"
              />
              {kind.kind === "interval" && (
                <NumberField
                  label="Every (minutes)"
                  value={kind.minutes}
                  min={MIN_INTERVAL_MINUTES}
                  max={7 * 24 * 60}
                  onChange={(minutes) => setKind({ ...kind, minutes })}
                  testId="trigger-interval"
                />
              )}
            </div>
            <fieldset className="xp-wf__fieldset" data-testid="trigger-watch">
              <legend className="ui-field__label">
                {kind.kind === "interval" ? "Inputs whose saved content may change between runs" : "Watched inputs (their saved content may change without asking)"}
              </legend>
              {watchable.length === 0 && <p className="xp-wf__muted">This workflow has no inputs that can be watched.</p>}
              <div className="xp-wf__inline">
                {watchable.map((input) => {
                  const path = input.source.kind === "resource" ? input.source.path : "no file chosen";
                  return (
                    <Checkbox
                      key={input.id}
                      isSelected={watchedIds.includes(input.id)}
                      onChange={(on) => setInputs(on ? [...watchedIds, input.id] : watchedIds.filter((x) => x !== input.id))}
                      data-testid={`trigger-watch-${input.id}`}
                    >
                      {input.label} <span className="xp-wf__muted">({path}, {PORT_TYPE_LABELS[input.portType]})</span>
                    </Checkbox>
                  );
                })}
              </div>
              {unwatchable.length > 0 && (
                <p className="xp-wf__muted">
                  Code and Lean sources cannot be watched ({unwatchable.map((i) => i.label).join(", ")}): changed code always needs a new
                  review.
                </p>
              )}
            </fieldset>
            {chooseProblem && (
              <p className="xp-wf__muted" data-testid="trigger-problems">
                {chooseProblem}
              </p>
            )}
          </>
        )}
        {phase.kind === "loading" && <LoadingState title="Resolving the standing plan…" />}
        {phase.kind === "error" && (
          <ErrorState title="Could not make a standing plan" description={phase.message} actions={<Button onPress={() => void review()}>Retry</Button>} />
        )}
        {standing && settings && (
          <div data-testid="trigger-standing" data-standing-hash={standing.standingHash} data-plan={standing.plan.plan}>
            <p className="xp-wf__muted">
              Enabling approves this plan as a standing plan: each time the trigger fires, the run plan is recomputed and must match it
              exactly, except for the watched content. Review it before {formatTime(standing.expiresAtMs)}.
            </p>
            {failure && (
              <div data-testid="trigger-enable-error" data-code={failure.code}>
                {stale ? (
                  <Banner
                    tone="stale"
                    title="The plan changed since you reviewed it"
                    live
                    actions={
                      <Button size="sm" icon={RefreshCw} onPress={() => void review()} data-testid="trigger-review-new">
                        Review the new plan
                      </Button>
                    }
                  >
                    {failure.message} Nothing was enabled.
                  </Banner>
                ) : (
                  <Banner tone="danger" title={failure.code === STORE_UNAVAILABLE ? STORE_TITLE : "The trigger was not enabled"} live>
                    {failure.message} Nothing was enabled.
                  </Banner>
                )}
              </div>
            )}
            {standing.blockers.length > 0 && (
              <div data-testid="trigger-blockers" role="group" aria-label="Why this standing plan cannot be enabled">
                <Banner tone="danger" title={`Cannot enable: ${standing.blockers.length} blocker${standing.blockers.length === 1 ? "" : "s"}`}>
                  <ul>
                    {standing.blockers.map((b, i) => (
                      <li key={i} data-code={b.code}>
                        {b.message}
                      </li>
                    ))}
                  </ul>
                </Banner>
              </div>
            )}
            <section className="xp-wf-standing" data-testid="trigger-may-change" aria-label="What may change without asking">
              <h3 className="xp-wf-plan__heading">What may change without asking</h3>
              <ul data-testid="trigger-may-change-list">
                {standing.mayChange.map((m, i) => (
                  <li key={i}>{m}</li>
                ))}
              </ul>
              {standing.watched.length > 0 && (
                <p className="xp-wf__muted" data-testid="trigger-watched">
                  Watched:{" "}
                  {standing.watched.map((w, i) => (
                    <span key={w.input} data-input={w.input}>
                      {i > 0 ? ", " : ""}
                      {w.label} (<code>{w.path}</code>)
                    </span>
                  ))}
                </p>
              )}
              <h3 className="xp-wf-plan__heading">Anything else suspends it until you review it again</h3>
              <ul data-testid="trigger-suspends-on">
                {standing.suspendsOn.map((m, i) => (
                  <li key={i}>{m}</li>
                ))}
              </ul>
            </section>
            {standing.notices.length > 0 && (
              <ul className="xp-wf-plan__notices" data-testid="trigger-notices">
                {standing.notices.map((n, i) => (
                  <li key={i}>
                    <Banner tone="info" title="Note">
                      {n}
                    </Banner>
                  </li>
                ))}
              </ul>
            )}
            <h3 className="xp-wf-plan__heading">Settings</h3>
            <div className="xp-wf__form" data-testid="trigger-settings">
              <NumberField
                label="Quiet period (seconds)"
                value={settings.debounceSeconds}
                min={standing.limits.minDebounceSeconds}
                max={standing.limits.maxDebounceSeconds}
                onChange={(debounceSeconds) => setSettings({ ...settings, debounceSeconds })}
                testId="trigger-debounce"
                description="Fires only after no further saved change for this long."
              />
              <NumberField
                label="Runs per hour, at most"
                value={settings.maxRunsPerHour}
                min={1}
                max={standing.limits.maxRunsPerHour}
                onChange={(maxRunsPerHour) => setSettings({ ...settings, maxRunsPerHour })}
                testId="trigger-per-hour"
              />
              <NumberField
                label="Runs per day, at most"
                value={settings.maxRunsPerDay}
                min={1}
                max={standing.limits.maxRunsPerDay}
                onChange={(maxRunsPerDay) => setSettings({ ...settings, maxRunsPerDay })}
                testId="trigger-per-day"
              />
              {standing.modelRequestsPerRun > 0 && (
                <NumberField
                  label="Model requests per day, at most"
                  value={settings.maxModelRequestsPerDay}
                  min={standing.modelRequestsPerRun}
                  max={standing.limits.maxModelRequestsPerDay}
                  onChange={(maxModelRequestsPerDay) => setSettings({ ...settings, maxModelRequestsPerDay })}
                  testId="trigger-model-per-day"
                  description={`Each run counts its full budget of ${standing.modelRequestsPerRun} model request${standing.modelRequestsPerRun === 1 ? "" : "s"}.`}
                />
              )}
              <div className="xp-wf__wide" data-testid="trigger-native" data-native-steps={standing.nativeSteps.length} data-allowed={String(settings.allowNative)}>
                {standing.nativeSteps.length === 0 ? (
                  <p className="xp-wf__muted">No step of this plan runs natively.</p>
                ) : (
                  <>
                    <p className="xp-wf__muted">
                      Runs natively with your account's privileges on every fire: {standing.nativeSteps.map((s) => `“${s}”`).join(", ")}.
                    </p>
                    <Checkbox isSelected={settings.allowNative} onChange={(allowNative) => setSettings({ ...settings, allowNative })} data-testid="trigger-allow-native">
                      Allow {standing.nativeSteps.length === 1 ? "this step" : "these steps"} to run natively without asking each time
                    </Checkbox>
                  </>
                )}
              </div>
            </div>
            {problems.length > 0 && (
              <ul className="xp-wf__muted" data-testid="trigger-problems">
                {problems.map((p) => (
                  <li key={p}>{p}</li>
                ))}
              </ul>
            )}
            <dl className="xp-wf-facts">
              <dt>Standing hash</dt>
              <dd>
                <FullHash hash={standing.standingHash} label="the standing plan" />
              </dd>
            </dl>
            <PlanDetails plan={standing.plan} />
          </div>
        )}
      </div>
    </Dialog>
  );
}

// ----- list -------------------------------------------------------------------------------------

function TriggerCard({
  trigger,
  currentRevision,
  onChanged,
  onReapprove,
}: {
  trigger: WorkflowTrigger;
  currentRevision: number;
  onChanged: () => void;
  onReapprove: (trigger: WorkflowTrigger) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<{ code: string; message: string } | null>(null);
  const view = TRIGGER_STATE_VIEW[trigger.state.kind];
  const disable = async () => {
    setBusy(true);
    setError(null);
    try {
      await workflowTriggerCommands.disable(trigger.id);
      onChanged();
    } catch (e) {
      setError({ code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };
  const history = [...trigger.history].sort((a, b) => b.atMs - a.atMs).slice(0, 20);
  return (
    <li
      className="xp-wf__card"
      data-testid={`workflow-trigger-${trigger.id}`}
      data-state={trigger.state.kind}
      data-queued={String(trigger.queued)}
      data-listening={String(trigger.listening)}
    >
      <div className="xp-wf__row">
        <div className="xp-wf__grow">
          <strong>{describeKind(trigger.kind, trigger.watched)}</strong>
          <p className="xp-wf__muted">
            Revision {trigger.revision}
            {trigger.revision !== currentRevision ? ` (the workflow is now at revision ${currentRevision})` : ""} · {describeSettings(trigger)}
          </p>
          <p className="xp-wf__muted" data-testid={`trigger-last-fired-${trigger.id}`}>
            {trigger.lastFiredMs !== null ? `Last fired ${formatTime(trigger.lastFiredMs)}` : "Has not fired yet"} · runs this hour {trigger.runsLastHour} of{" "}
            {trigger.maxRunsPerHour}, today {trigger.runsLastDay} of {trigger.maxRunsPerDay}
            {trigger.maxModelRequestsPerDay > 0 ? ` · model requests today ${trigger.modelRequestsLastDay} of ${trigger.maxModelRequestsPerDay}` : ""}
            {trigger.nextCheckMs !== null ? ` · next check ${formatTime(trigger.nextCheckMs)}` : ""} · enabled {formatTime(trigger.enabledAtMs)}
          </p>
        </div>
        {trigger.queued && (
          <Badge tone="info" dashed>
            1 run queued
          </Badge>
        )}
        {trigger.state.kind === "active" && !trigger.listening && (
          <Badge tone="neutral" dashed title="It listens once its project is open and has finished indexing.">
            Not listening yet
          </Badge>
        )}
        <Badge tone={view.tone} dashed={view.dashed} icon={trigger.state.kind === "active" ? Bell : BellOff}>
          {view.label}
        </Badge>
        {trigger.state.kind === "disabled" ? (
          <Button size="sm" onPress={() => onReapprove(trigger)} data-testid={`trigger-reapprove-${trigger.id}`}>
            Review and turn on…
          </Button>
        ) : (
          <Button size="sm" variant="secondary" icon={BellOff} isPending={busy} isDisabled={busy} onPress={() => void disable()} data-testid={`trigger-disable-${trigger.id}`}>
            Turn off
          </Button>
        )}
      </div>
      {trigger.state.kind === "suspended" && (
        <div data-testid={`trigger-suspended-${trigger.id}`} data-code={trigger.state.code}>
          <Banner
            tone="warning"
            title={suspensionTitle(trigger.state.code)}
            actions={
              <Button size="sm" icon={RefreshCw} onPress={() => onReapprove(trigger)} data-testid={`trigger-reapprove-${trigger.id}`}>
                Review again…
              </Button>
            }
          >
            {trigger.state.message}
          </Banner>
        </div>
      )}
      {trigger.storeProblem && (
        <div data-testid={`trigger-store-problem-${trigger.id}`}>
          <Banner tone="warning" title="The trigger store could not be saved or read">
            {trigger.storeProblem} Changes shown here may not survive a restart until it can be saved.
          </Banner>
        </div>
      )}
      {error && (
        <div data-testid={`trigger-error-${trigger.id}`} data-code={error.code}>
          <Banner tone="danger" title={error.code === STORE_UNAVAILABLE ? STORE_TITLE : "Not changed"} live>
            {error.message}
          </Banner>
        </div>
      )}
      {history.length > 0 && (
        <Section title={`Recent history (${history.length})`} collapsible defaultExpanded={false}>
          <ul className="xp-wf-history" data-testid={`trigger-history-${trigger.id}`}>
            {history.map((h, i) => {
              const event = h.event;
              return (
                <li key={i} data-event={event.kind}>
                  <span className="xp-wf__muted">{formatTime(h.atMs)}</span> {historyText(event)}
                  {event.kind === "fired" && (
                    <>
                      {event.changed.map((c) => (
                        <span key={c.input} className="xp-wf__muted">
                          {" "}
                          ({c.input}: {c.before ? shortHash(c.before) : "absent"} → {c.after ? shortHash(c.after) : "absent"})
                        </span>
                      ))}
                      <Button size="sm" variant="ghost" onPress={() => openRun({ id: event.run, title: trigger.title })}>
                        Open run
                      </Button>
                    </>
                  )}
                </li>
              );
            })}
          </ul>
        </Section>
      )}
    </li>
  );
}

export function TriggersSection({
  workflow,
  revision,
  definition,
  reviewBlock,
}: {
  workflow: string;
  revision: number;
  definition: WorkflowDefinition;
  /** Why the saved revision cannot be reviewed now (unsaved, incomplete, not runnable); null when it can. */
  reviewBlock: string | null;
}) {
  const [triggers, setTriggers] = useState<WorkflowTrigger[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState<{ open: boolean; initial?: TriggerDialogProps["initial"] }>({ open: false });
  const reasonId = useId();

  const refresh = useCallback(async () => {
    try {
      setTriggers(await workflowTriggerCommands.list(workflow));
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
      .trigger((event) => {
        if (event.workflow === workflow) void refresh();
      })
      .then((u) => {
        if (disposed) u();
        else unlisten = u;
      })
      // Outside the desktop app there are no events; the list still loads once.
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [workflow, refresh]);

  const reapprove = (t: WorkflowTrigger) =>
    setDialog({
      open: true,
      initial: {
        kind: t.kind,
        settings: { debounceSeconds: t.debounceSeconds, maxRunsPerHour: t.maxRunsPerHour, maxRunsPerDay: t.maxRunsPerDay, allowNative: t.allowNative, maxModelRequestsPerDay: t.maxModelRequestsPerDay },
      },
    });

  return (
    <section className="xp-wf__group" aria-label="Triggers" data-testid="workflow-triggers">
      <h3 className="xp-wf__group-title">Triggers</h3>
      <p className="xp-wf__muted">
        Off by default. A trigger starts runs by itself only under a standing plan you approve here; agents and imported workflows never
        enable one.
      </p>
      {error && (
        <Banner tone="danger" title="Triggers are unavailable" actions={<Button size="sm" onPress={() => void refresh()}>Retry</Button>}>
          {error}
        </Banner>
      )}
      {triggers && triggers.length === 0 && <EmptyState fill={false} title="No triggers" description="Runs start only when you review and start them." />}
      {triggers && triggers.length > 0 && (
        <ul className="xp-wf__cards" data-testid="workflow-trigger-list">
          {triggers.map((t) => (
            <TriggerCard key={t.id} trigger={t} currentRevision={revision} onChanged={() => void refresh()} onReapprove={reapprove} />
          ))}
        </ul>
      )}
      <div className="xp-wf__row">
        <Button
          icon={Plus}
          isDisabled={reviewBlock !== null}
          aria-describedby={reviewBlock ? reasonId : undefined}
          onPress={() => setDialog({ open: true })}
          data-testid="workflow-trigger-add"
        >
          {triggers && triggers.length > 0 ? "Replace trigger…" : "New trigger…"}
        </Button>
        {reviewBlock && (
          <span className="xp-wf__muted" id={reasonId}>
            {reviewBlock}
          </span>
        )}
      </div>
      <TriggerDialog
        workflow={workflow}
        revision={revision}
        definition={definition}
        initial={dialog.initial}
        replaces={triggers?.[0] ?? null}
        isOpen={dialog.open}
        onOpenChange={(open) => setDialog((d) => ({ ...d, open }))}
        onEnabled={() => void refresh()}
      />
    </section>
  );
}
