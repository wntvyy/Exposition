// Transcript entries of an agent task: messages, reasoning summaries, tool calls, notices, and the streaming draft.
// Agent responses render through the shared Markdown renderer inside a ResponseSurface; tool payloads and
// results, reasoning summaries, and the user's own text stay plain text.
import "./transcript.css";

import { useEffect, useSyncExternalStore, type ReactNode, type RefObject } from "react";

import { EDITOR_PAGE, pages } from "../../app/pages";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import type { TranscriptItem } from "../../bindings/TranscriptItem";
import { commands, errorMessage } from "../../lib/ipc";
import {
  Markdown,
  ResponseSurface,
  StatusText,
  type CodeRunAction,
  type ResponseVariant,
  type RunnableLanguage,
} from "../../ui/components";
import { activeToolLabel, toolStatusLabel, toolSummary } from "./format";
import type { StreamDraft } from "./store";

// ---------------------------------------------------------------- response treatment (review switch)

/**
 * The response treatment under review (A1): debug builds read
 * `localStorage["exposition.debug.responseVariant"]` (band | inset | document)
 * so the three can be compared in the real app. Release builds never read it;
 * it is not a user setting and goes away once a treatment is chosen.
 */
export const RESPONSE_VARIANT_KEY = "exposition.debug.responseVariant";
const DEFAULT_VARIANT: ResponseVariant = "inset";
const VARIANTS: readonly ResponseVariant[] = ["band", "inset", "document"];

let debugAllowed: boolean | null = null;
let variant: ResponseVariant = DEFAULT_VARIANT;
const listeners = new Set<() => void>();

function readVariant(): void {
  let next = DEFAULT_VARIANT;
  if (debugAllowed) {
    try {
      const stored = localStorage.getItem(RESPONSE_VARIANT_KEY);
      if (stored && (VARIANTS as readonly string[]).includes(stored)) next = stored as ResponseVariant;
    } catch {
      // storage unavailable: default treatment
    }
  }
  if (next !== variant) {
    variant = next;
    for (const listener of [...listeners]) listener();
  }
}

function subscribeVariant(listener: () => void): () => void {
  listeners.add(listener);
  if (debugAllowed === null) {
    debugAllowed = import.meta.env.DEV;
    readVariant();
    if (!debugAllowed && typeof commands.appInfo === "function") {
      commands.appInfo().then(
        (info) => {
          if (info.debugBuild) {
            debugAllowed = true;
            readVariant();
          }
        },
        () => {},
      );
    }
    window.addEventListener("storage", (event) => {
      if (event.key === RESPONSE_VARIANT_KEY || event.key === null) readVariant();
    });
  }
  return () => listeners.delete(listener);
}

function useResponseVariant(): ResponseVariant {
  return useSyncExternalStore(subscribeVariant, () => variant);
}

// ---------------------------------------------------------------- follow the bottom

/** Within this many pixels of the end, the reader counts as at the bottom. */
const BOTTOM_SLACK = 32;

/**
 * Keep the transcript at its end while new output arrives, but only while
 * the reader is there: scrolling up stops following (earlier text stays
 * put) and scrolling back to the end resumes it. Content changes of any kind
 * (streamed text rendered a frame later, typeset math, highlighting, new
 * entries) are followed. A new `key` (another task) starts at the end.
 */
export function useFollowBottom(ref: RefObject<HTMLElement | null>, key: unknown): void {
  useEffect(() => {
    const log = ref.current;
    if (!log) return;
    let atBottom = true;
    const onScroll = () => {
      atBottom = log.scrollHeight - log.scrollTop - log.clientHeight <= BOTTOM_SLACK;
    };
    const follow = () => {
      if (atBottom) log.scrollTop = log.scrollHeight;
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
  treatment,
  streaming = false,
  children,
  footer,
}: {
  text: string;
  treatment: ResponseVariant;
  streaming?: boolean;
  children?: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <ResponseSurface variant={treatment} label="Agent" copyText={streaming ? undefined : text} footer={footer}>
      {children}
      {text && (
        <Markdown source={text} streaming={streaming} runAction={RUN_IN_RESEARCH} onOpenExternal={openExternal} data-testid="agents-msg-markdown" />
      )}
    </ResponseSurface>
  );
}

export function TranscriptEntry({ item }: { item: TranscriptItem }) {
  switch (item.kind) {
    case "userText":
      return (
        <div className="agents-msg" data-role="user">
          <span className="ui-group-label">You</span>
          <p className="agents-msg__text">{item.text}</p>
        </div>
      );
    case "assistantText":
      return <AssistantMessage text={item.text} />;
    case "thinking":
      return (
        <details className="agents-disclosure">
          <summary>Reasoning summary</summary>
          <p className="agents-msg__text ui-meta">{item.text}</p>
        </details>
      );
    case "toolCall": {
      const pill = toolStatusLabel(item.status, item.result);
      return (
        <details className="agents-disclosure agents-tool" data-testid="agents-tool-call" data-tool={item.name}>
          <summary>
            <code className="ui-code">{item.name}</code>
            <span className="ui-meta agents-tool__summary">{toolSummary(item.name, item.input)}</span>
            <StatusText status={pill.status} label={pill.label} />
          </summary>
          <pre className="agents-pre">{JSON.stringify(item.input, null, 2)}</pre>
          {item.result && <pre className="agents-pre">{item.result}</pre>}
        </details>
      );
    }
    case "notice":
      return (
        <p className="agents-notice" data-level={item.level}>
          {item.message}
        </p>
      );
  }
}

function AssistantMessage({ text }: { text: string }) {
  const treatment = useResponseVariant();
  return (
    <div className="agents-msg" data-role="assistant" data-variant={treatment} data-testid="agents-assistant-text">
      <AgentResponse text={text} treatment={treatment} />
    </div>
  );
}

export function Draft({ draft }: { draft: StreamDraft }) {
  const treatment = useResponseVariant();
  const status = draft.activeTool ? (
    <StatusText {...activeToolLabel(draft.activeTool, draft.activeToolStatus ?? "running")} />
  ) : !draft.text && !draft.thinking ? (
    <StatusText status="running" label="Thinking" />
  ) : null;
  return (
    <div className="agents-msg" data-role="assistant" data-variant={treatment} data-testid="agents-draft">
      <AgentResponse text={draft.text} treatment={treatment} streaming footer={status}>
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
