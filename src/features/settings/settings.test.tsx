import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ExecutionSettingsState } from "../../bindings/ExecutionSettingsState";
import type { TrustList } from "../../bindings/TrustList";

const ipc = vi.hoisted(() => ({
  appInfo: vi.fn(),
  executionSettingsGet: vi.fn(),
  executionSettingsUpdate: vi.fn(),
  dockerImagesList: vi.fn(),
  trustList: vi.fn(),
  trustRevoke: vi.fn(),
  toolchainsList: vi.fn(),
  toolchainsRefresh: vi.fn(),
  jobList: vi.fn(),
}));
const ui = vi.hoisted(() => ({ confirm: vi.fn() }));
const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));

vi.mock("../../lib/ipc", () => ({
  commands: ipc,
  events: { jobs: vi.fn(), jobsResync: vi.fn() },
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
}));
vi.mock("../../app/notifications", () => notifications);
vi.mock("../../ui/components", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../ui/components")>()),
  confirm: ui.confirm,
}));

const { TrustSection } = await import("./TrustSection");
const { ExecutionSection } = await import("./ExecutionSection");
const { initialSettingsState, useSettings } = await import("./store");
const { useJobs } = await import("../jobs/store");

const trustList: TrustList = {
  roots: [
    { path: "C:\\papers\\thesis", grantedAtMs: Date.UTC(2026, 10, 11, 12), exists: true, ignoredReason: null },
    { path: "C:\\old\\gone", grantedAtMs: null, exists: false, ignoredReason: null },
  ],
  rules: {
    summary: "Trust covers a folder and everything inside it.",
    personal: [{ description: "your user profile folder", path: "C:\\Users\\me" }],
    system: [{ description: "the Windows folder", path: "C:\\Windows" }],
  },
};

function executionState(image: string | null, toolPaths: Record<string, string> = {}): ExecutionSettingsState {
  return {
    settings: {
      toolPaths,
      docker: { image, memory: "2g", cpus: "2", pidsLimit: 256, user: "65534:65534" },
      maxConcurrentJobs: null,
    },
    overridableTools: ["python", "docker"],
  };
}

const python = {
  id: "python",
  name: "Python",
  category: "python",
  path: "C:\\Py\\python.exe",
  source: "knownLocation" as const,
  version: "Python 3.14.0",
  status: "available" as const,
  detail: null,
  guidance: null,
};

beforeEach(() => {
  useSettings.setState(initialSettingsState, true);
  useJobs.setState({ toolchains: [python], toolchainsLoading: false, toolchainsError: null });
  for (const fn of [...Object.values(ipc), ui.confirm, notifications.notify]) fn.mockReset();
  ipc.trustList.mockResolvedValue(trustList);
  ipc.executionSettingsGet.mockResolvedValue(executionState(null));
  ipc.dockerImagesList.mockResolvedValue([{ reference: "python:3.13-slim", id: "e48f59cdba69", size: "178MB", created: "6 days ago" }]);
  ipc.toolchainsRefresh.mockResolvedValue([python]);
  ipc.jobList.mockResolvedValue([]);
});

afterEach(cleanup);

describe("Trusted folders", () => {
  it("lists folders with grant state and offers no way to add one", async () => {
    const { container } = render(<TrustSection />);
    const rows = await screen.findAllByTestId("trusted-folder");
    expect(rows.map((r) => r.dataset.path)).toEqual(["C:\\papers\\thesis", "C:\\old\\gone"]);
    expect(within(rows[0]!).getByText(/^Trusted .*2026/)).toBeTruthy();
    expect(within(rows[1]!).getByText("Trusted (date not recorded)")).toBeTruthy();
    expect(within(rows[1]!).getByText("Folder no longer exists")).toBeTruthy();
    expect(screen.getByTestId("trust-rules").textContent).toContain("C:\\Windows");
    // Trust is granted only by the trust prompt: no path input, no grant action.
    expect(container.querySelector("input, textarea")).toBeNull();
    expect(screen.queryByRole("button", { name: /^trust|add/i })).toBeNull();
  });

  it("revokes only after confirmation", async () => {
    render(<TrustSection />);
    await screen.findAllByTestId("trusted-folder");
    ui.confirm.mockResolvedValueOnce("cancel");
    fireEvent.click(screen.getByRole("button", { name: "Revoke trust for C:\\papers\\thesis" }));
    await waitFor(() => expect(ui.confirm).toHaveBeenCalledTimes(1));
    expect(ui.confirm.mock.calls[0]![0]).toMatchObject({ cancelId: "cancel", defaultId: "cancel" });
    expect(ipc.trustRevoke).not.toHaveBeenCalled();

    ui.confirm.mockResolvedValueOnce("revoke");
    ipc.trustRevoke.mockResolvedValue({ root: "C:\\papers\\thesis", trusted: false, trustedVia: null });
    ipc.trustList.mockResolvedValue({ ...trustList, roots: trustList.roots.slice(1) });
    fireEvent.click(screen.getByRole("button", { name: "Revoke trust for C:\\papers\\thesis" }));
    await waitFor(() => expect(ipc.trustRevoke).toHaveBeenCalledWith("C:\\papers\\thesis"));
    await waitFor(() => expect(screen.getAllByTestId("trusted-folder")).toHaveLength(1));
  });
});

