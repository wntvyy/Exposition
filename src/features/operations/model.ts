// Project operations (P1): the frontend's display layer over the Rust
// contract. Everything here is wording and grouping for plans, decisions and
// outcomes; what may happen, the impacts, recovery and identities all come
// from the backend plan (`project_op_plan`) and results (`project_op_apply`).
import { detectPlatform } from "../../app/keybindings";
import type { Tone } from "../../app/notifications";
import type { ImpactKind } from "../../bindings/ImpactKind";
import type { OpChoice } from "../../bindings/OpChoice";
import type { OpConsumer } from "../../bindings/OpConsumer";
import type { OpDestination } from "../../bindings/OpDestination";
import type { OpImpact } from "../../bindings/OpImpact";
import type { OpKind } from "../../bindings/OpKind";
import type { OpOutcome } from "../../bindings/OpOutcome";
import type { OpPlan } from "../../bindings/OpPlan";
import type { OpRecovery } from "../../bindings/OpRecovery";
import type { OpRefusal } from "../../bindings/OpRefusal";
import type { OpResult } from "../../bindings/OpResult";
import type { OpTarget } from "../../bindings/OpTarget";

/** Display names for targets, keyed by `targetKey` (surfaces know their items' names). */
export type TargetLabels = ReadonlyMap<string, string>;

const NO_LABELS: TargetLabels = new Map();

/** A stable key for an operation target (comparisons, label lookup). */
export function targetKey(target: OpTarget): string {
  switch (target.kind) {
    case "path":
    case "external":
      return `${target.kind}:${target.path}`;
    case "workflow":
      return `workflow:${target.workflow}`;
    case "workflowRevision":
      return `workflowRevision:${target.workflow}@${target.revision}`;
    case "workflowRun":
    case "researchRun":
      return `${target.kind}:${target.run}`;
    case "agentTask":
      return `agentTask:${target.task}`;
    case "graphEntity":
      return `graphEntity:${target.entity}`;
    case "graphRelation":
      return `graphRelation:${target.relation}`;
    case "graphView":
      return `graphView:${target.view}`;
    case "recoverable":
      return `recoverable:${target.id}`;
  }
}

export function sameTarget(a: OpTarget, b: OpTarget): boolean {
  return targetKey(a) === targetKey(b);
}

export function baseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const at = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return at >= 0 ? trimmed.slice(at + 1) : trimmed;
}

/** Parent folder of a workspace path (`null` = the project root). */
export function parentFolder(path: string): string | null {
  const at = path.lastIndexOf("/");
  return at > 0 ? path.slice(0, at) : null;
}

/** Short name of a target for titles ("plot.png", "Workflow “Fit”"). */
export function targetName(target: OpTarget, labels: TargetLabels = NO_LABELS): string {
  const known = labels.get(targetKey(target));
  if (known) return known;
  switch (target.kind) {
    case "path":
    case "external":
      return baseName(target.path) || target.path;
    case "workflow":
      return `workflow ${target.workflow}`;
    case "workflowRevision":
      return `revision ${target.revision} of workflow ${target.workflow}`;
    case "workflowRun":
      return `run ${target.run}`;
    case "researchRun":
      return `research run ${target.run}`;
    case "agentTask":
      return `agent task ${target.task}`;
    case "graphEntity":
      return `item ${target.entity}`;
    case "graphRelation":
      return `relationship ${target.relation}`;
    case "graphView":
      return `view ${target.view}`;
    case "recoverable":
      return `deleted item ${target.id}`;
  }
}

/** Longer description for detail lines: the workspace path for files. */
export function targetDetail(target: OpTarget, labels: TargetLabels = NO_LABELS): string {
  if (target.kind === "path" || target.kind === "external") return target.path;
  return targetName(target, labels);
}

export function quoted(text: string): string {
  return `“${text}”`;
}

/** "the Recycle Bin" on Windows, "the Trash" elsewhere. */
export function trashName(): string {
  return detectPlatform() === "windows" ? "the Recycle Bin" : "the Trash";
}

export function folderLabel(folder: string | null): string {
  return folder ? `${folder}/` : "the project folder";
}

