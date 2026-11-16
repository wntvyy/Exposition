// Labels and derived values for the agent panel (pure; unit tested).
import type { AllowanceKind } from "../../bindings/AllowanceKind";
import type { ApprovalMode } from "../../bindings/ApprovalMode";
import type { AuthorizedBy } from "../../bindings/AuthorizedBy";
import type { BudgetKind } from "../../bindings/BudgetKind";
import type { CodeAccess } from "../../bindings/CodeAccess";
import type { FilesAccess } from "../../bindings/FilesAccess";
import type { TaskSpec } from "../../bindings/TaskSpec";
import type { ContextConsent } from "../../bindings/ContextConsent";
import type { Effort } from "../../bindings/Effort";
import type { ExecutionMode } from "../../bindings/ExecutionMode";
import type { PermissionProfile } from "../../bindings/PermissionProfile";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
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

/** Short label for a response that stopped without finishing its turn. */
export function stopLabel(stopReason: string): string {
  switch (stopReason) {
    case "max_tokens":
      return "Stopped: response cut off";
    case "model_context_window_exceeded":
      return "Stopped: context window full";
    case "pause_turn":
      return "Stopped: response paused";
    default:
      return "Stopped: unexpected response";
  }
}

/**
 * A backend detail as a sentence of its own: budget details are worded to
 * follow "Stopped: " ("the limit of 40 model turns was reached."), so they
 * start in lower case.
 */
