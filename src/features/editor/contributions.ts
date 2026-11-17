// Source editor contributions: features add view-only behavior to every
// Monaco source editor (SourceEditor) without editing it. A contribution is
// called once per editor instance with the editor and its document handle and
// returns what to dispose when the editor goes away. Contributions must not
// edit the document (all edits go through the document service).
import type { monaco } from "../../lib/monaco";
import type { DocumentHandle } from "./documentClient";

export type SourceEditorContribution = (
  editor: monaco.editor.IStandaloneCodeEditor,
  handle: DocumentHandle,
) => monaco.IDisposable;

const contributions = new Map<string, SourceEditorContribution>();

/** Register (or replace) a contribution by id; returns its unregistration. */
export function registerSourceEditorContribution(id: string, contribution: SourceEditorContribution): () => void {
  contributions.set(id, contribution);
  return () => {
    if (contributions.get(id) === contribution) contributions.delete(id);
  };
}

/** Attach every registered contribution to a new editor (a failing one is skipped). */
export function attachSourceEditorContributions(
  editor: monaco.editor.IStandaloneCodeEditor,
  handle: DocumentHandle,
): monaco.IDisposable[] {
  const attached: monaco.IDisposable[] = [];
  for (const [id, contribution] of contributions) {
    try {
      attached.push(contribution(editor, handle));
    } catch (error) {
      console.warn(`Source editor contribution ${id} failed`, error);
    }
  }
  return attached;
}
