// The one composer for a new task and a follow-up (design C), anchored at
// the bottom of the Agent view and the Agents page: the context line
// (project · scope, the folder on demand), the request, attachment chips,
// Attach, one Options button that summarizes model · effort · approvals ·
// document changes, and Send / Stop. Options opens ONE sheet above the
// composer with the tabs Model, Permissions, and Budgets.
//
// Drafts live in the shared drafts store (per conversation: the new task, or
// each task), and a new task's settings in a store of their own, so both
// views show the same draft. While a task is active the person may keep
// typing, but nothing is queued: Send waits until the task stops or asks.
//
// "Chat about it" on a question card marks the task's draft as being about
// that question (`drafts.chatAbout`): the composer shows the About chip,
// takes focus, and Send discusses the question (`sendDiscussion`) instead of
// continuing the task. While a question waits, that is the only message Send
// takes; the answer itself is sent from the card.
//
// Keys in the field: Enter sends through Send (same rules, once; nothing
// happens when Send is unavailable and the draft stays), Shift+Enter is a new
// line, Ctrl/Cmd+Enter also sends. An Enter that confirms an IME composition
// never sends. A large text paste becomes an attachment (Attachments.tsx);
// files dropped from the operating system on the chat are attached to this
// draft (dropAttach.ts) and start nothing.
import { ArrowUp, FolderOpen, Paperclip, SlidersHorizontal, Square } from "lucide-react";
import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { create } from "zustand";

import type { Budgets } from "../../bindings/Budgets";
import type { CreateTaskRequest } from "../../bindings/CreateTaskRequest";
import type { ExecutionMode } from "../../bindings/ExecutionMode";
import type { ProviderChoice } from "../../bindings/ProviderChoice";
import type { TaskId } from "../../bindings/TaskId";
import type { TaskScope } from "../../bindings/TaskScope";
import type { TaskSummary } from "../../bindings/TaskSummary";
import { editors } from "../../app/editors";
import { registry } from "../../app/registry";
import { commands, errorMessage } from "../../lib/ipc";
import { Badge, Banner, Button, Chip, IconButton, PopoverDialog, Tab, TabList, TabPanel, Tabs, TextArea } from "../../ui/components";
import { DraftAttachments, pasteHandler } from "./Attachments";
import { BudgetFields, budgetsCustom, BudgetUsage, DEFAULT_BUDGETS } from "./budgets";
import { agentDrop, useAgentDrop } from "./dropAttach";
import { drafts, NEW_TASK_DRAFT, useDraft, useDraftAbout } from "./drafts";
import { EFFORT_LABELS, environmentText, FILES_LABELS, isActive, PERMISSION_LABELS } from "./format";
import { ModelMenu } from "./ModelMenu";
import { connectionOf } from "./models";
import { PermissionsPanel, scopeLabel, SCOPE_LABELS, useProjectPermissions, type ScopeKind, type TaskSettingsDraft } from "./permissions";
import { sendDiscussion } from "./questions";
import { agents, useAgents } from "./store";

export { DEFAULT_BUDGETS };

/** Why Send is disabled while the task works (typing stays allowed; nothing is queued). */
export const SEND_WAITS = "You can send when the task stops or asks you something";

/** The field's keyboard hint (its accessible description). */
export const KEY_HINT = "Enter to send · Shift+Enter for a new line";

/** Why Send is disabled while a question waits and the draft is not about it. */
export const ANSWER_FIRST = "Answer the question above, or use Chat about it.";

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

// ---------------------------------------------------------------- new task settings

/** A new task's settings; `consent`/`document` stay `null` until chosen (then the defaults apply). */
interface NewTaskOptions extends Omit<TaskSettingsDraft, "consent" | "document"> {
  consent: TaskSettingsDraft["consent"] | null;
  document: string | null;
  budgets: Budgets;
}

const NEW_TASK_OPTIONS: NewTaskOptions = {
  approvalMode: "ask",
  permission: "reviewEdits",
  execution: "disabled",
  consent: null,
  scopeKind: "workspace",
  files: "",
  document: null,
  budgets: DEFAULT_BUDGETS,
};

