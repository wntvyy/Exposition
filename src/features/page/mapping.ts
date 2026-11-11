// Offset mapping across document revisions (UTF-16, like JavaScript strings).
//
// The page view keeps the caret in the current document revision while the
// displayed page map belongs to the revision its build compiled. Offsets move
// between the two through the patches of every revision in between.
import type { Affinity } from "../../bindings/Affinity";
import type { TextPatch } from "../../bindings/TextPatch";

export interface RevisionStep {
  previous: number;
  revision: number;
  /** Sorted, non-overlapping patches relative to `previous`. */
  patches: TextPatch[];
}

/** Map an offset of the previous revision through one revision's patches. */
export function mapForward(offset: number, patches: readonly TextPatch[], affinity: Affinity): number {
  let shift = 0;
  for (const patch of patches) {
    const { start, end } = patch.range;
    const inserted = patch.text.length;
    if (end < offset || (end === offset && start < end)) {
      shift += inserted - (end - start);
    } else if (start === offset && start === end) {
      if (affinity === "downstream") shift += inserted;
    } else if (start < offset && offset < end) {
      return Math.max(0, (affinity === "upstream" ? start : start + inserted) + shift);
    } else if (start >= offset) {
      break;
    }
  }
  return Math.max(0, offset + shift);
}

/** Map an offset of the new revision back through one revision's patches. */
export function mapBackward(offset: number, patches: readonly TextPatch[]): number {
  let shift = 0;
  for (const patch of patches) {
    const newStart = patch.range.start + shift;
    const newEnd = newStart + patch.text.length;
    if (offset < newStart) return offset - shift;
    if (offset <= newEnd) return offset === newEnd ? patch.range.end : patch.range.start;
    shift += patch.text.length - (patch.range.end - patch.range.start);
  }
  return offset - shift;
}

/** Patches of recent revisions of one document. */
export class RevisionHistory {
  private steps: RevisionStep[] = [];

  add(step: RevisionStep): void {
    if (this.steps.some((s) => s.revision === step.revision)) return;
    this.steps.push(step);
    this.steps.sort((a, b) => a.revision - b.revision);
    if (this.steps.length > 4096) this.steps.splice(0, this.steps.length - 4096);
  }

  /** Map `offset` from revision `from` to `to` (to >= from); null if a step is missing. */
  forward(offset: number, from: number, to: number, affinity: Affinity): number | null {
    let current = offset;
    for (let rev = from; rev < to; rev++) {
      const step = this.steps.find((s) => s.previous === rev);
      if (!step) return null;
      current = mapForward(current, step.patches, affinity);
    }
    return current;
  }

  /** Map `offset` from revision `from` back to `to` (to <= from); null if a step is missing. */
  backward(offset: number, from: number, to: number): number | null {
    let current = offset;
    for (let rev = from; rev > to; rev--) {
      const step = this.steps.find((s) => s.revision === rev);
      if (!step) return null;
      current = mapBackward(current, step.patches);
    }
    return current;
  }

  /** Forget revisions older than `revision`. */
  prune(revision: number): void {
    this.steps = this.steps.filter((s) => s.revision > revision);
  }
}
