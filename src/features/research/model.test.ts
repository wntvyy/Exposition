import { describe, expect, it } from "vitest";

import type { OutputEntry } from "../../bindings/OutputEntry";
import type { RunSummary } from "../../bindings/RunSummary";
import type { SessionInfo } from "../../bindings/SessionInfo";
import {
  EMPTY_RUN_OUTPUT,
  MAX_ENTRIES,
  MAX_RETAINED_CHARS,
  appendEntries,
  applyEvent,
  attachmentKeys,
  attachmentKind,
  attachmentsOfKind,
  defaultInsertOffset,
  documentFormat,
  mergeSnapshot,
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

  it("bounds retained text so floods cannot grow the page without limit", () => {
    const big = Array.from({ length: 200 }, (_, i) => stream(i + 1, "x".repeat(50_000)));
    const out = appendEntries(EMPTY_RUN_OUTPUT, big);
    const chars = out.entries.reduce((n, e) => n + (e.item.kind === "stream" ? e.item.text.length : 0), 0);
    expect(chars).toBeLessThanOrEqual(MAX_RETAINED_CHARS);
    expect(out.truncated).toBe(true);
    expect(out.entries.at(-1)?.seq).toBe(200);
    // A single entry larger than the limit is still kept (shown as a preview).
    const one = appendEntries(EMPTY_RUN_OUTPUT, [stream(1, "y".repeat(3_000_000))]);
    expect(one.entries).toHaveLength(1);
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
    expect(sessionPill(session({ kind: "detached", reason: "closed" })).label).toBe("Session detached");
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

  it("a snapshot never replaces state that events changed while it was fetched", () => {
    const crashed = { ...session({ kind: "crashed", reason: "The kernel process exited." }), id: "s1" };
    const detached = {
      ...session({ kind: "detached", reason: "Workspace trust was revoked; this session's kernel was stopped." }),
      id: "s1",
    };
    // The snapshot was read while the session was briefly crashed; the
    // detach event arrived before the snapshot did.
    const changedDuringFetch = new Set(["s1"]);
    const merged = mergeSnapshot([detached], [crashed], (s) => s.id, (id) => changedDuringFetch.has(id));
    expect(merged).toEqual([detached]);
    // Untouched items take the snapshot; items removed by an event stay removed.
    const other = { ...session({ kind: "idle" }), id: "s2" };
    const otherNow = { ...session({ kind: "busy" }), id: "s2" };
    expect(mergeSnapshot([], [crashed, other], (s) => s.id, (id) => id === "s1")).toEqual([other]);
    expect(mergeSnapshot([detached, other], [crashed, otherNow], (s) => s.id, (id) => id === "s1")).toEqual([
      detached,
      otherNow,
    ]);
    // An item an event added during the fetch is kept even if the snapshot predates it.
    expect(mergeSnapshot([detached], [], (s) => s.id, (id) => id === "s1")).toEqual([detached]);
  });

  it("offers only attachments of the artifact's kind for update (content decides, not the key)", () => {
    const typst = [
      "// exposition:begin fig-4f769b79",
      "#figure(",
      '  image("a.png", width: 80%),',
      "  caption: [Agent figure, see table(1)],",
      ") <fig-4f769b79>",
      "// exposition:end fig-4f769b79",
      "// exposition:begin tab-1",
      "#figure(",
      "  table(",
      "    columns: 1,",
      '    table.header("n"),',
      "  ),",
      ") <tab-1>",
      "// exposition:end tab-1",
      "// exposition:begin fig-results",
      '#figure(table(columns: 1, "1")) <fig-results>',
      "// exposition:end fig-results",
      "",
    ].join("\n");
    expect(attachmentKind(typst, "typst", "fig-4f769b79")).toBe("figure");
    expect(attachmentKind(typst, "typst", "tab-1")).toBe("table");
    expect(attachmentKind(typst, "typst", "fig-results")).toBe("table");
    expect(attachmentsOfKind(typst, "typst", "table")).toEqual(["tab-1", "fig-results"]);
    expect(attachmentsOfKind(typst, "typst", "figure")).toEqual(["fig-4f769b79"]);
    const latex = [
      "% exposition:begin tab-2",
      "\\begin{table}[htbp]",
      "  \\begin{tabular}{r}",
      "  \\end{tabular}",
      "\\end{table}",
      "% exposition:end tab-2",
      "% exposition:begin fig-3",
      "\\begin{figure}[htbp]",
      "  \\includegraphics{a.png}",
      "\\end{figure}",
      "% exposition:end fig-3",
    ].join("\r\n");
    expect(attachmentsOfKind(latex, "latex", "table")).toEqual(["tab-2"]);
    expect(attachmentsOfKind(latex, "latex", "figure")).toEqual(["fig-3"]);
    const markdown = [
      "<!-- exposition:begin fig-5 -->",
      "![A | caption](a.png)",
      "<!-- exposition:end fig-5 -->",
      "<!-- exposition:begin tab-6 -->",
      "| n |",
      "| ---: |",
      "| 1 |",
      "<!-- exposition:end tab-6 -->",
    ].join("\n");
    expect(attachmentsOfKind(markdown, "markdown", "figure")).toEqual(["fig-5"]);
    expect(attachmentsOfKind(markdown, "markdown", "table")).toEqual(["tab-6"]);
    // Unrecognized content falls back to the generated key prefix; otherwise it is not offered.
    const edited =
      "// exposition:begin tab-9\nsomething else\n// exposition:end tab-9\n// exposition:begin mine\n?\n// exposition:end mine\n";
    expect(attachmentsOfKind(edited, "typst", "table")).toEqual(["tab-9"]);
    expect(attachmentsOfKind(edited, "typst", "figure")).toEqual([]);
    expect(attachmentKind(edited, "typst", "mine")).toBeNull();
  });
});
