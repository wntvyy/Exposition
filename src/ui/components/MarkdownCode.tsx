// Fenced code in the shared Markdown renderer: a header with the language and
// actions (Copy, and an optional Run supplied by the host), then the code,
// highlighted with Monaco's tokenizer and rendered as React text spans.
import { Check, Copy, Play } from "lucide-react";
import { memo, useEffect, useState, type ReactNode } from "react";

import { Button } from "./Button";
import type { HighlightSpan } from "./MarkdownHighlight";
import { monacoLanguage, runnableLanguage, type RunnableLanguage } from "./MarkdownPolicy";
import { useOverflows } from "./MarkdownScroll";

/** A Run action the host offers for runnable fences (Python, R, Julia). */
export interface CodeRunAction {
  /** Visible label, e.g. "Run in Research". */
  label: string;
  run: (code: string, language: RunnableLanguage) => void;
}

/** Write text to the system clipboard (true when it was written). */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

const COPY_LABELS = { idle: "Copy", copied: "Copied", failed: "Could not copy" } as const;

/**
 * A Copy button for `text` (copied exactly as given). The label reads
 * "Copied" for a moment once the text is on the clipboard, or "Could not
 * copy" when the clipboard refuses. The accessible name is the visible label
 * followed by what is copied ("Copy code").
 */
export function CopyButton({ text, what, testId }: { text: string; what: string; testId?: string }) {
  const [state, setState] = useState<keyof typeof COPY_LABELS>("idle");
  useEffect(() => {
    if (state === "idle") return;
    const timer = setTimeout(() => setState("idle"), 1500);
    return () => clearTimeout(timer);
  }, [state]);
  const label = COPY_LABELS[state];
  return (
    <Button
      size="sm"
      variant="ghost"
      icon={state === "copied" ? Check : Copy}
      aria-label={`${label} ${what}`}
      onPress={() => void copyText(text).then((ok) => setState(ok ? "copied" : "failed"))}
      data-testid={testId}
      data-copy-state={state}
    >
      {label}
    </Button>
  );
}

let highlighter: Promise<typeof import("./MarkdownHighlight")> | null = null;
const loadHighlighter = () => (highlighter ??= import("./MarkdownHighlight"));

function useHighlight(code: string, language: string | null, enabled: boolean): HighlightSpan[][] | null {
  const id = monacoLanguage(language);
  const [result, setResult] = useState<{ code: string; id: string; lines: HighlightSpan[][] } | null>(null);
  useEffect(() => {
    if (!enabled || !id) return;
    let alive = true;
    loadHighlighter()
      .then((m) => m.highlight(code, id))
      .then(
        (lines) => {
          if (alive && lines) setResult({ code, id, lines });
        },
        () => {},
      );
    return () => {
      alive = false;
    };
  }, [code, id, enabled]);
  return result && result.code === code && result.id === id ? result.lines : null;
}

export interface CodeBlockProps {
  /** The exact fence text (what Copy and Run use). */
  code: string;
  /** The fence's language as written (`python`, `py`, …), or null. */
  language: string | null;
  /** Still streaming without its closing fence: no highlighting or actions yet. */
  open?: boolean;
  runAction?: CodeRunAction;
}

export const CodeBlock = memo(function CodeBlock({ code, language, open = false, runAction }: CodeBlockProps) {
  const lines = useHighlight(code, language, !open);
  const [scrollRef, scrolls] = useOverflows<HTMLPreElement>(code);
  const runnable = runnableLanguage(language);
  let body: ReactNode = code;
  if (lines) {
    body = lines.map((spans, index) => (
      <span key={index}>
        {spans.map((span, s) =>
          span.kind ? (
            <span key={s} className={`ui-md-tok-${span.kind}`}>
              {span.text}
            </span>
          ) : (
            span.text
          ),
        )}
        {index < lines.length - 1 ? "\n" : null}
      </span>
    ));
  }
  return (
    <div className="ui-md-code" data-language={language ?? undefined} data-open={open || undefined} data-testid="md-code-block">
      <div className="ui-md-code__header">
        <span className="ui-md-code__language">{language ?? ""}</span>
        {!open && (
          <span className="ui-md-code__actions">
            {runAction && runnable && (
              <Button size="sm" variant="ghost" icon={Play} onPress={() => runAction.run(code, runnable)} data-testid="md-code-run">
                {runAction.label}
              </Button>
            )}
            <CopyButton text={code} what="code" testId="md-code-copy" />
          </span>
        )}
      </div>
      <pre
        ref={scrollRef}
        className="ui-md-code__body"
        tabIndex={scrolls ? 0 : undefined}
        role={scrolls ? "region" : undefined}
        aria-label={scrolls ? (language ? `${language} code` : "Code") : undefined}
      >
        <code data-testid="md-code-text">{body}</code>
      </pre>
    </div>
  );
});
