// A structured question from the agent ("Needs your answer"), inline at the
// `ask_user` tool call it belongs to: up to three questions, each an
// OptionList (nothing preselected; at most one Recommended) with an optional
// "Other answer…", one Send answer for the whole group, and Chat about it.
//
// The frame is the approval card's: a shared info Banner ("Needs your
// answer") with the questions and actions beneath it; an answered or
// superseded request is a quiet record without controls.
//
// Wiring:
// - Transcript: `QuestionSlot` (slots.tsx) renders the card at the `ask_user`
//   call (`questionsForTask(detail).get(item.toolUseId)`).
// - "Needs you" (NeedsYou.tsx) lists `pendingQuestions(detail)` with
//   `needsYouLabel(request)` and jumps with `jumpToQuestion`. The card is
//   `[data-question=<request id>]`, `id={questionAnchorId(request.id)}`,
//   focusable (tabIndex -1), labelled by its first prompt.
// - Composer (TaskComposer.tsx): Chat about it calls
//   `drafts.chatAbout(<draft key>, request)`; the composer shows the About
//   chip, focuses its field when `about.seq` changes, and sends through
//   `sendDiscussion`. While a question waits, Send takes only a discussion.
// - Status, notifications, and "Open Pending Agent Question" are wired in
//   store.ts / register.ts (`openQuestion(taskId)` focuses the open card).
//
// The answer grants nothing: it is user content, and the central gate still
// evaluates every later action.
import { MessageSquare, Send } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

import type { Question } from "../../bindings/Question";
import type { QuestionAnswer } from "../../bindings/QuestionAnswer";
import type { QuestionRequest } from "../../bindings/QuestionRequest";
import type { TaskId } from "../../bindings/TaskId";
import { Badge, Banner, Button, OptionList, StatusText, TextField } from "../../ui/components";
import {
  type AnswerResult,
  answerQuestion,
  isOpenQuestion,
  newOptionKeys,
  noteRevision,
  questionAnchorId,
  useAnswerInFlight,
  useChatAbout,
} from "./questions";

/** The backend's limit for a custom answer. */
export const CUSTOM_ANSWER_MAX = 2000;

export interface QuestionCardProps {
  task: TaskId;
  request: QuestionRequest;
  /** The composer draft Chat about it marks (default: the task's draft, its id). */
  draftKey?: string;
  /** Replaces the default Chat about it (`drafts.chatAbout(draftKey, request)`). */
  onChatAbout?: (request: QuestionRequest) => void;
}

/** The answer to one question from the current choices, or null when it has none. */
export function answerFor(question: Question, choice: string | null | undefined, custom: string | undefined): QuestionAnswer | null {
  const text = (custom ?? "").trim();
  if (question.allowCustom && text) return { questionId: question.id, value: { kind: "custom", text } };
  if (choice && question.options.some((o) => o.id === choice)) {
    return { questionId: question.id, value: { kind: "option", optionId: choice } };
  }
  return null;
}

/** What was answered, for the resolved card: the option label or the custom text. */
export function answeredText(question: Question, answer: QuestionAnswer | undefined): string {
  if (!answer) return "—";
  if (answer.value.kind === "custom") return answer.value.text;
  const { optionId } = answer.value;
  return question.options.find((o) => o.id === optionId)?.label ?? optionId;
}

/**
 * "The task was canceled" → "the task was canceled" after "No longer needed — ";
 * names ("Exposition restarted") keep their capital.
 */
export function continueSentence(reason: string): string {
  return /^(The|A|An|This|That|It) /.test(reason) ? reason.charAt(0).toLowerCase() + reason.slice(1) : reason;
}

