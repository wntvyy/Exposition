// Approval visibility, stopped tasks, review state that follows content, and
// the key-removal confirmation.
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { TaskSummary } from "../../bindings/TaskSummary";

const ipc = vi.hoisted(() => ({
  agentsTaskDetail: vi.fn(),
  agentsProviderClearKey: vi.fn(),
  agentsReview: vi.fn(),
  agentsReviewDiff: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn(), workspace: vi.fn(), documents: vi.fn() },
  errorMessage: (e: unknown) => String(e),
}));

// Monaco needs a real browser; the diff itself is not under test here.
vi.mock("./DiffView", () => ({ DiffView: () => null }));

const { statusLabel, stopLabel } = await import("./format");
const { DEFAULT_BUDGETS } = await import("./TaskComposer");
const store = await import("./store");
const { AgentStatusItem } = await import("./register");
const { ProviderSettingsDialog } = await import("./ProviderSettingsDialog");
const { ReviewView } = await import("./ReviewView");
const { ConfirmHost } = await import("../../ui/components");
const { useNotifications } = await import("../../app/notifications");
const { layout, useLayout } = await import("../../app/layout");

const { approvalNotificationId, finishedNotification, initialAgentsState, reduceEvent, reviewAffectedBy, useAgents } = store;

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

const approval = {
  id: "approval-1",
  taskId: "task-1",
  toolUseId: "t1",
  tool: "create_file",
  title: "Create notes/new.typ",
  detail: "= Draft",
};

afterEach(() => {
  cleanup();
  useNotifications.setState({ items: [] });
  useAgents.setState(initialAgentsState, true);
});

describe("stopped tasks", () => {
  it("label the stop reason specifically and never as completed", () => {
    expect(stopLabel("max_tokens")).toBe("Stopped: response cut off");
    expect(stopLabel("model_context_window_exceeded")).toBe("Stopped: context window full");
    expect(stopLabel("pause_turn")).toBe("Stopped: response paused");
    expect(stopLabel("something_new")).toBe("Stopped: unexpected response");
    const pill = statusLabel({ kind: "stopped", stopReason: "max_tokens", message: "cut off" });
    expect(pill).toEqual({ status: "warning", label: "Stopped: response cut off" });
    const note = finishedNotification(
      summary(),
      summary({ status: { kind: "stopped", stopReason: "model_context_window_exceeded", message: "The conversation filled…" } }),
    );
    expect(note).toMatchObject({ tone: "warning", title: "Stopped: context window full: Draft a note" });
    expect(note?.body).toContain("The conversation filled");
  });
});

describe("approval notifications", () => {
  it("are persistent, open the approval, and retire when answered or the task stops", () => {
    const state = { ...initialAgentsState, tasks: [summary({ status: { kind: "awaitingApproval" } })] };
    const requested = reduceEvent(state, { kind: "approvalRequested", approval });
    expect(requested.notification).toMatchObject({
      id: approvalNotificationId("task-1"),
      tone: "warning",
      title: "Agent needs your approval",
      body: "Create notes/new.typ · Task: Draft a note",
      timeoutMs: null,
    });
    expect(requested.notification?.actions?.map((a) => a.label)).toEqual(["Open approval"]);
    const resolved = reduceEvent(state, { kind: "approvalResolved", taskId: "task-1", approvalId: "approval-1", approved: true });
    expect(resolved.dismiss).toEqual([approvalNotificationId("task-1")]);
    const canceled = reduceEvent(state, { kind: "taskChanged", task: summary({ status: { kind: "canceled" } }) });
    expect(canceled.dismiss).toEqual([approvalNotificationId("task-1")]);
    const stillRunning = reduceEvent(state, { kind: "taskChanged", task: summary({ status: { kind: "running" } }) });
    expect(stillRunning.dismiss).toBeUndefined();
  });

  it("the action shows the Agent view on the task", () => {
    layout.setVisible("auxiliary", false);
    const action = store.approvalNotification("task-1", "Create notes/new.typ", "Draft a note").actions?.[0];
    act(() => void action?.run());
    expect(layout.isVisible("auxiliary")).toBe(true);
    expect(useLayout.getState().auxiliary.activeView).toBe(store.AGENT_VIEW_ID);
    expect(useAgents.getState()).toMatchObject({ view: "task", selected: "task-1" });
  });
});

