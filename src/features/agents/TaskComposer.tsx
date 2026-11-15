import { FolderOpen, Play } from "lucide-react";
import { useState, type FormEvent } from "react";

import type { ApprovalMode } from "../../bindings/ApprovalMode";
import type { Budgets } from "../../bindings/Budgets";
import type { ContextConsent } from "../../bindings/ContextConsent";
import type { ExecutionMode } from "../../bindings/ExecutionMode";
import type { PermissionProfile } from "../../bindings/PermissionProfile";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskScope } from "../../bindings/TaskScope";
import { editors } from "../../app/editors";
import { registry } from "../../app/registry";
import { commands } from "../../lib/ipc";
import { Banner, Button, SegmentedControl, Section, Select, TextArea, TextField } from "../../ui/components";
import { APPROVAL_MODE_LABELS, CONSENT_LABELS, EXECUTION_LABELS, PERMISSION_LABELS } from "./format";
import { AutoProfile, useProjectPermissions } from "./permissions";
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

/**
 * New task form. The Agent view shows the created task in the panel; the
 * Agents page passes `onCreated`/`onCancel` to keep it on the page.
 */
export function TaskComposer({
  onCreated,
  onCancel,
}: { onCreated?: (id: TaskId) => void; onCancel?: () => void } = {}) {
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
  const [approvalMode, setApprovalMode] = useState<ApprovalMode>("ask");
  const [busy, setBusy] = useState(false);
  const auto = approvalMode === "auto";
  const projectPermissions = useProjectPermissions(auto && folder ? folder : null);
  // Auto needs the project's permissions first (chosen in the profile below).
  const autoReady = !auto || projectPermissions.state?.permissions != null;

  const scope = buildScope(scopeKind, files, document);
  const providerReady = status !== null && (status.settings.provider === "scripted" || status.configured);
  const canStart =
    prompt.trim().length > 0 && folder.trim().length > 0 && scope !== null && providerReady && autoReady && !busy;

  async function submit(event?: FormEvent) {
    event?.preventDefault();
    if (!canStart || !scope) return;
    setBusy(true);
    // In Auto the project's permissions decide; the task's own settings are
    // the widest ceiling (the backend still refuses what the project or the
    // task prohibits).
    const request = auto
      ? { prompt, workspaceRoot: folder, scope, permission: "autoApply" as const, execution: "approveEach" as const, consent, budgets, approvalMode }
      : { prompt, workspaceRoot: folder, scope, permission, execution, consent, budgets, approvalMode };
    const task = await agents.run(() => commands.agentsTaskCreate(request));
    setBusy(false);
    if (task) {
      setPrompt("");
      if (onCreated) onCreated(task.id);
      else agents.show("task", task.id);
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
        <p className="ui-meta" data-testid="agents-folder">
          <span>Project folder: </span>
          <code className="ui-code agents-folder-path">{workspace.root}</code>
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
          An agent task works only inside the open project folder.
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
      <div className="agents-stack">
        <span className="ui-field__label" aria-hidden>
          Approvals
        </span>
        <SegmentedControl<ApprovalMode>
          aria-label="Approvals"
          value={approvalMode}
          onChange={setApprovalMode}
          options={option(APPROVAL_MODE_LABELS)}
          data-testid="agents-approval-mode"
        />
        <p className="ui-meta" data-testid="agents-approval-mode-help">
          {auto
            ? "Proceeds within this project's permissions and asks only for anything beyond them."
            : "Asks before actions the settings below do not cover."}
        </p>
      </div>
      {auto ? (
        folder && <AutoProfile permissions={projectPermissions} />
      ) : (
        <>
          <Select
            label="Document changes"
            value={permission}
            onChange={setPermission}
            options={option(PERMISSION_LABELS)}
            data-testid="agents-permission"
          />
          <Select label="Commands and code" value={execution} onChange={setExecution} options={option(EXECUTION_LABELS)} />
        </>
      )}
      <Select label="Content sent to the model provider" value={consent} onChange={setConsent} options={option(CONSENT_LABELS)} />
      <Section title="Budgets" collapsible defaultExpanded={false} className="agents-section">
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
        <Button variant="ghost" onPress={() => (onCancel ? onCancel() : agents.show("tasks"))}>
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
