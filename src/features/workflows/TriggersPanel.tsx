// Triggers of one workflow (workflow editor, "Triggers" section). Triggers
// are off by default and app-local; only the user enables one, by approving
// a *standing plan*: the exact revision, runners, image identity, trust and
// budgets, with only the declared watched inputs' saved content allowed to
// change. Any other change suspends the trigger with a visible reason until it
// is reviewed again. Agents, imports, and generated graphs never enable one
// (there is no agent tool; the backend refuses). Nothing here runs a step:
// a trigger fires inside the application under its approved standing plan.
import { Bell, BellOff, ChevronLeft, Plus, RefreshCw, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useId, useState } from "react";

import { errorMessage, isCommandError } from "../../lib/ipc";
import { Badge, Banner, Button, Checkbox, Dialog, EmptyState, ErrorState, LoadingState, Section, Select, TextField } from "../../ui/components";
import type { TriggerSettings, TriggerState, WorkflowDefinition, WorkflowStandingPlan, WorkflowTrigger } from "./contract";
import { TRIGGER_BOUNDS } from "./contract";
import { workflowEvents, workflowTriggerCommands } from "./ipc";
import { PORT_TYPE_LABELS } from "./model";
import { FullHash, PlanDetails } from "./PlanReview";
import { openRun } from "./RunView";
import { formatSeconds, formatTime, STALE_PLAN_CODES } from "./runModel";

export const TRIGGER_STATE_VIEW: Record<TriggerState["kind"], { label: string; tone: "success" | "warning" | "neutral"; dashed: boolean }> = {
  active: { label: "Active", tone: "success", dashed: false },
  suspended: { label: "Suspended", tone: "warning", dashed: false },
  disabled: { label: "Off", tone: "neutral", dashed: true },
};

export const TRIGGER_EVENT_LABELS: Record<string, string> = {
  fired: "Started a run",
  queued: "Queued one run (a run was active)",
  coalesced: "Coalesced into the queued run",
  rateLimited: "Not started: rate limit reached",
  suspended: "Suspended",
  enabled: "Enabled after review",
  disabled: "Turned off",
};

export function describeKind(trigger: Pick<WorkflowTrigger, "kind">, def: WorkflowDefinition | null): string {
  if (trigger.kind.kind === "interval") return `Every ${formatSeconds(trigger.kind.minutes * 60)}`;
  const names = trigger.kind.inputs.map((id) => {
    const input = def?.inputs.find((i) => i.id === id);
    const path = input?.source.kind === "resource" ? input.source.path : null;
    return path ? `${input?.label ?? id} (${path})` : (input?.label ?? id);
  });
  return `When the saved content of ${names.length ? names.join(", ") : "its watched inputs"} changes`;
}

export function describeLimits(t: Pick<TriggerSettings, "debounceSeconds" | "maxRunsPerHour" | "maxRunsPerDay" | "allowNative">): string {
  return (
    `Waits for ${formatSeconds(t.debounceSeconds)} without further changes · at most ${t.maxRunsPerHour} run${t.maxRunsPerHour === 1 ? "" : "s"} an hour and ` +
    `${t.maxRunsPerDay} a day · at most one queued follow-up (the latest change wins) · ` +
    (t.allowNative ? "native steps allowed without asking (your account's privileges)" : "native steps not allowed")
  );
}

// ----- settings + standing plan review -----------------------------------------------------------

const DEFAULTS: TriggerSettings = {
  kind: { kind: "inputsChanged", inputs: [] },
  debounceSeconds: 30,
  maxRunsPerHour: 6,
  maxRunsPerDay: 24,
  allowNative: false,
};

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

export function settingsProblems(settings: TriggerSettings): string[] {
  const problems: string[] = [];
  const b = TRIGGER_BOUNDS;
  if (settings.kind.kind === "inputsChanged" && settings.kind.inputs.length === 0) problems.push("Choose at least one input to watch.");
  if (settings.kind.kind === "interval" && (settings.kind.minutes < b.intervalMinutes.min || settings.kind.minutes > b.intervalMinutes.max))
    problems.push(`The interval is ${b.intervalMinutes.min} minutes to ${b.intervalMinutes.max / 60 / 24} days.`);
  if (settings.debounceSeconds < b.debounceSeconds.min || settings.debounceSeconds > b.debounceSeconds.max) problems.push("The quiet period is 2 seconds to 1 hour.");
  if (settings.maxRunsPerHour < 1 || settings.maxRunsPerHour > b.maxRunsPerHour.max) problems.push("At most 12 runs an hour.");
  if (settings.maxRunsPerDay < 1 || settings.maxRunsPerDay > b.maxRunsPerDay.max) problems.push("At most 100 runs a day.");
  return problems;
}

