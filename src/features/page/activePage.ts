// Which page view of a document was focused last, and where its caret is in
// the document's current revision. Tools that write source at the page caret
// (the equation editor) read the caret here; they never use a position of an
// older revision or of the displayed build. Read-only: nothing here edits.
import type { EditMode } from "../../bindings/EditMode";

/** A page caret expressed in the document's current revision. */
export interface PageCaret {
  /** Caret offset (UTF-16) in revision `revision`. */
  offset: number;
  /** Other end of the selection in the same revision, or null. */
  anchor: number | null;
  revision: number;
  /** How the page interprets typing at the caret (inside math or markup), when known. */
  mode: EditMode | null;
}

export interface PageSurface {
  /** Workspace-relative path of the document the view shows. */
  path: string;
  /** The caret in the current revision; null when there is none or it cannot be mapped there. */
  currentCaret(): PageCaret | null;
  /** Give keyboard focus back to the page (its caret and selection are kept). */
  focus(): void;
}

type Listener = () => void;

const surfaces = new Map<PageSurface, number>();
const listeners = new Set<Listener>();
let clock = 0;

function changed() {
  for (const listener of listeners) listener();
}

/** A page view took focus (or was shown active): it becomes the latest for its document. */
export function notePageFocus(surface: PageSurface): void {
  surfaces.set(surface, ++clock);
  changed();
}

/** A page view went away. */
export function releasePageSurface(surface: PageSurface): void {
  if (surfaces.delete(surface)) changed();
}

/** The most recently focused page view of `path`, with when it was focused (a sequence number). */
export function lastPageSurface(path: string): { surface: PageSurface; focusedAt: number } | null {
  let best: { surface: PageSurface; focusedAt: number } | null = null;
  for (const [surface, focusedAt] of surfaces) {
    if (surface.path === path && (!best || focusedAt > best.focusedAt)) best = { surface, focusedAt };
  }
  return best;
}

/** A sequence number shared with `notePageFocus`, for ordering focus across surfaces. */
export function focusTick(): number {
  return ++clock;
}
