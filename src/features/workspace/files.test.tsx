// The Files explorer with the real tree, selection, dispatcher and operation
// client (backend mocked at the operation IPC): a plain click still opens a
// file, Ctrl+click selects without opening, Delete on the focused selection
// plans and applies at once, the paste destination is shown, and a
// right-click on an unselected row selects it alone.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { Children, type ReactElement, type ReactNode } from "react";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { FileEntry } from "../../bindings/FileEntry";

const opIpc = vi.hoisted(() => ({ plan: vi.fn(), apply: vi.fn(), restore: vi.fn(), clipboardFilesRead: vi.fn(), clipboardFilesWrite: vi.fn() }));
vi.mock("../operations/ipc", () => ({ opCommands: opIpc, opEvents: { projectOp: vi.fn(async () => () => {}) } }));
vi.mock("../research/FilesSection", () => ({ ResearchFilesSection: () => null }));
const store = vi.hoisted(() => ({ openFile: vi.fn(), setExpanded: vi.fn(), loadDir: vi.fn() }));
vi.mock("./store", async () => {
  const { create } = await import("zustand");
  const useWorkspace = create(() => ({
    info: null as null | { root: string; name: string },
    dirs: {} as Record<string, { entries: FileEntry[] | null; loading: boolean; error: string | null }>,
    expanded: new Set<string>(),
    recovery: [],
    recent: [],
    opening: false,
    error: null,
  }));
  return {
    ROOT: "",
    useWorkspace,
    openFile: store.openFile,
    setExpanded: (keys: Set<string>) => {
      store.setExpanded(keys);
      useWorkspace.setState({ expanded: keys });
    },
    loadDir: store.loadDir,
    discardRecovery: vi.fn(),
    forgetRecent: vi.fn(),
    openFolderDialog: vi.fn(),
    openWorkspace: vi.fn(),
    restoreRecovery: vi.fn(),
  };
});

import { installKeybindings } from "../../app/keybindings";
import { registry } from "../../app/registry";
import { ConfirmHost } from "../../ui/components";
import { opHistory } from "../operations/client";
import { operationCommands, resetSurfacesForTests } from "../operations/surfaces";
import { FilesView, renderEntries } from "./FilesView";
import { useWorkspace } from "./store";

const entry = (path: string, kind: "file" | "directory" = "file"): FileEntry => ({
  name: path.split("/").pop()!,
  path,
  kind,
  size: kind === "file" ? 3 : null,
  link: false,
  unavailable: null,
});

let disposers: (() => void)[] = [];
let uninstall = () => {};

beforeAll(() => {
  Object.defineProperty(navigator, "platform", { value: "Win32", configurable: true });
});

beforeEach(() => {
  for (const fn of Object.values(opIpc)) fn.mockReset();
  store.openFile.mockReset();
  opHistory.clear();
  useWorkspace.setState({
    info: { root: "C:\\proj", name: "proj" },
    dirs: {
      "": { entries: [entry("chapters", "directory"), entry("main.typ"), entry("notes.md")], loading: false, error: null },
      chapters: { entries: [entry("chapters/one.typ")], loading: false, error: null },
    },
    expanded: new Set(),
  } as never);
  disposers = operationCommands().map((c) => registry.registerCommand(c));
  uninstall = installKeybindings(window, "windows");
  opIpc.plan.mockImplementation(async (request) => ({
    plan: "p",
    request,
    effects: request.targets.map((target: unknown) => ({ target, description: "Move to the Recycle Bin", destination: null, recovery: { kind: "osTrash" } })),
    refusals: [],
    impacts: [],
    needsDecision: false,
    choices: [],
    planHash: "h",
    expiresAtMs: 0,
  }));
  opIpc.apply.mockImplementation(async () => ({ plan: "p", outcomes: [], notices: [] }));
});

afterEach(() => {
  uninstall();
  for (const d of disposers) d();
  resetSurfacesForTests();
  cleanup();
});

afterAll(() => {
  document.body.innerHTML = "";
});

const row = (path: string) => screen.getByTestId("file-tree").querySelector(`[data-path="${path}"]`) as HTMLElement;
const title = (path: string) => row(path).querySelector(".ui-tree-item__title") as HTMLElement;

function ctrlClick(el: HTMLElement) {
  fireEvent.pointerDown(el, { button: 0, ctrlKey: true, pointerType: "mouse" });
  fireEvent.mouseDown(el, { button: 0, ctrlKey: true });
  fireEvent.pointerUp(el, { button: 0, ctrlKey: true, pointerType: "mouse" });
  fireEvent.mouseUp(el, { button: 0, ctrlKey: true });
  fireEvent.click(el, { button: 0, ctrlKey: true });
}

