// Presentation helpers for workflow run plans and runs (pure; no IPC).
// Wording follows UI_STANDARD "Workflow runs": containment is stated as what
// the runner actually does, never "sandboxed" because a schema says so.
import type { Containment } from "../../bindings/Containment";
import type { WorkflowAttemptState } from "../../bindings/WorkflowAttemptState";
import type { WorkflowEffect } from "../../bindings/WorkflowEffect";
import type { WorkflowOutputValue } from "../../bindings/WorkflowOutputValue";
import type { WorkflowProposalStatus } from "../../bindings/WorkflowProposalStatus";
import type { WorkflowRun } from "../../bindings/WorkflowRun";
import type { WorkflowRunState } from "../../bindings/WorkflowRunState";
import type { WorkflowStepState } from "../../bindings/WorkflowStepState";
import type { BadgeTone, Status } from "../../ui/components";

export const WORKFLOW_RUN_KEY_PREFIX = "workflow-run:";
export const workflowRunKey = (id: string) => `${WORKFLOW_RUN_KEY_PREFIX}${id}`;

export const CONTAINMENT_VIEW: Record<Containment, { label: string; tone: BadgeTone; detail: string }> = {
  restrictedContainer: {
    label: "Restricted container",
    tone: "info",
    detail: "Runs in the configured local container: only declared inputs enter it, read-only; no network; outputs are validated before use.",
  },
  nativeAccount: {
    label: "Runs natively with your account's privileges",
    tone: "warning",
    detail: "Not contained: the program can do anything your user account can do.",
  },
  readOnlyModel: {
    label: "Sends material to the model provider",
    tone: "warning",
    detail: "Its declared inputs and instructions are sent to the configured model provider; it cannot run code or change files.",
  },
  applicationService: {
    label: "Application service",
    tone: "neutral",
    detail: "Performed by Exposition itself; anything it changes is proposed for your review.",
  },
};

export const EFFECT_LABELS: Record<WorkflowEffect, string> = {
  readDeclaredInputs: "Reads only its declared inputs",
  executeRestricted: "Runs code in the restricted container",
  executeNative: "Runs code natively with your account's privileges",
  sendToModel: "Sends its inputs and instructions to the model provider",
  writeRunOutputs: "Writes files to this run's output area",
  retainArtifact: "Keeps results in Research and records their evidence",
  registerFormalEvidence: "Records Lean audit evidence",
  proposeDocumentChange: "Prepares a manuscript change for your review",
  proposeGraphChange: "Proposes project graph changes for your review",
};

export const RUN_STATE_VIEW: Record<WorkflowRunState, { status: Status; label: string }> = {
  running: { status: "running", label: "Running" },
  succeeded: { status: "succeeded", label: "Run succeeded" },
  failed: { status: "failed", label: "Run failed" },
  canceled: { status: "canceled", label: "Run canceled" },
  interrupted: { status: "warning", label: "Interrupted" },
};

export const STEP_STATE_VIEW: Record<WorkflowStepState, { status: Status; label: string }> = {
  pending: { status: "idle", label: "Not reached" },
  running: { status: "running", label: "Running" },
  succeeded: { status: "succeeded", label: "Succeeded" },
  failed: { status: "failed", label: "Failed" },
  canceled: { status: "canceled", label: "Canceled" },
  skipped: { status: "idle", label: "Skipped" },
  interrupted: { status: "warning", label: "Interrupted" },
};

export const ATTEMPT_STATE_VIEW: Record<WorkflowAttemptState, { status: Status; label: string }> = {
  intent: { status: "idle", label: "Dispatch recorded" },
  dispatched: { status: "running", label: "Dispatched" },
  succeeded: { status: "succeeded", label: "Succeeded" },
  failed: { status: "failed", label: "Failed" },
  canceled: { status: "canceled", label: "Canceled" },
  interrupted: { status: "warning", label: "Outcome unknown (interrupted)" },
};

export const PROPOSAL_STATUS_VIEW: Record<WorkflowProposalStatus, { tone: BadgeTone; label: string; dashed: boolean }> = {
  pending: { tone: "accent", label: "Awaiting your review", dashed: true },
  accepted: { tone: "success", label: "Accepted", dashed: false },
  rejected: { tone: "neutral", label: "Rejected", dashed: true },
  expired: { tone: "neutral", label: "Expired", dashed: true },
};

/** Exact Lean verdict labels → tone. Only a clean result is success; unknown labels are warnings. */
export function verdictTone(verdict: string): BadgeTone {
  switch (verdict) {
    case "Checked":
      return "success";
    case "Errors":
      return "danger";
    case "Not verified":
      return "neutral";
    default:
      return "warning";
  }
}

