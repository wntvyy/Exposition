// Project switch: open or create another project while one is open, in the
// same window. The order keeps the current project intact until the switch
// is committed:
//   1. the destination is resolved by the caller (folder dialog, New Project
//      form); canceling there never reaches this module;
//   2. unsaved work (workspace documents, including ones open only in
//      Research, and other editors such as workflow drafts) and running work
//      the switch stops (workflow runs, research code) are gathered; only when
//      there is some, ONE decision asks: Save All / Don't Save / Cancel (or
//      Stop and Open / Create when only running work is affected);
//   3. Save All saves; if anything stays unsaved the switch stops and reports
//      what was and was not saved (no rollback of what was saved);
//   4. the commit (`workspace_open` / `project_create`) checks the destination
//      before anything of the current project is closed; the documents chosen
//      for Don't Save are discarded by the backend only once the destination
//      opened. A refused destination leaves every buffer and recovery journal.
//   5. after the switch, discarded editors that are not documents close.
// Closing the project's documents, canceling its workflow runs, stopping its
// research kernels, and detaching its graph are the backend's existing close
// behavior (the same as File: Close Folder).
import type { DocumentId } from "../../bindings/DocumentId";

/** Unsaved work that a switch would close. */
export interface DirtyItem {
  /** Editor key (`file:<path>` for documents, e.g. `workflow:<id>` for drafts). */
  key: string;
  /** Name shown to the user. */
  name: string;
  /** The service document (workspace files); null for other editors. */
  document: DocumentId | null;
}

/** Running work that a switch stops. */
export interface ActiveWork {
  kind: "workflowRun" | "research";
  /** Name shown to the user (workflow title, research session name). */
  name: string;
}

export type SwitchAction = "open" | "create";

export interface SwitchDecision {
  action: SwitchAction;
  /** Name of the open project. */
  from: string;
  /** Name of the destination (folder name or project title). */
  to: string;
  dirty: DirtyItem[];
  active: ActiveWork[];
}

/** `proceed`: only running work is affected and the user accepted stopping it. */
export type SwitchChoice = "save" | "discard" | "proceed" | "cancel";

export interface SwitchTarget<T> {
  action: SwitchAction;
  /** Destination name for the decision and messages. */
  name: string;
  /** Open or create the destination, discarding these documents on success. Throws when refused. */
  commit: (discard: DocumentId[]) => Promise<T>;
}

export type SwitchResult<T> =
  | { status: "switched"; value: T; discarded: DirtyItem[] }
  | { status: "canceled" }
  /** Save All left work unsaved: the current project stays open. */
  | { status: "notSaved"; saved: DirtyItem[]; unsaved: DirtyItem[] }
  /** The destination could not be opened/created: nothing was discarded or closed. */
  | { status: "failed"; error: unknown };

export interface SwitchDeps {
  /** Name of the open project, or null when none is open. */
  currentName(): string | null;
  dirtyWork(): Promise<DirtyItem[]>;
  activeWork(): Promise<ActiveWork[]>;
  decide(decision: SwitchDecision): Promise<SwitchChoice>;
  /** Save every unsaved workspace document; false when any failed. */
  saveDocuments(): Promise<boolean>;
  /** Save a non-document editor (e.g. a workflow draft); false when it was not saved. */
  saveEditor(key: string): Promise<boolean>;
  /** Close editors whose changes were discarded (no prompt). */
  discardEditors(keys: string[]): Promise<void>;
}

export async function switchProject<T>(target: SwitchTarget<T>, deps: SwitchDeps): Promise<SwitchResult<T>> {
  const from = deps.currentName();
  let discard: DirtyItem[] = [];
  if (from !== null) {
    const [dirty, active] = await Promise.all([deps.dirtyWork(), deps.activeWork()]);
    if (dirty.length > 0 || active.length > 0) {
      const choice = await deps.decide({ action: target.action, from, to: target.name, dirty, active });
      if (choice === "cancel") return { status: "canceled" };
      if (choice === "save") {
        const saved = await saveEverything(dirty, deps);
        if (saved) return saved;
      } else if (choice === "discard") {
        discard = dirty;
      }
    }
  }
  let value: T;
  try {
    value = await target.commit(discard.flatMap((item) => (item.document ? [item.document] : [])));
  } catch (error) {
    return { status: "failed", error };
  }
  const editorKeys = discard.filter((item) => item.document === null).map((item) => item.key);
  if (editorKeys.length > 0) await deps.discardEditors(editorKeys);
  return { status: "switched", value, discarded: discard };
}

/** Save All. Returns the stop result when anything is still unsaved, else null. */
async function saveEverything(dirty: DirtyItem[], deps: SwitchDeps): Promise<SwitchResult<never> | null> {
  if (dirty.some((item) => item.document !== null)) await deps.saveDocuments();
  for (const item of dirty) {
    if (item.document === null) await deps.saveEditor(item.key);
  }
  // What is still unsaved decides, not the save calls' answers: a file
  // changed on disk asks first, and Reload from Disk leaves nothing unsaved.
  const remaining = await deps.dirtyWork();
  if (remaining.length === 0) return null;
  const unsavedKeys = new Set(remaining.map((item) => item.key));
  return {
    status: "notSaved",
    saved: dirty.filter((item) => !unsavedKeys.has(item.key)),
    unsaved: remaining,
  };
}

/** "a, b and c" for messages. */
export function listNames(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** The notification body after Save All left work unsaved. */
export function notSavedMessage(result: { saved: DirtyItem[]; unsaved: DirtyItem[] }, from: string): string {
  const parts = [
    result.saved.length > 0 && `Saved: ${listNames(result.saved.map((i) => i.name))}.`,
    `Not saved: ${listNames(result.unsaved.map((i) => i.name))}.`,
    `${from} is still open.`,
  ].filter(Boolean);
  return parts.join(" ");
}
