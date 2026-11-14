// Typed IPC for project operations (P1). Rust owns every decision about
// filesystem authority, impacts, recovery and identities; these wrappers only
// name the commands, their arguments and the `project-op` event. Never bypass
// them with raw `invoke`.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { OpApply } from "../../bindings/OpApply";
import type { OpOutcome } from "../../bindings/OpOutcome";
import type { OpPlan } from "../../bindings/OpPlan";
import type { OpRequest } from "../../bindings/OpRequest";
import type { OpRestore } from "../../bindings/OpRestore";
import type { OpResult } from "../../bindings/OpResult";
import type { StorageOverview } from "../../bindings/StorageOverview";
import type { WorkspacePath } from "../../bindings/WorkspacePath";

export const opCommands = {
  /** Normalize targets, refuse invalid ones, list concrete impacts. Nothing changes. */
  plan: (request: OpRequest) => invoke<OpPlan>("project_op_plan", { request }),
  /** Apply a plan (re-checked against its hash: `ops.planStale` when it changed). */
  apply: (apply: OpApply) => invoke<OpResult>("project_op_apply", { apply }),
  /** Restore an app-known deletion; never overwrites the original location. */
  restore: (restore: OpRestore) => invoke<OpOutcome>("project_op_restore", { restore }),
  /** Absolute paths of the OS clipboard's file list (empty when it holds none). */
  clipboardFilesRead: () => invoke<string[]>("clipboard_files_read"),
  /** Put project files on the OS clipboard as a file list. */
  clipboardFilesWrite: (paths: WorkspacePath[]) => invoke<void>("clipboard_files_write", { paths }),
  /** Settings → Storage & History. */
  storageOverview: () => invoke<StorageOverview>("storage_overview"),
};

export const opEvents = {
  /** After every apply/restore (any surface, agents included): views re-read what they show. */
  projectOp: (handler: (result: OpResult) => void): Promise<UnlistenFn> =>
    listen<OpResult>("project-op", (e) => handler(e.payload)),
};
