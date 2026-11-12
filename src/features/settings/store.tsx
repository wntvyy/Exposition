// Settings view state. Every value comes from (and every change goes through)
// the owning service: execution settings and trust in the execution service,
// app info from the app. Toolchains, research environments, theme, editor
// settings, and provider status reuse their features' existing stores.
import { create } from "zustand";

import type { AppInfo } from "../../bindings/AppInfo";
import type { DockerImage } from "../../bindings/DockerImage";
import type { ExecutionSettingsState } from "../../bindings/ExecutionSettingsState";
import type { TrustedRoot } from "../../bindings/TrustedRoot";
import type { TrustList } from "../../bindings/TrustList";
import { notify } from "../../app/notifications";
import { commands, errorMessage } from "../../lib/ipc";
import { confirm } from "../../ui/components";
import { loadToolchains } from "../jobs/store";
import { nestedTrustedRoots, type SectionId } from "./model";

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
    useSettings.setState({ execution: await commands.executionSettingsGet(), executionError: null });
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
          Builds, kernels, and commands in this folder will no longer run on this computer until you trust it again
          (Exposition asks when an action needs it). Processes that are already running are not stopped.
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
