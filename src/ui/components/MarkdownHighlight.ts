// Code-fence highlighting for the shared Markdown renderer, using Monaco's own
// tokenizers (the same Monarch grammars the editors use). Loaded on demand by
// MarkdownCode.tsx the first time a fence with a known language is shown.
//
// Output is data — lines of { text, class } — that React renders as text in
// spans. No HTML string is produced or inserted anywhere.
import "monaco-editor/languages/definitions/cpp/register";
import "monaco-editor/languages/definitions/css/register";
import "monaco-editor/languages/definitions/go/register";
import "monaco-editor/languages/definitions/html/register";
import "monaco-editor/languages/definitions/ini/register";
import "monaco-editor/languages/definitions/java/register";
import "monaco-editor/languages/definitions/javascript/register";
import "monaco-editor/languages/definitions/julia/register";
import "monaco-editor/languages/definitions/markdown/register";
import "monaco-editor/languages/definitions/powershell/register";
import "monaco-editor/languages/definitions/python/register";
import "monaco-editor/languages/definitions/r/register";
import "monaco-editor/languages/definitions/rust/register";
import "monaco-editor/languages/definitions/shell/register";
import "monaco-editor/languages/definitions/sql/register";
import "monaco-editor/languages/definitions/typescript/register";
import "monaco-editor/languages/definitions/xml/register";
import "monaco-editor/languages/definitions/yaml/register";

import { monaco } from "../../lib/monaco";
import { tokenClass } from "./MarkdownPolicy";

export interface HighlightSpan {
  text: string;
  /** Token class suffix (`keyword`, `string`, …), or null for plain text. */
  kind: string | null;
}

/** Longest fence that is highlighted; longer code is shown as plain text. */
const MAX_HIGHLIGHT_CHARS = 200_000;

const ready = new Map<string, Promise<boolean>>();

/**
 * Load the language's tokenizer. `monaco.editor.tokenize` only starts loading
 * a lazily registered grammar (and returns untokenized lines meanwhile);
 * `colorize` awaits it. The colorize result (HTML) is discarded unused.
 */
function ensureTokenizer(languageId: string): Promise<boolean> {
  let promise = ready.get(languageId);
  if (!promise) {
    // Exposition's own languages (Typst, LaTeX, Lean, …) exist once the editor has registered them.
    if (!monaco.languages.getLanguages().some((l) => l.id === languageId)) return Promise.resolve(false);
    promise = monaco.editor.colorize("", languageId, {}).then(
      () => true,
      () => false,
    );
    ready.set(languageId, promise);
  }
  return promise;
}

/** Highlight `code` as `languageId`; null when the language is unknown or the code too long. */
export async function highlight(code: string, languageId: string): Promise<HighlightSpan[][] | null> {
  if (code.length > MAX_HIGHLIGHT_CHARS) return null;
  if (!(await ensureTokenizer(languageId))) return null;
  const tokenLines = monaco.editor.tokenize(code, languageId);
  const lines = code.split(/\r\n|\r|\n/);
  return lines.map((line, index) => {
    const tokens = tokenLines[index] ?? [];
    if (tokens.length === 0) return [{ text: line, kind: null }];
    const spans: HighlightSpan[] = [];
    if (tokens[0]!.offset > 0) spans.push({ text: line.slice(0, tokens[0]!.offset), kind: null });
    for (let t = 0; t < tokens.length; t += 1) {
      const start = tokens[t]!.offset;
      const end = t + 1 < tokens.length ? tokens[t + 1]!.offset : line.length;
      if (end <= start) continue;
      const kind = tokenClass(tokens[t]!.type);
      const text = line.slice(start, end);
      const previous = spans[spans.length - 1];
      if (previous && previous.kind === kind) previous.text += text;
      else spans.push({ text, kind });
    }
    return spans;
  });
}
