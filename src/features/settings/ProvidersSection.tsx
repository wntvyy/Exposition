// Model providers: the one place provider connections are configured. Per
// provider: connection state, the API key (write-only: it goes to the OS
// credential store and the webview only learns whether one is configured),
// credential-store errors, OpenAI's organization/project, and the model
// list's state with a refresh. Then the defaults every new task inherits and
// the projects' Auto permissions. Everything goes through the agents service
// IPC; status changes arrive as provider events in the agents store.
import { ChevronDown, KeyRound, RefreshCw, Trash2 } from "lucide-react";
import { useEffect, useId, useState } from "react";

import type { ContextConsent } from "../../bindings/ContextConsent";
import type { ProviderKind } from "../../bindings/ProviderKind";
import type { ProviderSettings } from "../../bindings/ProviderSettings";
import type { ProviderStatus } from "../../bindings/ProviderStatus";
import { commands, errorMessage } from "../../lib/ipc";
import {
  Banner,
  Button,
  confirm,
  DialogTrigger,
  LoadingState,
  PopoverDialog,
  Select,
  StatusText,
  TextField,
} from "../../ui/components";
import { CONSENT_LABELS, relativeTime } from "../agents/format";
import { choiceSummary, defaultChoice, ModelMenuContent } from "../agents/ModelMenu";
import { catalogRows, PROVIDER_NAMES, useConnectionCatalog } from "../agents/models";
import { agents, useAgents } from "../agents/store";
import { SettingRow, SettingsGroup, SettingsPage } from "./parts";
import { ProjectPermissionsGroup } from "./ProjectPermissions";

type Keyed = "anthropic" | "openai";
type Catalog = ReturnType<typeof useConnectionCatalog>;

const KEY_PLACEHOLDER: Record<Keyed, string> = {
  anthropic: "Paste your Anthropic API key",
  openai: "Paste your OpenAI API key",
};

const BILLING: Record<Keyed, string> = {
  anthropic: "Requests use this API key and are billed to its Anthropic account.",
  openai: "Requests use this API key and are billed to its OpenAI API account (organization and project, when set).",
};

type Feedback = { tone: "done"; text: string } | { tone: "failed"; text: string } | null;

function FeedbackLine({ feedback }: { feedback: Feedback }) {
  if (!feedback) return null;
  return feedback.tone === "failed" ? (
    <Banner tone="danger" title="Not saved" className="exp-settings__notice" data-testid="agents-settings-error">
      {feedback.text}
    </Banner>
  ) : (
    <p role="status" className="ui-meta exp-settings__feedback" data-testid="agents-settings-message">
      {feedback.text}
    </p>
  );
}

async function updateSettings(status: ProviderStatus, patch: Partial<ProviderSettings>): Promise<void> {
  const next = await commands.agentsProviderUpdate({ ...status.settings, ...patch });
  agents.setStatus(next);
}

function focusKeyField(provider: Keyed) {
  requestAnimationFrame(() =>
    document.querySelector<HTMLInputElement>(`[data-testid=settings-provider-${provider}] [data-testid=agents-key-input] input`)?.focus(),
  );
}

/** The model list's state, as one row value. */
function CatalogState({ name, catalog }: { name: string; catalog: Catalog }) {
  const { connection, catalog: list, loading, error } = catalog;
  if (!connection.configured) return <span className="ui-meta">Listed once an API key is saved</span>;
  if (error) return <StatusText status="failed" label="Could not load the model list" detail={error} wrap />;
  if (!list) return <span className="ui-meta">{loading ? "Loading…" : "Not loaded"}</span>;
  const fetched = list.fetchedAtMs ? relativeTime(list.fetchedAtMs) : null;
  if (list.state === "stale") {
    return (
      <StatusText
        status="stale"
        label="May be out of date"
        detail={[fetched ? `Fetched ${fetched}.` : null, list.error].filter(Boolean).join(" ")}
        wrap
      />
    );
  }
  if (list.state === "unavailable") {
    return <StatusText status="failed" label={`${name} did not list its models`} detail={list.error ?? undefined} wrap />;
  }
  return (
    <span className="ui-meta">
      {[fetched ? `Listed by ${name} ${fetched}.` : `Listed by ${name}.`, `${list.models.length} models.`, list.metadataChecked ? `Support information checked ${list.metadataChecked}.` : null]
        .filter(Boolean)
        .join(" ")}
    </span>
  );
}

