// Workflow draft actions shared by the palette commands and views.
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import type { WorkflowPresetRequest } from "../../bindings/WorkflowPresetRequest";
import { errorMessage, isCommandError } from "../../lib/ipc";
import { workflowCommands } from "./ipc";
import { workflowKey } from "./model";

export const WORKFLOWS_KEY = "workflows:";

/** Bind a preset's inputs to the active file when its type fits. */
export function presetRequestFor(preset: string, activePath: string | null): WorkflowPresetRequest {
  const ext = activePath?.split(".").pop()?.toLowerCase() ?? "";
  return {
    preset,
    script: ext === "py" ? activePath : null,
    data: null,
    manuscript: ["typ", "tex", "md", "markdown"].includes(ext) ? activePath : null,
    leanFile: ext === "lean" ? activePath : null,
  };
}

/** Create and save a draft from a workflow preset, then open it. */
export async function newDraftFromPreset(preset: string): Promise<string | null> {
  try {
    const active = editors.active()?.resource.path ?? null;
    const definition = await workflowCommands.fromPreset(presetRequestFor(preset, active));
    const saved = await workflowCommands.save({ id: null, expectedRevision: null, definition });
    editors.open({ key: workflowKey(saved.id), title: saved.definition.title });
    notify({
      tone: "info",
      title: `${saved.definition.title} draft saved`,
      body:
        saved.validation.state === "wellFormed"
          ? saved.validation.runnable
            ? "Well formed. Nothing runs until you review its plan and start it."
            : `Well formed. ${saved.validation.runnableReason}`
          : `Incomplete (${saved.validation.errors} problem${saved.validation.errors === 1 ? "" : "s"}): finish it in the editor. Nothing runs until you review its plan and start it.`,
    });
    return saved.id;
  } catch (error) {
    notify({ tone: "danger", title: "Could not create the workflow draft", body: errorMessage(error) });
    return null;
  }
}

export async function importDraft(text: string): Promise<{ ok: true; id: string } | { ok: false; error: string }> {
  try {
    const saved = await workflowCommands.import(text);
    editors.open({ key: workflowKey(saved.id), title: saved.definition.title });
    return { ok: true, id: saved.id };
  } catch (error) {
    return { ok: false, error: errorMessage(error) };
  }
}

/**
 * Import a workflow bundle chosen in the open dialog: its children are saved
 * first as imported drafts and the root's references rewritten; the root opens.
 * Nothing is trusted, approved, triggered, or run. `null`: the dialog was dismissed.
 */
export async function importBundle(): Promise<{ ok: true; id: string; children: number } | { ok: false; error: string } | null> {
  const picked = await openDialog({ multiple: false, directory: false, title: "Import workflow", filters: [{ name: "Workflow bundle", extensions: ["json"] }] });
  if (typeof picked !== "string") return null;
  try {
    const imported = await workflowCommands.bundleImport(picked);
    editors.open({ key: workflowKey(imported.root.id), title: imported.root.definition.title });
    notify({
      tone: "info",
      title: `Imported “${imported.root.definition.title}” as a draft`,
      body:
        (imported.children.length
          ? `With ${imported.children.length} workflow${imported.children.length === 1 ? "" : "s"} it runs, saved as drafts first. `
          : "") + "Imported workflows are untrusted drafts: nothing runs until you review a plan and start it.",
    });
    return { ok: true, id: imported.root.id, children: imported.children.length };
  } catch (error) {
    return { ok: false, error: errorMessage(error) };
  }
}

/**
 * Export a saved revision as a workflow bundle; the backend asks where to save
 * it. Dismissing that dialog is not an error (`null`).
 */
export async function exportBundle(id: string, revision: number): Promise<{ ok: true; path: string; children: number } | { ok: false; code: string; error: string } | null> {
  try {
    const written = await workflowCommands.bundleExport(id, revision, null);
    notify({
      tone: "success",
      title: "Workflow exported",
      body: `${written.path}${written.children ? ` (with ${written.children} workflow${written.children === 1 ? "" : "s"} it runs)` : ""}. Definitions only: no trust, approvals, triggers, or results.`,
    });
    return { ok: true, path: written.path, children: written.children };
  } catch (error) {
    if (isCommandError(error) && error.code === "workflow.exportCanceled") return null;
    return { ok: false, code: isCommandError(error) ? error.code : "error", error: errorMessage(error) };
  }
}
