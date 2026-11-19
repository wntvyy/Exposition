// V2 review samples: a tiny line tokenizer for the static code sample (the real
// editor is Monaco). Token kinds map to the same color roles as the production
// Monaco theme, except keywords, which are drawn with the
// info role so syntax keeps its meaning when the accent changes (Ocean, Sage).
import type { Lang } from "./mock";

export type TokenKind = "plain" | "comment" | "keyword" | "string" | "number" | "type" | "label" | "operator";
export interface Token {
  kind: TokenKind;
  text: string;
}

type Rule = [RegExp, TokenKind];

const NUMBER: Rule = [/\d+(?:\.\d+)?/y, "number"];
const OPERATOR: Rule = [/[-+*/=<>!:.,;|&^%()[\]{}]+/y, "operator"];
const WORD: Rule = [/[A-Za-z_\u0080-￿][\w\u0080-￿']*/y, "plain"];

const RULES: Record<Lang, Rule[]> = {
  typst: [
    [/\/\/.*/y, "comment"],
    [/"(?:[^"\\]|\\.)*"/y, "string"],
    [/`[^`]*`/y, "string"],
    [/<[\w:-]+>/y, "label"],
    [/@[\w:-]+/y, "label"],
    [/#(?:import|let|set|show|if|else|for|in|return)\b/y, "keyword"],
    [/#[A-Za-z_][\w.]*/y, "type"],
    [/^=+ .*/y, "keyword"],
    NUMBER,
    [/\$/y, "operator"],
    WORD,
    OPERATOR,
  ],
  latex: [
    [/%.*/y, "comment"],
    [/\\(?:label|ref|eqref|cite)\{[^}]*\}/y, "label"],
    [/\\(?:begin|end)\{[^}]*\}/y, "keyword"],
    [/\\(?:section|subsection|chapter)\b/y, "keyword"],
    [/\\[A-Za-z]+/y, "type"],
    [/\\./y, "operator"],
    NUMBER,
    [/\$/y, "operator"],
    WORD,
    OPERATOR,
  ],
  python: [
    [/#.*/y, "comment"],
    [/"""[^]*?"""/y, "string"],
    [/"(?:[^"\\]|\\.)*"/y, "string"],
    [/'(?:[^'\\]|\\.)*'/y, "string"],
    [/@\w+/y, "type"],
    [/\b(?:from|import|def|class|return|if|else|elif|for|in|as|with|None|True|False|and|or|not)\b/y, "keyword"],
    [/\b(?:float|int|str|bool|FitResult|DataFrame)\b/y, "type"],
    NUMBER,
    WORD,
    OPERATOR,
  ],
  lean: [
    [/--.*/y, "comment"],
    [/\/-[^]*?-\//y, "comment"],
    [/\b(?:import|theorem|lemma|def|noncomputable|by|fun|let|have|show|from|at|with|sorry)\b/y, "keyword"],
    [/\b(?:unfold|positivity|exact|apply|intro|simp|linarith)\b/y, "keyword"],
    [/ℝ|ℕ|Real\.\w+|Mathlib[\w.]*/y, "type"],
    NUMBER,
    WORD,
    [/[-+*/=<>!:.,;|&^%()[\]{}→←≤≥]+/y, "operator"],
  ],
};

export function tokenizeLine(line: string, lang: Lang): Token[] {
  const rules = RULES[lang];
  const out: Token[] = [];
  const push = (kind: TokenKind, text: string) => {
    const last = out[out.length - 1];
    if (last && last.kind === kind) last.text += text;
    else out.push({ kind, text });
  };
  let i = 0;
  while (i < line.length) {
    let matched = false;
    for (const [re, kind] of rules) {
      // `^` rules only apply at the start of the line.
      if (re.source.startsWith("^") && i !== 0) continue;
      re.lastIndex = i;
      const m = re.exec(line);
      if (m && m[0].length > 0) {
        push(kind, m[0]);
        i += m[0].length;
        matched = true;
        break;
      }
    }
    if (!matched) {
      push("plain", line[i]!);
      i += 1;
    }
  }
  return out;
}