export function QuestionCard({ task, request, draftKey, onChatAbout }: QuestionCardProps) {
  const defaultChatAbout = useChatAbout(draftKey ?? task);
  const chatAbout = onChatAbout ?? defaultChatAbout;
  const inFlight = useAnswerInFlight(request.id);
  const [choices, setChoices] = useState<Record<string, string | null>>({});
  const [custom, setCustom] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<Extract<AnswerResult, { ok: false }> | null>(null);
  const [sent, setSent] = useState(false);
  // Synchronous guard: a second press arrives before React re-renders.
  const sending = useRef(false);

  // Remember this revision's options so a later revision can mark what is new.
  noteRevision(request);
  const added = newOptionKeys(request);

  // A revision keeps option ids that did not change: keep those choices, drop
  // the others. A new revision or state can be answered again.
  const optionsKey = request.questions.map((q) => `${q.id}:${q.options.map((o) => o.id).join(",")}`).join("|");
  useEffect(() => {
    setChoices((current) => {
      const next: Record<string, string | null> = {};
      for (const q of request.questions) {
        const id = current[q.id];
        if (id && q.options.some((o) => o.id === id)) next[q.id] = id;
      }
      return next;
    });
    setSent(false);
    // The questions are read through `optionsKey` (a re-read detail is a new array).
  }, [request.revision, optionsKey, request.state.kind]);

  const answers = useMemo(
    () => request.questions.map((q) => answerFor(q, choices[q.id], custom[q.id])),
    [request.questions, choices, custom],
  );
  const answeredCount = answers.filter(Boolean).length;
  const ready = request.questions.length > 0 && answeredCount === request.questions.length;
  const open = isOpenQuestion(request);
  const discussing = request.state.kind === "discussing";
  const group = request.questions.length > 1;
  const firstPrompt = request.questions[0]?.prompt ?? "";

  async function send() {
    if (sending.current || inFlight || sent || !ready) return;
    const complete = answers.filter((a): a is QuestionAnswer => a !== null);
    sending.current = true;
    setFailure(null);
    try {
      const result = await answerQuestion(task, request, complete);
      if (result === null) return; // another view is sending this answer
      if (result.ok) setSent(true);
      else setFailure(result);
    } finally {
      sending.current = false;
    }
  }

  const choose = (questionId: string, optionId: string) => {
    setFailure(null);
    setChoices((c) => ({ ...c, [questionId]: optionId }));
    // One explicit answer per question: an option replaces the Other text.
    setCustom((c) => ({ ...c, [questionId]: "" }));
  };
  const type = (questionId: string, text: string) => {
    setFailure(null);
    setCustom((c) => ({ ...c, [questionId]: text }));
    if (text.trim()) setChoices((c) => ({ ...c, [questionId]: null }));
  };

  const answered = request.state.kind === "answered" ? request.state.answers : null;

  return (
    <section
      className={open ? "agents-stack agents-question" : "agents-stack agents-stack--tight agents-question"}
      id={questionAnchorId(request.id)}
      data-testid="agents-question"
      data-question={request.id}
      data-state={request.state.kind}
      data-open={open ? "true" : "false"}
      data-revision={request.revision}
      tabIndex={-1}
      aria-label={firstPrompt}
    >
      {open && (
        <Banner tone="info" title="Needs your answer">
          The agent waits for your answer. It is passed on as your reply and approves nothing.
        </Banner>
      )}
      {open && (group || discussing) && (
        <div className="agents-row">
          {group && (
            <span className="ui-meta">
              {answeredCount} of {request.questions.length} answered
            </span>
          )}
          {discussing && <StatusText status="running" label="Discussing" data-testid="agents-question-discussing" />}
        </div>
      )}
      {request.questions.map((q) => (
        <div key={q.id} className="agents-stack agents-stack--tight" data-question-id={q.id}>
          {open ? (
            <>
              <OptionList<string>
                label={q.prompt}
                description={q.explanation ?? undefined}
                options={q.options.map((o) => ({
                  id: o.id,
                  label: o.label,
                  description: o.consequence ?? undefined,
                  recommended: o.recommended,
                  badge: added.has(`${q.id}/${o.id}`) ? (
                    <Badge tone="neutral" data-testid="agents-question-new">
                      New
                    </Badge>
                  ) : undefined,
                }))}
                value={choices[q.id] ?? null}
                onChange={(id) => choose(q.id, id)}
                isDisabled={sent}
              />
              {q.allowCustom && (
                <TextField
                  aria-label={`Other answer: ${q.prompt}`}
                  placeholder="Other answer…"
                  value={custom[q.id] ?? ""}
                  onChange={(text) => type(q.id, text)}
                  maxLength={CUSTOM_ANSWER_MAX}
                  isDisabled={sent}
                />
              )}
            </>
          ) : (
            <>
              <div className="ui-meta">{q.prompt}</div>
              {answered && (
                <StatusText
                  status="succeeded"
                  wrap
                  label={`Answered: ${answeredText(q, answered.find((a) => a.questionId === q.id))}`}
                  data-testid="agents-question-answered"
                />
              )}
            </>
          )}
        </div>
      ))}
      {request.state.kind === "superseded" && (
        <div className="ui-meta" data-testid="agents-question-superseded">
          No longer needed — {continueSentence(request.state.reason)}
        </div>
      )}
      {/* A late answer from this view stays explained after the card shows the answer that was used. */}
      {failure && (open || failure.code === "agents.questionResolved") && (
        <div role="alert" data-testid="agents-question-error" data-code={failure.code ?? ""}>
          <StatusText status="warning" wrap label={failure.message} />
        </div>
      )}
      {open && (
        <div className="agents-row">
          <Button
            size="sm"
            variant="primary"
            icon={Send}
            isDisabled={!ready || sent}
            isPending={inFlight}
            onPress={() => void send()}
            data-testid="agents-question-send"
          >
            Send answer
          </Button>
          <Button
            size="sm"
            variant="secondary"
            icon={MessageSquare}
            onPress={() => chatAbout(request)}
            data-testid="agents-question-chat"
          >
            Chat about it
          </Button>
          {sent && <StatusText status="succeeded" label="Answer sent" />}
        </div>
      )}
    </section>
  );
}
