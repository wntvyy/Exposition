// File operations for the explorers (P1): Copy, Paste, Duplicate, Delete and
// Delete Permanently over selected project paths, through the shared
// operation client. The destination of a paste is the selected folder, or the
// selected file's parent (the menu says which); nothing selected = the
// project folder.
import { notify } from "../../app/notifications";
import type { OpTarget } from "../../bindings/OpTarget";
import type { WorkspacePath } from "../../bindings/WorkspacePath";
import { runUserOperation, type OperationOutcome } from "../operations/client";
import { copyFiles, filesToPaste } from "../operations/clipboard";
import { baseName, folderLabel, parentFolder, targetKey, trashName } from "../operations/model";
import type { SurfaceActions } from "../operations/surfaces";

export interface FileSelectionItem {
  path: WorkspacePath;
  kind: "file" | "directory";
  name: string;
}

/** Where Paste puts copies for this selection (`undefined`: ambiguous). */
export function pasteFolder(selection: FileSelectionItem[]): string | null | undefined {
  if (selection.length === 0) return null;
  if (selection.length > 1) return undefined;
  const only = selection[0]!;
  return only.kind === "directory" ? only.path : parentFolder(only.path);
}

export function labelsFor(items: { path: string; name: string }[]): Map<string, string> {
  return new Map(items.map((i) => [targetKey({ kind: "path", path: i.path }), i.name]));
}

function pathTargets(items: FileSelectionItem[]): OpTarget[] {
  return items.map((i) => ({ kind: "path", path: i.path }));
}

/** Created paths of a successful copy (for selecting them afterwards). */
export function createdPaths(outcome: OperationOutcome | null): string[] {
  return (outcome?.result.outcomes ?? []).flatMap((o) => (o.status === "succeeded" && o.created?.kind === "path" ? [o.created.path] : []));
}

export async function copySelection(items: FileSelectionItem[]): Promise<void> {
  const { onOsClipboard, error } = await copyFiles(items.map((i) => i.path));
  const what = items.length === 1 ? `“${items[0]!.name}”` : `${items.length} items`;
  notify(
    onOsClipboard
      ? { tone: "info", title: `Copied ${what}`, body: "Paste into a folder here or in another application." }
      : { tone: "warning", title: `Copied ${what} for pasting in Exposition only`, body: `The system clipboard is unavailable${error ? `: ${error}` : "."}` },
  );
}

/** Paste the clipboard's files into `folder` (`null` = project folder). */
export async function pasteInto(folder: string | null, root: string | null): Promise<OperationOutcome | null> {
  const paste = await filesToPaste(root);
  if (paste.kind === "none") {
    notify({ tone: "info", title: "Nothing to paste", body: paste.reason });
    return null;
  }
  const labels = new Map(paste.targets.map((t) => [targetKey(t), t.kind === "path" || t.kind === "external" ? baseName(t.path) : targetKey(t)]));
  return runUserOperation({ kind: "paste", destination: { kind: "folder", folder } }, paste.targets, { labels });
}

/**
 * The operation actions for a file selection (Files explorer, Research
 * explorer files). `afterCopy` receives created paths (to select them).
 */
export function fileActions(
  items: FileSelectionItem[],
  options: { root: string | null; afterCreate?: (paths: string[], folder: string | null) => void; canPaste?: boolean },
): SurfaceActions {
  const has = items.length > 0;
  const none = "Select a file or folder first";
  const folder = pasteFolder(items);
  const labels = labelsFor(items);
  const trash = trashName();
  return {
    copy: { enabled: has, reason: none, run: () => copySelection(items) },
    paste:
      options.canPaste === false
        ? undefined
        : {
            enabled: folder !== undefined,
            reason: "Select one folder (or one file, to paste beside it)",
            label: folder === undefined ? "Paste" : `Paste into ${folderLabel(folder)}`,
            run: async () => {
              if (folder === undefined) return;
              const outcome = await pasteInto(folder, options.root);
              const created = createdPaths(outcome);
              if (created.length) options.afterCreate?.(created, folder);
            },
          },
    duplicate: {
      enabled: has,
      reason: none,
      description: has ? "Copies beside the original with a new name" : undefined,
      run: async () => {
        const outcome = await runUserOperation({ kind: "duplicate" }, pathTargets(items), { labels });
        const created = createdPaths(outcome);
        if (created.length) options.afterCreate?.(created, parentFolder(created[0]!));
      },
    },
    delete: {
      enabled: has,
      reason: none,
      description: `Moves to ${trash}`,
      run: async () => {
        await runUserOperation({ kind: "delete", permanent: false }, pathTargets(items), { labels });
      },
    },
    deletePermanently: {
      enabled: has,
      reason: none,
      description: "Cannot be restored",
      run: async () => {
        await runUserOperation({ kind: "delete", permanent: true }, pathTargets(items), { labels });
      },
    },
  };
}
