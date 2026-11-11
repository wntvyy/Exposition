// Pure job presentation helpers (status pills, durations, bounded output).
import type { JobSummary } from "../../bindings/JobSummary";
import type { OutputChunk } from "../../bindings/OutputChunk";
import type { Status } from "../../ui/components";

export interface JobStatus {
  /** Stable machine value (exposed as `data-job-status` for tests and tooling). */
  key:
    | "queued"
    | "running"
    | "canceling"
    | "succeeded"
    | "failed"
    | "terminated"
    | "canceled"
    | "timedOut"
    | "failedToStart";
  /** Shared status vocabulary (UI standard `StatusPill`). */
  status: Status;
  label: string;
  /** Extra explanation (exit code, refusal reason). */
  detail?: string;
}

const UNAVAILABLE_REFUSALS = new Set(["programNotFound", "restrictedBackendUnavailable", "unsupportedBackend"]);

export function jobStatus(job: JobSummary): JobStatus {
  const result = job.info.result;
  if (!result) {
    if (job.state === "canceling") return { key: "canceling", status: "running", label: "Canceling…" };
    if (job.state === "running") return { key: "running", status: "running", label: "Running" };
    return { key: "queued", status: "idle", label: "Queued" };
  }
  const outcome = result.outcome;
  switch (outcome.kind) {
    case "exited":
      return outcome.code === 0
        ? { key: "succeeded", status: "succeeded", label: "Succeeded" }
        : { key: "failed", status: "failed", label: `Failed (exit ${outcome.code})` };
    case "terminated":
      return { key: "terminated", status: "failed", label: "Terminated", detail: outcome.description };
    case "canceled":
      return { key: "canceled", status: "canceled", label: "Canceled" };
    case "timedOut":
      return { key: "timedOut", status: "failed", label: "Timed out" };
    case "failedToStart": {
      const code = job.refusal?.code;
      const detail = job.refusal?.message ?? outcome.reason;
      if (code === "untrustedWorkspace") return { key: "failedToStart", status: "warning", label: "Not trusted", detail };
      if (code && UNAVAILABLE_REFUSALS.has(code)) {
        return { key: "failedToStart", status: "unavailable", label: "Unavailable", detail };
      }
      return { key: "failedToStart", status: "failed", label: "Not started", detail };
    }
  }
}

export function isActive(job: JobSummary): boolean {
  return job.info.result === null;
}

/** Wall-clock duration: final for finished jobs, live for running ones. */
export function jobDurationMs(job: JobSummary, now: number): number | null {
  if (job.info.result) return job.info.result.durationMs;
  if (job.info.startedAtMs !== null) return Math.max(0, now - job.info.startedAtMs);
  return null;
}

export function formatDuration(ms: number | null): string {
  if (ms === null) return "—";
  if (ms < 1000) return `${ms} ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)} s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.floor(seconds % 60);
  if (minutes < 60) return `${minutes}m ${rest.toString().padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h ${(minutes % 60).toString().padStart(2, "0")}m`;
}

/** Output retained by the UI for one job (newest chunks, bounded). */
export interface OutputBuffer {
  chunks: OutputChunk[];
  bytes: number;
  lastSeq: number | null;
  /** Older output was dropped (by the service log limit or the UI bound). */
  truncated: boolean;
  complete: boolean;
}

export const EMPTY_OUTPUT: OutputBuffer = { chunks: [], bytes: 0, lastSeq: null, truncated: false, complete: false };

/** UI-side bound per job, independent of the service's log limit. */
export const UI_OUTPUT_LIMIT = 256 * 1024;

export function appendOutput(
  buffer: OutputBuffer,
  chunks: OutputChunk[],
  flags: { truncated: boolean; complete: boolean },
  limit = UI_OUTPUT_LIMIT,
): OutputBuffer {
  const fresh = chunks.filter((c) => buffer.lastSeq === null || c.seq > buffer.lastSeq);
  let all = buffer.chunks.concat(fresh);
  let bytes = buffer.bytes + fresh.reduce((n, c) => n + c.text.length, 0);
  let truncated = buffer.truncated || flags.truncated;
  while (bytes > limit && all.length > 1) {
    const dropped = all[0]!;
    all = all.slice(1);
    bytes -= dropped.text.length;
    truncated = true;
  }
  const last = all.length ? all[all.length - 1]!.seq : buffer.lastSeq;
  return { chunks: all, bytes, lastSeq: last, truncated, complete: flags.complete };
}
