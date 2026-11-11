import { useEffect, useId, useRef, useState, type FormEvent } from "react";

import type { ApprovalRequest } from "../../bindings/ApprovalRequest";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskSummary } from "../../bindings/TaskSummary";
import type { TranscriptItem } from "../../bindings/TranscriptItem";
import { commands } from "../../lib/ipc";
import {
  budgetRows,
  CONSENT_LABELS,
  EXECUTION_LABELS,
  isActive,
  PERMISSION_LABELS,
  statusLabel,
  toolStatusLabel,
  toolSummary,
} from "./format";
import { agents, useAgents, type StreamDraft } from "./store";

function BudgetMeter({ task }: { task: TaskSummary }) {
  return (
    <dl className="agents-budget-meter" aria-label="Budget usage" data-testid="agents-budget-meter">
      {budgetRows(task).map((row) => (
        <div key={row.key} className="agents-budget-row">
          <dt>{row.label}</dt>
          <dd>
            <meter min={0} max={Math.max(row.limit, 1)} value={Math.min(row.used, row.limit)} aria-label={row.label} />
            <span className="agents-muted">{row.text}</span>
          </dd>
        </div>
      ))}
    </dl>
  );
}

function Approval({ approval }: { approval: ApprovalRequest }) {
  const [busy, setBusy] = useState(false);
  async function resolve(approved: boolean) {
    setBusy(true);
    await agents.run(() => commands.agentsApprovalResolve(approval.taskId, approval.id, approved));
    setBusy(false);
  }
  return (
    <section className="agents-approval" aria-label={`Approval needed: ${approval.title}`} data-testid="agents-approval">
      <h4>{approval.title}</h4>
      <pre className="agents-pre">{approval.detail}</pre>
      <div className="agents-actions">
        <button
          type="button"
          className="agents-button agents-button-primary"
          disabled={busy}
          onClick={() => void resolve(true)}
          data-testid="agents-approve"
        >
          Approve
        </button>
        <button type="button" className="agents-button" disabled={busy} onClick={() => void resolve(false)}>
          Decline
        </button>
      </div>
    </section>
  );
}

function TranscriptEntry({ item }: { item: TranscriptItem }) {
  switch (item.kind) {
    case "userText":
      return (
        <div className="agents-msg agents-msg-user">
          <span className="agents-msg-role">You</span>
          <p className="agents-text">{item.text}</p>
        </div>
      );
    case "assistantText":
      return (
        <div className="agents-msg agents-msg-assistant" data-testid="agents-assistant-text">
          <span className="agents-msg-role">Agent</span>
          <p className="agents-text">{item.text}</p>
        </div>
      );
    case "thinking":
      return (
        <details className="agents-thinking">
          <summary>Reasoning summary</summary>
          <p className="agents-text">{item.text}</p>
        </details>
      );
    case "toolCall": {
      const tone = item.status === "failed" ? "danger" : item.status === "succeeded" ? "success" : "progress";
      return (
        <details className={`agents-tool agents-tone-${tone}`} data-testid="agents-tool-call" data-tool={item.name}>
          <summary>
            <code>{item.name}</code> <span className="agents-muted">{toolSummary(item.name, item.input)}</span>{" "}
            <span className={`agents-badge agents-tone-${tone}`}>{toolStatusLabel(item.status)}</span>
          </summary>
          <pre className="agents-pre">{JSON.stringify(item.input, null, 2)}</pre>
          {item.result && <pre className="agents-pre agents-result">{item.result}</pre>}
        </details>
      );
    }
    case "notice":
      return (
        <p className={`agents-notice agents-notice-${item.level}`} role={item.level === "error" ? "alert" : undefined}>
          {item.message}
        </p>
      );
  }
}

function Draft({ draft }: { draft: StreamDraft }) {
  return (
    <div className="agents-msg agents-msg-assistant agents-draft" data-testid="agents-draft">
      <span className="agents-msg-role">Agent</span>
      {draft.thinking && (
        <details className="agents-thinking">
          <summary>Reasoning summary (streaming)</summary>
          <p className="agents-text">{draft.thinking}</p>
        </details>
      )}
      {draft.text && <p className="agents-text">{draft.text}</p>}
      {draft.activeTool && <p className="agents-muted">Running {draft.activeTool}…</p>}
      {!draft.text && !draft.thinking && !draft.activeTool && <p className="agents-muted">Working…</p>}
    </div>
  );
}

function ContinueForm({ id }: { id: TaskId }) {
  const fieldId = useId();
  const [prompt, setPrompt] = useState("");
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!prompt.trim()) return;
    const task = await agents.run(() => commands.agentsTaskContinue(id, prompt, null));
    if (task) setPrompt("");
  }
  return (
    <form className="agents-continue" onSubmit={submit}>
      <label htmlFor={fieldId}>Follow-up</label>
      <textarea id={fieldId} rows={2} value={prompt} onChange={(e) => setPrompt(e.target.value)} />
      <button type="submit" className="agents-button" disabled={!prompt.trim()}>
        Continue task
      </button>
    </form>
  );
}

export function TaskView({ id }: { id: TaskId }) {
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
  if (!task) return <p className="agents-muted">Loading task…</p>;
  const status = statusLabel(task.status);
  const active = isActive(task.status);

  return (
    <article className="agents-task" aria-label={task.title} data-testid="agents-task-view">
      <header className="agents-task-header">
        <h3 className="agents-task-heading">{task.title}</h3>
        <span className={`agents-badge agents-tone-${status.tone}`} data-testid="agents-task-status">
          {status.label}
        </span>
        <div className="agents-header-actions">
          {active && (
            <button
              type="button"
              className="agents-button agents-button-danger"
              onClick={() => void agents.run(() => commands.agentsTaskCancel(id))}
              data-testid="agents-cancel"
            >
              Cancel
            </button>
          )}
          <button
            type="button"
            className="agents-button"
            disabled={task.changedFiles === 0}
            onClick={() => agents.show("review", id)}
            data-testid="agents-open-review"
          >
            Review changes{task.pendingReview > 0 ? ` (${task.pendingReview})` : ""}
          </button>
        </div>
      </header>
      <p className="agents-muted agents-task-spec">
        {PERMISSION_LABELS[task.spec.permission]} · {EXECUTION_LABELS[task.spec.execution]} ·{" "}
        {CONSENT_LABELS[task.spec.consent]} · {task.spec.provider.provider === "scripted" ? "Scripted provider — development only" : `${task.spec.provider.model} (${task.spec.provider.effort})`}
        {task.usage.servedBy && task.usage.servedBy !== task.spec.provider.model && task.spec.provider.provider !== "scripted"
          ? ` · served by ${task.usage.servedBy}`
          : ""}
      </p>
      <BudgetMeter task={task} />
      {detail?.approvals.map((approval) => <Approval key={approval.id} approval={approval} />)}
      <div className="agents-transcript" role="log" aria-live="polite" aria-label="Transcript" ref={logRef} data-testid="agents-transcript">
        {detail?.transcript.map((item, index) => <TranscriptEntry key={`${item.seq}-${index}`} item={item} />)}
        {active && draft && <Draft draft={draft} />}
      </div>
      {!active && <ContinueForm id={id} />}
    </article>
  );
}
