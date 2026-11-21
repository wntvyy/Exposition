// XR-A-03: research run records and their outputs are deleted recoverably
// from the run history, as workflow runs are: the row's menu (right click),
// the Delete key while a run row has focus, and the palette's Edit commands
// act on the run; the record and the outputs are separate targets.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { RunSummary } from "../../bindings/RunSummary";

vi.hoisted(() => {
  globalThis.matchMedia ??= ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof matchMedia;
});

const ops = vi.hoisted(() => ({ runUserOperation: vi.fn(async () => null), listeners: [] as ((r: unknown) => void)[] }));

vi.mock("../../lib/ipc", () => ({
  commands: { researchRuns: vi.fn(async () => ({ runs: [], next: null, hasMore: false, total: 0, unreadable: 0 })), researchRun: vi.fn(async () => null) },
  events: { research: vi.fn(async () => () => {}), researchResync: vi.fn(async () => () => {}), trustChanged: vi.fn(async () => () => {}) },
  errorMessage: (e: unknown) => String(e),
  isCommandError: () => false,
}));
vi.mock("./ipc", () => ({
  researchCommands: {},
  setupCommands: { selection: vi.fn(async () => null), select: vi.fn(async () => undefined), status: vi.fn(async () => null) },
  setupEvents: { setup: vi.fn(async () => () => {}), resync: vi.fn(async () => () => {}) },
}));
vi.mock("../../lib/graphIpc", () => ({ graphCommands: {}, graphEvents: { graph: vi.fn(), graphResync: vi.fn() } }));
vi.mock("../workflows/ipc", () => ({ workflowCommands: { operations: vi.fn(async () => []) } }));
vi.mock("../../app/notifications", () => ({ notify: vi.fn(), dismiss: vi.fn() }));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn(async () => true) }));
vi.mock("../editor", () => ({ activeHandle: () => null, handleForPath: () => null }));
vi.mock("../editor/documentClient", () => ({ openDocument: vi.fn(), fileKey: (p: string) => `file:${p}`, handleForPath: vi.fn(), saveDocument: vi.fn() }));
vi.mock("../operations/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../operations/client")>()),
  runUserOperation: ops.runUserOperation,
  onProjectOp: (listener: (r: unknown) => void) => {
    ops.listeners.push(listener);
    return () => {};
  },
}));

const { RunHistory } = await import("./ResearchPanel");
const { researchRunActions, researchRunLabel } = await import("./researchOps");
const { useResearch } = await import("./store");
const { registry } = await import("../../app/registry");
await import("../operations/register");

const run = (id: string, overrides: Partial<RunSummary> = {}): RunSummary => ({
  id,
  sessionId: "session-1",
  owner: { kind: "user" },
  status: { kind: "succeeded" },
  queued: false,
  origin: { kind: "input" },
  codePreview: "import numpy as np\nprint(np.mean([1, 2]))",
  executionCount: 1,
  startedAtMs: 1,
  finishedAtMs: 2,
  outputs: [],
  error: null,
  ...overrides,
});

const RECORD = { kind: "researchRun", run: "run-1" };
const OUTPUTS = { kind: "runOutputs", run: "run-1" };

describe("research run actions", () => {
  beforeEach(() => ops.runUserOperation.mockClear());

  it("delete the record only; outputs are a separate, explicit target", async () => {
    const { actions, extras } = researchRunActions([run("run-1")]);
    expect(actions.copy?.enabled).toBe(false);
    expect(actions.delete).toMatchObject({ enabled: true, label: "Delete Run Record", description: "The record only; outputs stay" });
    await actions.delete!.run();
    expect(ops.runUserOperation).toHaveBeenLastCalledWith({ kind: "delete", permanent: false }, [RECORD], expect.anything());
    const labels = (ops.runUserOperation.mock.calls.at(-1) as unknown as [unknown, unknown, { labels: Map<string, string> }])[2].labels;
    expect(labels.get("researchRun:run-1")).toBe("run “import numpy as np”");
    await extras.find((e) => e.id === "deleteWithOutputs")!.run();
    expect(ops.runUserOperation).toHaveBeenLastCalledWith({ kind: "delete", permanent: false }, [RECORD, OUTPUTS], expect.anything());
    await extras.find((e) => e.id === "deleteWithOutputsPermanently")!.run();
    expect(ops.runUserOperation).toHaveBeenLastCalledWith({ kind: "delete", permanent: true }, [RECORD, OUTPUTS], expect.anything());
    await actions.deletePermanently!.run();
    expect(ops.runUserOperation).toHaveBeenLastCalledWith({ kind: "delete", permanent: true }, [RECORD], expect.anything());
  });

  it("names a run by its code's first line, or its id", () => {
    expect(researchRunLabel({ id: "run-2", codePreview: "\n  x = 1\ny" })).toBe("run “x = 1”");
    expect(researchRunLabel({ id: "run-2", codePreview: "" })).toBe("run run-2");
    expect(researchRunLabel({ id: "run-2", codePreview: "a".repeat(60) })).toBe(`run “${"a".repeat(39)}…”`);
  });
});

