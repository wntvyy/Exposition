import { open } from "@tauri-apps/plugin-dialog";
import { useId, useState, type FormEvent } from "react";

import type { Budgets } from "../../bindings/Budgets";
import type { ContextConsent } from "../../bindings/ContextConsent";
import type { ExecutionMode } from "../../bindings/ExecutionMode";
import type { PermissionProfile } from "../../bindings/PermissionProfile";
import type { TaskScope } from "../../bindings/TaskScope";
import { editors } from "../../app/editors";
import { commands } from "../../lib/ipc";
import { CONSENT_LABELS, EXECUTION_LABELS, PERMISSION_LABELS } from "./format";
import { agents, useAgents } from "./store";

type ScopeKind = "workspace" | "files" | "document";

const FOLDER_KEY = "exposition.agents.projectFolder";

function rememberedFolder(): string {
  try {
    return localStorage.getItem(FOLDER_KEY) ?? "";
  } catch {
    return "";
  }
}

function rememberFolder(folder: string) {
  try {
    localStorage.setItem(FOLDER_KEY, folder);
  } catch {
    // Per-viewer convenience only.
  }
}

export const DEFAULT_BUDGETS: Budgets = {
  maxTurns: 40,
  maxOutputTokens: 400_000,
  maxInputTokens: 4_000_000,
  maxWallMs: 30 * 60 * 1000,
  maxToolCalls: 150,
  maxCommands: 20,
};

/** Build the scope from the composer fields; `null` when incomplete. */
export function buildScope(kind: ScopeKind, files: string, document: string): TaskScope | null {
  if (kind === "workspace") return { kind: "workspace" };
  if (kind === "document") return document.trim() ? { kind: "document", path: document.trim() } : null;
  const paths = files
    .split(/\r?\n|,/)
    .map((p) => p.trim())
    .filter(Boolean);
  return paths.length ? { kind: "files", paths } : null;
}

function NumberField(props: { label: string; value: number; min: number; onChange: (v: number) => void }) {
  const id = useId();
  return (
    <div className="agents-field agents-field-inline">
      <label htmlFor={id}>{props.label}</label>
      <input
        id={id}
        type="number"
        min={props.min}
        value={props.value}
        onChange={(e) => props.onChange(Math.max(props.min, Number(e.target.value) || props.min))}
      />
    </div>
  );
}

