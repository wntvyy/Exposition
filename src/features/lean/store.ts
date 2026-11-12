// Lean feature state: checking state per open Lean document (from `lean:event`),
// the Lean editor the user is working in, and the goals at its cursor.
//
// Goals are always requested for the exact revision Lean has: only when the
// editor has no unacknowledged local edits and its revision equals the
// revision Lean tracks. Older answers are dropped.
import { create } from "zustand";

import type { DocumentId } from "../../bindings/DocumentId";
import type { LeanDocumentState } from "../../bindings/LeanDocumentState";
import type { LeanGoals } from "../../bindings/LeanGoals";
import { commands, errorMessage, events, isCommandError } from "../../lib/ipc";
import type { monaco } from "../../lib/monaco";
import { handleFor } from "../editor";

export interface LeanCursor {
  document: DocumentId;
  /** One-based, for display. */
  line: number;
  column: number;
}

export interface GoalsView {
  /** document:revision:offset:server — what `goals` answers. */
  key: string;
  loading: boolean;
  goals: LeanGoals | null;
  error: string | null;
}

interface LeanStore {
  docs: Record<string, LeanDocumentState>;
  cursor: LeanCursor | null;
  goals: GoalsView | null;
  loaded: boolean;
}

export const useLean = create<LeanStore>(() => ({ docs: {}, cursor: null, goals: null, loaded: false }));

type Listener = (state: LeanDocumentState | null, document: DocumentId) => void;
const listeners = new Set<Listener>();

/** Observe document state changes (Monaco markers, decorations). */
export function onLeanDocument(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function setDoc(state: LeanDocumentState) {
  useLean.setState((s) => ({ docs: { ...s.docs, [state.document]: state } }));
  for (const l of listeners) l(state, state.document);
  if (useLean.getState().cursor?.document === state.document) scheduleGoals();
}

function removeDoc(document: DocumentId) {
  useLean.setState((s) => {
    const docs = { ...s.docs };
    delete docs[document];
    return { docs };
  });
  for (const l of listeners) l(null, document);
}

async function reload() {
  try {
    const list = await commands.leanDocuments();
    useLean.setState({ docs: Object.fromEntries(list.map((s) => [s.document, s])), loaded: true });
    for (const state of list) for (const l of listeners) l(state, state.document);
  } catch {
    useLean.setState({ loaded: true });
  }
}

let tracking = false;

/** Subscribe to Lean events once (idempotent). */
export function startLeanTracking(): void {
  if (tracking) return;
  tracking = true;
  try {
    void events.lean((event) => {
      if (event.kind === "document") setDoc(event.state);
      else removeDoc(event.document);
    });
    void events.leanResync(() => void reload());
    void reload();
  } catch {
    // Not running inside the desktop shell (unit tests).
  }
}

export function leanStateForPath(path: string | undefined): LeanDocumentState | undefined {
  if (!path) return undefined;
  return Object.values(useLean.getState().docs).find((s) => s.path === path);
}

// ----- cursor and goals ------------------------------------------------------

let cursorEditor: monaco.editor.ICodeEditor | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
let sequence = 0;

/** The Lean editor whose cursor drives the goal view. */
export function setCursorEditor(editor: monaco.editor.ICodeEditor, document: DocumentId): void {
  cursorEditor = editor;
  const position = editor.getPosition();
  useLean.setState({
    cursor: position ? { document, line: position.lineNumber, column: position.column } : null,
  });
  scheduleGoals();
}

export function clearCursorEditor(editor: monaco.editor.ICodeEditor): void {
  if (cursorEditor !== editor) return;
  cursorEditor = null;
  useLean.setState({ cursor: null, goals: null });
}

export function cursorTarget(): { editor: monaco.editor.ICodeEditor; document: DocumentId } | null {
  const cursor = useLean.getState().cursor;
  return cursor && cursorEditor ? { editor: cursorEditor, document: cursor.document } : null;
}

/** Revision and offset of the cursor if the editor is in sync with Lean's revision. */
export function syncedCursor(): { document: DocumentId; revision: number; offset: number } | null {
  const target = cursorTarget();
  if (!target) return null;
  const handle = handleFor(target.document);
  const state = useLean.getState().docs[target.document];
  const model = target.editor.getModel();
  const position = target.editor.getPosition();
  if (!handle || !state || !model || !position || model !== handle.model) return null;
  if (handle.sync.pendingCount > 0 || handle.sync.revision !== state.revision) return null;
  return { document: target.document, revision: state.revision, offset: model.getOffsetAt(position) };
}

export function scheduleGoals(): void {
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => void fetchGoals(), 120);
}

function serverHasRevision(state: LeanDocumentState): boolean {
  const s = state.status;
  if (s.kind === "checking") return !s.starting && s.revision === state.revision;
  return (s.kind === "checked" || s.kind === "checkedWithSorry" || s.kind === "errors") && s.revision === state.revision;
}

async function fetchGoals(): Promise<void> {
  const target = syncedCursor();
  if (!target) return; // a later document or Lean event reschedules
  const state = useLean.getState().docs[target.document];
  if (!state || !serverHasRevision(state)) {
    useLean.setState({ goals: null });
    return;
  }
  const key = `${target.document}:${target.revision}:${target.offset}:${state.job ?? ""}`;
  const current = useLean.getState().goals;
  if (current?.key === key && (current.loading || current.goals || current.error)) return;
  const seq = ++sequence;
  useLean.setState({ goals: { key, loading: true, goals: current?.goals ?? null, error: null } });
  try {
    const goals = await commands.leanGoals(target.document, target.revision, target.offset);
    if (seq === sequence) useLean.setState({ goals: { key, loading: false, goals, error: null } });
  } catch (e) {
    if (seq !== sequence) return;
    const transient = isCommandError(e) && ["lean.stale", "lean.notReady", "lean.cancelled"].includes(e.code);
    // Transient: keep the last goals; the next Lean event asks again.
    useLean.setState({
      goals: transient ? { key: "", loading: false, goals: current?.goals ?? null, error: null } : { key, loading: false, goals: null, error: errorMessage(e) },
    });
  }
}
