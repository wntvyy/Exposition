// Transcript entries of an agent task: messages, reasoning summaries, tool calls, notices, and the streaming draft.
// Agent responses render through the shared Markdown renderer inside a ResponseSurface; tool payloads and
// results, reasoning summaries, and the user's own text stay plain text.
import "./transcript.css";

import { useCallback, useEffect, useRef, useState, type ReactNode, type RefObject } from "react";

import { EDITOR_PAGE, pages } from "../../app/pages";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import type { TranscriptItem } from "../../bindings/TranscriptItem";
import { commands, errorMessage } from "../../lib/ipc";
import { Markdown, ResponseSurface, StatusText, type CodeRunAction, type RunnableLanguage } from "../../ui/components";
import { MessageAttachments } from "./Attachments";
import { activeToolLabel, authorizationLabel, toolStatusLabel, toolSummary } from "./format";
import type { StreamDraft } from "./store";

// ---------------------------------------------------------------- follow the bottom

/** Within this many pixels of the end, the reader counts as at the bottom. */
const BOTTOM_SLACK = 32;

/**
 * Keep the transcript at its end while new output arrives, but only while
 * the reader is there: scrolling up stops following (earlier text stays
 * put) and scrolling back to the end resumes it. Content changes of any kind
 * (streamed text rendered a frame later, typeset math, highlighting, new
 * entries) are followed. A new `key` (another task) starts at the end.
 *
 * `activity` is a value that changes with new recorded or streamed output
 * (not with the reader's own expanding and collapsing). When it changes
 * while the reader is away from the end, `newActivity` turns on (the "New
 * activity" affordance) until they are back at the end; `toEnd` goes there.
 */
export function useFollowBottom(
  ref: RefObject<HTMLElement | null>,
  key: unknown,
  activity?: unknown,
): { newActivity: boolean; toEnd: () => void } {
  const [newActivity, setNewActivity] = useState(false);
  const atBottom = useRef(true);
  useEffect(() => {
    const log = ref.current;
    atBottom.current = true;
    setNewActivity(false);
    if (!log) return;
    const onScroll = () => {
      atBottom.current = log.scrollHeight - log.scrollTop - log.clientHeight <= BOTTOM_SLACK;
      if (atBottom.current) setNewActivity(false);
    };
    const follow = () => {
      if (atBottom.current) log.scrollTop = log.scrollHeight;
    };
    follow();
    log.addEventListener("scroll", onScroll, { passive: true });
    const observer = typeof MutationObserver === "function" ? new MutationObserver(follow) : null;
    observer?.observe(log, { childList: true, subtree: true, characterData: true });
    return () => {
      log.removeEventListener("scroll", onScroll);
      observer?.disconnect();
    };
  }, [ref, key]);
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    if (!atBottom.current) setNewActivity(true);
  }, [activity]);
  const toEnd = useCallback(() => {
    const log = ref.current;
    if (log) log.scrollTop = log.scrollHeight;
    atBottom.current = true;
    setNewActivity(false);
  }, [ref]);
  return { newActivity, toEnd };
}

// ---------------------------------------------------------------- actions

/** Show the Research panel (Editor page) or the Research page (elsewhere). */
function revealResearch(): void {
  if (pages.active() === EDITOR_PAGE) void registry.runCommand("research.show");
  else pages.show("research");
}

/**
 * "Run in Research" on a Python/R/Julia fence: the user's explicit action
 * (D4). The code runs in the user's research session when one of that
 * language is running (no further dialog; it grants nothing to agents);
 * otherwise it is put into the Research input for the user to run, and
 * nothing is started. Rendering a message never calls this.
 */
export async function runInResearch(code: string, language: RunnableLanguage): Promise<void> {
  const research = await import("../research/store");
  revealResearch();
  const session = research.userSession();
  const ready = session?.state.kind === "idle" || session?.state.kind === "busy";
  if (session && ready && session.language === language) {
    await research.runCode(code);
    return;
  }
  // Never discard code the user typed into the Research input.
  const current = research.useResearch.getState().code;
  if (current.trim() && current !== code) {
    notify({
      tone: "info",
      title: "The Research input already has code",
      body: "It was kept. Replace it with the code from the response, or start a session to run the response's code directly.",
      actions: [{ label: "Replace input", run: () => research.useResearch.setState({ code }) }],
    });
    return;
  }
  research.useResearch.setState({ code });
}

