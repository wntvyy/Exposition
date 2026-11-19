import { describe, expect, it } from "vitest";

import type { TranscriptItem } from "../../bindings/TranscriptItem";
import { withoutRepeatedFailure } from "./turns";

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
