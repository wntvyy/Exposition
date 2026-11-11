// Monaco source editor bound to a document in the shared document service.
//
// Exported for composition: typesetting/page-editing views can render
// `<SourceEditor path="main.typ" />` or `<SourceEditor documentId={id} />`
// beside a page view; both edit the same revisioned document and share undo.
import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import type { DocumentId } from "../../bindings/DocumentId";
import type { TextRange } from "../../bindings/TextRange";
import { registry } from "../../app/registry";
import { errorMessage, isCommandError } from "../../lib/ipc";
import { monaco } from "../../lib/monaco";
import {
  attachDocument,
  keepBuffer,
  normalizeLineEndings,
  openDocument,
  reloadDocument,
  saveDocument,
  type DocumentHandle,
} from "./documentClient";
import { loadAppSettings, useAppSettings } from "./settings";
import "./editor.css";

export interface SourceEditorProps {
  /** Attach to a document already open in the document service. */
  documentId?: DocumentId;
  /** Or open a workspace file by its workspace-relative path. */
  path?: string;
  /** Focus the editor when this becomes true (e.g. its tab/group is active). */
  active?: boolean;
  /** Additional host-imposed read-only reason (shown in the editor banner). */
  readOnly?: string | null;
  /** Receives the Monaco editor and document handle once mounted (null on unmount). */
  onEditor?: (editor: monaco.editor.IStandaloneCodeEditor | null, handle: DocumentHandle | null) => void;
  className?: string;
}

type Load =
  | { status: "loading" }
  | { status: "ready"; handle: DocumentHandle }
  | { status: "error"; code: string; message: string };

const noop = () => () => {};
const zero = () => 0;

function toRanges(model: monaco.editor.ITextModel, selections: readonly monaco.Selection[]): TextRange[] {
  return selections.map((s) => ({
    start: model.getOffsetAt(s.getStartPosition()),
    end: model.getOffsetAt(s.getEndPosition()),
  }));
}

function prefersDark() {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-color-scheme: dark)").matches;
}

