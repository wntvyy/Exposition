// Settings view state. Every value comes from (and every change goes through)
// the owning service: execution settings and trust in the execution service,
// app info from the app. Toolchains, research environments, theme, editor
// settings, and provider status reuse their features' existing stores.
import { create } from "zustand";

import type { AppInfo } from "../../bindings/AppInfo";
import type { DockerImage } from "../../bindings/DockerImage";
import type { ExecutionSettingsState } from "../../bindings/ExecutionSettingsState";
import type { ProjectSettings } from "../../bindings/ProjectSettings";
import type { ProjectSettingsState } from "../../bindings/ProjectSettingsState";
import type { RestrictedLanguage } from "../../bindings/RestrictedLanguage";
import type { TrustedRoot } from "../../bindings/TrustedRoot";
import type { TrustList } from "../../bindings/TrustList";
import { notify } from "../../app/notifications";
import { commands, errorMessage } from "../../lib/ipc";
import { confirm } from "../../ui/components";
import { loadToolchains } from "../jobs/store";
import { nestedTrustedRoots, type SectionId } from "./model";
import { reportSettingsWarning } from "./warnings";

export interface SettingsState {
  section: SectionId;
  app: AppInfo | null;
  execution: ExecutionSettingsState | null;
  executionError: string | null;
  trust: TrustList | null;
  trustError: string | null;
  images: DockerImage[] | null;
  imagesError: string | null;
  imagesLoading: boolean;
  /** A Docker image change is being saved. */
  savingImage: boolean;
  /** Project settings of the open folder (`null`: not loaded, or no folder). */
  project: ProjectSettingsState | null;
  /** Folder the project settings were read from. */
  projectRoot: string | null;
  projectError: string | null;
  savingProject: boolean;
}

export const initialSettingsState: SettingsState = {
  section: "appearance",
  app: null,
  execution: null,
  executionError: null,
  trust: null,
  trustError: null,
  images: null,
  imagesError: null,
  imagesLoading: false,
  savingImage: false,
  project: null,
  projectRoot: null,
  projectError: null,
  savingProject: false,
};

export const useSettings = create<SettingsState>(() => initialSettingsState);

export function showSection(section: SectionId): void {
  useSettings.setState({ section });
}

export async function loadAppInfo(): Promise<void> {
  try {
    useSettings.setState({ app: await commands.appInfo() });
  } catch {
    // About shows "Checking…" until it loads; nothing else depends on it.
  }
}

export async function loadExecutionSettings(): Promise<void> {
  try {
    const execution = await commands.executionSettingsGet();
    useSettings.setState({ execution, executionError: null });
    reportSettingsWarning("execution", execution.warning);
  } catch (e) {
    useSettings.setState({ executionError: errorMessage(e) });
  }
}

export async function loadTrust(): Promise<void> {
  try {
    useSettings.setState({ trust: await commands.trustList(), trustError: null });
  } catch (e) {
    useSettings.setState({ trustError: errorMessage(e) });
  }
}

/** Read `.exposition/project.json` of the open folder (`root`). */
export async function loadProjectSettings(root: string): Promise<ProjectSettingsState | null> {
  try {
    const project = await commands.projectSettingsGet();
    useSettings.setState({ project, projectRoot: root, projectError: null });
    reportSettingsWarning("project", project.warning);
    return project;
  } catch (e) {
    useSettings.setState({ project: null, projectRoot: root, projectError: errorMessage(e) });
    return null;
  }
}

export function forgetProjectSettings(): void {
  useSettings.setState({ project: null, projectRoot: null, projectError: null });
}

/**
 * Save project settings (validated by the workspace service; open page views
 * of LaTeX entries rebuild with the new targets). Resolves with the refusal
 * message, or null when saved.
 */
export async function saveProjectSettings(settings: ProjectSettings): Promise<string | null> {
  useSettings.setState({ savingProject: true });
  try {
    const saved = await commands.projectSettingsUpdate(settings);
    useSettings.setState((s) => ({
      savingProject: false,
      project: { settings: saved, exists: true, warning: s.project?.warning ?? null },
    }));
    return null;
  } catch (e) {
    useSettings.setState({ savingProject: false });
    return errorMessage(e);
  }
}

