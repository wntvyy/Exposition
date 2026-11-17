// Research execution labels (stabilization ST, lane st/state). Each test
// failed before its fix:
// - C4 / XR-C-02: restricted research names Python, R, and Julia workflow
//   batch steps (interactive sessions stay trusted native);
// - XR-K-03: the Restricted research row follows the configured images
//   without reopening Research, and Refresh environments re-reads it.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { OperationContract } from "../../bindings/OperationContract";

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
  researchEnvironments: vi.fn(async () => ({ runtimes: [], scannedAtMs: 1 })),
  researchWorkspace: vi.fn(),
}));
const operations = vi.hoisted(() => vi.fn());

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
vi.mock("../workflows/ipc", () => ({ workflowCommands: { operations } }));
vi.mock("../settings/store", async () => {
  const { create } = await import("zustand");
  return { useSettings: create(() => ({ execution: null as unknown })) };
});
vi.mock("../../app/notifications", () => ({ notify: vi.fn() }));
vi.mock("../../app/registry", () => ({ registry: { runCommand: vi.fn() } }));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn(async () => true) }));
vi.mock("../editor", () => ({ activeHandle: () => null, handleForPath: () => null }));
vi.mock("../editor/documentClient", () => ({ openDocument: vi.fn(), fileKey: (p: string) => `file:${p}`, handleForPath: vi.fn(), saveDocument: vi.fn() }));

const { ExecutionLabel, ExecutionModes } = await import("./ExecutionLabel");
const { SessionBar } = await import("./ResearchPanel");
const { useResearch } = await import("./store");
const { useSettings } = await import("../settings/store");

const NO_IMAGE = "Restricted execution is unavailable: no container image is configured. Set execution.docker.image to an image present locally (`docker image ls`). It never falls back to native execution.";
const NO_R_IMAGE = "Restricted execution is unavailable: no R image is configured: choose a local image that provides Rscript as the R image. Exposition never pulls images. It never falls back to native execution.";

const batch = (id: string, available: boolean, reason = NO_IMAGE): OperationContract =>
  ({ id, runner: { id, title: id, containment: "restrictedContainer", allows: [], available, availability: available ? "Available" : reason } }) as unknown as OperationContract;

const catalog = (python: boolean, r: boolean, julia: boolean) => [
  batch("research.pythonBatch", python),
  batch("research.rBatch", r, NO_R_IMAGE),
  batch("research.juliaBatch", julia, NO_IMAGE.replace("container image", "Julia image")),
];

/** Execution settings as the settings store holds them (only the images matter here). */
const executionWith = (docker: object) => ({ settings: { docker }, warning: null }) as never;

const restrictedRow = () => screen.getByTestId("research-execution-modes").querySelector('[data-mode="restricted"]') as HTMLElement;

beforeEach(() => {
  vi.clearAllMocks();
  operations.mockResolvedValue(catalog(false, false, false));
  useResearch.setState({ workspace: { root: "C:\\p", trusted: true }, environments: { runtimes: [], scannedAtMs: 0 } as never, sessions: [], runs: [] });
});
afterEach(() => cleanup());

describe("C4: restricted research names every restricted batch language", () => {
  it("says restricted execution is for workflow batch steps in Python, R, and Julia; interactive sessions stay trusted native", async () => {
    render(<ExecutionLabel />);
    const note = screen.getByTestId("research-execution-mode").getAttribute("aria-label")!;
    expect(note).toContain("Trusted native");
    expect(note).toContain("workflow batch steps in Python, R, and Julia");
    expect(note).not.toMatch(/Python batch steps/);
    render(<ExecutionModes />);
    const row = restrictedRow();
    expect(row.textContent).toContain("Workflow batch steps in Python, R, and Julia run in a container");
    await waitFor(() => expect(within(row).getByTestId("research-restricted-r")).toBeTruthy());
    expect(within(row).getByTestId("research-restricted-r").textContent).toContain("No R image is configured");
    expect(within(row).getByTestId("research-restricted-julia").textContent).toContain("No Julia image is configured");
  });
});

describe("XR-K-03: the Restricted research row follows the configured images", () => {
  it("shows a language available once its image is configured, without reopening Research", async () => {
    render(<ExecutionModes />);
    await waitFor(() => expect(restrictedRow().textContent).toMatch(/no container image is configured/i));
    expect(restrictedRow().getAttribute("data-available")).toBe("false");

    // Settings › Execution: the Python image is set.
    operations.mockResolvedValue(catalog(true, false, false));
    act(() => useSettings.setState({ execution: executionWith({ image: "python:3.13-slim" }) }));
    await waitFor(() => expect(restrictedRow().getAttribute("data-available")).toBe("true"));
    expect(restrictedRow().textContent).not.toMatch(/no container image is configured/i);
    expect(within(restrictedRow()).getByTestId("research-restricted-python").getAttribute("data-available")).toBe("true");
    expect(within(restrictedRow()).getByTestId("research-restricted-r").getAttribute("data-available")).toBe("false");

    // The R image too.
    operations.mockResolvedValue(catalog(true, true, false));
    act(() => useSettings.setState({ execution: executionWith({ image: "python:3.13-slim", rImage: "r-base:4.6.2" }) }));
    await waitFor(() => expect(within(restrictedRow()).getByTestId("research-restricted-r").getAttribute("data-available")).toBe("true"));
  });

  it("re-reads restricted availability when Refresh environments is pressed", async () => {
    render(
      <>
        <SessionBar status={false} />
        <ExecutionModes />
      </>,
    );
    await waitFor(() => expect(restrictedRow().textContent).toMatch(/no container image is configured/i));
    expect(restrictedRow().getAttribute("data-available")).toBe("false");
    // Docker or the image changed outside Exposition (no setting changed here).
    operations.mockResolvedValue(catalog(true, false, false));
    fireEvent.click(screen.getByRole("button", { name: "Refresh environments" }));
    await waitFor(() => expect(ipc.researchEnvironments).toHaveBeenCalledWith(true));
    await waitFor(() => expect(restrictedRow().getAttribute("data-available")).toBe("true"));
  });
});
