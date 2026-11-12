// Agent panel state. Rust owns tasks; this store mirrors summaries and the
// selected task's persisted transcript, plus the live streaming draft.
import { create } from "zustand";

import type { AgentEvent } from "../../bindings/AgentEvent";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskSummary } from "../../bindings/TaskSummary";
import type { WorkspaceInfo } from "../../bindings/WorkspaceInfo";
import type { DocumentEvent } from "../../bindings/DocumentEvent";
import { layout, useLayout } from "../../app/layout";
import type { Notification } from "../../app/notifications";
import { dismiss, notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import { commands, errorMessage, events } from "../../lib/ipc";
import { isActive, statusLabel } from "./format";

/** The Agent view (auxiliary region). */
export const AGENT_VIEW_ID = "agents.panel";

export type PanelView = "tasks" | "compose" | "task" | "review";

/** Output streaming for the current turn, before it is persisted. */
export interface StreamDraft {
  turn: number;
  text: string;
  thinking: string;
  /** Name of the tool that is currently running, if any. */
  activeTool: string | null;
  /** The streamed output has been persisted; drop it on the next detail read. */
  persisted?: boolean;
}

export interface AgentsState {
  status: ProviderStatus | null;
  /** The open project folder; agent tasks act only on it. */
  workspace: WorkspaceInfo | null;
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
  workspace: null,
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
  /** Announce a finished task (the user may not be looking at the panel). */
  notification?: Omit<Notification, "id"> & { id: string };
  /** Notifications that no longer apply (e.g. an approval that was answered). */
  dismiss?: string[];
}

/** Notification id for a task's pending approval (a task waits for one at a time). */
export const approvalNotificationId = (taskId: TaskId) => `agents-approval-${taskId}`;

/**
 * A persistent notification for an approval the task is blocked on, with an
 * action that opens it. Retired when the approval is answered.
 */
export function approvalNotification(
  taskId: TaskId,
  what: string | null,
  task: string | undefined,
): NonNullable<EventEffect["notification"]> {
  return {
    id: approvalNotificationId(taskId),
    tone: "warning",
    title: "Agent needs your approval",
    body: [what, task ? `Task: ${task}` : null].filter(Boolean).join(" · ") || undefined,
    actions: [{ label: "Open approval", run: () => openApproval(taskId) }],
    // The task waits until the user answers; never time out.
    timeoutMs: null,
  };
}

/** Notification for a task that just stopped running; `undefined` otherwise. */
export function finishedNotification(previous: TaskSummary | undefined, task: TaskSummary): EventEffect["notification"] {
  if (!previous || !isActive(previous.status) || isActive(task.status)) return undefined;
  const { label } = statusLabel(task.status);
  const id = `agents-${task.id}`;
  const review = task.pendingReview > 0 ? ` ${task.pendingReview} file${task.pendingReview === 1 ? "" : "s"} to review.` : "";
  switch (task.status.kind) {
    case "completed":
      return { id, tone: "success", title: `${label}: ${task.title}`, body: review.trim() || undefined };
    case "canceled":
      return undefined;
    case "budgetExceeded":
      return { id, tone: "warning", title: `${label}: ${task.title}`, body: task.status.detail + review };
    case "failed":
      return { id, tone: "danger", title: `${label}: ${task.title}`, body: task.status.message };
    case "stopped":
      return { id, tone: "warning", title: `${label}: ${task.title}`, body: task.status.message + review };
    case "refused":
      return {
        id,
        tone: "danger",
        title: `${label}: ${task.title}`,
        body: task.status.explanation ?? "The model provider declined this request.",
      };
    default:
      return { id, tone: "warning", title: `${label}: ${task.title}` };
  }
}

/** Pure reducer for `agents:event` (exported for tests). */
export function reduceEvent(state: AgentsState, event: AgentEvent): EventEffect {
  switch (event.kind) {
    case "taskChanged": {
      const detail = state.details[event.task.id];
      const previous = state.tasks.find((t) => t.id === event.task.id);
      return {
        patch: {
          tasks: upsertTask(state.tasks, event.task),
          ...(detail ? { details: { ...state.details, [event.task.id]: { ...detail, summary: event.task } } } : {}),
        },
        refetch: null,
        notification: finishedNotification(previous, event.task),
        // A task that is no longer waiting (finished, canceled) has no approval to answer.
        ...(event.task.status.kind !== "awaitingApproval" && !isActive(event.task.status)
          ? { dismiss: [approvalNotificationId(event.task.id)] }
          : {}),
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
        patch: draft ? { drafts: { ...state.drafts, [event.taskId]: { ...draft, activeTool, persisted: true } } } : {},
        refetch: event.taskId,
      };
    }
    case "transcriptChanged": {
      // Emitted after the streamed message is stored (never mid-stream).
      const draft = state.drafts[event.taskId];
      return {
        patch: draft ? { drafts: { ...state.drafts, [event.taskId]: { ...draft, persisted: true } } } : {},
        refetch: event.taskId,
      };
    }
    case "approvalRequested": {
      const { taskId, title } = event.approval;
      const task = state.tasks.find((t) => t.id === taskId)?.title;
      return { patch: {}, refetch: taskId, notification: approvalNotification(taskId, title, task) };
    }
    case "approvalResolved":
      return { patch: {}, refetch: event.taskId, dismiss: [approvalNotificationId(event.taskId)] };
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
 * Merge a freshly read detail. Once the streamed output has been persisted the
 * transcript contains it, so the draft text is dropped (a running tool name is
 * kept); a draft that is still streaming is left alone.
 */
export function mergeDetail(state: AgentsState, detail: TaskDetail): Partial<AgentsState> {
  const id = detail.summary.id;
  const draft = state.drafts[id];
  const drafts = { ...state.drafts };
  if (draft) {
    const running = detail.summary.status.kind === "running" || detail.summary.status.kind === "awaitingApproval";
    if (!running) delete drafts[id];
    else if (draft.persisted) drafts[id] = { ...draft, text: "", thinking: "" };
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

/** Whether the Agent view is on screen (its region visible and the view active). */
function agentViewShowing(): boolean {
  if (!layout.isVisible("auxiliary")) return false;
  const active = useLayout.getState().auxiliary.activeView ?? registry.viewsAt("auxiliary")[0]?.id;
  return active === AGENT_VIEW_ID;
}

/** Whether the task's approval card is on screen right now. */
function approvalShowing(taskId: TaskId): boolean {
  const { view, selected } = useAgents.getState();
  return agentViewShowing() && view === "task" && selected === taskId;
}

/**
 * Show the Agent view on the task and move focus to its approval card, so a
 * keyboard or screen-reader user lands on the decision (the card's label
 * names the action; Approve and Decline are the next Tab stops).
 */
export function openApproval(taskId: TaskId): void {
  layout.showView("auxiliary", AGENT_VIEW_ID);
  agents.show("task", taskId);
  let frames = 0;
  const focus = () => {
    const card = document.querySelector<HTMLElement>("[data-testid=agents-panel] [data-testid=agents-approval]");
    if (!card) {
      if (++frames < 120) requestAnimationFrame(focus);
      return;
    }
    if (!card.hasAttribute("tabindex")) card.setAttribute("tabindex", "-1");
    card.scrollIntoView({ block: "nearest" });
    card.focus();
  };
  requestAnimationFrame(focus);
}

function handleEvent(event: AgentEvent) {
  const { patch, refetch: id, notification, dismiss: retired } = reduceEvent(useAgents.getState(), event);
  if (Object.keys(patch).length) useAgents.setState(patch);
  for (const old of retired ?? []) dismiss(old);
  // An approval the user is already looking at needs no notification.
  const shown = event.kind === "approvalRequested" && approvalShowing(event.approval.taskId);
  if (notification && !shown) notify(notification);
  if (id && (useAgents.getState().details[id] || useAgents.getState().selected === id)) void refetch(id);
}

/** Bring approval notifications in line with the tasks (after a resync). */
function syncApprovalNotifications(tasks: TaskSummary[]) {
  for (const task of tasks) {
    if (task.status.kind !== "awaitingApproval") dismiss(approvalNotificationId(task.id));
    else if (!approvalShowing(task.id)) notify(approvalNotification(task.id, null, task.title));
  }
}

async function resync() {
  const [status, tasks, workspace] = await Promise.all([
    commands.agentsProviderStatus(),
    commands.agentsTaskList(),
    commands.workspaceCurrent(),
  ]);
  useAgents.setState({ status, tasks, workspace });
  syncApprovalNotifications(tasks);
  const selected = useAgents.getState().selected;
  if (selected) await refetch(selected);
}

/** Whether a document event can change what a task's review shows. */
export function reviewAffectedBy(event: DocumentEvent): boolean {
  if (event.kind === "closed") return true;
  return event.kind === "changed" && event.origin.kind !== "agent";
}

/**
 * Review state follows file content (an undo can remove an agent change), so
 * after edits that are not the agent's, re-read open reviews and the tasks'
 * pending-review counts. Debounced: typing produces many events.
 */
let reviewRefresh: ReturnType<typeof setTimeout> | null = null;

function handleDocumentEvent(event: DocumentEvent) {
  if (!reviewAffectedBy(event)) return;
  if (!useAgents.getState().tasks.some((t) => t.changedFiles > 0 && !isActive(t.status))) return;
  if (reviewRefresh) clearTimeout(reviewRefresh);
  reviewRefresh = setTimeout(() => {
    reviewRefresh = null;
    useAgents.setState((state) => {
      const reviewVersions = { ...state.reviewVersions };
      for (const task of state.tasks) {
        if (task.changedFiles > 0) reviewVersions[task.id] = (reviewVersions[task.id] ?? 0) + 1;
      }
      return { reviewVersions };
    });
    void commands.agentsTaskList().then(
      (tasks) => useAgents.setState({ tasks }),
      () => {},
    );
  }, 400);
}

export const agents = {
  /** Load state and subscribe to events (once). */
  init(): Promise<void> {
    initialized ??= (async () => {
      await events.agents(handleEvent);
      await events.agentsResync(() => void resync().catch((e: unknown) => useAgents.setState({ error: errorMessage(e) })));
      await events.workspace((event) => {
        if (event.kind === "opened") useAgents.setState({ workspace: event.info });
        if (event.kind === "closed") useAgents.setState({ workspace: null });
      });
      await events.documents(handleDocumentEvent);
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
    // The task's approval card is on screen now; its notification has done its job.
    if (view === "task" && selected && agentViewShowing()) dismiss(approvalNotificationId(selected));
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
