// Read-only view of the active source position, for contextual inspectors
// (e.g. the Graph dependency inspector). It observes Monaco editors bound to
// shared documents and never changes editor or document state.
import { useEffect, useState, useSyncExternalStore } from "react";

import { useActiveEditor } from "../../app/editors";
import { monaco } from "../../lib/monaco";
import { handleForPath } from "./documentClient";

export interface SourcePosition {
  /** Workspace-relative path of the document. */
  path: string;
  /** UTF-16 offset of the caret (selection start) in the current text. */
  offset: number;
  /** 1-based line of the caret. */
  line: number;
  /** Document revision the offset refers to. */
  revision: number;
  /** The document has unsaved changes. */
  dirty: boolean;
  /** The caret position came from a Monaco editor (vs. the last known selection). */
  fromEditor: boolean;
}

type Listener = () => void;
const listeners = new Set<Listener>();
let focused: monaco.editor.ICodeEditor | null = null;
let installed = false;
let tick = 0;

function changed() {
  tick += 1;
  for (const l of listeners) l();
}

function install() {
  if (installed) return;
  installed = true;
  const watch = (editor: monaco.editor.ICodeEditor) => {
    editor.onDidFocusEditorText(() => {
      focused = editor;
      changed();
    });
    editor.onDidChangeCursorPosition(() => {
      if (editor === focused || editor.hasTextFocus()) {
        focused = editor;
        changed();
      }
    });
    editor.onDidDispose(() => {
      if (focused === editor) focused = null;
      changed();
    });
  };
  monaco.editor.getEditors().forEach(watch);
  monaco.editor.onDidCreateEditor(watch);
}

function subscribe(listener: Listener): () => void {
  install();
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Most recently focused Monaco editor showing `path`'s shared model, if any. */
function editorFor(path: string): monaco.editor.ICodeEditor | null {
  const handle = handleForPath(path);
  if (!handle) return null;
  if (focused && focused.getModel() === handle.model) return focused;
  return monaco.editor.getEditors().find((e) => e.getModel() === handle.model) ?? null;
}

/**
 * The caret of the active editor tab's document: from its Monaco editor when
 * one shows it, else from the document's last known selection. Null when the
 * active tab is not a workspace text document.
 */
export function useActiveSourcePosition(): SourcePosition | null {
  const active = useActiveEditor();
  const path = active?.resource.path ?? null;
  useSyncExternalStore(subscribe, () => tick);
  const handle = path ? handleForPath(path) : undefined;
  // Re-render on document changes (revision, dirty state).
  const [, setVersion] = useState(0);
  useEffect(() => handle?.subscribe(() => setVersion((v) => v + 1)), [handle]);
  if (!path || !handle) return null;
  const editor = editorFor(path);
  const model = handle.model;
  let offset: number | null = null;
  let fromEditor = false;
  const position = editor?.getPosition();
  if (editor && position && !model.isDisposed()) {
    offset = model.getOffsetAt(position);
    fromEditor = true;
  } else if (handle.selection[0]) {
    offset = handle.selection[0].start;
  }
  if (offset === null) offset = 0;
  const line = model.isDisposed() ? 1 : model.getPositionAt(offset).lineNumber;
  return { path, offset, line, revision: handle.sync.revision, dirty: handle.isDirty, fromEditor };
}