const RUN_IN_RESEARCH: CodeRunAction = {
  label: "Run in Research",
  run: (code, language) => void runInResearch(code, language),
};

function openExternal(url: string): void {
  commands.appOpenExternal(url).catch((e: unknown) => {
    notify({ tone: "warning", title: "Could not open the link", body: errorMessage(e) });
  });
}

// ---------------------------------------------------------------- entries

function AgentResponse({
  text,
  streaming = false,
  children,
  footer,
}: {
  text: string;
  streaming?: boolean;
  children?: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <ResponseSurface label="Agent" copyText={streaming ? undefined : text} footer={footer}>
      {children}
      {text && (
        <Markdown source={text} streaming={streaming} runAction={RUN_IN_RESEARCH} onOpenExternal={openExternal} data-testid="agents-msg-markdown" />
      )}
    </ResponseSurface>
  );
}

/**
 * One recorded tool call: name, summary, status, and how it was authorized;
 * expanding it shows the complete input and result (plain text).
 */
export function ToolCallRow({ item }: { item: Extract<TranscriptItem, { kind: "toolCall" }> }) {
  const pill = toolStatusLabel(item.status, item.result);
  // How it was authorized, quietly (nothing for reads and calls that did not run).
  const authorization = authorizationLabel(item.authorizedBy);
  return (
    <details className="agents-disclosure agents-tool" data-testid="agents-tool-call" data-tool={item.name} data-status={item.status}>
      <summary>
        <code className="ui-code">{item.name}</code>
        <span className="ui-meta agents-tool__summary">{toolSummary(item.name, item.input)}</span>
        <StatusText status={pill.status} label={pill.label} />
        {authorization && (
          <span className="ui-meta" data-testid="agents-tool-authorization">
            {authorization}
          </span>
        )}
      </summary>
      <pre className="agents-pre">{JSON.stringify(item.input, null, 2)}</pre>
      {item.result && <pre className="agents-pre">{item.result}</pre>}
    </details>
  );
}

export function TranscriptEntry({ item }: { item: TranscriptItem }) {
  switch (item.kind) {
    case "userText":
      return (
        <div className="agents-msg" data-role="user">
          <span className="ui-group-label">You</span>
          <p className="agents-msg__text">{item.text}</p>
          <MessageAttachments items={item.attachments ?? []} />
        </div>
      );
    case "assistantText":
      return (
        <div className="agents-msg" data-role="assistant" data-testid="agents-assistant-text">
          <AgentResponse text={item.text} />
        </div>
      );
    case "thinking":
      return (
        <details className="agents-disclosure">
          <summary>Reasoning summary</summary>
          <p className="agents-msg__text ui-meta">{item.text}</p>
        </details>
      );
    case "toolCall":
      return <ToolCallRow item={item} />;
    case "notice":
      return (
        <p className="agents-notice" data-level={item.level}>
          {item.message}
        </p>
      );
  }
}

export function Draft({ draft }: { draft: StreamDraft }) {
  const status = draft.activeTool ? (
    <StatusText {...activeToolLabel(draft.activeTool, draft.activeToolStatus ?? "running")} />
  ) : !draft.text && !draft.thinking ? (
    <StatusText status="running" label="Thinking" />
  ) : null;
  return (
    <div className="agents-msg" data-role="assistant" data-testid="agents-draft">
      <AgentResponse text={draft.text} streaming footer={status}>
        {draft.thinking && (
          <details className="agents-disclosure">
            <summary>Reasoning summary (streaming)</summary>
            <p className="agents-msg__text ui-meta">{draft.thinking}</p>
          </details>
        )}
      </AgentResponse>
    </div>
  );
}
