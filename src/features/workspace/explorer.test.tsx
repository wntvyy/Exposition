// QOL E/G in the Files explorer (real tree, dispatcher, dialogs; backend
// mocked at the file IPC): New File suggests Untitled.txt with its stem
// selected and uses the typed name exactly; a collision keeps the dialog and
// the typed name; F2 renames only with an explorer item focused (never from
// a text surface such as Monaco); a rename re-keys open tabs in place; a
// native drop imports into the folder under the pointer.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { FileEntry } from "../../bindings/FileEntry";
import type { FileImportProgress } from "../../bindings/FileImportProgress";

const fileIpc = vi.hoisted(() => ({
  create: vi.fn(),
  renameCheck: vi.fn(),
  rename: vi.fn(),
  importStart: vi.fn(),
  importCancel: vi.fn(),
}));
vi.mock("./fileIpc", () => ({ fileCommands: fileIpc, fileEvents: { import: vi.fn(async () => () => {}) } }));
vi.mock("../operations/ipc", () => ({ opCommands: {}, opEvents: { projectOp: vi.fn(async () => () => {}) } }));
vi.mock("../research/FilesSection", () => ({ ResearchFilesSection: () => null }));
const store = vi.hoisted(() => ({ openFile: vi.fn(), loadDir: vi.fn(), refreshAfterOperation: vi.fn() }));
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
    FILES_VIEW_ID: "workspace.files",
    useWorkspace,
    openFile: store.openFile,
    setExpanded: (keys: Set<string>) => useWorkspace.setState({ expanded: keys }),
    loadDir: store.loadDir,
    refreshAfterOperation: store.refreshAfterOperation,
    discardRecovery: vi.fn(),
    forgetRecent: vi.fn(),
    openFolderDialog: vi.fn(),
    openWorkspace: vi.fn(),
    restoreRecovery: vi.fn(),
    startWorkspaceEvents: vi.fn(),
    closeWorkspace: vi.fn(),
  };
});

import { editors, useEditors } from "../../app/editors";
import { installKeybindings } from "../../app/keybindings";
import { registry } from "../../app/registry";
import { operationCommands, resetSurfacesForTests } from "../operations/surfaces";
import { destinationAt, handleNativeDrop, useDropTarget } from "./dropTarget";
import { activeRenameTarget, openNewFileDialog, openRenameDialog, stemLength, summarizeImport, useFileDialog } from "./explorerActions";
import { FilesView } from "./FilesView";
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
  // jsdom has no layout: the drop tests say what is under the pointer.
  if (!document.elementFromPoint) document.elementFromPoint = () => null;
});

beforeEach(async () => {
  for (const fn of [...Object.values(fileIpc), ...Object.values(store)]) fn.mockReset();
  fileIpc.renameCheck.mockResolvedValue({ path: "x", isDir: false, references: [], unknown: null });
  useFileDialog.setState({ request: null });
  useWorkspace.setState({
    info: { root: "C:\\proj", name: "proj" },
    dirs: {
      "": { entries: [entry("chapters", "directory"), entry("main.typ"), entry("notes.txt")], loading: false, error: null },
      chapters: { entries: [entry("chapters/one.tex")], loading: false, error: null },
    },
    expanded: new Set(),
  } as never);
  disposers = operationCommands().map((c) => registry.registerCommand(c));
  // The explorer's commands (New File, Rename).
  await import("./register");
  uninstall = installKeybindings(window, "windows");
});

afterEach(() => {
  uninstall();
  for (const d of disposers) d();
  resetSurfacesForTests();
  cleanup();
  for (const e of editors.all()) void editors.close(e.resource.key);
});

const row = (path: string) => screen.getByTestId("file-tree").querySelector(`[data-path="${path}"]`) as HTMLElement;
const title = (path: string) => row(path).querySelector(".ui-tree-item__title") as HTMLElement;
const nameInput = () => within(screen.getByTestId("file-name-dialog")).getByRole("textbox") as HTMLInputElement;