export function asSentence(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * The one phrase for "blocked on the user's decision", used by the task
 * status, the tool call, the streaming draft, and the status bar alike.
 */
export const WAITING_FOR_APPROVAL = "Waiting for approval";

/** Task status in the shared status vocabulary, with a specific label. */
export function statusLabel(status: TaskStatus): PillState {
  switch (status.kind) {
    case "queued":
      return { status: "running", label: "Queued" };
    case "running":
      return { status: "running", label: "Agent working" };
    case "awaitingApproval":
      return { status: "warning", label: WAITING_FOR_APPROVAL };
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
    case "stopped":
      return { status: "warning", label: stopLabel(status.stopReason) };
    case "interrupted":
      return { status: "canceled", label: "Interrupted" };
  }
}

/** Tool call status; unavailable services are shown as unavailable, not failed. */
export function toolStatusLabel(status: ToolCallStatus, result: string | null = null): PillState {
  switch (status) {
    case "pending":
      return { status: "idle", label: "Pending" };
    case "awaitingApproval":
      return { status: "warning", label: WAITING_FOR_APPROVAL };
    case "running":
      return { status: "running", label: "Running" };
    case "succeeded":
      return { status: "succeeded", label: "Done" };
    case "failed":
      return result?.includes("not available in this build")
        ? { status: "unavailable", label: "Unavailable" }
        : { status: "failed", label: "Failed" };
    case "declined":
      return { status: "canceled", label: "Declined" };
    case "canceled":
      return { status: "canceled", label: "Canceled" };
    case "notRun":
      // Paused by the application because the project graph could not be used.
      if (result?.includes('"code":"agents.graphUnavailable"')) return { status: "unavailable", label: "Paused: graph unavailable" };
      if (result?.includes('"code":"agents.graphPending"')) return { status: "stale", label: "Paused: graph not reconciled" };
      if (result?.includes('"code":"agents.graphAmbiguous"')) return { status: "warning", label: "Paused: ambiguous in graph" };
      return { status: "canceled", label: "Not run" };
  }
}

/** The live pill for the tool the agent is on, e.g. "Waiting for approval: create_file". */
export function activeToolLabel(name: string, status: "running" | "awaitingApproval"): PillState {
  const pill = toolStatusLabel(status);
  return { status: pill.status, label: `${pill.label}: ${name}` };
}

/** Status bar text while tasks wait for the user. */
export function waitingLabel(count: number): string {
  return count === 1 ? `Agent: ${WAITING_FOR_APPROVAL.toLowerCase()}` : `Agent: ${count} ${WAITING_FOR_APPROVAL.toLowerCase()}`;
}

/** The provider actually selected, and whether it can run tasks. */
export function providerLabel(status: ProviderStatus): { text: string; ready: boolean } {
  if (status.settings.provider === "scripted") {
    return { text: "Scripted provider — development only", ready: true };
  }
  if (status.credentialError) return { text: "Claude · credential store error", ready: false };
  return status.configured
    ? { text: `Claude · ${status.settings.model}`, ready: true }
    : { text: "Claude · no API key configured", ready: false };
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

export const APPROVAL_MODE_LABELS: Record<ApprovalMode, string> = {
  ask: "Ask",
  auto: "Auto",
};

/** Files access of a project's Auto permissions. */
export const FILES_LABELS: Record<FilesAccess, string> = {
  edit: "Edit files in scope",
  off: "Ask before each change",
};

/** Code access of a project's Auto permissions. */
export const CODE_LABELS: Record<CodeAccess, string> = {
  off: "Ask before running code",
  restricted: "Restricted container",
  native: "Native on this computer",
};

const FILES_SHORT: Record<FilesAccess, string> = { edit: "edit", off: "ask" };
const CODE_SHORT: Record<CodeAccess, string> = { off: "ask", restricted: "restricted container", native: "native" };

/** One line for a project's permissions: "Files: edit · Code: restricted container · Setup: off". */
export function profileSummary(p: { files: FilesAccess; code: CodeAccess; setup: boolean }): string {
  return [`Files: ${FILES_SHORT[p.files]}`, `Code: ${CODE_SHORT[p.code]}`, `Setup: ${p.setup ? "on" : "off"}`].join(" · ");
}

/** What running code can actually access under a code access level, stated as enforced. */
export function environmentText(
  code: CodeAccess,
  facts: { trusted: boolean; restrictedAvailable: boolean },
): { tone: "info" | "warning"; text: string }[] {
  switch (code) {
    case "native":
      return [
        {
          tone: "warning",
          text: "Runs on this computer with your account's privileges — not a sandbox; Exposition cannot confirm each action inside running code.",
        },
        ...(facts.trusted
          ? []
          : [{ tone: "warning" as const, text: "This folder is not trusted, so native code is refused until you trust it." }]),
      ];
    case "restricted":
      return facts.restrictedAvailable
        ? [
            {
              tone: "info",
              text: "Runs in the restricted container: no network, read-only project, non-root, no host environment; never falls back to native.",
            },
          ]
        : [
            {
              tone: "warning",
              text: "No container image is configured, so restricted runs are refused; they never run natively instead.",
            },
          ];
    case "off":
      return [{ tone: "info", text: "Running code asks each time; approving runs it natively on this computer." }];
  }
}

/** Allowance kinds as nouns ("Allowed for this task: Native code"). */
export const ALLOWANCE_LABELS: Record<AllowanceKind, string> = {
  nativeCode: "Native code",
  documentChanges: "File changes in scope",
  packageSetup: "Package installs",
};

function listJoin(items: string[]): string {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

/** What "Approve for this task" also covers. */
export function forTaskSentence(kinds: AllowanceKind[]): string {
  const what = listJoin(kinds.map((k) => ALLOWANCE_LABELS[k].toLowerCase()));
  return `Approve for this task also covers later ${what} in this task; revoke it from the task.`;
}

/** How a call that ran was authorized (quiet text); `null` when nothing needed authorizing. */
export function authorizationLabel(by: AuthorizedBy | null | undefined): string | null {
  if (!by) return null;
  switch (by.kind) {
    case "notRequired":
      return null;
    case "taskSettings":
      return "Allowed by task settings";
    case "projectPermissions":
      return "Auto: project permissions";
    case "taskAllowance":
      return "Allowed for this task";
    case "approved":
      return by.forTask ? "Approved by you for this task" : "Approved by you";
  }
}

/** The task's settings for its meta line (joined with middle dots by the caller). */
export function specLabels(spec: TaskSpec): string[] {
  if (spec.approvalMode === "auto") {
    const ceiling = [
      spec.permission === "readOnly" ? PERMISSION_LABELS.readOnly : null,
      spec.execution === "disabled" ? EXECUTION_LABELS.disabled : null,
      spec.execution === "restricted" ? EXECUTION_LABELS.restricted : null,
    ].filter((part): part is string => part !== null);
    return ["Auto: project permissions", ...ceiling, CONSENT_LABELS[spec.consent]];
  }
  return [PERMISSION_LABELS[spec.permission], EXECUTION_LABELS[spec.execution], CONSENT_LABELS[spec.consent]];
}

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
    case "save_document":
      return str(obj.path);
    case "list_files":
      return str(obj.path) || "project root";
    case "build_document":
      return str(obj.entry);
    case "graph_query":
      return [str(obj.query), str(obj.text) || str(obj.path) || str(obj.entity)].filter(Boolean).join(" ");
    case "graph_propose": {
      const ops = Array.isArray(obj.operations) ? obj.operations.length : 0;
      return `${ops} operation${ops === 1 ? "" : "s"}${str(obj.summary) ? ` — ${str(obj.summary)}` : ""}`;
    }
    case "insert_artifact":
      return `${str(obj.path)} ← ${str(obj.artifact_id)}`;
    case "run_command": {
      const args = Array.isArray(obj.args) ? obj.args.filter((a): a is string => typeof a === "string") : [];
      return [str(obj.program), ...args].join(" ");
    }
    case "run_python":
      return str(obj.code).split("\n")[0] ?? "";
    case "install_packages":
      return Array.isArray(obj.packages) ? obj.packages.filter((p): p is string => typeof p === "string").join(" ") : "";
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
