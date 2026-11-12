// Build status item: the active editor's typesetting entry (or the most
// recently built one) as building / built / failed / stale.
import { useActiveEditor } from "../../app/editors";
import { StatusPill } from "../../ui/components";
import { summarize, useTypesetting } from "./store";

export function BuildStatus() {
  const active = useActiveEditor();
  const entries = useTypesetting((s) => s.entries);
  const path = active?.resource.path;
  const state = (path && entries[path]) || Object.values(entries)[0];
  const summary = summarize(state);
  if (!state || summary === "none") return null;
  const errors = state.latest?.result.diagnostics.filter((d) => d.severity === "error").length ?? 0;
  const errorText = `${errors} error${errors === 1 ? "" : "s"}`;
  const name = state.entry;
  return (
    <span data-testid="build-status" data-status={summary}>
      {summary === "building" && <StatusPill status="running" label={`Building ${name}`} size="sm" />}
      {summary === "succeeded" && (
        <StatusPill status="succeeded" label={`Built ${name}`} detail={`${state.latest?.result.durationMs ?? 0} ms`} size="sm" />
      )}
      {summary === "failed" && <StatusPill status="failed" label={`${name} failed`} detail={errorText} size="sm" />}
      {summary === "stale" && <StatusPill status="stale" label={`${name}: pages stale`} detail={errorText} size="sm" />}
      {summary === "untrusted" && (
        <StatusPill status="unavailable" label={`${name}: workspace not trusted`} size="sm" />
      )}
    </span>
  );
}
