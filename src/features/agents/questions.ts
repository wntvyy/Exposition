// Structured questions ("Needs your answer"): selectors over the task detail,
// the in-memory diff that marks options a discussion added, answering with an
// in-flight guard shared by every view, and "Chat about it" sending.
//
// The backend owns the request, its revision, and its state; events
// (questionRequested / questionChanged / questionResolved, store.ts) re-read
// the task detail, and everything here derives from that detail.
import { useCallback } from "react";
import { create } from "zustand";

import type { QuestionAnswer } from "../../bindings/QuestionAnswer";
import type { QuestionRequest } from "../../bindings/QuestionRequest";
import type { TaskDetail } from "../../bindings/TaskDetail";
import type { TaskId } from "../../bindings/TaskId";
import { commands, errorMessage } from "../../lib/ipc";
import { type DraftAbout, drafts } from "./drafts";
import { type AgentsState, agents, useAgents } from "./store";

// ---------------------------------------------------------------- selectors

/** Whether the request still waits for the user (pending or being discussed). */
export function isOpenQuestion(request: QuestionRequest): boolean {
  return request.state.kind === "pending" || request.state.kind === "discussing";
}

/**
 * The task's question requests by the `toolUseId` of the `ask_user` call
 * they render at, open requests first (then in the task's order).
 */
export function questionsForTask(detail: TaskDetail | null | undefined): Map<string, QuestionRequest> {
  const all = detail?.questions ?? [];
  const map = new Map<string, QuestionRequest>();
  for (const request of all) if (isOpenQuestion(request)) map.set(request.toolUseId, request);
  for (const request of all) if (!map.has(request.toolUseId)) map.set(request.toolUseId, request);
  return map;
}

/** Open requests in the task's order (the "Needs you" list). */
export function pendingQuestions(detail: TaskDetail | null | undefined): QuestionRequest[] {
  return (detail?.questions ?? []).filter(isOpenQuestion);
}

/** A prompt shortened for one-line lists (whole words, with an ellipsis). */
export function shortPrompt(prompt: string, max = 60): string {
  const text = prompt.replace(/\s+/g, " ").trim();
  if (text.length <= max) return text;
  const cut = text.slice(0, max - 1);
  const space = cut.lastIndexOf(" ");
  return `${(space > max / 2 ? cut.slice(0, space) : cut).replace(/[\s,;:.]+$/, "")}…`;
}

/** The "Needs you" entry for an open request: "Needs your answer: <short prompt>". */
export function needsYouLabel(request: QuestionRequest): string {
  return `Needs your answer: ${shortPrompt(request.questions[0]?.prompt ?? "")}`;
}

/** DOM id of a request's card (jump links); one per view, so scope lookups with `findQuestionCard`. */
export function questionAnchorId(requestId: string): string {
  return `agents-question-${requestId}`;
}

/**
 * The card of `requestId` in the same view as `from` (Agent view or Agents
 * page), else anywhere in the document.
 */
export function findQuestionCard(requestId: string, from?: Element | null): HTMLElement | null {
  const selector = `[data-question="${requestId.replace(/["\\]/g, "\\$&")}"]`;
  const view = from?.closest("[data-testid=agents-panel], [data-testid=agents-page]");
  return view?.querySelector<HTMLElement>(selector) ?? document.querySelector<HTMLElement>(selector);
}

/** Jump link target: scroll the card into view and focus it (the card is focusable). */
export function jumpToQuestion(requestId: string, from?: Element | null): boolean {
  const card = findQuestionCard(requestId, from);
  if (!card) return false;
  card.scrollIntoView({ block: "nearest" });
  card.focus();
  return true;
}

/** The request as the store has it now. */
export function currentRequest(state: AgentsState, task: TaskId, requestId: string): QuestionRequest | null {
  return state.details[task]?.questions?.find((r) => r.id === requestId) ?? null;
}

// ---------------------------------------------------------------- revisions

/** Option ids per question id at one revision. */
type OptionSet = Record<string, string[]>;

interface Seen {
  revision: number;
  options: OptionSet;
  /** `questionId/optionId` keys new at `revision` compared with the revision seen before. */
  added: Set<string>;
}

const seen = new Map<string, Seen>();
const NONE: ReadonlySet<string> = new Set();

function optionSet(request: QuestionRequest): OptionSet {
  return Object.fromEntries(request.questions.map((q) => [q.id, q.options.map((o) => o.id)]));
}

/**
 * Remember a request's options. When its revision is newer than the one seen
 * before, the options (and questions) that were not there are marked new.
 * The first revision seen in this session marks nothing (no earlier one known).
 */
export function noteRevision(request: QuestionRequest): void {
  const before = seen.get(request.id);
  if (before && before.revision >= request.revision) return;
  const options = optionSet(request);
  const added = new Set<string>();
  if (before) {
    for (const [question, ids] of Object.entries(options)) {
      const old = new Set(before.options[question] ?? []);
      for (const id of ids) if (!old.has(id)) added.add(`${question}/${id}`);
    }
  }
  seen.set(request.id, { revision: request.revision, options, added });
}

