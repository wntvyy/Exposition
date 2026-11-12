// Navigation from graph views to sources: show the Editor page, open the file,
// and select the bound range in its source editor. Read-only with respect to
// documents (it only moves the caret).
import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import { EDITOR_PAGE, pages } from "../../app/pages";
import type { TextRange } from "../../bindings/TextRange";
import { monaco } from "../../lib/monaco";
import { fileKey, handleForPath } from "../editor";

export async function revealSource(path: string, range: TextRange | null): Promise<boolean> {
  pages.show(EDITOR_PAGE);
  editors.open({ key: fileKey(path), path, title: path.split("/").pop() ?? path }, { preview: false });
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const handle = handleForPath(path);
    const editor = handle && monaco.editor.getEditors().find((e) => e.getModel() === handle.model);
    if (handle && editor) {
      const model = handle.model;
      if (range) {
        const length = model.getValueLength();
        const start = model.getPositionAt(Math.min(range.start, length));
        const end = model.getPositionAt(Math.min(range.end, length));
        editor.setSelection(new monaco.Selection(start.lineNumber, start.column, end.lineNumber, end.column));
        editor.revealRangeInCenterIfOutsideViewport(new monaco.Range(start.lineNumber, start.column, end.lineNumber, end.column));
      }
      editor.focus();
      return true;
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  notify({ tone: "warning", title: `Could not show ${path}`, body: "The file did not open in a source editor." });
  return false;
}
