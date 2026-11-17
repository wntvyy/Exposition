import { ArrowLeft, FileDiff, PanelTop, Square } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { ProviderChoice } from "../../bindings/ProviderChoice";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskStatus } from "../../bindings/TaskStatus";
import type { TaskSummary } from "../../bindings/TaskSummary";
import { commands, errorMessage } from "../../lib/ipc";
import { Banner, Button, IconButton, LoadingState, ProgressBar, Section, StatusPill, StatusText, TextArea } from "../../ui/components";
import { ApprovalCard } from "./ApprovalCard";
import { DraftAttachments, pasteHandler } from "./Attachments";
import { drafts, useDraft } from "./drafts";
import { ModelChoice } from "./ModelPicker";
import { asSentence, budgetRows, EFFORT_LABELS, isActive, specLabels, statusLabel } from "./format";
import { freshnessLabel, graphSummary } from "./GraphViews";
import { TaskAllowances } from "./permissions";
import { agents, useAgents } from "./store";
import { Draft, TranscriptEntry, useFollowBottom } from "./Transcript";

export { payloadSize } from "./ApprovalCard";

function BudgetMeter({ task }: { task: TaskSummary }) {
  return (
    <Section title="Budget" collapsible defaultExpanded className="agents-section">
      <div className="agents-grid agents-grid--wide" data-testid="agents-budget-meter">
        {budgetRows(task).map((row) => (
          <ProgressBar
            key={row.key}
            label={row.label}
            value={row.limit > 0 ? Math.min(100, (row.used / row.limit) * 100) : 0}
            valueLabel={row.text}
            tone={row.limit > 0 && row.used >= row.limit ? "danger" : "accent"}
          />
        ))}
      </div>
    </Section>
  );
}

/** Why a finished task stopped, as an inline banner. */
function OutcomeBanner({ status }: { status: TaskStatus }) {
  switch (status.kind) {
    case "canceled":
      return <Banner tone="canceled" title="Canceled">Changes made before canceling remain reviewable and revertible.</Banner>;
    case "interrupted":
      return <Banner tone="warning" title="Interrupted">Exposition closed while this task was running. Continue it to resume.</Banner>;
    case "budgetExceeded":
      return (
        // A follow-up from this view sends no new budgets, and usage counts across runs.
        <Banner tone="warning" title={statusLabel(status).label}>
          {asSentence(status.detail)} A follow-up keeps the same budgets and the usage so far.
        </Banner>
      );
    case "refused":
      return (
        <Banner tone="danger" title="The model provider declined this request">
          {status.category ? `Category: ${status.category}. ` : ""}
          {status.explanation ?? ""} Nothing from the declined response was applied.
        </Banner>
      );
    case "failed":
      return (
        <Banner tone="danger" title="Task failed">
          {status.message}
        </Banner>
      );
    case "stopped":
      return (
        <Banner tone="warning" title={statusLabel(status).label}>
          {status.message}
        </Banner>
      );
    default:
      return null;
  }
}

const sameChoice = (a: ProviderChoice, b: ProviderChoice) => a.provider === b.provider && a.model === b.model && a.effort === b.effort;

/**
 * The follow-up of a stopped task: text, attachments, and optionally another
 * model for this and later turns. Its draft belongs to this task only. A
 * model switch the conversation cannot carry is refused by the backend with
 * its explanation, shown here; the draft is kept.
 */
function ContinueForm({ id, current }: { id: TaskId; current: ProviderChoice }) {
  const status = useAgents((s) => s.status);
  const draft = useDraft(id);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [modelProblem, setModelProblem] = useState<string | null>(null);
  const hasContent = draft.text.trim().length > 0 || draft.attachments.length > 0;
  const canSend = hasContent && !busy && draft.ingesting === 0 && (draft.model === null || modelProblem === null);
  async function submit() {
    if (!canSend) return;
    const sent = { text: draft.text, attachments: draft.attachments.map((a) => a.id), model: draft.model };
    const switchTo = sent.model && !sameChoice(sent.model, current) ? sent.model : null;
    setBusy(true);
    setFailure(null);
    try {
      await commands.agentsTaskContinue(id, sent.text, null, sent.attachments, switchTo);
    } catch (e) {
      setFailure(errorMessage(e));
      return;
    } finally {
      setBusy(false);
    }
    drafts.completeSend(id, sent);
  }
  return (
    <div className="agents-stack" data-testid="agents-follow-up">
      <div onPaste={pasteHandler(id)}>
        <TextArea label="Follow-up" rows={2} value={draft.text} onChange={(text) => drafts.setText(id, text)} testId="agents-follow-up-text" />
      </div>
      <DraftAttachments draftKey={id} />
      <ModelChoice
        value={draft.model}
        inherited={current}
        inheritedLabel={null}
        chooseLabel="Change model…"
        resetLabel="Keep current model"
        onChange={(model) => drafts.setModel(id, model)}
        status={status}
        onProblemChange={setModelProblem}
        onConfigure={() => agents.openSettings(true)}
        testId="agents-follow-up-model"
      />
      {failure && (
        <Banner tone="danger" title="The follow-up was not sent" live data-testid="agents-follow-up-error">
          {failure}
        </Banner>
      )}
      <div className="agents-row">
        <Button isDisabled={!canSend} isPending={busy} onPress={() => void submit()} data-testid="agents-continue">
          Continue task
        </Button>
      </div>
    </div>
  );
}

