// Monaco source editor bound to a document in the shared document service.
//
// Exported for composition: typesetting/page-editing views can render
// `<SourceEditor path="main.typ" />` or `<SourceEditor documentId={id} />`
// beside a page view; both edit the same revisioned document and share undo.
import { FileWarning } from "lucide-react";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";

import type { DocumentId } from "../../bindings/DocumentId";
import type { TextRange } from "../../bindings/TextRange";
import { registry } from "../../app/registry";
import { errorMessage, isCommandError } from "../../lib/ipc";
import { editorDefaults, monaco } from "../../lib/monaco";
import { Banner, Button, ErrorState, UnavailableState } from "../../ui/components";
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
      ...editorDefaults,
      model: handle.model,
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
      editor.onDidCompositionStart(() => handle.setComposing(true)),
      editor.onDidCompositionEnd(() => handle.setComposing(false)),
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
      lineHeight: Math.round((settings.fontSize * 20) / 13),
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
    const unavailable = ["file.binary", "file.undecodable", "file.tooLarge"].includes(load.code);
    return (
      <div
        className={`xp-source-editor ${className ?? ""}`}
        data-testid="source-editor-unavailable"
        data-code={load.code}
        data-path={path}
      >
        {unavailable ? (
          <UnavailableState
            icon={FileWarning}
            title={load.code === "file.tooLarge" ? "File too large to edit as text" : "Can't open this file as text"}
            description={load.message}
          />
        ) : (
          <ErrorState title="Could not open this file" description={load.message} />
        )}
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
        <div data-testid="conflict-banner" data-kind={conflict.kind}>
          <Banner
            tone="warning"
            live
            title={
              conflict.kind === "deleted"
                ? `${handle.name} was deleted on disk`
                : conflict.kind === "unreadable"
                  ? `${handle.name} can no longer be read from disk`
                  : `${handle.name} changed on disk`
            }
            actions={
              <>
                {conflict.kind === "modified" && (
                  <Button size="sm" onPress={() => void reloadDocument(handle)}>
                    Reload from Disk
                  </Button>
                )}
                <Button size="sm" variant="primary" onPress={() => void keepBuffer(handle)}>
                  {conflict.kind === "deleted" ? "Keep in Editor" : "Keep My Changes"}
                </Button>
              </>
            }
          >
            {conflict.message}
            {conflict.kind === "modified" && " Reloading keeps your version in undo history."}
          </Banner>
        </div>
      )}
      {handle?.saveError && (
        <div data-testid="save-error-banner">
          <Banner
            tone="danger"
            live
            title={`${handle.name} was not saved`}
            actions={
              <>
                <Button size="sm" onPress={() => handle.setSaveError(null)}>
                  Dismiss
                </Button>
                <Button size="sm" variant="primary" onPress={() => void saveDocument(handle)}>
                  Retry
                </Button>
              </>
            }
          >
            {handle.saveError}
          </Banner>
        </div>
      )}
      {handle?.eolMode === "mixed" && (
        <div data-testid="mixed-eol-banner">
          <Banner
            tone="info"
            title="Mixed line endings"
            actions={
              <>
                <Button size="sm" onPress={() => void normalizeLineEndings(handle, "lf")}>
                  Normalize to LF
                </Button>
                <Button size="sm" onPress={() => void normalizeLineEndings(handle, "crlf")}>
                  Normalize to CRLF
                </Button>
              </>
            }
          >
            This file mixes line endings, so it is read-only here and they are never changed silently.
          </Banner>
        </div>
      )}
      {handle?.info.readOnly && <Banner tone="info" title="Read-only">{handle.info.readOnly}</Banner>}
      {handle?.hasNul && (
        <div data-testid="nul-banner">
          <Banner tone="info" title="This file contains NUL characters">
            They are shown as control symbols and kept exactly as written.
          </Banner>
        </div>
      )}
      <div className="xp-monaco" ref={container} />
    </div>
  );
}
