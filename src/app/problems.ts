// Problems contract: features publish the diagnostics they own (build results,
// Lean checks, ...) with a way to reveal each one; the Problems view shows all
// sources together. Features never render each other's diagnostics directly.
import { create } from "zustand";

import type { Diagnostic } from "../bindings/Diagnostic";

export interface ProblemSource {
  /** Stable source id, e.g. "lean". */
  id: string;
  /** Short label shown next to each problem, e.g. "Lean". */
  label: string;
  diagnostics: Diagnostic[];
  /** Show the diagnostic's location in an editor. */
  reveal: (diagnostic: Diagnostic) => void;
}

export const useProblems = create<{ sources: Record<string, ProblemSource> }>(() => ({ sources: {} }));

/** Publish (or replace) a source's current diagnostics. */
export function publishProblems(source: ProblemSource): void {
  useProblems.setState((state) => ({ sources: { ...state.sources, [source.id]: source } }));
}

export function clearProblems(id: string): void {
  useProblems.setState((state) => {
    if (!(id in state.sources)) return state;
    const { [id]: _removed, ...rest } = state.sources;
    return { sources: rest };
  });
}
