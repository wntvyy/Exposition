// Source-editor languages. Monaco's core API ships without a language
// catalogue; register the ids the workspace assigns (see `language_for` in
// crates/exposition-workspace/src/encoding.rs) with lightweight tokenizers.
// Language services (completion, diagnostics) come from language servers later.
import "monaco-editor/languages/definitions/julia/register";
import "monaco-editor/languages/definitions/markdown/register";
import "monaco-editor/languages/definitions/python/register";
import "monaco-editor/languages/definitions/r/register";
import "monaco-editor/languages/definitions/yaml/register";

import { monaco } from "../../lib/monaco";

type Monarch = monaco.languages.IMonarchLanguage;

const latex: Monarch = {
  tokenizer: {
    root: [
      [/%.*$/, "comment"],
      [/\\[a-zA-Z@]+\*?/, "keyword"],
      [/\\./, "keyword"],
      [/\$\$|\$|\\\[|\\\]|\\\(|\\\)/, "string"],
      [/[{}[\]]/, "delimiter.bracket"],
    ],
  },
};

const typst: Monarch = {
  tokenizer: {
    root: [
      [/\/\/.*$/, "comment"],
      [/\/\*/, "comment", "@comment"],
      [/^\s*=+ .*$/, "keyword"],
      [/#[a-zA-Z_][\w-]*/, "keyword"],
      [/\$/, "string"],
      [/"([^"\\]|\\.)*"/, "string"],
      [/[{}[\]()]/, "delimiter.bracket"],
    ],
    comment: [
      [/\*\//, "comment", "@pop"],
      [/./, "comment"],
    ],
  },
};

const bibtex: Monarch = {
  tokenizer: {
    root: [
      [/%.*$/, "comment"],
      [/@[a-zA-Z]+/, "keyword"],
      [/[a-zA-Z]+(?=\s*=)/, "attribute.name"],
      [/"([^"\\]|\\.)*"/, "string"],
      [/[{}]/, "delimiter.bracket"],
    ],
  },
};

const lean: Monarch = {
  keywords: ["theorem", "lemma", "def", "example", "by", "fun", "have", "show", "from", "import", "open", "namespace", "end", "variable", "structure", "inductive", "where", "match", "with", "let", "in", "sorry"],
  tokenizer: {
    root: [
      [/--.*$/, "comment"],
      [/\/-/, "comment", "@comment"],
      [/[a-zA-Z_][\w'.]*/, { cases: { "@keywords": "keyword", "@default": "identifier" } }],
      [/"([^"\\]|\\.)*"/, "string"],
    ],
    comment: [
      [/-\//, "comment", "@pop"],
      [/./, "comment"],
    ],
  },
};

const toml: Monarch = {
  tokenizer: {
    root: [
      [/#.*$/, "comment"],
      [/^\s*\[\[?[^\]]*\]\]?/, "keyword"],
      [/[A-Za-z0-9_.-]+(?=\s*=)/, "attribute.name"],
      [/"([^"\\]|\\.)*"|'[^']*'/, "string"],
      [/\b(true|false)\b|-?\d[\d_.eE+-]*/, "number"],
    ],
  },
};

const json: Monarch = {
  tokenizer: {
    root: [
      [/"([^"\\]|\\.)*"(?=\s*:)/, "attribute.name"],
      [/"([^"\\]|\\.)*"/, "string"],
      [/\b(true|false|null)\b|-?\d[\d.eE+-]*/, "number"],
      [/[{}[\]]/, "delimiter.bracket"],
    ],
  },
};

const custom: Record<string, Monarch> = { latex, typst, bibtex, lean, toml, json };
const builtIn = new Set(["markdown", "python", "r", "julia", "yaml"]);
const registered = new Set<string>();

/** Ensure `id` is a registered Monaco language; returns the id to use for models. */
export function ensureLanguage(id: string): string {
  if (builtIn.has(id) || registered.has(id)) return id;
  const monarch = custom[id];
  if (!monarch) {
    if (!registered.has("plaintext")) registered.add("plaintext");
    return "plaintext";
  }
  monaco.languages.register({ id });
  monaco.languages.setMonarchTokensProvider(id, monarch);
  registered.add(id);
  return id;
}
