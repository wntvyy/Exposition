// "Run again" on a finished run: plan the same workflow revision again; when
// the new plan's approval hash equals the plan this run was approved with
// (same inputs at the same hashes, steps, payloads, limits, execution and
// native consent) and nothing blocks it, the person's press starts it
// directly. Anything else opens the normal plan review. An interrupted run is
// never started again without a review (a step may have partly run).
import type { WorkflowRun, WorkflowRunPlan } from "./contract";

export type RunAgainDecision =
  | { kind: "start"; plan: WorkflowRunPlan }
  | { kind: "review"; reason: string };

export function decideRunAgain(finished: WorkflowRun, fresh: WorkflowRunPlan): RunAgainDecision {
  if (finished.state === "interrupted" || finished.state === "running") {
    return { kind: "review", reason: "This run did not finish; review the new plan before starting." };
  }
  if (fresh.blockers.length > 0) {
    return { kind: "review", reason: "The new plan cannot start as it is; review it." };
  }
  if (fresh.approvalHash !== finished.plan.approvalHash) {
    return {
      kind: "review",
      reason:
        "The plan differs from the one this run was approved with (inputs, steps, limits, or execution changed); review it before starting.",
    };
  }
  return { kind: "start", plan: fresh };
}
