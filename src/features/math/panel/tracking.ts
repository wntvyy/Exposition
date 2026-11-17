// Target tracking for an open equation draft.
//
// A draft remembers the source range it will replace (an equation, or the
// caret/selection where a new one goes) and the exact text that was there.
// The range follows every later change of the document's shared editor model
// — the user's typing, page edits, agent and external changes, undo/redo —
// because every one of them reaches that model. A change that touches the
// range (or, for an empty insertion point, happens exactly at it) marks the
// target as conflicted; so does a full replacement of the model text. Before
// a write, `verify` re-checks that the range still holds exactly the original
// text. A conflicted target is never written: the draft stays and the user
// re-targets or copies it.

/** One content change, offsets relative to the text before the change event. */
export interface ContentChange {
  rangeOffset: number;
  rangeLength: number;
  text: string;
}

/** The subset of a Monaco text model the tracker needs (tests use a fake). */
export interface TrackedModel {
  onDidChangeContent(listener: (event: { changes: readonly ContentChange[]; isFlush: boolean }) => void): { dispose(): void };
  getValue(): string;
  isDisposed(): boolean;
}

export type TargetProblem =
  | { kind: "changed" }
  | { kind: "replaced" }
  | { kind: "closed" };

export class TrackedRange {
  private startOffset: number;
  private endOffset: number;
  private problem: TargetProblem | null = null;
  private readonly subscription: { dispose(): void };
  /** Called once, when the target first becomes conflicted by a change. */
  onProblem: ((problem: TargetProblem) => void) | null = null;

  constructor(
    private readonly model: TrackedModel,
    start: number,
    end: number,
    /** The source text of [start, end) when the draft opened. */
    readonly original: string,
  ) {
    this.startOffset = start;
    this.endOffset = end;
    this.subscription = model.onDidChangeContent((event) => this.onChange(event.changes, event.isFlush));
  }

  get start(): number {
    return this.startOffset;
  }

  get end(): number {
    return this.endOffset;
  }

  /** Why the range can no longer be written (null while it is intact). */
  get conflict(): TargetProblem | null {
    if (!this.problem && this.model.isDisposed()) return { kind: "closed" };
    return this.problem;
  }

  /**
   * The range is intact and holds exactly the original text in `text` (the
   * model's current value). Returns the problem otherwise.
   */
  verify(text: string = this.model.isDisposed() ? "" : this.model.getValue()): TargetProblem | null {
    const problem = this.conflict;
    if (problem) return problem;
    if (this.endOffset > text.length || text.slice(this.startOffset, this.endOffset) !== this.original) {
      this.fail({ kind: "changed" });
      return this.problem;
    }
    return null;
  }

  dispose(): void {
    this.subscription.dispose();
    this.onProblem = null;
  }

  private fail(problem: TargetProblem): void {
    if (this.problem) return;
    this.problem = problem;
    this.onProblem?.(problem);
  }

  private onChange(changes: readonly ContentChange[], isFlush: boolean): void {
    if (isFlush) {
      this.fail({ kind: "replaced" });
      return;
    }
    const sorted = [...changes].sort((a, b) => a.rangeOffset - b.rangeOffset);
    const empty = this.startOffset === this.endOffset;
    let shiftStart = 0;
    let shiftEnd = 0;
    let touched = false;
    for (const change of sorted) {
      const from = change.rangeOffset;
      const to = from + change.rangeLength;
      const delta = change.text.length - change.rangeLength;
      const touches = empty
        ? // An insertion point: any change at it or across it is ambiguous.
          from <= this.startOffset && to >= this.startOffset
        : // A range: any change overlapping its inside.
          from < this.endOffset && to > this.startOffset;
      if (touches) {
        // The position stays approximate (for re-targeting only; never written to).
        touched = true;
        continue;
      }
      if (to <= this.startOffset) {
        shiftStart += delta;
        shiftEnd += delta;
      }
      // A change after the range moves nothing.
    }
    this.startOffset = Math.max(0, this.startOffset + shiftStart);
    this.endOffset = Math.max(this.startOffset, this.endOffset + shiftEnd);
    if (touched) this.fail({ kind: "changed" });
  }
}
