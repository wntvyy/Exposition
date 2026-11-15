// The run loop controls (Run / Stop / Rerun) are wired to the user's session:
// Stop cancels the code running now, Rerun repeats the last run's kind of
// input against the current source, and neither asks for an extra approval.
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { RunSummary } from "../../bindings/RunSummary";
import type { SessionInfo } from "../../bindings/SessionInfo";

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

const ipc = vi.hoisted(() => ({
  researchRunCancel: vi.fn(async () => undefined),
  researchExecute: vi.fn(),
  researchRun: vi.fn(),
  researchSessionStart: vi.fn(),
  researchSessionShutdown: vi.fn(),
  researchWorkspace: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
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
vi.mock("../../app/notifications", () => ({ notify: vi.fn() }));
vi.mock("../../app/registry", () => ({ registry: { runCommand: vi.fn() } }));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn(async () => true) }));
vi.mock("../editor", () => ({ activeHandle: () => null, handleForPath: () => null }));
vi.mock("../editor/documentClient", () => ({ openDocument: vi.fn(), fileKey: (p: string) => `file:${p}`, handleForPath: vi.fn(), saveDocument: vi.fn() }));

const { RunControls } = await import("./ResearchPanel");
const { useResearch } = await import("./store");

const session = (overrides: Partial<SessionInfo> = {}): SessionInfo => ({
  id: "session-1",
  owner: { kind: "user" },
  environmentId: "python:venv",
  language: "python",
  displayName: "Python 3.12.10 (CPython, project .venv)",
  state: { kind: "idle" },
  kernel: null,
  kernelPid: null,
  helperPid: null,
  helperVersion: null,
  transportEncryption: "none",
  interruptMode: "event",
  activeRun: null,
  queuedRuns: [],
  startedAtMs: 1,
  generation: 1,
  ...overrides,
});

const run = (id: string, overrides: Partial<RunSummary> = {}): RunSummary => ({
  id,
  sessionId: "session-1",
  owner: { kind: "user" },
  status: { kind: "succeeded" },
  queued: false,
  origin: { kind: "input" },
  codePreview: "print(1)",
  executionCount: 1,
  startedAtMs: 10,
  finishedAtMs: 11,
  outputs: [],
  error: null,
  ...overrides,
});

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockClear();
  ipc.researchExecute.mockImplementation(async () => run("run-new", { origin: { kind: "input" }, status: { kind: "running" } }));
  useResearch.setState({
    workspace: { root: "C:\\p", trusted: true },
    environments: { runtimes: [], scannedAtMs: 0 },
    selectedEnvironment: "python:venv",
    sessions: [session()],
    runs: [],
    code: "",
    starting: false,
  });
});
afterEach(() => cleanup());

describe("run controls", () => {
  it("uses the same labels and disables Stop and Rerun until they apply", () => {
    render(<RunControls onRun={vi.fn()} />);
    expect(screen.getByTestId("research-run").textContent).toBe("Run");
    expect(screen.getByTestId("research-stop").textContent).toBe("Stop");
    expect(screen.getByTestId("research-rerun").textContent).toBe("Rerun");
    expect(screen.getByTestId("research-stop").hasAttribute("disabled")).toBe(true);
    expect(screen.getByTestId("research-rerun").hasAttribute("disabled")).toBe(true);
    expect(screen.getByTestId("research-rerun").getAttribute("aria-label")).toBe("Rerun: nothing has run yet");
  });

  it("Stop cancels the code the session runs now", async () => {
    useResearch.setState({ sessions: [session({ state: { kind: "busy" }, activeRun: "run-7" })], runs: [run("run-7", { status: { kind: "running" } })] });
    render(<RunControls onRun={vi.fn()} />);
    fireEvent.click(screen.getByTestId("research-stop"));
    await waitFor(() => expect(ipc.researchRunCancel).toHaveBeenCalledWith("run-7"));
  });

  it("Rerun repeats the last file run against the current source", async () => {
    useResearch.setState({ runs: [run("run-1", { origin: { kind: "file", path: "analysis/loop.py", revision: 4 } })] });
    render(<RunControls onRun={vi.fn()} />);
    const rerun = screen.getByTestId("research-rerun");
    expect(rerun.getAttribute("aria-label")).toBe("Rerun analysis/loop.py");
    fireEvent.click(rerun);
    await waitFor(() =>
      expect(ipc.researchExecute).toHaveBeenCalledWith({ sessionId: "session-1", source: { kind: "file", path: "analysis/loop.py" } }),
    );
  });

  it("Rerun of an input runs the input as it is now, else the last input's code", async () => {
    useResearch.setState({ runs: [run("run-1")], code: "print(2)" });
    const { unmount } = render(<RunControls onRun={vi.fn()} />);
    fireEvent.click(screen.getByTestId("research-rerun"));
    await waitFor(() => expect(ipc.researchExecute).toHaveBeenCalledWith({ sessionId: "session-1", source: { kind: "input", code: "print(2)" } }));
    unmount();
    ipc.researchExecute.mockClear();
    ipc.researchRun.mockResolvedValue({ code: { text: "print(1)" } });
    useResearch.setState({ runs: [run("run-1")], code: "  " });
    render(<RunControls onRun={vi.fn()} />);
    fireEvent.click(screen.getByTestId("research-rerun"));
    await waitFor(() => expect(ipc.researchExecute).toHaveBeenCalledWith({ sessionId: "session-1", source: { kind: "input", code: "print(1)" } }));
  });
});
