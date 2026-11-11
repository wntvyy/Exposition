import { ListTodo, Plus, Settings } from "lucide-react";
import { useEffect } from "react";

import { Banner, Button, IconButton, Toolbar } from "../../ui/components";
import { ProviderSettingsDialog } from "./ProviderSettingsDialog";
import { ReviewView } from "./ReviewView";
import { agents, useAgents } from "./store";
import { TaskComposer } from "./TaskComposer";
import { TaskList } from "./TaskList";
import { TaskView } from "./TaskView";
import "./agents.css";

function ProviderLabel() {
  const status = useAgents((s) => s.status);
  if (!status) return null;
  const scripted = status.settings.provider === "scripted";
  const text = scripted
    ? "Scripted provider — development only"
    : status.configured
      ? `Claude · ${status.settings.model}`
      : "Provider not configured";
  return (
    <span className="agents-provider" data-scripted={scripted || undefined} data-testid="agents-provider-chip">
      {text}
    </span>
  );
}

/** Body of the Agent view (the shell provides the tab, header, and scrolling region). */
export function AgentPanel() {
  const view = useAgents((s) => s.view);
  const selected = useAgents((s) => s.selected);
  const error = useAgents((s) => s.error);

  useEffect(() => {
    void agents.init();
  }, []);

  return (
    <div className="agents" data-testid="agents-panel">
      <div className="agents-bar">
        <Toolbar aria-label="Agent actions" className="agents-bar__actions">
          <Button
            variant="primary"
            size="sm"
            icon={Plus}
            onPress={() => agents.show("compose")}
            data-testid="agents-new-task"
          >
            New task
          </Button>
          <Button variant="ghost" size="sm" icon={ListTodo} onPress={() => agents.show("tasks", null)} data-testid="agents-show-tasks">
            Tasks
          </Button>
          <IconButton
            icon={Settings}
            size="sm"
            aria-label="Agent provider settings"
            onPress={() => agents.openSettings(true)}
            data-testid="agents-open-settings"
          />
        </Toolbar>
        <ProviderLabel />
      </div>
      {error && (
        <div data-testid="agents-error">
          <Banner tone="danger" title="Agent action failed" live actions={<Button size="sm" variant="ghost" onPress={() => agents.setError(null)}>Dismiss</Button>}>
            {error}
          </Banner>
        </div>
      )}
      <div className="agents-content">
        {view === "tasks" && <TaskList />}
        {view === "compose" && <TaskComposer />}
        {view === "task" && selected && <TaskView id={selected} />}
        {view === "review" && selected && <ReviewView id={selected} />}
      </div>
      <ProviderSettingsDialog />
    </div>
  );
}
