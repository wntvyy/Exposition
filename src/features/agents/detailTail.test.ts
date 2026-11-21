// Partial re-reads of a task's detail: a transcript event of a running task
// reads only the end of the conversation and completes it with what is held;
// showing a task, a resync, and anything unsure read all of it.
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentEvent } from "../../bindings/AgentEvent";
import type { GraphContext } from "../../bindings/GraphContext";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskGraphInfo } from "../../bindings/TaskGraphInfo";
import type { TaskSummary } from "../../bindings/TaskSummary";
import type { TranscriptItem } from "../../bindings/TranscriptItem";

const ipc = vi.hoisted(() => ({
  agentsTaskDetail: vi.fn(),
  agentsTaskList: vi.fn(),
  agentsProviderStatus: vi.fn(),
  workspaceCurrent: vi.fn(),
}));
const wired = vi.hoisted(() => ({ emit: null as ((event: unknown) => void) | null, resync: null as (() => void) | null }));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: {
    agents: vi.fn((handler: (event: unknown) => void) => {
      wired.emit = handler;
    }),
    agentsResync: vi.fn((handler: () => void) => {
      wired.resync = handler;
    }),
    workspace: vi.fn(),
    documents: vi.fn(),
  },
  errorMessage: (e: unknown) => String(e),
}));
vi.mock("../operations/client", () => ({ onProjectOp: vi.fn() }));

const { agents, changedFrom, completeDetail, initialAgentsState, useAgents } = await import("./store");

const TASK = "task-1";
const CONTEXT = { revision: 7 } as unknown as GraphContext;

function graph(contexts = 1, context: GraphContext | null = CONTEXT): TaskGraphInfo {
  return { project: null, context, contexts, targets: [], proposals: [], investigation: null };
}

const summary = { id: TASK, title: "Task", createdAtMs: 1, status: { kind: "running" } } as unknown as TaskSummary;

function call(seq: number, toolUseId: string, status: string, result: string | null = null): TranscriptItem {
  return { kind: "toolCall", seq, toolUseId, name: "read_file", input: {}, status, result, isError: false, authorizedBy: null } as TranscriptItem;
}

const user: TranscriptItem = { kind: "userText", seq: 1, text: "Do it", attachments: [] };
const said: TranscriptItem = { kind: "assistantText", seq: 2, text: "Reading." };
const done = call(2, "t1", "succeeded", "{}");
const running = call(4, "t2", "running");

function detail(transcript: TranscriptItem[], info = graph()): TaskDetail {
  return { summary, transcript, approvals: [], graph: info, allowances: [], questions: [] };
}

function emit(event: AgentEvent) {
  wired.emit?.(event);
}

