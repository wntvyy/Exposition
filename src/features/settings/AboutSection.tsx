// About: version, data directory, platform, and a capability summary derived
// from live service status (toolchains, research discovery, provider, settings).
import { useEffect } from "react";

import { KeyValueList, SimpleTable, StatusText, type SimpleTableColumn, type Status } from "../../ui/components";
import { agents, useAgents } from "../agents/store";
import { loadToolchains, useJobs } from "../jobs/store";
import { loadEnvironments, useResearch } from "../research/store";
import { deriveCapabilities, type Capability, type CapabilityState } from "./model";
import { SettingsGroup, SettingsPage } from "./parts";
import { loadAppInfo, loadExecutionSettings, loadImages, useSettings } from "./store";

const CAPABILITY_STATUS: Record<CapabilityState, { status: Status; label: string }> = {
  available: { status: "succeeded", label: "Available" },
  unavailable: { status: "unavailable", label: "Unavailable" },
  checking: { status: "running", label: "Checking" },
};

const CAPABILITY_COLUMNS: SimpleTableColumn<Capability>[] = [
  { id: "capability", title: "Capability", width: "28%", isRowHeader: true, render: (c) => c.label },
  {
    id: "status",
    title: "Status",
    width: "1%",
    render: (c) => <StatusText status={CAPABILITY_STATUS[c.state].status} label={CAPABILITY_STATUS[c.state].label} />,
  },
  { id: "details", title: "Details", render: (c) => <span className="ui-meta">{c.detail}</span> },
];

const PLATFORM_LABELS: Record<string, string> = { windows: "Windows", macos: "macOS", linux: "Linux" };

export function AboutSection() {
  const app = useSettings((s) => s.app);
  const execution = useSettings((s) => s.execution);
  const images = useSettings((s) => s.images);
  const toolchains = useJobs((s) => s.toolchains);
  const environments = useResearch((s) => s.environments);
  const provider = useAgents((s) => s.status);

  useEffect(() => {
    void loadAppInfo();
    void loadExecutionSettings();
    if (useSettings.getState().images === null) void loadImages();
    if (useJobs.getState().toolchains === null) void loadToolchains(false);
    if (useResearch.getState().environments === null) void loadEnvironments(false);
    void agents.init();
  }, []);

  const capabilities = deriveCapabilities({ app, toolchains, environments, provider, execution, images });

  return (
    <SettingsPage title="About" testId="settings-about">
      <SettingsGroup title="Application">
        <KeyValueList
          items={[
            {
              key: "version",
              label: "Version",
              value: (
                <span data-testid="about-version">
                  {app ? `${app.name} ${app.version}${app.debugBuild ? " (debug build)" : ""}` : "…"}
                </span>
              ),
            },
            { key: "platform", label: "Platform", value: app ? `${PLATFORM_LABELS[app.platform] ?? app.platform} (${app.arch})` : "…" },
            {
              key: "dataDir",
              label: "Data directory",
              value: (
                <>
                  {app ? (
                    <code className="ui-code exp-settings__path" data-testid="about-data-dir">
                      {app.dataDir}
                    </code>
                  ) : (
                    <span>…</span>
                  )}
                  <div className="ui-meta">Settings, trust, recovery journals, and task history.</div>
                </>
              ),
            },
          ]}
        />
      </SettingsGroup>
      <SettingsGroup
        title="Capabilities"
        testId="capabilities"
        description="What works on this computer right now, from the services' own checks."
      >
        <SimpleTable
          aria-label="Capabilities"
          className="exp-settings__table"
          columns={CAPABILITY_COLUMNS}
          rows={capabilities}
          rowKey={(c) => c.id}
          rowProps={(c) => ({ "data-capability": c.id, "data-state": c.state })}
        />
      </SettingsGroup>
    </SettingsPage>
  );
}