export function shortHash(hash: string): string {
  const hex = hash.replace(/^sha256:/, "");
  return hex.length > 12 ? `${hex.slice(0, 12)}…` : hex;
}

export function formatBytes(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KiB`;
  if (size < 1024 * 1024 * 1024) return `${(size / 1024 / 1024).toFixed(1)} MiB`;
  return `${(size / 1024 / 1024 / 1024).toFixed(1)} GiB`;
}

export function formatSeconds(seconds: number): string {
  if (seconds < 60) return `${seconds} s`;
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  if (m < 60) return s ? `${m} min ${s} s` : `${m} min`;
  const h = Math.floor(m / 60);
  return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
}

export function formatDuration(fromMs: number, toMs: number | null): string | undefined {
  if (toMs === null) return undefined;
  const seconds = Math.max(0, (toMs - fromMs) / 1000);
  return seconds < 10 ? `${seconds.toFixed(1)} s` : formatSeconds(Math.round(seconds));
}

export function formatTime(ms: number | null): string {
  if (ms === null) return "—";
  return new Date(ms).toLocaleString();
}

/**
 * How a file output may be previewed. Images are shown only through `<img>`
 * from a blob URL (SVG included: an `<img>` never runs its scripts); tables
 * are parsed into plain cells; everything else is listed by path and hash.
 * Markup (HTML, SVG source) is never inserted into the page.
 */
export function filePreviewKind(mediaType: string, path: string): "image" | "table" | "none" {
  const type = mediaType.toLowerCase().split(";")[0]!.trim();
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  if (["image/png", "image/jpeg", "image/gif", "image/webp", "image/svg+xml"].includes(type)) return "image";
  if (type === "text/csv" || type === "text/tab-separated-values" || ext === "csv" || ext === "tsv") return "table";
  return "none";
}

/** Plain-cell parse of CSV/TSV text (RFC 4180 quoting), bounded. */
export function parseDelimited(text: string, delimiter: "," | "\t", maxRows = 200): { rows: string[][]; truncated: boolean; totalRows: number } {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let quoted = false;
  let total = 0;
  const push = () => {
    row.push(cell);
    cell = "";
  };
  const endRow = () => {
    push();
    if (!(row.length === 1 && row[0] === "")) {
      total += 1;
      if (rows.length < maxRows) rows.push(row);
    }
    row = [];
  };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]!;
    if (quoted) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          cell += '"';
          i += 1;
        } else quoted = false;
      } else cell += ch;
    } else if (ch === '"' && cell === "") quoted = true;
    else if (ch === delimiter) push();
    else if (ch === "\n") endRow();
    else if (ch !== "\r") cell += ch;
  }
  if (cell !== "" || row.length) endRow();
  return { rows, truncated: total > rows.length, totalRows: total };
}

export function outputValueLabel(value: WorkflowOutputValue): string {
  switch (value.kind) {
    case "file":
      return value.path;
    case "retained":
      return value.path;
    case "formalEvidence":
      return value.file;
    case "report":
      return "Report";
    case "promotionProposal":
      return `Manuscript change for ${value.document}`;
    case "associationProposal":
      return `${value.declarations.length} declaration association${value.declarations.length === 1 ? "" : "s"}`;
  }
}

/** The formal evidence a step's association proposal was made from (same run, upstream). */
export function auditedDeclarations(run: WorkflowRun): Map<string, { verdict: string; axioms: string[]; statement: string | null }> {
  const map = new Map<string, { verdict: string; axioms: string[]; statement: string | null }>();
  for (const step of run.steps) {
    for (const attempt of step.attempts) {
      for (const output of attempt.outputs) {
        if (output.value.kind !== "formalEvidence") continue;
        for (const d of output.value.declarations) map.set(d.name, { verdict: d.verdict, axioms: d.axioms, statement: d.statement });
      }
    }
  }
  return map;
}

export interface RunTotals {
  label: string;
  used: number;
  limit: number;
  format: (n: number) => string;
}

export function usageRows(run: Pick<WorkflowRun, "usage" | "budgets">): RunTotals[] {
  const { usage, budgets } = run;
  return [
    { label: "Steps", used: usage.steps, limit: budgets.maxSteps, format: String },
    { label: "Time", used: usage.seconds, limit: budgets.maxTotalSeconds, format: formatSeconds },
    { label: "Output", used: usage.outputBytes, limit: budgets.maxOutputBytes, format: formatBytes },
    { label: "Model requests", used: usage.modelRequests, limit: budgets.maxModelRequests, format: String },
  ];
}

/** `workflow.planStale` and friends: the reviewed plan no longer matches; review a new one. */
export const STALE_PLAN_CODES = ["workflow.planStale", "workflow.planExpired", "workflow.planUnknown"];