function ctrlClick(el: HTMLElement) {
  fireEvent.pointerDown(el, { button: 0, ctrlKey: true, pointerType: "mouse" });
  fireEvent.mouseDown(el, { button: 0, ctrlKey: true });
  fireEvent.pointerUp(el, { button: 0, ctrlKey: true, pointerType: "mouse" });
  fireEvent.mouseUp(el, { button: 0, ctrlKey: true });
  fireEvent.click(el, { button: 0, ctrlKey: true });
}

describe("New File", () => {
  it("suggests Untitled.txt with the stem selected and creates exactly the typed name", async () => {
    fileIpc.create.mockResolvedValue({ path: "paper.tex" });
    render(<FilesView />);
    fireEvent.click(screen.getByTestId("files-new-file"));
    await waitFor(() => expect(nameInput().value).toBe("Untitled.txt"));
    await waitFor(() => expect([nameInput().selectionStart, nameInput().selectionEnd]).toEqual([0, 8]));
    // The person replaces the whole name with another extension.
    fireEvent.change(nameInput(), { target: { value: "paper.tex" } });
    fireEvent.keyDown(nameInput(), { key: "Enter" });
    await waitFor(() => expect(fileIpc.create).toHaveBeenCalledWith(null, "paper.tex"));
    await waitFor(() => expect(screen.queryByTestId("file-name-dialog")).toBeNull());
    await waitFor(() => expect(store.openFile).toHaveBeenCalledWith({ path: "paper.tex", name: "paper.tex" }));
  });

  it("creates in the selected folder (or a selected file's folder), and keeps the typed name on a collision", async () => {
    fileIpc.create.mockRejectedValueOnce({ code: "file.exists", message: "chapters/ already contains “one.tex”. Choose another name." });
    render(<FilesView />);
    ctrlClick(title("chapters"));
    await waitFor(() => expect(row("chapters").getAttribute("aria-selected")).toBe("true"));
    fireEvent.contextMenu(title("chapters"), { button: 2, clientX: 10, clientY: 10 });
    const menu = await screen.findByRole("menu", { name: "File actions" });
    fireEvent.click(within(menu).getByRole("menuitem", { name: /New File/ }));
    await waitFor(() => expect(nameInput().value).toBe("Untitled.txt"));
    fireEvent.change(nameInput(), { target: { value: "one.tex" } });
    fireEvent.click(screen.getByTestId("file-name-submit"));
    await waitFor(() => expect(fileIpc.create).toHaveBeenCalledWith("chapters", "one.tex"));
    // Refused: the dialog stays with the name and the reason; nothing opened.
    await screen.findByText(/already contains “one.tex”/);
    expect(nameInput().value).toBe("one.tex");
    expect(store.openFile).not.toHaveBeenCalled();
    // A leading-dot, extensionless name is sent as typed.
    fileIpc.create.mockResolvedValueOnce({ path: "chapters/.latexmkrc" });
    fireEvent.change(nameInput(), { target: { value: ".latexmkrc" } });
    fireEvent.click(screen.getByTestId("file-name-submit"));
    await waitFor(() => expect(fileIpc.create).toHaveBeenLastCalledWith("chapters", ".latexmkrc"));
  });

  it("Ctrl+N opens New File for the project folder when nothing is selected", async () => {
    render(<FilesView />);
    fireEvent.keyDown(document.body, { key: "n", code: "KeyN", ctrlKey: true });
    await waitFor(() => expect(useFileDialog.getState().request).toEqual({ mode: "new", surface: "files", folder: null }));
  });
});

