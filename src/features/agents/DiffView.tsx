import { useEffect, useRef } from "react";

import type { FileDiff } from "../../bindings/FileDiff";
import { editorDefaults, monaco } from "../../lib/monaco";

function languageId(language: string): string {
  return monaco.languages.getLanguages().some((l) => l.id === language) ? language : "plaintext";
}

/** Read-only Monaco diff of a task's change to one file (follows the app theme). */
export function DiffView({ diff, height = 360 }: { diff: FileDiff; height?: number | "fill" }) {
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    const editor = monaco.editor.createDiffEditor(element, {
      ...editorDefaults,
      readOnly: true,
      originalEditable: false,
      renderSideBySide: true,
      useInlineViewWhenSpaceIsLimited: true,
      renderSideBySideInlineBreakpoint: 700,
      wordWrap: "on",
      diffWordWrap: "on",
      ariaLabel: `Changes to ${diff.path}`,
    });
    const language = languageId(diff.language);
    const original = monaco.editor.createModel(diff.original, language);
    const modified = monaco.editor.createModel(diff.modified, language);
    editor.setModel({ original, modified });
    return () => {
      editor.dispose();
      original.dispose();
      modified.dispose();
    };
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
