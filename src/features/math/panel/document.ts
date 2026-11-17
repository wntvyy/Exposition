// How the equation panel reaches a document: the shared document handle (one
// Monaco model per document, behind both the source editor and the page
// view). Every write is one edit of that model, which the document client
// sends as ONE source transaction into the document's shared undo history —
// the same path as typing in the source editor, so undo/redo work from either
// surface. Nothing here keeps its own copy of the text.
import type { PageCaret, PageSurface } from "../../page/activePage";
import type { TrackedModel } from "./tracking";

export type Surface = "source" | "page";

/** The editable model of a document (a Monaco text model; tests use a fake). */
export interface EditableModel extends TrackedModel {
  getPositionAt(offset: number): { lineNumber: number; column: number };
  applyEdits(
    operations: {
      range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number };
      text: string;
    }[],
  ): unknown;
}

/** The parts of a `DocumentHandle` the panel uses. */
export interface EquationDocument {
  readonly id: string;
  readonly path: string | null;
  readonly name: string;
  readonly closed: boolean;
  /** Why no view may edit the document (read-only, mixed line endings), or null. */
  readonly editReason: string | null;
  readonly model: EditableModel;
  readonly sync: { revision: number; pendingCount: number; isResyncing: boolean; isHolding: boolean; idle(): Promise<void> };
  readonly grouper: { reset(): void };
  /** Selection restored around undo/redo of the next local edit. */
  selection: { start: number; end: number }[];
}

/** A text replacement in the model's current text. */
export interface Patch {
  start: number;
  end: number;
  text: string;
}

/** Where an equation operation acts: a caret/selection in a document's current text. */
export interface CaretTarget {
  surface: Surface;
  document: EquationDocument;
  path: string;
  /** Selection in the model's current text (start <= end; equal for a caret). */
  start: number;
  end: number;
  /** Give focus back to the surface, with its caret at `offset` when given (source only). */
  focus(offset?: number): void;
}

export type Resolved = CaretTarget | { error: string };

/** Why the document cannot take a write right now, or null. */
export function writeBlocker(document: EquationDocument): string | null {
  if (document.closed || document.model.isDisposed()) return `${document.name} was closed.`;
  if (document.editReason) return document.editReason;
  if (document.sync.isResyncing) return `${document.name} is refreshing from the document service. Try again in a moment.`;
  // Text composition (IME) in progress in a source editor of this document.
  if (document.sync.isHolding) return "Finish the text being composed in the source editor first.";
  return null;
}

/**
 * Apply `patches` (non-overlapping, offsets in the model's current text) as
 * one edit of the shared model: one transaction, its own undo step. The undo
 * selection is `selection` (the edited range), whichever surface asked.
 */
export function writePatches(document: EquationDocument, patches: Patch[], selection: { start: number; end: number }): void {
  const blocker = writeBlocker(document);
  if (blocker) throw new Error(blocker);
  const sorted = [...patches].sort((a, b) => a.start - b.start);
  for (let i = 1; i < sorted.length; i++) {
    if (sorted[i]!.start < sorted[i - 1]!.end || sorted[i]!.start === sorted[i - 1]!.start) {
      throw new Error("Overlapping equation edits");
    }
  }
  const model = document.model;
  const operations = sorted.map((p) => {
    const a = model.getPositionAt(p.start);
    const b = model.getPositionAt(p.end);
    return {
      range: { startLineNumber: a.lineNumber, startColumn: a.column, endLineNumber: b.lineNumber, endColumn: b.column },
      text: p.text,
    };
  });
  // A new undo group: never merged into the typing before or after it.
  document.grouper.reset();
  document.selection = [selection];
  model.applyEdits(operations);
  document.grouper.reset();
}

/**
 * The caret of a page view as offsets of the document model's text. Page
 * carets live in document revisions; the model may still have unacknowledged
 * local edits or lag behind the page's revision, so the two must agree
 * exactly (waiting once for the model to settle). Never an approximate or
 * older-revision position: when they cannot agree, the caller is told why.
 */
export async function resolvePageCaret(surface: PageSurface, document: EquationDocument): Promise<{ caret: PageCaret } | { error: string }> {
  const agrees = (caret: PageCaret | null): caret is PageCaret =>
    caret !== null && !document.sync.isResyncing && document.sync.pendingCount === 0 && document.sync.revision === caret.revision;
  let caret = surface.currentCaret();
  if (agrees(caret)) return { caret };
  if (!caret) {
    return { error: "Place the caret on the page first (click in the text or an equation)." };
  }
  await document.sync.idle();
  caret = surface.currentCaret();
  if (agrees(caret)) return { caret };
  return {
    error: caret
      ? "The page has not caught up with the latest change to the source yet. Try again in a moment."
      : "The page caret could not be placed in the current source. Click in the page again.",
  };
}
