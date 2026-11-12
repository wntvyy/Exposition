// Agent feature registration: the Agent view (auxiliary region), the review
// editor (`agents-review:<taskId>` resources), commands, and a status item.
import { Bot } from "lucide-react";
import { createElement, useEffect } from "react";

import type { EditorProps } from "../../app/registry";
import { registry } from "../../app/registry";
import { layout } from "../../app/layout";
import { StatusPill } from "../../ui/components";
import { AgentPanel } from "./AgentPanel";
import { isActive } from "./format";
import { REVIEW_PREFIX, ReviewView } from "./ReviewView";
import { agents, useAgents } from "./store";

export const AGENT_VIEW_ID = "agents.panel";

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

function AgentStatusItem() {
  const running = useAgents((s) => s.tasks.filter((t) => isActive(t.status)).length);
  const review = useAgents((s) => s.tasks.reduce((n, t) => n + t.pendingReview, 0));
  useEffect(() => {
    void agents.init();
  }, []);
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
