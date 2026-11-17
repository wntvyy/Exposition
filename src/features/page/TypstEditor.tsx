// Typesetting document editor: Source (Monaco), Page (exact page), and Split
// (both side by side) for one document. All views share the one document
// handle (one Monaco model) and the document service, so page edits appear
// in the source and undo/redo from either side uses the shared history.
import { useEffect, useRef } from "react";
import { Group, Panel as ResizablePanel } from "react-resizable-panels";
import type * as monaco from "monaco-editor/editor";

import { editors, useEditors } from "../../app/editors";
import type { EditorProps } from "../../app/registry";
import { ResizeHandle } from "../../ui/components";
import { fileKey, SourceEditor, type DocumentHandle } from "../editor";
import { useTypesetting } from "../typesetting/store";
import { PageEditor } from "./PageEditor";
import "./page.css";

export function TypstEditor({ resource, mode, active }: EditorProps) {
  // Inactive tabs stay mounted; only the selected tab of the active group takes focus.
  const selected = useEditors((s) => s.groups[s.activeGroup].activeKey === resource.key);
  const focused = active && selected;
  const monacoEditor = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const reveal = useTypesetting((s) => s.reveal);
  const modeRef = useRef(mode);
  modeRef.current = mode;
  /** A source offset to show (and focus) once the source editor exists. */
  const pendingSource = useRef<number | null>(null);

  // Problems panel: reveal the diagnostic's range in the source view too.
  useEffect(() => {
    const editor = monacoEditor.current;
    if (!reveal || !editor || reveal.path !== resource.path || mode === "page") return;
    showInSource(editor, reveal.offset, mode === "source");
  }, [reveal, resource.path, mode]);

  // XR-B-01: the pages cannot show the position (no pages yet, or source the
  // last good build does not contain, such as an error added after it): show
  // the source beside the pages with the caret there, instead of nothing.
  const onRevealMissed = (offset: number) => {
    if (modeRef.current !== "page") return;
    pendingSource.current = offset;
    editors.setMode(resource.key, "split");
  };

  const onEditor = (editor: monaco.editor.IStandaloneCodeEditor | null, handle: DocumentHandle | null) => {
    monacoEditor.current = editor;
    if (editor && pendingSource.current !== null) {
      showInSource(editor, pendingSource.current, true);
      pendingSource.current = null;
    }
    // The tab's path was an alias (case variant, short name, link inside the
    // folder) of a file open under its canonical path: show one tab for it
    // (as the plain source editor does).
    if (handle?.path && resource.path && handle.path !== resource.path) {
      editors.open({ key: fileKey(handle.path), path: handle.path, title: handle.name });
      void editors.close(resource.key);
    }
  };

  if (mode === "source") {
    return <SourceEditor path={resource.path} active={focused} onEditor={onEditor} />;
  }
  if (mode === "page") {
    return <PageEditor resource={resource} mode="page" active={focused} onRevealMissed={onRevealMissed} />;
  }
  return (
    <Group orientation="horizontal" className="typst-split" data-testid="typst-split">
      <ResizablePanel id="source" defaultSize="50%" minSize="20%">
        <SourceEditor path={resource.path} active={false} onEditor={onEditor} />
      </ResizablePanel>
      <ResizeHandle aria-label="Resize source and page views" />
      <ResizablePanel id="page" defaultSize="50%" minSize="20%">
        <PageEditor resource={resource} mode="page" active={focused} />
      </ResizablePanel>
    </Group>
  );
}

/** Place the caret at a source offset, scrolled into view. */
function showInSource(editor: monaco.editor.IStandaloneCodeEditor, offset: number, focus: boolean): void {
  const model = editor.getModel();
  if (!model) return;
  const position = model.getPositionAt(offset);
  editor.setPosition(position);
  editor.revealPositionInCenter(position);
  if (focus) editor.focus();
}
