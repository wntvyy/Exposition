// The one table of Lean verdict words. Every surface that names a verdict
// (Lean panel and status, Graph, Research inspector, workflow conditions and
// run monitor) reads its label here; the wording is product contract.
import type { FormalVerdict } from "../../bindings/FormalVerdict";

export const VERDICT_LABEL: Record<FormalVerdict, string> = {
  checked: "Checked",
  checkedWithAxioms: "Checked with axioms",
  incomplete: "Incomplete (sorry)",
  reliesOnCompiledCode: "Relies on compiled code",
  partiallyChecked: "Partially checked",
  notVerified: "Not verified",
  errors: "Errors",
};

/**
 * The verdict classes as workflows store them (Rust `KNOWN_VERDICTS`,
 * crates/exposition-graph/src/workflow.rs: condition values and the audit
 * adapter's output). Stored values never change; only their display does.
 */
export const STORED_VERDICT_CLASS: Record<string, FormalVerdict> = {
  Checked: "checked",
  "Checked with nonstandard axioms": "checkedWithAxioms",
  Incomplete: "incomplete",
  "Relies on compiled code": "reliesOnCompiledCode",
  "Partially checked": "partiallyChecked",
  Unverified: "notVerified",
  Errors: "errors",
};

/** The class a stored label belongs to, by Rust's `verdict_class` rule (longest matching class). */
function storedClass(stored: string): string | null {
  let best: string | null = null;
  for (const known of Object.keys(STORED_VERDICT_CLASS)) {
    if (!stored.startsWith(known)) continue;
    const rest = stored.slice(known.length);
    if (rest !== "" && !rest.startsWith(" (") && !rest.startsWith(":") && !rest.startsWith(" —")) continue;
    if (best === null || known.length > best.length) best = known;
  }
  return best;
}

/**
 * A workflow's stored verdict label in the words of the Lean view: the class
 * is replaced by its label and any detail is kept ("Unverified: reason" →
 * "Not verified: reason", "Errors (2)" → "Errors (2)", "Incomplete (2 sorry)"
 * stays). A label of no known class ("Not checked", "Still checking") is
 * returned unchanged.
 */
export function storedVerdictLabel(stored: string): string {
  const known = storedClass(stored);
  if (known === null) return stored;
  const rest = stored.slice(known.length);
  const verdict = STORED_VERDICT_CLASS[known]!;
  // The count form already names sorry; do not say it twice.
  if (verdict === "incomplete" && rest.startsWith(" (")) return stored;
  return `${VERDICT_LABEL[verdict]}${rest}`;
}
