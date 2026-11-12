// Build diagnostics as squiggles in the source editor: after every finished
// build (Typst, LaTeX, Markdown), the open documents it compiled get its
// diagnostics as Monaco markers; hovering one shows the message.
import type { BuildEvent } from "../../bindings/BuildEvent";
import { monaco } from "../../lib/monaco";
import { openHandles, type DocumentHandle } from "../editor";
import { diagnosticMarks, type DiagnosticMark } from "./markerData";
import { useTypesetting } from "./store";

const OWNER = "typesetting";

const SEVERITY = {
  error: monaco.MarkerSeverity.Error,
  warning: monaco.MarkerSeverity.Warning,
  info: monaco.MarkerSeverity.Info,
} as const;

function latestBuilds(): BuildEvent[] {
  return Object.values(useTypesetting.getState().entries).flatMap((e) => (e.latest ? [e.latest] : []));
}

function toMarker(model: monaco.editor.ITextModel, mark: DiagnosticMark): monaco.editor.IMarkerData {
  const start = model.getPositionAt(mark.start);
  const end = model.getPositionAt(mark.end);
  return {
    severity: SEVERITY[mark.severity],
    message: mark.message,
    source: mark.source,
    startLineNumber: start.lineNumber,
    startColumn: start.column,
    endLineNumber: end.lineNumber,
    endColumn: end.column,
  };
}

function update(handle: DocumentHandle, builds: BuildEvent[]) {
  if (handle.closed || !handle.path || handle.sync.pendingCount > 0) return;
  const marks = diagnosticMarks(builds, handle.path, handle.sync.revision);
  if (!marks) return;
  const model = handle.model;
  monaco.editor.setModelMarkers(
    model,
    OWNER,
    marks.map((mark) => toMarker(model, mark)),
  );
}

function updateAll() {
  const builds = latestBuilds();
  for (const handle of openHandles()) update(handle, builds);
}

let installed = false;

/** Keep build-diagnostic markers on open documents. Idempotent. */
export function installBuildMarkers(): void {
  if (installed) return;
  installed = true;
  useTypesetting.subscribe((state, previous) => {
    if (state.entries !== previous.entries) updateAll();
  });
  // A document opened after its build: mark it once its handle is registered.
  monaco.editor.onDidCreateModel(() => queueMicrotask(updateAll));
}
