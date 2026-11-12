// Lean checking status → the shared status vocabulary. A checked proof, an
// incomplete proof (sorry), errors, and "still checking" are always distinct
// labels; nothing here ever says a generic "Done".
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
        label: status.starting ? "Starting Lean" : "Checking",
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
    case "errors":
      return {
        kind: status.kind,
        status: "failed",
        label: plural(status.count, "error"),
        detail: `rev ${status.revision}`,
      };
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
      return status.starting
        ? `Starting the Lean server for revision ${status.revision}…`
        : `Lean is checking revision ${status.revision}.`;
    case "checked":
      return `Lean checked revision ${status.revision}: no errors and no sorry${status.warnings ? ` (${plural(status.warnings, "warning")})` : ""}.`;
    case "checkedWithSorry":
      return `Revision ${status.revision} has no errors, but ${plural(status.count, "declaration")} ${status.count === 1 ? "uses" : "use"} sorry: the proof is incomplete.`;
    case "errors":
      return `Lean found ${plural(status.count, "error")} in revision ${status.revision}${status.sorryCount ? `; ${plural(status.sorryCount, "declaration")} also ${status.sorryCount === 1 ? "uses" : "use"} sorry` : ""}.`;
    case "serverCrashed":
      return status.restarting
        ? `The Lean server stopped unexpectedly (attempt ${status.attempts}); restarting.`
        : "The Lean server stopped unexpectedly and was not restarted automatically.";
  }
}

/** The revision a finished status describes, if any. */
export function finishedRevision(status: LeanStatus): number | null {
  return status.kind === "checked" || status.kind === "checkedWithSorry" || status.kind === "errors"
    ? status.revision
    : null;
}
