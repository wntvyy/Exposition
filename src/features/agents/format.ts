// Labels and derived values for the agent panel (pure; unit tested).
import type { BudgetKind } from "../../bindings/BudgetKind";
import type { ContextConsent } from "../../bindings/ContextConsent";
import type { Effort } from "../../bindings/Effort";
import type { ExecutionMode } from "../../bindings/ExecutionMode";
import type { PermissionProfile } from "../../bindings/PermissionProfile";
import type { TaskStatus } from "../../bindings/TaskStatus";
import type { TaskSummary } from "../../bindings/TaskSummary";
import type { ToolCallStatus } from "../../bindings/ToolCallStatus";
import type { Status } from "../../ui/components";

/** A `StatusPill` status plus a specific label. */
export interface PillState {
  status: Status;
  label: string;
}

export function isActive(status: TaskStatus): boolean {
  return status.kind === "queued" || status.kind === "running" || status.kind === "awaitingApproval";
}

const BUDGET_NAMES: Record<BudgetKind, string> = {
  turns: "turn",
  outputTokens: "output-token",
  inputTokens: "input-token",
  wallTime: "time",
  toolCalls: "tool-call",
  commands: "command",
};

/** Task status in the shared status vocabulary, with a specific label. */
export function statusLabel(status: TaskStatus): PillState {
  switch (status.kind) {
    case "queued":
      return { status: "running", label: "Queued" };
    case "running":
      return { status: "running", label: "Agent working" };
    case "awaitingApproval":
      return { status: "warning", label: "Needs approval" };
    case "completed":
      return { status: "succeeded", label: "Task completed" };
    case "canceled":
      return { status: "canceled", label: "Canceled" };
    case "budgetExceeded":
      return { status: "warning", label: `Stopped: ${BUDGET_NAMES[status.budget]} budget` };
    case "refused":
      return { status: "failed", label: "Declined by provider" };
    case "failed":
      return { status: "failed", label: "Task failed" };
    case "interrupted":
      return { status: "canceled", label: "Interrupted" };
  }
}

/** Tool call status; unavailable services are shown as unavailable, not failed. */
export function toolStatusLabel(status: ToolCallStatus, result: string | null = null): PillState {
  switch (status) {
    case "running":
      return { status: "running", label: "Running" };
    case "awaitingApproval":
      return { status: "warning", label: "Waiting for approval" };
    case "succeeded":
      return { status: "succeeded", label: "Done" };
    case "failed":
      return result?.includes("not available in this build")
        ? { status: "unavailable", label: "Unavailable" }
        : { status: "failed", label: "Failed" };
    case "notRun":
      return { status: "canceled", label: "Not run" };
  }
}

export const PERMISSION_LABELS: Record<PermissionProfile, string> = {
  reviewEdits: "Ask before each change",
  autoApply: "Apply changes, review afterwards",
  readOnly: "Read only",
};

export const EXECUTION_LABELS: Record<ExecutionMode, string> = {
  disabled: "No commands",
  approveEach: "Ask before each command (runs natively)",
  restricted: "Restricted sandbox only (unattended)",
};

export const CONSENT_LABELS: Record<ContextConsent, string> = {
  scopeContent: "File contents in scope",
  metadataOnly: "File names only",
};

export const EFFORT_LABELS: Record<Effort, string> = {
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
};

export interface BudgetRow {
  key: string;
  label: string;
  used: number;
  limit: number;
  /** Human-readable "used of limit". */
  text: string;
}

function compact(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 10_000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

function minutes(ms: number): string {
  const total = Math.round(ms / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

/** Budget meter rows: usage against the task's limits. */
export function budgetRows(task: TaskSummary): BudgetRow[] {
  const { usage, spec } = task;
  const b = spec.budgets;
  const input = usage.inputTokens + usage.cacheReadTokens + usage.cacheWriteTokens;
  return [
    { key: "turns", label: "Turns", used: usage.turns, limit: b.maxTurns, text: `${usage.turns} of ${b.maxTurns}` },
    {
      key: "tools",
      label: "Tool calls",
      used: usage.toolCalls,
      limit: b.maxToolCalls,
      text: `${usage.toolCalls} of ${b.maxToolCalls}`,
    },
    {
      key: "output",
      label: "Output tokens",
      used: usage.outputTokens,
      limit: b.maxOutputTokens,
      text: `${compact(usage.outputTokens)} of ${compact(b.maxOutputTokens)}`,
    },
    {
      key: "input",
      label: "Input tokens",
      used: input,
      limit: b.maxInputTokens,
      text: `${compact(input)} of ${compact(b.maxInputTokens)}`,
    },
    {
      key: "time",
      label: "Time",
      used: usage.elapsedMs,
      limit: b.maxWallMs,
      text: `${minutes(usage.elapsedMs)} of ${minutes(b.maxWallMs)}`,
    },
    {
      key: "commands",
      label: "Commands",
      used: usage.commands,
      limit: b.maxCommands,
      text: `${usage.commands} of ${b.maxCommands}`,
    },
  ];
}

/** One-line description of a tool call's input. */
export function toolSummary(name: string, input: unknown): string {
  const obj = (typeof input === "object" && input !== null ? input : {}) as Record<string, unknown>;
  const str = (v: unknown) => (typeof v === "string" ? v : "");
  switch (name) {
    case "read_file":
    case "check_lean":
      return str(obj.path);
    case "edit_document": {
      const edits = Array.isArray(obj.edits) ? obj.edits.length : 0;
      return `${str(obj.path)} (${edits} edit${edits === 1 ? "" : "s"})`;
    }
    case "create_file":
      return str(obj.path);
    case "list_files":
      return str(obj.path) || "project root";
    case "build_document":
      return str(obj.entry);
    case "run_command": {
      const args = Array.isArray(obj.args) ? obj.args.filter((a): a is string => typeof a === "string") : [];
      return [str(obj.program), ...args].join(" ");
    }
    case "run_python":
      return str(obj.code).split("\n")[0] ?? "";
    default:
      return "";
  }
}

export function relativeTime(ms: number, now = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - ms) / 1000));
  if (seconds < 60) return "just now";
  const mins = Math.round(seconds / 60);
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} h ago`;
  return new Date(ms).toLocaleDateString();
}