export function SourceEditor({ documentId, path, active, readOnly, onEditor, className }: SourceEditorProps) {
  const [load, setLoad] = useState<Load>({ status: "loading" });
  const container = useRef<HTMLDivElement>(null);
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const settings = useAppSettings((s) => s.settings.editor);
  const handle = load.status === "ready" ? load.handle : null;
  useSyncExternalStore(handle?.subscribe ?? noop, handle?.getVersion ?? zero);

  useEffect(() => {
    void loadAppSettings();
  }, []);

  // Resolve the document.
  useEffect(() => {
    let cancelled = false;
    setLoad({ status: "loading" });
    const pending = documentId ? attachDocument(documentId) : path ? openDocument(path) : null;
    if (!pending) {
      setLoad({ status: "error", code: "editor.noDocument", message: "No document to show." });
      return;
    }
    pending.then(
      (h) => !cancelled && setLoad({ status: "ready", handle: h }),
      (e: unknown) =>
        !cancelled &&
        setLoad({ status: "error", code: isCommandError(e) ? e.code : "document.unavailable", message: errorMessage(e) }),
    );
    return () => {
      cancelled = true;
    };
  }, [documentId, path]);

  // Create the Monaco editor for the document's model.
  useEffect(() => {
    if (!handle || !container.current || handle.closed) return;
    const editor = monaco.editor.create(container.current, {
      model: handle.model,
      automaticLayout: true,
      theme: prefersDark() ? "vs-dark" : "vs",
      fontFamily: 'var(--xp-font-mono, "Cascadia Code", Consolas, "Courier New", monospace)',
      scrollBeyondLastLine: false,
      renderWhitespace: "selection",
      unicodeHighlight: { ambiguousCharacters: false },
      ariaLabel: `Source of ${handle.name}`,
    });
    editorRef.current = editor;
    const disposables: monaco.IDisposable[] = [];
    const { KeyMod, KeyCode } = monaco;
    // Undo/redo go to the shared document history, never Monaco's own stack.
    disposables.push(
      editor.addAction({
        id: "exposition.undo",
        label: "Undo",
        keybindings: [KeyMod.CtrlCmd | KeyCode.KeyZ],
        run: () => handle.undo(),
      }),
      editor.addAction({
        id: "exposition.redo",
        label: "Redo",
        keybindings: [KeyMod.CtrlCmd | KeyCode.KeyY, KeyMod.CtrlCmd | KeyMod.Shift | KeyCode.KeyZ],
        run: () => handle.redo(),
      }),
      editor.addAction({
        id: "exposition.save",
        label: "Save",
        keybindings: [KeyMod.CtrlCmd | KeyCode.KeyS],
        run: () => void saveDocument(handle),
      }),
      editor.addAction({
        id: "exposition.saveAll",
        label: "Save All",
        keybindings: [KeyMod.CtrlCmd | KeyMod.Alt | KeyCode.KeyS],
        run: () => void registry.runCommand("file.saveAll"),
      }),
      editor.onDidCompositionStart(() => handle.grouper.setComposing(true)),
      editor.onDidCompositionEnd(() => handle.grouper.setComposing(false)),
      editor.onDidChangeCursorSelection((e) => {
        if (e.source === "api" && !editor.hasTextFocus()) return;
        const model = editor.getModel();
        if (model) handle.selection = toRanges(model, [e.selection, ...e.secondarySelections]);
      }),
      editor.onDidChangeCursorPosition((e) => {
        if (e.reason === monaco.editor.CursorChangeReason.Explicit) handle.grouper.reset();
      }),
    );
    const offSelection = handle.onSelection((ranges) => {
      const model = editor.getModel();
      if (!model || !editor.hasWidgetFocus() || ranges.length === 0) return;
      editor.setSelections(
        ranges.map((r) => {
          const a = model.getPositionAt(r.start);
          const b = model.getPositionAt(r.end);
          return new monaco.Selection(a.lineNumber, a.column, b.lineNumber, b.column);
        }),
      );
    });
    onEditor?.(editor, handle);
    return () => {
      offSelection();
      for (const d of disposables) d.dispose();
      onEditor?.(null, null);
      editor.dispose();
      editorRef.current = null;
    };
    // `onEditor` is a notification callback; a new identity does not recreate the editor.
  }, [handle]);

  // Options that follow settings and document state.
  const reason = readOnly ?? handle?.readOnlyReason ?? null;
  useEffect(() => {
    editorRef.current?.updateOptions({
      readOnly: reason !== null,
      readOnlyMessage: reason ? { value: reason } : undefined,
      fontSize: settings.fontSize,
      tabSize: settings.tabSize,
      insertSpaces: settings.insertSpaces,
      wordWrap: settings.wordWrap ? "on" : "off",
      lineNumbers: settings.lineNumbers ? "on" : "off",
      minimap: { enabled: settings.minimap },
    });
    handle?.model.updateOptions({ tabSize: settings.tabSize, insertSpaces: settings.insertSpaces });
  }, [reason, settings, handle]);

  useEffect(() => {
    if (active && editorRef.current) editorRef.current.focus();
  }, [active, handle]);

  if (load.status === "error") {
    return (
      <div className={`xp-source-editor ${className ?? ""}`} data-testid="source-editor-unavailable">
        <div className="xp-unavailable" role="status">
          <h2>{load.code === "file.tooLarge" ? "File too large" : "Can't open this file as text"}</h2>
          <p>{load.message}</p>
        </div>
      </div>
    );
  }

  const conflict = handle?.conflict;
  return (
    <div
      className={`xp-source-editor ${className ?? ""}`}
      data-testid="source-editor"
      data-document-id={handle?.id}
      data-path={handle?.path ?? undefined}
      data-dirty={handle?.isDirty ? "true" : "false"}
      data-eol={handle?.eolMode}
      aria-busy={load.status === "loading"}
    >
      {conflict && handle && (
        <div className="xp-banner xp-banner-warning" role="alert" data-testid="conflict-banner" data-kind={conflict.kind}>
          <span>{conflict.message}</span>
          <span className="xp-banner-actions">
            {conflict.kind === "modified" && (
              <button type="button" className="xp-button" onClick={() => void reloadDocument(handle)}>
                Reload from Disk
              </button>
            )}
            <button type="button" className="xp-button" onClick={() => void keepBuffer(handle)}>
              {conflict.kind === "deleted" ? "Keep in Editor" : "Keep My Changes"}
            </button>
          </span>
        </div>
      )}
      {handle?.saveError && (
        <div className="xp-banner xp-banner-danger" role="alert" data-testid="save-error-banner">
          <span>Not saved: {handle.saveError}</span>
          <span className="xp-banner-actions">
            <button type="button" className="xp-button" onClick={() => void saveDocument(handle)}>
              Retry
            </button>
            <button type="button" className="xp-button" onClick={() => handle.setSaveError(null)}>
              Dismiss
            </button>
          </span>
        </div>
      )}
      {handle?.eolMode === "mixed" && (
        <div className="xp-banner xp-banner-info" role="status" data-testid="mixed-eol-banner">
          <span>This file mixes line endings and is read-only here, so they are never changed silently.</span>
          <span className="xp-banner-actions">
            <button type="button" className="xp-button" onClick={() => void normalizeLineEndings(handle, "lf")}>
              Normalize to LF
            </button>
            <button type="button" className="xp-button" onClick={() => void normalizeLineEndings(handle, "crlf")}>
              Normalize to CRLF
            </button>
          </span>
        </div>
      )}
      {handle?.info.readOnly && (
        <div className="xp-banner xp-banner-info" role="status">
          Read-only: {handle.info.readOnly}
        </div>
      )}
      <div className="xp-monaco" ref={container} />
    </div>
  );
}
