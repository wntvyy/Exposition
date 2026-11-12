// Research: discovered Python/R/Julia environments (read-only) with refresh.
import { useEffect } from "react";
import { RefreshCw } from "lucide-react";

import type { RuntimeEnvironment } from "../../bindings/RuntimeEnvironment";
import { Button, EmptyState, ErrorState, LoadingState, StatusPill } from "../../ui/components";
import { loadEnvironments, useResearch } from "../research/store";
import { SettingsGroup, SettingsPage } from "./parts";

function kernelLabel(runtime: RuntimeEnvironment): string {
  if (!runtime.kernel) return "—";
  return runtime.kernel.kind === "python" ? "ipykernel" : `kernelspec ${runtime.kernel.name}`;
}

export function ResearchSection() {
  const report = useResearch((s) => s.environments);
  const loading = useResearch((s) => s.environmentsLoading);
  const error = useResearch((s) => s.environmentsError);
  useEffect(() => {
    if (useResearch.getState().environments === null) void loadEnvironments(false);
  }, []);

  return (
    <SettingsPage
      title="Research"
      testId="settings-research"
      description="Python, R, and Julia environments found on this computer. Exposition uses existing installations and never installs packages."
      actions={
        <Button size="sm" variant="ghost" icon={RefreshCw} isPending={loading} onPress={() => void loadEnvironments(true)}>
          {loading ? "Scanning…" : "Refresh"}
        </Button>
      }
    >
      {report === null ? (
        error ? (
          <ErrorState
            title="Could not discover environments"
            details={error}
            actions={<Button onPress={() => void loadEnvironments(true)}>Retry</Button>}
          />
        ) : (
          <LoadingState title="Discovering environments…" />
        )
      ) : (
        <SettingsGroup
          title="Environments"
          description={`Scanned ${new Date(report.scannedAtMs).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}.`}
        >
          {report.runtimes.length === 0 ? (
            <EmptyState title="No environments found" description="Install Python, R, or Julia, then refresh." />
          ) : (
            <table className="exp-settings__table" data-testid="research-environments" aria-busy={loading}>
              <thead>
                <tr>
                  <th scope="col">Environment</th>
                  <th scope="col">Status</th>
                  <th scope="col">Executable and kernel</th>
                </tr>
              </thead>
              <tbody>
                {report.runtimes.map((runtime) => (
                  <tr key={runtime.id} data-runtime-id={runtime.id} data-language={runtime.language}>
                    <th scope="row">
                      {runtime.displayName}
                      {runtime.version && <div className="exp-settings__muted">{runtime.version}</div>}
                    </th>
                    <td>
                      {runtime.availability.kind === "available" ? (
                        <StatusPill size="sm" status="succeeded" label="Available" />
                      ) : (
                        <StatusPill size="sm" status="unavailable" label="Unavailable" />
                      )}
                    </td>
                    <td>
                      {runtime.executable && <code className="exp-settings__path">{runtime.executable}</code>}
                      <div className="exp-settings__muted">Kernel: {kernelLabel(runtime)}</div>
                      {runtime.packages.length > 0 && (
                        <div className="exp-settings__muted">
                          {runtime.packages.map((p) => `${p.name} ${p.version}`).join(", ")}
                        </div>
                      )}
                      {runtime.availability.kind === "unavailable" && (
                        <div className="exp-settings__muted">
                          {runtime.availability.reason}. {runtime.availability.guidance}
                        </div>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </SettingsGroup>
      )}
    </SettingsPage>
  );
}
