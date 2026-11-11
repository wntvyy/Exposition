// Agent feature registration: the agent panel (auxiliary view), the review
// editor (`agents-review:<taskId>` resources), commands, and a status item.
import { Bot } from "lucide-react";
import { createElement, useEffect } from "react";
import { createRoot } from "react-dom/client";

import type { EditorProps } from "../../app/registry";
import { registry } from "../../app/registry";
import { commands } from "../../lib/ipc";
import { AgentPanel } from "./AgentPanel";
import { isActive } from "./format";
import { ReviewView } from "./ReviewView";
import { agents, useAgents } from "./store";

export const REVIEW_PREFIX = "agents-review:";

function ReviewEditor({ resource }: EditorProps) {
  useEffect(() => {
    void agents.init();
  }, []);
  return createElement(ReviewView, { id: resource.key.slice(REVIEW_PREFIX.length) });
}

function AgentStatusItem() {
  const running = useAgents((s) => s.tasks.filter((t) => isActive(t.status)).length);
  const review = useAgents((s) => s.tasks.reduce((n, t) => n + t.pendingReview, 0));
  useEffect(() => {
    void agents.init();
  }, []);
  if (!running && !review) return null;
  const parts = [running ? `${running} agent task${running === 1 ? "" : "s"} running` : "", review ? `${review} to review` : ""];
  return createElement("span", { className: "agents-status-item" }, parts.filter(Boolean).join(" · "));
}

registry.registerView({
  id: "agents.panel",
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
  run: () => agents.show("compose"),
});

registry.registerCommand({
  id: "agents.providerSettings",
  title: "Agent Provider Settings",
  category: "Agent",
  run: () => agents.openSettings(true),
});

registry.registerStatusItem({
  id: "agents.status",
  alignment: "right",
  order: 50,
  render: () => createElement(AgentStatusItem),
});

// Development builds only: let end-to-end scenarios mount the agent panel in
// the real app before the workbench shell renders auxiliary views.
void commands
  .appInfo()
  .then((info) => {
    if (!info.debugBuild) return;
    const target = window as unknown as { __expositionAgentsDev?: { mountPanel: () => void } };
    target.__expositionAgentsDev = {
      mountPanel() {
        if (document.getElementById("agents-dev-host")) return;
        const host = document.createElement("div");
        host.id = "agents-dev-host";
        host.className = "agents-dev-host";
        document.body.appendChild(host);
        createRoot(host).render(createElement(AgentPanel));
      },
    };
  })
  .catch(() => {
    // Not running inside the desktop app (e.g. unit tests).
  });
