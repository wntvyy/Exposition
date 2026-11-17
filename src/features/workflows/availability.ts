// Runner availability (whether a step's runner can run in the open project
// now) depends on state that changes while views stay open: the restricted
// images and tool locations in Settings › Execution, folder trust, the open
// project, and what an explicit environment refresh finds (Docker started or
// stopped). Views that show availability (workflow editors, the Workflows
// list, Research's restricted row) re-read it whenever this epoch advances.
// Display only: planning, approval, and dispatch check availability again
// in the backend.
import { useEffect } from "react";
import { create } from "zustand";

import { events } from "../../lib/ipc";
import { useSettings } from "../settings/store";
import { useWorkspace } from "../workspace/store";

const useEpoch = create<{ epoch: number }>(() => ({ epoch: 0 }));

/** Availability may have changed (e.g. an explicit environment refresh): views re-read it. */
export function runnerAvailabilityChanged(): void {
  useEpoch.setState((s) => ({ epoch: s.epoch + 1 }));
}

let listening = false;

function ensureListening(): void {
  if (listening) return;
  listening = true;
  // Execution settings change only through the settings store (images, tool locations).
  useSettings.subscribe((state, previous) => {
    if (state.execution !== previous.execution) runnerAvailabilityChanged();
  });
  useWorkspace.subscribe((state, previous) => {
    if ((state.info?.root ?? null) !== (previous.info?.root ?? null)) runnerAvailabilityChanged();
  });
  try {
    void events.trustChanged(() => runnerAvailabilityChanged()).catch(() => {
      // Not running inside the desktop shell (unit tests).
    });
  } catch {
    // Not running inside the desktop shell (unit tests).
  }
}

/** A number that changes whenever runner availability may have changed. */
export function useRunnerAvailabilityEpoch(): number {
  useEffect(ensureListening, []);
  return useEpoch((s) => s.epoch);
}
