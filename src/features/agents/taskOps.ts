// Operations on agent task records (P1): Delete removes the record (and its
// transcript/review history) only — files the task changed stay, source
// inputs and manuscripts are never touched. A running task is coordinated
// through the plan's decision (stop first, or cancel). Tasks are not
// duplicated: a historical task is never replayed as a new success; start a
// new task to reuse its instructions.
import type { OpTarget } from "../../bindings/OpTarget";
import type { TaskSummary } from "../../bindings/TaskSummary";
import { runUserOperation } from "../operations/client";
import { writeText } from "../operations/clipboard";
import { targetKey } from "../operations/model";
import type { SurfaceActions } from "../operations/surfaces";

export function taskActions(selected: TaskSummary[]): { actions: SurfaceActions } {
  const has = selected.length > 0;
  const none = "Select a task first";
  const targets: OpTarget[] = selected.map((t) => ({ kind: "agentTask", task: t.id }));
  const labels = new Map(selected.map((t) => [targetKey({ kind: "agentTask", task: t.id }), t.title]));
  const plural = (one: string, many: string) => (selected.length === 1 ? one : many);
  return {
    actions: {
      copy: {
        enabled: has,
        reason: none,
        description: "Copies the task title as text",
        run: async () => {
          await writeText(selected.map((t) => t.title).join("\n"));
        },
      },
      duplicate: {
        enabled: false,
        reason: "A task is not duplicated; start a new task to reuse its instructions",
        run: () => {},
      },
      delete: {
        enabled: has,
        reason: none,
        label: plural("Delete Task Record", "Delete Task Records"),
        description: "The record only; files it changed stay",
        run: async () => {
          await runUserOperation({ kind: "delete", permanent: false }, targets, { labels });
        },
      },
      deletePermanently: {
        enabled: has,
        reason: none,
        description: "The record cannot be restored; files stay",
        run: async () => {
          await runUserOperation({ kind: "delete", permanent: true }, targets, { labels });
        },
      },
    },
  };
}
