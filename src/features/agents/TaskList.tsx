import { Bot, Plus } from "lucide-react";

import { Button, EmptyState, StatusPill } from "../../ui/components";
import { useOpList } from "../operations/useOpList";
import { relativeTime, statusLabel } from "./format";
import { agents, useAgents } from "./store";
import { taskActions } from "./taskOps";

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
          <Button variant="primary" icon={Plus} onPress={() => agents.show("compose")}>
            New task
          </Button>
        }
      />
    );
  }
  return (
    <div ref={list.ref} className="xp-ops-surface">
    <ul className="agents-task-list" aria-label="Agent tasks" data-testid="agents-task-list">
      {tasks.map((task) => {
        const pill = statusLabel(task.status);
        const op = list.itemProps(task.id);
        return (
          <li
            key={task.id}
            data-op-item={op["data-op-item"]}
            data-op-selected={op["data-op-selected"]}
            className="agents-task-row"
            onMouseDown={op.onMouseDown}
            onClick={op.onClick}
            onFocus={op.onFocus}
            onContextMenu={op.onContextMenu}
          >
            <button
              type="button"
              className="agents-task-item"
              onClick={() => agents.show("task", task.id)}
              data-testid="agents-task-item"
            >
              <span className="agents-task-item__title">{task.title}</span>
              <span className="agents-task-item__meta">
                <StatusPill status={pill.status} label={pill.label} size="sm" />
                {task.pendingReview > 0 && (
                  <StatusPill status="warning" label={`${task.pendingReview} to review`} size="sm" />
                )}
                <span className="agents-muted">{relativeTime(task.updatedAtMs)}</span>
              </span>
            </button>
            {list.moreButton(task.id, task.title)}
          </li>
        );
      })}
    </ul>
    {list.contextMenu}
    </div>
  );
}
