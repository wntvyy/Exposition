// The equation draft: one open equation being built or edited in the panel,
// bound to a target in one document.
//
// - Opening records the document, the source range the draft will replace
//   (the whole equation for an edit; the caret or selection for a new one),
//   and the exact original text there, and tracks that range through every
//   later change of the shared model (`TrackedRange`).
// - The math tree is only an editing representation: nothing is written until
//   Apply, which re-checks the target and writes ONE edit (the equation's
//   content, plus an explicitly chosen requirement line such as
//   \usepackage{amssymb}) through the document handle.
// - Cancel, an unchanged edit, and an empty new equation write nothing. A
//   changed or closed target is a recoverable conflict: nothing is written and
//   the draft stays until the user re-targets, copies, or discards it.
// - A draft belongs to its document: switching documents keeps it, and it is
//   only moved by an explicit re-target.
import {
  adapterFor,
  createState,
  insertSymbol,
  previewLatex,
  sameMath,
  withContentLayout,
  type EditorState,
  type EquationRange,
  type MathLanguage,
  type MathRow,
  type Requirement,
  type SerializeResult,
  type SourceContext,
  type SymbolId,
} from "../core";
import { languageForPath } from "../core";
import { writeBlocker, writePatches, type CaretTarget, type EquationDocument, type Patch, type Resolved, type Surface } from "./document";
import { TrackedRange, type TargetProblem } from "./tracking";
import { isEmptyRow, LANGUAGE_LABELS, opaqueNodes } from "./tree";

/** How an entry point asks for an equation. `auto`: edit the equation at the caret, else insert an inline one. */
export type OpenMode = "auto" | "inline" | "display" | "edit";

export interface Notice {
  tone: "info" | "warning";
  text: string;
}

export interface Draft {
  serial: number;
  documentId: string;
  path: string;
  name: string;
  language: MathLanguage;
  surface: Surface;
  /** `edit`: replaces an existing equation's content; `insert`: a new equation or math at the caret. */
  kind: "edit" | "insert";
  /** `math`: inserted into an existing equation without delimiters; `text`: a new inline/display construct. */
  context: "math" | "text";
  display: boolean;
  /** The edited equation (offsets of the revision the draft opened on). */
  equation: EquationRange | null;
  /** 1-based line of the target when the draft opened. */
  line: number;
  editor: EditorState;
  /** The tree the draft started from (unchanged drafts are not written). */
  initial: MathRow;
  /** The equation parsed without opaque (unsupported) segments. */
  complete: boolean;
  /** Add missing requirements (e.g. \usepackage lines) in the same edit. */
  addRequirements: boolean;
  /** Why the target can no longer be written (the draft is kept). */
  conflict: TargetProblem | null;
  past: EditorState[];
  future: EditorState[];
}

export interface PanelState {
  draft: Draft | null;
  /** The latest message for the user (a refusal, a no-op), cleared by the next action. */
  notice: Notice | null;
}

/** Derived facts about a draft for display and Apply. */
export interface DraftAnalysis {
  serialized: SerializeResult | null;
  /** Serialization failed (a core error). */
  error: string | null;
  /** Requirements the document does not satisfy yet. */
  missing: { requirement: Requirement; addable: boolean }[];
  /** Applying would write nothing (an unchanged edit or an empty new equation). */
  unchanged: boolean;
  /** LaTeX for the approximate local preview ("" when unavailable). */
  preview: string;
  /** Reasons for opaque segments (kept exactly as written). */
  opaque: string[];
}

export interface DraftEnvironment {
  /** The caret/selection of the active surface (or of `preferred`), in the model's current text. */
  resolve(preferred?: Surface): Promise<Resolved>;
  /** The open document with this id, if it is still open. */
  document(id: string): EquationDocument | undefined;
  /** Show the Equation panel. */
  showPanel(): void;
  copy(text: string): Promise<boolean>;
}

export type ApplyOutcome = "written" | "unchanged" | "conflict" | "blocked";

const HISTORY_LIMIT = 200;

const lineAt = (text: string, offset: number): number => {
  let line = 1;
  for (let i = 0; i < offset && i < text.length; i++) if (text.charCodeAt(i) === 10) line += 1;
  return line;
};