/** List local images (an application probe: `docker image ls`; never pulls). */
export async function loadImages(): Promise<void> {
  useSettings.setState({ imagesLoading: true });
  try {
    useSettings.setState({ images: await commands.dockerImagesList(), imagesError: null, imagesLoading: false });
  } catch (e) {
    useSettings.setState({ images: null, imagesError: errorMessage(e), imagesLoading: false });
  }
}

/** Set or clear the restricted-execution image. Resolves true when saved. */
export async function setDockerImage(image: string | null): Promise<boolean> {
  useSettings.setState({ savingImage: true });
  try {
    const execution = await commands.executionSettingsUpdate({ kind: "dockerImage", image });
    useSettings.setState({ execution, savingImage: false });
    notify({
      tone: "success",
      title: image ? "Restricted execution image set" : "Restricted execution image cleared",
      body: image ?? undefined,
    });
    return true;
  } catch (e) {
    useSettings.setState({ savingImage: false });
    notify({ tone: "danger", title: "Could not change the container image", body: errorMessage(e) });
    return false;
  }
}

/**
 * Set or clear the image of restricted R or Julia workflow batches (their own
 * image; without one those batches are refused, never run natively instead).
 */
export async function setLanguageImage(language: RestrictedLanguage, image: string | null): Promise<boolean> {
  useSettings.setState({ savingImage: true });
  const name = language === "r" ? "R" : "Julia";
  try {
    const execution = await commands.executionSettingsUpdate({ kind: "languageImage", language, image });
    useSettings.setState({ execution, savingImage: false });
    notify({ tone: "success", title: image ? `${name} image set` : `${name} image cleared`, body: image ?? undefined });
    return true;
  } catch (e) {
    useSettings.setState({ savingImage: false });
    notify({ tone: "danger", title: `Could not change the ${name} image`, body: errorMessage(e) });
    return false;
  }
}

/**
 * Set (`path`) or clear (`null`) a tool location. Resolves with the refusal
 * message, or null when saved (discovery is then re-run so the tool list
 * shows the effect).
 */
export async function setToolPath(tool: string, path: string | null): Promise<string | null> {
  try {
    const execution = await commands.executionSettingsUpdate({ kind: "toolPath", tool, path });
    useSettings.setState({ execution });
  } catch (e) {
    return errorMessage(e);
  }
  void loadToolchains(true);
  return null;
}

/**
 * Ask, then revoke trust for a listed folder (and, as the service does, any
 * trusted folder inside it, which the prompt names). Resolves true when revoked.
 */
export async function revokeTrust(root: TrustedRoot): Promise<boolean> {
  const nested = nestedTrustedRoots(
    root.path,
    (useSettings.getState().trust?.roots ?? []).map((r) => r.path),
  );
  const choice = await confirm<"cancel" | "revoke">({
    title: "Revoke trust for this folder?",
    message: (
      <div className="exp-settings__confirm" data-testid="revoke-dialog">
        <code className="exp-settings__path">{root.path}</code>
        <p>
          Builds, kernels, Lean servers, and commands running in this folder are stopped now. Nothing in it runs again
          until you trust it again (Exposition asks when an action needs it).
        </p>
        {nested.length > 0 && (
          <>
            <p>Trusted folders inside it are revoked too:</p>
            <ul className="exp-settings__locations">
              {nested.map((p) => (
                <li key={p}>
                  <code className="exp-settings__path">{p}</code>
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    ),
    actions: [
      { id: "cancel", label: "Cancel", variant: "secondary" },
      { id: "revoke", label: "Revoke trust", variant: "danger" },
    ],
    cancelId: "cancel",
    defaultId: "cancel",
    tone: "warning",
  });
  if (choice !== "revoke") return false;
  try {
    await commands.trustRevoke(root.path);
    notify({ tone: "success", title: "Trust revoked", body: root.path });
    return true;
  } catch (e) {
    notify({ tone: "danger", title: "Could not revoke trust", body: errorMessage(e) });
    return false;
  } finally {
    await loadTrust();
  }
}