/** Shared by the Agent view and the Agents page; back to the defaults after a task starts (native is never carried over). */
const useNewTaskOptions = create<NewTaskOptions>(() => NEW_TASK_OPTIONS);

export function resetComposerForTests() {
  useNewTaskOptions.setState(NEW_TASK_OPTIONS, true);
}

// ---------------------------------------------------------------- summary

/** "claude-opus-5-5 · High · Ask · Ask before each change · custom budgets" (pure; unit tested). */
export function optionsSummary(parts: { model: ProviderChoice; approvals: "Ask" | "Auto"; changes: string | null; customBudgets: boolean }): string {
  const model = parts.model.provider === "scripted" ? ["Scripted provider"] : [parts.model.model || "no model chosen", EFFORT_LABELS[parts.model.effort]];
  return [...model, parts.approvals, parts.changes, parts.customBudgets ? "custom budgets" : null]
    .filter((part): part is string => part !== null)
    .join(" · ");
}

type SheetTab = "model" | "permissions" | "budgets";

const sameChoice = (a: ProviderChoice, b: ProviderChoice) => a.provider === b.provider && a.model === b.model && a.effort === b.effort;

// ---------------------------------------------------------------- composer

export interface ComposerProps {
  /** The task to follow up; `null` for a new task. */
  task: TaskSummary | null;
  /** A new task was started (the Agents page keeps it on the page). */
  onCreated?: (id: TaskId) => void;
  /** The panel the Options sheet stays inside. */
  boundary?: Element | null;
  /** "Needs you" above the field. */
  above?: ReactNode;
}