const errorText = (error: unknown): string => (error instanceof Error ? error.message : String(error));

export const requirementLabel = (requirement: Requirement): string => `\\usepackage{${requirement.name}}`;

const otherDocumentNotice = (draft: Draft): Notice => ({
  tone: "info",
  text: `The open draft belongs to ${draft.name}. Apply or discard it, or move it to the caret, before inserting here.`,
});

/** An odd number of backslashes ends right before `offset` (the next character is escaped). */
export function escapedAt(text: string, offset: number): boolean {
  let count = 0;
  for (let i = offset - 1; i >= 0 && text.charCodeAt(i) === 92; i--) count++;
  return count % 2 === 1;
}

/** The caret follows an unescaped `^` or `_`: it is the start of a script argument. */
export function inScriptPosition(text: string, offset: number): boolean {
  const ch = text[offset - 1];
  return (ch === "^" || ch === "_") && !escapedAt(text, offset - 1);
}

/** Unbalanced `{}` (TeX) or `()` / `[]` (Typst) in a piece of math source. */
function unbalanced(language: MathLanguage, source: string): boolean {
  const pairs: [string, string][] = language === "typst" ? [["(", ")"], ["[", "]"]] : [["{", "}"]];
  for (const [open, close] of pairs) {
    let depth = 0;
    for (let i = 0; i < source.length; i++) {
      if (language !== "typst" && escapedAt(source, i)) continue;
      if (source[i] === open) depth++;
      else if (source[i] === close && --depth < 0) return true;
    }
    if (depth !== 0) return true;
  }
  return false;
}

/**
 * A selection inside equation source that cuts a token: part of a control
 * word (`pha+\b` in `\alpha+\beta`), an escaped character, a Typst name, a
 * script argument, or half of a group. Such a selection cannot become a
 * draft without changing what the source means.
 */
export function cutsToken(language: MathLanguage, text: string, start: number, end: number): boolean {
  const before = text.slice(Math.max(0, start - 64), start);
  const selected = text.slice(start, end);
  const after = text.slice(end, end + 1);
  if (unbalanced(language, selected) || inScriptPosition(text, start)) return true;
  if (language === "typst") {
    return (/[\p{L}\p{N}]$/u.test(before) && /^[\p{L}\p{N}]/u.test(selected)) || (/[\p{L}\p{N}]$/u.test(selected) && /^[\p{L}\p{N}]/u.test(after));
  }
  return (
    escapedAt(text, start) ||
    escapedAt(text, end) ||
    (/\\[A-Za-z]*$/.test(before) && /^[A-Za-z]/.test(selected)) ||
    (/\\[A-Za-z]*$/.test(selected) && /^[A-Za-z]/.test(after))
  );
}

/**
 * Fit math inserted into existing equation source to its neighbours: a space
 * where tokens would fuse (`\alpha` + `x` reads as `\alphax` in TeX, `x` +
 * `alpha` as the name `xalpha` in Typst), and a group in a script position
 * (`x^` + `ab` must be `x^{ab}` / Typst `x^(a b)`).
 */
export function fitToContext(language: MathLanguage, text: string, start: number, end: number, source: string): string {
  if (!source) return source;
  if (inScriptPosition(text, start) && !/^[\p{L}\p{N}]$/u.test(source)) {
    return language === "typst" ? `(${source})` : `{${source}}`;
  }
  const before = text.slice(Math.max(0, start - 64), start);
  const after = text.slice(end, end + 1);
  const fusesBefore =
    language === "typst" ? /[\p{L}\p{N}.]$/u.test(before) && /^[\p{L}\p{N}]/u.test(source) : /\\[A-Za-z]+$/.test(before) && /^[A-Za-z]/.test(source);
  const fusesAfter =
    language === "typst" ? /[\p{L}\p{N}]$/u.test(source) && /^[\p{L}\p{N}]/u.test(after) : /\\[A-Za-z]+$/.test(source) && /^[A-Za-z]/.test(after);
  return `${fusesBefore ? " " : ""}${source}${fusesAfter ? " " : ""}`;
}

