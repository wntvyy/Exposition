// Explorer file actions (QOL E/G): New File and Rename (one name dialog,
// backend-validated), and importing files dropped from the operating system.
//
// - New File creates in the selected folder (or the selected file's folder;
//   the project folder otherwise) with the editable suggestion `Untitled.txt`
//   whose stem is selected: typing replaces the stem, select-all replaces the
//   whole name. The typed name is used as is (no extension is added). The new
//   file opens and takes focus in the editor.
// - Rename (F2 on a focused explorer item, the context menu, or the palette)
//   renames within the folder. Open documents keep their buffer, undo,
//   recovery and tab (re-keyed to the new path); the graph keeps the
//   resource's identity. Uses of the old name are listed in the dialog and
//   after the rename; they are never rewritten.
// - A failed create/rename keeps the dialog open with the typed name and the
//   backend's reason; nothing on disk changed.
import { useEffect, useRef, useState } from "react";
import { create } from "zustand";

import { editors } from "../../app/editors";
import { layout } from "../../app/layout";
import { notify, dismiss } from "../../app/notifications";
import { EDITOR_PAGE, pages } from "../../app/pages";
import type { FileImportItem } from "../../bindings/FileImportItem";
import type { FileImportProgress } from "../../bindings/FileImportProgress";
import type { FileRenamed } from "../../bindings/FileRenamed";
import type { RenameCheck } from "../../bindings/RenameCheck";
import { errorMessage, isCommandError } from "../../lib/ipc";
import { Banner, Button, Dialog, TextField } from "../../ui/components";
import { activeSurface } from "../operations/surfaces";
import { baseName, folderLabel, parentFolder, quoted } from "../operations/model";
import { fileCommands, fileEvents } from "./fileIpc";
import { FILES_VIEW_ID, ROOT, loadDir, openFile, refreshAfterOperation, setExpanded, useWorkspace } from "./store";

export const DEFAULT_NEW_FILE_NAME = "Untitled.txt";

export interface RenameTarget {
  path: string;
  name: string;
  kind: "file" | "directory";
}

/** Where the dialog is shown: the explorer that asked (each renders its own host). */
export type DialogSurface = "files" | "research";

type DialogRequest =
  | { mode: "new"; surface: DialogSurface; folder: string | null }
  | { mode: "rename"; surface: DialogSurface; target: RenameTarget };

export const useFileDialog = create<{ request: DialogRequest | null }>(() => ({ request: null }));

/** Ask an explorer tree to select (and scroll to) a path once its listing shows it. */
export const useTreeReveal = create<{ paths: string[]; nonce: number }>(() => ({ paths: [], nonce: 0 }));

export function revealInTree(paths: string[]): void {
  useTreeReveal.setState((s) => ({ paths, nonce: s.nonce + 1 }));
}

// ----- selection providers ------------------------------------------------------------

/** The Files tree's current selection (kept by the tree, read by commands). */
let filesSelection: RenameTarget[] = [];

export function setFilesSelection(items: RenameTarget[]): void {
  filesSelection = items;
}

/** Folder for New File: the selected folder, the selected file's folder, else the project folder. */
export function newFileFolder(selection: RenameTarget[] = filesSelection): string | null {
  if (selection.length !== 1) return null;
  const only = selection[0]!;
  return only.kind === "directory" ? only.path : parentFolder(only.path);
}

const renameProviders = new Map<string, () => RenameTarget[]>([["files", () => filesSelection]]);

/** An explorer surface (by its operation-surface id) offers its selection for Rename. */
export function registerRenameProvider(surface: string, selection: () => RenameTarget[]): () => void {
  renameProviders.set(surface, selection);
  return () => {
    if (renameProviders.get(surface) === selection) renameProviders.delete(surface);
  };
}

