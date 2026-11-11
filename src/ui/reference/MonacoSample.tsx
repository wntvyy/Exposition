import { useEffect, useRef } from "react";

import { editorDefaults, monaco } from "../../lib/monaco";

const SAMPLE = `= Exposition UI reference
// Monaco uses the exposition-light / exposition-dark themes,
// the shared code font stack, 13px / 20px metrics, and the
// focus, selection, and current-line colors from tokens.css.

#let total = 12 + 30
The answer is #total.
`;

/** Read-only Monaco instance demonstrating the shared editor theme and defaults. */
export default function MonacoSample() {
  const host = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!host.current) return;
    const editor = monaco.editor.create(host.current, {
      ...editorDefaults,
      value: SAMPLE,
      language: "plaintext",
      readOnly: true,
      ariaLabel: "Monaco theme sample (read-only)",
      lineNumbersMinChars: 3,
    });
    editor.setSelection(new monaco.Selection(7, 5, 7, 10));
    return () => editor.dispose();
  }, []);
  return <div ref={host} className="ref-monaco" data-testid="monaco-sample" />;
}
