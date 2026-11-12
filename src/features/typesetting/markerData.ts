// Which build diagnostics belong on an open document, as UTF-16 ranges over
// the revision the view holds. Monaco-free (unit-tested); `markers.ts` turns
// them into editor markers.
import type { BuildEvent } from "../../bindings/BuildEvent";
import type { DocumentFormat } from "../../bindings/DocumentFormat";
import type { Severity } from "../../bindings/Severity";

export interface DiagnosticMark {
  start: number;
  end: number;
  severity: Severity;
  message: string;
  /** Shown with the message, e.g. "Typst". */
  source: string;
}

const FORMAT_NAMES: Record<DocumentFormat, string> = { typst: "Typst", latex: "LaTeX", markdown: "Markdown" };

/**
 * The diagnostics the finished `builds` report for `path`, when they compiled
 * `revision` of it (the view's revision of the open document). `null` when no
 * build compiled that revision: diagnostic positions belong to the text a build
 * read, so the marks already shown stay (they move with later edits) until a
 * build of the current text reports.
 */
export function diagnosticMarks(builds: readonly BuildEvent[], path: string, revision: number): DiagnosticMark[] | null {
  let compiled = false;
  const marks: DiagnosticMark[] = [];
  for (const { result } of builds) {
    const source = result.sources.find((s) => s.path === path);
    if (!source || source.revision !== revision) continue;
    compiled = true;
    const name = FORMAT_NAMES[result.target.format] ?? result.target.format;
    for (const diagnostic of result.diagnostics) {
      const location = diagnostic.location;
      if (!location || location.path !== path) continue;
      const hints = diagnostic.hints.length ? `\n${diagnostic.hints.map((h) => `hint: ${h}`).join("\n")}` : "";
      marks.push({
        start: Math.min(location.range.start, location.range.end),
        end: Math.max(location.range.start, location.range.end),
        severity: diagnostic.severity,
        message: `${diagnostic.message}${hints}`,
        source: name,
      });
    }
  }
  return compiled ? marks : null;
}
