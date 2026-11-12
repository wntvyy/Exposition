import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskSummary } from "../../bindings/TaskSummary";

const ipc = vi.hoisted(() => ({
  agentsTaskDetail: vi.fn(),
  agentsApprovalResolve: vi.fn(),
  agentsTaskCancel: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { agents: vi.fn(), agentsResync: vi.fn() },
  errorMessage: (e: unknown) => String(e),
}));

const { budgetRows, statusLabel, toolStatusLabel, toolSummary } = await import("./format");
const { buildScope, DEFAULT_BUDGETS } = await import("./TaskComposer");
const { finishedNotification, initialAgentsState, mergeDetail, reduceEvent, useAgents } = await import("./store");
const { TaskView } = await import("./TaskView");

function summary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    title: "Fix the intro",
    createdAtMs: 1,
    updatedAtMs: 1,
    status: { kind: "running" },
    spec: {
      prompt: "Fix the intro",
      workspaceRoot: "C:/paper",
      scope: { kind: "workspace" },
      permission: "reviewEdits",
      execution: "disabled",
      consent: "scopeContent",
      budgets: DEFAULT_BUDGETS,
      provider: { provider: "anthropic", model: "claude-opus-5-5", effort: "medium" },
    },
    usage: {
      turns: 2,
      inputTokens: 1000,
      outputTokens: 500,
      cacheReadTokens: 4000,
      cacheWriteTokens: 0,
      toolCalls: 3,
      commands: 0,
      elapsedMs: 65_000,
      servedBy: "claude-opus-5-5",
    },
    pendingReview: 1,
    changedFiles: 1,
    ...overrides,
  };
}

describe("event reducer", () => {
  it("streams text for the current turn and resets on retry", () => {
    let state = { ...initialAgentsState };
    const apply = (event: Parameters<typeof reduceEvent>[1]) => {
      state = { ...state, ...reduceEvent(state, event).patch };
    };
    apply({ kind: "streamStarted", taskId: "task-1", turn: 1 });
    apply({ kind: "textDelta", taskId: "task-1", turn: 1, text: "Hello " });
    apply({ kind: "textDelta", taskId: "task-1", turn: 1, text: "world" });
    apply({ kind: "textDelta", taskId: "task-1", turn: 0, text: "stale" });
    expect(state.drafts["task-1"]?.text).toBe("Hello world");
    apply({ kind: "streamReset", taskId: "task-1", turn: 1, reason: "overloaded; retrying" });
    expect(state.drafts["task-1"]?.text).toBe("");
    const effect = reduceEvent(state, { kind: "toolCallStarted", taskId: "task-1", toolUseId: "t", name: "read_file" });
    expect(effect.refetch).toBe("task-1");
    expect(effect.patch.drafts?.["task-1"]?.activeTool).toBe("read_file");
  });

  it("keeps task summaries sorted and updates loaded details", () => {
    const detail: TaskDetail = { summary: summary(), transcript: [], approvals: [] };
    const state = { ...initialAgentsState, details: { "task-1": detail } };
    const done = summary({ status: { kind: "completed" } });
    const { patch } = reduceEvent(state, { kind: "taskChanged", task: done });
    expect(patch.tasks?.map((t) => t.id)).toEqual(["task-1"]);
    expect(patch.details?.["task-1"]?.summary.status.kind).toBe("completed");
    const newer = summary({ id: "task-2", createdAtMs: 5 });
    const next = reduceEvent({ ...state, ...patch }, { kind: "taskChanged", task: newer });
    expect(next.patch.tasks?.map((t) => t.id)).toEqual(["task-2", "task-1"]);
  });

  it("notifies only when a running task finishes", () => {
    const running = summary();
    expect(finishedNotification(undefined, summary({ status: { kind: "completed" } }))).toBeUndefined();
    expect(finishedNotification(running, summary({ status: { kind: "completed" } }))?.tone).toBe("success");
    expect(finishedNotification(running, summary({ status: { kind: "canceled" } }))).toBeUndefined();
    const failed = finishedNotification(running, summary({ status: { kind: "failed", message: "network error" } }));
    expect(failed).toMatchObject({ tone: "danger", body: "network error" });
    expect(finishedNotification(summary({ status: { kind: "completed" } }), summary({ status: { kind: "completed" } }))).toBeUndefined();
  });

  it("drops the streamed draft only once it has been persisted", () => {
    const streaming = {
      ...initialAgentsState,
      drafts: { "task-1": { turn: 1, text: "partial", thinking: "", activeTool: null } },
    };
    const detail = { summary: summary(), transcript: [], approvals: [] };
    // A detail read while the turn is still streaming keeps the draft.
    expect(mergeDetail(streaming, detail).drafts?.["task-1"]?.text).toBe("partial");
    const stored = { ...streaming, ...reduceEvent(streaming, { kind: "transcriptChanged", taskId: "task-1" }).patch };
    expect(mergeDetail(stored, detail).drafts?.["task-1"]?.text).toBe("");
    const finished = mergeDetail(streaming, { ...detail, summary: summary({ status: { kind: "completed" } }) });
    expect(finished.drafts?.["task-1"]).toBeUndefined();
  });
});

