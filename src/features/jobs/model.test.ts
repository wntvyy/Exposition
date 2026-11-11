import { describe, expect, it } from "vitest";

import type { JobSummary } from "../../bindings/JobSummary";
import { EMPTY_OUTPUT, appendOutput, formatDuration, jobDurationMs, jobStatus } from "./model";

function job(partial: Partial<JobSummary> & { result?: JobSummary["info"]["result"] }): JobSummary {
  return {
    info: {
      id: "job-1",
      label: "Build",
      program: "pdflatex",
      args: [],
      policy: { kind: "trustedNative" },
      startedAtMs: 1_000,
      result: partial.result ?? null,
    },
    kind: "batch",
    state: partial.state ?? "running",
    workingDirectory: "C:/w",
    refusal: partial.refusal ?? null,
    pid: 1,
  };
}

describe("job presentation", () => {
  it("maps states and outcomes to status pills", () => {
    expect(jobStatus(job({ state: "queued" })).key).toBe("queued");
    expect(jobStatus(job({ state: "running" })).key).toBe("running");
    expect(jobStatus(job({ state: "canceling" })).key).toBe("canceling");
    const finished = (outcome: NonNullable<JobSummary["info"]["result"]>["outcome"]) =>
      jobStatus(job({ state: "finished", result: { id: "job-1", outcome, durationMs: 5, outputTruncated: false } }));
    expect(finished({ kind: "exited", code: 0 }).key).toBe("succeeded");
    expect(finished({ kind: "exited", code: 2 }).label).toBe("Failed (exit 2)");
    expect(finished({ kind: "canceled" }).key).toBe("canceled");
    expect(finished({ kind: "timedOut" }).status).toBe("failed");
    expect(finished({ kind: "canceled" }).status).toBe("canceled");
    expect(jobStatus(job({ state: "queued" })).status).toBe("idle");
    expect(finished({ kind: "failedToStart", reason: "untrusted" }).detail).toBe("untrusted");
  });

  it("formats durations", () => {
    expect(formatDuration(null)).toBe("—");
    expect(formatDuration(250)).toBe("250 ms");
    expect(formatDuration(2_500)).toBe("2.5 s");
    expect(formatDuration(75_000)).toBe("1m 15s");
    expect(jobDurationMs(job({}), 4_000)).toBe(3_000);
  });

  it("bounds retained output and ignores duplicates", () => {
    const chunk = (seq: number, text: string) => ({ seq, stream: "stdout" as const, text });
    let buffer = appendOutput(EMPTY_OUTPUT, [chunk(1, "aaaa"), chunk(2, "bbbb")], { truncated: false, complete: false }, 6);
    expect(buffer.chunks.map((c) => c.seq)).toEqual([2]);
    expect(buffer.truncated).toBe(true);
    buffer = appendOutput(buffer, [chunk(2, "bbbb"), chunk(3, "cc")], { truncated: false, complete: true }, 6);
    expect(buffer.chunks.map((c) => c.seq)).toEqual([2, 3]);
    expect(buffer.lastSeq).toBe(3);
    expect(buffer.complete).toBe(true);
  });
});