describe("status bar item", () => {
  beforeEach(() => layout.setVisible("auxiliary", false));

  it("asks for attention when a task waits for approval and opens it from the keyboard", () => {
    useAgents.setState({ tasks: [summary({ status: { kind: "awaitingApproval" } }), summary({ id: "task-2" })] });
    render(<AgentStatusItem />);
    const button = screen.getByRole("button", { name: "Agent: approval needed" });
    expect(button.querySelector("[data-status='warning']")).toBeTruthy();
    button.focus();
    act(() => {
      fireEvent.click(button);
    });
    expect(layout.isVisible("auxiliary")).toBe(true);
    expect(useAgents.getState()).toMatchObject({ view: "task", selected: "task-1" });
  });

  it("shows running work and pending review without a button", () => {
    useAgents.setState({ tasks: [summary()] });
    const { rerender } = render(<AgentStatusItem />);
    expect(screen.getByText("Agent: 1 running")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
    useAgents.setState({ tasks: [summary({ status: { kind: "completed" }, pendingReview: 2 })] });
    rerender(<AgentStatusItem />);
    expect(screen.getByText("Agent: 2 to review")).toBeTruthy();
  });
});

describe("review follows content", () => {
  it("reads edits that are not the agent's as review-affecting", () => {
    const changed = (kind: "agent" | "source") => ({
      kind: "changed" as const,
      document: "doc-1",
      transaction: null,
      origin: kind === "agent" ? { kind: "agent" as const, taskId: "task-1" } : { kind: "source" as const },
      change: "undo" as const,
      previousRevision: 2,
      revision: 3,
      patches: [],
      selection: null,
      canUndo: false,
      canRedo: true,
    });
    expect(reviewAffectedBy(changed("source"))).toBe(true);
    expect(reviewAffectedBy(changed("agent"))).toBe(false);
    expect(reviewAffectedBy({ kind: "closed", document: "doc-1" })).toBe(true);
  });

  it("shows an undone change as undone, with nothing to keep or revert", async () => {
    useAgents.setState({ tasks: [summary({ status: { kind: "completed" }, changedFiles: 1 })] });
    ipc.agentsReview.mockResolvedValue({
      taskId: "task-1",
      checkpoint: null,
      files: [
        {
          path: "main.typ",
          kind: "edited",
          state: "undone",
          changeCount: 1,
          entangled: false,
          tipRevision: 2,
          currentRevision: 3,
          message: "The agent's changes are no longer in the file: they were undone or discarded.",
        },
      ],
    });
    ipc.agentsReviewDiff.mockResolvedValue(null);
    render(<ReviewView id="task-1" />);
    await screen.findByText("Undone in editor");
    expect(screen.getByText(/no longer in the file/)).toBeTruthy();
    expect(screen.queryByText(/edited since/)).toBeNull();
    for (const name of ["Keep", "Revert"]) {
      expect(screen.getByRole("button", { name }).hasAttribute("disabled")).toBe(true);
    }
  });
});

describe("provider settings", () => {
  const status: ProviderStatus = {
    settings: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium", consent: "scopeContent" },
    configured: true,
    credentialStore: "Windows Credential Manager",
    credentialError: null,
    scriptedAvailable: false,
  };

  beforeEach(() => {
    ipc.agentsProviderClearKey.mockReset();
    ipc.agentsProviderClearKey.mockResolvedValue({ ...status, configured: false });
    useAgents.setState({ settingsOpen: true, status });
  });

  it("asks before removing the stored key; Keep is focused and Escape keeps it", async () => {
    render(
      <>
        <ProviderSettingsDialog />
        <ConfirmHost />
      </>,
    );
    fireEvent.click(screen.getByTestId("agents-key-remove"));
    const alert = await screen.findByRole("alertdialog");
    expect(alert.textContent).toContain("Remove the API key?");
    // Escape keeps the key. (That "Keep key" gets focus is checked in the real
    // app; jsdom does not move focus between sibling modal scopes.)
    expect(screen.getByRole("button", { name: "Keep key" })).toBeTruthy();
    fireEvent.keyDown(alert, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(ipc.agentsProviderClearKey).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("agents-key-remove"));
    fireEvent.click(await screen.findByRole("button", { name: "Remove key" }));
    await waitFor(() => expect(ipc.agentsProviderClearKey).toHaveBeenCalledTimes(1));
    await screen.findByText("API key removed.");
  });
});