/**
 * The graph context line: what the agent was given, and a way to the full
 * view on the Agents page (from the panel) or to the graph tab (on the page).
 */
function GraphSummaryLine({ id, variant }: { id: TaskId; variant: "panel" | "page" }) {
  const context = useAgents((s) => s.details[id]?.graph?.context ?? null);
  if (!context) return null;
  const fresh = freshnessLabel(context.freshness);
  return (
    <div className="agents-row ui-meta" data-testid="agents-graph-summary" data-revision={context.revision}>
      <StatusText status={fresh.status} label={fresh.label} />
      <span>{graphSummary(context)}</span>
      <Button
        size="sm"
        variant="ghost"
        icon={PanelTop}
        onPress={() => agents.showInPage(id, "graph")}
        data-testid={variant === "panel" ? "agents-open-page" : "agents-show-graph"}
      >
        {variant === "panel" ? "Open in Agents page" : "Show graph context"}
      </Button>
    </div>
  );
}

/**
 * One task: status, approvals, transcript, budgets, follow-up. The Agent view
 * (`panel`) and the Agents page (`page`) render the same task from the same
 * store; on the page the heading and review navigation belong to the page.
 */
export function TaskView({ id, variant = "panel" }: { id: TaskId; variant?: "panel" | "page" }) {
  const detail = useAgents((s) => s.details[id]);
  const summary = useAgents((s) => s.tasks.find((t) => t.id === id));
  const draft = useAgents((s) => s.drafts[id]);
  const logRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    void agents.refetch(id);
  }, [id]);

  const task = detail?.summary ?? summary;
  // Follow new output only while the reader is at the end of the transcript (A1 lane R).
  useFollowBottom(logRef, task ? id : null);
  if (!task) return <LoadingState title="Loading task…" />;
  const pill = statusLabel(task.status);
  const active = isActive(task.status);
  const provider =
    task.spec.provider.provider === "scripted"
      ? "Scripted provider — development only"
      : `${task.spec.provider.model} (${EFFORT_LABELS[task.spec.provider.effort].toLowerCase()} effort)`;
  const servedBy =
    task.spec.provider.provider !== "scripted" && task.usage.servedBy && task.usage.servedBy !== task.spec.provider.model
      ? ` · served by ${task.usage.servedBy}`
      : "";

  return (
    <article className="agents-stack" aria-label={task.title} data-testid="agents-task-view" data-variant={variant}>
      {variant === "panel" && (
        <div className="agents-heading">
          <IconButton icon={ArrowLeft} size="sm" aria-label="Back to tasks" onPress={() => agents.show("tasks", null)} />
          <h3 className="agents-heading__title">{task.title}</h3>
        </div>
      )}
      {variant === "panel" && (
        <div className="agents-row">
          <span data-testid="agents-task-status" data-status={task.status.kind}>
            <StatusPill status={pill.status} label={pill.label} />
          </span>
          {active && (
            <Button size="sm" variant="secondary" icon={Square} onPress={() => void agents.run(() => commands.agentsTaskCancel(id))} data-testid="agents-cancel">
              Cancel
            </Button>
          )}
          <Button
            size="sm"
            icon={FileDiff}
            isDisabled={task.changedFiles === 0}
            onPress={() => agents.show("review", id)}
            data-testid="agents-open-review"
          >
            Review changes{task.pendingReview > 0 ? ` (${task.pendingReview})` : ""}
          </Button>
        </div>
      )}
      <p className="ui-meta" data-testid="agents-task-spec">
        {[...specLabels(task.spec), provider].join(" · ")}
        {servedBy}
      </p>
      <TaskAllowances id={id} />
      <GraphSummaryLine id={id} variant={variant} />
      {!active && <OutcomeBanner status={task.status} />}
      {detail?.approvals.map((approval) => <ApprovalCard key={approval.id} approval={approval} />)}
      <div className="agents-transcript" role="log" aria-live="polite" aria-label="Transcript" ref={logRef} data-testid="agents-transcript">
        {detail?.transcript.map((item, index) => <TranscriptEntry key={`${item.seq}-${index}`} item={item} />)}
        {active && draft && <Draft draft={draft} />}
      </div>
      <BudgetMeter task={task} />
      {!active && <ContinueForm key={id} id={id} current={task.spec.provider} />}
    </article>
  );
}