describe("Rename", () => {
  it("F2 renames the focused explorer item; F2 in a text surface is left to it", async () => {
    fileIpc.rename.mockResolvedValue({
      from: "notes.txt",
      to: "notes.md",
      isDir: false,
      documents: [],
      graph: { kind: "recorded", renamed: 1 },
      references: [],
    });
    render(
      <>
        <textarea data-testid="monaco-like" />
        <FilesView />
      </>,
    );
    // A text surface (Monaco's own F2 renames a symbol): nothing here.
    const text = screen.getByTestId("monaco-like");
    act(() => text.focus());
    fireEvent.keyDown(text, { key: "F2", code: "F2" });
    expect(useFileDialog.getState().request).toBeNull();

    ctrlClick(title("notes.txt"));
    await waitFor(() => expect(row("notes.txt").getAttribute("aria-selected")).toBe("true"));
    act(() => row("notes.txt").focus());
    expect(activeRenameTarget()?.target.path).toBe("notes.txt");
    fireEvent.keyDown(row("notes.txt"), { key: "F2", code: "F2" });
    await waitFor(() => expect(nameInput().value).toBe("notes.txt"));
    await waitFor(() => expect([nameInput().selectionStart, nameInput().selectionEnd]).toEqual([0, 5]));
    fireEvent.change(nameInput(), { target: { value: "notes.md" } });
    fireEvent.keyDown(nameInput(), { key: "Enter" });
    await waitFor(() => expect(fileIpc.rename).toHaveBeenCalledWith("notes.txt", "notes.md"));
    await waitFor(() => expect(screen.queryByTestId("file-name-dialog")).toBeNull());
    expect(store.refreshAfterOperation).toHaveBeenCalledWith(["notes.txt", "notes.md"]);
  });

  it("lists what still uses the current name before renaming, and a refusal keeps the typed name", async () => {
    fileIpc.renameCheck.mockResolvedValue({
      path: "chapters/one.tex",
      isDir: false,
      references: [{ kind: "manuscriptDependency", label: "main.tex", message: "Uses “one.tex” by its current name." }],
      unknown: null,
    });
    fileIpc.rename.mockRejectedValue({ code: "name.reserved", message: "“con” is a reserved name on Windows and cannot be used." });
    render(<FilesView />);
    act(() => openRenameDialog({ path: "chapters/one.tex", name: "one.tex", kind: "file" }));
    const refs = await screen.findByTestId("rename-references");
    expect(refs.textContent).toContain("main.tex");
    fireEvent.change(nameInput(), { target: { value: "con.tex" } });
    fireEvent.click(screen.getByTestId("file-name-submit"));
    await screen.findByText(/reserved name on Windows/);
    expect(nameInput().value).toBe("con.tex");
  });

  it("a renamed file's tab keeps its place, mode, dirty and active state under the new key", async () => {
    editors.open({ key: "file:a.txt", path: "a.txt", title: "a.txt" }, { reveal: false });
    editors.open({ key: "file:img.png", path: "img.png", title: "img.png" }, { reveal: false });
    editors.open({ key: "file:b.typ", path: "b.typ", title: "b.typ" }, { reveal: false });
    editors.setDirty("file:img.png", true);
    editors.activate("file:img.png");
    let guarded = 0;
    editors.registerCloseGuard("file:img.png", async () => {
      guarded += 1;
      return true;
    });
    const { applyRenamed } = await import("./explorerActions");
    await applyRenamed({ from: "img.png", to: "figure.png", isDir: false, documents: [], graph: { kind: "unavailable" }, references: [] });
    const group = useEditors.getState().groups.primary;
    expect(group.editors.map((e) => e.resource.key)).toEqual(["file:a.txt", "file:figure.png", "file:b.typ"]);
    expect(group.activeKey).toBe("file:figure.png");
    const renamed = group.editors[1]!;
    expect([renamed.resource.path, renamed.resource.title, renamed.dirty]).toEqual(["figure.png", "figure.png", true]);
    await editors.close("file:figure.png");
    expect(guarded).toBe(1);
  });

  it("selects the stem of files and the whole name of folders", () => {
    expect(stemLength("Untitled.txt", "file")).toBe(8);
    expect(stemLength(".gitignore", "file")).toBe(10);
    expect(stemLength("Makefile", "file")).toBe(8);
    expect(stemLength("archive.tar.gz", "file")).toBe(11);
    expect(stemLength("v1.2", "directory")).toBe(4);
  });
});

