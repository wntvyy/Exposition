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

export type BuildSummary = "building" | "succeeded" | "failed" | "stale" | "untrusted" | "none";

/** Summary status of an entry: a failed latest build leaves the shown pages stale. */
export function summarize(state: EntryBuildState | undefined): BuildSummary {
  if (!state) return "none";
  if (state.activity !== "idle") return "building";
  const latest = state.latest;
  if (!latest) return "none";
  if (latest.result.status.kind === "succeeded") return "succeeded";
  // Refused before running (LaTeX in an untrusted workspace): not a failure.
  if (latest.refusal?.code === "untrustedWorkspace") return "untrusted";
  return latest.lastGood ? "stale" : "failed";
}
