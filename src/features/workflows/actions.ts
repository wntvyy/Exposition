// Workflow draft actions shared by the palette commands and views.
import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import type { WorkflowPresetRequest } from "../../bindings/WorkflowPresetRequest";
import { errorMessage } from "../../lib/ipc";
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
