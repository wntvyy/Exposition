import { describe, expect, it } from "vitest";

import type { TranscriptItem } from "../../bindings/TranscriptItem";
import { turnSummary, withoutRepeatedFailure, type ToolCallItem } from "./turns";

const user: TranscriptItem = { kind: "userText", seq: 1, text: "Compare the export", attachments: [] } as TranscriptItem;
const notice = (message: string, level: "info" | "warning" | "error" = "error"): TranscriptItem => ({ kind: "notice", seq: 2, level, message });

describe("withoutRepeatedFailure", () => {
  it("drops a last error notice that repeats the failure message", () => {
    const items = [user, notice("prompt is too long")];
    expect(withoutRepeatedFailure(items, { kind: "failed", message: "prompt is too long" })).toEqual([user]);
  });

  it("keeps notices that differ, are not last, are not errors, or when the task did not fail", () => {
    expect(withoutRepeatedFailure([user, notice("other")], { kind: "failed", message: "prompt is too long" })).toHaveLength(2);
    expect(withoutRepeatedFailure([notice("prompt is too long"), user], { kind: "failed", message: "prompt is too long" })).toHaveLength(2);
    expect(withoutRepeatedFailure([user, notice("prompt is too long", "warning")], { kind: "failed", message: "prompt is too long" })).toHaveLength(2);
    expect(withoutRepeatedFailure([user, notice("prompt is too long")], { kind: "completed" })).toHaveLength(2);
  });
});

describe("turnSummary", () => {
  const call = (seq: number, name: string, status: ToolCallItem["status"]): ToolCallItem => ({
    kind: "toolCall",
    seq,
    toolUseId: `t${seq}`,
    name,
    input: {},
    status,
    result: null,
    isError: false,
    authorizedBy: null,
  });

  it("names only a call that is running as running: a queued call behind a waiting one is not (AG-8)", () => {
    // `run_command` waits for approval; `list_files` is queued behind it.
    const held = turnSummary([call(1, "run_command", "awaitingApproval"), call(2, "list_files", "pending")]);
    expect(held.waiting).toBe(1);
    expect(held.running).toBeNull();
    // A queued call alone (an earlier call's answer is awaited) is not running either.
    expect(turnSummary([call(1, "list_files", "pending")]).running).toBeNull();
    const working = turnSummary([call(1, "read_file", "succeeded"), call(2, "run_python", "running"), call(3, "list_files", "pending")]);
    expect(working.running).toBe("run_python");
  });
});
