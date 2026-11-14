// Run records vs outputs (P1): deleting a record never takes its outputs;
// "record and outputs" is both targets in ONE request (one decision).
import { describe, expect, it, vi } from "vitest";

const client = vi.hoisted(() => ({ runUserOperation: vi.fn(async () => null) }));
vi.mock("../operations/client", () => client);
vi.mock("../../app/editors", () => ({ editors: { all: () => [], close: vi.fn() } }));

import type { WorkflowRunSummary } from "./contract";
import { runActions } from "./workflowOps";

const run = { id: "wrun-1", workflow: "wf-1", revision: 1, title: "Fit", state: "succeeded" } as unknown as WorkflowRunSummary;

describe("run actions", () => {
  it("Delete Run Record deletes the record only", async () => {
    await runActions([run]).actions.delete!.run();
    expect(client.runUserOperation).toHaveBeenLastCalledWith({ kind: "delete", permanent: false }, [{ kind: "workflowRun", run: "wrun-1" }], expect.anything());
  });

  it("Delete Run and Its Outputs sends both targets in one request; the permanent variant too", async () => {
    const { extras } = runActions([run]);
    await extras.find((e) => e.id === "deleteWithOutputs")!.run();
    expect(client.runUserOperation).toHaveBeenLastCalledWith(
      { kind: "delete", permanent: false },
      [{ kind: "workflowRun", run: "wrun-1" }, { kind: "runOutputs", run: "wrun-1" }],
      expect.objectContaining({ labels: expect.any(Map) }),
    );
    await extras.find((e) => e.id === "deleteWithOutputsPermanently")!.run();
    expect(client.runUserOperation).toHaveBeenLastCalledWith(
      { kind: "delete", permanent: true },
      [{ kind: "workflowRun", run: "wrun-1" }, { kind: "runOutputs", run: "wrun-1" }],
      expect.anything(),
    );
    const call = client.runUserOperation.mock.calls.at(-1) as unknown as [unknown, unknown, { presetChoices?: unknown }];
    expect(call[2].presetChoices).toBeUndefined();
  });

  it("runs are not copied or duplicated", () => {
    const { actions } = runActions([run]);
    expect(actions.copy?.enabled).toBe(false);
    expect(actions.duplicate).toBeUndefined();
  });
});
