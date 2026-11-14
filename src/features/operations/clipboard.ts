// Clipboards for project operations (P1).
//
// - Files and folders go to the OS clipboard as a file list
//   (`clipboard_files_write` / `clipboard_files_read`), so they paste into and
//   from other applications. A file list only identifies inputs: pasting
//   external paths happens only through an explicit Paste, and the backend
//   validates every source and destination.
// - Workflows, workflow-canvas subgraphs and graph items stay in an
//   app-internal clipboard (plus plain text where it is meaningful).
// - Arbitrary clipboard text is never interpreted as a filesystem command.
import type { OpTarget } from "../../bindings/OpTarget";
import type { WorkspacePath } from "../../bindings/WorkspacePath";
import { opCommands } from "./ipc";

export interface FileClip {
  kind: "files";
  paths: WorkspacePath[];
  /** The OS clipboard holds these files too (false: the native bridge was unavailable). */
  onOsClipboard: boolean;
}

export interface WorkflowClip {
  kind: "workflows";
  workflows: { id: string; title: string }[];
}

export interface GraphClip {
  kind: "graph";
  entities: { id: string; label: string }[];
}

export interface SubgraphClip<T = unknown> {
  kind: "workflowSubgraph";
  /** Workflow the fragment was copied from. */
  workflow: string;
  fragment: T;
  /** Count of copied steps (for wording). */
  steps: number;
}

export type AppClip = FileClip | WorkflowClip | GraphClip | SubgraphClip;

let current: AppClip | null = null;
const listeners = new Set<() => void>();

export const appClipboard = {
  get: (): AppClip | null => current,
  set(clip: AppClip | null): void {
    current = clip;
    for (const listener of [...listeners]) listener();
  },
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
};

/** Best-effort plain text on the system clipboard (never for files: it would replace the file list). */
export async function writeText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Copy project files/folders: the OS clipboard file list plus the app record. */
export async function copyFiles(paths: WorkspacePath[]): Promise<{ onOsClipboard: boolean; error: string | null }> {
  let onOsClipboard = false;
  let error: string | null = null;
  try {
    await opCommands.clipboardFilesWrite(paths);
    onOsClipboard = true;
  } catch (e) {
    error = typeof e === "object" && e !== null && "message" in e ? String((e as { message: unknown }).message) : String(e);
  }
  appClipboard.set({ kind: "files", paths: [...paths], onOsClipboard });
  return { onOsClipboard, error };
}

function normalize(path: string): string {
  return path.replace(/\\/g, "/").replace(/\/+$/, "");
}

/**
 * Classify absolute OS paths: inside the open project → a project `path`
 * target (workspace-relative), otherwise an `external` paste source. This is
 * only a classification; the backend validates both kinds.
 */
export function classifyAbsolute(paths: string[], root: string | null, caseInsensitive = true): OpTarget[] {
  const base = root ? normalize(root) : null;
  const fold = (s: string) => (caseInsensitive ? s.toLowerCase() : s);
  return paths.map((raw) => {
    const abs = normalize(raw);
    if (base && fold(abs).startsWith(`${fold(base)}/`)) {
      return { kind: "path", path: abs.slice(base.length + 1) } satisfies OpTarget;
    }
    return { kind: "external", path: raw } satisfies OpTarget;
  });
}

export type FilePaste =
  | { kind: "files"; targets: OpTarget[]; source: "os" | "app" }
  | { kind: "none"; reason: string };

/**
 * What a Paste into a folder would copy. The OS clipboard's file list wins
 * (it is what the person copied last, possibly in another application); the
 * app record is used only when the native bridge is unavailable.
 */
export async function filesToPaste(root: string | null): Promise<FilePaste> {
  const clip = appClipboard.get();
  let osPaths: string[] | null = null;
  try {
    osPaths = await opCommands.clipboardFilesRead();
  } catch {
    osPaths = null;
  }
  if (osPaths && osPaths.length > 0) return { kind: "files", targets: classifyAbsolute(osPaths, root), source: "os" };
  if (clip?.kind === "files" && (osPaths === null || !clip.onOsClipboard)) {
    return { kind: "files", targets: clip.paths.map((path) => ({ kind: "path", path })), source: "app" };
  }
  return { kind: "none", reason: osPaths === null ? "The clipboard cannot be read here." : "The clipboard holds no files." };
}