/** What Rename (F2 / palette) acts on now: the one item selected in the focused explorer. */
export function activeRenameTarget(): { surface: DialogSurface; target: RenameTarget } | null {
  const surface = activeSurface();
  if (!surface || (surface.id !== "files" && surface.id !== "research")) return null;
  const selection = renameProviders.get(surface.id)?.() ?? [];
  return selection.length === 1 ? { surface: surface.id, target: selection[0]! } : null;
}

// ----- opening the dialog --------------------------------------------------------------

export function openNewFileDialog(folder: string | null = newFileFolder(), surface: DialogSurface = "files"): void {
  if (!useWorkspace.getState().info) return;
  if (surface === "files") {
    // The Files view hosts the dialog: show it (from any page or a collapsed sidebar).
    pages.show(EDITOR_PAGE);
    layout.showView("sidebar", FILES_VIEW_ID);
  }
  useFileDialog.setState({ request: { mode: "new", surface, folder } });
}

export function openRenameDialog(target: RenameTarget, surface: DialogSurface = "files"): void {
  useFileDialog.setState({ request: { mode: "rename", surface, target } });
}

// ----- create / rename ----------------------------------------------------------------

async function createFile(folder: string | null, name: string): Promise<void> {
  const { path } = await fileCommands.create(folder, name);
  if (folder) setExpanded(new Set([...useWorkspace.getState().expanded, folder]));
  void loadDir(folder ?? ROOT);
  revealInTree([path]);
  // Opened after the dialog has returned focus, so the editor keeps it.
  setTimeout(() => openFile({ path, name: baseName(path) }), 0);
}

/** Editor resource key of a workspace file (as `documentClient.fileKey`, without loading the editor). */
const fileKey = (path: string) => `file:${path}`;

/** Tabs of files at or under `from` that have no open document (e.g. images): re-keyed to `to`. */
function rekeyOtherTabs(from: string, to: string): void {
  for (const editor of editors.all()) {
    const path = editor.resource.path;
    if (!editor.resource.key.startsWith("file:") || !path) continue;
    if (path !== from && !path.startsWith(`${from}/`)) continue;
    const next = `${to}${path.slice(from.length)}`;
    editors.rekey(editor.resource.key, { key: fileKey(next), path: next, title: baseName(next) });
  }
}

export async function applyRenamed(result: FileRenamed): Promise<void> {
  // Documents first (their handles re-key their own tabs), then any other tab.
  if (result.documents.length) {
    // Loaded on use: the editor brings Monaco with it.
    const { handleFor } = await import("../editor/documentClient");
    for (const info of result.documents) handleFor(info.id)?.setInfo(info);
  }
  rekeyOtherTabs(result.from, result.to);
  refreshAfterOperation([result.from, result.to]);
  revealInTree([result.to]);
}

function reportRenamed(result: FileRenamed): void {
  const title = `Renamed ${quoted(baseName(result.from))} to ${quoted(baseName(result.to))}`;
  const lines: string[] = [];
  if (result.references.length) {
    lines.push("Not changed — still uses the old name:");
    for (const r of result.references) lines.push(`${r.label}: ${r.message}`);
  }
  if (result.graph.kind === "pending") {
    lines.push(`The project graph has not recorded the rename yet (${result.graph.message}); it is reconciled from the files automatically.`);
  }
  const attention = lines.length > 0;
  notify({
    id: `rename-${result.to}`,
    tone: attention ? "warning" : "info",
    title,
    body: attention ? lines.join("\n") : undefined,
    timeoutMs: attention ? null : undefined,
  });
}

async function renameEntry(target: RenameTarget, name: string): Promise<void> {
  const result = await fileCommands.rename(target.path, name);
  await applyRenamed(result);
  reportRenamed(result);
}

// ----- the dialog ---------------------------------------------------------------------

/** Length of the part of a name selected first: the stem of a file, a folder's whole name. */
export function stemLength(name: string, kind: "file" | "directory"): number {
  if (kind === "directory") return name.length;
  const dot = name.lastIndexOf(".");
  return dot > 0 ? dot : name.length;
}