/** A draft built for a target, with what tracks that target. */
interface Opened {
  draft: Draft;
  /** The range the draft replaces. */
  tracked: TrackedRange;
  /** Ranges that must stay unchanged for the target to stay valid (the equation's delimiters). */
  guards: TrackedRange[];
  notice: Notice | null;
}

/** Delay of the context re-check after document changes (Apply always re-checks at once). */
const CONTEXT_CHECK_MS = 120;

const ESCAPED_CARET = "The caret follows a backslash, which would escape what is inserted. Move the caret or remove the backslash first.";
const CUT_SELECTION = "The selection cuts through a command, name, script, or group. Select whole symbols, or edit the whole equation.";

export class EquationPanelStore {
  private state: PanelState = { draft: null, notice: null };
  private readonly listeners = new Set<() => void>();
  private serial = 0;
  private tracked: TrackedRange | null = null;
  private guards: TrackedRange[] = [];
  private watch: { dispose(): void } | null = null;
  private contextTimer: ReturnType<typeof setTimeout> | null = null;
  private target: CaretTarget | null = null;

  constructor(private readonly env: DraftEnvironment) {}

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getState = (): PanelState => this.state;

  private set(patch: Partial<PanelState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }

  private setDraft(patch: Partial<Draft>): void {
    if (this.state.draft) this.set({ draft: { ...this.state.draft, ...patch } });
  }

  notify(notice: Notice | null): void {
    this.set({ notice });
  }

  // ---- opening -------------------------------------------------------------

  /** Open a draft at the active surface's caret. A changed draft is never replaced silently. */
  async open(mode: OpenMode, preferred?: Surface): Promise<boolean> {
    this.env.showPanel();
    const current = this.state.draft;
    if (current && this.isDirty(current)) {
      this.set({
        notice: {
          tone: "info",
          text: `Apply or discard the open draft for ${current.name} (line ${current.line}) first, or move it to the caret.`,
        },
      });
      return false;
    }
    const resolved = await this.env.resolve(preferred);
    if ("error" in resolved) {
      this.set({ notice: { tone: "info", text: resolved.error } });
      return false;
    }
    const opened = this.draftAt(resolved, mode, null);
    if ("error" in opened) {
      this.set({ notice: { tone: "info", text: opened.error } });
      return false;
    }
    this.install(opened, resolved);
    return true;
  }

  /**
   * Insert one catalog symbol at the active surface's caret without opening a
   * draft (inside math: the symbol; in text: a new inline equation holding
   * it). A symbol that needs document configuration, or that the language
   * cannot express, opens a draft instead so the user sees and decides.
   * With a draft open, the symbol goes into the draft at its caret — but only
   * when the caret is in the draft's own document: a caret in another
   * document never adds to that draft (move the draft first).
   */
  async insertSymbolAtCaret(id: SymbolId, preferred?: Surface): Promise<ApplyOutcome | "draft"> {
    const resolved = await this.env.resolve(preferred);
    const open = this.state.draft;
    if (open) {
      if ("error" in resolved || resolved.document.id !== open.documentId) {
        this.set({ notice: otherDocumentNotice(open) });
        return "blocked";
      }
      this.edit((s) => insertSymbol(s, id));
      return "draft";
    }
    if ("error" in resolved) {
      this.set({ notice: { tone: "info", text: resolved.error } });
      return "blocked";
    }
    const opened = this.draftAt(resolved, "inline", null);
    if ("error" in opened) {
      this.set({ notice: { tone: "info", text: opened.error } });
      return "blocked";
    }
    if (opened.draft.kind === "edit") {
      // The caret was at a script: show the whole equation instead of guessing.
      this.install(opened, resolved);
      return "draft";
    }
    this.install({ ...opened, draft: { ...opened.draft, editor: insertSymbol(opened.draft.editor, id) } }, resolved);
    const analysis = this.analyze();
    if (analysis && !analysis.error && analysis.missing.length === 0 && analysis.serialized?.unsupported.length === 0) {
      return this.apply();
    }
    this.env.showPanel();
    return "draft";
  }

