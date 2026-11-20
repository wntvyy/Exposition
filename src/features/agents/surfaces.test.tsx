// What the shared store does for whichever surface is on screen: attention
// from a page that shows neither the Agent view nor the Agents page (AG-3),
// a finished task announced when its detail is read first (AG-2), and a
// resync that covers every loaded conversation (AG-10).
import { act, cleanup } from "@testing-library/react";
import { Network } from "lucide-react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentEvent } from "../../bindings/AgentEvent";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskGraphInfo } from "../../bindings/TaskGraphInfo";
import type { TaskSummary } from "../../bindings/TaskSummary";

const ipc = vi.hoisted(() => ({
  agentsTaskDetail: vi.fn(),
  agentsTaskList: vi.fn(),
  agentsProviderStatus: vi.fn(),
  agentsModelsList: vi.fn(() => new Promise(() => {})),
  workspaceCurrent: vi.fn(),
}));
const subscriptions = vi.hoisted(() => ({
  agents: vi.fn(),
  agentsResync: vi.fn(),
  workspace: vi.fn(),
  documents: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: subscriptions,
  errorMessage: (e: unknown) => String(e),
}));
vi.mock("../operations/client", () => ({ onProjectOp: vi.fn() }));
// Monaco needs a real browser; no diff is under test here.
vi.mock("./DiffView", () => ({ DiffView: () => null }));

const { DEFAULT_BUDGETS } = await import("./TaskComposer");
const store = await import("./store");
await import("./register");
const { useNotifications } = await import("../../app/notifications");
const { layout, useLayout } = await import("../../app/layout");
const { EDITOR_PAGE, pages } = await import("../../app/pages");
const { registry } = await import("../../app/registry");

const { agents, approvalNotificationId, initialAgentsState, questionNotificationId, useAgents } = store;

const NO_GRAPH: TaskGraphInfo = { project: null, context: null, contexts: 0, targets: [], proposals: [], investigation: null };
/** A page that shows neither the Agent view nor the Agents page (like Research or Graph). */
const OTHER_PAGE = "test.other";

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    title: "Draft a note",
    createdAtMs: 1,
    updatedAtMs: 1,
    status: { kind: "running" },
    spec: {
      prompt: "Draft a note",
      workspaceRoot: "C:/paper",
      scope: { kind: "workspace" },
      permission: "reviewEdits",
      execution: "disabled",
      consent: "scopeContent",
      budgets: DEFAULT_BUDGETS,
      provider: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium" },
      approvalMode: "ask",
    },
    usage: {
      turns: 1,
      inputTokens: 10,
      outputTokens: 10,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      toolCalls: 1,
      commands: 0,
      elapsedMs: 1000,
      servedBy: null,
    },
    pendingReview: 0,
    changedFiles: 0,
    ...overrides,
  };
}

const detailOf = (task: TaskSummary): TaskDetail => ({
  summary: task,
  transcript: [],
  approvals: [],
  graph: NO_GRAPH,
  allowances: [],
  questions: [],
});

const approval = {
  id: "approval-1",
  taskId: "task-1",
  toolUseId: "t1",
  tool: "create_file",
  title: "Create notes/new.typ",
  detail: "= Draft",
  reason: null,
  forTask: [],
};

let emit: (event: AgentEvent) => void;
let resync: () => void;
const notificationIds = () => useNotifications.getState().items.map((n) => n.id);
const settle = () => act(async () => void (await new Promise((r) => setTimeout(r, 0))));

beforeAll(async () => {
  registry.registerPage({ id: OTHER_PAGE, title: "Other", icon: Network, render: () => null });
  ipc.agentsTaskList.mockResolvedValue([]);
  ipc.agentsProviderStatus.mockResolvedValue(null);
  ipc.workspaceCurrent.mockResolvedValue(null);
  await agents.init();
  emit = subscriptions.agents.mock.calls[0]![0] as typeof emit;
  resync = subscriptions.agentsResync.mock.calls[0]![0] as typeof resync;
});

beforeEach(() => {
  ipc.agentsTaskDetail.mockReset();
  ipc.agentsTaskDetail.mockImplementation((id: string) => Promise.resolve(detailOf(summary({ id, status: { kind: "awaitingApproval" } }))));
  pages.show(EDITOR_PAGE);
  layout.showView("auxiliary", store.AGENT_VIEW_ID);
});

afterEach(async () => {
  await settle();
  cleanup();
  useNotifications.setState({ items: [] });
  useAgents.setState(initialAgentsState, true);
  pages.show(EDITOR_PAGE);
});

describe("attention from a page that shows no agent surface (AG-3)", () => {
  /** The Agent view is open on the task in the Editor page, which is not the page on screen. */
  function panelOpenOnTaskBehind(page: string) {
    useAgents.setState({ tasks: [summary({ status: { kind: "awaitingApproval" } })], view: "task", selected: "task-1" });
    pages.show(page);
    expect(layout.isVisible("auxiliary")).toBe(true);
    expect(useLayout.getState().auxiliary.activeView).toBe(store.AGENT_VIEW_ID);
  }

  it("raises the approval and question notifications: the hidden Agent view is not on screen", async () => {
    panelOpenOnTaskBehind(OTHER_PAGE);
    act(() => emit({ kind: "approvalRequested", approval }));
    expect(notificationIds()).toContain(approvalNotificationId("task-1"));
    act(() =>
      emit({
        kind: "questionRequested",
        request: { id: "question-1", taskId: "task-1", toolUseId: "t2", revision: 1, questions: [], state: { kind: "pending" }, createdAtMs: 1 } as never,
      }),
    );
    expect(notificationIds()).toContain(questionNotificationId("task-1"));
  });

  it("still raises none while the Agent view shows that task on the Editor page", async () => {
    panelOpenOnTaskBehind(EDITOR_PAGE);
    act(() => emit({ kind: "approvalRequested", approval }));
    expect(notificationIds()).not.toContain(approvalNotificationId("task-1"));
  });

  it("a resync notifies for a waiting task the hidden Agent view has selected", async () => {
    panelOpenOnTaskBehind(OTHER_PAGE);
    ipc.agentsTaskList.mockResolvedValueOnce([summary({ status: { kind: "awaitingApproval" } })]);
    act(() => resync());
    await settle();
    expect(notificationIds()).toContain(approvalNotificationId("task-1"));
  });

  it("Open approval and Open question bring the Editor page and the card's task on screen", async () => {
    panelOpenOnTaskBehind(OTHER_PAGE);
    useAgents.setState({ view: "tasks", selected: null });
    act(() => void store.approvalNotification("task-1", null, "Draft a note").actions?.[0]?.run());
    expect(pages.active()).toBe(EDITOR_PAGE);
    expect(layout.isVisible("auxiliary")).toBe(true);
    expect(useAgents.getState()).toMatchObject({ view: "task", selected: "task-1" });

    pages.show(OTHER_PAGE);
    useAgents.setState({ view: "tasks", selected: null });
    act(() => void store.questionNotification("task-1", null, "Draft a note").actions?.[0]?.run());
    expect(pages.active()).toBe(EDITOR_PAGE);
    expect(useAgents.getState()).toMatchObject({ view: "task", selected: "task-1" });
  });

  it("the Agent view commands show the Editor page it lives on", async () => {
    for (const [command, view] of [
      ["agents.newTask", "compose"],
      ["agents.showTasks", "tasks"],
    ] as const) {
      pages.show(OTHER_PAGE);
      await act(async () => void (await registry.runCommand(command)));
      expect(pages.active()).toBe(EDITOR_PAGE);
      expect(useAgents.getState().view).toBe(view);
    }
  });
});
