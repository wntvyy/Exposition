// About: version, data directory, platform, and a capability summary derived
// from live service status (toolchains, research discovery, provider, settings).
import { useEffect } from "react";

import { StatusPill, type Status } from "../../ui/components";
import { agents, useAgents } from "../agents/store";
import { loadToolchains, useJobs } from "../jobs/store";
import { loadEnvironments, useResearch } from "../research/store";
import { deriveCapabilities, type CapabilityState } from "./model";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";
import { loadAppInfo, loadExecutionSettings, loadImages, useSettings } from "./store";

const CAPABILITY_STATUS: Record<CapabilityState, { status: Status; label: string }> = {
  available: { status: "succeeded", label: "Available" },
  unavailable: { status: "unavailable", label: "Unavailable" },
  checking: { status: "running", label: "Checking" },
};

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
        <SettingRow label="Version">
          <span data-testid="about-version">
            {app ? `${app.name} ${app.version}${app.debugBuild ? " (debug build)" : ""}` : "…"}
          </span>
        </SettingRow>
        <SettingRow label="Platform">
          <span>{app ? `${PLATFORM_LABELS[app.platform] ?? app.platform} (${app.arch})` : "…"}</span>
        </SettingRow>
        <SettingRow label="Data directory" description="Settings, trust, recovery journals, and task history.">
          {app ? (
            <code className="exp-settings__path" data-testid="about-data-dir">
              {app.dataDir}
            </code>
          ) : (
            <span>…</span>
          )}
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup
        title="Capabilities"
        testId="capabilities"
        description="What works on this computer right now, from the services' own checks."
      >
        <table className="exp-settings__table">
          <thead>
            <tr>
              <th scope="col" className="exp-settings__col-name">
                Capability
              </th>
              <th scope="col" className="exp-settings__col-fit">
                Status
              </th>
              <th scope="col">Details</th>
            </tr>
          </thead>
          <tbody>
            {capabilities.map((c) => (
              <tr key={c.id} data-capability={c.id} data-state={c.state}>
                <th scope="row">{c.label}</th>
                <td>
                  <StatusPill size="sm" status={CAPABILITY_STATUS[c.state].status} label={CAPABILITY_STATUS[c.state].label} />
                </td>
                <td className="exp-settings__muted">{c.detail}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </SettingsGroup>
    </SettingsPage>
  );
}
