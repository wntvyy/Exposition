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
const switching = vi.hoisted(() => ({
  dirtyWork: vi.fn(async (): Promise<unknown[]> => []),
  activeWork: vi.fn(async (): Promise<unknown[]> => []),
  decide: vi.fn(async (): Promise<string> => "cancel"),
  saveDocuments: vi.fn(async () => true),
  saveEditor: vi.fn(async () => true),
  discardEditors: vi.fn(async () => {}),
}));
vi.mock("./projectSwitch", () => ({
  switchDeps: (currentName: () => string | null) => ({ currentName, ...switching }),
}));

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

const { forgetRecent, loadDir, openWorkspace, reopenLastProject, samePath, shareEntries, startWorkspaceEvents, useWorkspace } =
  await import("./store");
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

describe("opening another folder while one is open (project switch)", () => {
  const beta = { root: "C:\\papers\\beta", name: "beta", recovery: [] };
  const unsaved = [{ key: "file:main.tex", name: "main.tex", document: "doc-1" }];

  beforeEach(() => {
    useWorkspace.setState({ info });
    switching.dirtyWork.mockReset().mockResolvedValue([]);
    switching.decide.mockReset().mockResolvedValue("cancel");
  });

  it("compares folder spellings like Windows does", () => {
    expect(samePath("C:\\Papers\\Thesis\\", "c:/papers/thesis")).toBe(true);
    expect(samePath("C:\\papers\\thesis", "C:\\papers\\thesis2")).toBe(false);
  });

  it("reopening the open folder asks nothing", async () => {
    switching.dirtyWork.mockResolvedValue(unsaved);
    ipc.commands.workspaceOpen.mockResolvedValue(info);
    expect(await openWorkspace("c:\\papers\\THESIS")).toBe(true);
    expect(switching.decide).not.toHaveBeenCalled();
  });

  it("Cancel keeps the project and opens nothing", async () => {
    switching.dirtyWork.mockResolvedValue(unsaved);
    expect(await openWorkspace(beta.root)).toBe(false);
    expect(switching.decide).toHaveBeenCalledWith(expect.objectContaining({ action: "open", from: "thesis", to: "beta" }));
    expect(ipc.commands.workspaceOpen).not.toHaveBeenCalled();
    expect(useWorkspace.getState().info?.root).toBe(info.root);
  });

  it("Don't Save sends the documents with the open request; the new folder becomes current", async () => {
    switching.dirtyWork.mockResolvedValue(unsaved);
    switching.decide.mockResolvedValue("discard");
    ipc.commands.workspaceOpen.mockResolvedValue(beta);
    expect(await openWorkspace(beta.root)).toBe(true);
    expect(ipc.commands.workspaceOpen).toHaveBeenCalledWith(beta.root, ["doc-1"]);
    expect(useWorkspace.getState().info?.root).toBe(beta.root);
  });

  it("a folder that cannot be opened leaves the open project and says so", async () => {
    switching.dirtyWork.mockResolvedValue(unsaved);
    switching.decide.mockResolvedValue("discard");
    ipc.commands.workspaceOpen.mockRejectedValue({ code: "workspace.notFound", message: "C:\\gone does not exist." });
    expect(await openWorkspace("C:\\gone")).toBe(false);
    expect(useWorkspace.getState().info?.root).toBe(info.root);
    expect(useWorkspace.getState().opening).toBe(false);
    expect(useWorkspace.getState().error).toBeNull();
    expect(notifications.notify).toHaveBeenCalledWith(
      expect.objectContaining({
        tone: "danger",
        title: "Could not open folder",
        body: "C:\\gone does not exist. thesis is still open; nothing was closed or discarded.",
      }),
    );
  });

  it("a failed Save All opens nothing and reports the actual state", async () => {
    switching.dirtyWork.mockResolvedValueOnce(unsaved).mockResolvedValue(unsaved);
    switching.decide.mockResolvedValue("save");
    switching.saveDocuments.mockResolvedValue(false);
    expect(await openWorkspace(beta.root)).toBe(false);
    expect(ipc.commands.workspaceOpen).not.toHaveBeenCalled();
    expect(notifications.notify).toHaveBeenCalledWith(
      expect.objectContaining({ tone: "warning", title: "Did not open beta", body: "Not saved: main.tex. thesis is still open." }),
    );
  });
});

