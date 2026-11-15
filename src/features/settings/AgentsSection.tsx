// Agents: provider status and a way into the existing provider settings dialog
// (which owns key handling), and the projects' Auto permissions (inspect,
// change the open project's, revoke any). The key itself is never shown or
// handled here.
import { useEffect, useState } from "react";
import { Settings2 } from "lucide-react";

import type { PermissionChoice } from "../../bindings/PermissionChoice";
import type { ProjectPermissionsEntry } from "../../bindings/ProjectPermissionsEntry";
import { registry } from "../../app/registry";
import { errorMessage } from "../../lib/ipc";
import { Banner, Button, Dialog, ListRow, LoadingState, StatusText } from "../../ui/components";
import { profileSummary } from "../agents/format";
import { EnvironmentNotes, PermissionsEditor, useProjectPermissions } from "../agents/permissions";
import { agentPermissions } from "../agents/permissionsIpc";
import { agents, useAgents } from "../agents/store";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";

const PROVIDER_LABELS: Record<string, string> = {
  anthropic: "Anthropic Claude",
  scripted: "Scripted provider — development only",
};

/** Change the open project's permissions (same editor as the composer). */
function ChangeDialog({ root, onClose }: { root: string; onClose: () => void }) {
  const permissions = useProjectPermissions(root);
  const state = permissions.state;
  const [draft, setDraft] = useState<PermissionChoice | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const current = state?.permissions;
  const shown: PermissionChoice | null =
    draft ?? (current ? { files: current.files, code: current.code, setup: current.setup } : (state?.defaults ?? null));
  async function save() {
    if (!state || !shown) return;
    setBusy(true);
    try {
      await agentPermissions.set(state.root, shown);
      onClose();
    } catch (e) {
      setFailure(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      title="Change project permissions"
      body="content"
      isOpen
      onOpenChange={(open) => !open && onClose()}
      footer={
        <>
          <Button slot="close" variant="ghost">
            Cancel
          </Button>
          <Button variant="primary" isPending={busy} isDisabled={!shown} onPress={() => void save()} data-testid="settings-permissions-save">
            Save permissions
          </Button>
        </>
      }
    >
      {!state || !shown ? (
        <LoadingState title="Loading project permissions…" />
      ) : (
        <div className="agents-stack">
          <code className="ui-code exp-settings__path">{state.root}</code>
          <PermissionsEditor value={shown} onChange={setDraft} />
          <EnvironmentNotes code={shown.code} state={state} />
          <p className="ui-meta">Narrowing them stops this project's running Auto tasks; Ask tasks are not affected.</p>
          {failure && (
            <Banner tone="danger" title="Permissions were not changed">
              {failure}
            </Banner>
          )}
        </div>
      )}
    </Dialog>
  );
}

function ProjectPermissionsGroup() {
  const version = useAgents((s) => s.permissionsVersion);
  const [entries, setEntries] = useState<ProjectPermissionsEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [changing, setChanging] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    agentPermissions.list().then(
      (next) => live && (setEntries(next), setError(null)),
      (e: unknown) => live && setError(errorMessage(e)),
    );
    return () => {
      live = false;
    };
  }, [version]);

  async function revoke(entry: ProjectPermissionsEntry) {
    setBusy(entry.root);
    try {
      await agentPermissions.revoke(entry.root, entry.project);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <SettingsGroup
      title="Project permissions"
      count={entries ? String(entries.length) : undefined}
      description="What Auto tasks may do without asking, per project. Stored on this computer, never in the project; a copied or moved project has none."
      testId="settings-project-permissions"
    >
      {error && (
        <Banner tone="danger" title="Project permissions could not be changed" className="exp-settings__notice">
          {error}
        </Banner>
      )}
      {!entries ? (
        <LoadingState title="Loading project permissions…" />
      ) : entries.length === 0 ? (
        <p className="ui-meta" data-testid="settings-project-permissions-empty">
          No project has Auto permissions.
        </p>
      ) : (
        <div role="list" aria-label="Project permissions">
          {entries.map((entry) => (
            <ListRow
              key={`${entry.root}|${entry.project ?? ""}`}
              static
              codeTitle
              title={entry.root}
              meta={[profileSummary(entry.permissions), `Updated ${new Date(entry.permissions.updatedAtMs).toLocaleDateString()}`].join(" · ")}
              trailing={
                <div className="agents-row">
                  {!entry.exists && <StatusText status="unavailable" label="Folder no longer exists" />}
                  <Button
                    size="sm"
                    variant="ghost"
                    isDisabled={!entry.open}
                    onPress={() => setChanging(entry.root)}
                    aria-label={`Change permissions of ${entry.root}`}
                    data-testid="settings-permissions-change"
                  >
                    Change…
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    isPending={busy === entry.root}
                    onPress={() => void revoke(entry)}
                    aria-label={`Revoke permissions of ${entry.root}`}
                    data-testid="settings-permissions-revoke"
                  >
                    Revoke
                  </Button>
                </div>
              }
              role="listitem"
              data-testid="settings-project-permission"
              data-root={entry.root}
              data-open={entry.open ? "true" : "false"}
            />
          ))}
        </div>
      )}
      {entries && entries.some((e) => !e.open) && (
        <p className="ui-meta">Open a project to change its permissions; any project's can be revoked here.</p>
      )}
      {changing && <ChangeDialog root={changing} onClose={() => setChanging(null)} />}
    </SettingsGroup>
  );
}

export function AgentsSection() {
  const status = useAgents((s) => s.status);
  useEffect(() => {
    void agents.init();
  }, []);

  return (
    <SettingsPage
      title="Agents"
      testId="settings-agents"
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
            <code className="ui-code exp-settings__path">{status.settings.model}</code>
          </SettingRow>
          <SettingRow
            label="API key"
            description={
              status.configured
                ? `Stored only in ${status.credentialStore}; never shown.`
                : `Set it in Provider settings; it is stored only in ${status.credentialStore}.`
            }
            testId="agents-key"
          >
            <StatusText status={status.configured ? "succeeded" : "unavailable"} label={status.configured ? "Configured" : "Not configured"} />
          </SettingRow>
          {status.credentialError && (
            <Banner tone="danger" title="Credential storage error" className="exp-settings__notice">
              {status.credentialError}
            </Banner>
          )}
        </SettingsGroup>
      )}
      <ProjectPermissionsGroup />
    </SettingsPage>
  );
}
