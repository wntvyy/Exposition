import { Bot, Plus } from "lucide-react";
import { useId, type KeyboardEvent, type MouseEvent, type ReactNode } from "react";

import type { TaskSummary } from "../../bindings/TaskSummary";
import { Button, cx, EmptyState, ListRow, StatusText } from "../../ui/components";
import { useOpList } from "../operations/useOpList";
import { relativeTime, statusLabel } from "./format";
import { agents, useAgents } from "./store";
import { taskActions } from "./taskOps";

/** Operation attributes and handlers of one list item (`useOpList().itemProps`). */
export type TaskOpProps = ReturnType<ReturnType<typeof useOpList<TaskSummary>>["itemProps"]>;

/**
 * Whether a click belongs to a control in the row (its More actions button)
 * or did not happen in the row at all (a menu item in the row's popover:
 * React events bubble through portals).
 */
export function clickOnControl(event: MouseEvent<HTMLElement>): boolean {
  const target = event.target;
  if (!(target instanceof Node) || !event.currentTarget.contains(target)) return true;
  return target instanceof Element && target.closest("button, a, input, textarea, [role='button']") !== null;
}

/**
 * One agent task in a list (Agent view and Agents page): a flat row with the
 * title, its state and age, and a More actions button. The row is the P1
 * operation item: click opens the task and selects it, Ctrl/Cmd+click adds
 * it to the selection, Space selects, Enter opens.
 */
export function TaskRow({
  task,
  op,
  more,
  onOpen,
  selected,
  kept,
  testId,
}: {
  task: TaskSummary;
  op: TaskOpProps;
  more: ReactNode;
  onOpen: () => void;
  selected?: boolean;
  kept?: boolean;
  testId: string;
}) {
  const metaId = useId();
  const pill = statusLabel(task.status);
  return (
    <ListRow
      role="listitem"
      {...op}
      className={cx("agents-task-row", op.className)}
      selected={selected}
      current={selected}
      aria-current={selected ? "true" : undefined}
      aria-label={task.title}
      aria-describedby={metaId}
      data-testid={testId}
      data-task-id={task.id}
      onClick={(event: MouseEvent<HTMLDivElement>) => {
        op.onClick(event);
        if (!event.ctrlKey && !event.metaKey && !clickOnControl(event)) onOpen();
      }}
      onKeyDown={(event: KeyboardEvent<HTMLDivElement>) => {
        op.onKeyDown(event);
        if (event.key === "Enter" && event.target === event.currentTarget && !event.repeat) {
          event.preventDefault();
          onOpen();
        }
      }}
      title={task.title}
      meta={
        <span id={metaId} className="agents-task-row__meta">
          <StatusText status={pill.status} label={pill.label} />
          {task.pendingReview > 0 && <StatusText status="warning" label={`${task.pendingReview} to review`} />}
          {kept && <span>Kept</span>}
          <span>{relativeTime(task.updatedAtMs)}</span>
        </span>
      }
      trailing={more}
    />
  );
}

export function TaskList() {
  const tasks = useAgents((s) => s.tasks);
  const list = useOpList({ id: "agent-tasks-panel", label: "Task actions", items: tasks, keyOf: (t) => t.id, actionsFor: taskActions });
  if (tasks.length === 0) {
    return (
      <EmptyState
        icon={Bot}
        title="No agent tasks yet"
        description="Describe a change, calculation, or new document. The agent works through the same document service as your editors, and every change stays reviewable."
        actions={
          <Button size="sm" icon={Plus} onPress={() => agents.show("compose")}>
            New task
          </Button>
        }
      />
    );
  }
  return (
    <div ref={list.ref} className="xp-ops-surface">
      <div role="list" aria-label="Agent tasks" data-testid="agents-task-list">
        {tasks.map((task) => (
          <TaskRow
            key={task.id}
            task={task}
            op={list.itemProps(task.id)}
            more={list.moreButton(task.id, task.title)}
            onOpen={() => agents.show("task", task.id)}
            testId="agents-task-item"
          />
        ))}
      </div>
      {list.contextMenu}
    </div>
  );
}