describe("Files explorer operations", () => {
  it("a plain click opens a file; Ctrl+click selects without opening; Delete applies at once", async () => {
    render(
      <>
        <ConfirmHost />
        <FilesView />
      </>,
    );
    fireEvent.click(title("main.typ"), { button: 0, detail: 1 });
    expect(store.openFile).toHaveBeenCalledWith(expect.objectContaining({ path: "main.typ" }));
    store.openFile.mockClear();

    ctrlClick(title("notes.md"));
    await waitFor(() => expect(row("notes.md").getAttribute("aria-selected")).toBe("true"));
    expect(store.openFile).not.toHaveBeenCalled();

    act(() => row("notes.md").focus());
    fireEvent.keyDown(row("notes.md"), { key: "Delete", code: "Delete" });
    await waitFor(() => expect(opIpc.apply).toHaveBeenCalled());
    expect(opIpc.plan).toHaveBeenCalledWith({ kind: { kind: "delete", permanent: false }, targets: expect.arrayContaining([{ kind: "path", path: "notes.md" }]), actor: { kind: "user" } });
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("right click on an unselected file selects it alone and shows where Paste goes", async () => {
    render(<FilesView />);
    fireEvent.contextMenu(title("main.typ"), { button: 2, clientX: 20, clientY: 20 });
    const menu = await screen.findByRole("menu", { name: "File actions" });
    await waitFor(() => expect(row("main.typ").getAttribute("aria-selected")).toBe("true"));
    expect(within(menu).getByRole("menuitem", { name: /Paste into the project folder/ })).toBeTruthy();
    expect(within(menu).getByRole("menuitem", { name: /Delete Permanently/ })).toBeTruthy();
    fireEvent.keyDown(menu, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

    // A folder is the destination itself.
    fireEvent.contextMenu(title("chapters"), { button: 2, clientX: 20, clientY: 20 });
    const folderMenu = await screen.findByRole("menu", { name: "File actions" });
    expect(within(folderMenu).getByRole("menuitem", { name: /Paste into chapters\// })).toBeTruthy();
  });

  // UI-8: Discard removes the only copy of recovered text; the banner says so.
  it("builds rows only for expanded folders and keeps the rows of unchanged files (perf FE-5)", () => {
    const dirs = useWorkspace.getState().dirs;
    const root = dirs[""]!.entries!;
    const rows = (expanded: string[]) => renderEntries(root, dirs, new Set(expanded)) as ReactElement<{ children?: ReactNode }>[];
    const collapsed = rows([]);
    // The loaded listing of a collapsed folder is not built into the tree.
    expect(Children.toArray(collapsed[0]!.props.children)).toHaveLength(0);
    const open = rows(["chapters"]);
    expect(Children.toArray(open[0]!.props.children)).toHaveLength(1);
    // A file's row is the same element as long as its entry object is.
    expect(open[1]).toBe(collapsed[1]);
    expect(open[2]).toBe(collapsed[2]);
    const changed = [root[0]!, { ...root[1]!, size: 99 }, root[2]!];
    const next = renderEntries(changed, dirs, new Set()) as ReactElement[];
    expect(next[1]).not.toBe(collapsed[1]);
    expect(next[2]).toBe(collapsed[2]);

    // In the tree: expanding shows the folder's files, collapsing removes them.
    render(<FilesView />);
    expect(screen.getByTestId("file-tree").querySelector('[data-path="chapters/one.typ"]')).toBeNull();
    act(() => useWorkspace.setState({ expanded: new Set(["chapters"]) } as never));
    expect(row("chapters/one.typ")).toBeTruthy();
    act(() => useWorkspace.setState({ expanded: new Set() } as never));
    expect(screen.getByTestId("file-tree").querySelector('[data-path="chapters/one.typ"]')).toBeNull();
  });

  it("states that discarded recovered changes cannot be restored", () => {
    useWorkspace.setState({
      recovery: [{ id: "r1", path: "main.typ", savedAt: 0, length: 3, disk: "unchanged", problem: null, location: null }],
    } as never);
    render(<FilesView />);
    const list = screen.getByTestId("recovery-list");
    expect(within(list).getByRole("button", { name: "Restore" })).toBeTruthy();
    expect(within(list).getByRole("button", { name: "Discard" })).toBeTruthy();
    expect(screen.getByTestId("recovery-consequence").textContent).toBe("Discarded changes cannot be restored. The file on disk stays as it is.");
    act(() => useWorkspace.setState({ recovery: [] } as never));
  });

  it("Ctrl+V pastes the OS clipboard's files into the selected folder", async () => {
    opIpc.clipboardFilesRead.mockResolvedValue(["C:\\proj\\notes.md", "D:\\elsewhere\\data.csv"]);
    render(<FilesView />);
    ctrlClick(title("chapters"));
    await waitFor(() => expect(row("chapters").getAttribute("aria-selected")).toBe("true"));
    act(() => row("chapters").focus());
    fireEvent.keyDown(row("chapters"), { key: "v", code: "KeyV", ctrlKey: true });
    await waitFor(() => expect(opIpc.plan).toHaveBeenCalled());
    expect(opIpc.plan).toHaveBeenCalledWith({
      kind: { kind: "paste", destination: { kind: "folder", folder: "chapters" } },
      targets: [
        { kind: "path", path: "notes.md" },
        { kind: "external", path: "D:\\elsewhere\\data.csv" },
      ],
      actor: { kind: "user" },
    });
  });
});