export function destinationLabel(destination: OpDestination): string {
  return destination.kind === "folder" ? folderLabel(destination.folder) : "this project";
}

// ----- impacts and decisions -----------------------------------------------------------

/** Headings of impact groups in the decision, in the order they are shown. */
export const IMPACT_HEADINGS: Record<ImpactKind, string> = {
  unsavedBuffer: "Unsaved changes",
  activeWork: "Work in progress",
  enabledTrigger: "Enabled triggers",
  manuscriptDependency: "Used by a manuscript",
  workflowInput: "Required workflow inputs",
  pinnedRevision: "Pinned workflow revisions",
  retainedEvidence: "Retained evidence",
  recoveryLoss: "Cannot be restored",
  savedVersionCopied: "Saved version copied",
};

const IMPACT_ORDER = Object.keys(IMPACT_HEADINGS) as ImpactKind[];

/** Whether an impact needs the user's decision (everything but the saved-version notice). */
export function impactNeedsDecision(kind: ImpactKind): boolean {
  return kind !== "savedVersionCopied";
}

export interface ImpactGroup {
  kind: ImpactKind;
  heading: string;
  impacts: OpImpact[];
}

/** Impacts that need the decision, grouped by kind in a fixed order. */
export function decisionGroups(plan: OpPlan): ImpactGroup[] {
  const groups: ImpactGroup[] = [];
  for (const kind of IMPACT_ORDER) {
    if (!impactNeedsDecision(kind)) continue;
    const impacts = plan.impacts.filter((i) => i.kind === kind);
    if (impacts.length) groups.push({ kind, heading: IMPACT_HEADINGS[kind], impacts });
  }
  return groups;
}

/** Informational notices of a plan (saved versions of dirty sources were copied). */
export function planNotices(plan: OpPlan): string[] {
  return plan.impacts.filter((i) => i.kind === "savedVersionCopied").map((i) => i.message);
}

export function consumerText(consumer: OpConsumer): string {
  return consumer.path && consumer.path !== consumer.label ? `${consumer.label} (${consumer.path})` : consumer.label;
}

/** Initial values for the decision's choices: each choice's default, then explicit presets. */
export function initialChoices(choices: OpChoice[], presets: Record<string, string> = {}): Record<string, string> {
  const values: Record<string, string> = {};
  for (const choice of choices) {
    const preset = presets[choice.key];
    values[choice.key] = preset !== undefined && choice.options.some((o) => o.value === preset) ? preset : choice.default;
  }
  return values;
}

export function isDelete(kind: OpKind): kind is { kind: "delete"; permanent: boolean } {
  return kind.kind === "delete";
}

export function isPermanent(kind: OpKind): boolean {
  return kind.kind === "delete" && kind.permanent;
}

/** Imperative verb of an operation ("Delete", "Delete permanently", "Duplicate", "Paste"). */
export function verbOf(kind: OpKind): string {
  switch (kind.kind) {
    case "delete":
      return kind.permanent ? "Delete permanently" : "Delete";
    case "duplicate":
      return "Duplicate";
    case "paste":
      return "Paste";
  }
}

function countText(count: number, noun = "item"): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

/** What the request acts on: “plot.png” or "3 items". */
export function subjectOf(targets: OpTarget[], labels: TargetLabels = NO_LABELS): string {
  return targets.length === 1 ? quoted(targetName(targets[0]!, labels)) : countText(targets.length);
}

/** The decision's title: names the action and its subject (never "Are you sure?"). */
export function decisionTitle(plan: OpPlan, labels: TargetLabels = NO_LABELS): string {
  const subject = subjectOf(plan.request.targets, labels);
  switch (plan.request.kind.kind) {
    case "delete":
      return plan.request.kind.permanent ? `Permanently delete ${subject}` : `Delete ${subject}`;
    case "duplicate":
      return `Duplicate ${subject}`;
    case "paste":
      return `Paste ${subject} into ${destinationLabel(plan.request.kind.destination)}`;
  }
}