async function settled() {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

describe("changedFrom", () => {
  it("is the latest assistant message, or an earlier call that has not finished", () => {
    expect(changedFrom(detail([]))).toBeNull();
    expect(changedFrom(detail([user]))).toBeNull();
    expect(changedFrom(detail([user, said, done]))).toBe(2);
    expect(changedFrom(detail([user, said, done, running]))).toBe(4);
    // A question still waiting (seq 2) while its discussion (seq 5, 6) follows it.
    const asked = call(2, "ask", "running");
    const reply: TranscriptItem = { kind: "assistantText", seq: 6, text: "It depends." };
    expect(changedFrom(detail([user, asked, { ...user, seq: 5, question: "q1" }, reply]))).toBe(2);
    // A notice after the last assistant message does not move the start.
    expect(changedFrom(detail([user, said, done, { kind: "notice", seq: 9, level: "info", message: "Canceled." }]))).toBe(2);
  });
});

describe("completeDetail", () => {
  it("keeps the held items before the start and the held graph context", () => {
    const held = detail([user, said, done, running]);
    const finished = call(4, "t2", "succeeded", "{}");
    const more: TranscriptItem = { kind: "assistantText", seq: 6, text: "Done." };
    const read = { ...detail([finished, more], graph(1, null)), transcriptFrom: 4 };
    const complete = completeDetail(held, read);
    expect(complete?.transcript).toEqual([user, said, done, finished, more]);
    // Unchanged items are the same objects (a list can skip re-rendering them).
    expect(complete?.transcript[0]).toBe(user);
    expect(complete?.graph.context).toBe(CONTEXT);
    expect(complete && "transcriptFrom" in complete).toBe(false);
  });

  it("cannot complete without a held detail or across a new graph context", () => {
    const read = { ...detail([running], graph(2, null)), transcriptFrom: 4 };
    expect(completeDetail(undefined, read)).toBeNull();
    expect(completeDetail(detail([user, said, done], graph(1)), read)).toBeNull();
    const whole = detail([user]);
    expect(completeDetail(undefined, whole)).toBe(whole);
  });
});

describe("re-reading a task's detail", () => {
  beforeEach(async () => {
    ipc.agentsTaskDetail.mockReset();
    ipc.agentsTaskList.mockResolvedValue([summary]);
    ipc.agentsProviderStatus.mockResolvedValue(null);
    ipc.workspaceCurrent.mockResolvedValue(null);
    useAgents.setState({ ...initialAgentsState, tasks: [summary] });
    await agents.init();
    await settled();
    ipc.agentsTaskDetail.mockReset();
  });

  it("reads only the tail on a transcript event and completes it", async () => {
    const held = detail([user, said, done, running]);
    useAgents.setState({ details: { [TASK]: held } });
    const finished = call(4, "t2", "succeeded", "{}");
    ipc.agentsTaskDetail.mockResolvedValue({ ...detail([finished], graph(1, null)), transcriptFrom: 4 });
    emit({ kind: "toolCallFinished", taskId: TASK, toolUseId: "t2", isError: false });
    await settled();
    expect(ipc.agentsTaskDetail.mock.calls).toEqual([[TASK, 4]]);
    const now = useAgents.getState().details[TASK];
    expect(now?.transcript).toEqual([user, said, done, finished]);
    expect(now?.transcript[1]).toBe(said);
    expect(now?.graph.context).toBe(CONTEXT);
    expect(now && "transcriptFrom" in now).toBe(false);
  });

  it("reads everything when a new run attached a new graph context", async () => {
    useAgents.setState({ details: { [TASK]: detail([user, said, done]) } });
    const next: TranscriptItem = { ...user, seq: 7, text: "And then?" };
    const whole = detail([user, said, done, next], graph(2, { revision: 9 } as unknown as GraphContext));
    ipc.agentsTaskDetail.mockImplementation((_task: string, sinceSeq?: number) =>
      Promise.resolve(sinceSeq === undefined ? whole : { ...detail([said, done, next], graph(2, null)), transcriptFrom: sinceSeq }),
    );
    emit({ kind: "transcriptChanged", taskId: TASK });
    await settled();
    expect(ipc.agentsTaskDetail.mock.calls).toEqual([[TASK, 2], [TASK]]);
    expect(useAgents.getState().details[TASK]).toBe(whole);
  });

  it("reads everything when nothing is held, when a task is shown, and on a resync", async () => {
    const whole = detail([user, said, done]);
    ipc.agentsTaskDetail.mockResolvedValue(whole);
    useAgents.setState({ selected: TASK });
    emit({ kind: "transcriptChanged", taskId: TASK });
    await settled();
    expect(ipc.agentsTaskDetail.mock.calls).toEqual([[TASK]]);
    await agents.refetch(TASK);
    expect(ipc.agentsTaskDetail.mock.calls).toEqual([[TASK], [TASK]]);
    wired.resync?.();
    await settled();
    await settled();
    expect(ipc.agentsTaskDetail.mock.calls).toEqual([[TASK], [TASK], [TASK]]);
  });

  it("a full read asked for during a tail read is made afterwards", async () => {
    const held = detail([user, said, done, running]);
    useAgents.setState({ details: { [TASK]: held } });
    let finishTail: (value: TaskDetail) => void = () => {};
    const whole = detail([user, said, done, call(4, "t2", "succeeded", "{}")]);
    ipc.agentsTaskDetail.mockImplementation((_task: string, sinceSeq?: number) =>
      sinceSeq === undefined ? Promise.resolve(whole) : new Promise<TaskDetail>((resolve) => (finishTail = resolve)),
    );
    emit({ kind: "transcriptChanged", taskId: TASK });
    const full = agents.refetch(TASK);
    finishTail({ ...detail([running], graph(1, null)), transcriptFrom: 4 });
    await full;
    await settled();
    expect(ipc.agentsTaskDetail.mock.calls).toEqual([[TASK, 4], [TASK]]);
    expect(useAgents.getState().details[TASK]).toBe(whole);
  });
});
