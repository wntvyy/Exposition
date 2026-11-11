import { useEffect } from "react";

import { ProviderSettingsDialog } from "./ProviderSettingsDialog";
import { ReviewView } from "./ReviewView";
import { agents, useAgents } from "./store";
import { TaskComposer } from "./TaskComposer";
import { TaskList } from "./TaskList";
import { TaskView } from "./TaskView";
import "./agents.css";

function ProviderChip() {
  const status = useAgents((s) => s.status);
  if (!status) return null;
  const scripted = status.settings.provider === "scripted";
  const label = scripted
    ? "Scripted provider — development only"
    : status.configured
      ? `Claude · ${status.settings.model}`
      : "Provider not configured";
  const tone = scripted ? "warning" : status.configured ? "neutral" : "warning";
  return (
    <span className={`agents-chip agents-tone-${tone}`} data-testid="agents-provider-chip">
      {label}
    </span>
  );
}

/** The agent panel (workbench auxiliary view). */
export function AgentPanel() {
  const view = useAgents((s) => s.view);
  const selected = useAgents((s) => s.selected);
  const error = useAgents((s) => s.error);

  useEffect(() => {
    void agents.init();
  }, []);

  return (
    <section className="agents" aria-label="Agent" data-testid="agents-panel">
      <header className="agents-header">
        <h2 className="agents-title">Agent</h2>
        <ProviderChip />
        <div className="agents-header-actions">
          <button type="button" className="agents-button" onClick={() => agents.show("tasks", null)}>
            Tasks
          </button>
          <button
            type="button"
            className="agents-button agents-button-primary"
            onClick={() => agents.show("compose")}
            data-testid="agents-new-task"
          >
            New task
          </button>
          <button
            type="button"
            className="agents-button"
            onClick={() => agents.openSettings(true)}
            data-testid="agents-open-settings"
          >
            Provider settings
          </button>
        </div>
      </header>
      {error && (
        <p className="agents-error" role="alert" data-testid="agents-error">
          {error}
        </p>
      )}
      <div className="agents-body">
        {view === "tasks" && <TaskList />}
        {view === "compose" && <TaskComposer />}
        {view === "task" && selected && <TaskView id={selected} />}
        {view === "review" && selected && <ReviewView id={selected} />}
      </div>
      <ProviderSettingsDialog />
    </section>
  );
}
