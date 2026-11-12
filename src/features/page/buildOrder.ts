// Ordering of build results shown by a page view. Results arrive as events
// and as the reply to typesetting_watch, in either order; a view must never
// regress to an older build.

/** The trailing sequence number of a build id (`typst-12`, `tex-3`), or 0. */
export function buildSequence(id: string): number {
  const match = /(\d+)$/.exec(id);
  return match ? Number(match[1]) : 0;
}

/** Whether build `a` is older than `b` (by compiled revision, then sequence). */
export function isOlder(a: { revision: number; seq: number }, b: { revision: number; seq: number }): boolean {
  return a.revision < b.revision || (a.revision === b.revision && a.seq < b.seq);
}
