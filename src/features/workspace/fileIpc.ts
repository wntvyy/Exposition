// Typed IPC for the explorer's file actions (QOL E/G): New File, Rename, and
// importing files dropped from the operating system. Rust validates names,
// scope and collisions and owns every filesystem decision.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { FileCreated } from "../../bindings/FileCreated";
import type { FileImportProgress } from "../../bindings/FileImportProgress";
import type { FileImportRequest } from "../../bindings/FileImportRequest";
import type { FileRenamed } from "../../bindings/FileRenamed";
import type { RenameCheck } from "../../bindings/RenameCheck";
import type { WorkspacePath } from "../../bindings/WorkspacePath";

export const fileCommands = {
  /** Create an empty file `name` in `folder` (`null`: the project folder); never replaces anything. */
  create: (folder: WorkspacePath | null, name: string) => invoke<FileCreated>("file_create", { folder, name }),
  /** What still uses `path` by its current name (nothing changes). */
  renameCheck: (path: WorkspacePath) => invoke<RenameCheck>("file_rename_check", { path }),
  /** Rename within its folder; open documents follow; the graph keeps the identity. */
  rename: (path: WorkspacePath, name: string) => invoke<FileRenamed>("file_rename", { path, name }),
  /** Start copying dropped files into a folder; progress arrives as `file-import` events. Returns the import id. */
  importStart: (request: FileImportRequest) => invoke<string>("file_import_start", { request }),
  /** Stop an import after its current step (resolves false when it already ended). */
  importCancel: (importId: string) => invoke<boolean>("file_import_cancel", { import: importId }),
};

export const fileEvents = {
  import: (handler: (progress: FileImportProgress) => void): Promise<UnlistenFn> =>
    listen<FileImportProgress>("file-import", (e) => handler(e.payload)),
};
