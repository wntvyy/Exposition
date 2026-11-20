// One vocabulary for tool-call and approval states across the task list, task
// view (status, streaming draft, transcript), and status bar; and a provider
// chip that follows the provider actually selected. Uses only long-standing
// exports, so a regression shows up as a wrong label, not a missing import.
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AgentEvent } from "../../bindings/AgentEvent";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskSummary } from "../../bindings/TaskSummary";
import type { ToolCallStatus } from "../../bindings/ToolCallStatus";
import type { TaskGraphInfo } from "../../bindings/TaskGraphInfo";

const NO_GRAPH: TaskGraphInfo = { project: null, context: null, contexts: 0, targets: [], proposals: [], investigation: null };

const ipc = vi.hoisted(() => ({
  agentsTaskDetail: vi.fn(),
  agentsProviderStatus: vi.fn(),
  agentsTaskList: vi.fn(),
  workspaceCurrent: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn(), workspace: vi.fn(), documents: vi.fn() },
  errorMessage: (e: unknown) => String(e),
}));
vi.mock("./DiffView", () => ({ DiffView: () => null }));
// The panel's first load never completes here, so it cannot overwrite the
// state each test sets up.
ipc.agentsProviderStatus.mockReturnValue(new Promise(() => {}));

const { statusLabel, toolStatusLabel } = await import("./format");
const { DEFAULT_BUDGETS } = await import("./TaskComposer");
const { initialAgentsState, reduceEvent, useAgents } = await import("./store");
const { TaskView } = await import("./TaskView");
const { TaskList } = await import("./TaskList");
const { AgentStatusItem } = await import("./register");
const { AgentPanel } = await import("./AgentPanel");

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    title: "Draft a note file",
    createdAtMs: 1,
    updatedAtMs: 1,
    status: { kind: "running" },
    spec: {
      prompt: "Draft a note file",
      workspaceRoot: "C:/paper",
      scope: { kind: "workspace" },
      permission: "reviewEdits",
      execution: "disabled",
      consent: "scopeContent",
      budgets: DEFAULT_BUDGETS,
      provider: { provider: "scripted", model: "claude-opus-5-5", effort: "medium" },
      approvalMode: "ask",
    },
    usage: {
      turns: 1,
      inputTokens: 1,
      outputTokens: 1,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      toolCalls: 1,
      commands: 0,
      elapsedMs: 0,
      servedBy: null,
    },
    pendingReview: 0,
    changedFiles: 0,
    ...overrides,
  };
}

/** Apply events to the store the way the app does. */
function dispatch(...events: AgentEvent[]) {
  for (const event of events) useAgents.setState(reduceEvent(useAgents.getState(), event).patch);
}

const approval = {
  id: "approval-1",
  taskId: "task-1",
  toolUseId: "c",
  tool: "create_file",
  title: "Create notes/new.typ",
  detail: "= Draft",
  reason: null,
  forTask: [],
};

afterEach(() => {
  cleanup();
  useAgents.setState(initialAgentsState, true);
});

describe("tool-call states", () => {
  it("every state has its own specific label", () => {
    const all: ToolCallStatus[] = ["pending", "awaitingApproval", "running", "succeeded", "failed", "declined", "canceled", "notRun"];
    const labels = all.map((s) => toolStatusLabel(s)?.label);
    expect(labels).toEqual([
      "Pending",
      "Waiting for approval",
      "Running",
      "Done",
      "Failed",
      "Declined",
      "Canceled",
      "Not run",
    ]);
    expect(toolStatusLabel("pending").status).toBe("idle");
    expect(toolStatusLabel("declined").status).toBe("canceled");
  });

  it("an interrupted task has the tone of its banner: a warning, not the user's cancel (UI-2)", () => {
    expect(statusLabel({ kind: "interrupted" })).toEqual({ status: "warning", label: "Interrupted" });
    expect(statusLabel({ kind: "canceled" })).toEqual({ status: "canceled", label: "Canceled" });
  });

  it("a task waiting for approval says so in the same words as its tool call", () => {
    expect(statusLabel({ kind: "awaitingApproval" })).toEqual({ status: "warning", label: "Waiting for approval" });
    expect(toolStatusLabel("awaitingApproval").label).toBe(statusLabel({ kind: "awaitingApproval" }).label);
  });
});

