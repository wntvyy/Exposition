// Research: discovered Python/R/Julia environments (read-only) with refresh.
import { useEffect } from "react";
import { RefreshCw } from "lucide-react";

import type { RuntimeEnvironment } from "../../bindings/RuntimeEnvironment";
import { Button, EmptyState, ErrorState, LoadingState, SimpleTable, StatusText, type SimpleTableColumn } from "../../ui/components";
import { loadEnvironments, useResearch } from "../research/store";
import { SettingsGroup, SettingsPage } from "./parts";

function kernelLabel(runtime: RuntimeEnvironment): string {
  if (!runtime.kernel) return "—";
  return runtime.kernel.kind === "python" ? "ipykernel" : `kernelspec ${runtime.kernel.name}`;
}

const COLUMNS: SimpleTableColumn<RuntimeEnvironment>[] = [
  {
    id: "environment",
    title: "Environment",
    width: "24%",
    render: (runtime) => (
      <>
        <div className="exp-settings__cell-title">{runtime.displayName}</div>
        {runtime.version && !runtime.displayName.includes(runtime.version) && <div className="ui-meta">{runtime.version}</div>}
      </>
    ),
  },
  {
    id: "status",
    title: "Status",
    width: "1%",
    render: (runtime) =>
      runtime.availability.kind === "available" ? (
        <StatusText status="succeeded" label="Available" />
      ) : (
        <StatusText status="unavailable" label="Unavailable" />
      ),
  },
  {
    id: "details",
    title: "Executable and kernel",
    render: (runtime) => (
      <>
        {runtime.executable && <code className="ui-code exp-settings__path">{runtime.executable}</code>}
        <div className="ui-meta">Kernel: {kernelLabel(runtime)}</div>
        {runtime.packages.length > 0 && <div className="ui-meta">{runtime.packages.map((p) => `${p.name} ${p.version}`).join(", ")}</div>}
        {runtime.availability.kind === "unavailable" && (
          <div className="ui-meta">
            {runtime.availability.reason}. {runtime.availability.guidance}
          </div>
        )}
      </>
    ),
  },
];

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
            <EmptyState size="compact" className="exp-settings__bleed" title="No environments found" description="Install Python, R, or Julia, then refresh." />
          ) : (
            <div data-testid="research-environments" aria-busy={loading}>
              <SimpleTable
                aria-label="Environments"
                className="exp-settings__table"
                columns={COLUMNS}
                rows={report.runtimes}
                rowKey={(runtime) => runtime.id}
                rowProps={(runtime) => ({ "data-runtime-id": runtime.id, "data-language": runtime.language })}
              />
            </div>
          )}
        </SettingsGroup>
      )}
    </SettingsPage>
  );
}