  private install(opened: Opened, target: CaretTarget): void {
    this.release();
    this.tracked = opened.tracked;
    this.guards = opened.guards;
    this.target = target;
    // Re-check the target's context shortly after every document change, so a
    // conflict shows before Apply is tried (Apply re-checks at once anyway).
    const serial = opened.draft.serial;
    const model = target.document.model;
    const subscription = model.onDidChangeContent(() => {
      if (this.contextTimer) clearTimeout(this.contextTimer);
      this.contextTimer = setTimeout(() => {
        this.contextTimer = null;
        const draft = this.state.draft;
        if (draft?.serial !== serial || draft.conflict || model.isDisposed()) return;
        const problem = this.contextProblem(draft, model.getValue());
        if (problem) this.setDraft({ conflict: problem });
      }, CONTEXT_CHECK_MS);
    });
    this.watch = subscription;
    this.set({ draft: opened.draft, notice: opened.notice });
  }

  /** Stop tracking the current target. */
  private release(): void {
    this.tracked?.dispose();
    for (const guard of this.guards) guard.dispose();
    this.watch?.dispose();
    if (this.contextTimer) clearTimeout(this.contextTimer);
    this.tracked = null;
    this.guards = [];
    this.watch = null;
    this.contextTimer = null;
  }

  /**
   * Whether the target is still the same place in the CURRENT text, read
   * again with the language adapter: an edit's range must still be exactly
   * an equation with the same delimiters; an insertion point must still be
   * in text, or inside the same equation (same delimiter positions), and not
   * escaped by a backslash typed before it.
   */
  private contextProblem(draft: Draft, text: string): TargetProblem | null {
    const tracked = this.tracked;
    if (!tracked) return { kind: "changed" };
    for (const range of [tracked, ...this.guards]) {
      const problem = range.check(text);
      if (problem) return problem;
    }
    let adapter;
    try {
      adapter = adapterFor(draft.language);
    } catch {
      return { kind: "context" };
    }
    const context: TargetProblem = { kind: "context" };
    try {
      if (draft.kind === "edit") {
        const eq = draft.equation;
        const found = adapter.equations(text).find((e) => e.start === tracked.start && e.end === tracked.end);
        if (
          !eq ||
          !found ||
          found.style !== eq.style ||
          found.contentStart - found.start !== eq.contentStart - eq.start ||
          found.end - found.contentEnd !== eq.end - eq.contentEnd
        ) {
          return context;
        }
        return null;
      }
      if (escapedAt(text, tracked.start)) return context;
      const points = tracked.start === tracked.end ? [tracked.start] : [tracked.start, tracked.end];
      for (const point of points) {
        const at = adapter.contextAt(text, point);
        if (draft.context === "text") {
          if (at.kind !== "text") return context;
          continue;
        }
        const [open, close] = this.guards;
        const e = at.kind === "math" ? at.equation : null;
        if (
          !e ||
          !open ||
          !close ||
          e.style !== draft.equation?.style ||
          e.start !== open.start ||
          e.contentStart !== open.end ||
          e.contentEnd !== close.start ||
          e.end !== close.end
        ) {
          return context;
        }
      }
      return null;
    } catch {
      return context;
    }
  }

