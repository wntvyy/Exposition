import { ListTodo, Plus } from "lucide-react";
import { useEffect } from "react";

import { Banner, Button, Toolbar } from "../../ui/components";
import { providerLabel } from "./format";
import { ReviewView } from "./ReviewView";
import { agents, useAgents } from "./store";
import { TaskComposer } from "./TaskComposer";
import { TaskList } from "./TaskList";
import { TaskView } from "./TaskView";
import "./agents.css";

/** The provider actually selected, as a quiet status line (warning text when it cannot run tasks). */
function ProviderLabel() {
  const status = useAgents((s) => s.status);
  if (!status) return null;
  const scripted = status.settings.provider === "scripted";
  const { text, ready } = providerLabel(status);
  return (
    <span
      className="ui-meta agents-provider"
      data-scripted={scripted || undefined}
      data-ready={ready}
      data-testid="agents-provider-chip"
    >
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
        </Toolbar>
        <ProviderLabel />
      </div>
      {error && (
        <div className="agents-error" data-testid="agents-error">
          <Banner tone="danger" title="Agent action failed" live actions={<Button size="sm" variant="ghost" onPress={() => agents.setError(null)}>Dismiss</Button>}>
            {error}
          </Banner>
        </div>
      )}
      <div className="agents-content" data-view={view}>
        {view === "tasks" && <TaskList />}
        {view === "compose" && <TaskComposer />}
        {view === "task" && selected && <TaskView id={selected} />}
        {view === "review" && selected && <ReviewView id={selected} />}
      </div>
    </div>
  );
}
