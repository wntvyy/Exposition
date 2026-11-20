import { ArrowLeft, Plus } from "lucide-react";
import { useEffect, useState } from "react";

import { Banner, Button, EmptyState, IconButton, Toolbar } from "../../ui/components";
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

/** The new task in the Agent view: a header, room for the conversation, the composer at the bottom. */
function NewTaskView() {
  const workspace = useAgents((s) => s.workspace);
  const [self, setSelf] = useState<HTMLElement | null>(null);
  return (
    <section ref={setSelf} className="agents-task" aria-label="New task" data-testid="agents-new-task-view" data-agent-chat>
      <header className="agents-task__header">
        <IconButton icon={ArrowLeft} size="sm" aria-label="Back to tasks" onPress={() => agents.show("tasks", null)} data-testid="agents-show-tasks" />
        <h2 className="agents-task__title">New task</h2>
      </header>
      <div className="agents-task__scroll">
        <EmptyState
          title={`Describe what the agent should do${workspace ? ` in ${workspace.name}` : ""}`}
          description="Options sets the model, permissions, and budgets."
        />
      </div>
      <div className="agents-task__foot">
        <TaskComposer boundary={self} />
      </div>
    </section>
  );
}

/** Body of the Agent view (the shell provides the tab and the region). */
export function AgentPanel() {
  const view = useAgents((s) => s.view);
  const selected = useAgents((s) => s.selected);
  const error = useAgents((s) => s.error);

  useEffect(() => {
    void agents.init();
  }, []);

  return (
    <div className="agents agents-panel" data-testid="agents-panel">
      {view === "tasks" && (
        <div className="agents-bar">
          <h2 className="agents-task__title">Tasks</h2>
          <Toolbar aria-label="Agent actions" className="agents-bar__actions">
            <Button variant="primary" size="sm" icon={Plus} onPress={() => agents.show("compose")} data-testid="agents-new-task">
              New task
            </Button>
          </Toolbar>
          <ProviderLabel />
        </div>
      )}
      {error && (
        <div className="agents-error" data-testid="agents-error">
          <Banner tone="danger" title="Agent action failed" live actions={<Button size="sm" variant="ghost" onPress={() => agents.setError(null)}>Dismiss</Button>}>
            {error}
          </Banner>
        </div>
      )}
      <div className="agents-content" data-view={view}>
        {view === "tasks" && <TaskList />}
        {view === "compose" && <NewTaskView />}
        {view === "task" && selected && <TaskView id={selected} />}
        {view === "review" && selected && <ReviewView id={selected} />}
      </div>
    </div>
  );
}