export function Composer({ task, onCreated, boundary, above }: ComposerProps) {
  const status = useAgents((s) => s.status);
  const workspace = useAgents((s) => s.workspace);
  const allowances = useAgents((s) => (task ? (s.details[task.id]?.allowances.length ?? 0) : 0));
  const nativeAllowed = useAgents((s) => (task ? (s.details[task.id]?.allowances.some((a) => a.kind === "nativeCode") ?? false) : false));
  const options = useNewTaskOptions();
  const key = task ? task.id : NEW_TASK_DRAFT;
  const draft = useDraft(key);
  // Chat about it: the message discusses a waiting question (follow-ups only).
  const about = useDraftAbout(key);
  const discussing = task !== null && about !== null;
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [sheet, setSheet] = useState<SheetTab | null>(null);
  const [showPath, setShowPath] = useState(false);
  const anchorRef = useRef<HTMLFormElement>(null);
  const closedAt = useRef(0);
  // Synchronous guard: a second Enter or press arrives before React re-renders.
  const sending = useRef(false);
  // This composer as a target for files dropped from the operating system.
  const zone = useId();
  const dropping = useAgentDrop((s) => s.zone === zone);
  useEffect(() => () => agentDrop.leave(zone), [zone, key]);
  // The field takes focus whenever Chat about it is pressed (not when the view mounts).
  const focusedSeq = useRef(about?.seq ?? 0);
  useEffect(() => {
    if (!about || about.seq === focusedSeq.current) return;
    focusedSeq.current = about.seq;
    anchorRef.current?.querySelector<HTMLTextAreaElement>("textarea")?.focus();
  }, [about]);

  const folder = task ? task.spec.workspaceRoot : (workspace?.root ?? "");
  const active = task ? isActive(task.status) : false;
  const awaitingAnswer = task?.status.kind === "awaitingAnswer";

  // The new task's settings (a started task's are fixed in its spec).
  const settings: TaskSettingsDraft & { budgets: Budgets } = {
    ...options,
    consent: options.consent ?? status?.settings.consent ?? "scopeContent",
    document: options.document ?? editors.active()?.resource.path ?? "",
  };
  const setSettings = (patch: Partial<TaskSettingsDraft>) => useNewTaskOptions.setState(patch);
  const approvalMode = task ? task.spec.approvalMode : settings.approvalMode;
  const auto = approvalMode === "auto";
  const readOnly = settings.permission === "readOnly";
  const effectiveExecution: ExecutionMode = readOnly ? "disabled" : settings.execution;
  const projectPermissions = useProjectPermissions(auto && folder ? folder : null);
  const project = projectPermissions.state?.permissions ?? null;
  const autoReady = !auto || project !== null;

  const inherited: ProviderChoice = task
    ? task.spec.provider
    : status
      ? { provider: status.settings.provider, model: status.settings.model, effort: status.settings.effort }
      : { provider: "anthropic", model: "", effort: "medium" };
  // The provider this message would run on: the explicit choice, else the default (or the task's).
  const effective = draft.model ?? inherited;
  const modelReady = draft.model === null || draft.model.provider === "scripted" || draft.model.model.trim().length > 0;
  const providerReady = status !== null && connectionOf(status, effective.provider).configured;
  const scope = task ? task.spec.scope : buildScope(settings.scopeKind, settings.files, settings.document);
  const hasContent = draft.text.trim().length > 0 || draft.attachments.length > 0;
  // A discussion sends only its text (attachments stay in the draft) and may
  // be sent while the task waits for the answer; nothing else is sent then.
  const canSend = discussing
    ? draft.text.trim().length > 0 && !busy
    : task
      ? hasContent && !active && !busy && draft.ingesting === 0 && modelReady
      : hasContent && folder.trim().length > 0 && scope !== null && providerReady && autoReady && !busy && draft.ingesting === 0 && modelReady;

  // The Options summary and the native indication.
  const changes = task
    ? auto
      ? task.spec.permission === "readOnly"
        ? PERMISSION_LABELS.readOnly
        : project
          ? FILES_LABELS[project.files]
          : null
      : PERMISSION_LABELS[task.spec.permission]
    : auto
      ? project
        ? FILES_LABELS[project.files]
        : "No project permissions yet"
      : PERMISSION_LABELS[settings.permission];
  const execution = task ? task.spec.execution : auto ? "approveEach" : effectiveExecution;
  // Native runs apply: Ask with native commands, Auto with the project's native code, or an allowance for this task.
  const native = (auto ? execution === "approveEach" && project?.code === "native" : execution === "approveEach") || nativeAllowed;
  const customBudgets = budgetsCustom(task ? task.spec.budgets : settings.budgets);
  const summary = optionsSummary({ model: effective, approvals: auto ? "Auto" : "Ask", changes, customBudgets });

  async function submit(event?: FormEvent) {
    event?.preventDefault();
    if (!canSend || !scope || sending.current) return;
    sending.current = true;
    try {
      await send(scope);
    } finally {
      sending.current = false;
    }
  }

  async function send(scope: TaskScope) {
    if (task && about) {
      // One bounded discussion turn; on success the sent text and the About
      // are cleared by `sendDiscussion` (not `completeSend`).
      setBusy(true);
      setFailure(null);
      try {
        await sendDiscussion(task.id, about, draft.text, key);
      } catch (e) {
        setFailure(errorMessage(e));
      } finally {
        setBusy(false);
      }
      return;
    }
    // Exactly what is sent now; additions made while the request runs stay.
    const sent = {
      text: draft.text,
      attachments: draft.attachments.map((a) => a.id),
      model: draft.model,
      rejections: draft.rejections.map((r) => r.id),
    };
    setBusy(true);
    setFailure(null);
    try {
      if (task) {
        const switchTo = sent.model && !sameChoice(sent.model, task.spec.provider) ? sent.model : null;
        await commands.agentsTaskContinue(task.id, sent.text, null, sent.attachments, switchTo);
        drafts.completeSend(key, sent);
        return;
      }
      // In Auto the project's permissions decide; the task's own settings are
      // the widest ceiling (the backend still refuses what the project or the
      // task prohibits).
      const request: CreateTaskRequest = {
        prompt: sent.text,
        workspaceRoot: folder,
        scope,
        ...(auto ? { permission: "autoApply" as const, execution: "approveEach" as const } : { permission: settings.permission, execution: effectiveExecution }),
        consent: settings.consent,
        budgets: settings.budgets,
        approvalMode: settings.approvalMode,
        ...(sent.model ? { provider: sent.model } : {}),
        ...(sent.attachments.length ? { attachments: sent.attachments } : {}),
      };
      const created = await commands.agentsTaskCreate(request);
      drafts.completeSend(key, sent);
      useNewTaskOptions.setState(NEW_TASK_OPTIONS, true);
      setSheet(null);
      if (onCreated) onCreated(created.id);
      else agents.show("task", created.id);
    } catch (e) {
      // Nothing is lost: the text and attachments stay for another try.
      setFailure(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  // Enter sends, as Send does (Ctrl/Cmd+Enter too); Shift+Enter is a new line.
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Enter" || !(event.target instanceof HTMLTextAreaElement)) return;
    // The Enter that confirms an IME composition belongs to the input method.
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    const modified = event.ctrlKey || event.metaKey;
    // Shift+Enter: the field inserts the line break itself (at the caret, replacing a selection).
    if (!modified && (event.shiftKey || event.altKey)) return;
    // A control inside the field that already handled this Enter keeps it.
    if (event.defaultPrevented) return;
    // No line break; when Send is unavailable nothing happens and the draft stays.
    event.preventDefault();
    // A held key sends once.
    if (event.repeat) return;
    void submit();
  };

  const toggleSheet = () => {
    // A press on the button that just closed the sheet (an outside press) does not reopen it.
    if (sheet === null && Date.now() - closedAt.current < 250) return;
    setSheet(sheet ? null : "model");
  };
  const projectName = task ? (task.spec.workspaceRoot.split(/[\\/]/).filter(Boolean).pop() ?? task.spec.workspaceRoot) : (workspace?.name ?? "");
  const scopeText = task ? scopeLabel(task.spec.scope) : SCOPE_LABELS[settings.scopeKind];
  const nativeText = environmentText("native", { trusted: true, restrictedAvailable: true })[0]!.text;

  return (
    <form
      ref={anchorRef}
      className="agents-composer"
      onSubmit={(event) => void submit(event)}
      aria-label={task ? "Follow-up" : "New agent task"}
      data-testid="agents-composer"
      data-mode={task ? "follow-up" : "new"}
      data-active={active || undefined}
      data-agent-drop-zone={zone}
      data-draft-key={key}
      data-drop-target={dropping || undefined}
    >
      {above}
      {!task && !providerReady && status && (
        <Banner
          tone="warning"
          title="The model provider is not configured"
          actions={
            <Button size="sm" onPress={() => void registry.runCommand("agents.providerSettings")}>
              Provider settings
            </Button>
          }
        >
          Add {effective.provider === "openai" ? "an OpenAI" : "an Anthropic"} API key before starting a task. It is stored only in {status.credentialStore}.
        </Banner>
      )}
      {!task && !workspace && (
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
      {failure && (
        <Banner
          tone="danger"
          title={discussing ? "The message about the question was not sent" : task ? "The follow-up was not sent" : "The task was not started"}
          live
          data-testid={discussing ? "agents-discussion-error" : task ? "agents-follow-up-error" : "agents-composer-error"}
        >
          {failure}
        </Banner>
      )}
      {folder && (
        <div className="agents-composer__context" data-testid="agents-folder">
          <button
            type="button"
            className="agents-composer__context-button ui-meta"
            aria-expanded={showPath}
            title={folder}
            onClick={() => setShowPath((v) => !v)}
          >
            {[projectName, scopeText].filter(Boolean).join(" · ")}
          </button>
          {showPath && <code className="ui-code agents-folder-path">{folder}</code>}
        </div>
      )}
      {task && about && (
        <div className="agents-row">
          <Chip
            tone="accent"
            label={`About: ${about.prompt}`}
            onRemove={() => {
              setFailure(null);
              drafts.clearAbout(key);
            }}
            data-testid="agents-about"
          />
        </div>
      )}
      <div className="agents-composer__field" onPaste={pasteHandler(key)} onKeyDown={onKeyDown}>
        <TextArea
          label={task ? "Follow-up" : "Request"}
          rows={task ? 2 : 3}
          value={draft.text}
          placeholder={task ? "Reply or give the next instruction" : "Describe what the agent should do in this project."}
          onChange={(text) => drafts.setText(key, text)}
          description={KEY_HINT}
          testId={task ? "agents-follow-up-text" : "agents-prompt"}
        />
      </div>
      <DraftAttachments draftKey={key} sending={busy} />
      <div className="agents-composer__bar">
        <IconButton icon={Paperclip} size="sm" aria-label="Attach files…" onPress={() => void drafts.pickFiles(key)} data-testid="agents-attach" />
        <span className="agents-composer__options">
          <Button
            size="sm"
            variant="ghost"
            icon={SlidersHorizontal}
            className="agents-composer__options-button"
            aria-haspopup="dialog"
            aria-expanded={sheet !== null}
            onPress={toggleSheet}
            data-testid="agents-options"
          >
            <span className="agents-composer__options-name">Options:</span> <span className="agents-composer__options-summary">{summary}</span>
          </Button>
          {native && (
            <Badge tone="warning" title={nativeText} data-testid="agents-native-badge">
              Native
            </Badge>
          )}
        </span>
        <span className="agents-grow" />
        {active && task && (
          <Button size="sm" variant="secondary" icon={Square} onPress={() => void agents.run(() => commands.agentsTaskCancel(task.id))} data-testid="agents-cancel">
            Stop
          </Button>
        )}
        <Button
          size="sm"
          // One primary per surface: while the task works, its approval card holds it.
          variant={active ? "secondary" : "primary"}
          icon={ArrowUp}
          isDisabled={!canSend}
          onPress={() => void submit()}
          isPending={busy}
          data-testid={task ? "agents-continue" : "agents-start"}
        >
          Send
        </Button>
      </div>
      {active && !discussing && (
        <p className="ui-meta" data-testid="agents-send-waits">
          {awaitingAnswer ? ANSWER_FIRST : SEND_WAITS}
        </p>
      )}
      <PopoverDialog
        title="Options"
        hideTitle
        triggerRef={anchorRef}
        isOpen={sheet !== null}
        onOpenChange={(open) => {
          if (open) return;
          closedAt.current = Date.now();
          setSheet(null);
        }}
        placement="top"
        width="anchor"
        boundaryElement={boundary ?? undefined}
        maxHeight={600}
        className="agents-sheet-popover"
      >
        <Tabs selectedKey={sheet ?? "model"} onSelectionChange={(k) => setSheet(k as SheetTab)} className="agents-sheet">
          <TabList aria-label="Options">
            <Tab id="model">Model</Tab>
            <Tab id="permissions" count={allowances > 0 ? `${allowances} allowed` : undefined}>
              Permissions
            </Tab>
            <Tab id="budgets" count={customBudgets ? "custom" : undefined}>
              Budgets
            </Tab>
          </TabList>
          <TabPanel id="model" className="agents-sheet__panel" data-testid="agents-options-model">
            <ModelMenu
              value={draft.model}
              onChange={(model) => drafts.setModel(key, model)}
              status={status}
              running={task ? { provider: task.spec.provider.provider, model: task.spec.provider.model, effort: task.spec.provider.effort } : null}
              onManageProviders={() => void registry.runCommand("agents.providerSettings")}
              autoFocus
            />
            {task && task.usage.servedBy && task.spec.provider.provider !== "scripted" && task.usage.servedBy !== task.spec.provider.model && (
              <p className="ui-meta">The latest turn was served by {task.usage.servedBy}.</p>
            )}
          </TabPanel>
          <TabPanel id="permissions" className="agents-sheet__panel ui-inset-reset" data-testid="agents-options-permissions">
            {task ? (
              <PermissionsPanel task={task} projectPermissions={projectPermissions} />
            ) : (
              <PermissionsPanel draft={settings} onDraftChange={setSettings} projectPermissions={projectPermissions} />
            )}
          </TabPanel>
          <TabPanel id="budgets" className="agents-sheet__panel ui-inset-reset" data-testid="agents-options-budgets">
            {task ? <BudgetUsage task={task} /> : <BudgetFields value={settings.budgets} onChange={(budgets) => useNewTaskOptions.setState({ budgets })} />}
          </TabPanel>
        </Tabs>
      </PopoverDialog>
    </form>
  );
}

/**
 * The new task composer (no conversation yet). The Agent view shows the
 * created task in the panel; the Agents page passes `onCreated`.
 */
export function TaskComposer({ onCreated, boundary, above }: { onCreated?: (id: TaskId) => void; boundary?: Element | null; above?: ReactNode } = {}) {
  return <Composer task={null} onCreated={onCreated} boundary={boundary} above={above} />;
}