/** The name dialog of one explorer (`surface`): renders only that explorer's requests. */
export function FileNameDialogHost({ surface }: { surface: DialogSurface }) {
  const request = useFileDialog((s) => s.request);
  if (!request || request.surface !== surface) return null;
  const key = request.mode === "new" ? `new:${request.folder ?? ""}` : `rename:${request.target.path}`;
  return <FileNameDialog key={key} request={request} onClose={() => useFileDialog.setState({ request: null })} />;
}

function FileNameDialog({ request, onClose }: { request: DialogRequest; onClose: () => void }) {
  const isNew = request.mode === "new";
  const kind = isNew ? "file" : request.target.kind;
  const initial = isNew ? DEFAULT_NEW_FILE_NAME : request.target.name;
  const [name, setName] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [check, setCheck] = useState<RenameCheck | null>(null);
  const field = useRef<HTMLDivElement>(null);

  // Focus the name with its stem selected (after the dialog's own initial focus).
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      const input = field.current?.querySelector("input");
      if (!input) return;
      input.focus();
      input.setSelectionRange(0, stemLength(initial, kind));
    });
    return () => cancelAnimationFrame(frame);
  }, [initial, kind]);

  // What still uses the current name (shown before renaming; nothing changes).
  useEffect(() => {
    if (request.mode !== "rename") return;
    let cancelled = false;
    fileCommands.renameCheck(request.target.path).then(
      (c) => !cancelled && setCheck(c),
      () => {},
    );
    return () => {
      cancelled = true;
    };
  }, [request]);

  const unchanged = !isNew && name === initial;
  const submit = async () => {
    if (busy || !name.trim()) return;
    if (unchanged) {
      onClose();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (request.mode === "new") await createFile(request.folder, name);
      else await renameEntry(request.target, name);
      onClose();
    } catch (e) {
      // Nothing changed on disk; the typed name stays for correction.
      setError(isCommandError(e) ? e.message : errorMessage(e));
      setBusy(false);
    }
  };

  const where = isNew ? folderLabel(request.folder) : folderLabel(parentFolder(request.target.path));
  const title = isNew ? "New File" : `Rename ${quoted(request.target.name)}`;
  return (
    <Dialog
      title={title}
      size="sm"
      body="content"
      isOpen
      onOpenChange={(open) => !open && onClose()}
      footer={
        <>
          <Button variant="secondary" onPress={onClose} data-testid="file-name-cancel">
            Cancel
          </Button>
          <Button variant="primary" isPending={busy} isDisabled={!name.trim()} onPress={() => void submit()} data-testid="file-name-submit">
            {isNew ? "Create" : "Rename"}
          </Button>
        </>
      }
    >
      <div ref={field} data-testid="file-name-dialog" data-mode={request.mode}>
        <TextField
          label={isNew ? "File name" : "New name"}
          code
          value={name}
          onChange={(value) => {
            setName(value);
            setError(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              void submit();
            }
          }}
          isInvalid={error !== null}
          errorMessage={error ?? undefined}
          description={isNew ? `In ${where}. Type any name and extension.` : `In ${where}.`}
        />
      </div>
      {check && check.references.length > 0 && (
        <div data-testid="rename-references">
          <Banner tone="warning" title="Still uses the current name — not changed by the rename">
            <ul className="xp-rename-references">
              {check.references.map((r, i) => (
                <li key={`${r.kind}-${i}`} data-kind={r.kind}>
                  {r.label}: {r.message}
                </li>
              ))}
            </ul>
          </Banner>
        </div>
      )}
      {check?.unknown && <p className="ui-meta">{check.unknown}</p>}
    </Dialog>
  );
}

// ----- importing dropped files -------------------------------------------------------

/** Imports with more items than this show progress with Cancel. */
const PROGRESS_THRESHOLD = 3;

let listening = false;

function listenImports(): void {
  if (listening) return;
  listening = true;
  try {
    void fileEvents.import(onImportProgress).catch(() => {});
  } catch {
    // Not running inside the desktop shell (unit tests).
  }
}

function itemName(item: FileImportItem): string {
  return baseName(item.source);
}

