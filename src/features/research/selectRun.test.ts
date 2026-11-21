// Selecting a stored run reads its whole transcript, so the newest output is
// what is shown (the view keeps the newest entries and marks older ones as
// dropped); it never stops at an earlier page and shows that as the end.
import { describe, expect, it, vi } from "vitest";

import type { OutputEntry } from "../../bindings/OutputEntry";

const TOTAL = 7_300;

const ipc = vi.hoisted(() => ({
  commands: {
    researchRunOutput: vi.fn(async (_run: string, after: number | null, limit: number) => {
      const first = (after ?? 0) + 1;
      const entries: OutputEntry[] = [];
      for (let seq = first; seq <= 7_300 && entries.length < limit; seq += 1) {
        entries.push({ seq, item: { kind: "stream", name: "stdout", text: `line ${seq}\n` } });
      }
      return { runId: "run-1", entries, firstRetainedSeq: 1, lastSeq: 7_300, truncated: false };
    }),
  },
}));
vi.mock("../../lib/ipc", () => ({ commands: ipc.commands, events: {}, errorMessage: (e: unknown) => String(e), isCommandError: () => false }));
vi.mock("./ipc", () => ({ setupCommands: {} }));
vi.mock("../../app/notifications", () => ({ notify: vi.fn() }));
vi.mock("../../app/registry", () => ({ registry: { runCommand: vi.fn() } }));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn(async () => true) }));
vi.mock("../editor", () => ({ activeHandle: () => null, handleForPath: () => null }));
vi.mock("../operations/client", () => ({ onProjectOp: vi.fn(() => () => {}) }));

const { selectRun, useResearch } = await import("./store");
const { MAX_ENTRIES } = await import("./model");

describe("selectRun", () => {
  it("reads every page of a long stored transcript and keeps the newest entries", async () => {
    await selectRun("run-1");
    const output = useResearch.getState().outputs["run-1"]!;
    expect(ipc.commands.researchRunOutput).toHaveBeenCalledTimes(Math.ceil((TOTAL + 1) / 500));
    expect(output.entries.at(-1)!.seq).toBe(TOTAL);
    expect(output.entries).toHaveLength(MAX_ENTRIES);
    expect(output.truncated).toBe(true);
    expect(output.loaded).toBe(true);
  });
});
