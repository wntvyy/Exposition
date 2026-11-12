import { describe, expect, it } from "vitest";

import type { OutputEntry } from "../../bindings/OutputEntry";
import type { RunSummary } from "../../bindings/RunSummary";
import type { SessionInfo } from "../../bindings/SessionInfo";
import {
  EMPTY_RUN_OUTPUT,
  MAX_ENTRIES,
  appendEntries,
  applyEvent,
  attachmentKeys,
  defaultInsertOffset,
  documentFormat,
  runPill,
  sessionPill,
  type ResearchModel,
} from "./model";

const stream = (seq: number, text: string): OutputEntry => ({ seq, item: { kind: "stream", name: "stdout", text } });

const run = (patch: Partial<RunSummary> = {}): RunSummary => ({
  id: "run-1",
  sessionId: "session-1",
  owner: { kind: "user" },
  status: { kind: "running" },
  queued: false,
  origin: { kind: "input" },
  codePreview: "print(1)",
  executionCount: null,
  startedAtMs: 1,
  finishedAtMs: null,
  outputs: [],
  error: null,
  ...patch,
});

const session = (state: SessionInfo["state"]): SessionInfo => ({
  id: "session-1",
  owner: { kind: "user" },
  environmentId: "python:x",
  language: "python",
  displayName: "Python 3.14.5",
  state,
  kernel: null,
  kernelPid: null,
  helperPid: null,
  helperVersion: "0.1.0",
  transportEncryption: "curve",
  interruptMode: "event",
  activeRun: null,
  queuedRuns: [],
  startedAtMs: 1,
  generation: 1,
});

describe("research model", () => {
  it("appends output in order, ignores duplicates, and honors clear_output", () => {
    let out = appendEntries(EMPTY_RUN_OUTPUT, [stream(1, "a"), stream(2, "b")]);
    out = appendEntries(out, [stream(2, "b-again")]);
    expect(out.entries.map((e) => e.seq)).toEqual([1, 2]);
    out = appendEntries(out, [
      { seq: 3, item: { kind: "notice", message: "Interrupt requested." } },
      { seq: 4, item: { kind: "clearOutput", wait: false } },
      stream(5, "c"),
    ]);
    expect(out.entries.map((e) => e.seq)).toEqual([3, 5]);
  });

  it("bounds retained output", () => {
    const many = Array.from({ length: MAX_ENTRIES + 10 }, (_, i) => stream(i + 1, "x"));
    const out = appendEntries(EMPTY_RUN_OUTPUT, many);
    expect(out.entries).toHaveLength(MAX_ENTRIES);
    expect(out.truncated).toBe(true);
    expect(out.entries[0]?.seq).toBe(11);
  });

  it("reduces session and run events", () => {
    let model: ResearchModel = { sessions: [], runs: [], outputs: {} };
    model = applyEvent(model, { kind: "sessionChanged", session: session({ kind: "idle" }) });
    model = applyEvent(model, { kind: "runQueued", run: run({ queued: true }) });
    model = applyEvent(model, { kind: "runStarted", runId: "run-1", sessionId: "session-1", executionCount: 3 });
    model = applyEvent(model, { kind: "output", runId: "run-1", entry: stream(1, "hi\n") });
    expect(model.runs[0]?.queued).toBe(false);
    expect(model.runs[0]?.executionCount).toBe(3);
    expect(model.outputs["run-1"]?.entries).toHaveLength(1);
    model = applyEvent(model, { kind: "runFinished", run: run({ status: { kind: "interrupted" }, finishedAtMs: 9 }) });
    expect(runPill(model.runs[0]!)).toEqual({ status: "canceled", label: "Interrupted" });
    model = applyEvent(model, { kind: "sessionRemoved", sessionId: "session-1" });
    expect(model.sessions).toHaveLength(0);
  });

  it("uses one status vocabulary", () => {
    expect(sessionPill(session({ kind: "crashed", reason: "x" })).status).toBe("failed");
    expect(sessionPill(session({ kind: "busy" })).status).toBe("running");
    expect(runPill(run({ queued: true })).label).toBe("Queued");
    expect(runPill(run({ status: { kind: "succeeded" } })).label).toBe("Executed");
  });

  it("finds formats, insert offsets, and attachment keys", () => {
    expect(documentFormat("paper/main.typ")).toBe("typst");
    expect(documentFormat("a.TEX")).toBe("latex");
    expect(documentFormat("notes.md")).toBe("markdown");
    expect(documentFormat("x.py")).toBeNull();
    const tex = "\\begin{document}\nHi\n\\end{document}\n";
    expect(defaultInsertOffset(tex, "latex")).toBe(tex.indexOf("\\end{document}"));
    expect(defaultInsertOffset("abc", "typst")).toBe(3);
    const text = "x\n// exposition:begin fig-1\n...\n// exposition:end fig-1\n  // exposition:begin tab-2\r\n";
    expect(attachmentKeys(text, "typst")).toEqual(["fig-1", "tab-2"]);
    expect(attachmentKeys("<!-- exposition:begin fig-9 -->\n", "markdown")).toEqual(["fig-9"]);
  });
});
