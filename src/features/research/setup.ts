// Guided environment setup state: the running (or last) setup of the open
// project, its live output, and what happens when it ends (re-discovery,
// selecting the new environment). All work happens in Rust; this mirrors
// `research:setup` events.
import { create } from "zustand";

import { notify } from "../../app/notifications";
import type { SetupEvent } from "../../bindings/SetupEvent";
import type { SetupPlan } from "../../bindings/SetupPlan";
import type { SetupRequest } from "../../bindings/SetupRequest";
import type { SetupStatus } from "../../bindings/SetupStatus";
import { errorMessage } from "../../lib/ipc";
import { setupCommands, setupEvents } from "./ipc";
import { chooseEnvironment, loadEnvironments, useResearch } from "./store";

/** Output lines kept in the webview for the live log. */
export const SETUP_LOG_LINES = 200;

export interface SetupViewState {
  /** The running setup of the open project, else its last finished one. */
  status: SetupStatus | null;
  /** Live output lines of the running setup (the status carries a shorter tail). */
  log: string[];
  /** Finished setups whose outcome banner was dismissed. */
  dismissed: string[];
}

export const useSetup = create<SetupViewState>(() => ({ status: null, log: [], dismissed: [] }));

let started = false;
let partial = "";

function appendOutput(text: string): void {
  const lines = (partial + text).split(/\r?\n/);
  partial = lines.pop() ?? "";
  useSetup.setState((state) => {
    const log = [...state.log, ...lines];
    return { log: log.length > SETUP_LOG_LINES ? log.slice(log.length - SETUP_LOG_LINES) : log };
  });
}

/** Apply one setup event (exported for tests). */
export function applySetupEvent(event: SetupEvent): void {
  if (event.kind === "output") {
    if (useSetup.getState().status?.id === event.setupId) appendOutput(event.text);
    return;
  }
  const previous = useSetup.getState().status;
  const status = event.status;
  const root = useResearch.getState().workspace?.root;
  if (root && status.plan.root.toLowerCase() !== root.toLowerCase()) return;
  const fresh = previous?.id !== status.id;
  if (fresh) partial = "";
  useSetup.setState((state) => ({ status, log: fresh ? [...status.log] : state.log }));
  // A live end of this setup (also when its start was not seen here): re-read and select.
  const alreadyEnded = previous?.id === status.id && previous.state !== "running";
  if (status.state !== "running" && !alreadyEnded) void finished(status);
}

async function finished(status: SetupStatus): Promise<void> {
  if (partial) appendOutput("\n");
  // Rust discovered again when the setup ended; read that report.
  await loadEnvironments(false);
  if (status.state === "succeeded" && status.environmentId) {
    chooseEnvironment(status.environmentId);
    const env = useResearch.getState().environments?.runtimes.find((r) => r.id === status.environmentId);
    notify({
      tone: "success",
      title: "Environment ready",
      body: env ? `${env.displayName} is selected for this project.` : `${status.plan.targetName} is ready.`,
    });
  } else if (status.state === "failed" && document.visibilityState !== "visible") {
    notify({ tone: "danger", title: "Environment setup failed", body: status.message ?? undefined });
  }
}

/** Subscribe once and read the open project's setup. */
export function startSetupTracking(): void {
  if (started) return;
  started = true;
  void setupEvents.setup(applySetupEvent);
  void setupEvents.resync(() => void refreshSetup());
  let root = useResearch.getState().workspace?.root ?? null;
  useResearch.subscribe((state) => {
    const next = state.workspace?.root ?? null;
    if (next !== root) {
      root = next;
      void refreshSetup();
    }
  });
  void refreshSetup();
}

export async function refreshSetup(): Promise<void> {
  if (!useResearch.getState().workspace) {
    useSetup.setState({ status: null, log: [] });
    return;
  }
  try {
    const status = await setupCommands.status();
    partial = "";
    useSetup.setState((state) => ({
      status,
      log: status && state.status?.id === status.id && state.log.length > status.log.length ? state.log : (status?.log ?? []),
    }));
  } catch {
    // The status is informational; the next event brings it.
  }
}

export function planSetup(request: SetupRequest): Promise<SetupPlan> {
  return setupCommands.plan(request);
}

/** Start a reviewed plan; throws the command error (e.g. `setup.planStale`) for the dialog. */
export async function startSetup(plan: SetupPlan): Promise<SetupStatus> {
  const status = await setupCommands.start(plan);
  partial = "";
  useSetup.setState({ status, log: [...status.log] });
  return status;
}

export async function cancelSetup(): Promise<void> {
  const status = useSetup.getState().status;
  if (!status || status.state !== "running") return;
  try {
    await setupCommands.cancel(status.id);
  } catch (e) {
    notify({ tone: "danger", title: "Could not cancel the setup", body: errorMessage(e) });
  }
}

export function dismissSetupOutcome(id: string): void {
  useSetup.setState((state) => ({ dismissed: [...state.dismissed, id] }));
}
