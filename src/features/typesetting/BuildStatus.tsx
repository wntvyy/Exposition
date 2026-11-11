// Build status item: building / succeeded / failed / stale for the active
// editor's typesetting entry (or the most recently built entry).
import { CircleCheck, CircleX, Loader, TriangleAlert } from "lucide-react";

import { useActiveEditor } from "../../app/editors";
import { summarize, useTypesetting } from "./store";

const LABELS = {
  building: "Building",
  succeeded: "Built",
  failed: "Build failed",
  stale: "Errors — pages stale",
  none: "",
} as const;

export function BuildStatus() {
  const active = useActiveEditor();
  const entries = useTypesetting((s) => s.entries);
  const path = active?.resource.path;
  const state = (path && entries[path]) || Object.values(entries)[0];
  const summary = summarize(state);
  if (summary === "none") return null;
  const Icon =
    summary === "building" ? Loader : summary === "succeeded" ? CircleCheck : summary === "failed" ? CircleX : TriangleAlert;
  const errors = state?.latest?.result.diagnostics.filter((d) => d.severity === "error").length ?? 0;
  const duration = state?.latest?.result.durationMs;
  return (
    <span className={`build-status build-status--${summary}`} data-testid="build-status" role="status">
      <Icon size={12} aria-hidden />
      {LABELS[summary]}
      {summary === "succeeded" && duration !== undefined ? ` · ${duration} ms` : ""}
      {errors > 0 ? ` · ${errors} error${errors === 1 ? "" : "s"}` : ""}
    </span>
  );
}
