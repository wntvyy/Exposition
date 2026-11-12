// Problems panel: diagnostics of the latest build of each watched entry plus
// every source published to the app's problems contract (e.g. Lean checks),
// linked to their source ranges.
import { useEffect, useMemo, useState } from "react";
import { AlertTriangle, CircleCheck, CircleX, Info } from "lucide-react";

import { useProblems } from "../../app/problems";
import { registry } from "../../app/registry";
import type { Diagnostic } from "../../bindings/Diagnostic";
import { commands } from "../../lib/ipc";
import { Button, EmptyState } from "../../ui/components";
import {
  mentionsToolSettings,
  requestReveal,
  statusProblem,
  TOOL_SETTINGS_COMMAND,
  TOOL_SETTINGS_LABEL,
  useTypesetting,
} from "./store";
import "./typesetting.css";

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

interface ProblemRow {
  diagnostic: Diagnostic;
  /** Source label (e.g. "Lean"); build diagnostics have none. */
  label: string | null;
  reveal: () => void;
  /** Offer to open the tool locations in Settings. */
  toolSettings: boolean;
}

function revealBuildDiagnostic(diagnostic: Diagnostic) {
  const loc = diagnostic.location;
  if (loc) requestReveal(loc.path, loc.range.start);
}

export function ProblemsPanel() {
  const entries = useTypesetting((s) => s.entries);
  const sources = useProblems((s) => s.sources);
  const rows = useMemo<ProblemRow[]>(() => {
    // A build that could not run (missing compiler, failed to start) reports
    // it in its status, not as a diagnostic: list it as a problem too.
    const status = Object.values(entries).flatMap((e): ProblemRow[] => {
      const problem = statusProblem(e.latest);
      if (!problem) return [];
      const diagnostic: Diagnostic = { severity: "error", message: problem.message, location: null, hints: problem.hints };
      return [{ diagnostic, label: null, reveal: () => {}, toolSettings: problem.toolSettings }];
    });
    const build = Object.values(entries)
      .flatMap((e) => e.latest?.result.diagnostics ?? [])
      .map((diagnostic) => ({
        diagnostic,
        label: null,
        reveal: () => revealBuildDiagnostic(diagnostic),
        toolSettings: mentionsToolSettings(diagnostic.hints),
      }));
    const published = Object.values(sources).flatMap((source) =>
      source.diagnostics.map((diagnostic) => ({
        diagnostic,
        label: source.label,
        reveal: () => source.reveal(diagnostic),
        toolSettings: false,
      })),
    );
    return [...status, ...build, ...published];
  }, [entries, sources]);
  const diagnostics = useMemo(() => rows.map((r) => r.diagnostic), [rows]);
  const key = JSON.stringify(diagnostics);
  const [positions, setPositions] = useState<Map<number, string>>(new Map());

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const docs = await commands.documentList().catch(() => []);
      const texts = new Map<string, string>();
      const result = new Map<number, string>();
      for (const [i, diagnostic] of diagnostics.entries()) {
        const loc = diagnostic.location;
        if (!loc) continue;
        if (!texts.has(loc.path)) {
          const doc = docs.find((d) => d.path === loc.path);
          const state = doc ? await commands.documentState(doc.id).catch(() => null) : null;
          texts.set(loc.path, state?.text ?? "");
        }
        const { line, column } = lineColumn(texts.get(loc.path) ?? "", loc.range.start);
        result.set(i, `${loc.path}:${line}:${column}`);
      }
      if (!cancelled) setPositions(result);
    })();
    return () => {
      cancelled = true;
    };
    // `key` captures the diagnostics' content.
  }, [key]);

  if (diagnostics.length === 0) {
    return (
      <div data-testid="problems-panel" className="problems-empty">
        <EmptyState
          icon={CircleCheck}
          title="No problems"
          description="Build and proof-checking diagnostics of open documents appear here."
        />
      </div>
    );
  }
  return (
    <ul className="problems" data-testid="problems-panel" aria-label="Problems">
      {rows.map(({ diagnostic, label, reveal, toolSettings }, i) => {
        const Icon = ICONS[diagnostic.severity];
        const loc = diagnostic.location;
        return (
          <li key={i} className="problems__entry">
            <button
              type="button"
              className="problems__item"
              data-severity={diagnostic.severity}
              data-testid="problem"
              disabled={!loc}
              data-source={label ?? "build"}
              onClick={() => loc && reveal()}
            >
              <Icon size={14} className="problems__icon" aria-label={diagnostic.severity} />
              {label && <span className="problems__source">{label}</span>}
              <span className="problems__message">{diagnostic.message}</span>
              {loc && <span className="problems__location">{positions.get(i) ?? loc.path}</span>}
            </button>
            {diagnostic.hints.length > 0 && (
              <ul className="problems__hints">
                {diagnostic.hints.map((h, j) => (
                  <li key={j}>{h}</li>
                ))}
              </ul>
            )}
            {toolSettings && (
              <div className="problems__actions">
                <Button
                  size="sm"
                  variant="secondary"
                  data-testid="problem-tool-settings"
                  onPress={() => void registry.runCommand(TOOL_SETTINGS_COMMAND)}
                >
                  {TOOL_SETTINGS_LABEL}
                </Button>
              </div>
            )}
          </li>
        );
      })}
    </ul>
  );
}
