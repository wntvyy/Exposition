// Data hooks for graph views: load on demand, keep showing the last result
// while reloading, and re-read when the graph revision or epoch moves.
import { useEffect, useRef, useState } from "react";

import { errorCode, errorText } from "./api";
import { useGraph } from "./store";

export interface Loaded<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
  errorCode: string | null;
  /** Re-run the loader now. */
  reload: () => void;
}

/**
 * Run `load` when `key` changes or the graph changes (revision/epoch). Pass
 * `key = null` to load nothing. Results of superseded loads are dropped.
 */
export function useGraphLoad<T>(key: string | null, load: () => Promise<T>, options: { followRevision?: boolean; delayMs?: number } = {}): Loaded<T> {
  const followRevision = options.followRevision ?? true;
  const revision = useGraph((s) => (followRevision ? s.revision : 0));
  const epoch = useGraph((s) => s.epoch);
  const [state, setState] = useState<{ data: T | null; loading: boolean; error: string | null; errorCode: string | null; key: string | null }>({
    data: null,
    loading: key !== null,
    error: null,
    errorCode: null,
    key,
  });
  const [nonce, setNonce] = useState(0);
  const loadRef = useRef(load);
  loadRef.current = load;

  useEffect(() => {
    if (key === null) {
      setState({ data: null, loading: false, error: null, errorCode: null, key });
      return;
    }
    let cancelled = false;
    setState((s) => ({ ...s, loading: true, key, ...(s.key !== key ? { data: null } : {}) }));
    const run = () =>
      loadRef.current().then(
        (data) => !cancelled && setState({ data, loading: false, error: null, errorCode: null, key }),
        (error: unknown) =>
          !cancelled && setState((s) => ({ ...s, loading: false, error: errorText(error), errorCode: errorCode(error), key })),
      );
    const timer = options.delayMs ? setTimeout(run, options.delayMs) : null;
    if (!timer) void run();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [key, revision, epoch, nonce, options.delayMs]);

  return { data: state.data, loading: state.loading, error: state.error, errorCode: state.errorCode, reload: () => setNonce((n) => n + 1) };
}
