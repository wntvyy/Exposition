// Workspace state: the open folder, lazily listed directories, recent
// projects, and recovery entries. Actions call the Rust workspace service.
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { create } from "zustand";

import { editors } from "../../app/editors";
import { layout } from "../../app/layout";
import { notify } from "../../app/notifications";
import type { FileChange } from "../../bindings/FileChange";
import type { FileEntry } from "../../bindings/FileEntry";
import type { RecentProject } from "../../bindings/RecentProject";
import type { RecoveryEntry } from "../../bindings/RecoveryEntry";
import type { WorkspaceEvent } from "../../bindings/WorkspaceEvent";
import type { WorkspaceInfo } from "../../bindings/WorkspaceInfo";
import { commands, errorMessage, events } from "../../lib/ipc";
import { disposeAllHandles, fileKey } from "../editor/documentClient";

/** Key of the root directory in `dirs`. */
export const ROOT = "";

export interface DirState {
  entries: FileEntry[] | null;
  loading: boolean;
  error: string | null;
}

export interface WorkspaceStore {
  info: WorkspaceInfo | null;
  recent: RecentProject[];
  dirs: Record<string, DirState>;
  expanded: Set<string>;
  recovery: RecoveryEntry[];
  opening: boolean;
  error: string | null;
}

export const useWorkspace = create<WorkspaceStore>(() => ({
  info: null,
  recent: [],
  dirs: {},
  expanded: new Set(),
  recovery: [],
  opening: false,
  error: null,
}));

const setDir = (path: string, patch: Partial<DirState>) =>
  useWorkspace.setState((s) => ({
    dirs: { ...s.dirs, [path]: { entries: null, loading: false, error: null, ...s.dirs[path], ...patch } },
  }));

export async function loadDir(path: string): Promise<void> {
  setDir(path, { loading: true });
  try {
    const entries = await commands.workspaceListDir(path === ROOT ? null : path);
    setDir(path, { entries, loading: false, error: null });
  } catch (error) {
    setDir(path, { loading: false, error: errorMessage(error) });
  }
}

export function setExpanded(keys: Set<string>) {
  const before = useWorkspace.getState().expanded;
  useWorkspace.setState({ expanded: keys });
  for (const key of keys) {
    if (!before.has(key) && !useWorkspace.getState().dirs[key]?.entries) void loadDir(key);
  }
}

export async function refreshRecent(): Promise<void> {
  try {
    useWorkspace.setState({ recent: await commands.workspaceRecent() });
  } catch {
    // Recent projects are optional.
  }
}

/** Close every open workspace-file editor, honoring unsaved-changes prompts. */
async function closeFileEditors(): Promise<boolean> {
  for (const editor of editors.all()) {
    if (editor.resource.key.startsWith("file:") && !(await editors.close(editor.resource.key))) return false;
  }
  return true;
}

export const FILES_VIEW_ID = "workspace.files";

function setWindowTitle(name: string | null) {
  try {
    void getCurrentWindow()
      .setTitle(name ? `${name} — Exposition` : "Exposition")
      .catch(() => {});
  } catch {
    // Not running inside the desktop shell.
  }
}

function applyInfo(info: WorkspaceInfo) {
  useWorkspace.setState({
    info,
    dirs: {},
    expanded: new Set(),
    recovery: info.recovery,
    opening: false,
    error: null,
  });
  void loadDir(ROOT);
  void refreshRecent();
  setWindowTitle(info.name);
  if (info.recovery.length > 0) {
    layout.showView("sidebar", FILES_VIEW_ID);
    notify({
      id: "recovery-available",
      tone: "warning",
      title: "Unsaved changes were recovered",
      body: `${info.recovery.length} file${info.recovery.length === 1 ? " has" : "s have"} unsaved changes from a previous session. Restore or discard them in the Files view.`,
      timeoutMs: null,
    });
  }
}

/** Open a folder by absolute path. */
export async function openWorkspace(path: string): Promise<boolean> {
  const current = useWorkspace.getState().info;
  if (current && current.root !== path && !(await closeFileEditors())) return false;
  useWorkspace.setState({ opening: true, error: null });
  try {
    applyInfo(await commands.workspaceOpen(path));
    return true;
  } catch (error) {
    const message = errorMessage(error);
    useWorkspace.setState({ opening: false, error: message });
    notify({ tone: "danger", title: "Could not open folder", body: message });
    return false;
  }
}