/** The decision's primary action label. */
export function decisionAction(plan: OpPlan): string {
  const acting = plan.effects.length;
  const total = plan.request.targets.length;
  const verb = verbOf(plan.request.kind);
  const base = verb === "Delete permanently" ? "Delete Permanently" : verb;
  return acting < total ? `${base} ${acting} of ${total}` : base;
}

// ----- outcomes ------------------------------------------------------------------------

export function recoveryText(recovery: OpRecovery): string {
  switch (recovery.kind) {
    case "osTrash":
      return `in ${trashName()}`;
    case "appManaged":
      return "restorable from Settings › Storage & History";
    case "none":
      return "not recoverable";
  }
}

/** Codes of a restore refused because the original location is occupied. */
export const COLLISION_CODES = new Set(["ops.occupied", "ops.collision", "ops.exists", "ops.restoreCollision"]);

export function isCollision(outcome: OpOutcome): boolean {
  return outcome.status !== "succeeded" && outcome.code !== null && COLLISION_CODES.has(outcome.code);
}

export const TRASH_UNAVAILABLE = "ops.trashUnavailable";

export interface ResultSummary {
  tone: Tone;
  title: string;
  /** One line per sentence/failure (rendered with line breaks). */
  lines: string[];
  succeeded: OpOutcome[];
  failed: OpOutcome[];
  skipped: OpOutcome[];
  /** Restore tokens of succeeded outcomes (Undo is offered only with these). */
  restoreTokens: { token: string; target: OpTarget }[];
  /** Targets whose ordinary deletion failed because the trash is unavailable. */
  trashUnavailable: OpTarget[];
}

/** What the person can do about a failure code, when the message does not say it already. */
const CODE_HINTS: Record<string, string> = {
  "ops.inUse": "Close the program that is using it, then try again.",
  "ops.interrupted": "The operation was interrupted; check the item before trying again.",
};

function failureLine(outcome: OpOutcome, labels: TargetLabels, skipped: boolean): string {
  const name = quoted(targetDetail(outcome.target, labels));
  const reason = outcome.message ?? outcome.code ?? (skipped ? "not attempted" : "failed");
  const hint = outcome.code ? CODE_HINTS[outcome.code] : undefined;
  const tail = hint && !reason.toLowerCase().includes(hint.split(" ")[0]!.toLowerCase()) ? ` ${hint}` : "";
  return `${name}: ${skipped ? "not changed — " : ""}${reason}${tail}`;
}

function successVerb(kind: OpKind, succeeded: OpOutcome[]): string {
  if (kind.kind === "duplicate") return "Duplicated";
  if (kind.kind === "paste") return "Pasted";
  if (kind.permanent) return "Permanently deleted";
  if (succeeded.length > 0 && succeeded.every((o) => o.recovery.kind === "osTrash")) return "Moved";
  return "Deleted";
}

/**
 * Summarize an apply result for the non-modal notification: actual partial
 * success and failure, recovery truthfully described, and the restore tokens
 * that allow Undo.
 */