type Phase = { kind: "settings" } | { kind: "loading" } | { kind: "error"; message: string } | { kind: "review"; standing: WorkflowStandingPlan };

export interface TriggerDialogProps {
  workflow: string;
  revision: number;
  definition: WorkflowDefinition;
  /** Re-approve an existing trigger's settings (a suspended or disabled one). */
  initial?: TriggerSettings;
  isOpen: boolean;
  onOpenChange: (open: boolean) => void;
  onEnabled: (trigger: WorkflowTrigger) => void;
}

export function TriggerDialog({ workflow, revision, definition, initial, isOpen, onOpenChange, onEnabled }: TriggerDialogProps) {
  const [settings, setSettings] = useState<TriggerSettings>(initial ?? DEFAULTS);
  const [phase, setPhase] = useState<Phase>({ kind: "settings" });
  const [enabling, setEnabling] = useState(false);
  const [failure, setFailure] = useState<{ code: string; message: string } | null>(null);
  const reasonId = useId();

  useEffect(() => {
    if (isOpen) {
      setSettings(initial ?? DEFAULTS);
      setPhase({ kind: "settings" });
      setFailure(null);
    }
  }, [isOpen, initial]);

  const review = useCallback(async () => {
    setPhase({ kind: "loading" });
    setFailure(null);
    try {
      setPhase({ kind: "review", standing: await workflowTriggerCommands.plan({ workflow, revision, ...settings }) });
    } catch (e) {
      setPhase({ kind: "error", message: errorMessage(e) });
    }
  }, [workflow, revision, settings]);

  const standing = phase.kind === "review" ? phase.standing : null;
  const blockers = standing ? [...standing.blockers, ...standing.plan.blockers.map((b) => ({ code: b.code, message: b.message }))] : [];
  const stale = failure !== null && STALE_PLAN_CODES.includes(failure.code);
  const problems = settingsProblems(settings);

  const enable = async () => {
    if (!standing || blockers.length) return;
    setEnabling(true);
    setFailure(null);
    try {
      const trigger = await workflowTriggerCommands.enable({ plan: standing.plan.plan, standingHash: standing.standingHash, ...standing.settings });
      onOpenChange(false);
      onEnabled(trigger);
    } catch (e) {
      setFailure({ code: isCommandError(e) ? e.code : "error", message: errorMessage(e) });
    } finally {
      setEnabling(false);
    }
  };

  const watchable = definition.inputs.filter((i) => i.portType !== "codeSource");
  const unwatchable = definition.inputs.filter((i) => i.portType === "codeSource");
  const enableReason = !standing
    ? null
    : blockers.length
      ? `This standing plan cannot be enabled: ${blockers.length} blocker${blockers.length === 1 ? "" : "s"} listed above.`
      : stale
        ? "The plan changed since you reviewed it. Review it again."
        : null;
  const nativeSteps = standing?.plan.execution.nativeSteps.length ?? 0;
  const modelSteps = standing?.plan.execution.modelSteps.length ?? 0;

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
              <Button icon={ChevronLeft} isDisabled={enabling} onPress={() => setPhase({ kind: "settings" })} data-testid="trigger-back">
                Change settings
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
                Enable trigger
              </Button>
            </>
          ) : (
            <Button variant="primary" isDisabled={problems.length > 0 || phase.kind === "loading"} onPress={() => void review()} data-testid="trigger-review">
              Review standing plan…
            </Button>
          )}
        </>
      }
    >
      <div className="xp-wf-plan-body" data-testid="trigger-dialog" data-phase={phase.kind}>
        {phase.kind === "settings" && (
          <>
            <p className="xp-wf__muted">
              A trigger starts runs of revision {revision} by itself, only under the standing plan you review next. It never fires when
              the project opens or for changes made while it was closed, and it stops when the project closes or trust is revoked.
            </p>
            <div className="xp-wf__form">
              <Select<"inputsChanged" | "interval">
                label="Starts a run"
                value={settings.kind.kind}
                onChange={(k) =>
                  setSettings({ ...settings, kind: k === "interval" ? { kind: "interval", minutes: 60 } : { kind: "inputsChanged", inputs: [] } })
                }
                options={[
                  { id: "inputsChanged", label: "When watched inputs are saved with new content" },
                  { id: "interval", label: "On a fixed interval (at least 15 minutes)" },
                ]}
                data-testid="trigger-kind"
              />
              {settings.kind.kind === "interval" && (
                <NumberField
                  label="Every (minutes)"
                  value={settings.kind.minutes}
                  min={TRIGGER_BOUNDS.intervalMinutes.min}
                  max={TRIGGER_BOUNDS.intervalMinutes.max}
                  onChange={(minutes) => setSettings({ ...settings, kind: { kind: "interval", minutes } })}
                  testId="trigger-interval"
                />
              )}
            </div>
            {settings.kind.kind === "inputsChanged" && (
              <fieldset className="xp-wf__fieldset" data-testid="trigger-watch">
                <legend className="ui-field__label">Watched inputs (their saved content may change without asking)</legend>
                {watchable.length === 0 && <p className="xp-wf__muted">This workflow has no inputs that can be watched.</p>}
                <div className="xp-wf__inline">
                  {watchable.map((input) => {
                    const inputs = settings.kind.kind === "inputsChanged" ? settings.kind.inputs : [];
                    const path = input.source.kind === "resource" ? input.source.path : "no file chosen";
                    return (
                      <Checkbox
                        key={input.id}
                        isSelected={inputs.includes(input.id)}
                        onChange={(on) =>
                          setSettings({ ...settings, kind: { kind: "inputsChanged", inputs: on ? [...inputs, input.id] : inputs.filter((x) => x !== input.id) } })
                        }
                        data-testid={`trigger-watch-${input.id}`}
                      >
                        {input.label} <span className="xp-wf__muted">({path}, {PORT_TYPE_LABELS[input.portType]})</span>
                      </Checkbox>
                    );
                  })}
                </div>
                {unwatchable.length > 0 && (
                  <p className="xp-wf__muted">
                    Code inputs cannot be watched ({unwatchable.map((i) => i.label).join(", ")}): changed code always needs a new review.
                  </p>
                )}
              </fieldset>
            )}
            <div className="xp-wf__form">
              <NumberField
                label="Quiet period (seconds)"
                value={settings.debounceSeconds}
                min={TRIGGER_BOUNDS.debounceSeconds.min}
                max={TRIGGER_BOUNDS.debounceSeconds.max}
                onChange={(debounceSeconds) => setSettings({ ...settings, debounceSeconds })}
                testId="trigger-debounce"
                description="Fires only after no further change for this long."
              />
              <NumberField
                label="Runs per hour, at most"
                value={settings.maxRunsPerHour}
                min={TRIGGER_BOUNDS.maxRunsPerHour.min}
                max={TRIGGER_BOUNDS.maxRunsPerHour.max}
                onChange={(maxRunsPerHour) => setSettings({ ...settings, maxRunsPerHour })}
                testId="trigger-per-hour"
              />
              <NumberField
                label="Runs per day, at most"
                value={settings.maxRunsPerDay}
                min={TRIGGER_BOUNDS.maxRunsPerDay.min}
                max={TRIGGER_BOUNDS.maxRunsPerDay.max}
                onChange={(maxRunsPerDay) => setSettings({ ...settings, maxRunsPerDay })}
                testId="trigger-per-day"
              />
              <Checkbox isSelected={settings.allowNative} onChange={(allowNative) => setSettings({ ...settings, allowNative })} data-testid="trigger-allow-native">
                Allow steps that run natively with your account's privileges to start without asking
              </Checkbox>
            </div>
            {problems.length > 0 && (
              <ul className="xp-wf__muted" data-testid="trigger-problems">
                {problems.map((p) => (
                  <li key={p}>{p}</li>
                ))}
              </ul>
            )}
          </>
        )}
        {phase.kind === "loading" && <LoadingState title="Resolving the standing plan…" />}
        {phase.kind === "error" && (
          <ErrorState title="Could not make a standing plan" description={phase.message} actions={<Button onPress={() => void review()}>Retry</Button>} />
        )}
        {standing && (
          <div data-testid="trigger-standing" data-standing-hash={standing.standingHash} data-plan={standing.plan.plan}>
            <p className="xp-wf__muted">
              Enabling approves this plan as a standing plan: each time the trigger fires, the run plan is recomputed and must match it
              exactly, except for the watched content below.
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
                  <Banner tone="danger" title="The trigger was not enabled" live>
                    {failure.message}
                  </Banner>
                )}
              </div>
            )}
            {blockers.length > 0 && (
              <div data-testid="trigger-blockers" role="group" aria-label="Why this standing plan cannot be enabled">
                <Banner tone="danger" title={`Cannot enable: ${blockers.length} blocker${blockers.length === 1 ? "" : "s"}`}>
                  <ul>
                    {blockers.map((b, i) => (
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
              {standing.watched.length ? (
                <ul>
                  {standing.watched.map((w) => (
                    <li key={w.input} data-input={w.input}>
                      The saved content of <strong>{w.label}</strong> (<code>{w.path}</code>)
                    </li>
                  ))}
                </ul>
              ) : (
                <p>Nothing: every run uses exactly the inputs reviewed here.</p>
              )}
              <p className="xp-wf__muted">
                Anything else suspends the trigger until you review it again: a new revision of the workflow, folder trust, the
                container image identity, a runner becoming unavailable, any code input, or any step's code or instructions.
              </p>
              <dl className="xp-wf-facts" data-testid="trigger-standing-settings">
                <dt>Fires</dt>
                <dd>{describeKind(standing.settings, definition)}</dd>
                <dt>Limits</dt>
                <dd>{describeLimits(standing.settings)}</dd>
                <dt>Native steps</dt>
                <dd data-testid="trigger-standing-native" data-allowed={String(standing.settings.allowNative)}>
                  {nativeSteps === 0
                    ? "None in this plan."
                    : standing.settings.allowNative
                      ? `${nativeSteps} step${nativeSteps === 1 ? " runs" : "s run"} natively with your account's privileges on every fire, without asking.`
                      : `${nativeSteps} step${nativeSteps === 1 ? "" : "s"} would run natively: not allowed by these settings.`}
                </dd>
                <dt>Model requests</dt>
                <dd>
                  {modelSteps === 0
                    ? "None."
                    : `${modelSteps} step${modelSteps === 1 ? " sends" : "s send"} material to the model provider on every fire; requests count against a daily cap.`}
                </dd>
                <dt>Standing hash</dt>
                <dd>
                  <FullHash hash={standing.standingHash} label="the standing plan" />
                </dd>
              </dl>
            </section>
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
  definition,
  currentRevision,
  onChanged,
  onReapprove,
}: {
  trigger: WorkflowTrigger;
  definition: WorkflowDefinition;
  currentRevision: number;
  onChanged: () => void;
  onReapprove: (trigger: WorkflowTrigger) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const view = TRIGGER_STATE_VIEW[trigger.state.kind];
  const disable = async () => {
    setBusy(true);
    setError(null);
    try {
      await workflowTriggerCommands.disable(trigger.id);
      onChanged();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const history = [...trigger.history].sort((a, b) => b.atMs - a.atMs).slice(0, 20);
  return (
    <li className="xp-wf__card" data-testid={`workflow-trigger-${trigger.id}`} data-state={trigger.state.kind} data-queued={String(trigger.queued)}>
      <div className="xp-wf__row">
        <div className="xp-wf__grow">
          <strong>{describeKind(trigger, definition)}</strong>
          <p className="xp-wf__muted">
            Revision {trigger.revision}
            {trigger.revision !== currentRevision ? ` (the workflow is now at revision ${currentRevision})` : ""} · {describeLimits(trigger)}
          </p>
          <p className="xp-wf__muted" data-testid={`trigger-last-fired-${trigger.id}`}>
            {trigger.lastFiredMs !== null ? `Last fired ${formatTime(trigger.lastFiredMs)}` : "Has not fired yet"} · enabled {formatTime(trigger.createdAtMs)}
          </p>
        </div>
        {trigger.queued && (
          <Badge tone="info" dashed>
            1 run queued
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
            title="Suspended: the approved plan no longer matches"
            actions={
              <Button size="sm" icon={RefreshCw} onPress={() => onReapprove(trigger)} data-testid={`trigger-reapprove-${trigger.id}`}>
                Review again…
              </Button>
            }
          >
            {trigger.state.message} It does not fire until you review and approve a new standing plan.
          </Banner>
        </div>
      )}
      {error && (
        <Banner tone="danger" title="Not changed" live>
          {error}
        </Banner>
      )}
      {history.length > 0 && (
        <Section title={`Recent history (${history.length})`} collapsible defaultExpanded={false}>
          <ul className="xp-wf-history" data-testid={`trigger-history-${trigger.id}`}>
            {history.map((h, i) => (
              <li key={i} data-event={h.event}>
                <span className="xp-wf__muted">{formatTime(h.atMs)}</span> {TRIGGER_EVENT_LABELS[h.event] ?? h.event}
                {h.message ? <span className="xp-wf__muted"> — {h.message}</span> : null}
                {h.run && (
                  <Button size="sm" variant="ghost" onPress={() => openRun({ id: h.run as string, title: "Workflow" })}>
                    Open run
                  </Button>
                )}
              </li>
            ))}
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
  const [dialog, setDialog] = useState<{ open: boolean; initial?: TriggerSettings }>({ open: false });
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
      initial: { kind: t.kind, debounceSeconds: t.debounceSeconds, maxRunsPerHour: t.maxRunsPerHour, maxRunsPerDay: t.maxRunsPerDay, allowNative: t.allowNative },
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
            <TriggerCard key={t.id} trigger={t} definition={definition} currentRevision={revision} onChanged={() => void refresh()} onReapprove={reapprove} />
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
          New trigger…
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
        isOpen={dialog.open}
        onOpenChange={(open) => setDialog((d) => ({ ...d, open }))}
        onEnabled={() => void refresh()}
      />
    </section>
  );
}