/** Show the folder picker and open the chosen folder. */
export async function openFolderDialog(): Promise<void> {
  const chosen = await openDialog({ directory: true, multiple: false, title: "Open Folder" });
  if (typeof chosen === "string") await openWorkspace(chosen);
}

export async function closeWorkspace(): Promise<void> {
  if (!(await closeFileEditors())) return;
  await commands.workspaceClose();
  useWorkspace.setState({ info: null, dirs: {}, expanded: new Set(), recovery: [] });
  setWindowTitle(null);
  void refreshRecent();
}

export function openFile(entry: Pick<FileEntry, "path" | "name">, options: { preview?: boolean } = {}) {
  editors.open({ key: fileKey(entry.path), path: entry.path, title: entry.name }, { preview: options.preview });
}

// ----- recovery -------------------------------------------------------------------

export async function restoreRecovery(entry: RecoveryEntry): Promise<void> {
  try {
    const outcome = await commands.recoveryRestore(entry.id);
    useWorkspace.setState((s) => ({ recovery: s.recovery.filter((e) => e.id !== entry.id) }));
    const name = entry.path.split("/").pop() ?? entry.path;
    openFile({ path: entry.path, name });
    notify({
      tone: outcome.diskChanged ? "warning" : "success",
      title: `Restored unsaved changes to ${name}`,
      body: outcome.diskChanged
        ? "The file changed on disk after these edits were made. The editor shows your recovered text; the file on disk is unchanged. Undo returns to the disk version."
        : "Undo returns to the version on disk.",
      timeoutMs: outcome.diskChanged ? null : undefined,
    });
  } catch (error) {
    notify({ tone: "danger", title: "Could not restore unsaved changes", body: errorMessage(error) });
  }
}

export async function discardRecovery(entry: RecoveryEntry): Promise<void> {
  try {
    useWorkspace.setState({ recovery: await commands.recoveryDiscard(entry.id) });
  } catch (error) {
    notify({ tone: "danger", title: "Could not discard recovered changes", body: errorMessage(error) });
  }
}

// ----- file-system events ---------------------------------------------------------

const parentOf = (path: string) => (path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : ROOT);

function onFilesChanged(changes: FileChange[], rescan: boolean) {
  const { dirs } = useWorkspace.getState();
  const stale = new Set<string>();
  if (rescan) {
    for (const key of Object.keys(dirs)) stale.add(key);
  } else {
    for (const change of changes) {
      const parent = parentOf(change.path);
      if (dirs[parent]) stale.add(parent);
      if (change.kind === "removed" && dirs[change.path]) {
        // A removed folder: drop its listing.
        useWorkspace.setState((s) => {
          const next = { ...s.dirs };
          delete next[change.path];
          return { dirs: next };
        });
      }
    }
  }
  for (const key of stale) void loadDir(key);
}

/** Remove every workspace-file tab without prompting (their documents are already closed). */
function dropFileTabs() {
  disposeAllHandles();
  for (const editor of editors.all()) {
    if (editor.resource.key.startsWith("file:")) void editors.close(editor.resource.key);
  }
}

let started = false;

export function startWorkspaceEvents() {
  if (started) return;
  started = true;
  void events.workspace((event: WorkspaceEvent) => {
    switch (event.kind) {
      case "opened":
        if (useWorkspace.getState().info?.root !== event.info.root) applyInfo(event.info);
        break;
      case "closed":
        // The service closed the folder's documents (unsaved ones keep their
        // recovery journals); tabs of its files no longer refer to anything.
        dropFileTabs();
        useWorkspace.setState({ info: null, dirs: {}, expanded: new Set(), recovery: [] });
        setWindowTitle(null);
        break;
      case "filesChanged":
        onFilesChanged(event.changes, event.rescan);
        break;
      case "recoveryChanged":
        useWorkspace.setState({ recovery: event.entries });
        break;
      default:
        break;
    }
  });
  // Adopt a workspace that is already open (e.g. after a webview reload).
  void commands.workspaceCurrent().then((info) => {
    if (info && !useWorkspace.getState().info) applyInfo(info);
  });
  void refreshRecent();
}
