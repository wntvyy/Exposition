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
import type { PermissionsChange } from "../../bindings/PermissionsChange";
import { layout, useLayout } from "../../app/layout";
import { EDITOR_PAGE, pages } from "../../app/pages";
import type { Notification } from "../../app/notifications";
import { dismiss, notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import { commands, errorMessage, events } from "../../lib/ipc";
import { onProjectOp } from "../operations/client";
import { asSentence, isActive, statusLabel } from "./format";

/** The Agent view (auxiliary region). */
export const AGENT_VIEW_ID = "agents.panel";
/** The Agents page (top-level page): detailed task and workflow management. */
export const AGENTS_PAGE_ID = "agents";

export type PanelView = "tasks" | "compose" | "task" | "review";

/**
 * The Agents page's right panel beside the conversation: `run` = none (the
 * conversation alone), else the task's Graph context, Proposals, or Changes.
 */
export type PageTab = "run" | "graph" | "proposals" | "changes";

/** Which tasks the Agents page lists. */
export type TaskFilter = "all" | "active" | "attention" | "kept";

/**
 * The Agents page's own navigation. Tasks, details, drafts, and approvals are
 * shared with the Agent view: the page and the panel are two views of the same
 * service and store, so navigating between them never duplicates a task or
 * loses an approval or review.
 */
export interface PageState {
  selected: TaskId | null;
  tab: PageTab;
  filter: TaskFilter;
  composing: boolean;
}

/** Output streaming for the current turn, before it is persisted. */
export interface StreamDraft {
  turn: number;
  text: string;
  thinking: string;
  /** Name of the tool the agent is on, if any. */
  activeTool: string | null;
  /** Whether that tool runs or waits for the user's approval or answer (default running). */
  activeToolStatus?: "running" | "awaitingApproval" | "awaitingAnswer";
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
  error: string | null;
  page: PageState;
  /** Incremented when any project's permissions change (views re-read them). */
  permissionsVersion: number;
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
  error: null,
  page: { selected: null, tab: "run", filter: "all", composing: false },
  permissionsVersion: 0,
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

/** Notification id for a task's open structured question (one at a time). */
export const questionNotificationId = (taskId: TaskId) => `agents-question-${taskId}`;

/**
 * A persistent notification for a structured question the task waits on,
 * with an action that opens it. Retired when the question is resolved.
 */
export function questionNotification(
  taskId: TaskId,
  prompt: string | null,
  task: string | undefined,
): NonNullable<EventEffect["notification"]> {
  return {
    id: questionNotificationId(taskId),
    tone: "warning",
    title: "Agent needs your answer",
    body: [prompt, task ? `Task: ${task}` : null].filter(Boolean).join(" · ") || undefined,
    actions: [{ label: "Open question", run: () => openQuestion(taskId) }],
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
      return { id, tone: "warning", title: `${label}: ${task.title}`, body: asSentence(task.status.detail) + review };
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
        // A task that is no longer waiting (finished, canceled) has no approval or question to answer.
        ...(event.task.status.kind !== "awaitingApproval" && !isActive(event.task.status)
          ? { dismiss: [approvalNotificationId(event.task.id), questionNotificationId(event.task.id)] }
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
        patch: draft
          ? {
              drafts: {
                ...state.drafts,
                [event.taskId]: { ...draft, activeTool, activeToolStatus: "running", persisted: true },
              },
            }
          : {},
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
      const { taskId, title, tool } = event.approval;
      const task = state.tasks.find((t) => t.id === taskId)?.title;
      // The tool has not started: it waits for the user.
      const draft = state.drafts[taskId];
      const waiting = draft ? { ...draft, activeTool: tool, activeToolStatus: "awaitingApproval" as const } : null;
      return {
        patch: waiting ? { drafts: { ...state.drafts, [taskId]: waiting } } : {},
        refetch: taskId,
        notification: approvalNotification(taskId, title, task),
      };
    }
    case "approvalResolved": {
      // Approved: it runs now. Declined (or canceled): it will not run.
      const draft = state.drafts[event.taskId];
      const next = draft
        ? event.approved
          ? { ...draft, activeToolStatus: "running" as const }
          : { ...draft, activeTool: null, activeToolStatus: undefined }
        : null;
      return {
        patch: next ? { drafts: { ...state.drafts, [event.taskId]: next } } : {},
        refetch: event.taskId,
        dismiss: [approvalNotificationId(event.taskId)],
      };
    }
    // Structured questions live in the task detail: re-read it.
    case "questionRequested": {
      const { taskId, questions, state: requestState } = event.request;
      const open = requestState.kind === "pending" || requestState.kind === "discussing";
      const task = state.tasks.find((t) => t.id === taskId)?.title;
      // ask_user has not returned: it waits for the user's answer.
      const draft = state.drafts[taskId];
      const waiting = open && draft ? { ...draft, activeToolStatus: "awaitingAnswer" as const } : null;
      return {
        patch: waiting ? { drafts: { ...state.drafts, [taskId]: waiting } } : {},
        refetch: taskId,
        ...(open ? { notification: questionNotification(taskId, questions[0]?.prompt ?? null, task) } : {}),
      };
    }
    case "questionChanged":
      return { patch: {}, refetch: event.taskId };
    case "questionResolved": {
      const draft = state.drafts[event.taskId];
      const next = draft?.activeToolStatus === "awaitingAnswer" ? { ...draft, activeToolStatus: "running" as const } : null;
      return {
        patch: next ? { drafts: { ...state.drafts, [event.taskId]: next } } : {},
        refetch: event.taskId,
        dismiss: [questionNotificationId(event.taskId)],
      };
    }
    case "providerChanged":
      return { patch: { status: event.status }, refetch: null };
    case "reviewChanged":
      return {
        patch: {
          reviewVersions: { ...state.reviewVersions, [event.taskId]: (state.reviewVersions[event.taskId] ?? 0) + 1 },
        },
        refetch: null,
      };
    case "fallback":
      return { patch: {}, refetch: null };
    case "permissionsChanged":
      return {
        patch: { permissionsVersion: state.permissionsVersion + 1 },
        refetch: null,
        notification: permissionsNotification(event.change),
      };
  }
}

/**
 * After a narrowing: which Auto tasks stopped, and any cleanup Exposition
 * could not confirm (never a claim that finished work was undone).
 */
export function permissionsNotification(change: PermissionsChange): EventEffect["notification"] {
  if (change.stopped.length === 0 && change.problems.length === 0) return undefined;
  const n = change.stopped.length;
  const stopped = n > 0 ? `${n} Auto task${n === 1 ? "" : "s"} stopped; what they did stays reviewable.` : null;
  return {
    id: `agents-permissions-${change.root}`,
    tone: change.problems.length > 0 ? "warning" : "info",
    title: change.permissions ? "Project permissions changed" : "Project permissions revoked",
    body: [stopped, ...change.problems].filter((part): part is string => part !== null).join(" "),
    timeoutMs: change.problems.length > 0 ? null : undefined,
  };
}

const FINISHED_CALLS = new Set(["succeeded", "failed", "declined", "canceled", "notRun"]);

/**
 * The sequence number from which a held detail may be out of date, for a
 * partial re-read: stored rows before the latest assistant message never
 * change, except a tool call that has not finished (a question that waits
 * while a discussion follows it). `null`: nothing to build on, read it all.
 */
export function changedFrom(detail: TaskDetail): number | null {
  let from: number | null = null;
  for (const item of detail.transcript) {
    if (item.kind === "assistantText" || item.kind === "thinking" || item.kind === "toolCall") from = item.seq;
  }
  if (from === null) return null;
  for (const item of detail.transcript) {
    if (item.kind === "toolCall" && !FINISHED_CALLS.has(item.status)) return Math.min(from, item.seq);
  }
  return from;
}

/**
 * A partial read (`transcriptFrom` set) completed with the detail it extends:
 * the held items before that sequence number, then the items read, and the
 * held graph context (a partial read leaves it out). `null` when it cannot
 * be completed: no detail is held, or a new run attached a new context.
 */
export function completeDetail(held: TaskDetail | undefined, read: TaskDetail): TaskDetail | null {
  const from = read.transcriptFrom;
  if (from == null) return read;
  if (!held || held.graph.contexts !== read.graph.contexts) return null;
  const complete: TaskDetail = {
    ...read,
    transcript: [...held.transcript.filter((item) => item.seq < from), ...read.transcript],
    graph: { ...read.graph, context: held.graph.context },
  };
  delete complete.transcriptFrom;
  return complete;
}

/**
 * Merge a freshly read (complete) detail. Once the streamed output has been persisted the
 * transcript contains it, so the draft text is dropped (a running tool name is
 * kept); a draft that is still streaming is left alone.
 */
export function mergeDetail(state: AgentsState, detail: TaskDetail): Partial<AgentsState> {
  const id = detail.summary.id;
  const draft = state.drafts[id];
  const drafts = { ...state.drafts };
  if (draft) {
    // A discussion turn ("Chat about it") streams while the task waits for the answer.
    const running =
      detail.summary.status.kind === "running" ||
      detail.summary.status.kind === "awaitingApproval" ||
      detail.summary.status.kind === "awaitingAnswer";
    if (!running) delete drafts[id];
    else if (draft.persisted) drafts[id] = { ...draft, text: "", thinking: "" };
  }
  return { details: { ...state.details, [id]: detail }, drafts, tasks: upsertTask(state.tasks, detail.summary) };
}

/** Per task with a read in flight: the read to make once it finishes, if any. */
const inflight = new Map<TaskId, "tail" | "all" | null>();

/** Read the whole detail, or (`tail`) only what may have changed in the one held. */
async function readDetail(id: TaskId, tail: boolean): Promise<TaskDetail> {
  const held = useAgents.getState().details[id];
  const from = tail && held ? changedFrom(held) : null;
  if (from === null) return commands.agentsTaskDetail(id);
  const read = await commands.agentsTaskDetail(id, from);
  // Completed only with the very transcript the read was made for.
  const now = useAgents.getState().details[id];
  const complete = now?.transcript === held?.transcript ? completeDetail(now, read) : null;
  return complete ?? commands.agentsTaskDetail(id);
}

/**
 * Re-read a task's detail. `tail`: a transcript event of a running task, so
 * only the end of the conversation is read; everything else (showing a task,
 * a resync, a question's refusal) reads it all.
 */
async function refetch(id: TaskId, tail = false): Promise<void> {
  const queued = inflight.get(id);
  if (queued !== undefined) {
    // Re-read once more when the current read finishes (all of it if anyone asked for all).
    inflight.set(id, queued === "all" || !tail ? "all" : "tail");
    return;
  }
  let next: "tail" | "all" | null = tail ? "tail" : "all";
  inflight.set(id, null);
  try {
    do {
      inflight.set(id, null);
      const detail = await readDetail(id, next === "tail");
      // The read can show the task finished before its `taskChanged` arrives
      // (or when that event was missed): announce it from here as well. The
      // later event then finds the task already finished and adds nothing.
      const previous = useAgents.getState().tasks.find((t) => t.id === id);
      useAgents.setState((state) => mergeDetail(state, detail));
      const finished = finishedNotification(previous, detail.summary);
      if (finished) notify(finished);
      next = inflight.get(id) ?? null;
    } while (next);
  } catch (e) {
    useAgents.setState({ error: errorMessage(e) });
  } finally {
    inflight.delete(id);
  }
}

let initialized: Promise<void> | null = null;

/**
 * Whether the Agent view is on screen: the Editor page (the only page with
 * the auxiliary region; it stays mounted but hidden behind other pages) is
 * the page shown, the region is visible, and the view is the active one.
 */
function agentViewShowing(): boolean {
  if (pages.active() !== EDITOR_PAGE || !layout.isVisible("auxiliary")) return false;
  const active = useLayout.getState().auxiliary.activeView ?? registry.viewsAt("auxiliary")[0]?.id;
  return active === AGENT_VIEW_ID;
}

/** Whether the Agents page is the page on screen. */
function agentsPageShowing(): boolean {
  return pages.active() === AGENTS_PAGE_ID;
}

/** Whether the task's approval card is on screen right now (panel or page). */
function approvalShowing(taskId: TaskId): boolean {
  const { view, selected, page } = useAgents.getState();
  // The page shows the conversation beside its right panel (Changes, Graph context, Proposals).
  if (agentsPageShowing()) return page.selected === taskId && !page.composing;
  return agentViewShowing() && view === "task" && selected === taskId;
}

/** Bring the Agent view on screen: the Editor page it lives on, its region, and the view. */
export function showAgentView(): void {
  pages.show(EDITOR_PAGE);
  layout.showView("auxiliary", AGENT_VIEW_ID);
}

function focusApprovalIn(container: string, selector = "[data-testid=agents-approval]") {
  let frames = 0;
  const focus = () => {
    const card = document.querySelector<HTMLElement>(`[data-testid=${container}] ${selector}`);
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

/**
 * Show the Agent view on the task and move focus to its approval card, so a
 * keyboard or screen-reader user lands on the decision (the card's label
 * names the action; Approve and Decline are the next Tab stops).
 */
export function openApproval(taskId: TaskId): void {
  // On the Agents page the approval opens there; elsewhere in the Agent view.
  if (agentsPageShowing()) {
    agents.showInPage(taskId, "run");
    focusApprovalIn("agents-page");
    return;
  }
  showAgentView();
  agents.show("task", taskId);
  focusApprovalIn("agents-panel");
}

/** Selector of a task's open question card (the first one when several are open). */
const OPEN_QUESTION = "[data-testid=agents-question][data-open=true]";

/**
 * Show the task where the user works (Agents page or Agent view) and move
 * focus to its open question card (labelled by the question; the options
 * are the next Tab stop).
 */
export function openQuestion(taskId: TaskId): void {
  if (agentsPageShowing()) {
    agents.showInPage(taskId, "run");
    focusApprovalIn("agents-page", OPEN_QUESTION);
    return;
  }
  showAgentView();
  agents.show("task", taskId);
  focusApprovalIn("agents-panel", OPEN_QUESTION);
}

function handleEvent(event: AgentEvent) {
  const { patch, refetch: id, notification, dismiss: retired } = reduceEvent(useAgents.getState(), event);
  if (Object.keys(patch).length) useAgents.setState(patch);
  for (const old of retired ?? []) dismiss(old);
  // An approval or question the user is already looking at needs no notification.
  const shown =
    (event.kind === "approvalRequested" && approvalShowing(event.approval.taskId)) ||
    (event.kind === "questionRequested" && approvalShowing(event.request.taskId));
  if (notification && !shown) notify(notification);
  if (id && (useAgents.getState().details[id] || useAgents.getState().selected === id)) void refetch(id, true);
}

/** Bring approval notifications in line with the tasks (after a resync). */
function syncApprovalNotifications(tasks: TaskSummary[]) {
  for (const task of tasks) {
    if (task.status.kind !== "awaitingApproval") dismiss(approvalNotificationId(task.id));
    else if (!approvalShowing(task.id)) notify(approvalNotification(task.id, null, task.title));
    if (task.status.kind !== "awaitingAnswer") dismiss(questionNotificationId(task.id));
    else if (!approvalShowing(task.id)) notify(questionNotification(task.id, null, task.title));
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
  // Every conversation that is loaded may have missed events: the Agent
  // view's, the Agents page's, and any other read earlier (a hidden surface
  // shows its detail again without re-reading it).
  const { selected, page, details } = useAgents.getState();
  const known = new Set(tasks.map((t) => t.id));
  const loaded = new Set([selected, page.selected, ...Object.keys(details)].filter((id): id is TaskId => !!id && known.has(id)));
  await Promise.all([...loaded].map((id) => refetch(id)));
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
      // Deleted or restored task records (Agents lists, Storage & History).
      onProjectOp((result) => {
        if (!result.outcomes.some((o) => o.target.kind === "agentTask" || o.created?.kind === "agentTask")) return;
        void commands.agentsTaskList().then(
          (tasks) => {
            const ids = new Set(tasks.map((t) => t.id));
            useAgents.setState((state) => ({
              tasks,
              selected: state.selected && !ids.has(state.selected) ? null : state.selected,
              view: state.selected && !ids.has(state.selected) && (state.view === "task" || state.view === "review") ? "tasks" : state.view,
              page: state.page.selected && !ids.has(state.page.selected) ? { ...state.page, selected: null } : state.page,
            }));
          },
          () => {},
        );
      });
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
    // The task's approval or question card is on screen now; its notification has done its job.
    if (view === "task" && selected && agentViewShowing()) {
      dismiss(approvalNotificationId(selected));
      dismiss(questionNotificationId(selected));
    }
  },

  /** Show a task on the Agents page (the same task the Agent view shows). */
  showInPage(taskId: TaskId | null, tab?: PageTab) {
    pages.show(AGENTS_PAGE_ID);
    useAgents.setState((state) => ({
      page: { ...state.page, selected: taskId, tab: tab ?? (taskId === state.page.selected ? state.page.tab : "run"), composing: false },
      error: null,
    }));
    if (taskId) void refetch(taskId);
    if (taskId) dismiss(approvalNotificationId(taskId));
    if (taskId) dismiss(questionNotificationId(taskId));
  },

  /** Change the Agents page's navigation without leaving the current page. */
  setPage(patch: Partial<PageState>) {
    useAgents.setState((state) => ({ page: { ...state.page, ...patch } }));
    const selected = patch.selected;
    if (selected) void refetch(selected);
    // An approval now on screen on the page needs no notification.
    const { page } = useAgents.getState();
    if (page.selected && approvalShowing(page.selected)) {
      dismiss(approvalNotificationId(page.selected));
      dismiss(questionNotificationId(page.selected));
    }
  },

  setStatus(status: ProviderStatus) {
    useAgents.setState({ status });
  },

  /**
   * Open Settings → Model providers, the only place providers and keys are
   * configured. Drafts live in the drafts store and survive the trip.
   */
  openSettings(open = true) {
    if (open && registry.command("agents.providerSettings")) void registry.runCommand("agents.providerSettings");
  },

  setError(error: string | null) {
    useAgents.setState({ error });
  },

  /** Re-read a task's whole detail. */
  refetch: (id: TaskId) => refetch(id),

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
