// Agent panel state. Rust owns tasks; this store mirrors summaries and the
// selected task's persisted transcript, plus the live streaming draft.
import { create } from "zustand";

import type { AgentEvent } from "../../bindings/AgentEvent";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskSummary } from "../../bindings/TaskSummary";
import { commands, errorMessage, events } from "../../lib/ipc";

export type PanelView = "tasks" | "compose" | "task" | "review";

/** Output streaming for the current turn, before it is persisted. */
export interface StreamDraft {
  turn: number;
  text: string;
  thinking: string;
  /** Name of the tool that is currently running, if any. */
  activeTool: string | null;
}

export interface AgentsState {
  status: ProviderStatus | null;
  tasks: TaskSummary[];
  details: Record<TaskId, TaskDetail>;
  drafts: Record<TaskId, StreamDraft>;
  /** Incremented when a task's review data changes. */
  reviewVersions: Record<TaskId, number>;
  selected: TaskId | null;
  view: PanelView;
  settingsOpen: boolean;
  error: string | null;
}

export const initialAgentsState: AgentsState = {
  status: null,
  tasks: [],
  details: {},
  drafts: {},
  reviewVersions: {},
  selected: null,
  view: "tasks",
  settingsOpen: false,
  error: null,
};

export const useAgents = create<AgentsState>(() => initialAgentsState);

function upsertTask(tasks: TaskSummary[], task: TaskSummary): TaskSummary[] {
  const next = tasks.filter((t) => t.id !== task.id);
  next.push(task);
  return next.sort((a, b) => b.createdAtMs - a.createdAtMs || a.id.localeCompare(b.id));
}

export interface EventEffect {
  patch: Partial<AgentsState>;
  /** Task whose persisted detail should be re-read. */
  refetch: TaskId | null;
}

/** Pure reducer for `agents:event` (exported for tests). */
export function reduceEvent(state: AgentsState, event: AgentEvent): EventEffect {
  switch (event.kind) {
    case "taskChanged": {
      const detail = state.details[event.task.id];
      return {
        patch: {
          tasks: upsertTask(state.tasks, event.task),
          ...(detail ? { details: { ...state.details, [event.task.id]: { ...detail, summary: event.task } } } : {}),
        },
        refetch: null,
      };
    }
    case "streamStarted":
      return {
        patch: {
          drafts: {
            ...state.drafts,
            [event.taskId]: { turn: event.turn, text: "", thinking: "", activeTool: null },
          },
        },
        refetch: null,
      };
    case "textDelta":
    case "thinkingDelta": {
      const draft = state.drafts[event.taskId] ?? { turn: event.turn, text: "", thinking: "", activeTool: null };
      if (draft.turn !== event.turn) return { patch: {}, refetch: null };
      const next =
        event.kind === "textDelta"
          ? { ...draft, text: draft.text + event.text }
          : { ...draft, thinking: draft.thinking + event.text };
      return { patch: { drafts: { ...state.drafts, [event.taskId]: next } }, refetch: null };
    }
    case "streamReset": {
      const draft = state.drafts[event.taskId];
      if (!draft) return { patch: {}, refetch: null };
      return {
        patch: { drafts: { ...state.drafts, [event.taskId]: { ...draft, text: "", thinking: "" } } },
        refetch: null,
      };
    }
    case "toolCallStarted":
    case "toolCallFinished": {
      const draft = state.drafts[event.taskId];
      const activeTool = event.kind === "toolCallStarted" ? event.name : null;
      return {
        patch: draft ? { drafts: { ...state.drafts, [event.taskId]: { ...draft, activeTool } } } : {},
        refetch: event.taskId,
      };
    }
    case "transcriptChanged":
      return { patch: {}, refetch: event.taskId };
    case "approvalRequested":
      return { patch: {}, refetch: event.approval.taskId };
    case "approvalResolved":
      return { patch: {}, refetch: event.taskId };
    case "reviewChanged":
      return {
        patch: {
          reviewVersions: { ...state.reviewVersions, [event.taskId]: (state.reviewVersions[event.taskId] ?? 0) + 1 },
        },
        refetch: null,
      };
    case "fallback":
      return { patch: {}, refetch: null };
  }
}

/**
 * Merge a freshly read detail. The persisted transcript now contains what was
 * streamed, so the draft text is dropped (a running tool name is kept).
 */
export function mergeDetail(state: AgentsState, detail: TaskDetail): Partial<AgentsState> {
  const id = detail.summary.id;
  const draft = state.drafts[id];
  const drafts = { ...state.drafts };
  if (draft) {
    const running = detail.summary.status.kind === "running" || detail.summary.status.kind === "awaitingApproval";
    if (running) drafts[id] = { ...draft, text: "", thinking: "" };
    else delete drafts[id];
  }
  return { details: { ...state.details, [id]: detail }, drafts, tasks: upsertTask(state.tasks, detail.summary) };
}

const inflight = new Map<TaskId, boolean>();

async function refetch(id: TaskId): Promise<void> {
  if (inflight.has(id)) {
    inflight.set(id, true); // re-read once more when the current read finishes
    return;
  }
  inflight.set(id, false);
  try {
    do {
      inflight.set(id, false);
      const detail = await commands.agentsTaskDetail(id);
      useAgents.setState((state) => mergeDetail(state, detail));
    } while (inflight.get(id));
  } catch (e) {
    useAgents.setState({ error: errorMessage(e) });
  } finally {
    inflight.delete(id);
  }
}

let initialized: Promise<void> | null = null;

function handleEvent(event: AgentEvent) {
  const { patch, refetch: id } = reduceEvent(useAgents.getState(), event);
  if (Object.keys(patch).length) useAgents.setState(patch);
  if (id && (useAgents.getState().details[id] || useAgents.getState().selected === id)) void refetch(id);
}

async function resync() {
  const [status, tasks] = await Promise.all([commands.agentsProviderStatus(), commands.agentsTaskList()]);
  useAgents.setState({ status, tasks });
  const selected = useAgents.getState().selected;
  if (selected) await refetch(selected);
}

export const agents = {
  /** Load state and subscribe to events (once). */
  init(): Promise<void> {
    initialized ??= (async () => {
      await events.agents(handleEvent);
      await events.agentsResync(() => void resync().catch((e: unknown) => useAgents.setState({ error: errorMessage(e) })));
      await resync();
    })().catch((e: unknown) => {
      initialized = null;
      useAgents.setState({ error: errorMessage(e) });
    });
    return initialized;
  },

  show(view: PanelView, selected?: TaskId | null) {
    useAgents.setState((state) => ({
      view,
      selected: selected === undefined ? state.selected : selected,
      error: null,
    }));
    if (selected) void refetch(selected);
  },

  setStatus(status: ProviderStatus) {
    useAgents.setState({ status });
  },

  openSettings(open: boolean) {
    useAgents.setState({ settingsOpen: open });
  },

  setError(error: string | null) {
    useAgents.setState({ error });
  },

  refetch,

  /** Run an IPC action, reporting failures in the panel. */
  async run<T>(action: () => Promise<T>): Promise<T | undefined> {
    try {
      useAgents.setState({ error: null });
      return await action();
    } catch (e) {
      useAgents.setState({ error: errorMessage(e) });
      return undefined;
    }
  },
};
