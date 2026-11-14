// The shared operation client (P1): every surface (explorers, graph,
// workflows, runs, agent tasks, Settings › Storage & History) deletes,
// duplicates and pastes through this one path:
//
//   plan → (ONE combined decision only when the plan needs it) → apply →
//   non-modal outcome notification (actual partial success/failure, Undo only
//   with restore tokens, saved-version notices).
//
// Routine operations therefore run immediately; the frontend never decides
// filesystem authority, impacts or recovery (the backend plan does) and never
// falls back to anything the plan did not offer. Undo/Redo restore and
// re-delete through the same contract; restores never overwrite (a collision
// asks before using a fresh name).
import { notify, type NotificationAction } from "../../app/notifications";
import type { OpKind } from "../../bindings/OpKind";
import type { OpOutcome } from "../../bindings/OpOutcome";
import type { OpPlan } from "../../bindings/OpPlan";
import type { OpRequest } from "../../bindings/OpRequest";
import type { OpResult } from "../../bindings/OpResult";
import type { OpTarget } from "../../bindings/OpTarget";
import { errorMessage } from "../../lib/ipc";
import { confirm } from "../../ui/components";
import { requestDecision } from "./decision";
import { opCommands, opEvents } from "./ipc";
import {
  isCollision,
  isDelete,
  planNotices,
  quoted,
  restoreTitle,
  summarizeResult,
  targetKey,
  targetName,
  verbOf,
  type TargetLabels,
} from "./model";

export interface RunOptions {
  /** Display names of the targets (by `targetKey`). */
  labels?: TargetLabels;
  /** Choice values chosen up front (an explicitly scoped action). */
  presetChoices?: Record<string, string>;
  /** Always show the decision (explicit scope such as "and its outputs"), even when the plan needs none. */
  review?: boolean;
  /** Leave the result notification to the caller. */
  quiet?: boolean;
  /**
   * Follow-up actions for refused or skipped targets (e.g. "Copy Text" when a
   * source-anchored item cannot be duplicated, "Delete Source File" for a
   * file-backed item), offered in the notification.
   */
  onRefused?: (refusals: { code: string | null; target: OpTarget }[]) => NotificationAction[];
}

export interface OperationOutcome {
  plan: OpPlan;
  result: OpResult;
}

function errorCodeOf(error: unknown): string | null {
  return typeof error === "object" && error !== null && "code" in error ? String((error as { code: unknown }).code) : null;
}

// ----- project-op fan-out ----------------------------------------------------------------

type Listener = (result: OpResult) => void;
const listeners = new Set<Listener>();
const recent: string[] = [];
let listening = false;

function signature(result: OpResult): string {
  return `${result.plan}|${result.outcomes.map((o) => `${targetKey(o.target)}:${o.status}`).join(",")}`;
}

function fanOut(result: OpResult): void {
  const sig = signature(result);
  if (recent.includes(sig)) return;
  recent.push(sig);
  if (recent.length > 20) recent.shift();
  for (const listener of [...listeners]) {
    try {
      listener(result);
    } catch (error) {
      console.error("project-op listener failed", error);
    }
  }
}

function ensureListening(): void {
  if (listening) return;
  listening = true;
  try {
    void opEvents.projectOp(fanOut).catch(() => {
      // Not running inside the desktop shell (unit tests).
    });
  } catch {
    // Not running inside the desktop shell (unit tests).
  }
}

/**
 * Subscribe to completed operations (from any surface, agents included, via
 * the `project-op` event; local applies are delivered at once too). Views
 * re-read what they show. Returns an unsubscribe function.
 */
