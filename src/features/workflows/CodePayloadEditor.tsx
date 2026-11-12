// A small Monaco editor for a step's code payload. The code is data of the
// workflow definition (saved as part of a revision), not a project file; it
// is never run in this version.
import { useEffect, useRef } from "react";

import { editorDefaults, monaco } from "../../lib/monaco";
import { ensureLanguage } from "../editor/languages";

export function CodePayloadEditor({
  value,
  onChange,
  label,
}: {
  value: string;
  onChange: (value: string) => void;
  label: string;
}) {
  const container = useRef<HTMLDivElement | null>(null);
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  useEffect(() => {
    if (!container.current) return;
    const model = monaco.editor.createModel(value, ensureLanguage("python"));
    const editor = monaco.editor.create(container.current, {
      ...editorDefaults,
      model,
      ariaLabel: label,
      lineNumbers: "on",
      scrollBeyondLastLine: false,
    });
    editorRef.current = editor;
    const sub = model.onDidChangeContent(() => onChangeRef.current(model.getValue()));
    return () => {
      sub.dispose();
      editor.dispose();
      model.dispose();
      editorRef.current = null;
    };
    // The editor owns its text after creation; external resets are applied below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const model = editorRef.current?.getModel();
    if (model && model.getValue() !== value) model.setValue(value);
  }, [value]);

  return <div className="xp-wf__code" ref={container} data-testid="workflow-code" />;
}
