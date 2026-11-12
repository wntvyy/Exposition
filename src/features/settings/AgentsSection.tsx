// Agents: provider status and a way into the existing provider settings dialog
// (which owns key handling). The key itself is never shown or handled here.
import { useEffect } from "react";
import { Settings2 } from "lucide-react";

import { registry } from "../../app/registry";
import { Banner, Button, LoadingState, StatusPill } from "../../ui/components";
import { agents, useAgents } from "../agents/store";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";

const PROVIDER_LABELS: Record<string, string> = {
  anthropic: "Anthropic Claude",
  scripted: "Scripted provider (development only)",
};

export function AgentsSection() {
  const status = useAgents((s) => s.status);
  useEffect(() => {
    void agents.init();
  }, []);

  return (
    <SettingsPage
      title="Agents"
      testId="settings-agents"
      description="Agent tasks use the model provider configured here. The API key lives only in the operating system's credential store and is never shown."
      actions={
        <Button
          size="sm"
          variant="primary"
          icon={Settings2}
          onPress={() => void registry.runCommand("agents.providerSettings")}
          data-testid="open-provider-settings"
        >
          Provider settings…
        </Button>
      }
    >
      {!status ? (
        <LoadingState title="Loading provider status…" />
      ) : (
        <SettingsGroup title="Provider">
          <SettingRow label="Provider">
            <span>{PROVIDER_LABELS[status.settings.provider] ?? status.settings.provider}</span>
          </SettingRow>
          <SettingRow label="Model">
            <code className="exp-settings__path">{status.settings.model}</code>
          </SettingRow>
          <SettingRow
            label="API key"
            description={
              status.configured
                ? `Stored in ${status.credentialStore}.`
                : `Set it in Provider settings; it is kept in ${status.credentialStore}.`
            }
            testId="agents-key"
          >
            <StatusPill
              size="sm"
              status={status.configured ? "succeeded" : "unavailable"}
              label={status.configured ? "Configured" : "Not configured"}
            />
          </SettingRow>
          {status.credentialError && (
            <Banner tone="danger" title="Credential storage error">
              {status.credentialError}
            </Banner>
          )}
        </SettingsGroup>
      )}
    </SettingsPage>
  );
}
