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

/**
 * Invisible and direction-changing characters, the set approval cards make
 * visible (Rust `tools::exec::hidden`): C0/C1 controls except tab and line
 * ends, the escape's own opening bracket, and the format, joiner, bidi, and
 * tag characters. A carriage return directly before a line feed is a line end.
 * Code points, not a regular expression, so no escape can be mistyped.
 */
const HIDDEN_RANGES: readonly (readonly [number, number])[] = [
  [0x0000, 0x0008],
  [0x000b, 0x000c],
  [0x000e, 0x001f],
  [0x007f, 0x009f],
  [0x00ad, 0x00ad],
  [0x034f, 0x034f],
  [0x061c, 0x061c],
  [0x115f, 0x1160],
  [0x17b4, 0x17b5],
  [0x180b, 0x180f],
  [0x200b, 0x200f],
  [0x2028, 0x202e],
  [0x2060, 0x206f],
  [0x27e8, 0x27e8],
  [0x3164, 0x3164],
  [0xfeff, 0xfeff],
  [0xffa0, 0xffa0],
  [0xfff9, 0xfffb],
  [0x1d173, 0x1d17a],
  [0xe0000, 0xe0fff],
];

function isHidden(cp: number, next: number | undefined): boolean {
  if (cp === 0x0d) return next !== 0x0a;
  return HIDDEN_RANGES.some(([lo, hi]) => cp >= lo && cp <= hi);
}

/** The text split into visible runs and hidden code points. */
function splitHidden(text: string): (string | number)[] {
  const parts: (string | number)[] = [];
  const cps = Array.from(text, (c) => c.codePointAt(0) ?? 0);
  let run = "";
  cps.forEach((cp, i) => {
    if (isHidden(cp, cps[i + 1])) {
      if (run) parts.push(run);
      run = "";
      parts.push(cp);
    } else {
      run += String.fromCodePoint(cp);
    }
  });
  if (run) parts.push(run);
  return parts;
}

/** How many hidden characters `text` holds. */
export function hiddenCount(text: string): number {
  return splitHidden(text).filter((part) => typeof part === "number").length;
}

/** `text` with each hidden character shown as ⟨U+XXXX⟩ (React nodes; the source is unchanged). */
function revealHidden(text: string): ReactNode {
  const parts = splitHidden(text);
  if (parts.every((part) => typeof part === "string")) return text;
  return parts.map((part, i) => {
    if (typeof part === "string") return part;
    const code = part.toString(16).toUpperCase().padStart(4, "0");
    return (
      <span key={i} className="ui-md-hidden-char" title={`Invisible or direction-changing character U+${code}`}>
        {`⟨U+${code}⟩`}
      </span>
    );
  });
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
  // What is shown is what Copy and Run use: hidden characters appear as ⟨U+XXXX⟩.
  const hidden = hiddenCount(code);
  let body: ReactNode = revealHidden(code);
  if (lines) {
    body = lines.map((spans, index) => (
      <span key={index}>
        {spans.map((span, s) =>
          span.kind ? (
            <span key={s} className={`ui-md-tok-${span.kind}`}>
              {revealHidden(span.text)}
            </span>
          ) : (
            <span key={s}>{revealHidden(span.text)}</span>
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
      {hidden > 0 && !open && (
        <p className="ui-md-code__notice" data-testid="md-code-hidden">
          {hidden === 1 ? "1 invisible or direction-changing character" : `${hidden} invisible or direction-changing characters`}{" "}
          shown as ⟨U+…⟩. Copy and Run use the code exactly as written.
        </p>
      )}
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
