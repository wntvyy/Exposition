// OS drag-and-drop into the Files tree (QOL G). Files dragged from the
// operating system arrive through the desktop shell's native drop events
// (exact absolute paths; the webview's HTML5 drop has no paths). While a drag
// is over the Files view, the destination folder is highlighted: the folder
// row under the pointer, a file row's folder, or the whole tree for the
// project folder. A drop there copies the files into that folder through the
// backend import (originals untouched). Drops anywhere else do nothing here.
import { create } from "zustand";

import { parentFolder } from "../operations/model";
import { startImport } from "./explorerActions";
import { useWorkspace } from "./store";

export interface DropDestination {
  /** Destination folder (`null`: the project folder). */
  folder: string | null;
}

/** The highlighted destination while a drag is over the Files view (`undefined`: none). */
export const useDropTarget = create<{ target: DropDestination | undefined }>(() => ({ target: undefined }));

/** The destination under a point (CSS pixels), if it is inside the Files tree. */
export function destinationAt(x: number, y: number, doc: Document = document): DropDestination | undefined {
  const element = doc.elementFromPoint(x, y);
  if (!element || !useWorkspace.getState().info) return undefined;
  const view = element.closest("[data-testid='files-view']");
  if (!view) return undefined;
  const surface = element.closest("[data-testid='files-surface']");
  const row = surface ? element.closest("[data-path][data-kind]") : null;
  if (row && surface?.contains(row)) {
    const path = row.getAttribute("data-path")!;
    // An unavailable entry (e.g. a link leading outside the folder) takes nothing.
    if (row.hasAttribute("data-disabled")) return undefined;
    return { folder: row.getAttribute("data-kind") === "directory" ? path : parentFolder(path) };
  }
  // The tree's empty space or the header with the project name: the project folder.
  if (surface || element.closest(".xp-files__header")) return { folder: null };
  return undefined;
}

function same(a: DropDestination | undefined, b: DropDestination | undefined): boolean {
  return a?.folder === b?.folder && (a === undefined) === (b === undefined);
}

function setTarget(target: DropDestination | undefined): void {
  if (!same(useDropTarget.getState().target, target)) useDropTarget.setState({ target });
}

interface NativeDrop {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
  position?: { x: number; y: number };
}

/** Handle one native drag-and-drop event (exported for tests). */
export function handleNativeDrop(event: NativeDrop, scale: number = globalThis.devicePixelRatio || 1): void {
  if (event.type === "leave" || !event.position) {
    setTarget(undefined);
    return;
  }
  const target = destinationAt(event.position.x / scale, event.position.y / scale);
  if (event.type !== "drop") {
    setTarget(target);
    return;
  }
  setTarget(undefined);
  const paths = event.paths ?? [];
  if (target && paths.length) void startImport(paths, target.folder);
}

let started = false;

/** Listen to the desktop shell's drag-and-drop events (once). */
export function startDropTarget(): void {
  if (started) return;
  started = true;
  void import("@tauri-apps/api/webview")
    .then(({ getCurrentWebview }) => getCurrentWebview().onDragDropEvent((event) => handleNativeDrop(event.payload as NativeDrop)))
    .catch(() => {
      // Not running inside the desktop shell (unit tests, browser preview).
    });
}
