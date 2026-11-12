import { KeyRound, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";

import type { ContextConsent } from "../../bindings/ContextConsent";
import type { Effort } from "../../bindings/Effort";
import type { ProviderKind } from "../../bindings/ProviderKind";
import { commands } from "../../lib/ipc";
import { Banner, Button, confirm, Dialog, Select, StatusPill, TextField } from "../../ui/components";
import { CONSENT_LABELS, EFFORT_LABELS } from "./format";
import { agents, useAgents } from "./store";

const option = <K extends string>(labels: Record<K, string>) =>
  (Object.keys(labels) as K[]).map((id) => ({ id, label: labels[id] }));

/**
 * Provider settings. The API key is write-only: it goes to OS credential
 * storage and the webview only ever learns whether a key is configured.
 */
export function ProviderSettingsDialog() {
  const open = useAgents((s) => s.settingsOpen);
  const status = useAgents((s) => s.status);
  const [key, setKey] = useState("");
  const [provider, setProvider] = useState<ProviderKind>("anthropic");
  const [model, setModel] = useState("claude-opus-5-5");
  const [effort, setEffort] = useState<Effort>("medium");
  const [consent, setConsent] = useState<ContextConsent>("scopeContent");
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) return;
    if (status) {
      setProvider(status.settings.provider);
      setModel(status.settings.model);
      setEffort(status.settings.effort);
      setConsent(status.settings.consent);
    }
    setKey("");
    setMessage(null);
    // Reset the form only when the dialog opens, not on every status change.
  }, [open]); // eslint-disable-line react-hooks/exhaustive-deps

  async function saveKey() {
    const value = key;
    if (!value.trim()) return;
    // Take the key out of the form before saving: the provider event can
    // report "Configured" before this call returns, and the key must not stay
    // in the webview meanwhile. A failed save gives it back for a retry.
    setKey("");
    setBusy(true);
    const next = await agents.run(() => commands.agentsProviderSetKey(value));
    setBusy(false);
    if (next) {
      agents.setStatus(next);
      setMessage("API key saved to the credential store.");
    } else {
      setKey(value);
    }
  }

  async function removeKey() {
    // Destructive and not undoable from here: the key is not shown again, so
    // removing it means pasting it in again. Keep is the default.
    const choice = await confirm<"keep" | "remove">({
      title: "Remove the API key?",
      message: `The key is deleted from ${status?.credentialStore ?? "the credential store"}. Agent tasks cannot use the model provider until you paste a key again; Exposition cannot show or restore the removed key.`,
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
    const next = await agents.run(() => commands.agentsProviderClearKey());
    setBusy(false);
    if (next) {
      agents.setStatus(next);
      setMessage("API key removed.");
      // The Remove button is now disabled; keep focus in the dialog, on the
      // field for a new key.
      requestAnimationFrame(() => document.querySelector<HTMLInputElement>("[data-testid=agents-key-input] input")?.focus());
    }
  }

  async function saveSettings() {
    setBusy(true);
    const next = await agents.run(() => commands.agentsProviderUpdate({ provider, model, effort, consent }));
    setBusy(false);
    if (next) {
      agents.setStatus(next);
      setMessage("Settings saved.");
    }
  }

  const configured = status?.configured ?? false;
  const providers = [
    { id: "anthropic" as const, label: "Anthropic Claude" },
    ...(status?.scriptedAvailable ? [{ id: "scripted" as const, label: "Scripted provider — development only" }] : []),
  ];

  return (
    <Dialog
      title="Agent provider"
      isOpen={open}
      onOpenChange={(next) => agents.openSettings(next)}
      footer={
        <>
          <Button variant="secondary" slot="close" data-testid="agents-settings-close">
            Close
          </Button>
          <Button variant="primary" isPending={busy} onPress={() => void saveSettings()} data-testid="agents-settings-save">
            Save settings
          </Button>
        </>
      }
    >
      <div className="agents-stack" data-testid="agents-settings">
        <Select label="Provider" value={provider} onChange={setProvider} options={providers} />
        <div className="agents-stack" role="group" aria-label="Credential">
          <p className="agents-row" data-testid="agents-key-status">
            <span>API key:</span>
            <StatusPill
              status={configured ? "succeeded" : "unavailable"}
              label={configured ? "Configured" : "Not configured"}
              size="sm"
            />
            {status && <span className="agents-muted agents-small">stored in {status.credentialStore}</span>}
          </p>
          {status?.credentialError && (
            <Banner tone="danger" title="Credential storage error">
              {status.credentialError}
            </Banner>
          )}
          <div className="agents-row agents-row--end">
            <TextField
              label={configured ? "Replace API key" : "API key"}
              type="password"
              autoComplete="off"
              code
              placeholder={configured ? "Paste a new key to replace it" : "Paste your Anthropic API key"}
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
          <p className="agents-muted agents-small">
            The key is kept only in the operating system's credential store. Exposition never shows it again, never writes it to
            project files or logs, and never passes it to commands it runs.
          </p>
        </div>
        <TextField label="Model" code value={model} onChange={setModel} description="For example claude-opus-5-5 (default)." />
        <Select label="Effort" value={effort} onChange={setEffort} options={option(EFFORT_LABELS)} />
        <Select label="Default content consent" value={consent} onChange={setConsent} options={option(CONSENT_LABELS)} />
        {message && (
          <p role="status" className="agents-muted" data-testid="agents-settings-message">
            {message}
          </p>
        )}
      </div>
    </Dialog>
  );
}
