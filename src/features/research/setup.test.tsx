// Guided environment setup in the Research UI: the concrete next step for an
// unavailable environment, the plan shown before anything runs (exact
// packages, folder, network, privileges), validation errors from Rust,
// progress with Cancel, an honest outcome, official download pages with
// automatic re-detection, R/Julia commands to copy, and the remembered
// environment. IPC is mocked at the feature boundary.
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EnvironmentReport } from "../../bindings/EnvironmentReport";
import type { RuntimeEnvironment } from "../../bindings/RuntimeEnvironment";
import type { SetupEvent } from "../../bindings/SetupEvent";
import type { SetupPlan } from "../../bindings/SetupPlan";
import type { SetupStatus } from "../../bindings/SetupStatus";

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
  researchEnvironments: vi.fn(),
  researchWorkspace: vi.fn(),
  researchSessions: vi.fn(async () => []),
  researchRuns: vi.fn(async () => []),
  researchRun: vi.fn(),
  researchRunCancel: vi.fn(),
  researchExecute: vi.fn(),
  appOpenExternal: vi.fn(async () => undefined),
}));
const setupIpc = vi.hoisted(() => ({
  plan: vi.fn(),
  start: vi.fn(),
  cancel: vi.fn(async () => true),
  status: vi.fn(async () => null),
  selection: vi.fn(async () => null as string | null),
  select: vi.fn(async () => undefined),
}));
const setupHandlers = vi.hoisted(() => [] as ((e: SetupEvent) => void)[]);
const notify = vi.hoisted(() => vi.fn());

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { research: vi.fn(async () => () => {}), researchResync: vi.fn(async () => () => {}), trustChanged: vi.fn(async () => () => {}) },
  errorMessage: (e: unknown) => (e && typeof e === "object" && "message" in e ? String((e as { message: string }).message) : String(e)),
  isCommandError: (e: unknown) => Boolean(e && typeof e === "object" && "code" in e),
}));
vi.mock("./ipc", () => ({
  researchCommands: {},
  setupCommands: setupIpc,
  setupEvents: {
    setup: vi.fn(async (handler: (e: SetupEvent) => void) => {
      setupHandlers.push(handler);
      return () => {};
    }),
    resync: vi.fn(async () => () => {}),
  },
}));
vi.mock("../../app/notifications", () => ({ notify }));
vi.mock("../../app/registry", () => ({ registry: { runCommand: vi.fn() } }));
vi.mock("../jobs/trust", () => ({ requestWorkspaceTrust: vi.fn(async () => true) }));
vi.mock("../editor", () => ({ activeHandle: () => null, handleForPath: () => null }));

const { EnvironmentHelp, SetupOutcome, SetupProgress } = await import("./EnvironmentSetup");
const { useResearch } = await import("./store");
const { applySetupEvent, useSetup } = await import("./setup");

const ROOT = "C:\\Users\\r\\thesis";
const unavailable = (reason: string) => ({ kind: "unavailable" as const, reason, guidance: "see the action" });

const env = (id: string, overrides: Partial<RuntimeEnvironment> = {}): RuntimeEnvironment => ({
  id,
  language: "python",
  displayName: `Python ${id}`,
  executable: `C:\\Python\\${id}\\python.exe`,
  version: "3.12.10",
  kernel: null,
  hostInterpreter: null,
  packages: [],
  kernelEnvironment: {},
  availability: { kind: "available" },
  ...overrides,
});

const system312 = env("python:312", {
  displayName: "Python 3.12.10 (CPython)",
  availability: unavailable("Python 3.12.10 at C:\\Python312\\python.exe lacks ipykernel and jupyter_client."),
  setup: { downloadUrl: null, action: "createEnvironment", commands: [], commandsFor: null, userWide: false },
});

const report = (...runtimes: RuntimeEnvironment[]): EnvironmentReport => ({ runtimes, scannedAtMs: 1 });

const plan = (overrides: Partial<SetupPlan> = {}): SetupPlan => ({
  request: { kind: "createEnvironment", baseEnvironment: "python:312", packages: [] },
  root: ROOT,
  target: `${ROOT}\\.venv`,
  targetName: ".venv",
  createsEnvironment: true,
  removesUnfinished: false,
  baseInterpreter: "C:\\Python312\\python.exe",
  baseDisplayName: "Python 3.12.10 (CPython)",
  interpreter: `${ROOT}\\.venv\\Scripts\\python.exe`,
  packages: ["ipykernel", "jupyter_client"],
  indexUrl: "https://pypi.org/simple",
  network: true,
  steps: [
    { label: "Create the environment in .venv", program: "C:\\Python312\\python.exe", args: ["-I", "-m", "venv", `${ROOT}\\.venv`], network: false, timeoutMs: 300000 },
    {
      label: "Install packages from PyPI",
      program: `${ROOT}\\.venv\\Scripts\\python.exe`,
      args: ["-I", "-m", "pip", "--isolated", "install", "--only-binary=:all:", "--index-url", "https://pypi.org/simple", "ipykernel", "jupyter_client"],
      network: true,
      timeoutMs: 1800000,
    },
  ],
  summary:
    "Creates a Python environment in .venv from Python 3.12.10 (CPython), then installs ipykernel and jupyter_client from PyPI into it. Runs on this computer with your account's privileges; if setup fails or is canceled, the new environment is removed.",
  planHash: "f".repeat(64),
  ...overrides,
});

