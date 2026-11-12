// Build state of watched typesetting entries, fed by `typesetting:*` events.
import { create } from "zustand";

import type { BuildActivity } from "../../bindings/BuildActivity";
import type { BuildEvent } from "../../bindings/BuildEvent";
import { events } from "../../lib/ipc";

export interface EntryBuildState {
  entry: string;
  activity: BuildActivity;
  /** Latest finished build (successful or not). */
  latest: BuildEvent | null;
}

/** A request to show a source position (from the problems panel). */
export interface RevealRequest {
  path: string;
  offset: number;
  serial: number;
}

interface TypesettingState {
  entries: Record<string, EntryBuildState>;
  reveal: RevealRequest | null;
}

export const useTypesetting = create<TypesettingState>(() => ({ entries: {}, reveal: null }));

export function recordBuild(event: BuildEvent): void {
  const entry = event.result.target.entry;
  useTypesetting.setState((state) => ({
    entries: {
      ...state.entries,
      [entry]: { entry, activity: state.entries[entry]?.activity ?? "idle", latest: event },
    },
  }));
}

export function recordActivity(entry: string, activity: BuildActivity): void {
  useTypesetting.setState((state) => ({
    entries: {
      ...state.entries,
      [entry]: { entry, latest: state.entries[entry]?.latest ?? null, activity },
    },
  }));
}

let serial = 0;
export function requestReveal(path: string, offset: number): void {
  useTypesetting.setState({ reveal: { path, offset, serial: ++serial } });
}

let installed = false;
/** Subscribe the store to build events once. */
export function installTypesettingEvents(): void {
  if (installed) return;
  installed = true;
  void events.typesettingBuild(recordBuild).catch(() => {});
  void events.typesettingStatus((e) => recordActivity(e.entry, e.activity)).catch(() => {});
}

export type BuildSummary = "building" | "succeeded" | "failed" | "stale" | "untrusted" | "unavailable" | "none";

/** Summary status of an entry: a failed latest build leaves the shown pages stale. */
export function summarize(state: EntryBuildState | undefined): BuildSummary {
  if (!state) return "none";
  if (state.activity !== "idle") return "building";
  const latest = state.latest;
  if (!latest) return "none";
  if (latest.result.status.kind === "succeeded") return "succeeded";
  // Refused before running (LaTeX in an untrusted workspace): not a failure.
  if (latest.refusal?.code === "untrustedWorkspace") return "untrusted";
  // The compiler is missing: nothing ran, so nothing failed.
  if (latest.result.status.kind === "toolUnavailable") return "unavailable";
  return latest.lastGood ? "stale" : "failed";
}

/** The command that opens Settings at the tool locations. */
export const TOOL_SETTINGS_COMMAND = "settings.execution";
export const TOOL_SETTINGS_LABEL = "Open Settings → Tool locations";

/** Why a build produced nothing, when its status (not a diagnostic) says so. */
export interface BuildProblem {
  message: string;
  hints: string[];
  /** Offer to open the tool locations in Settings. */
  toolSettings: boolean;
}

/**
 * The problem a build's status reports beyond its diagnostics: a missing
 * compiler (with its install guidance) or a build that could not run. Trust
 * refusals have their own prompt and are not problems.
 */
export function statusProblem(event: BuildEvent | null | undefined): BuildProblem | null {
  const status = event?.result.status;
  if (!event || !status || event.refusal?.code === "untrustedWorkspace") return null;
  if (status.kind === "toolUnavailable") {
    return {
      message: `${status.tool} is not available, so ${event.result.target.entry} cannot be built.`,
      hints: [status.guidance],
      toolSettings: true,
    };
  }
  if (status.kind === "error") {
    return { message: status.message, hints: [], toolSettings: /tool|executable|program|not found/i.test(status.message) };
  }
  return null;
}

/** A build diagnostic that points at the tool locations (e.g. a wrong compiler path). */
export function mentionsToolSettings(hints: readonly string[]): boolean {
  return hints.some((h) => h.includes("Tool locations"));
}
