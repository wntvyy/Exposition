// A build that could not run (missing or wrong compiler) is actionable: the
// build status, the Problems panel, and its guidance say why, with a way to
// the tool locations in Settings (R-m23 F7).
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { BuildEvent } from "../../bindings/BuildEvent";
import type { BuildStatus as Status } from "../../bindings/BuildStatus";
import type { Diagnostic } from "../../bindings/Diagnostic";

const workspaceHandlers: ((event: { kind: string }) => void)[] = [];

vi.mock("../../lib/ipc", () => ({
  commands: { documentList: vi.fn(async () => []), documentState: vi.fn() },
  events: {
    typesettingBuild: vi.fn(async () => () => {}),
    typesettingStatus: vi.fn(async () => () => {}),
    workspace: vi.fn(async (handler: (event: { kind: string }) => void) => {
      workspaceHandlers.push(handler);
      return () => {};
    }),
  },
}));

const { registry } = await import("../../app/registry");
const { ProblemsPanel } = await import("./ProblemsPanel");
const { BuildStatus } = await import("./BuildStatus");
const store = await import("./store");

const GUIDANCE = "Install a TeX distribution (TeX Live or MiKTeX) or set the pdflatex location in Settings.";

function event(status: Status, diagnostics: Diagnostic[] = []): BuildEvent {
  return {
    result: {
      id: "tex-1",
      target: { format: "latex", engine: "pdfLatex", entry: "main.tex" },
      snapshot: "",
      sources: [],
      configuration: null,
      status,
      diagnostics,
      pages: [],
      mapping: "none",
      exports: [],
      durationMs: 0,
    },
    timings: { snapshotMs: 0, compileMs: 0, mapMs: 0, totalMs: 0 },
    lastGood: null,
    refusal: null,
  };
}

afterEach(() => {
  cleanup();
  store.useTypesetting.setState({ entries: {}, reveal: null });
});

describe("builds that could not run", () => {
  it("summarize a missing compiler as unavailable with its guidance", () => {
    const missing = event({ kind: "toolUnavailable", tool: "pdflatex", guidance: GUIDANCE });
    expect(store.summarize({ entry: "main.tex", activity: "idle", latest: missing })).toBe("unavailable");
    expect(store.statusProblem(missing)).toEqual({
      message: "pdflatex is not available, so main.tex cannot be built.",
      hints: [GUIDANCE],
      toolSettings: true,
    });
    expect(store.statusProblem(event({ kind: "failed" }))).toBeNull();
    const untrusted = {
      ...event({ kind: "error", message: "not trusted" }),
      refusal: { code: "untrustedWorkspace", message: "not trusted", workspaceRoot: "C:/p" },
    } as BuildEvent;
    expect(store.statusProblem(untrusted)).toBeNull();
  });

  it("list a missing compiler in Problems with the guidance and a way to Settings", async () => {
    const run = vi.spyOn(registry, "runCommand").mockResolvedValue();
    store.recordBuild(event({ kind: "toolUnavailable", tool: "pdflatex", guidance: GUIDANCE }));
    render(<ProblemsPanel />);
    const problem = screen.getByTestId("problem");
    expect(problem.textContent).toContain("pdflatex is not available");
    expect(screen.getByText(GUIDANCE)).toBeTruthy();
    fireEvent.click(screen.getByTestId("problem-tool-settings"));
    await waitFor(() => expect(run).toHaveBeenCalledWith("settings.execution"));
    render(<BuildStatus />);
    const status = screen.getByTestId("build-status");
    expect(status.dataset.status).toBe("unavailable");
    expect(status.textContent).toContain("pdflatex not available");
  });

  it("offer Settings for a compiler path that is not a TeX engine", () => {
    store.recordBuild(
      event({ kind: "failed" }, [
        {
          severity: "error",
          message: "lualatex (C:/x/lualatex.exe) did not run as a TeX engine: it exited with code 1 without writing a TeX log.",
          location: null,
          hints: ["Check the lualatex location in Settings → Tool locations, or clear it to use the TeX installation found on this computer."],
        },
      ]),
    );
    render(<ProblemsPanel />);
    expect(screen.getByTestId("problem").textContent).toContain("did not run as a TeX engine");
    expect(screen.getByTestId("problem-tool-settings")).toBeTruthy();
    render(<BuildStatus />);
    expect(screen.getByTestId("build-status").textContent).toContain("1 error");
  });
});

// XR-A-01: a build state belongs to the folder it was built in; after Close
// Folder or opening another folder the status bar and Problems show nothing of it.
describe("build state across a project switch", () => {
  const untrusted = {
    ...event({ kind: "error", message: "not trusted" }),
    refusal: { code: "untrustedWorkspace", message: "not trusted", workspaceRoot: "C:/old" },
  } as BuildEvent;

  it.each(["closed", "opened"])("is cleared when the workspace is %s", async (kind) => {
    store.installTypesettingEvents();
    await waitFor(() => expect(workspaceHandlers.length).toBeGreaterThan(0));
    store.recordBuild(untrusted);
    store.recordBuild(event({ kind: "failed" }, [{ severity: "error", message: "boom", location: null, hints: [] }]));
    render(<BuildStatus />);
    expect(screen.getByTestId("build-status").textContent).toContain("main.tex");
    for (const handler of workspaceHandlers) handler({ kind });
    await waitFor(() => expect(screen.queryByTestId("build-status")).toBeNull());
    cleanup();
    render(<ProblemsPanel />);
    expect(screen.queryByTestId("problem")).toBeNull();
    expect(store.useTypesetting.getState().entries).toEqual({});
  });

  it("keeps the state on other workspace events", async () => {
    store.installTypesettingEvents();
    await waitFor(() => expect(workspaceHandlers.length).toBeGreaterThan(0));
    store.recordBuild(untrusted);
    for (const handler of workspaceHandlers) handler({ kind: "filesChanged" });
    expect(Object.keys(store.useTypesetting.getState().entries)).toEqual(["main.tex"]);
  });
});
