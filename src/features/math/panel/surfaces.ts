// The two entry surfaces of the equation editor — the Monaco source editor and
// the exact-page view — resolved to one caret target in the document model's
// current text. The surface is the one the user worked in last for the active
// document (or the one an entry point names). Observes editors only; the
// draft writes through the document handle (see document.ts).
import { editors } from "../../../app/editors";
import { layout } from "../../../app/layout";
import { EDITOR_PAGE, pages } from "../../../app/pages";
import { monaco } from "../../../lib/monaco";
import { copyText } from "../../../ui/components";
import { handleFor, handleForPath, type DocumentHandle } from "../../editor";
import { registerSourceEditorContribution } from "../../editor/contributions";
import { revealSource } from "../../editor/reveal";
import { requestSourceFocus } from "../../editor/sourceFocus";
import { focusTick, lastPageSurface } from "../../page/activePage";
import { languageForPath } from "../core";
import { resolvePageCaret, type CaretTarget, type EquationDocument, type Resolved, type Surface } from "./document";
import type { DraftEnvironment } from "./draft";

export const EQUATION_VIEW_ID = "editor.equation";

/** When each live source editor last had text focus (shared clock with page views). */
const sourceFocus = new Map<monaco.editor.ICodeEditor, number>();

/** Record source-editor focus (installed on every source editor; never edits). */
export function installSourceFocusTracking(): () => void {
  return registerSourceEditorContribution("math.equationFocus", (editor) => {
    const note = () => sourceFocus.set(editor, focusTick());
    if (editor.hasTextFocus()) note();
    const focus = editor.onDidFocusEditorText(note);
    const position = editor.onDidChangeCursorPosition(() => {
      if (editor.hasTextFocus()) note();
    });
    return {
      dispose() {
        focus.dispose();
        position.dispose();
        sourceFocus.delete(editor);
      },
    };
  });
}

/** The most recently focused live source editor showing `model`. */
function sourceEditorFor(model: monaco.editor.ITextModel): { editor: monaco.editor.ICodeEditor; focusedAt: number } | null {
  let best: { editor: monaco.editor.ICodeEditor; focusedAt: number } | null = null;
  for (const editor of monaco.editor.getEditors()) {
    if (editor.getModel() !== model) continue;
    const focusedAt = sourceFocus.get(editor) ?? 0;
    if (!best || focusedAt > best.focusedAt) best = { editor, focusedAt };
  }
  return best;
}

/** The surface holding keyboard focus right now, if it is one of the two. */
export function focusedSurface(): Surface | undefined {
  const active = typeof document === "undefined" ? null : document.activeElement;
  if (!(active instanceof Element)) return undefined;
  if (active.closest('[data-testid="page-editor"]')) return "page";
  if (active.closest(".monaco-editor")) return "source";
  return undefined;
}

function sourceTarget(handle: DocumentHandle, path: string, editor: monaco.editor.ICodeEditor): CaretTarget | { error: string } {
  const model = handle.model;
  const selection = editor.getSelection();
  if (!selection) return { error: "Place the caret in the source first." };
  const a = model.getOffsetAt(selection.getStartPosition());
  const b = model.getOffsetAt(selection.getEndPosition());
  return {
    surface: "source",
    document: handle,
    path,
    start: Math.min(a, b),
    end: Math.max(a, b),
    focus: (offset) => {
      if (editor.getModel() !== model) return;
      if (offset !== undefined) editor.setPosition(model.getPositionAt(Math.min(offset, model.getValueLength())));
      editor.focus();
    },
  };
}

/** Resolve the caret of the active document's surface (or of `preferred`). */
export async function resolveActiveCaret(preferred?: Surface): Promise<Resolved> {
  const active = editors.active();
  const path = active?.resource.path;
  if (!active || !path) return { error: "Open a LaTeX, Typst, or Markdown document first." };
  if (!languageForPath(path)) return { error: "Equations can be edited in LaTeX, Typst, and Markdown documents." };
  const handle = handleForPath(path);
  if (!handle || handle.closed) return { error: "The document is still opening. Try again in a moment." };
  const page = active.mode === "source" ? null : lastPageSurface(path);
  const source = active.mode === "page" ? null : sourceEditorFor(handle.model);
  let surface: Surface | null = preferred ?? focusedSurface() ?? null;
  if (surface === "page" && !page) surface = null;
  if (surface === "source" && !source) surface = null;
  surface ??= page && (!source || page.focusedAt > source.focusedAt) ? "page" : source ? "source" : null;
  if (surface === "source" && source) return sourceTarget(handle, path, source.editor);
  if (surface === "page" && page) {
    const resolved = await resolvePageCaret(page.surface, handle);
    if ("error" in resolved) return resolved;
    const { offset, anchor } = resolved.caret;
    const other = anchor ?? offset;
    return {
      surface: "page",
      document: handle,
      path,
      start: Math.min(offset, other),
      end: Math.max(offset, other),
      focus: () => page.surface.focus(),
    };
  }
  return { error: "Show the source or the page of the document to choose where the equation goes." };
}

export function showEquationPanel(): void {
  pages.show(EDITOR_PAGE);
  layout.showView("auxiliary", EQUATION_VIEW_ID);
}

/** Show `range` of `path` selected in a source editor (switching a Page view to Split). */
export async function editInSource(path: string, range: { start: number; end: number } | null): Promise<void> {
  const key = `file:${path}`;
  const open = editors.all().find((e) => e.resource.key === key);
  if (open?.mode === "page") {
    requestSourceFocus(key);
    editors.setMode(key, "split");
  }
  await revealSource(path, range);
}

export const environment: DraftEnvironment = {
  resolve: resolveActiveCaret,
  document: (id) => {
    const handle = handleFor(id);
    return handle && !handle.closed ? (handle as EquationDocument) : undefined;
  },
  showPanel: showEquationPanel,
  copy: copyText,
};