  /** Build a draft for a caret target (and optionally keep `keep`'s editing state). */
  private draftAt(target: CaretTarget, mode: OpenMode, keep: Draft | null): Opened | { error: string } {
    const path = target.path;
    const language = languageForPath(path);
    if (!language) return { error: "Equations can be edited in LaTeX, Typst, and Markdown documents." };
    const document = target.document;
    const blocker = writeBlocker(document);
    if (blocker) return { error: blocker };
    let adapter;
    try {
      adapter = adapterFor(language);
    } catch (error) {
      return { error: `Equations are not available for ${LANGUAGE_LABELS[language]}: ${errorText(error)}` };
    }
    const text = document.model.getValue();
    const { start, end } = target;
    let context: SourceContext;
    let endContext: SourceContext;
    try {
      context = adapter.contextAt(text, start);
      endContext = start === end ? context : adapter.contextAt(text, end);
    } catch (error) {
      return { error: errorText(error) };
    }
    if (context.kind === "unavailable") return { error: `No equation can go here: ${context.reason}.` };
    if (endContext.kind === "unavailable") return { error: `No equation can go here: ${endContext.reason}.` };

    const equation = context.kind === "math" ? context.equation : null;
    let editWhole = mode === "edit" || (mode === "auto" && equation !== null);
    if (mode === "edit" && !equation) return { error: "The caret is not inside an equation." };
    let notice: Notice | null = null;
    if (!editWhole) {
      // Never insert where the source would read it differently.
      if (escapedAt(text, start)) return { error: ESCAPED_CARET };
      if (equation && start !== end && cutsToken(language, text, start, end)) return { error: CUT_SELECTION };
      // In front of an existing script argument (`x^|2`) an insertion would
      // change which part is the script: edit the whole equation instead.
      if (equation && start === end && inScriptPosition(text, start) && /^[\p{L}\p{N}\\{("]/u.test(text.slice(start, start + 1))) {
        editWhole = true;
        notice = { tone: "info", text: "The caret is at a script; the whole equation is open for editing." };
      }
    }

    const base = {
      serial: ++this.serial,
      documentId: document.id,
      path,
      name: document.name,
      language,
      surface: target.surface,
      addRequirements: keep?.addRequirements ?? true,
      conflict: null,
      past: [],
      future: [],
    };

    if (editWhole && equation) {
      const content = text.slice(equation.contentStart, equation.contentEnd);
      let parsed;
      try {
        parsed = adapter.parse(content);
      } catch (error) {
        return { error: `This equation could not be read: ${errorText(error)}` };
      }
      const initial = parsed.row;
      const tracked = new TrackedRange(document.model, equation.start, equation.end, text.slice(equation.start, equation.end));
      tracked.onProblem = (problem) => this.onTargetProblem(base.serial, problem);
      return {
        tracked,
        guards: [],
        notice,
        draft: {
          ...base,
          kind: "edit",
          context: "math",
          display: equation.display,
          equation,
          line: lineAt(text, equation.start),
          initial,
          complete: parsed.complete,
          editor: keep ? keep.editor : createState(initial),
        },
      };
    }

    // A new equation (or math inserted into an existing one) at the caret/selection.
    if (start !== end) {
      const sameEquation =
        context.kind === endContext.kind &&
        (context.kind !== "math" ||
          (endContext.kind === "math" && endContext.equation.contentStart === context.equation.contentStart));
      if (!sameEquation) return { error: "The selection crosses an equation boundary. Select inside one equation or only text." };
    }
    const selected = text.slice(start, end);
    let initial: MathRow = { kind: "row", children: [] };
    let complete = true;
    if (selected) {
      try {
        const parsed = adapter.parse(selected);
        initial = parsed.row;
        complete = parsed.complete;
      } catch (error) {
        return { error: `The selection could not be read as mathematics: ${errorText(error)}` };
      }
    }
    const tracked = new TrackedRange(document.model, start, end, selected);
    tracked.onProblem = (problem) => this.onTargetProblem(base.serial, problem);
    // Inside math, the equation's delimiters must stay where and what they were.
    const guards = equation
      ? [
          new TrackedRange(document.model, equation.start, equation.contentStart, text.slice(equation.start, equation.contentStart)),
          new TrackedRange(document.model, equation.contentEnd, equation.end, text.slice(equation.contentEnd, equation.end)),
        ]
      : [];
    for (const guard of guards) guard.onProblem = (problem) => this.onTargetProblem(base.serial, problem);
    return {
      tracked,
      guards,
      notice,
      draft: {
        ...base,
        kind: "insert",
        context: context.kind === "math" ? "math" : "text",
        display: context.kind === "math" ? context.equation.display : (keep?.display ?? mode === "display"),
        equation,
        line: lineAt(text, start),
        initial,
        complete,
        editor: keep ? keep.editor : createState(initial),
      },
    };
  }

  private onTargetProblem(serial: number, problem: TargetProblem): void {
    if (this.state.draft?.serial === serial && !this.state.draft.conflict) this.setDraft({ conflict: problem });
  }

  // ---- editing ---------------------------------------------------------------

  /** Apply one editing operation of the core to the draft (with draft-level undo). */
  edit(operation: (state: EditorState) => EditorState): void {
    const draft = this.state.draft;
    if (!draft) return;
    let next: EditorState;
    try {
      next = operation(draft.editor);
    } catch (error) {
      this.set({ notice: { tone: "warning", text: errorText(error) } });
      return;
    }
    if (next === draft.editor) return;
    const contentChanged = next.root !== draft.editor.root;
    this.set({
      notice: null,
      draft: {
        ...draft,
        editor: next,
        // Caret moves are not undo steps.
        past: contentChanged ? [...draft.past, draft.editor].slice(-HISTORY_LIMIT) : draft.past,
        future: contentChanged ? [] : draft.future,
      },
    });
  }

  undo(): void {
    const draft = this.state.draft;
    const previous = draft?.past.at(-1);
    if (!draft || !previous) return;
    this.setDraft({ editor: previous, past: draft.past.slice(0, -1), future: [draft.editor, ...draft.future] });
  }

  redo(): void {
    const draft = this.state.draft;
    const next = draft?.future[0];
    if (!draft || !next) return;
    this.setDraft({ editor: next, past: [...draft.past, draft.editor], future: draft.future.slice(1) });
  }

  setDisplay(display: boolean): void {
    const draft = this.state.draft;
    if (draft?.kind === "insert" && draft.context === "text") this.setDraft({ display });
  }

  setAddRequirements(add: boolean): void {
    this.setDraft({ addRequirements: add });
  }

  isDirty(draft: Draft): boolean {
    try {
      return !sameMath(draft.editor.root, draft.initial);
    } catch {
      return true;
    }
  }

  // ---- analysis --------------------------------------------------------------

  analyze(draft: Draft | null = this.state.draft): DraftAnalysis | null {
    if (!draft) return null;
    const opaque = opaqueNodes(draft.editor.root).map((n) => n.reason);
    let serialized: SerializeResult | null = null;
    let error: string | null = null;
    let preview = "";
    const missing: DraftAnalysis["missing"] = [];
    try {
      const adapter = adapterFor(draft.language);
      serialized = adapter.serialize(draft.editor.root);
      const document = this.env.document(draft.documentId);
      const text = document && !document.model.isDisposed() ? document.model.getValue() : null;
      const seen = new Set<string>();
      for (const requirement of serialized.requirements) {
        const key = `${requirement.kind}:${requirement.name}`;
        if (seen.has(key) || text === null || adapter.satisfied(text, requirement)) continue;
        seen.add(key);
        missing.push({ requirement, addable: adapter.addRequirement(text, requirement) !== null });
      }
    } catch (e) {
      error = errorText(e);
    }
    try {
      preview = previewLatex(draft.editor.root);
    } catch {
      preview = "";
    }
    // Nothing to write: an unchanged edit, an empty new equation, or math
    // selected inside an equation and left as it was (never re-serialized).
    const unchanged =
      draft.kind === "edit"
        ? !this.isDirty(draft)
        : isEmptyRow(draft.editor.root) || (draft.context === "math" && !isEmptyRow(draft.initial) && !this.isDirty(draft));
    return { serialized, error, missing, unchanged, preview, opaque };
  }

  // ---- apply / cancel / re-target -----------------------------------------------

  /** Write the draft to its target as one edit (or nothing; see the module comment). */
  async apply(): Promise<ApplyOutcome> {
    const draft = this.state.draft;
    const tracked = this.tracked;
    if (!draft || !tracked) return "blocked";
    const analysis = this.analyze(draft)!;
    if (analysis.unchanged) {
      // Nothing to write: close quietly (an unchanged equation is never reformatted).
      this.close(
        draft.kind === "insert" && isEmptyRow(draft.editor.root) ? { tone: "info", text: "Nothing to insert: the equation is empty." } : null,
      );
      return "unchanged";
    }
    if (analysis.error || !analysis.serialized) {
      this.set({ notice: { tone: "warning", text: `This draft cannot be written: ${analysis.error ?? "no source"}` } });
      return "blocked";
    }
    if (analysis.serialized.unsupported.length > 0) {
      this.set({
        notice: {
          tone: "warning",
          text: `${LANGUAGE_LABELS[draft.language]} cannot express part of this draft; remove it before applying.`,
        },
      });
      return "blocked";
    }
    const document = this.env.document(draft.documentId);
    if (!document || document.closed || document.model.isDisposed()) {
      this.setDraft({ conflict: { kind: "closed" } });
      return "conflict";
    }
    const blocker = writeBlocker(document);
    if (blocker) {
      this.set({ notice: { tone: "warning", text: blocker } });
      return "blocked";
    }
    const text = document.model.getValue();
    const problem = this.contextProblem(draft, text);
    if (problem) {
      this.setDraft({ conflict: problem });
      return "conflict";
    }
    let adapter;
    try {
      adapter = adapterFor(draft.language);
    } catch (error) {
      this.set({ notice: { tone: "warning", text: errorText(error) } });
      return "blocked";
    }
    const source = analysis.serialized.source;
    let main: Patch;
    if (draft.kind === "edit" && draft.equation) {
      // Only the content between the delimiters changes; the delimiters,
      // environment, labels outside it, and everything else stay byte-for-byte,
      // and so does the whitespace/indentation around the content.
      const eq = draft.equation;
      const start = tracked.start + (eq.contentStart - eq.start);
      const end = tracked.end - (eq.end - eq.contentEnd);
      main = { start, end, text: withContentLayout(text.slice(start, end), source) };
    } else if (draft.context === "math") {
      main = { start: tracked.start, end: tracked.end, text: fitToContext(draft.language, text, tracked.start, tracked.end, source) };
    } else {
      // A selection made into an equation unchanged keeps its exact source.
      const content = tracked.original && !this.isDirty(draft) ? tracked.original : source;
      main = { start: tracked.start, end: tracked.end, text: adapter.wrap(content, draft.display) };
    }
    const patches: Patch[] = [main];
    if (draft.addRequirements) {
      const byOffset = new Map<number, string>();
      for (const { requirement } of analysis.missing) {
        const addition = adapter.addRequirement(text, requirement);
        if (!addition) continue;
        if (addition.offset > main.start && addition.offset < main.end) {
          this.set({ notice: { tone: "warning", text: `${requirementLabel(requirement)} would go inside the equation; add it yourself.` } });
          return "blocked";
        }
        byOffset.set(addition.offset, (byOffset.get(addition.offset) ?? "") + addition.insert);
      }
      for (const [offset, insert] of byOffset) {
        if (offset === main.start && main.start === main.end) {
          // Same insertion point as the equation: one patch keeps the order explicit.
          main.text = insert + main.text;
        } else {
          patches.push({ start: offset, end: offset, text: insert });
        }
      }
    }
    // The target is verified and the patches are computed from the same
    // text: write now, as one edit, before anything else can change it.
    // Stop tracking first: the write itself changes the target.
    const guards = this.guards.map((g) => ({ start: g.start, end: g.end, original: g.original }));
    const target = this.target;
    this.release();
    try {
      writePatches(document, patches, { start: main.start, end: main.end }, draft.surface);
    } catch (error) {
      // Nothing was written (the model refuses invalid edits before changing): keep tracking.
      const again = (r: { start: number; end: number; original: string }) => {
        const range = new TrackedRange(document.model, r.start, r.end, r.original);
        range.onProblem = (p) => this.onTargetProblem(draft.serial, p);
        return range;
      };
      if (target) this.install({ draft, tracked: again(tracked), guards: guards.map(again), notice: null }, target);
      this.set({ notice: { tone: "warning", text: `The equation was not written: ${errorText(error)}` } });
      return "blocked";
    }
    const shift = patches.filter((p) => p.start < main.start).reduce((n, p) => n + p.text.length - (p.end - p.start), 0);
    const after = main.start + shift + main.text.length;
    this.close(null, false);
    target?.focus(after);
    return "written";
  }

  /** Discard the draft (nothing is written). */
  cancel(): void {
    const target = this.target;
    const sameDocument = target && this.state.draft && target.document.id === this.state.draft.documentId;
    this.close(null);
    if (sameDocument) target.focus();
  }

  private close(notice: Notice | null, refocus = false): void {
    const target = this.target;
    this.release();
    this.target = null;
    this.set({ draft: null, notice });
    if (refocus) target?.focus();
  }

  /**
   * Move the draft to the active surface's caret (the equation there, or a
   * new one there), keeping what the user built. Used after a conflict or a
   * document switch; it is always an explicit user action.
   */
  async retargetToCaret(preferred?: Surface): Promise<boolean> {
    const draft = this.state.draft;
    if (!draft) return false;
    const resolved = await this.env.resolve(preferred);
    if ("error" in resolved) {
      this.set({ notice: { tone: "info", text: resolved.error } });
      return false;
    }
    const opened = this.draftAt(resolved, "auto", draft);
    if ("error" in opened) {
      this.set({ notice: { tone: "info", text: opened.error } });
      return false;
    }
    if (opened.draft.language !== draft.language) {
      opened.tracked.dispose();
      for (const guard of opened.guards) guard.dispose();
      this.set({
        notice: { tone: "info", text: `This draft is ${LANGUAGE_LABELS[draft.language]}; the caret is in a ${LANGUAGE_LABELS[opened.draft.language]} document.` },
      });
      return false;
    }
    this.install({ ...opened, draft: { ...opened.draft, past: draft.past, future: draft.future } }, resolved);
    return true;
  }

  /**
   * After a conflict in the same document: re-target the draft to the
   * equation now at (or around) the draft's former position, keeping the
   * draft's math. The new original text is what is there now.
   */
  retargetToCurrentEquation(): boolean {
    const draft = this.state.draft;
    const tracked = this.tracked;
    const document = draft ? this.env.document(draft.documentId) : undefined;
    if (!draft || !tracked || !document || writeBlocker(document)) return false;
    const found = this.currentEquation();
    if (!found) {
      this.set({ notice: { tone: "info", text: "No equation is at the draft's former position any more." } });
      return false;
    }
    const target: CaretTarget = {
      surface: draft.surface,
      document,
      path: draft.path,
      start: found.contentStart,
      end: found.contentStart,
      focus: this.target?.focus ?? (() => {}),
    };
    const opened = this.draftAt(target, "edit", draft);
    if ("error" in opened) {
      this.set({ notice: { tone: "info", text: opened.error } });
      return false;
    }
    this.install({ ...opened, draft: { ...opened.draft, past: draft.past, future: draft.future } }, target);
    return true;
  }

  /** The equation now at the conflicted draft's former position, if any. */
  currentEquation(): EquationRange | null {
    const draft = this.state.draft;
    const tracked = this.tracked;
    const document = draft ? this.env.document(draft.documentId) : undefined;
    if (!draft || !tracked || !document || document.model.isDisposed()) return null;
    try {
      const text = document.model.getValue();
      const at = Math.min(tracked.start, text.length);
      const equations = adapterFor(draft.language).equations(text);
      return equations.find((e) => e.start <= at && at <= e.end) ?? equations.find((e) => e.start >= at) ?? null;
    } catch {
      return null;
    }
  }

  /** The draft's source as it would be written (wrapped when it is a new equation in text). */
  draftSource(): string | null {
    const draft = this.state.draft;
    const analysis = this.analyze(draft);
    if (!draft || !analysis?.serialized) return null;
    if (draft.kind === "insert" && draft.context === "text") {
      try {
        return adapterFor(draft.language).wrap(analysis.serialized.source, draft.display);
      } catch {
        return analysis.serialized.source;
      }
    }
    return analysis.serialized.source;
  }

  async copyDraft(): Promise<boolean> {
    const source = this.draftSource();
    if (source === null) return false;
    const copied = await this.env.copy(source);
    this.set({ notice: { tone: "info", text: copied ? "Copied the draft's source." : "Could not copy the draft." } });
    return copied;
  }

  /**
   * Why the draft's target can no longer be written, including a document
   * closed since the last change (shown before Apply is tried).
   */
  targetConflict(): TargetProblem | null {
    const draft = this.state.draft;
    if (!draft) return null;
    if (draft.conflict) return draft.conflict;
    const document = this.env.document(draft.documentId);
    if (!document || document.closed || document.model.isDisposed()) return { kind: "closed" };
    // Read again from the current text (pure: nothing is recorded here).
    return this.contextProblem(draft, document.model.getValue());
  }

  /** The draft's tracked target range now (for "Edit in Source"). */
  targetRange(): { start: number; end: number } | null {
    return this.tracked ? { start: this.tracked.start, end: this.tracked.end } : null;
  }
}
