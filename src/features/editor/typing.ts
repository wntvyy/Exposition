// Undo grouping for source-editor edits.
//
// Consecutive typing merges into one undo group (`UndoBehavior::Merge` with a
// shared key). The key rolls over after a pause, a cursor jump (the edit is
// not contiguous with the previous one), a switch between inserting and
// deleting, a line break, or any non-typing edit (paste, multi-cursor, replace).
// Non-typing edits get their own group. An IME composition starts its own
// group, and every composition update (and the final commit, which replaces
// the composed text) merges into it, so a composed word undoes as one step.
import type { TextPatch } from "../../bindings/TextPatch";
import type { UndoBehavior } from "../../bindings/UndoBehavior";

export const TYPING_PAUSE_MS = 1000;

type Kind = "insert" | "delete";

export class UndoGrouper {
  private key: string | null = null;
  private kind: Kind | null = null;
  /** Start of the most recent insertion and the offset just after it. */
  private start = -1;
  private end = -1;
  private last = 0;
  private counter = 0;
  private composing = false;

  /** Start/end an IME composition. */
  setComposing(composing: boolean) {
    this.composing = composing;
    if (composing) {
      this.key = this.nextKey();
      this.kind = "insert";
      this.start = -1;
    }
  }

  /** Break the current group (e.g. after a save, a cursor jump, or an explicit undo). */
  reset() {
    if (this.composing) return;
    this.key = null;
    this.kind = null;
    this.start = -1;
    this.end = -1;
  }

  private nextKey() {
    this.counter += 1;
    return `typing-${this.counter}`;
  }

  /** Classify one local change (patches relative to the text before it). */
  classify(patches: readonly TextPatch[], now: number): UndoBehavior {
    const single = patches.length === 1 ? patches[0] : undefined;
    if (this.key && single) {
      // Replacing exactly the text just inserted: a composition update or commit.
      const replacesLast =
        single.range.start === this.start &&
        single.range.end === this.end &&
        single.range.end > single.range.start &&
        single.text.length > 0;
      if (this.composing || (replacesLast && now - this.last <= TYPING_PAUSE_MS)) {
        this.start = single.range.start;
        this.end = single.range.start + single.text.length;
        this.last = now;
        return { kind: "merge", key: this.key };
      }
    }
    const kind = single ? typingKind(single) : null;
    if (!single || !kind) {
      this.reset();
      return { kind: "newGroup" };
    }
    const contiguous =
      kind === "insert"
        ? single.range.start === this.end
        : // Backspace ends where the previous edit ended; forward delete starts there.
          single.range.end === this.end || single.range.start === this.end;
    const lineBreak = kind === "insert" && /[\r\n]/.test(single.text);
    if (this.key === null || kind !== this.kind || !contiguous || now - this.last > TYPING_PAUSE_MS || lineBreak) {
      this.key = this.nextKey();
    }
    this.kind = kind;
    this.start = single.range.start;
    this.end = kind === "insert" ? single.range.start + single.text.length : single.range.start;
    this.last = now;
    return { kind: "merge", key: this.key };
  }
}

/** Is this patch a single typed character (or auto-closed pair / indented line break) or a small deletion? */
function typingKind(patch: TextPatch): Kind | null {
  const removed = patch.range.end - patch.range.start;
  if (removed === 0 && patch.text.length > 0) {
    if (patch.text.length <= 2) return "insert";
    // Enter with auto-indentation: a line break followed by whitespace only.
    if (/^(\r\n|\n|\r)[ \t]*$/.test(patch.text)) return "insert";
    return null;
  }
  if (patch.text === "" && removed > 0 && removed <= 2) return "delete";
  return null;
}