describe("run history as an operation surface", () => {
  beforeEach(() => {
    ops.runUserOperation.mockClear();
    useResearch.setState({ runs: [run("run-1"), run("run-2", { codePreview: "print(2)" })], selectedRun: null });
  });
  afterEach(() => cleanup());

  it("offers Delete Run Record and the outputs variants in the row's context menu", async () => {
    render(<RunHistory />);
    const row = screen.getByTestId("research-history").querySelector('[data-op-item="run-1"]')!;
    expect(row).toBeTruthy();
    fireEvent.contextMenu(row);
    const menu = await screen.findByTestId("research-runs-context-menu");
    expect(within(menu).getByText("Delete Run Record")).toBeTruthy();
    expect(within(menu).getByText("The record only; outputs stay")).toBeTruthy();
    expect(within(menu).getByText("Delete Run and Its Outputs")).toBeTruthy();
    await act(async () => {
      fireEvent.click(within(menu).getByText("Delete Run Record"));
    });
    expect(ops.runUserOperation).toHaveBeenCalledWith({ kind: "delete", permanent: false }, [RECORD], expect.anything());
  });

  // Found by the p1-research-runs e2e: after a right click (no click event),
  // keyboard focus on another run did not select it, so the palette (focus
  // in its input) offered no Delete for it.
  it("selects a run focused from the keyboard after a right click elsewhere", () => {
    render(<RunHistory />);
    const history = screen.getByTestId("research-history");
    const first = history.querySelector<HTMLElement>('[data-op-item="run-1"]')!;
    const second = history.querySelector<HTMLElement>('[data-op-item="run-2"]')!;
    fireEvent.mouseDown(first);
    fireEvent.mouseUp(first);
    fireEvent.contextMenu(first);
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    act(() => second.focus());
    expect(second.getAttribute("data-op-selected")).toBe("true");
    expect(first.getAttribute("data-op-selected")).toBeNull();
  });

  it("deletes the focused run with the Delete key (workbench shortcut) and shows it on click", async () => {
    render(<RunHistory />);
    const row = screen.getByTestId("research-history").querySelector<HTMLElement>('[data-op-item="run-2"]')!;
    act(() => row.focus());
    await act(async () => {
      await registry.runCommand("operations.delete");
    });
    expect(ops.runUserOperation).toHaveBeenCalledWith(
      { kind: "delete", permanent: false },
      [{ kind: "researchRun", run: "run-2" }],
      expect.anything(),
    );
    fireEvent.click(row);
    expect(useResearch.getState().selectedRun).toBe("run-2");
  });
});

describe("run history longer than the rows shown", () => {
  const many = (n: number) => Array.from({ length: n }, (_, i) => run(`run-${String(n - i).padStart(4, "0")}`, { startedAtMs: n - i }));
  afterEach(() => {
    cleanup();
    useResearch.setState({ runs: [], runsNext: null, runsRemaining: 0, runsUnreadable: 0, runsLoadingMore: false, selectedRun: null });
  });

  it("shows nothing but the runs when all of them are listed", () => {
    useResearch.setState({ runs: many(3), runsNext: null, runsRemaining: 0 });
    render(<RunHistory />);
    expect(screen.queryByTestId("research-history-count")).toBeNull();
    expect(screen.queryByTestId("research-history-more")).toBeNull();
    expect(screen.queryByTestId("research-history-unreadable")).toBeNull();
  });

  it("says how many are listed of how many exist and shows more on request", async () => {
    const page = { runs: many(1_000).slice(500), next: null, hasMore: false, total: 1_000, unreadable: 2 };
    const { commands } = await import("../../lib/ipc");
    (commands.researchRuns as ReturnType<typeof vi.fn>).mockResolvedValueOnce(page);
    useResearch.setState({ runs: many(1_000).slice(0, 500), runsNext: "next", runsRemaining: 500, selectedRun: "run-0990" });
    render(<RunHistory />);
    const rows = () => screen.getByTestId("research-history").querySelectorAll("[data-op-item]").length;
    expect(rows()).toBe(200);
    expect(screen.getByTestId("research-history-count").textContent).toBe("200 of 1,000");
    const more = () => screen.getByTestId("research-history-more");
    expect(within(more()).getByText("Show more runs")).toBeTruthy();
    expect(within(more()).getByText("800 not shown")).toBeTruthy();
    // 400 rows are loaded already: no request.
    await act(async () => {
      fireEvent.click(more());
    });
    expect(rows()).toBe(400);
    expect(commands.researchRuns).not.toHaveBeenCalledWith({ after: "next" });
    // 600 rows need the next page.
    await act(async () => {
      fireEvent.click(more());
    });
    expect(commands.researchRuns).toHaveBeenCalledWith({ after: "next" });
    expect(rows()).toBe(600);
    expect(screen.getByTestId("research-history-count").textContent).toBe("600 of 1,000");
    expect(useResearch.getState().selectedRun).toBe("run-0990");
    for (let i = 0; i < 2; i += 1) {
      await act(async () => {
        fireEvent.click(more());
      });
    }
    expect(rows()).toBe(1_000);
    expect(screen.queryByTestId("research-history-count")).toBeNull();
    expect(screen.queryByTestId("research-history-more")).toBeNull();
    expect(within(screen.getByTestId("research-history-unreadable")).getByText("2 run records cannot be read")).toBeTruthy();
    // The last row is a run like the first: selecting it shows it.
    const last = screen.getByTestId("research-history").querySelector('[data-op-item="run-0001"]')!;
    expect(within(last as HTMLElement).getByText(/import numpy/)).toBeTruthy();
  });
});
