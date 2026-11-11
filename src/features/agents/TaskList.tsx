import { relativeTime, statusLabel } from "./format";
import { agents, useAgents } from "./store";

export function TaskList() {
  const tasks = useAgents((s) => s.tasks);
  if (tasks.length === 0) {
    return (
      <div className="agents-empty">
        <p>No agent tasks yet.</p>
        <button type="button" className="agents-button agents-button-primary" onClick={() => agents.show("compose")}>
          Describe a task
        </button>
      </div>
    );
  }
  return (
    <ul className="agents-task-list" aria-label="Agent tasks" data-testid="agents-task-list">
      {tasks.map((task) => {
        const status = statusLabel(task.status);
        return (
          <li key={task.id}>
            <button
              type="button"
              className="agents-task-item"
              onClick={() => agents.show("task", task.id)}
              data-testid="agents-task-item"
            >
              <span className="agents-task-title">{task.title}</span>
              <span className="agents-task-meta">
                <span className={`agents-badge agents-tone-${status.tone}`}>{status.label}</span>
                {task.pendingReview > 0 && (
                  <span className="agents-badge agents-tone-warning">
                    {task.pendingReview} to review
                  </span>
                )}
                <span className="agents-muted">{relativeTime(task.updatedAtMs)}</span>
              </span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}
