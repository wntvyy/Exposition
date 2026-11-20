// One task (design C): in the Agent view a compact header (back to the task
// list, title, the one StatusPill, the task's actions in a menu); the
// conversation as a turn outline that follows new output while the reader is
// at its end; "Needs you" and the composer anchored at the bottom. The
// Agents page renders the same conversation and composer under its own
// header. Both read the same store: one task, one approval, one draft.
import { ArrowDown, ArrowLeft, ExternalLink, FileDiff, MoreHorizontal, Network, Plus, Square, Trash2 } from "lucide-react";
import { useEffect, useRef, useState, type Key } from "react";

import type { TaskId } from "../../bindings/TaskId";
import type { TaskStatus } from "../../bindings/TaskStatus";
import type { TaskSummary } from "../../bindings/TaskSummary";
import { commands } from "../../lib/ipc";
import { Banner, Button, IconButton, LoadingState, Menu, MenuItem, MenuSeparator, MenuTrigger, StatusPill } from "../../ui/components";
import { asSentence, isActive, statusLabel } from "./format";
import { jumpTo, NeedsYou } from "./NeedsYou";
import { pendingQuestions } from "./questions";
import { agents, useAgents } from "./store";
import { Composer } from "./TaskComposer";
import { taskActions } from "./taskOps";
import { unsavedCount, useTaskReview } from "./taskReview";
import { useFollowBottom } from "./Transcript";
import { TurnOutline } from "./TurnOutline";
import { withoutRepeatedFailure } from "./turns";

export { payloadSize } from "./ApprovalCard";

/** Why a finished task stopped, as an inline banner at the end of the conversation. */
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

/**
 * The task's actions (Agent view and Agents page): the panel's menu also
 * leads to the task's review, the Agents page, and its graph context; the
 * page shows those as header actions instead.
 */
export function TaskMenu({ task, where }: { task: TaskSummary; where: "panel" | "page" }) {
  const hasGraph = useAgents((s) => s.details[task.id]?.graph?.context != null);
  const active = isActive(task.status);
  const run = (key: Key) => {
    switch (key) {
      case "new":
        agents.show("compose");
        return;
      case "cancel":
        void agents.run(() => commands.agentsTaskCancel(task.id));
        return;
      case "review":
        agents.show("review", task.id);
        return;
      case "page":
        agents.showInPage(task.id);
        return;
      case "graph":
        agents.showInPage(task.id, "graph");
        return;
      case "delete":
        void taskActions([task]).actions.delete?.run();
        return;
    }
  };
  return (
    <MenuTrigger>
      <IconButton icon={MoreHorizontal} size="sm" aria-label="Task actions" data-testid="agents-task-menu" />
      <Menu placement="bottom end" onAction={run} aria-label="Task actions">
        {where === "panel" ? (
          <MenuItem id="new" icon={Plus}>
            New task
          </MenuItem>
        ) : null}
        {active ? (
          <MenuItem id="cancel" icon={Square}>
            Cancel task
          </MenuItem>
        ) : null}
        {where === "panel" ? (
          <MenuItem id="review" icon={FileDiff} isDisabled={task.changedFiles === 0}>
            Review changes
          </MenuItem>
        ) : null}
        {where === "panel" ? (
          <MenuItem id="page" icon={ExternalLink}>
            Open in Agents page
          </MenuItem>
        ) : null}
        {where === "panel" && hasGraph ? (
          <MenuItem id="graph" icon={Network}>
            Show graph context
          </MenuItem>
        ) : null}
        <MenuSeparator />
        <MenuItem id="delete" icon={Trash2} description="The record only; files it changed stay">
          Delete Task Record
        </MenuItem>
      </Menu>
    </MenuTrigger>
  );
}

/**
 * The conversation and composer of one task. `panel` adds the Agent view's
 * header; on the page the header, Changes, Graph context, and Proposals
 * belong to the page (`onReview` opens its Changes panel).
 */
export function TaskView({
  id,
  variant = "panel",
  boundary,
  onReview,
  reviewShown = false,
}: {
  id: TaskId;
  variant?: "panel" | "page";
  /** Element the Options sheet stays inside (default: this view). */
  boundary?: Element | null;
  onReview?: () => void;
  /** The review is already open beside the conversation (page). */
  reviewShown?: boolean;
}) {
  const detail = useAgents((s) => s.details[id]);
  const summary = useAgents((s) => s.tasks.find((t) => t.id === id));
  const draft = useAgents((s) => s.drafts[id]);
  const logRef = useRef<HTMLDivElement>(null);
  const [self, setSelf] = useState<HTMLElement | null>(null);

  useEffect(() => {
    void agents.refetch(id);
  }, [id]);

  const task = detail?.summary ?? summary;
  const reviews = useTaskReview(task ? id : null, task?.changedFiles ?? 0);
  const transcript = detail?.transcript ?? [];
  const approvals = detail?.approvals ?? [];
  const active = task ? isActive(task.status) : false;
  // Follow new output only while the reader is at the end of the transcript (A1 lane R).
  const activity = `${transcript.length}|${approvals.length}|${draft?.text.length ?? 0}|${draft?.thinking.length ?? 0}|${draft?.activeTool ?? ""}`;
  const { newActivity, toEnd } = useFollowBottom(logRef, task ? id : null, activity);
  if (!task) return <LoadingState title="Loading task…" />;
  const pill = statusLabel(task.status);
  const review =
    !active && task.pendingReview > 0 && !reviewShown ? { files: task.pendingReview, unsaved: unsavedCount(reviews, true) } : null;

  return (
    <article ref={setSelf} className="agents-task" aria-label={task.title} data-testid="agents-task-view" data-variant={variant} data-agent-chat>
      {variant === "panel" && (
        <header className="agents-task__header">
          <IconButton icon={ArrowLeft} size="sm" aria-label="Back to tasks" onPress={() => agents.show("tasks", null)} data-testid="agents-show-tasks" />
          <h2 className="agents-task__title" title={task.title}>
            {task.title}
          </h2>
          <span className="agents-task__status" data-testid="agents-task-status" data-status={task.status.kind}>
            <StatusPill status={pill.status} label={pill.label} size="sm" />
          </span>
          <TaskMenu task={task} where="panel" />
        </header>
      )}
      <div className="agents-task__scroll" role="log" aria-live="polite" aria-label="Transcript" ref={logRef} data-testid="agents-transcript">
        <div className="agents-column">
          <TurnOutline
            taskId={id}
            transcript={withoutRepeatedFailure(transcript, task.status)}
            approvals={approvals} draft={active ? (draft ?? null) : null} reviews={reviews} />
          {!active && <OutcomeBanner status={task.status} />}
        </div>
      </div>
      <div className="agents-task__foot">
        {newActivity && (
          <div className="agents-task__new-activity">
            <Button size="sm" variant="secondary" icon={ArrowDown} onPress={toEnd} data-testid="agents-new-activity">
              New activity
            </Button>
          </div>
        )}
        <div className="agents-column">
          <Composer
            key={id}
            task={task}
            boundary={boundary ?? self}
            above={
              <NeedsYou
                approvals={approvals}
                questions={pendingQuestions(detail)}
                review={review}
                onJump={(anchor) => jumpTo(logRef.current, anchor)}
                onReview={onReview ?? (() => agents.show("review", id))}
              />
            }
          />
        </div>
      </div>
    </article>
  );
}