export function TaskComposer() {
  const status = useAgents((s) => s.status);
  const ids = { prompt: useId(), folder: useId(), scope: useId(), files: useId(), doc: useId(), perm: useId(), exec: useId(), consent: useId() };
  const [prompt, setPrompt] = useState("");
  const [folder, setFolder] = useState(rememberedFolder);
  const [scopeKind, setScopeKind] = useState<ScopeKind>("workspace");
  const [files, setFiles] = useState("");
  const [document, setDocument] = useState(() => editors.active()?.resource.path ?? "");
  const [permission, setPermission] = useState<PermissionProfile>("reviewEdits");
  const [execution, setExecution] = useState<ExecutionMode>("disabled");
  const [consent, setConsent] = useState<ContextConsent>(status?.settings.consent ?? "scopeContent");
  const [budgets, setBudgets] = useState<Budgets>(DEFAULT_BUDGETS);
  const [busy, setBusy] = useState(false);

  const scope = buildScope(scopeKind, files, document);
  const providerReady = status !== null && (status.settings.provider === "scripted" || status.configured);
  const canStart = prompt.trim().length > 0 && folder.trim().length > 0 && scope !== null && providerReady && !busy;

  async function browse() {
    const picked = await agents.run(() => open({ directory: true, multiple: false, title: "Project folder" }));
    if (typeof picked === "string") setFolder(picked);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!canStart || !scope) return;
    setBusy(true);
    const task = await agents.run(() =>
      commands.agentsTaskCreate({
        prompt,
        workspaceRoot: folder.trim(),
        scope,
        permission,
        execution,
        consent,
        budgets,
      }),
    );
    setBusy(false);
    if (task) {
      rememberFolder(folder.trim());
      setPrompt("");
      agents.show("task", task.id);
    }
  }

  return (
    <form className="agents-composer" onSubmit={submit} aria-label="New agent task" data-testid="agents-composer">
      <div className="agents-field">
        <label htmlFor={ids.prompt}>Request</label>
        <textarea
          id={ids.prompt}
          rows={5}
          value={prompt}
          placeholder="Describe what the agent should do in this project."
          onChange={(e) => setPrompt(e.target.value)}
          data-testid="agents-prompt"
        />
      </div>
      <div className="agents-field">
        <label htmlFor={ids.folder}>Project folder</label>
        <div className="agents-row">
          <input
            id={ids.folder}
            type="text"
            value={folder}
            spellCheck={false}
            onChange={(e) => setFolder(e.target.value)}
            data-testid="agents-folder"
          />
          <button type="button" className="agents-button" onClick={() => void browse()}>
            Browse…
          </button>
        </div>
      </div>
      <div className="agents-field">
        <label htmlFor={ids.scope}>Scope</label>
        <select
          id={ids.scope}
          value={scopeKind}
          onChange={(e) => setScopeKind(e.target.value as ScopeKind)}
          data-testid="agents-scope"
        >
          <option value="workspace">Whole project</option>
          <option value="document">One document</option>
          <option value="files">Selected files and folders</option>
        </select>
      </div>
      {scopeKind === "document" && (
        <div className="agents-field">
          <label htmlFor={ids.doc}>Document (relative to the project folder)</label>
          <input id={ids.doc} type="text" value={document} spellCheck={false} onChange={(e) => setDocument(e.target.value)} />
        </div>
      )}
      {scopeKind === "files" && (
        <div className="agents-field">
          <label htmlFor={ids.files}>Files and folders, one per line</label>
          <textarea id={ids.files} rows={3} value={files} spellCheck={false} onChange={(e) => setFiles(e.target.value)} />
        </div>
      )}
      <div className="agents-field">
        <label htmlFor={ids.perm}>Document changes</label>
        <select
          id={ids.perm}
          value={permission}
          onChange={(e) => setPermission(e.target.value as PermissionProfile)}
          data-testid="agents-permission"
        >
          {(Object.keys(PERMISSION_LABELS) as PermissionProfile[]).map((p) => (
            <option key={p} value={p}>
              {PERMISSION_LABELS[p]}
            </option>
          ))}
        </select>
      </div>
      <div className="agents-field">
        <label htmlFor={ids.exec}>Commands and code</label>
        <select id={ids.exec} value={execution} onChange={(e) => setExecution(e.target.value as ExecutionMode)}>
          {(Object.keys(EXECUTION_LABELS) as ExecutionMode[]).map((m) => (
            <option key={m} value={m}>
              {EXECUTION_LABELS[m]}
            </option>
          ))}
        </select>
      </div>
      <div className="agents-field">
        <label htmlFor={ids.consent}>Content sent to the model provider</label>
        <select id={ids.consent} value={consent} onChange={(e) => setConsent(e.target.value as ContextConsent)}>
          {(Object.keys(CONSENT_LABELS) as ContextConsent[]).map((c) => (
            <option key={c} value={c}>
              {CONSENT_LABELS[c]}
            </option>
          ))}
        </select>
      </div>
      <details className="agents-budgets">
        <summary>Budgets</summary>
        <div className="agents-budget-grid">
          <NumberField label="Model turns" min={1} value={budgets.maxTurns} onChange={(v) => setBudgets({ ...budgets, maxTurns: v })} />
          <NumberField label="Tool calls" min={0} value={budgets.maxToolCalls} onChange={(v) => setBudgets({ ...budgets, maxToolCalls: v })} />
          <NumberField label="Commands" min={0} value={budgets.maxCommands} onChange={(v) => setBudgets({ ...budgets, maxCommands: v })} />
          <NumberField
            label="Output tokens"
            min={1024}
            value={budgets.maxOutputTokens}
            onChange={(v) => setBudgets({ ...budgets, maxOutputTokens: v })}
          />
          <NumberField
            label="Minutes"
            min={1}
            value={Math.round(budgets.maxWallMs / 60000)}
            onChange={(v) => setBudgets({ ...budgets, maxWallMs: v * 60000 })}
          />
        </div>
      </details>
      {!providerReady && (
        <p className="agents-hint">
          Add an API key in{" "}
          <button type="button" className="agents-link" onClick={() => agents.openSettings(true)}>
            provider settings
          </button>{" "}
          before starting a task.
        </p>
      )}
      <div className="agents-actions">
        <button type="submit" className="agents-button agents-button-primary" disabled={!canStart} data-testid="agents-start">
          {busy ? "Starting…" : "Start task"}
        </button>
        <button type="button" className="agents-button" onClick={() => agents.show("tasks")}>
          Cancel
        </button>
      </div>
    </form>
  );
}
