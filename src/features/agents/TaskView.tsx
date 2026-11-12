import { ArrowLeft, Check, FileDiff, PanelTop, Square, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { ApprovalRequest } from "../../bindings/ApprovalRequest";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskStatus } from "../../bindings/TaskStatus";
import type { TaskSummary } from "../../bindings/TaskSummary";
import type { TranscriptItem } from "../../bindings/TranscriptItem";
import { commands } from "../../lib/ipc";
import { Banner, Button, IconButton, LoadingState, ProgressBar, Section, StatusPill } from "../../ui/components";
import {
  activeToolLabel,
  budgetRows,
  CONSENT_LABELS,
  EXECUTION_LABELS,
  isActive,
  PERMISSION_LABELS,
  statusLabel,
  toolStatusLabel,
  toolSummary,
} from "./format";
import { freshnessLabel, graphSummary } from "./GraphViews";
import { agents, useAgents, type StreamDraft } from "./store";
import { TextArea } from "./TaskComposer";

function BudgetMeter({ task }: { task: TaskSummary }) {
  return (
    <Section title="Budget" collapsible defaultExpanded>
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
        <Banner tone="warning" title={statusLabel(status).label}>
          {status.detail} Continue the task to give it more room.
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

/** Size of an approval payload, so the user knows how much there is to review. */
export function payloadSize(text: string): string {
  const lines = text.split("\n").length;
  const bytes = new TextEncoder().encode(text).length;
  return `${lines.toLocaleString()} ${lines === 1 ? "line" : "lines"} · ${bytes.toLocaleString()} ${bytes === 1 ? "byte" : "bytes"}`;
}

/**
 * Whether an element's content overflows it (it scrolls). Re-measured when
 * `content` changes and when the element is resized.
 */
function useOverflows(content: string) {
  const ref = useRef<HTMLPreElement>(null);
  const [overflows, setOverflows] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setOverflows(el.scrollHeight > el.clientHeight || el.scrollWidth > el.clientWidth);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [content]);
  return [ref, overflows] as const;
}

function Approval({ approval }: { approval: ApprovalRequest }) {
  const [busy, setBusy] = useState(false);
  // A payload that scrolls is a tab stop, so it can be read with the keyboard;
  // a short one is not, and Tab goes straight to Approve.
  const [payloadRef, scrolls] = useOverflows(approval.detail);
  async function resolve(approved: boolean) {
    setBusy(true);
    await agents.run(() => commands.agentsApprovalResolve(approval.taskId, approval.id, approved));
    setBusy(false);
  }
  return (
    <section className="agents-approval agents-stack" aria-label={`Approval needed: ${approval.title}`} data-testid="agents-approval">
      <Banner tone="warning" title={approval.title}>
        The agent is waiting for your decision. Nothing is applied until you approve. Below is the complete content of
        what will run or change; scroll to review all of it.
      </Banner>
      <p className="agents-small agents-muted" data-testid="agents-approval-size">
        {payloadSize(approval.detail)} · complete, nothing omitted
      </p>
      <pre
        ref={payloadRef}
        className="agents-pre agents-approval__payload"
        tabIndex={scrolls ? 0 : undefined}
        role="region"
        aria-label={`Complete content to approve: ${approval.title}`}
        data-testid="agents-approval-payload"
      >
        {approval.detail}
      </pre>
      <div className="agents-row">
        <Button size="sm" variant="primary" icon={Check} isDisabled={busy} onPress={() => void resolve(true)} data-testid="agents-approve">
          Approve
        </Button>
        <Button size="sm" variant="secondary" icon={X} isDisabled={busy} onPress={() => void resolve(false)}>
          Decline
        </Button>
      </div>
    </section>
  );
}

function TranscriptEntry({ item }: { item: TranscriptItem }) {
  switch (item.kind) {
    case "userText":
      return (
        <div className="agents-msg" data-role="user">
          <span className="agents-msg__role">You</span>
          <p className="agents-msg__text">{item.text}</p>
        </div>
      );
    case "assistantText":
      return (
        <div className="agents-msg" data-role="assistant" data-testid="agents-assistant-text">
          <span className="agents-msg__role">Agent</span>
          <p className="agents-msg__text">{item.text}</p>
        </div>
      );
    case "thinking":
      return (
        <details className="agents-disclosure">
          <summary>Reasoning summary</summary>
          <p className="agents-msg__text agents-muted">{item.text}</p>
        </details>
      );
    case "toolCall": {
      const pill = toolStatusLabel(item.status, item.result);
      return (
        <details className="agents-disclosure agents-tool" data-testid="agents-tool-call" data-tool={item.name}>
          <summary>
            <code className="agents-code">{item.name}</code>
            <span className="agents-muted agents-tool__summary">{toolSummary(item.name, item.input)}</span>
            <StatusPill status={pill.status} label={pill.label} size="sm" />
          </summary>
          <pre className="agents-pre">{JSON.stringify(item.input, null, 2)}</pre>
          {item.result && <pre className="agents-pre">{item.result}</pre>}
        </details>
      );
    }
    case "notice":
      return (
        <p className="agents-notice" data-level={item.level}>
          {item.message}
        </p>
      );
  }
}

function Draft({ draft }: { draft: StreamDraft }) {
  return (
    <div className="agents-msg" data-role="assistant" data-testid="agents-draft">
      <span className="agents-msg__role">Agent</span>
      {draft.thinking && (
        <details className="agents-disclosure">
          <summary>Reasoning summary (streaming)</summary>
          <p className="agents-msg__text agents-muted">{draft.thinking}</p>
        </details>
      )}
      {draft.text && <p className="agents-msg__text">{draft.text}</p>}
      {draft.activeTool && <StatusPill {...activeToolLabel(draft.activeTool, draft.activeToolStatus ?? "running")} size="sm" />}
      {!draft.text && !draft.thinking && !draft.activeTool && <StatusPill status="running" label="Thinking" size="sm" />}
    </div>
  );
}

function ContinueForm({ id }: { id: TaskId }) {
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  async function submit() {
    if (!prompt.trim()) return;
    setBusy(true);
    const task = await agents.run(() => commands.agentsTaskContinue(id, prompt, null));
    setBusy(false);
    if (task) setPrompt("");
  }
  return (
    <div className="agents-stack">
      <TextArea label="Follow-up" rows={2} value={prompt} onChange={setPrompt} />
      <div className="agents-row">
        <Button isDisabled={!prompt.trim()} isPending={busy} onPress={() => void submit()}>
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
    <div className="agents-row agents-small" data-testid="agents-graph-summary" data-revision={context.revision}>
      <StatusPill status={fresh.status} label={fresh.label} size="sm" />
      <span className="agents-muted">{graphSummary(context)}</span>
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

  useEffect(() => {
    const log = logRef.current;
    if (log) log.scrollTop = log.scrollHeight;
  }, [detail?.transcript.length, draft?.text, draft?.thinking]);

  const task = detail?.summary ?? summary;
  if (!task) return <LoadingState title="Loading task…" />;
  const pill = statusLabel(task.status);
  const active = isActive(task.status);
  const provider =
    task.spec.provider.provider === "scripted"
      ? "Scripted provider — development only"
      : `${task.spec.provider.model} (${task.spec.provider.effort} effort)`;
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
          onPress={() => (variant === "page" ? agents.setPage({ tab: "changes" }) : agents.show("review", id))}
          data-testid="agents-open-review"
        >
          Review changes{task.pendingReview > 0 ? ` (${task.pendingReview})` : ""}
        </Button>
      </div>
      <p className="agents-muted agents-small">
        {PERMISSION_LABELS[task.spec.permission]} · {EXECUTION_LABELS[task.spec.execution]} · {CONSENT_LABELS[task.spec.consent]} · {provider}
        {servedBy}
      </p>
      <GraphSummaryLine id={id} variant={variant} />
      {!active && <OutcomeBanner status={task.status} />}
      {detail?.approvals.map((approval) => <Approval key={approval.id} approval={approval} />)}
      <div className="agents-transcript" role="log" aria-live="polite" aria-label="Transcript" ref={logRef} data-testid="agents-transcript">
        {detail?.transcript.map((item, index) => <TranscriptEntry key={`${item.seq}-${index}`} item={item} />)}
        {active && draft && <Draft draft={draft} />}
      </div>
      <BudgetMeter task={task} />
      {!active && <ContinueForm id={id} />}
    </article>
  );
}