describe("formatting", () => {
  it("labels statuses and budgets", () => {
    expect(statusLabel({ kind: "budgetExceeded", budget: "toolCalls", detail: "" }).label).toBe(
      "Stopped: tool-call budget",
    );
    expect(statusLabel({ kind: "refused", category: "cyber", explanation: null }).status).toBe("failed");
    expect(statusLabel({ kind: "completed" })).toEqual({ status: "succeeded", label: "Task completed" });
    expect(toolStatusLabel("failed", '{"error":"Lean checking is not available in this build of Exposition"}').status).toBe(
      "unavailable",
    );
    const rows = budgetRows(summary());
    expect(rows.find((r) => r.key === "input")?.used).toBe(5000);
    expect(rows.find((r) => r.key === "time")?.text).toBe("1:05 of 30:00");
  });

  it("summarizes tool inputs and builds scopes", () => {
    expect(toolSummary("edit_document", { path: "main.typ", edits: [{}, {}] })).toBe("main.typ (2 edits)");
    expect(toolSummary("run_command", { program: "latexmk", args: ["-pdf", "a.tex"] })).toBe("latexmk -pdf a.tex");
    expect(toolSummary("insert_artifact", { path: "main.typ", artifact_id: "fig-1" })).toBe("main.typ ← fig-1");
    expect(buildScope("files", "a.typ\nchapters, \n", "")).toEqual({ kind: "files", paths: ["a.typ", "chapters"] });
    expect(buildScope("document", "", " ")).toBeNull();
    expect(buildScope("workspace", "", "")).toEqual({ kind: "workspace" });
  });
});

describe("task view", () => {
  beforeEach(() => {
    const detail: TaskDetail = {
      summary: summary({ status: { kind: "awaitingApproval" } }),
      transcript: [
        { kind: "userText", seq: 1, text: "Fix the intro" },
        { kind: "assistantText", seq: 2, text: "Reading main.typ." },
        {
          kind: "toolCall",
          seq: 2,
          toolUseId: "t1",
          name: "edit_document",
          input: { path: "main.typ", edits: [{ old_text: "a", new_text: "b" }] },
          status: "awaitingApproval",
          result: null,
          isError: false,
        },
        { kind: "notice", seq: 3, level: "warning", message: "Careful" },
      ],
      approvals: [
        { id: "approval-1", taskId: "task-1", toolUseId: "t1", tool: "edit_document", title: "Apply edits to main.typ", detail: "- a\n+ b" },
      ],
    };
    ipc.agentsTaskDetail.mockResolvedValue(detail);
    ipc.agentsApprovalResolve.mockResolvedValue(undefined);
    useAgents.setState({
      ...initialAgentsState,
      details: { "task-1": detail },
      tasks: [detail.summary],
      drafts: { "task-1": { turn: 3, text: "Streaming now", thinking: "", activeTool: null } },
    });
  });

  afterEach(() => cleanup());

  it("renders the transcript, streaming draft, budget meter, and approvals", async () => {
    render(<TaskView id="task-1" />);
    expect(screen.getByRole("log", { name: "Transcript" })).toBeTruthy();
    expect(screen.getByText("Reading main.typ.")).toBeTruthy();
    expect(screen.getByText("Streaming now")).toBeTruthy();
    // Task status and the tool call say the same thing.
    expect(screen.getAllByText("Waiting for approval")).toHaveLength(2);
    expect(screen.getByRole("progressbar", { name: "Tool calls" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    });
    expect(ipc.agentsApprovalResolve).toHaveBeenCalledWith("task-1", "approval-1", true);
  });
});
