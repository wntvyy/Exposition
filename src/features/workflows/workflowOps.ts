// Operations on saved workflows and workflow runs (P1).
//
// Workflows: Copy (app clipboard) / Paste / Duplicate make NEW workflows (new
// id, revision 1, new step ids, internal connections remapped, no triggers,
// runs, approvals or verdicts — the backend owns the copy); Delete removes it
// from active use and disables its triggers (restorable from Settings ›
// Storage & History, triggers stay off after a restore); Delete Permanently
// purges it (revisions that surviving composites pin are kept unless the
// person includes them in the decision).
// Runs: Delete removes the record only; outputs are a separate, explicit
// scope ("…and its outputs") that is always reviewed. Active runs are
// coordinated through the plan's choices (stop first, or cancel).
import { FileOutput } from "lucide-react";

import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import type { OpTarget } from "../../bindings/OpTarget";
import type { WorkflowSummary } from "../../bindings/WorkflowSummary";
import { runUserOperation } from "../operations/client";
import { appClipboard, writeText } from "../operations/clipboard";
import { quoted, targetKey } from "../operations/model";
import type { ExtraItem } from "../operations/OpMenu";
import type { SurfaceActions } from "../operations/surfaces";
import type { WorkflowRunSummary } from "./contract";
import { workflowKey } from "./model";

function plural(n: number, one: string, many: string) {
  return n === 1 ? one : many;
}

/** Actions for selected saved workflows (Workflows list). */
export function workflowActions(selected: WorkflowSummary[]): { actions: SurfaceActions } {
  const has = selected.length > 0;
  const none = "Select a workflow first";
  const targets: OpTarget[] = selected.map((w) => ({ kind: "workflow", workflow: w.id }));
  const labels = new Map(selected.map((w) => [targetKey({ kind: "workflow", workflow: w.id }), w.title]));
  const clip = appClipboard.get();
  const clipped = clip?.kind === "workflows" ? clip.workflows : [];
  return {
    actions: {
      copy: {
        enabled: has,
        reason: none,
        description: "Paste makes a new, separate workflow",
        run: async () => {
          appClipboard.set({ kind: "workflows", workflows: selected.map((w) => ({ id: w.id, title: w.title })) });
          await writeText(selected.map((w) => w.title).join("\n"));
          notify({ tone: "info", title: selected.length === 1 ? `Copied workflow ${quoted(selected[0]!.title)}` : `Copied ${selected.length} workflows` });
        },
      },
      paste: {
        enabled: clipped.length > 0,
        reason: "Copy a workflow first",
        label: clipped.length ? `Paste ${plural(clipped.length, "Workflow", `${clipped.length} Workflows`)}` : "Paste",
        description: "New drafts with no triggers, runs, or approvals",
        run: async () => {
          const current = appClipboard.get();
          if (current?.kind !== "workflows") return;
          await runUserOperation(
            { kind: "paste", destination: { kind: "project" } },
            current.workflows.map((w) => ({ kind: "workflow", workflow: w.id })),
            { labels: new Map(current.workflows.map((w) => [targetKey({ kind: "workflow", workflow: w.id }), w.title])) },
          );
        },
      },
      duplicate: {
        enabled: has,
        reason: none,
        description: "A new draft; triggers, runs, and approvals are not copied",
        run: async () => {
          await runUserOperation({ kind: "duplicate" }, targets, { labels });
        },
      },
      delete: {
        enabled: has,
        reason: none,
        description: "Disables its triggers; restorable from Storage & History",
        run: async () => {
          closeDeletedTabs(await runUserOperation({ kind: "delete", permanent: false }, targets, { labels }));
        },
      },
      deletePermanently: {
        enabled: has,
        reason: none,
        description: "Cannot be restored",
        run: async () => {
          closeDeletedTabs(await runUserOperation({ kind: "delete", permanent: true }, targets, { labels }));
        },
      },
    },
  };
}

const ACTIVE_RUN = new Set(["running"]);

/** Actions for selected workflow runs (records; outputs only by explicit choice). */
export function runActions(selected: WorkflowRunSummary[]): { actions: SurfaceActions; extras: ExtraItem[] } {
  const has = selected.length > 0;
  const none = "Select a run first";
  const targets: OpTarget[] = selected.map((r) => ({ kind: "workflowRun", run: r.id }));
  const labels = new Map(selected.map((r) => [targetKey({ kind: "workflowRun", run: r.id }), `run of ${r.title}`]));
  const active = selected.some((r) => ACTIVE_RUN.has(r.state));
  // A record delete never deletes outputs; "record and outputs" is both targets in one request (one decision).
  const outputTargets: OpTarget[] = selected.map((r) => ({ kind: "runOutputs", run: r.id }));
  for (const r of selected) labels.set(targetKey({ kind: "runOutputs", run: r.id }), `outputs of ${r.title}`);
  const remove = (permanent: boolean, outputs: boolean) => async () => {
    await runUserOperation({ kind: "delete", permanent }, outputs ? [...targets, ...outputTargets] : targets, { labels });
  };
  return {
    actions: {
      copy: { enabled: false, reason: "A run is not copied; review a new run from its workflow", run: () => {} },
      delete: {
        enabled: has,
        reason: none,
        label: plural(selected.length, "Delete Run Record", "Delete Run Records"),
        description: active ? "Running work is stopped first if you choose" : "The record only; outputs stay",
        run: remove(false, false),
      },
      deletePermanently: { enabled: has, reason: none, description: "The record cannot be restored; outputs stay", run: remove(true, false) },
    },
    extras: [
      {
        id: "deleteWithOutputs",
        label: plural(selected.length, "Delete Run and Its Outputs", "Delete Runs and Their Outputs"),
        icon: FileOutput,
        description: "Outputs are held for restore (their files stay on disk until purged); never inputs or files the project keeps",
        isDisabled: !has,
        run: remove(false, true),
      },
      {
        id: "deleteWithOutputsPermanently",
        label: plural(selected.length, "Delete Run and Its Outputs Permanently…", "Delete Runs and Their Outputs Permanently…"),
        icon: FileOutput,
        description: "Removes the record and the output files; cannot be restored",
        isDisabled: !has,
        run: remove(true, true),
      },
    ],
  };
}

/** Close clean editor tabs of workflows that were deleted (a tab with unsaved edits stays open). */
function closeDeletedTabs(outcome: Awaited<ReturnType<typeof runUserOperation>>): void {
  for (const o of outcome?.result.outcomes ?? []) {
    if (o.status !== "succeeded" || o.target.kind !== "workflow") continue;
    const key = workflowKey(o.target.workflow);
    const open = editors.all().find((e) => e.resource.key === key);
    if (open && !open.dirty) void editors.close(key);
  }
}