export function onProjectOp(listener: Listener): () => void {
  ensureListening();
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Paths touched by a result (targets and created copies), for view refreshes. */
export function touchedPaths(result: OpResult): string[] {
  const paths = new Set<string>();
  for (const outcome of result.outcomes) {
    for (const t of [outcome.target, outcome.created]) if (t?.kind === "path") paths.add(t.path);
  }
  return [...paths];
}

// ----- history (Undo / Redo) --------------------------------------------------------------

interface HistoryEntry {
  title: string;
  /** Restore tokens with the targets they bring back. */
  restores: { token: string; target: OpTarget }[];
  labels: TargetLabels;
}

interface RedoEntry {
  title: string;
  /** What Undo restored (deleted again, recoverably, on Redo). */
  targets: OpTarget[];
  labels: TargetLabels;
}

const undoStack: HistoryEntry[] = [];
const redoStack: RedoEntry[] = [];
const MAX_HISTORY = 20;

export const opHistory = {
  canUndo: () => undoStack.length > 0,
  canRedo: () => redoStack.length > 0,
  undoTitle: () => undoStack.at(-1)?.title ?? null,
  redoTitle: () => redoStack.at(-1)?.title ?? null,
  /** Test helper. */
  clear: () => {
    undoStack.length = 0;
    redoStack.length = 0;
  },
};

function record(entry: HistoryEntry): void {
  undoStack.push(entry);
  if (undoStack.length > MAX_HISTORY) undoStack.shift();
  redoStack.length = 0;
}

// ----- busy guard -------------------------------------------------------------------------

let inFlight = 0;

/** True while an operation is planning, waiting for its decision, or applying. */
export function operationInFlight(): boolean {
  return inFlight > 0;
}

// ----- run --------------------------------------------------------------------------------

async function planOrReport(request: OpRequest): Promise<OpPlan | null> {
  try {
    return await opCommands.plan(request);
  } catch (error) {
    notify({
      tone: "danger",
      title: `Could not ${verbOf(request.kind).toLowerCase()} ${request.targets.length === 1 ? quoted(targetName(request.targets[0]!)) : `${request.targets.length} items`}`,
      body: errorMessage(error),
    });
    return null;
  }
}

function reportNothingToDo(plan: OpPlan, labels: TargetLabels, options: RunOptions): void {
  const summary = summarizeResult(plan.request.kind, { plan: plan.plan, outcomes: [], notices: [] }, labels, plan.refusals);
  const actions = options.onRefused?.(plan.refusals.map((r) => ({ code: r.code, target: r.target }))) ?? [];
  notify({
    tone: "warning",
    title: `Nothing to ${verbOf(plan.request.kind).toLowerCase()}`,
    body: summary.lines.join("\n"),
    actions: actions.length ? actions : undefined,
    timeoutMs: null,
  });
}

/**
 * Plan, decide when needed, apply, and report. Resolves with the plan and the
 * result, or `null` when nothing was applied (refused, cancelled, failed to
 * plan, or another operation is still in flight).
 */
export async function runOperation(request: OpRequest, options: RunOptions = {}): Promise<OperationOutcome | null> {
  // A held key or a double invocation must not start a second operation.
  if (inFlight > 0) return null;
  inFlight += 1;
  try {
    return await runUnguarded(request, options);
  } finally {
    inFlight -= 1;
  }
}

async function runUnguarded(request: OpRequest, options: RunOptions): Promise<OperationOutcome | null> {
  const labels = options.labels ?? new Map<string, string>();
  let note: string | undefined;
  for (let attempt = 0; attempt < 3; attempt++) {
    const plan = await planOrReport(request);
    if (!plan) return null;
    if (plan.effects.length === 0) {
      reportNothingToDo(plan, labels, options);
      return null;
    }
    let choices: Record<string, string> = { ...(options.presetChoices ?? {}) };
    if (plan.needsDecision || options.review || note) {
      const decided = await requestDecision(plan, { labels, presetChoices: options.presetChoices, note });
      if (!decided) return null; // Cancel changes nothing.
      choices = decided;
    } else {
      // Only values for choices the plan offers.
      choices = Object.fromEntries(Object.entries(choices).filter(([key]) => plan.choices.some((c) => c.key === key)));
    }
    let result: OpResult;
    try {
      result = await opCommands.apply({ plan: plan.plan, planHash: plan.planHash, choices });
    } catch (error) {
      const code = errorCodeOf(error);
      if (code === "ops.planStale" || code === "ops.planUnknown" || code === "ops.planExpired") {
        // Something changed between plan and apply: re-plan; never apply blindly.
        note = "The items or their consequences changed since this was planned.";
        continue;
      }
      notify({ tone: "danger", title: `Could not ${verbOf(request.kind).toLowerCase()} ${request.targets.length === 1 ? quoted(targetName(request.targets[0]!, labels)) : `${request.targets.length} items`}`, body: errorMessage(error) });
      return null;
    }
    fanOut(result);
    if (!options.quiet) report(plan, result, labels, options);
    return { plan, result };
  }
  notify({ tone: "warning", title: "Nothing was changed", body: "The items kept changing while this was being planned. Try again." });
  return null;
}

function report(plan: OpPlan, result: OpResult, labels: TargetLabels, options: RunOptions): void {
  const kind = plan.request.kind;
  const summary = summarizeResult(kind, result, labels, plan.refusals);
  const actions: NotificationAction[] = [];
  const notDone = [...summary.failed, ...summary.skipped].map((o) => ({ code: o.code, target: o.target }));
  const followUps = notDone.length && options.onRefused ? options.onRefused(notDone) : [];
  if (summary.restoreTokens.length > 0) {
    const entry: HistoryEntry = { title: summary.title, restores: summary.restoreTokens, labels };
    record(entry);
    actions.push({ label: "Undo", run: () => void undoEntry(entry) });
  }
  if (summary.trashUnavailable.length > 0 && isDelete(kind) && !kind.permanent) {
    const targets = summary.trashUnavailable;
    actions.push({
      label: "Delete Permanently…",
      run: () => void runOperation({ kind: { kind: "delete", permanent: true }, targets, actor: { kind: "user" } }, { labels }),
    });
  }
  actions.push(...followUps);
  notify({
    tone: summary.tone,
    title: summary.title,
    body: summary.lines.length ? summary.lines.join("\n") : undefined,
    actions: actions.length ? actions : undefined,
    // Failures stay until dismissed; routine results fade.
    timeoutMs: summary.tone === "info" ? (actions.length ? 10_000 : undefined) : null,
  });
  const notices = [...new Set([...planNotices(plan), ...result.notices])];
  if (notices.length) {
    notify({ tone: "info", title: kind.kind === "delete" ? "Note" : "Saved version copied", body: notices.join("\n"), timeoutMs: 10_000 });
  }
}

// ----- restore -----------------------------------------------------------------------------

/**
 * Restore app-known deletions. Never overwrites: a first attempt refuses an
 * occupied original location; the person then chooses a fresh name (both
 * versions kept) or leaving it deleted.
 */
export async function restore(
  restores: { token: string; target: OpTarget }[],
  labels: TargetLabels = new Map(),
): Promise<{ target: OpTarget; outcome: OpOutcome }[]> {
  const done: { target: OpTarget; outcome: OpOutcome }[] = [];
  const collisions: { token: string; target: OpTarget }[] = [];
  for (const item of restores) {
    try {
      const outcome = await opCommands.restore({ restore: item.token, onCollision: "cancel" });
      if (isCollision(outcome)) collisions.push(item);
      else done.push({ target: item.target, outcome });
    } catch (error) {
      done.push({
        target: item.target,
        outcome: { target: item.target, status: "failed", code: errorCodeOf(error), message: errorMessage(error), created: null, recovery: { kind: "none" }, restore: item.token },
      });
    }
  }
  if (collisions.length) {
    const names = collisions.map((c) => quoted(targetName(c.target, labels))).join(", ");
    const choice = await confirm<"keep" | "rename">({
      title: collisions.length === 1 ? `${names} already exists` : `${collisions.length} original locations are occupied`,
      message:
        collisions.length === 1
          ? `Something new is at the original location of ${names}. Restore the deleted version under a new name (both are kept), or leave it deleted.`
          : `New items now occupy the original locations of ${names}. Restore the deleted versions under new names (both versions are kept), or leave them deleted.`,
      actions: [
        { id: "keep", label: "Leave Deleted", variant: "secondary" },
        { id: "rename", label: "Restore with a New Name", variant: "primary" },
      ],
      cancelId: "keep",
      defaultId: "rename",
      tone: "warning",
      testId: "op-restore-collision",
    });
    for (const item of collisions) {
      if (choice === "rename") {
        try {
          done.push({ target: item.target, outcome: await opCommands.restore({ restore: item.token, onCollision: "rename" }) });
        } catch (error) {
          done.push({
            target: item.target,
            outcome: { target: item.target, status: "failed", code: errorCodeOf(error), message: errorMessage(error), created: null, recovery: { kind: "none" }, restore: item.token },
          });
        }
      } else {
        done.push({
          target: item.target,
          outcome: { target: item.target, status: "skipped", code: "ops.occupied", message: "Left deleted: its original location is occupied.", created: null, recovery: { kind: "none" }, restore: item.token },
        });
      }
    }
  }
  if (done.length) fanOut({ plan: `restore:${restores.map((r) => r.token).join(",")}`, outcomes: done.map((d) => d.outcome), notices: [] });
  return done;
}

function reportRestore(results: { target: OpTarget; outcome: OpOutcome }[], labels: TargetLabels): void {
  const ok = results.filter((r) => r.outcome.status === "succeeded");
  const notOk = results.filter((r) => r.outcome.status !== "succeeded");
  const title =
    results.length === 1
      ? restoreTitle(results[0]!.target, results[0]!.outcome, labels)
      : ok.length === results.length
        ? `Restored ${results.length} items`
        : `Restored ${ok.length} of ${results.length} items`;
  const lines = [
    ...(results.length > 1 ? ok.filter((r) => restoreTitle(r.target, r.outcome, labels).includes(" as ")).map((r) => restoreTitle(r.target, r.outcome, labels) + ".") : []),
    ...notOk.map((r) => `${quoted(targetName(r.target, labels))}: ${r.outcome.message ?? r.outcome.code ?? "not restored"}`),
  ];
  notify({
    tone: ok.length === 0 ? "danger" : notOk.length ? "warning" : "info",
    title,
    body: lines.length ? lines.join("\n") : undefined,
    timeoutMs: notOk.length ? null : undefined,
  });
}

async function undoEntry(entry: HistoryEntry): Promise<void> {
  const index = undoStack.indexOf(entry);
  if (index >= 0) undoStack.splice(index, 1);
  inFlight += 1;
  try {
    const results = await restore(entry.restores, entry.labels);
    reportRestore(results, entry.labels);
    const back = results.filter((r) => r.outcome.status === "succeeded").map((r) => r.outcome.created ?? r.outcome.target);
    if (back.length) redoStack.push({ title: entry.title, targets: back, labels: entry.labels });
  } finally {
    inFlight -= 1;
  }
}

/** Undo the most recent operation that can be undone (restore with its tokens). */
export async function undoLast(): Promise<boolean> {
  const entry = undoStack.at(-1);
  if (!entry || inFlight > 0) return false;
  await undoEntry(entry);
  return true;
}

/** Redo: delete again (recoverably) what the last Undo restored. */
export async function redoLast(): Promise<boolean> {
  const entry = redoStack.pop();
  if (!entry || inFlight > 0) return false;
  const kind: OpKind = { kind: "delete", permanent: false };
  const outcome = await runOperation({ kind, targets: entry.targets, actor: { kind: "user" } }, { labels: entry.labels });
  return outcome !== null;
}

/** Convenience: run a user operation of `kind` on `targets`. */
export function runUserOperation(kind: OpKind, targets: OpTarget[], options: RunOptions = {}): Promise<OperationOutcome | null> {
  return runOperation({ kind, targets, actor: { kind: "user" } }, options);
}

/** Restore app-managed deletions chosen in Storage & History, and report the outcome. */
export async function restoreAndReport(restores: { token: string; target: OpTarget }[], labels: TargetLabels = new Map()): Promise<number> {
  if (inFlight > 0) return 0;
  inFlight += 1;
  try {
    const results = await restore(restores, labels);
    reportRestore(results, labels);
    return results.filter((r) => r.outcome.status === "succeeded").length;
  } finally {
    inFlight -= 1;
  }
}