describe("folder listings (perf FE-5 / WS-7)", () => {
  const entry = (path: string, size: number | null = 1) => ({
    name: path.split("/").pop() ?? path,
    path,
    kind: "file" as const,
    size,
    link: false,
    unavailable: null,
  });
  const deferred = <T,>() => {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>((r) => (resolve = r));
    return { promise, resolve };
  };

  it("lists a folder with one request at a time; requests made meanwhile share one more listing", async () => {
    const first = deferred<unknown[]>();
    const second = deferred<unknown[]>();
    const answers = [first.promise, second.promise];
    ipc.commands.workspaceListDir.mockReset();
    ipc.commands.workspaceListDir.mockImplementation(((path: string | null) =>
      path === "flat" ? answers.shift() : Promise.resolve([])) as never);
    const flatCalls = () => (ipc.commands.workspaceListDir.mock.calls as unknown[][]).filter((call) => call[0] === "flat");
    const a = loadDir("flat");
    // A burst of change notifications while the listing is in flight.
    const b = loadDir("flat");
    const c = loadDir("flat");
    void loadDir("other");
    expect(flatCalls()).toHaveLength(1);
    first.resolve([entry("flat/a.txt")]);
    await Promise.resolve();
    await Promise.resolve();
    // The listing that was running may predate the changes: one more, not two.
    await vi.waitFor(() => expect(flatCalls()).toHaveLength(2));
    second.resolve([entry("flat/a.txt"), entry("flat/b.txt")]);
    await Promise.all([a, b, c]);
    expect(flatCalls()).toHaveLength(2);
    expect(useWorkspace.getState().dirs.flat?.entries?.map((e) => e.path)).toEqual(["flat/a.txt", "flat/b.txt"]);
    // Idle again: the next request lists again.
    ipc.commands.workspaceListDir.mockResolvedValue([entry("flat/a.txt")] as never);
    await loadDir("flat");
    expect(useWorkspace.getState().dirs.flat?.entries?.map((e) => e.path)).toEqual(["flat/a.txt"]);
  });

  it("an unchanged listing changes nothing; a changed one keeps the objects of unchanged entries", async () => {
    ipc.commands.workspaceListDir.mockResolvedValue([entry("a.txt"), entry("b.txt")] as never);
    await loadDir("");
    const before = useWorkspace.getState().dirs;
    const updates = vi.fn();
    const off = useWorkspace.subscribe(updates);
    ipc.commands.workspaceListDir.mockResolvedValue([entry("a.txt"), entry("b.txt")] as never);
    await loadDir("");
    expect(updates).not.toHaveBeenCalled();
    expect(useWorkspace.getState().dirs).toBe(before);

    ipc.commands.workspaceListDir.mockResolvedValue([entry("a.txt"), entry("b.txt", 2), entry("c.txt")] as never);
    await loadDir("");
    off();
    expect(updates).toHaveBeenCalledTimes(1);
    const after = useWorkspace.getState().dirs[""]?.entries ?? [];
    expect(after.map((e) => e.path)).toEqual(["a.txt", "b.txt", "c.txt"]);
    expect(after[0]).toBe(before[""]?.entries?.[0]);
    expect(after[1]).not.toBe(before[""]?.entries?.[1]);
    expect(after[1]?.size).toBe(2);
    expect(shareEntries([], [])).toEqual([]);
  });

  it("drops the answer of a listing that was requested for a folder that is no longer open", async () => {
    const slow = deferred<unknown[]>();
    ipc.commands.workspaceListDir.mockReset();
    ipc.commands.workspaceListDir.mockReturnValueOnce(slow.promise as never).mockResolvedValue([entry("new.txt")] as never);
    const stale = loadDir("");
    ipc.commands.settingsGet.mockResolvedValue({ settings: settings(false), warning: null });
    ipc.commands.workspaceOpen.mockResolvedValue({ root: "C:\other", name: "other", recovery: [] });
    expect(await openWorkspace("C:\other")).toBe(true);
    await vi.waitFor(() => expect(useWorkspace.getState().dirs[""]?.entries?.map((e) => e.path)).toEqual(["new.txt"]));
    slow.resolve([entry("old-project.txt")]);
    await stale;
    expect(useWorkspace.getState().dirs[""]?.entries?.map((e) => e.path)).toEqual(["new.txt"]);
  });
});
