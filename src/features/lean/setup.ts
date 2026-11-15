// Mathlib setup state: the running (or last) setup, its live output, and the
// dialog that plans one. All work happens in Rust (`lean_setup_*`); this
// mirrors `lean:setup` events.
import { create } from "zustand";

import { notify } from "../../app/notifications";
import type { LeanSetupEvent } from "../../bindings/LeanSetupEvent";
import type { LeanSetupPlan } from "../../bindings/LeanSetupPlan";
import type { LeanSetupStatus } from "../../bindings/LeanSetupStatus";
import { commands, errorMessage, events } from "../../lib/ipc";

/** Output lines kept in the webview for the live log. */
export const SETUP_LOG_LINES = 200;

/** What the dialog proposes when it opens. */
export type MathlibSetupTarget =
  | { kind: "auto" }
  /** Prepare the existing package in `folder` (`""` is the project folder). */
  | { kind: "prepare"; folder: string }
  | { kind: "new" };

export interface LeanSetupViewState {
  /** The running setup, else the last finished one of this session. */
  status: LeanSetupStatus | null;
  /** Live output lines of the running setup (the status carries a shorter tail). */
  log: string[];
  /** Finished setups whose outcome was dismissed. */
  dismissed: string[];
  /** The Set Up Mathlib dialog: open with this proposal, or closed. */
  dialog: MathlibSetupTarget | null;
}

export const useLeanSetup = create<LeanSetupViewState>(() => ({ status: null, log: [], dismissed: [], dialog: null }));

/** ANSI escape sequences (Lake and Mathlib's cache print them). */
const ANSI = new RegExp(`${String.fromCharCode(27)}\\[[0-9;?]*[A-Za-z]`, "g");

let started = false;
let partial = "";

function appendOutput(text: string): void {
  // A carriage return restarts the line (download counters overwrite themselves).
  const lines = (partial + text).split(/\r?\n/).map((line) => line.slice(line.lastIndexOf("\r") + 1));
  partial = lines.pop() ?? "";
  const clean = lines.map((line) => line.replace(ANSI, "")).filter((line) => line.trim() !== "");
  if (clean.length === 0) return;
  useLeanSetup.setState((state) => {
    const log = [...state.log, ...clean];
    return { log: log.length > SETUP_LOG_LINES ? log.slice(log.length - SETUP_LOG_LINES) : log };
  });
}

/** The package folder as shown to people. */
export function folderLabel(folder: string): string {
  return folder === "" ? "the project folder" : folder;
}

/** Apply one setup event (exported for tests). */
export function applyLeanSetupEvent(event: LeanSetupEvent): void {
  if (event.kind === "output") {
    if (useLeanSetup.getState().status?.id === event.setupId) appendOutput(event.text);
    return;
  }
  const previous = useLeanSetup.getState().status;
  const status = event.status;
  const fresh = previous?.id !== status.id;
  if (fresh) partial = "";
  useLeanSetup.setState((state) => ({ status, log: fresh || status.state !== "running" ? [...status.log] : state.log }));
  const alreadyEnded = previous?.id === status.id && previous.state !== "running";
  if (status.state !== "running" && !alreadyEnded) finished(status);
}

function finished(status: LeanSetupStatus): void {
  const where = folderLabel(status.plan.folder);
  // The dialog shows the outcome while it is open.
  if (useLeanSetup.getState().dialog) return;
  if (status.state === "succeeded") {
    notify({
      tone: "success",
      title: "Mathlib is ready",
      body: `The package in ${where} uses Lean ${status.plan.toolchain} and Mathlib ${status.plan.mathlibRevision}. Open a Lean file in it to check it.`,
    });
  } else {
    notify({
      tone: status.state === "canceled" ? "info" : "danger",
      title: status.state === "canceled" ? "Mathlib setup canceled" : "Mathlib setup failed",
      body: status.message ?? undefined,
      actions: [{ label: "Show details", run: () => openMathlibSetup({ kind: "auto" }) }],
    });
  }
}

/** Subscribe once and read the current setup. */
export function startLeanSetupTracking(): void {
  if (started) return;
  started = true;
  void events.leanSetup(applyLeanSetupEvent);
  void events.leanSetupResync(() => void refreshLeanSetup());
  void refreshLeanSetup();
}

export async function refreshLeanSetup(): Promise<void> {
  try {
    const status = await commands.leanSetupStatus();
    partial = "";
    useLeanSetup.setState((state) => ({
      status,
      log: status && state.status?.id === status.id && state.log.length > status.log.length ? state.log : (status?.log ?? []),
    }));
  } catch {
    // Informational; the next event brings it.
  }
}

/** Start a reviewed plan; throws the command error (e.g. `lean.setup.planStale`) for the dialog. */
export async function startLeanSetup(plan: LeanSetupPlan): Promise<LeanSetupStatus> {
  const status = await commands.leanSetupStart(plan);
  partial = "";
  useLeanSetup.setState({ status, log: [...status.log] });
  return status;
}

export async function cancelLeanSetup(): Promise<void> {
  const status = useLeanSetup.getState().status;
  if (!status || status.state !== "running") return;
  try {
    await commands.leanSetupCancel(status.id);
  } catch (e) {
    notify({ tone: "danger", title: "Could not cancel the setup", body: errorMessage(e) });
  }
}

export function dismissLeanSetupOutcome(id: string): void {
  useLeanSetup.setState((state) => ({ dismissed: [...state.dismissed, id] }));
}

export function openMathlibSetup(target: MathlibSetupTarget): void {
  startLeanSetupTracking();
  useLeanSetup.setState({ dialog: target });
}

export function closeMathlibSetup(): void {
  useLeanSetup.setState({ dialog: null });
}

/** One command line as it is shown in a plan (arguments with spaces quoted). */
export function commandLine(program: string, args: string[]): string {
  const quote = (s: string) => (s === "" || /[\s"'<>|&;^%()]/.test(s) ? `"${s.replace(/"/g, '\\"')}"` : s);
  return [quote(program), ...args.map(quote)].join(" ");
}

/** Reset for tests. */
export function resetLeanSetupForTests(): void {
  partial = "";
  useLeanSetup.setState({ status: null, log: [], dismissed: [], dialog: null });
}