const status = (overrides: Partial<SetupStatus> = {}): SetupStatus => ({
  id: "setup-1",
  owner: { kind: "user" },
  plan: plan(),
  state: "running",
  phase: "step",
  step: 1,
  message: null,
  removed: false,
  environmentId: null,
  ready: false,
  jobs: ["job-1", "job-2"],
  log: ["Collecting ipykernel", "  Downloading ipykernel-6.29.5-py3-none-any.whl"],
  startedAtMs: 1,
  finishedAtMs: null,
  ...overrides,
});

beforeEach(() => {
  useResearch.setState({
    workspace: { root: ROOT, trusted: true },
    environments: report(system312),
    environmentsLoading: false,
    environmentsError: null,
    selectedEnvironment: system312.id,
    sessions: [],
    runs: [],
  });
  useSetup.setState({ status: null, log: [], dismissed: [] });
  for (const fn of [...Object.values(ipc), ...Object.values(setupIpc), notify]) fn.mockClear();
  setupIpc.plan.mockReset();
  setupIpc.start.mockReset();
});

afterEach(() => cleanup());

describe("environment setup", () => {
  it("shows the plan before anything runs and starts it only on Set up", async () => {
    setupIpc.plan.mockImplementation(async (request: { packages: string[] }) =>
      plan({ packages: ["ipykernel", "jupyter_client", ...request.packages] }),
    );
    setupIpc.start.mockResolvedValue(status());
    render(<EnvironmentHelp env={system312} />);
    expect(screen.getByTestId("research-unavailable").getAttribute("data-setup")).toBe("createEnvironment");
    expect(screen.getByText(/creates a Python environment in .venv inside the project/)).toBeTruthy();
    expect(setupIpc.plan).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("setup-open"));
    await waitFor(() =>
      expect(setupIpc.plan).toHaveBeenCalledWith({ kind: "createEnvironment", baseEnvironment: "python:312", packages: [] }),
    );
    await screen.findByTestId("setup-summary");
    expect(screen.getByTestId("setup-folder").textContent).toContain(".venv");
    expect(screen.getByTestId("setup-folder").textContent).toContain("new, inside the project");
    expect(screen.getByTestId("setup-packages").textContent).toBe("ipykernel jupyter_client");
    expect(screen.getByText(/binary packages only/)).toBeTruthy();
    expect(screen.getByText(/On this computer with your account/)).toBeTruthy();
    expect(screen.getByText(/The new .venv is removed/)).toBeTruthy();
    // The exact commands are one disclosure away.
    expect(screen.getByRole("button", { name: /Commands/ })).toBeTruthy();

    fireEvent.change(screen.getByLabelText("Additional packages"), { target: { value: "numpy pandas>=2.2" } });
    await waitFor(
      () =>
        expect(setupIpc.plan).toHaveBeenLastCalledWith({
          kind: "createEnvironment",
          baseEnvironment: "python:312",
          packages: ["numpy", "pandas>=2.2"],
        }),
      { timeout: 2000 },
    );
    await waitFor(() => expect(screen.getByTestId("setup-packages").textContent).toBe("ipykernel jupyter_client numpy pandas>=2.2"));
    expect(setupIpc.start).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("setup-start"));
    await waitFor(() => expect(setupIpc.start).toHaveBeenCalledTimes(1));
    expect(setupIpc.start.mock.calls[0]?.[0].packages).toEqual(["ipykernel", "jupyter_client", "numpy", "pandas>=2.2"]);
    await waitFor(() => expect(useSetup.getState().status?.id).toBe("setup-1"));
  });

  it("shows Rust's refusal of a package on the field and keeps Set up disabled", async () => {
    setupIpc.plan.mockImplementation(async (request: { packages: string[] }) => {
      if (request.packages.length > 0) {
        throw { code: "setup.invalidPackage", message: "“-r” is not accepted: options are not accepted." };
      }
      return plan();
    });
    render(<EnvironmentHelp env={system312} />);
    fireEvent.click(screen.getByTestId("setup-open"));
    await screen.findByTestId("setup-summary");
    fireEvent.change(screen.getByLabelText("Additional packages"), { target: { value: "-r requirements.txt" } });
    await screen.findByText(/options are not accepted/, undefined, { timeout: 2000 });
    expect(screen.getByTestId("setup-start").hasAttribute("disabled") || screen.getByTestId("setup-start").getAttribute("aria-disabled") === "true").toBe(true);
    expect(setupIpc.start).not.toHaveBeenCalled();
  });

  it("asks for a new review when the project changed after planning", async () => {
    setupIpc.plan.mockResolvedValue(plan());
    setupIpc.start.mockRejectedValue({ code: "setup.planStale", message: "The project or its environments changed since this setup was planned; review the new plan." });
    render(<EnvironmentHelp env={system312} />);
    fireEvent.click(screen.getByTestId("setup-open"));
    await screen.findByTestId("setup-summary");
    fireEvent.click(screen.getByTestId("setup-start"));
    const error = await screen.findByTestId("setup-start-error");
    expect(error.getAttribute("data-code")).toBe("setup.planStale");
    expect(error.textContent).toContain("Nothing was changed.");
    fireEvent.click(screen.getByRole("button", { name: "Review the new plan" }));
    await waitFor(() => expect(setupIpc.plan).toHaveBeenCalledTimes(2));
  });

  it("links a missing Python to its official page and checks again when the window gets focus", async () => {
    const none = env("python:none", {
      displayName: "Python",
      executable: null,
      availability: unavailable("No Python interpreter was found."),
      setup: { downloadUrl: "https://www.python.org/downloads/", action: null, commands: [], commandsFor: null, userWide: false },
    });
    ipc.researchEnvironments.mockResolvedValue(report(none));
    useResearch.setState({ environments: report(none), selectedEnvironment: none.id });
    render(<EnvironmentHelp env={none} />);
    expect(screen.queryByTestId("setup-open")).toBeNull();
    fireEvent.click(screen.getByTestId("setup-download"));
    await waitFor(() => expect(ipc.appOpenExternal).toHaveBeenCalledWith("https://www.python.org/downloads/"));
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });
    await waitFor(() => expect(ipc.researchEnvironments).toHaveBeenCalledWith(true));
  });

  it("gives R kernel commands to copy and says they are user-wide", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    const r = env("r:none", {
      language: "r",
      displayName: "R 4.6.2",
      executable: "C:\\Program Files\\R\\R-4.6.2\\bin\\Rscript.exe",
      availability: unavailable("R 4.6.2 is installed, but its Jupyter kernel (IRkernel) is not installed."),
      setup: { downloadUrl: null, action: null, commands: ["install.packages('IRkernel')", "IRkernel::installspec()"], commandsFor: "R", userWide: true },
    });
    render(<EnvironmentHelp env={r} />);
    expect(screen.getByTestId("setup-guidance-commands").textContent).toContain("IRkernel::installspec()");
    expect(screen.getByText("These install for your user account, not only for this project.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Copy the R commands" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("install.packages('IRkernel')\nIRkernel::installspec()"));
  });

  it("shows progress with Cancel and an honest outcome", async () => {
    useSetup.setState({ status: status(), log: status().log });
    const { unmount } = render(<SetupProgress status={status()} />);
    expect(screen.getByTestId("setup-progress").getAttribute("data-step")).toBe("1");
    expect(screen.getByText("Step 2 of 2: Install packages from PyPI")).toBeTruthy();
    expect(screen.getByTestId("setup-log").textContent).toContain("Downloading ipykernel");
    useSetup.setState({ status: status() });
    fireEvent.click(screen.getByTestId("setup-cancel"));
    await waitFor(() => expect(setupIpc.cancel).toHaveBeenCalledWith("setup-1"));
    unmount();

    const failed = status({
      state: "canceled",
      phase: "finished",
      removed: true,
      message: "Setup was canceled. The new environment in .venv was removed.",
    });
    render(<SetupOutcome status={failed} />);
    expect(screen.getByTestId("setup-outcome").getAttribute("data-removed")).toBe("true");
    expect(screen.getByText("Setup canceled")).toBeTruthy();
    expect(screen.getByTestId("setup-outcome-message").textContent).toContain("was removed");
  });

  it("selects and remembers the new environment when setup succeeds", async () => {
    const venv = env("python:venv", { displayName: "Python 3.12.10 (CPython, project .venv)", projectFolder: ".venv" });
    ipc.researchEnvironments.mockResolvedValue(report(system312, venv));
    applySetupEvent({ kind: "changed", status: status() });
    applySetupEvent({ kind: "output", setupId: "setup-1", step: 1, text: "Successfully installed ipykernel\n" });
    expect(useSetup.getState().log.at(-1)).toBe("Successfully installed ipykernel");
    applySetupEvent({
      kind: "changed",
      status: status({ state: "succeeded", phase: "finished", environmentId: venv.id, ready: true, finishedAtMs: 2 }),
    });
    await waitFor(() => expect(useResearch.getState().selectedEnvironment).toBe(venv.id));
    expect(setupIpc.select).toHaveBeenCalledWith(venv.id);
    expect(notify).toHaveBeenCalledWith(expect.objectContaining({ tone: "success", title: "Environment ready" }));
  });

  it("ignores another project's setup events", () => {
    applySetupEvent({ kind: "changed", status: status({ plan: plan({ root: "D:\\other" }) }) });
    expect(useSetup.getState().status).toBeNull();
  });
});
