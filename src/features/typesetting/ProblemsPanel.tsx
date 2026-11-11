// Problems panel: diagnostics of the latest build of each watched entry,
// linked to their source ranges.
import { useEffect, useState } from "react";
import { AlertTriangle, CircleX, Info } from "lucide-react";

import type { Diagnostic } from "../../bindings/Diagnostic";
import { commands } from "../../lib/ipc";
import { requestReveal, useTypesetting } from "./store";

interface Located {
  diagnostic: Diagnostic;
  line: number | null;
  column: number | null;
}

/** 1-based line/column of a UTF-16 offset. */
export function lineColumn(text: string, offset: number): { line: number; column: number } {
  let line = 1;
  let start = 0;
  for (let i = 0; i < Math.min(offset, text.length); i++) {
    const c = text.charCodeAt(i);
    if (c === 10 || (c === 13 && text.charCodeAt(i + 1) !== 10)) {
      line += 1;
      start = i + 1;
    }
  }
  return { line, column: offset - start + 1 };
}

const ICONS = { error: CircleX, warning: AlertTriangle, info: Info } as const;

export function ProblemsPanel() {
  const entries = useTypesetting((s) => s.entries);
  const diagnostics = Object.values(entries).flatMap((e) => e.latest?.result.diagnostics ?? []);
  const [located, setLocated] = useState<Located[]>([]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const docs = await commands.documentList().catch(() => []);
      const texts = new Map<string, string>();
      const result: Located[] = [];
      for (const diagnostic of diagnostics) {
        const loc = diagnostic.location;
        if (!loc) {
          result.push({ diagnostic, line: null, column: null });
          continue;
        }
        if (!texts.has(loc.path)) {
          const doc = docs.find((d) => d.path === loc.path);
          const text = doc ? (await commands.documentState(doc.id).catch(() => null))?.text : undefined;
          texts.set(loc.path, text ?? "");
        }
        const text = texts.get(loc.path) ?? "";
        const { line, column } = lineColumn(text, loc.range.start);
        result.push({ diagnostic, line, column });
      }
      if (!cancelled) setLocated(result);
    })();
    return () => {
      cancelled = true;
    };
    // Recompute when the set of diagnostics changes.
  }, [JSON.stringify(diagnostics)]);

  if (diagnostics.length === 0) {
    return (
      <div className="problems problems--empty" data-testid="problems-panel">
        No problems
      </div>
    );
  }
  return (
    <ul className="problems" data-testid="problems-panel" aria-label="Problems">
      {located.map(({ diagnostic, line, column }, i) => {
        const Icon = ICONS[diagnostic.severity];
        const loc = diagnostic.location;
        return (
          <li key={i}>
            <button
              type="button"
              className={`problems__item problems__item--${diagnostic.severity}`}
              data-testid="problem"
              disabled={!loc}
              onClick={() => loc && requestReveal(loc.path, loc.range.start)}
            >
              <Icon size={14} aria-label={diagnostic.severity} />
              <span className="problems__message">{diagnostic.message}</span>
              {loc && (
                <span className="problems__location">
                  {loc.path}
                  {line !== null ? `:${line}:${column}` : ""}
                </span>
              )}
            </button>
            {diagnostic.hints.length > 0 && (
              <ul className="problems__hints">
                {diagnostic.hints.map((h, j) => (
                  <li key={j}>{h}</li>
                ))}
              </ul>
            )}
          </li>
        );
      })}
    </ul>
  );
}