describe("the streaming draft follows the call through approval", () => {
  function renderTask(status: TaskSummary["status"]) {
    const detail: TaskDetail = { summary: summary({ status }), transcript: [], approvals: [], graph: NO_GRAPH, allowances: [] };
    ipc.agentsTaskDetail.mockResolvedValue(detail);
    useAgents.setState({ details: { "task-1": detail }, tasks: [detail.summary] });
    return render(<TaskView id="task-1" />);
  }

  it("shows waiting for approval (not running) until the user approves, then running", async () => {
    dispatch(
      { kind: "streamStarted", taskId: "task-1", turn: 1 },
      { kind: "toolCallStarted", taskId: "task-1", toolUseId: "c", name: "create_file" },
    );
    const view = renderTask({ kind: "running" });
    expect(screen.getByText("Running: create_file")).toBeTruthy();

    act(() => dispatch({ kind: "approvalRequested", approval }));
    view.rerender(<TaskView id="task-1" />);
    expect(screen.queryByText(/Running.*create_file/)).toBeNull();
    expect(screen.getByText("Waiting for approval: create_file")).toBeTruthy();

    act(() => dispatch({ kind: "approvalResolved", taskId: "task-1", approvalId: "approval-1", approved: true }));
    view.rerender(<TaskView id="task-1" />);
    expect(screen.getByText("Running: create_file")).toBeTruthy();
  });

  it("a declined call is not shown as running", () => {
    dispatch(
      { kind: "streamStarted", taskId: "task-1", turn: 1 },
      { kind: "toolCallStarted", taskId: "task-1", toolUseId: "c", name: "create_file" },
      { kind: "approvalRequested", approval },
      { kind: "approvalResolved", taskId: "task-1", approvalId: "approval-1", approved: false },
    );
    renderTask({ kind: "running" });
    expect(screen.queryByText(/create_file/)).toBeNull();
  });

  // Independent review D1: the draft said "Running: ask_user" while the task waited for the user.
  it("shows waiting for your answer (not running) while a question is open, then running", async () => {
    const request = {
      id: "question-1",
      taskId: "task-1",
      toolUseId: "q",
      revision: 1,
      questions: [
        {
          id: "q1",
          prompt: "Which unit?",
          explanation: null,
          options: [
            { id: "c", label: "Celsius", consequence: null, recommended: false },
            { id: "k", label: "Kelvin", consequence: null, recommended: false },
          ],
          allowCustom: true,
        },
      ],
      state: { kind: "pending" as const },
      createdAtMs: 1,
    };
    dispatch(
      { kind: "streamStarted", taskId: "task-1", turn: 1 },
      { kind: "toolCallStarted", taskId: "task-1", toolUseId: "q", name: "ask_user" },
      { kind: "questionRequested", request },
    );
    const view = renderTask({ kind: "awaitingAnswer" });
    expect(screen.queryByText(/Running.*ask_user/)).toBeNull();
    expect(screen.getByText("Waiting for your answer: ask_user")).toBeTruthy();

    act(() => dispatch({ kind: "questionResolved", taskId: "task-1", requestId: "question-1" }));
    view.rerender(<TaskView id="task-1" />);
    expect(screen.getByText("Running: ask_user")).toBeTruthy();
  });
});

describe("a finished task's outcome", () => {
  it("a budget stop says what a follow-up keeps, and the spec names the effort", () => {
    const stopped = summary({
      status: { kind: "budgetExceeded", budget: "turns", detail: "the limit of 40 model turns was reached." },
      spec: { ...summary().spec, provider: { provider: "anthropic", model: "claude-opus-5-5", effort: "xhigh" } },
    });
    const detail: TaskDetail = { summary: stopped, transcript: [], approvals: [], graph: NO_GRAPH, allowances: [] };
    ipc.agentsTaskDetail.mockResolvedValue(detail);
    useAgents.setState({ details: { "task-1": detail }, tasks: [stopped] });
    render(<TaskView id="task-1" />);
    // The UI's follow-up sends no new budgets and usage counts across runs.
    expect(
      screen.getByText("The limit of 40 model turns was reached. A follow-up keeps the same budgets and the usage so far."),
    ).toBeTruthy();
    expect(screen.getByTestId("agents-options").textContent).toContain("claude-opus-5-5 · Extra high");
  });
});

describe("waiting for approval reads the same everywhere", () => {
  it("in the task list and the status bar", () => {
    useAgents.setState({ tasks: [summary({ status: { kind: "awaitingApproval" } })] });
    render(
      <>
        <TaskList />
        <AgentStatusItem />
      </>,
    );
    expect(screen.getByText("Waiting for approval")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Agent: waiting for approval" })).toBeTruthy();
  });
});

describe("provider chip", () => {
  const status = (overrides: Partial<ProviderStatus> = {}): ProviderStatus => ({
    settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    configured: false,
    connections: [],
    credentialStore: "Windows Credential Manager",
    credentialError: null,
    scriptedAvailable: true,
    ...overrides,
  });

  it("follows the provider selected anywhere (the backend announces each change)", () => {
    useAgents.setState({ status: status() });
    render(<AgentPanel />);
    const chip = () => screen.getByTestId("agents-provider-chip");
    expect(chip().textContent).toBe("Claude · no API key configured");
    // Selected from another surface (Settings, another window, a script).
    const scripted = status({ settings: { ...status().settings, provider: "scripted" } });
    act(() => dispatch({ kind: "providerChanged", status: scripted }));
    expect(chip().textContent).toBe("Scripted provider — development only");
    act(() => dispatch({ kind: "providerChanged", status: status({ configured: true }) }));
    expect(chip().textContent).toBe("Claude · claude-opus-5-5");
  });
});