describe("Trusted folders: nested", () => {
  it("names trusted folders inside the one being revoked", async () => {
    ipc.trustList.mockResolvedValue({
      ...trustList,
      roots: [
        { path: "C:\\papers", grantedAtMs: 1, exists: true, ignoredReason: null },
        { path: "C:\\papers\\thesis", grantedAtMs: 2, exists: true, ignoredReason: null },
      ],
    });
    render(<TrustSection />);
    await screen.findAllByTestId("trusted-folder");
    ui.confirm.mockResolvedValueOnce("cancel");
    fireEvent.click(screen.getByRole("button", { name: "Revoke trust for C:\\papers" }));
    await waitFor(() => expect(ui.confirm).toHaveBeenCalledTimes(1));
    cleanup();
    render(<>{ui.confirm.mock.calls[0]![0].message}</>);
    expect(screen.getByText(/inside it are revoked too/)).toBeTruthy();
    expect(screen.getByText("C:\\papers\\thesis")).toBeTruthy();
  });
});

describe("Execution", () => {
  it("shows the server's refusal for a tool location and keeps the dialog open", async () => {
    render(<ExecutionSection />);
    const button = await screen.findByRole("button", { name: "Set location of Python" });
    fireEvent.click(button);
    const dialog = await screen.findByRole("dialog", { name: "Location of Python" });
    const input = within(dialog).getByLabelText("Executable path") as HTMLInputElement;
    expect(input.value).toBe("C:\\Py\\python.exe");
    fireEvent.change(input, { target: { value: "C:\\Py\\python.cmd" } });
    ipc.executionSettingsUpdate.mockRejectedValueOnce({
      code: "settings.invalidToolPath",
      message: "C:\\Py\\python.cmd is run by cmd.exe, which re-parses arguments as a shell command line.",
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "Use this executable" }));
    expect(await within(dialog).findByText(/is run by cmd\.exe/)).toBeTruthy();
    expect(ipc.executionSettingsUpdate).toHaveBeenCalledWith({ kind: "toolPath", tool: "python", path: "C:\\Py\\python.cmd" });
    expect(ipc.toolchainsRefresh).not.toHaveBeenCalled();

    // A valid path is saved, discovery re-runs, and the dialog closes.
    ipc.executionSettingsUpdate.mockResolvedValueOnce(executionState(null, { python: "D:\\Py312\\python.exe" }));
    fireEvent.change(input, { target: { value: "D:\\Py312\\python.exe" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Use this executable" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(ipc.toolchainsRefresh).toHaveBeenCalled();
    expect(await screen.findByRole("button", { name: "Use the discovered Python" })).toBeTruthy();
  });

  it("states what restricted runs enforce and that nothing is configured yet", async () => {
    render(<ExecutionSection />);
    const guarantees = await screen.findByTestId("restricted-guarantees");
    expect(guarantees.textContent).toContain("--network=none");
    expect(guarantees.textContent).toContain("65534:65534");
    expect((await screen.findByTestId("docker-image-state")).textContent).toMatch(/No image configured/);
    await act(async () => {
      await Promise.resolve();
    });
    expect(ipc.dockerImagesList).toHaveBeenCalled();
  });

  it("warns when the configured image is not present locally", async () => {
    ipc.executionSettingsGet.mockResolvedValue(executionState("gone:1"));
    render(<ExecutionSection />);
    expect(await screen.findByText("gone:1 is not present locally")).toBeTruthy();
  });
});
