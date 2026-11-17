// Which side of a Split view takes keyboard focus. A route that switches a
// Page view to Split in order to work in the source (Find in Source) asks for
// the source side; the request lasts until the document leaves Split.
import { create } from "zustand";

const useSourceFocus = create<{ keys: ReadonlySet<string> }>(() => ({ keys: new Set() }));

/** The Split view of `key` should focus its source editor, not the page. */
export function requestSourceFocus(key: string): void {
  useSourceFocus.setState((s) => ({ keys: new Set([...s.keys, key]) }));
}

/** Forget the request for `key` (the document left Split). */
export function clearSourceFocus(key: string): void {
  if (!useSourceFocus.getState().keys.has(key)) return;
  useSourceFocus.setState((s) => ({ keys: new Set([...s.keys].filter((k) => k !== key)) }));
}

/** React hook: whether the Split view of `key` focuses its source side. */
export function usePrefersSource(key: string): boolean {
  return useSourceFocus((s) => s.keys.has(key));
}