describe("OS drop import", () => {
  it("highlights the folder under the pointer and imports a drop there; a drop elsewhere does nothing", async () => {
    fileIpc.importStart.mockResolvedValue("import-1");
    render(
      <>
        <div data-testid="elsewhere">editor</div>
        <FilesView />
      </>,
    );
    const at = (element: Element | null) => vi.spyOn(document, "elementFromPoint").mockReturnValue(element);
    // Over the folder row (a child element of it).
    at(title("chapters"));
    act(() => handleNativeDrop({ type: "enter", paths: ["D:\\in\\a.png"], position: { x: 20, y: 30 } }, 1));
    expect(useDropTarget.getState().target).toEqual({ folder: "chapters" });
    await waitFor(() => expect(row("chapters").hasAttribute("data-drop-target")).toBe(true));
    // Over a file: its folder (the project folder) — the whole tree is highlighted.
    at(title("main.typ"));
    act(() => handleNativeDrop({ type: "over", position: { x: 20, y: 60 } }, 1));
    expect(useDropTarget.getState().target).toEqual({ folder: null });
    await waitFor(() => expect(screen.getByTestId("file-tree").hasAttribute("data-drop-target")).toBe(true));
    expect(row("chapters").hasAttribute("data-drop-target")).toBe(false);
    // Dropped on the folder: copies go there, bound to the open project.
    at(title("chapters"));
    act(() => handleNativeDrop({ type: "drop", paths: ["D:\\in\\a.png", "D:\\in\\b.csv"], position: { x: 40, y: 60 } }, 2));
    await waitFor(() =>
      expect(fileIpc.importStart).toHaveBeenCalledWith({ root: "C:\\proj", folder: "chapters", sources: ["D:\\in\\a.png", "D:\\in\\b.csv"], retryOf: null }),
    );
    expect(useDropTarget.getState().target).toBeUndefined();
    // Outside the Files view: nothing is imported.
    fileIpc.importStart.mockClear();
    at(screen.getByTestId("elsewhere"));
    expect(destinationAt(1, 1)).toBeUndefined();
    act(() => handleNativeDrop({ type: "drop", paths: ["D:\\in\\c.txt"], position: { x: 1, y: 1 } }, 1));
    await new Promise((r) => setTimeout(r, 10));
    expect(fileIpc.importStart).not.toHaveBeenCalled();
    vi.restoreAllMocks();
  });

  it("summarizes partial results truthfully and offers Retry only for what was not imported", () => {
    const progress: FileImportProgress = {
      import: "import-1",
      root: "C:\\proj",
      folder: "figures",
      total: 4,
      done: 4,
      finished: true,
      canceled: false,
      items: [
        { source: "D:\\in\\a.png", status: "succeeded", created: "figures/a.png", code: null, message: null },
        { source: "D:\\in\\b.png", status: "succeeded", created: "figures/b (2).png", code: null, message: null },
        { source: "D:\\in\\link.lnk", status: "skipped", created: null, code: "ops.linkNotCopied", message: "D:\\in\\link.lnk is a link; links are not copied." },
        { source: "D:\\in\\locked.csv", status: "failed", created: null, code: "ops.permissionDenied", message: "locked.csv could not be copied: access denied." },
      ],
    };
    const summary = summarizeImport(progress);
    expect(summary.tone).toBe("warning");
    expect(summary.title).toBe("Imported 2 of 4 items into figures/");
    expect(summary.lines).toContain("“b.png” as “b (2).png” (the name was taken)");
    // The link stays a link: only the failure is offered again.
    expect(summary.retry).toEqual(["D:\\in\\locked.csv"]);
    const canceled = summarizeImport({ ...progress, canceled: true, items: progress.items.slice(0, 1).concat([{ source: "D:\\in\\z", status: "skipped", created: null, code: "import.canceled", message: "Not imported: the import was canceled." }]) });
    expect(canceled.title).toBe("Import canceled: 1 of 4 imported into figures/");
    const retried = summarizeImport({ ...progress, total: 1, items: [{ source: "D:\\in\\a.png", status: "skipped", created: "figures/a.png", code: "import.alreadyImported", message: "Already imported as figures/a.png." }] });
    expect([retried.tone, retried.retry]).toEqual(["info", []]);
  });

  it("New File from another surface is shown in the Files view", () => {
    act(() => openNewFileDialog("chapters"));
    expect(useFileDialog.getState().request).toEqual({ mode: "new", surface: "files", folder: "chapters" });
  });
});
