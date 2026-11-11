import { useEffect, useId, useRef, useState, type FormEvent } from "react";

import type { ContextConsent } from "../../bindings/ContextConsent";
import type { Effort } from "../../bindings/Effort";
import type { ProviderKind } from "../../bindings/ProviderKind";
import { commands } from "../../lib/ipc";
import { CONSENT_LABELS, EFFORT_LABELS } from "./format";
import { agents, useAgents } from "./store";

const KNOWN_MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];

/**
 * Provider settings. The API key is write-only: after saving, the webview only
 * learns whether a key is configured.
 */
export function ProviderSettingsDialog() {
  const open = useAgents((s) => s.settingsOpen);
  const status = useAgents((s) => s.status);
  const dialog = useRef<HTMLDialogElement>(null);
  const ids = { title: useId(), provider: useId(), key: useId(), model: useId(), models: useId(), effort: useId(), consent: useId() };
  const [key, setKey] = useState("");
  const [provider, setProvider] = useState<ProviderKind>("anthropic");
  const [model, setModel] = useState("claude-opus-5-5");
  const [effort, setEffort] = useState<Effort>("medium");
  const [consent, setConsent] = useState<ContextConsent>("scopeContent");
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    if (open && !element.open) {
      if (status) {
        setProvider(status.settings.provider);
        setModel(status.settings.model);
        setEffort(status.settings.effort);
        setConsent(status.settings.consent);
      }
      setKey("");
      setMessage(null);
      element.showModal();
    } else if (!open && element.open) {
      element.close();
    }
  }, [open, status]);

  async function saveKey(event: FormEvent) {
    event.preventDefault();
    if (!key.trim()) return;
    setBusy(true);
    const next = await agents.run(() => commands.agentsProviderSetKey(key));
    setBusy(false);
    setKey("");
    if (next) {
      agents.setStatus(next);
      setMessage("API key saved to the credential store.");
    }
  }

  async function removeKey() {
    setBusy(true);
    const next = await agents.run(() => commands.agentsProviderClearKey());
    setBusy(false);
    if (next) {
      agents.setStatus(next);
      setMessage("API key removed.");
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

  return (
    <dialog
      ref={dialog}
      className="agents-dialog"
      aria-labelledby={ids.title}
      onClose={() => agents.openSettings(false)}
      data-testid="agents-settings"
    >
      <h3 id={ids.title}>Agent provider</h3>
      <div className="agents-field">
        <label htmlFor={ids.provider}>Provider</label>
        <select id={ids.provider} value={provider} onChange={(e) => setProvider(e.target.value as ProviderKind)} data-testid="agents-provider-select">
          <option value="anthropic">Anthropic Claude</option>
          {status?.scriptedAvailable && <option value="scripted">Scripted provider — development only</option>}
        </select>
      </div>
      <section className="agents-key" aria-label="API key">
        <p data-testid="agents-key-status">
          API key:{" "}
          <strong>{configured ? "Configured" : "Not configured"}</strong>
          {status && <span className="agents-muted"> · stored in {status.credentialStore}</span>}
        </p>
        {status?.credentialError && (
          <p className="agents-error" role="alert">
            {status.credentialError}
          </p>
        )}
        <form onSubmit={saveKey} className="agents-row">
          <label htmlFor={ids.key} className="agents-visually-hidden">
            {configured ? "Replace API key" : "API key"}
          </label>
          <input
            id={ids.key}
            type="password"
            autoComplete="off"
            spellCheck={false}
            placeholder={configured ? "Paste a new key to replace it" : "Paste your Anthropic API key"}
            value={key}
            onChange={(e) => setKey(e.target.value)}
            data-testid="agents-key-input"
          />
          <button type="submit" className="agents-button agents-button-primary" disabled={busy || !key.trim()} data-testid="agents-key-save">
            Save key
          </button>
          <button type="button" className="agents-button" disabled={busy || !configured} onClick={() => void removeKey()} data-testid="agents-key-remove">
            Remove
          </button>
        </form>
        <p className="agents-muted">
          The key is kept only in the operating system's credential store. Exposition never shows it again, never
          writes it to project files or logs, and never passes it to commands it runs.
        </p>
      </section>
      <div className="agents-field">
        <label htmlFor={ids.model}>Model</label>
        <input id={ids.model} type="text" list={ids.models} value={model} spellCheck={false} onChange={(e) => setModel(e.target.value)} />
        <datalist id={ids.models}>
          {KNOWN_MODELS.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
      </div>
      <div className="agents-field">
        <label htmlFor={ids.effort}>Effort</label>
        <select id={ids.effort} value={effort} onChange={(e) => setEffort(e.target.value as Effort)}>
          {(Object.keys(EFFORT_LABELS) as Effort[]).map((level) => (
            <option key={level} value={level}>
              {EFFORT_LABELS[level]}
            </option>
          ))}
        </select>
      </div>
      <div className="agents-field">
        <label htmlFor={ids.consent}>Default content consent</label>
        <select id={ids.consent} value={consent} onChange={(e) => setConsent(e.target.value as ContextConsent)}>
          {(Object.keys(CONSENT_LABELS) as ContextConsent[]).map((c) => (
            <option key={c} value={c}>
              {CONSENT_LABELS[c]}
            </option>
          ))}
        </select>
      </div>
      {message && (
        <p role="status" className="agents-muted" data-testid="agents-settings-message">
          {message}
        </p>
      )}
      <div className="agents-actions">
        <button type="button" className="agents-button agents-button-primary" disabled={busy} onClick={() => void saveSettings()} data-testid="agents-settings-save">
          Save settings
        </button>
        <button type="button" className="agents-button" onClick={() => agents.openSettings(false)} data-testid="agents-settings-close">
          Close
        </button>
      </div>
    </dialog>
  );
}
