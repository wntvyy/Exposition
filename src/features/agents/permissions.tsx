// Project permissions (Auto) and task allowances: the composer's Auto
// profile, the profile editor shared with Settings, and the task view's list
// of what the user approved for the rest of a task. Rust owns every decision;
// these views only show it and send the user's choices.
import { ShieldCheck, ShieldOff } from "lucide-react";
import { useEffect, useState } from "react";

import type { CodeAccess } from "../../bindings/CodeAccess";
import type { FilesAccess } from "../../bindings/FilesAccess";
import type { PermissionChoice } from "../../bindings/PermissionChoice";
import type { ProjectPermissionsState } from "../../bindings/ProjectPermissionsState";
import type { TaskAllowance } from "../../bindings/TaskAllowance";
import type { TaskId } from "../../bindings/TaskId";
import { errorMessage } from "../../lib/ipc";
import { Banner, Button, Checkbox, Select } from "../../ui/components";
import { ALLOWANCE_LABELS, CODE_LABELS, environmentText, FILES_LABELS, profileSummary } from "./format";
import { agentPermissions } from "./permissionsIpc";
import { agents, useAgents } from "./store";

const option = <K extends string>(labels: Record<K, string>) =>
  (Object.keys(labels) as K[]).map((id) => ({ id, label: labels[id] }));

/** The open project's permissions, re-read whenever any project's permissions change. */
export function useProjectPermissions(root: string | null): {
  state: ProjectPermissionsState | null;
  error: string | null;
  reload: () => void;
} {
  const version = useAgents((s) => s.permissionsVersion);
  const [state, setState] = useState<ProjectPermissionsState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let live = true;
    if (!root) {
      setState(null);
      return;
    }
    agentPermissions.get(root).then(
      (next) => {
        if (!live) return;
        setState(next);
        setError(null);
      },
      (e: unknown) => {
        if (!live) return;
        setState(null);
        setError(errorMessage(e));
      },
    );
    return () => {
      live = false;
    };
  }, [root, version, tick]);
  return { state, error, reload: () => setTick((t) => t + 1) };
}

/** Files, code, and setup as choices; setup needs native code. */
export function PermissionsEditor({
  value,
  onChange,
}: {
  value: PermissionChoice;
  onChange: (next: PermissionChoice) => void;
}) {
  return (
    <div className="agents-grid" data-testid="agents-permissions-editor">
      <Select<FilesAccess>
        label="Files"
        value={value.files}
        onChange={(files) => onChange({ ...value, files })}
        options={option(FILES_LABELS)}
        data-testid="agents-permissions-files"
      />
      <Select<CodeAccess>
        label="Code"
        value={value.code}
        onChange={(code) => onChange({ ...value, code, setup: code === "native" ? value.setup : false })}
        options={option(CODE_LABELS)}
        data-testid="agents-permissions-code"
      />
      <Checkbox
        isSelected={value.setup}
        isDisabled={value.code !== "native"}
        onChange={(setup) => onChange({ ...value, setup })}
        data-testid="agents-permissions-setup"
      >
        Install packages into the project environment (needs network; installers run natively)
      </Checkbox>
    </div>
  );
}

/**
 * What running code can actually access under `code` (an execution-policy
 * statement: a tinted banner, never only a tooltip).
 */
export function EnvironmentNotes({ code, state }: { code: CodeAccess; state: ProjectPermissionsState }) {
  const notes = environmentText(code, { trusted: state.trusted, restrictedAvailable: state.restrictedAvailable });
  return (
    <div className="agents-stack" data-testid="agents-permissions-environment" data-code={code}>
      {notes.map((note) => (
        <Banner key={note.text} tone={note.tone}>
          {note.text}
        </Banner>
      ))}
    </div>
  );
}

/**
 * The composer's Auto block: the project's permissions, or (without a record)
 * a concise suggested profile with "Use these permissions". The individual
 * choices are details on demand ("Choose…" / "Change…").
 */