export function summarizeResult(
  kind: OpKind,
  result: OpResult,
  labels: TargetLabels = NO_LABELS,
  refusals: OpRefusal[] = [],
): ResultSummary {
  const outcomes = [...result.outcomes];
  // A refused target that the apply did not report is still part of the request.
  for (const refusal of refusals) {
    if (!outcomes.some((o) => sameTarget(o.target, refusal.target))) {
      outcomes.push({
        target: refusal.target,
        status: "skipped",
        code: refusal.code,
        message: refusal.message,
        created: null,
        recovery: { kind: "none" },
        restore: null,
      });
    }
  }
  const succeeded = outcomes.filter((o) => o.status === "succeeded");
  const failed = outcomes.filter((o) => o.status === "failed");
  const skipped = outcomes.filter((o) => o.status === "skipped");
  const total = outcomes.length;
  const verb = successVerb(kind, succeeded);
  const trash = trashName();
  const restoreTokens = succeeded.flatMap((o) => (o.restore ? [{ token: o.restore, target: o.target }] : []));
  const trashUnavailable = failed.filter((o) => o.code === TRASH_UNAVAILABLE).map((o) => o.target);

  let title: string;
  const lines: string[] = [];
  const where = kind.kind === "paste" ? ` into ${destinationLabel(kind.destination)}` : "";
  const toTrash = verb === "Moved" ? ` to ${trash}` : "";
  if (succeeded.length === 0) {
    const name = total === 1 ? quoted(targetName(outcomes[0]!.target, labels)) : countText(total);
    title = `Could not ${verbOf(kind).toLowerCase()} ${name}`;
  } else if (succeeded.length === total) {
    if (total === 1) {
      const only = succeeded[0]!;
      const name = quoted(targetName(only.target, labels));
      if (kind.kind === "duplicate" && only.created) title = `Duplicated ${name} as ${quoted(targetName(only.created, labels))}`;
      else title = `${verb} ${name}${toTrash}${where}`;
    } else {
      title = `${verb} ${countText(total)}${toTrash}${where}`;
    }
  } else {
    title = `${verb} ${succeeded.length} of ${countText(total)}${toTrash}${where}`;
  }

  if (kind.kind === "delete" && succeeded.length > 0) {
    if (kind.permanent) {
      lines.push(succeeded.length === 1 ? "It cannot be restored." : "They cannot be restored.");
    } else {
      const inTrash = succeeded.filter((o) => o.recovery.kind === "osTrash");
      const managed = succeeded.filter((o) => o.recovery.kind === "appManaged");
      const none = succeeded.filter((o) => o.recovery.kind === "none");
      const undoable = new Set(restoreTokens.map((t) => targetKey(t.target)));
      const trashNoUndo = inTrash.filter((o) => !undoable.has(targetKey(o.target)));
      if (inTrash.length && verb !== "Moved") lines.push(`${countText(inTrash.length)} moved to ${trash}.`);
      if (trashNoUndo.length) {
        lines.push(
          trashNoUndo.length === inTrash.length && inTrash.length === succeeded.length
            ? `Restore ${succeeded.length === 1 ? "it" : "them"} from ${trash} if needed.`
            : `${countText(trashNoUndo.length)} can be restored from ${trash}.`,
        );
      }
      if (managed.length) {
        lines.push(
          managed.length === succeeded.length
            ? `${succeeded.length === 1 ? "It" : "They"} can be restored from Settings › Storage & History.`
            : `${countText(managed.length)} can be restored from Settings › Storage & History.`,
        );
      }
      if (none.length) lines.push(`${countText(none.length)} cannot be restored.`);
    }
  }
  if ((kind.kind === "duplicate" || kind.kind === "paste") && succeeded.length > 1) {
    const created = succeeded.filter((o) => o.created).map((o) => targetDetail(o.created!, labels));
    if (created.length) lines.push(`Created ${created.join(", ")}.`);
  }
  for (const outcome of failed) lines.push(failureLine(outcome, labels, false));
  for (const outcome of skipped) lines.push(failureLine(outcome, labels, true));
  if (trashUnavailable.length) {
    lines.push(`${trash.charAt(0).toUpperCase()}${trash.slice(1)} is unavailable, so nothing was deleted permanently. Use Delete Permanently to remove ${trashUnavailable.length === 1 ? "it" : "them"} without recovery.`);
  }

  const tone: Tone = succeeded.length === 0 ? "danger" : failed.length || skipped.length ? "warning" : "info";
  return { tone, title, lines, succeeded, failed, skipped, restoreTokens, trashUnavailable };
}

/** Title for a restore outcome; mentions a new name when the original location was occupied. */
export function restoreTitle(original: OpTarget, outcome: OpOutcome, labels: TargetLabels = NO_LABELS): string {
  const name = quoted(targetName(original, labels));
  if (outcome.status !== "succeeded") return `Could not restore ${name}`;
  const restoredAt = outcome.created ?? outcome.target;
  if (!sameTarget(restoredAt, original)) return `Restored ${name} as ${quoted(targetName(restoredAt, labels))}`;
  return `Restored ${name}`;
}

/** Sizes in binary units (KiB, MiB, GiB); `null` = not measurable. */
export function formatBytes(size: number | null): string {
  if (size === null) return "not measured";
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KiB`;
  if (size < 1024 * 1024 * 1024) return `${(size / 1024 / 1024).toFixed(1)} MiB`;
  return `${(size / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}
