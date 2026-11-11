// Position transforms between concurrent patch lists.
//
// The document service rebases a stale edit over newer revisions with
// `ConflictPolicy::RebaseDisjoint` (crates/exposition-workspace/src/documents.rs
// `map_range`). A view with unacknowledged local edits must transform incoming
// revisions over those edits with the mirror-image tie-breaking, so both sides
// converge on the same text. Overlapping edits are a conflict (the service
// rejects the local edit; the view resynchronizes).
import type { TextPatch } from "../../bindings/TextPatch";

const delta = (p: TextPatch) => p.text.length - (p.range.end - p.range.start);

/**
 * Map a local range over concurrent (remote, applied-first) patches exactly as
 * the service rebases it: a remote insertion at the start of the range lands
 * before it, one at the end of a non-empty range lands after it, one strictly
 * inside conflicts. Returns null on conflict.
 */
export function mapLocalRange(range: { start: number; end: number }, remote: readonly TextPatch[]) {
  let shift = 0;
  for (const q of remote) {
    const d = delta(q);
    if (q.range.start === q.range.end) {
      const at = q.range.start;
      if (at <= range.start) shift += d;
      else if (at < range.end) return null;
    } else if (q.range.end <= range.start) {
      shift += d;
    } else if (q.range.start >= range.end) {
      // after the range
    } else {
      return null;
    }
  }
  return { start: range.start + shift, end: range.end + shift };
}

/**
 * Map a remote range over local patches (applied first in the view) with the
 * mirror-image tie-breaking of `mapLocalRange`. Returns null on conflict.
 */
export function mapRemoteRange(range: { start: number; end: number }, local: readonly TextPatch[]) {
  let shift = 0;
  const remoteEmpty = range.start === range.end;
  for (const q of local) {
    const d = delta(q);
    const s = q.range.start;
    const e = q.range.end;
    if (s === e) {
      if (remoteEmpty) {
        // Same point: the service puts the remote insertion first.
        if (s < range.start) shift += d;
      } else if (s <= range.start) {
        shift += d;
      } else if (s >= range.end) {
        // after
      } else {
        return null;
      }
    } else if (remoteEmpty) {
      if (range.start <= s) {
        // before the local replacement
      } else if (range.start >= e) {
        shift += d;
      } else {
        return null;
      }
    } else if (range.end <= s) {
      // before
    } else if (range.start >= e) {
      shift += d;
    } else {
      return null;
    }
  }
  return { start: range.start + shift, end: range.end + shift };
}

function mapAll(
  patches: readonly TextPatch[],
  over: readonly TextPatch[],
  map: typeof mapLocalRange,
): TextPatch[] | null {
  const out: TextPatch[] = [];
  for (const p of patches) {
    const range = map(p.range, over);
    if (!range) return null;
    out.push({ range, text: p.text });
  }
  return out;
}

/**
 * Transform concurrent patch lists based on the same text. Returns
 * `remote'` (to apply after `local`) and `local'` (to apply after `remote`),
 * or null when they overlap.
 */
export function transform(
  remote: readonly TextPatch[],
  local: readonly TextPatch[],
): { remote: TextPatch[]; local: TextPatch[] } | null {
  const remoteAfter = mapAll(remote, local, mapRemoteRange);
  const localAfter = mapAll(local, remote, mapLocalRange);
  if (!remoteAfter || !localAfter) return null;
  return { remote: remoteAfter, local: localAfter };
}

/** Apply sorted, non-overlapping patches (relative to `text`). */
export function applyPatches(text: string, patches: readonly TextPatch[]): string {
  let out = "";
  let cursor = 0;
  for (const p of patches) {
    if (p.range.start < cursor || p.range.end > text.length || p.range.start > p.range.end) {
      throw new Error(`invalid patch ${JSON.stringify(p.range)} for length ${text.length}`);
    }
    out += text.slice(cursor, p.range.start) + p.text;
    cursor = p.range.end;
  }
  return out + text.slice(cursor);
}

export function samePatches(a: readonly TextPatch[], b: readonly TextPatch[]): boolean {
  return (
    a.length === b.length &&
    a.every((p, i) => {
      const q = b[i];
      return q !== undefined && p.range.start === q.range.start && p.range.end === q.range.end && p.text === q.text;
    })
  );
}

/** One prefix/suffix-trimmed patch turning `from` into `to` (null when equal). */
export function diffPatch(from: string, to: string): TextPatch | null {
  if (from === to) return null;
  let start = 0;
  const max = Math.min(from.length, to.length);
  while (start < max && from.charCodeAt(start) === to.charCodeAt(start)) start++;
  // Do not split a surrogate pair.
  if (start > 0 && isHighSurrogate(from.charCodeAt(start - 1))) start--;
  let endFrom = from.length;
  let endTo = to.length;
  while (endFrom > start && endTo > start && from.charCodeAt(endFrom - 1) === to.charCodeAt(endTo - 1)) {
    endFrom--;
    endTo--;
  }
  if (endFrom < from.length && isLowSurrogate(from.charCodeAt(endFrom))) {
    endFrom++;
    endTo++;
  }
  return { range: { start, end: endFrom }, text: to.slice(start, endTo) };
}

const isHighSurrogate = (c: number) => c >= 0xd800 && c <= 0xdbff;
const isLowSurrogate = (c: number) => c >= 0xdc00 && c <= 0xdfff;
