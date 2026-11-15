// Mathematics in the shared Markdown renderer: KaTeX, configured as part of
// the untrusted-content boundary (see the audit in Markdown.tsx).
import "katex/dist/katex.min.css";

import katex from "katex";
import { memo, useLayoutEffect, useRef, useState } from "react";

import { useOverflows } from "./MarkdownScroll";

/**
 * KaTeX options. `trust: false` refuses every command that could produce a
 * link, an image, a class, an id, a style, or data attributes (`\href`,
 * `\url`, `\includegraphics`, `\htmlClass`, `\htmlId`, `\htmlStyle`,
 * `\htmlData`): they render as their source text. `maxSize` caps every
 * user-given dimension (`\rule`, `\kern`, …) at 10em; `maxExpand` bounds macro
 * expansion (a parse error past it). No `macros` object is shared, so a
 * `\gdef` in one formula never reaches another. Output keeps MathML for
 * screen readers next to the visual HTML.
 */
export const KATEX_OPTIONS = {
  trust: false,
  strict: "ignore",
  throwOnError: false,
  maxSize: 10,
  maxExpand: 500,
  output: "htmlAndMathml",
  errorColor: "var(--color-danger-text)",
} as const satisfies katex.KatexOptions;

/** Longest formula that is typeset; longer TeX is shown as its source. */
export const MAX_TEX_CHARS = 20_000;

export interface MathTeXProps {
  tex: string;
  display: boolean;
  /** Still streaming without its closing delimiter: shown as source until it closes. */
  open?: boolean;
}

/**
 * One formula. KaTeX builds DOM nodes directly (`katex.render`, no HTML string
 * parsing) into an element React leaves empty. A formula KaTeX cannot parse
 * shows its TeX source as code.
 */
export const MathTeX = memo(function MathTeX({ tex, display, open = false }: MathTeXProps) {
  const ref = useRef<HTMLSpanElement>(null);
  const [scrollRef, scrolls] = useOverflows<HTMLDivElement>(tex);
  // The formula (and mode) KaTeX could not parse, if it is the current one.
  const [failedKey, setFailedKey] = useState<string | null>(null);
  const key = `${display ? "d" : "i"}:${tex}`;
  const typeset = !open && tex.length <= MAX_TEX_CHARS && failedKey !== key;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || !typeset) return;
    try {
      katex.render(tex, el, { ...KATEX_OPTIONS, displayMode: display });
      if (el.querySelector(".katex-error")) setFailedKey(key);
    } catch {
      el.textContent = "";
      setFailedKey(key);
    }
  }, [tex, display, typeset, key]);

  if (!typeset) {
    const source = display ? `$$\n${tex}\n$$` : `$${tex}$`;
    return display ? (
      <pre className="ui-md-math-source" data-math-source="display">
        <code>{source}</code>
      </pre>
    ) : (
      <code className="ui-md-code-inline" data-math-source="inline">
        {source}
      </code>
    );
  }
  if (!display) return <span className="ui-md-math-inline" ref={ref} data-math="inline" />;
  // A display formula wider than the message scrolls inside its own block.
  return (
    <div
      className="ui-md-math-display"
      ref={scrollRef}
      data-math="display"
      tabIndex={scrolls ? 0 : undefined}
      role={scrolls ? "region" : undefined}
      aria-label={scrolls ? "Equation" : undefined}
    >
      <span ref={ref} />
    </div>
  );
});
