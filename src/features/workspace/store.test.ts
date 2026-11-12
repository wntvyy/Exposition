// Workspace store: reopening the last project at startup (files.reopenLastProject),
// removing a folder from Recent, re-reading recovery entries after missed
// events, and telling the user about project settings that were set aside.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AppSettings } from "../../bindings/AppSettings";
import type { RecentProject } from "../../bindings/RecentProject";
import type { WorkspaceEvent } from "../../bindings/WorkspaceEvent";

const ipc = vi.hoisted(() => ({
  commands: {
    settingsGet: vi.fn(),
    workspaceRecent: vi.fn(),
    workspaceOpen: vi.fn(),
    workspaceCurrent: vi.fn(),
    workspaceForgetRecent: vi.fn(),
    workspaceListDir: vi.fn(async () => []),
    recoveryList: vi.fn(),
  },
  workspaceHandlers: [] as Array<(event: WorkspaceEvent) => void>,
}));
const notifications = vi.hoisted(() => ({ notify: vi.fn(), dismiss: vi.fn() }));

vi.mock("../../lib/ipc", () => ({
  commands: ipc.commands,
  events: {
    workspace: async (handler: (event: WorkspaceEvent) => void) => {
      ipc.workspaceHandlers.push(handler);
      return () => {};
    },
  },
  errorMessage: (e: unknown) => (typeof e === "object" && e && "message" in e ? String(e.message) : String(e)),
}));
vi.mock("../../app/notifications", () => notifications);
vi.mock("../../app/layout", () => ({ layout: { showView: vi.fn() } }));
vi.mock("../../app/editors", () => ({ editors: { all: () => [], close: vi.fn(), open: vi.fn() } }));
vi.mock("../editor/documentClient", () => ({ disposeAllHandles: vi.fn(), fileKey: (p: string) => `file:${p}` }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ setTitle: async () => {} }) }));

const { forgetRecent, reopenLastProject, startWorkspaceEvents, useWorkspace } = await import("./store");
const { resetAppSettingsForTests } = await import("../editor/settings");
const { resetSettingsWarnings } = await import("../settings/warnings");

const settings = (reopenLastProject: boolean): AppSettings => ({
  version: 1,
  editor: { fontSize: 13, tabSize: 2, insertSpaces: true, wordWrap: true, lineNumbers: true, minimap: false, defaultEol: "lf" },
  files: { reopenLastProject },
});
const paper: RecentProject = { path: "C:\\papers\\thesis", name: "thesis", openedAt: 2 };
const old: RecentProject = { path: "C:\\old", name: "old", openedAt: 1 };
const info = { root: "C:\\papers\\thesis", name: "thesis", recovery: [] };

beforeEach(() => {
  for (const fn of Object.values(ipc.commands)) fn.mockReset();
  ipc.commands.workspaceListDir.mockResolvedValue([]);
  notifications.notify.mockReset();
  sessionStorage.clear();
  resetAppSettingsForTests();
  resetSettingsWarnings();
  useWorkspace.setState({ info: null, recent: [], dirs: {}, expanded: new Set(), recovery: [], opening: false, error: null });
  ipc.commands.workspaceRecent.mockResolvedValue([paper, old]);
});

afterEach(() => vi.clearAllMocks());

describe("reopen the last project at startup", () => {
  it("opens the most recent folder when the setting is on, once per application start", async () => {
    ipc.commands.settingsGet.mockResolvedValue({ settings: settings(true), warning: null });
    ipc.commands.workspaceOpen.mockResolvedValue(info);
    expect(await reopenLastProject()).toBe("opened");
    expect(ipc.commands.workspaceOpen).toHaveBeenCalledWith("C:\\papers\\thesis");
    expect(useWorkspace.getState().info?.root).toBe("C:\\papers\\thesis");
    // A webview reload in the same session does not reopen (the user may have closed it).
    useWorkspace.setState({ info: null });
    expect(await reopenLastProject()).toBe("skipped");
    expect(ipc.commands.workspaceOpen).toHaveBeenCalledTimes(1);
  });

  it("keeps the default: nothing is opened when the setting is off", async () => {
    ipc.commands.settingsGet.mockResolvedValue({ settings: settings(false), warning: null });
    expect(await reopenLastProject()).toBe("skipped");
    expect(ipc.commands.workspaceOpen).not.toHaveBeenCalled();
  });

  it("leaves a short notice when the folder is gone or may not be opened", async () => {
    ipc.commands.settingsGet.mockResolvedValue({ settings: settings(true), warning: null });
    ipc.commands.workspaceOpen.mockRejectedValue({ code: "path.notFound", message: "C:\\papers\\thesis does not exist." });
    expect(await reopenLastProject()).toBe("failed");
    expect(useWorkspace.getState().info).toBeNull();
    expect(useWorkspace.getState().opening).toBe(false);
    expect(notifications.notify).toHaveBeenCalledWith(
      expect.objectContaining({ tone: "warning", title: "Could not reopen thesis", body: expect.stringContaining("does not exist") }),
    );
  });
});

describe("recent projects", () => {
  it("removes a folder from Recent through the workspace service", async () => {
    useWorkspace.setState({ recent: [paper, old] });
    ipc.commands.workspaceForgetRecent.mockResolvedValue([old]);
    await forgetRecent(paper);
    expect(ipc.commands.workspaceForgetRecent).toHaveBeenCalledWith("C:\\papers\\thesis");
    expect(useWorkspace.getState().recent).toEqual([old]);
  });
});

describe("workspace events", () => {
  it("re-reads recovery entries after missed events, and reports project settings that were set aside", async () => {
    ipc.commands.workspaceCurrent.mockResolvedValue(info);
    startWorkspaceEvents();
    await vi.waitFor(() => expect(useWorkspace.getState().info?.root).toBe("C:\\papers\\thesis"));
    const handler = ipc.workspaceHandlers.at(-1)!;

    const entry = { id: "rec-1", path: "main.tex", savedAt: 1, length: 3, disk: "unchanged", problem: null, location: null };
    ipc.commands.recoveryList.mockResolvedValue([entry]);
    handler({ kind: "filesChanged", changes: [], rescan: true });
    await vi.waitFor(() => expect(useWorkspace.getState().recovery).toEqual([entry]));

    const message = "Default target \"x\" is not defined. Defaults are in use; the original file was kept as C:\\p\\.exposition\\project.json.invalid-1.";
    handler({ kind: "projectSettingsInvalid", message });
    handler({ kind: "projectSettingsInvalid", message });
    const warnings = notifications.notify.mock.calls.filter(([n]) => n.id === "settings-warning-project");
    expect(warnings).toHaveLength(1);
    expect(warnings[0]![0]).toMatchObject({ tone: "warning", body: message });
  });
});
