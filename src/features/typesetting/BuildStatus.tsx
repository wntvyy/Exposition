// Build status item: the active editor's typesetting entry (or the most
// recently built one) as building / built / failed / stale / unavailable.
import { useActiveEditor } from "../../app/editors";
import { StatusPill } from "../../ui/components";
import { statusProblem, summarize, useTypesetting } from "./store";

export function BuildStatus() {
  const active = useActiveEditor();
  const entries = useTypesetting((s) => s.entries);
  const path = active?.resource.path;
  const state = (path && entries[path]) || Object.values(entries)[0];
  const summary = summarize(state);
  if (!state || summary === "none") return null;
  const errors = state.latest?.result.diagnostics.filter((d) => d.severity === "error").length ?? 0;
  // A build that could not run has no error diagnostics: say why instead.
  const problem = statusProblem(state.latest);
  const errorText = errors === 0 && problem ? problem.message : `${errors} error${errors === 1 ? "" : "s"}`;
  const name = state.entry;
  const status = state.latest?.result.status;
  return (
    <span data-testid="build-status" data-status={summary} title={problem?.hints.join("\n") || undefined}>
      {summary === "building" && <StatusPill status="running" label={`Building ${name}`} size="sm" />}
      {summary === "succeeded" && (
        <StatusPill status="succeeded" label={`Built ${name}`} detail={`${state.latest?.result.durationMs ?? 0} ms`} size="sm" />
      )}
      {summary === "failed" && <StatusPill status="failed" label={`${name} failed`} detail={errorText} size="sm" />}
      {summary === "stale" && <StatusPill status="stale" label={`${name}: pages stale`} detail={errorText} size="sm" />}
      {summary === "untrusted" && (
        <StatusPill status="unavailable" label={`${name}: workspace not trusted`} size="sm" />
      )}
      {summary === "unavailable" && status?.kind === "toolUnavailable" && (
        <StatusPill status="unavailable" label={`${name}: ${status.tool} not available`} detail="see Problems" size="sm" />
      )}
    </span>
  );
}
