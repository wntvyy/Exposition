import { KeyRound, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";

import type { ContextConsent } from "../../bindings/ContextConsent";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import type { ProviderKind } from "../../bindings/ProviderKind";
import type { ProviderSettings } from "../../bindings/ProviderSettings";
import { commands, errorMessage } from "../../lib/ipc";
import { Banner, Button, confirm, Dialog, SegmentedControl, Section, Select, StatusPill, TextField } from "../../ui/components";
import { CONSENT_LABELS } from "./format";
import { ModelPicker } from "./ModelPicker";
import { connectionOf, KEYED_PROVIDERS, PROVIDER_NAMES } from "./models";
import { agents, useAgents } from "./store";

const option = <K extends string>(labels: Record<K, string>) =>
  (Object.keys(labels) as K[]).map((id) => ({ id, label: labels[id] }));

const KEY_PLACEHOLDER: Record<string, string> = {
  anthropic: "Paste your Anthropic API key",
  openai: "Paste your OpenAI API key",
};

function focusKeyField() {
  requestAnimationFrame(() => document.querySelector<HTMLInputElement>("[data-testid=agents-key-input] input")?.focus());
}

/**
 * Provider settings: one connection per provider (each with its own API key,
 * which is write-only: it goes to OS credential storage and the webview only
 * learns whether one is configured), and the default every new task inherits
 * (provider, model, effort, content consent). OpenAI here is API-key access
 * billed to the key's API account; ChatGPT-plan sign-in is not offered.
 */
export function ProviderSettingsDialog() {
  const open = useAgents((s) => s.settingsOpen);
  const status = useAgents((s) => s.status);
  const [key, setKey] = useState("");
  // The connection whose credential is shown (one at a time).
  const [connection, setConnection] = useState<ProviderKind>("anthropic");
  const [choice, setChoice] = useState<ProviderChoice>({ provider: "anthropic", model: "claude-opus-5-5", effort: "medium" });
  const [consent, setConsent] = useState<ContextConsent>("scopeContent");
  const [organization, setOrganization] = useState("");
  const [project, setProject] = useState("");
  const [message, setMessage] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [modelProblem, setModelProblem] = useState<string | null>(null);
  // A new picker per opening (it starts from the saved choice), and a reload
  // after a key change the status cannot show (a replaced key).
  const [session, setSession] = useState(0);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    if (!open) return;
    if (status) {
      const s = status.settings;
      setChoice({ provider: s.provider, model: s.model, effort: s.effort });
      setConsent(s.consent);
      setOrganization(s.openai?.organization ?? "");
      setProject(s.openai?.project ?? "");
      setConnection(s.provider === "openai" ? "openai" : "anthropic");
    }
    setKey("");
    setMessage(null);
    setFailure(null);
    setSession((n) => n + 1);
    // Reset the form only when the dialog opens, not on every status change.
  }, [open]); // eslint-disable-line react-hooks/exhaustive-deps

  const onProblemChange = useCallback((problem: string | null) => setModelProblem(problem), []);

  async function saveKey() {
    const value = key;
    if (!value.trim()) return;
    // Take the key out of the form before saving: the provider event can
    // report "Configured" before this call returns, and the key must not stay
    // in the webview meanwhile. A failed save gives it back for a retry.
    setKey("");
    setBusy(true);
    setFailure(null);
    try {
      const next = await commands.agentsProviderSetKey(value, connection);
      agents.setStatus(next);
      setRevision((n) => n + 1);
      setMessage(`${PROVIDER_NAMES[connection]} API key saved to the credential store.`);
    } catch (e) {
      setKey(value);
      setFailure(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  async function removeKey() {
    // Destructive and not undoable from here: the key is not shown again, so
    // removing it means pasting it in again. Keep is the default.
    const which = connection;
    const choiceMade = await confirm<"keep" | "remove">({
      title: "Remove the API key?",
      message: `The ${PROVIDER_NAMES[which]} key is deleted from ${status?.credentialStore ?? "the credential store"}. Agent tasks cannot use ${PROVIDER_NAMES[which]} until you paste a key again; Exposition cannot show or restore the removed key.`,
      actions: [
        { id: "keep", label: "Keep key", variant: "secondary" },
        { id: "remove", label: "Remove key", variant: "danger" },
      ],
      cancelId: "keep",
      defaultId: "keep",
      tone: "danger",
    });
    if (choiceMade !== "remove") return;
    setBusy(true);
    setFailure(null);
    try {
      const next = await commands.agentsProviderClearKey(which);
      agents.setStatus(next);
      setRevision((n) => n + 1);
      setMessage(`${PROVIDER_NAMES[which]} API key removed.`);
      // The Remove button is now disabled; keep focus in the dialog, on the
      // field for a new key.
      focusKeyField();
    } catch (e) {
      setFailure(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  async function saveSettings() {
    setBusy(true);
    setFailure(null);
    const settings: ProviderSettings = {
      ...choice,
      consent,
      openai: { organization: organization.trim() || null, project: project.trim() || null },
    };
    try {
      const next = await commands.agentsProviderUpdate(settings);
      agents.setStatus(next);
      setMessage("Settings saved.");
    } catch (e) {
      setFailure(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  const state = connectionOf(status, connection);
  const configured = state.configured;

  return (
    <Dialog
      title="Agent provider"
      size="lg"
      body="content"
      isOpen={open}
      onOpenChange={(next) => agents.openSettings(next)}
      footer={
        <>
          <Button variant="secondary" slot="close" data-testid="agents-settings-close">
            Close
          </Button>
          <Button
            variant="primary"
            isPending={busy}
            isDisabled={modelProblem !== null}
            onPress={() => void saveSettings()}
            data-testid="agents-settings-save"
          >
            Save settings
          </Button>
        </>
      }
    >
      <div className="agents-stack" data-testid="agents-settings">
        <Section title="Connections" className="ui-inset-reset" data-testid="agents-connections">
          <div className="agents-stack">
            <SegmentedControl<ProviderKind>
              aria-label="Connection"
              value={connection}
              onChange={(next) => {
                setConnection(next);
                setKey("");
              }}
              options={KEYED_PROVIDERS.map((id) => ({ id, label: PROVIDER_NAMES[id] }))}
              data-testid="agents-connection"
            />
            <p className="agents-row" data-testid="agents-key-status" data-provider={connection}>
              <span>API key:</span>
              <StatusPill
                status={configured ? "succeeded" : "unavailable"}
                label={configured ? "Configured" : "Not configured"}
                size="sm"
              />
              {status && <span className="ui-meta">stored in {status.credentialStore}</span>}
            </p>
            {state.credentialError && (
              <Banner tone="danger" title="Credential storage error">
                {state.credentialError}
              </Banner>
            )}
            <div className="agents-row agents-row--end">
              <TextField
                label={configured ? "Replace API key" : "API key"}
                type="password"
                autoComplete="off"
                code
                placeholder={configured ? undefined : KEY_PLACEHOLDER[connection]}
                value={key}
                onChange={setKey}
                className="agents-grow"
                data-testid="agents-key-input"
              />
              <Button variant="secondary" icon={KeyRound} isDisabled={busy || !key.trim()} onPress={() => void saveKey()} data-testid="agents-key-save">
                Save key
              </Button>
              <Button variant="ghost" icon={Trash2} isDisabled={busy || !configured} onPress={() => void removeKey()} data-testid="agents-key-remove">
                Remove
              </Button>
            </div>
            {connection === "openai" && (
              <div className="agents-grid agents-grid--wide" data-testid="agents-openai-context">
                <TextField
                  label="Organization ID (optional)"
                  code
                  placeholder="org-…"
                  value={organization}
                  onChange={setOrganization}
                  data-testid="agents-openai-organization"
                />
                <TextField
                  label="Project ID (optional)"
                  code
                  placeholder="proj_…"
                  value={project}
                  onChange={setProject}
                  data-testid="agents-openai-project"
                />
              </div>
            )}
            <p className="ui-meta">
              {connection === "openai"
                ? "Requests use this API key and are billed to its OpenAI API account (organization and project, when set; saved with the settings). "
                : ""}
              Exposition never shows the key again, never writes it to project files or logs, and never passes it to commands it runs.
            </p>
          </div>
        </Section>
        <Section title="Default for new tasks" className="ui-inset-reset" data-testid="agents-default">
          <div className="agents-stack">
            <ModelPicker
              key={session}
              value={choice}
              onChange={setChoice}
              status={status}
              revision={revision}
              onProblemChange={onProblemChange}
              onConfigure={(provider) => {
                if (provider !== "scripted") setConnection(provider);
                focusKeyField();
              }}
              testId="agents-default-model"
            />
            <Select label="Default content sent to the model provider" value={consent} onChange={setConsent} options={option(CONSENT_LABELS)} />
          </div>
        </Section>
        {modelProblem && (
          <p className="ui-meta" data-testid="agents-settings-problem">
            {modelProblem}
          </p>
        )}
        {failure && (
          <Banner tone="danger" title="Not saved" data-testid="agents-settings-error">
            {failure}
          </Banner>
        )}
        {message && (
          <p role="status" className="ui-meta" data-testid="agents-settings-message">
            {message}
          </p>
        )}
      </div>
    </Dialog>
  );
}