/** `questionId/optionId` keys a discussion added in the request's current revision. */
export function newOptionKeys(request: QuestionRequest): ReadonlySet<string> {
  const entry = seen.get(request.id);
  return entry && entry.revision === request.revision ? entry.added : NONE;
}

function noteDetails(details: AgentsState["details"]): void {
  for (const detail of Object.values(details)) for (const request of detail.questions ?? []) noteRevision(request);
}

// Follow every detail the store reads (both views share it), so the diff
// survives switching between the Agent view and the Agents page.
noteDetails(useAgents.getState().details);
useAgents.subscribe((state, previous) => {
  if (state.details !== previous.details) noteDetails(state.details);
});

/** Tests only. */
export function resetQuestionMemoryForTests(): void {
  seen.clear();
  useAnswering.setState({ requests: {} });
}

// ---------------------------------------------------------------- answering

/** Requests whose answer is being sent, in any view (the in-flight guard). */
export const useAnswering = create<{ requests: Record<string, true> }>(() => ({ requests: {} }));

export function useAnswerInFlight(requestId: string): boolean {
  return useAnswering((s) => Boolean(s.requests[requestId]));
}

function codeOf(error: unknown): string | null {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string" ? error.code : null;
}

export const QUESTION_STALE_TEXT = "The question changed; review the updated options.";
export const QUESTION_RESOLVED_TEXT = "This question is no longer open.";
export const QUESTION_BUSY_TEXT = "The agent is still replying about this question. Send again after its reply.";

/** What the user can do about a refused answer or discussion message. */
export function questionErrorText(error: unknown): string {
  switch (codeOf(error)) {
    case "agents.questionStale":
      return QUESTION_STALE_TEXT;
    case "agents.questionResolved":
      return QUESTION_RESOLVED_TEXT;
    case "agents.questionBusy":
      return QUESTION_BUSY_TEXT;
    default:
      return errorMessage(error);
  }
}

export type AnswerResult = { ok: true } | { ok: false; code: string | null; message: string };

/**
 * Send one answer per question at the request's revision. Returns `null`
 * without calling the backend while an answer for the same request is in
 * flight (double clicks, a second view); the backend is idempotent as well.
 * A stale or resolved refusal re-reads the task so the card shows the
 * current options or state.
 */
export async function answerQuestion(task: TaskId, request: QuestionRequest, answers: QuestionAnswer[]): Promise<AnswerResult | null> {
  if (useAnswering.getState().requests[request.id]) return null;
  useAnswering.setState((s) => ({ requests: { ...s.requests, [request.id]: true } }));
  try {
    await commands.agentsQuestionAnswer(task, request.id, request.revision, answers);
    void agents.refetch(task);
    return { ok: true };
  } catch (e) {
    const code = codeOf(e);
    if (code === "agents.questionStale" || code === "agents.questionResolved") void agents.refetch(task);
    return { ok: false, code, message: questionErrorText(e) };
  } finally {
    useAnswering.setState((s) => {
      const requests = { ...s.requests };
      delete requests[request.id];
      return { requests };
    });
  }
}

// ---------------------------------------------------------------- Chat about it

/**
 * The Chat about it action for a conversation: marks the draft as being
 * about the request (the composer shows the About chip and focuses its field
 * when `about.seq` changes). Submits nothing.
 */
export function useChatAbout(draftKey: string): (request: QuestionRequest) => void {
  return useCallback((request: QuestionRequest) => drafts.chatAbout(draftKey, request), [draftKey]);
}

/**
 * Send a discussion message about `about`'s request: one bounded turn in the
 * task's worker (`agents_question_discuss`) at the request's current
 * revision. A stale revision is re-read and sent once more (the options
 * changed, the message did not). On success the sent text and the About are
 * cleared from the draft (`drafts.completeDiscussion`; do not also call
 * `completeSend`). Attachments are not sent and stay in the draft. Throws an
 * Error with an actionable message on failure; a resolved or superseded
 * request also clears the About.
 */
export async function sendDiscussion(task: TaskId, about: DraftAbout, text: string, draftKey: string = task): Promise<void> {
  const lookup = () => currentRequest(useAgents.getState(), task, about.requestId);
  let request = lookup();
  if (!request) {
    await agents.refetch(task);
    request = lookup();
  }
  if (!request || !isOpenQuestion(request)) {
    drafts.clearAbout(draftKey);
    throw new Error(request?.state.kind === "answered" ? "This question was already answered." : "This question is no longer open.");
  }
  try {
    await commands.agentsQuestionDiscuss(task, request.id, request.revision, text);
  } catch (e) {
    const code = codeOf(e);
    if (code === "agents.questionStale") {
      await agents.refetch(task);
      const fresh = lookup();
      if (!fresh || fresh.revision === request.revision) throw new Error(QUESTION_STALE_TEXT);
      try {
        await commands.agentsQuestionDiscuss(task, fresh.id, fresh.revision, text);
      } catch (again) {
        throw new Error(questionErrorText(again));
      }
    } else {
      if (code === "agents.questionResolved") {
        drafts.clearAbout(draftKey);
        void agents.refetch(task);
      }
      throw new Error(questionErrorText(e));
    }
  }
  drafts.completeDiscussion(draftKey, text);
}
