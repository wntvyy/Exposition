// Lean checking status → the shared status vocabulary. A checked proof, an
// incomplete proof (sorry), a proof resting on extra axioms or compiled code, a
// partially or unverified check, errors, and "still checking" are always
// distinct labels; nothing here ever says a generic "Done", and only a clean
// result (standard axioms only) is ever labelled "Checked".
import type { LeanStatus } from "../../bindings/LeanStatus";
import type { Status } from "../../ui/components";

export interface StatusView {
  status: Status;
  /** Short pill label. */
  label: string;
  /** Pill detail (revision and counts). */
  detail?: string;
  /** Stable machine-readable kind for tests and automation. */
  kind: LeanStatus["kind"];
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
const list = (items: string[]) => items.map((a) => `\`${a}\``).join(", ");

export function statusView(status: LeanStatus): StatusView {
  switch (status.kind) {
    case "notStarted":
      return { kind: status.kind, status: "idle", label: "Not checked" };
    case "unavailable":
      return { kind: status.kind, status: "unavailable", label: "Lean unavailable" };
    case "toolchainMissing":
      return { kind: status.kind, status: "unavailable", label: "Toolchain missing", detail: status.toolchain };
    case "untrusted":
      return { kind: status.kind, status: "unavailable", label: "Untrusted" };
    case "checking":
      return {
        kind: status.kind,
        status: "running",
        label: status.starting ? "Starting Lean" : status.auditing ? "Auditing axioms" : "Checking",
        detail: `rev ${status.revision}`,
      };
    case "checked":
      return { kind: status.kind, status: "succeeded", label: "Checked", detail: `rev ${status.revision}` };
    case "checkedWithSorry":
      return {
        kind: status.kind,
        status: "warning",
        label: "Incomplete (sorry)",
        detail: `${status.count} · rev ${status.revision}`,
      };
    case "checkedWithAxioms":
      return {
        kind: status.kind,
        status: "warning",
        label: "Checked with axioms",
        detail: `${status.axioms.length} · rev ${status.revision}`,
      };
    case "reliesOnCompiledCode":
      return { kind: status.kind, status: "warning", label: "Relies on compiled code", detail: `rev ${status.revision}` };
    case "partiallyChecked":
      return { kind: status.kind, status: "warning", label: "Partially checked", detail: `rev ${status.revision}` };
    case "unverified":
      return { kind: status.kind, status: "warning", label: "Not verified", detail: `rev ${status.revision}` };
    case "errors":
      return {
        kind: status.kind,
        status: "failed",
        label: plural(status.count, "error"),
        detail: `rev ${status.revision}`,
      };
    case "serverUnavailable":
      return { kind: status.kind, status: "unavailable", label: "Lean not started" };
    case "serverCrashed":
      return {
        kind: status.kind,
        status: "failed",
        label: "Server crashed",
        detail: status.restarting ? "restarting" : undefined,
      };
  }
}

/** One-sentence explanation shown under the pill. */
export function statusSentence(status: LeanStatus): string {
  switch (status.kind) {
    case "notStarted":
      return "Lean has not checked this file yet.";
    case "unavailable":
      return status.reason;
    case "toolchainMissing":
      return status.guidance;
    case "untrusted":
      return "Checking a Lean project runs its code (the lakefile, macros, and #eval) on this computer. Trust the workspace to check it.";
    case "checking":
      if (status.starting) return `Starting the Lean server for revision ${status.revision}…`;
      return status.auditing
        ? `Lean reported no errors in revision ${status.revision}; auditing the axioms its declarations depend on…`
        : `Lean is checking revision ${status.revision}.`;
    case "checked": {
      const axioms = status.axioms.length ? `only the standard axioms ${list(status.axioms)}` : "no axioms";
      return `Lean checked revision ${status.revision}: no errors, no sorry, and its declarations use ${axioms}${status.warnings ? ` (${plural(status.warnings, "warning")})` : ""}.`;
    }
    case "checkedWithSorry":
      return `Revision ${status.revision} has no errors, but ${plural(status.count, "declaration")} ${status.count === 1 ? "depends" : "depend"} on sorry: the proof is incomplete.`;
    case "checkedWithAxioms":
      return `Revision ${status.revision} has no errors, but declarations depend on axioms beyond Lean's standard ones: ${list(status.axioms)}. The results hold only if those axioms do.`;
    case "reliesOnCompiledCode":
      return `Revision ${status.revision} has no errors, but declarations trust compiled code instead of the kernel (native_decide or similar): ${list(status.axioms)}.`;
    case "partiallyChecked":
      return status.reason;
    case "unverified":
      return status.reason;
    case "errors":
      return `Lean found ${plural(status.count, "error")} in revision ${status.revision}${status.sorryCount ? `; ${plural(status.sorryCount, "declaration")} also ${status.sorryCount === 1 ? "uses" : "use"} sorry` : ""}.`;
    case "serverUnavailable":
      return status.reason;
    case "serverCrashed":
      return status.restarting
        ? `The Lean server stopped unexpectedly (attempt ${status.attempts}); restarting.`
        : "The Lean server stopped unexpectedly and was not restarted automatically.";
  }
}

/** The revision a finished status describes, if any. */
export function finishedRevision(status: LeanStatus): number | null {
  switch (status.kind) {
    case "checked":
    case "checkedWithSorry":
    case "checkedWithAxioms":
    case "reliesOnCompiledCode":
    case "partiallyChecked":
    case "unverified":
    case "errors":
      return status.revision;
    default:
      return null;
  }
}
