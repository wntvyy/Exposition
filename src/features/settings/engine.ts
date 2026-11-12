// Quick LaTeX engine switch for the active .tex file (command palette): sets
// the engine of the target the TeX service builds that file with, or adds a
// target for it. Open page views of the file rebuild with the new engine.
import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import { useWorkspace } from "../workspace/store";
import { engineLabel, isTexPath, targetForEntry, withEntryEngine, type LatexEngine } from "./project";
import { loadProjectSettings, saveProjectSettings, useSettings } from "./store";

/** A .tex file is active in an open folder. */
export function canSwitchEngine(): boolean {
  return isTexPath(editors.active()?.resource.path) && useWorkspace.getState().info !== null;
}

export async function switchEngine(engine: LatexEngine): Promise<boolean> {
  const path = editors.active()?.resource.path;
  const root = useWorkspace.getState().info?.root;
  if (!isTexPath(path) || !root) return false;
  const name = path.split("/").pop() ?? path;
  const state = await loadProjectSettings(root);
  if (!state) {
    notify({
      tone: "danger",
      title: "Could not read the project settings",
      body: useSettings.getState().projectError ?? undefined,
    });
    return false;
  }
  if (targetForEntry(state.settings, path).engine === engine) {
    notify({ tone: "info", title: `${name} already builds with ${engineLabel(engine)}` });
    return true;
  }
  const refusal = await saveProjectSettings(withEntryEngine(state.settings, path, engine));
  if (refusal) {
    notify({ tone: "danger", title: "Could not change the LaTeX engine", body: refusal });
    return false;
  }
  notify({
    tone: "success",
    title: `${name} now builds with ${engineLabel(engine)}`,
    body: "Saved in .exposition/project.json. Open page views rebuild now.",
  });
  return true;
}
