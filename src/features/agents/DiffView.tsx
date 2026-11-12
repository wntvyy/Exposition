import { useEffect, useRef } from "react";

import type { FileDiff } from "../../bindings/FileDiff";
import { editorDefaults, monaco } from "../../lib/monaco";

type Models = { original: monaco.editor.ITextModel; modified: monaco.editor.ITextModel; path: string };

function languageId(language: string): string {
  return monaco.languages.getLanguages().some((l) => l.id === language) ? language : "plaintext";
}

/** Dispose models only once no diff computation can still be reading them. */
function disposeLater(models: Models, editor: monaco.editor.IStandaloneDiffEditor | null) {
  let done = false;
  const dispose = () => {
    if (done) return;
    done = true;
    models.original.dispose();
    models.modified.dispose();
  };
  const subscription = editor?.onDidUpdateDiff(() => {
    subscription?.dispose();
    dispose();
  });
  setTimeout(() => {
    subscription?.dispose();
    dispose();
  }, 2000);
}

/**
 * Read-only Monaco diff of a task's change to one file (follows the app
 * theme). One diff editor lives as long as the view; new texts swap models,
 * and superseded models are disposed after Monaco has finished diffing.
 */
export function DiffView({ diff, height = 360 }: { diff: FileDiff; height?: number | "fill" }) {
  const host = useRef<HTMLDivElement>(null);
  const editor = useRef<monaco.editor.IStandaloneDiffEditor | null>(null);
  const models = useRef<Models | null>(null);

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    const instance = monaco.editor.createDiffEditor(element, {
      ...editorDefaults,
      readOnly: true,
      originalEditable: false,
      renderSideBySide: true,
      useInlineViewWhenSpaceIsLimited: true,
      renderSideBySideInlineBreakpoint: 700,
      wordWrap: "on",
      diffWordWrap: "on",
    });
    editor.current = instance;
    return () => {
      editor.current = null;
      instance.dispose();
      if (models.current) disposeLater(models.current, null);
      models.current = null;
    };
  }, []);

  useEffect(() => {
    const instance = editor.current;
    if (!instance) return;
    const current = models.current;
    if (
      current &&
      current.path === diff.path &&
      current.original.getValue() === diff.original &&
      current.modified.getValue() === diff.modified
    ) {
      return;
    }
    const language = languageId(diff.language);
    const next: Models = {
      original: monaco.editor.createModel(diff.original, language),
      modified: monaco.editor.createModel(diff.modified, language),
      path: diff.path,
    };
    instance.updateOptions({ ariaLabel: `Changes to ${diff.path}` });
    instance.setModel({ original: next.original, modified: next.modified });
    models.current = next;
    if (current) disposeLater(current, instance);
  }, [diff]);

  return (
    <div
      className="agents-diff"
      data-fill={height === "fill" || undefined}
      style={height === "fill" ? undefined : { height }}
      ref={host}
      data-testid="agents-diff"
    />
  );
}