export function AutoProfile({ permissions }: { permissions: ReturnType<typeof useProjectPermissions> }) {
  const { state, error, reload } = permissions;
  const [draft, setDraft] = useState<PermissionChoice | null>(null);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  if (error) {
    return (
      <Banner tone="danger" title="Project permissions are unavailable">
        {error}
      </Banner>
    );
  }
  if (!state) return <p className="ui-meta">Loading project permissions…</p>;
  const current = state.permissions;
  const editing = draft !== null;
  const shown: PermissionChoice =
    draft ?? (current ? { files: current.files, code: current.code, setup: current.setup } : state.defaults);

  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setFailure(null);
    try {
      await action();
      setDraft(null);
      reload();
    } catch (e) {
      setFailure(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }
  const save = () => act(() => agentPermissions.set(state.root, shown));
  const revoke = () => act(() => agentPermissions.revoke(state.root, state.project));

  return (
    <section
      className="agents-stack agents-permissions"
      aria-label="Project permissions"
      data-testid="agents-auto-profile"
      data-granted={current ? "true" : "false"}
    >
      {current ? (
        <p className="ui-meta" data-testid="agents-permissions-summary">
          Project permissions: {profileSummary(current)}
        </p>
      ) : (
        <p className="ui-meta" data-testid="agents-permissions-summary">
          This project has no Auto permissions yet. Choose them before starting an Auto task.
        </p>
      )}
      {editing ? (
        <PermissionsEditor value={shown} onChange={setDraft} />
      ) : (
        !current && <p className="ui-meta" data-testid="agents-permissions-suggested">Suggested: {profileSummary(shown)}</p>
      )}
      <EnvironmentNotes code={shown.code} state={state} />
      {failure && (
        <Banner tone="danger" title="Permissions were not changed">
          {failure}
        </Banner>
      )}
      <div className="agents-row">
        {(editing || !current) && (
          <Button size="sm" icon={ShieldCheck} isPending={busy} onPress={() => void save()} data-testid="agents-permissions-use">
            {current ? "Save permissions" : "Use these permissions"}
          </Button>
        )}
        {editing ? (
          <Button size="sm" variant="ghost" isDisabled={busy} onPress={() => setDraft(null)}>
            Cancel
          </Button>
        ) : (
          <Button size="sm" variant="ghost" isDisabled={busy} onPress={() => setDraft(shown)} data-testid="agents-permissions-change">
            {current ? "Change…" : "Choose…"}
          </Button>
        )}
        {current && !editing && (
          <Button
            size="sm"
            variant="ghost"
            icon={ShieldOff}
            isDisabled={busy}
            onPress={() => void revoke()}
            data-testid="agents-permissions-revoke"
          >
            Revoke
          </Button>
        )}
      </div>
    </section>
  );
}

/** What the user approved for the rest of a task, each revocable. */
const NO_ALLOWANCES: TaskAllowance[] = [];

export function TaskAllowances({ id }: { id: TaskId }) {
  // A stable empty list: a new array per read would re-render forever.
  const allowances = useAgents((s) => s.details[id]?.allowances ?? NO_ALLOWANCES);
  const [busy, setBusy] = useState(false);
  if (allowances.length === 0) return null;
  async function revoke(kind: (typeof allowances)[number]["kind"] | null) {
    setBusy(true);
    await agents.run(() => agentPermissions.revokeAllowance(id, kind));
    await agents.refetch(id);
    setBusy(false);
  }
  return (
    <div className="agents-row ui-meta" data-testid="agents-allowances">
      <span>Allowed for this task:</span>
      {allowances.map((a) => (
        <span key={a.kind} className="agents-row" data-testid="agents-allowance" data-kind={a.kind}>
          <span title={`Approved with “${a.approved}”`}>{ALLOWANCE_LABELS[a.kind]}</span>
          <Button
            size="sm"
            variant="ghost"
            isDisabled={busy}
            onPress={() => void revoke(a.kind)}
            aria-label={`Revoke ${ALLOWANCE_LABELS[a.kind]}`}
            data-testid="agents-allowance-revoke"
          >
            Revoke
          </Button>
        </span>
      ))}
    </div>
  );
}
