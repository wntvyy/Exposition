import { FolderOpen, Play } from "lucide-react";
import { useId, useState, type FormEvent } from "react";

import type { Budgets } from "../../bindings/Budgets";
import type { ContextConsent } from "../../bindings/ContextConsent";
import type { ExecutionMode } from "../../bindings/ExecutionMode";
import type { PermissionProfile } from "../../bindings/PermissionProfile";
import type { TaskScope } from "../../bindings/TaskScope";
import { editors } from "../../app/editors";
import { registry } from "../../app/registry";
import { commands } from "../../lib/ipc";
import { Banner, Button, Section, Select, TextField } from "../../ui/components";
import { CONSENT_LABELS, EXECUTION_LABELS, PERMISSION_LABELS } from "./format";
import { agents, useAgents } from "./store";

type ScopeKind = "workspace" | "files" | "document";

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

/** Multi-line text input with a visible label (native textarea: IME safe). */
export function TextArea(props: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  rows?: number;
  placeholder?: string;
  code?: boolean;
  testId?: string;
}) {
  const id = useId();
  return (
    <div className="ui-field">
      <label className="ui-field__label" htmlFor={id}>
        {props.label}
      </label>
      <textarea
        id={id}
        className="ui-input agents-textarea"
        rows={props.rows ?? 3}
        value={props.value}
        placeholder={props.placeholder}
        spellCheck={!props.code}
        data-code={props.code || undefined}
        onChange={(e) => props.onChange(e.target.value)}
        data-testid={props.testId}
      />
    </div>
  );
}

function NumberField(props: { label: string; value: number; min: number; onChange: (v: number) => void }) {
  return (
    <TextField
      label={props.label}
      type="number"
      inputMode="numeric"
      value={String(props.value)}
      onChange={(raw) => props.onChange(Math.max(props.min, Number(raw) || props.min))}
    />
  );
}

const option = <K extends string>(labels: Record<K, string>) =>
  (Object.keys(labels) as K[]).map((id) => ({ id, label: labels[id] }));

export function TaskComposer() {
  const status = useAgents((s) => s.status);
  const workspace = useAgents((s) => s.workspace);
  const folder = workspace?.root ?? "";
  const [prompt, setPrompt] = useState("");
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

  async function submit(event?: FormEvent) {
    event?.preventDefault();
    if (!canStart || !scope) return;
    setBusy(true);
    const task = await agents.run(() =>
      commands.agentsTaskCreate({ prompt, workspaceRoot: folder, scope, permission, execution, consent, budgets }),
    );
    setBusy(false);
    if (task) {
      setPrompt("");
      agents.show("task", task.id);
    }
  }

  return (
    <form className="agents-stack" onSubmit={submit} aria-label="New agent task" data-testid="agents-composer">
      {!providerReady && status && (
        <Banner
          tone="warning"
          title="The model provider is not configured"
          actions={
            <Button size="sm" onPress={() => agents.openSettings(true)}>
              Provider settings
            </Button>
          }
        >
          Add an Anthropic API key before starting a task. It is stored only in {status.credentialStore}.
        </Banner>
      )}
      <TextArea
        label="Request"
        rows={5}
        value={prompt}
        placeholder="Describe what the agent should do in this project."
        onChange={setPrompt}
        testId="agents-prompt"
      />
      {workspace ? (
        <p className="agents-small" data-testid="agents-folder">
          <span className="agents-muted">Project folder: </span>
          <code className="agents-code">{workspace.root}</code>
        </p>
      ) : (
        <Banner
          tone="info"
          title="No project folder is open"
          actions={
            <Button size="sm" icon={FolderOpen} onPress={() => void registry.runCommand("workspace.openFolder")}>
              Open Folder…
            </Button>
          }
        >
          The agent works on the open project, through the same documents as your editors.
        </Banner>
      )}
      <Select
        label="Scope"
        value={scopeKind}
        onChange={setScopeKind}
        options={[
          { id: "workspace", label: "Whole project" },
          { id: "document", label: "One document" },
          { id: "files", label: "Selected files and folders" },
        ]}
        data-testid="agents-scope"
      />
      {scopeKind === "document" && (
        <TextField label="Document (relative to the project folder)" code value={document} onChange={setDocument} />
      )}
      {scopeKind === "files" && <TextArea label="Files and folders, one per line" code value={files} onChange={setFiles} />}
      <Select
        label="Document changes"
        value={permission}
        onChange={setPermission}
        options={option(PERMISSION_LABELS)}
        data-testid="agents-permission"
      />
      <Select label="Commands and code" value={execution} onChange={setExecution} options={option(EXECUTION_LABELS)} />
      <Select label="Content sent to the model provider" value={consent} onChange={setConsent} options={option(CONSENT_LABELS)} />
      <Section title="Budgets" collapsible defaultExpanded={false}>
        <div className="agents-grid">
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
      </Section>
      <div className="agents-row">
        <Button variant="ghost" onPress={() => agents.show("tasks")}>
          Cancel
        </Button>
        <Button
          variant="primary"
          icon={Play}
          isDisabled={!canStart}
          isPending={busy}
          onPress={() => void submit()}
          data-testid="agents-start"
        >
          Start task
        </Button>
      </div>
    </form>
  );
}