export function summarizeImport(progress: FileImportProgress): { tone: "info" | "warning" | "danger"; title: string; lines: string[]; retry: string[] } {
  const where = folderLabel(progress.folder);
  const imported = progress.items.filter((i) => i.status === "succeeded");
  const already = progress.items.filter((i) => i.code === "import.alreadyImported");
  const notDone = progress.items.filter((i) => i.status !== "succeeded" && i.code !== "import.alreadyImported");
  const count = (n: number) => (n === 1 ? "1 item" : `${n} items`);
  const done = imported.length + already.length;
  const title = progress.canceled
    ? `Import canceled: ${done} of ${progress.total} imported into ${where}`
    : notDone.length === 0
      ? `Imported ${count(progress.total)} into ${where}`
      : done === 0
        ? `Nothing was imported into ${where}`
        : `Imported ${done} of ${progress.total} items into ${where}`;
  const lines: string[] = [];
  for (const item of imported) {
    const created = item.created ? baseName(item.created) : null;
    if (created && created !== itemName(item)) lines.push(`${quoted(itemName(item))} as ${quoted(created)} (the name was taken)`);
    if (item.message) lines.push(`${quoted(itemName(item))}: ${item.message}`);
  }
  for (const item of already) lines.push(`${quoted(itemName(item))}: ${item.message ?? "already imported"}`);
  for (const item of notDone) lines.push(`${quoted(itemName(item))}: ${item.message ?? item.code ?? "not imported"}`);
  const tone = notDone.length === 0 ? "info" : done === 0 && !progress.canceled ? "danger" : "warning";
  // Retry what may succeed now (failures, canceled or interrupted items), not
  // refusals that stay true (a link, a protected destination).
  const retryable = (i: FileImportItem) => i.status === "failed" || i.code === "ops.planStale" || (i.code?.startsWith("import.") ?? false);
  return { tone, title, lines, retry: notDone.filter(retryable).map((i) => i.source) };
}

function onImportProgress(progress: FileImportProgress): void {
  const id = `import-${progress.import}`;
  if (!progress.finished) {
    if (progress.total > PROGRESS_THRESHOLD) {
      notify({
        id,
        tone: "info",
        title: `Importing ${progress.total} items into ${folderLabel(progress.folder)}`,
        body: `${progress.done} of ${progress.total}`,
        actions: [{ label: "Cancel", run: () => void fileCommands.importCancel(progress.import).catch(() => {}) }],
        timeoutMs: null,
      });
    }
    return;
  }
  dismiss(id);
  const created = progress.items.flatMap((i) => (i.created ? [i.created] : []));
  if (useWorkspace.getState().info?.root === progress.root && created.length) {
    if (progress.folder) setExpanded(new Set([...useWorkspace.getState().expanded, progress.folder]));
    refreshAfterOperation(created);
    revealInTree(created);
  }
  const summary = summarizeImport(progress);
  notify({
    id: `${id}-done`,
    tone: summary.tone,
    title: summary.title,
    body: summary.lines.length ? summary.lines.join("\n") : undefined,
    actions:
      summary.retry.length && useWorkspace.getState().info?.root === progress.root
        ? [
            {
              label: "Retry",
              run: () =>
                void startImport(summary.retry, progress.folder, progress.import),
            },
          ]
        : undefined,
    timeoutMs: summary.tone === "info" ? undefined : null,
  });
}

/** Copy dropped files (absolute paths) into `folder` of the open project. */
export async function startImport(sources: string[], folder: string | null, retryOf: string | null = null): Promise<string | null> {
  const info = useWorkspace.getState().info;
  if (!info || sources.length === 0) return null;
  listenImports();
  try {
    return await fileCommands.importStart({ root: info.root, folder, sources, retryOf });
  } catch (error) {
    notify({ tone: "danger", title: `Could not import into ${folderLabel(folder)}`, body: errorMessage(error) });
    return null;
  }
}

export function startImportEvents(): void {
  listenImports();
}
