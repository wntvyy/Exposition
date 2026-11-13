// Loads the saved workflows that composite steps reference (pinned
// revisions, nested up to the depth Rust allows) and the latest revision of
// each, so the editor can derive composite ports, show the child's steps
// read-only, and offer "Use latest revision" as an explicit parent edit.
import { useCallback, useEffect, useMemo, useState } from "react";

import { errorMessage } from "../../lib/ipc";
import type { CompositeRef, SavedWorkflow, WorkflowDefinition } from "./contract";
import { workflowCommands } from "./ipc";
import { compositeKey, compositeRefs } from "./orchestration";

/** Composite depth Rust accepts (`workflow.compositeTooDeep` beyond it). */
export const MAX_COMPOSITE_DEPTH = 3;

export interface CompositeInfo {
  ref: CompositeRef;
  /** The pinned revision (null while loading or when it failed). */
  saved: SavedWorkflow | null;
  /** The child's newest saved revision. */
  latest: number | null;
  error: string | null;
}

export interface CompositeChildren {
  infos: ReadonlyMap<string, CompositeInfo>;
  /** Pinned child definitions by `workflow@revision` (for `withComposites`). */
  definitions: ReadonlyMap<string, WorkflowDefinition>;
  loading: boolean;
  refresh: () => void;
}

export function useCompositeChildren(def: WorkflowDefinition | null): CompositeChildren {
  const refs = useMemo(() => (def ? compositeRefs(def) : []), [def]);
  const signature = refs.map(compositeKey).sort().join("|");
  const [infos, setInfos] = useState<ReadonlyMap<string, CompositeInfo>>(new Map());
  const [loading, setLoading] = useState(false);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    if (!signature) {
      setInfos(new Map());
      return;
    }
    let live = true;
    setLoading(true);
    const load = async () => {
      const found = new Map<string, CompositeInfo>();
      let level: CompositeRef[] = signature.split("|").map((key) => {
        const at = key.lastIndexOf("@");
        return { workflow: key.slice(0, at), revision: Number(key.slice(at + 1)) };
      });
      for (let depth = 1; depth <= MAX_COMPOSITE_DEPTH && level.length; depth++) {
        const next: CompositeRef[] = [];
        await Promise.all(
          level.map(async (ref) => {
            const key = compositeKey(ref);
            if (found.has(key)) return;
            found.set(key, { ref, saved: null, latest: null, error: null });
            try {
              const saved = (await workflowCommands.get(ref.workflow, ref.revision)) as SavedWorkflow;
              const latest = saved.revisions.reduce((m, r) => Math.max(m, r.revision), saved.revision);
              found.set(key, { ref, saved, latest, error: null });
              for (const inner of compositeRefs(saved.definition)) if (!found.has(compositeKey(inner))) next.push(inner);
            } catch (e) {
              found.set(key, { ref, saved: null, latest: null, error: errorMessage(e) });
            }
          }),
        );
        level = next;
      }
      if (live) {
        setInfos(found);
        setLoading(false);
      }
    };
    void load();
    return () => {
      live = false;
    };
  }, [signature, tick]);

  const definitions = useMemo(() => {
    const map = new Map<string, WorkflowDefinition>();
    for (const [key, info] of infos) if (info.saved) map.set(key, info.saved.definition);
    return map;
  }, [infos]);

  const refresh = useCallback(() => setTick((t) => t + 1), []);
  return { infos, definitions, loading, refresh };
}
