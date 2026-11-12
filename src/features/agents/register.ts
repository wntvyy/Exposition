// Agent feature registration: the Agent view (auxiliary region), the review
// editor (`agents-review:<taskId>` resources), commands, and a status item.
import { Bot } from "lucide-react";
import { createElement, useEffect } from "react";

import type { EditorProps } from "../../app/registry";
import { registry } from "../../app/registry";
import { layout } from "../../app/layout";
import { StatusPill } from "../../ui/components";
import { AgentPanel } from "./AgentPanel";
import { isActive, waitingLabel } from "./format";
import { REVIEW_PREFIX, ReviewView } from "./ReviewView";
import { AGENT_VIEW_ID, agents, openApproval, useAgents } from "./store";

export { AGENT_VIEW_ID };

function ReviewEditor({ resource }: EditorProps) {
  useEffect(() => {
    void agents.init();
  }, []);
  return createElement(
    "div",
    { className: "agents agents-editor" },
    createElement(ReviewView, { id: resource.key.slice(REVIEW_PREFIX.length), inEditor: true }),
  );
}

export function AgentStatusItem() {
  const running = useAgents((s) => s.tasks.filter((t) => isActive(t.status)).length);
  const waiting = useAgents((s) => s.tasks.filter((t) => t.status.kind === "awaitingApproval").length);
  const review = useAgents((s) => s.tasks.reduce((n, t) => n + t.pendingReview, 0));
  useEffect(() => {
    void agents.init();
  }, []);
  if (waiting) {
    // A task is blocked on the user: attention style, and a button that opens
    // the (oldest) pending approval wherever the Agent view is.
    const label = waitingLabel(waiting);
    return createElement(
      "button",
      {
        type: "button",
        className: "agents-status-attention",
        "data-testid": "agents-status-approval",
        title: "Open the pending approval",
        onClick: () => {
          const blocked = useAgents.getState().tasks.filter((t) => t.status.kind === "awaitingApproval");
          const oldest = blocked.sort((x, y) => x.updatedAtMs - y.updatedAtMs)[0];
          if (oldest) openApproval(oldest.id);
        },
      },
      createElement(StatusPill, { status: "warning", size: "sm", label }),
    );
  }
  if (running) {
    return createElement(StatusPill, { status: "running", size: "sm", label: `Agent: ${running} running` });
  }
  if (review) {
    return createElement(StatusPill, { status: "warning", size: "sm", label: `Agent: ${review} to review` });
  }
  return null;
}

function showAgent() {
  layout.showView("auxiliary", AGENT_VIEW_ID);
}

registry.registerView({
  id: AGENT_VIEW_ID,
  location: "auxiliary",
  title: "Agent",
  icon: Bot,
  order: 10,
  render: () => createElement(AgentPanel),
});

registry.registerEditor({
  id: "agents.review",
  priority: 100,
  matches: (resource) => resource.key.startsWith(REVIEW_PREFIX),
  modes: ["source"],
  component: ReviewEditor,
});

registry.registerCommand({
  id: "agents.newTask",
  title: "New Agent Task",
  category: "Agent",
  run: () => {
    showAgent();
    agents.show("compose");
  },
});

registry.registerCommand({
  id: "agents.showTasks",
  title: "Show Agent Tasks",
  category: "Agent",
  run: () => {
    showAgent();
    agents.show("tasks", null);
  },
});

registry.registerCommand({
  id: "agents.openApproval",
  title: "Open Pending Agent Approval",
  category: "Agent",
  enabled: () => useAgents.getState().tasks.some((t) => t.status.kind === "awaitingApproval"),
  run: () => {
    const task = useAgents.getState().tasks.find((t) => t.status.kind === "awaitingApproval");
    if (task) openApproval(task.id);
  },
});

registry.registerCommand({
  id: "agents.providerSettings",
  title: "Agent Provider Settings",
  category: "Agent",
  run: () => {
    showAgent();
    agents.openSettings(true);
  },
});

registry.registerStatusItem({
  id: "agents.status",
  alignment: "right",
  order: 50,
  render: () => createElement(AgentStatusItem),
});