function ProviderGroup({ provider, status, catalog }: { provider: Keyed; status: ProviderStatus; catalog: Catalog }) {
  const name = PROVIDER_NAMES[provider];
  const { connection } = catalog;
  const configured = connection.configured;
  const [replacing, setReplacing] = useState(false);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState<Feedback>(null);
  const savedOrganization = status.settings.openai?.organization ?? "";
  const savedProject = status.settings.openai?.project ?? "";
  const [organization, setOrganization] = useState(savedOrganization);
  const [project, setProject] = useState(savedProject);
  // A change saved elsewhere (a provider event) replaces the fields.
  useEffect(() => {
    setOrganization(savedOrganization);
    setProject(savedProject);
  }, [savedOrganization, savedProject]);

  const showForm = !configured || replacing;

  async function saveKey() {
    const value = key;
    if (!value.trim()) return;
    // Take the key out of the form before saving: the provider event can
    // report "Configured" before this call returns, and the key must not stay
    // in the webview meanwhile. A failed save gives it back for a retry.
    setKey("");
    setBusy(true);
    setFeedback(null);
    try {
      agents.setStatus(await commands.agentsProviderSetKey(value, provider));
      setReplacing(false);
      setFeedback({ tone: "done", text: `${name} API key saved to the credential store.` });
    } catch (e) {
      setKey(value);
      setFeedback({ tone: "failed", text: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  }

  async function removeKey() {
    // Destructive and not undoable from here: the key is not shown again, so
    // removing it means pasting it in again. Keep is the default.
    const choice = await confirm<"keep" | "remove">({
      title: "Remove the API key?",
      message: `The ${name} key is deleted from ${status.credentialStore}. Agent tasks cannot use ${name} until you paste a key again; Exposition cannot show or restore the removed key.`,
      actions: [
        { id: "keep", label: "Keep key", variant: "secondary" },
        { id: "remove", label: "Remove key", variant: "danger" },
      ],
      cancelId: "keep",
      defaultId: "keep",
      tone: "danger",
    });
    if (choice !== "remove") return;
    setBusy(true);
    setFeedback(null);
    try {
      agents.setStatus(await commands.agentsProviderClearKey(provider));
      setReplacing(false);
      setFeedback({ tone: "done", text: `${name} API key removed.` });
      // The Remove button is gone now; focus the field for a new key.
      focusKeyField(provider);
    } catch (e) {
      setFeedback({ tone: "failed", text: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  }

  async function saveContext() {
    setBusy(true);
    setFeedback(null);
    try {
      await updateSettings(status, { openai: { organization: organization.trim() || null, project: project.trim() || null } });
      setFeedback({ tone: "done", text: "OpenAI organization and project saved." });
    } catch (e) {
      setFeedback({ tone: "failed", text: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  }

  const contextChanged = organization.trim() !== savedOrganization || project.trim() !== savedProject;

  return (
    <SettingsGroup
      title={name}
      actions={
        <StatusText
          status={connection.credentialError ? "failed" : configured ? "succeeded" : "unavailable"}
          label={connection.credentialError ? "Credential storage error" : configured ? "Connected" : "Not connected"}
        />
      }
      testId={`settings-provider-${provider}`}
    >
      {connection.credentialError && (
        <Banner tone="danger" title="Credential storage error" className="exp-settings__notice">
          {connection.credentialError}
        </Banner>
      )}
      {/* The key's state, its actions, and (when there is no key, or while
          replacing it) the field, all in the API key row. */}
      <SettingRow label="API key">
        <div className="exp-settings__key">
          <div className="exp-settings__key-line">
            <span data-testid="agents-key-status" data-provider={provider}>
              {configured ? `Configured — stored in ${status.credentialStore}` : "Not configured"}
            </span>
            {configured && (
              <span className="exp-settings__value-actions">
                {!replacing && (
                  <Button
                    size="sm"
                    icon={KeyRound}
                    isDisabled={busy}
                    onPress={() => {
                      setReplacing(true);
                      focusKeyField(provider);
                    }}
                    data-testid="agents-key-replace"
                  >
                    Replace key
                  </Button>
                )}
                <Button size="sm" variant="ghost" icon={Trash2} isDisabled={busy} onPress={() => void removeKey()} data-testid="agents-key-remove">
                  Remove
                </Button>
              </span>
            )}
          </div>
          {showForm && (
            <div className="exp-settings__key-form">
              <TextField
                aria-label={configured ? "New API key" : "API key"}
                type="password"
                autoComplete="off"
                code
                placeholder={KEY_PLACEHOLDER[provider]}
                value={key}
                onChange={setKey}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void saveKey();
                  } else event.continuePropagation();
                }}
                className="exp-settings__grow"
                data-testid="agents-key-input"
              />
              <Button icon={KeyRound} isDisabled={busy || !key.trim()} onPress={() => void saveKey()} data-testid="agents-key-save">
                Save key
              </Button>
              {replacing && (
                <Button
                  variant="ghost"
                  onPress={() => {
                    setReplacing(false);
                    setKey("");
                  }}
                >
                  Cancel
                </Button>
              )}
            </div>
          )}
        </div>
      </SettingRow>
      <SettingRow label="Model list" testId="settings-provider-catalog">
        <CatalogState name={name} catalog={catalog} />
        {configured && (
          <span className="exp-settings__value-actions">
            <Button
              size="sm"
              variant="ghost"
              icon={RefreshCw}
              isPending={catalog.loading}
              onPress={catalog.refresh}
              data-testid="settings-provider-refresh"
            >
              Refresh model list
            </Button>
          </span>
        )}
      </SettingRow>
      {provider === "openai" && (
        <>
          <SettingRow label="Organization ID" description="Optional. Requests are billed to this organization.">
            {({ labelId, descriptionId }) => (
              <TextField
                aria-labelledby={labelId}
                aria-describedby={descriptionId}
                code
                placeholder="org-…"
                value={organization}
                onChange={setOrganization}
                className="exp-settings__grow"
                data-testid="agents-openai-organization"
              />
            )}
          </SettingRow>
          <SettingRow label="Project ID" description="Optional. Requests are billed to this project.">
            {({ labelId, descriptionId }) => (
              <TextField
                aria-labelledby={labelId}
                aria-describedby={descriptionId}
                code
                placeholder="proj_…"
                value={project}
                onChange={setProject}
                className="exp-settings__grow"
                data-testid="agents-openai-project"
              />
            )}
          </SettingRow>
          <div className="exp-settings__key-actions">
            <Button size="sm" isDisabled={busy || !contextChanged} onPress={() => void saveContext()} data-testid="agents-openai-save">
              Save organization and project
            </Button>
          </div>
        </>
      )}
      <FeedbackLine feedback={feedback} />
      <SettingRow label="Billing" testId="settings-provider-billing">
        <div className="exp-settings__stack">
          <span>{BILLING[provider]}</span>
          {provider === "openai" && <span className="ui-meta">Sign in with ChatGPT is not available</span>}
        </div>
      </SettingRow>
    </SettingsGroup>
  );
}

function DefaultsGroup({ status, catalogs }: { status: ProviderStatus; catalogs: Record<Keyed, Catalog> }) {
  const choice = defaultChoice(status)!;
  const valueId = useId();
  const [open, setOpen] = useState(false);
  const [feedback, setFeedback] = useState<Feedback>(null);
  const list = choice.provider === "scripted" ? null : catalogs[choice.provider as Keyed].catalog;
  const descriptor = list ? catalogRows(list, choice).selected : null;

  async function save(patch: Partial<ProviderSettings>, done: string) {
    setFeedback(null);
    try {
      await updateSettings(status, patch);
      setFeedback({ tone: "done", text: done });
    } catch (e) {
      setFeedback({ tone: "failed", text: errorMessage(e) });
    }
  }

  return (
    <SettingsGroup
      title="Defaults for new tasks"
      testId="agents-default"
    >
      <SettingRow label="Model and effort" description="A task or a message can choose another model without changing this.">
        {({ labelId, descriptionId }) => (
          <DialogTrigger isOpen={open} onOpenChange={setOpen}>
            <Button aria-labelledby={`${labelId} ${valueId}`} aria-describedby={descriptionId} data-testid="agents-default-model">
              <span id={valueId} className="exp-settings__model-label">
                {choiceSummary(choice, descriptor)}
              </span>
              <ChevronDown size={14} aria-hidden />
            </Button>
            <PopoverDialog title="Default model" hideTitle placement="bottom start" className="exp-settings__model-popover">
              <ModelMenuContent
                value={choice}
                onChange={(next) => {
                  if (next) void save({ provider: next.provider, model: next.model, effort: next.effort }, "Default model saved.");
                }}
                status={status}
                onManageProviders={() => setOpen(false)}
                footer={false}
                autoFocus
                testId="agents-default-menu"
              />
            </PopoverDialog>
          </DialogTrigger>
        )}
      </SettingRow>
      <SettingRow label="Content sent to the model provider">
        {({ labelId }) => (
          <Select<ContextConsent>
            aria-labelledby={labelId}
            value={status.settings.consent}
            onChange={(consent) => void save({ consent }, "Default content saved.")}
            options={(Object.keys(CONSENT_LABELS) as ContextConsent[]).map((id) => ({ id, label: CONSENT_LABELS[id] }))}
            data-testid="agents-default-consent"
          />
        )}
      </SettingRow>
      <FeedbackLine feedback={feedback} />
    </SettingsGroup>
  );
}

export function ProvidersSection() {
  const status = useAgents((s) => s.status);
  useEffect(() => {
    void agents.init();
  }, []);
  // One model list per connection, shared by its group and the default's label.
  const anthropic = useConnectionCatalog(status, "anthropic");
  const openai = useConnectionCatalog(status, "openai");
  const catalogs: Record<Keyed, Catalog> = { anthropic, openai };

  return (
    <SettingsPage
      title="Model providers"
      description={
        status
          ? `API keys are stored only in ${status.credentialStore} and never shown. Exposition never writes them to project files or logs and never passes them to commands it runs.`
          : undefined
      }
      testId="settings-providers"
    >
      {!status ? (
        <LoadingState title="Loading provider status…" />
      ) : (
        <>
          {(["anthropic", "openai"] as const).map((provider: ProviderKind & Keyed) => (
            <ProviderGroup key={provider} provider={provider} status={status} catalog={catalogs[provider]} />
          ))}
          <DefaultsGroup status={status} catalogs={catalogs} />
        </>
      )}
      <ProjectPermissionsGroup />
    </SettingsPage>
  );
}
