// Research state for the workbench: workspace, environments, sessions, runs,
// and bounded run output. All work happens in Rust; this store mirrors events.
import { create } from "zustand";

import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import type { ArtifactRecord } from "../../bindings/ArtifactRecord";
import type { DocumentId } from "../../bindings/DocumentId";
import type { EnvironmentReport } from "../../bindings/EnvironmentReport";
import type { ResearchEvent } from "../../bindings/ResearchEvent";
import type { RunSource } from "../../bindings/RunSource";
import type { RunSummary } from "../../bindings/RunSummary";
import type { SessionInfo } from "../../bindings/SessionInfo";
import type { TextRange } from "../../bindings/TextRange";
import type { WorkspaceStatus } from "../../bindings/WorkspaceStatus";
import { commands, errorMessage, events, isCommandError } from "../../lib/ipc";
import { activeHandle, handleForPath } from "../editor";
import { requestWorkspaceTrust } from "../jobs/trust";
import { onProjectOp } from "../operations/client";
import { setupCommands } from "./ipc";
import {
  EMPTY_RUN_OUTPUT,
  applyEvent,
  appendEntries,
  mergeSnapshot,
  type RunOutput,
} from "./model";
import { activeUserRunId, lastUserRun, pickEnvironment } from "./runloop";

export interface ResearchState {
  workspace: WorkspaceStatus | null;
  environments: EnvironmentReport | null;
  environmentsLoading: boolean;
  environmentsError: string | null;
  selectedEnvironment: string | null;
  sessions: SessionInfo[];
  runs: RunSummary[];
  outputs: Record<string, RunOutput>;
  selectedRun: string | null;
  starting: boolean;
  error: string | null;
  code: string;
}

export const useResearch = create<ResearchState>(() => ({
  workspace: null,
  environments: null,
  environmentsLoading: false,
  environmentsError: null,
  selectedEnvironment: null,
  sessions: [],
  runs: [],
  outputs: {},
  selectedRun: null,
  starting: false,
  error: null,
  code: "",
}));

let started = false;

// Research events arrive in bursts during output floods. They are queued and
// applied in one store update per animation frame, so rendering never runs
// once per event.
let pending: ResearchEvent[] = [];
let flushScheduled = false;

function scheduleFlush(): void {
  if (flushScheduled) return;
  flushScheduled = true;
  const run = () => {
    flushScheduled = false;
    flushEvents();
  };
  if (typeof requestAnimationFrame === "function" && document.visibilityState === "visible") {
    requestAnimationFrame(run);
  } else {
    setTimeout(run, 16);
  }
}

/** Apply every queued event in one store update (exported for tests). */
export function flushEvents(): void {
  if (pending.length === 0) return;
  const batch = pending;
  pending = [];
  useResearch.setState((state) => {
    let model: { sessions: ResearchState["sessions"]; runs: ResearchState["runs"]; outputs: ResearchState["outputs"] } = state;
    let selectedRun = state.selectedRun;
    for (const event of batch) {
      // Follow the newest run of the user's session.
      if (event.kind === "runQueued" && event.run.owner.kind === "user" && !model.runs.some((r) => r.id === event.run.id)) {
        selectedRun = event.run.id;
      }
      model = applyEvent(model, event);
    }
    return { sessions: model.sessions, runs: model.runs, outputs: model.outputs, selectedRun };
  });
}

// When each session and run last changed (by event arrival order), so a
// snapshot fetched before a change never replaces the newer state.
let eventSerial = 0;
const changedAt = new Map<string, number>();

function noteChange(event: ResearchEvent): void {
  eventSerial += 1;
  switch (event.kind) {
    case "sessionChanged":
      changedAt.set(`session:${event.session.id}`, eventSerial);
      break;
    case "sessionRemoved":
      changedAt.set(`session:${event.sessionId}`, eventSerial);
      break;
    case "runQueued":
    case "runFinished":
      changedAt.set(`run:${event.run.id}`, eventSerial);
      break;
    case "runStarted":
      changedAt.set(`run:${event.runId}`, eventSerial);
      break;
    case "output":
      break;
  }
}

const changedSince = (kind: "session" | "run", since: number) => (id: string) =>
  (changedAt.get(`${kind}:${id}`) ?? 0) > since;

/** Subscribe to research events once and load the current state. */
export function startResearchTracking(): void {
  if (started) return;
  started = true;
  void events.research((event) => {
    noteChange(event);
    pending.push(event);
    if (event.kind === "runFinished" && event.run.status.kind === "failed" && document.visibilityState !== "visible") {
      notify({ tone: "danger", title: "Research run failed", body: event.run.status.message });
    }
    scheduleFlush();
  });
  void events.researchResync(() => void refreshAll());
  // Trust granted or revoked anywhere (another view's dialog, Settings): the
  // banner and the environment list (project interpreters are offered only
  // in a trusted folder) follow. A revoke also stops this folder's sessions
  // in the service; their state arrives as research events.
  void events.trustChanged(() => void refreshTrust());
  // Run records deleted or restored (the run history, Storage & History, an
  // agent): re-read the history; a shown run that is gone is no longer shown.
  onProjectOp((result) => {
    const runs = new Set(
      result.outcomes.flatMap((o) => (o.target.kind === "researchRun" || o.target.kind === "runOutputs" ? [o.target.run] : [])),
    );
    const touchesRuns = runs.size > 0 || result.outcomes.some((o) => o.target.kind === "recoverable");
    if (!touchesRuns) return;
    void refreshRuns().then(() => {
      const { selectedRun, runs: listed } = useResearch.getState();
      if (selectedRun && runs.has(selectedRun) && !listed.some((r) => r.id === selectedRun)) {
        useResearch.setState({ selectedRun: null });
      }
    });
  });
  void refreshAll();
}

/** Re-read the workspace's trust and, when it changed, the environments. */
export async function refreshTrust(): Promise<void> {
  // Only the trust flag (and what depends on it): sessions and runs follow
  // their own events (a revoke detaches this folder's sessions in the service).
  const before = useResearch.getState().workspace;
  let after: WorkspaceStatus | null;
  try {
    after = await commands.researchWorkspace();
  } catch (e) {
    useResearch.setState({ error: errorMessage(e) });
    return;
  }
  if (!before || !after || before.root !== after.root) {
    await refreshAll();
    return;
  }
  useResearch.setState({ workspace: after });
  if (before.trusted !== after.trusted) {
    // An error shown before the change (e.g. a refused start) no longer applies.
    useResearch.setState({ error: null });
    if (useResearch.getState().environments) await loadEnvironments(false);
  }
}

export async function refreshAll(): Promise<void> {
  try {
    const since = eventSerial;
    const [workspace, sessions] = await Promise.all([commands.researchWorkspace(), commands.researchSessions()]);
    const before = useResearch.getState();
    const switched = before.workspace?.root !== workspace?.root;
    // Another project has its own environments (its .venv) and remembered choice.
    const reload = switched && workspace !== null && (before.environments !== null || before.environmentsLoading);
    useResearch.setState((state) =>
      state.workspace?.root === workspace?.root
        ? { workspace, sessions: mergeSnapshot(state.sessions, sessions, (s) => s.id, changedSince("session", since)) }
        : { workspace, sessions, runs: [], outputs: {}, selectedRun: null, environments: null, selectedEnvironment: null },
    );
    if (reload) void loadEnvironments(false);
    if (workspace) await refreshRuns();
  } catch (e) {
    useResearch.setState({ error: errorMessage(e) });
  }
}

export async function refreshRuns(): Promise<void> {
  try {
    const since = eventSerial;
    const runs = await commands.researchRuns();
    useResearch.setState((state) => ({ runs: mergeSnapshot(state.runs, runs, (r) => r.id, changedSince("run", since)) }));
  } catch (e) {
    if (!(isCommandError(e) && e.code === "research.noWorkspace")) useResearch.setState({ error: errorMessage(e) });
  }
}

/** Increases with every environment load; an older load's result is dropped. */
let environmentsSerial = 0;

export async function loadEnvironments(refresh = false): Promise<void> {
  const serial = ++environmentsSerial;
  const root = useResearch.getState().workspace?.root ?? null;
  // A newer load (or another project) supersedes this one.
  const superseded = () => serial !== environmentsSerial || (useResearch.getState().workspace?.root ?? null) !== root;
  useResearch.setState({ environmentsLoading: true, environmentsError: null });
  try {
    const environments = await commands.researchEnvironments(refresh);
    // The environment remembered for this project (app-local; may be stale).
    const remembered = await setupCommands.selection().catch(() => null);
    if (superseded()) return;
    useResearch.setState((state) => ({
      environments,
      environmentsLoading: false,
      selectedEnvironment: pickEnvironment(environments, state.selectedEnvironment, remembered),
    }));
  } catch (e) {
    if (superseded()) return;
    useResearch.setState({ environmentsLoading: false, environmentsError: errorMessage(e) });
  }
}

/** The person chose an environment: select it and remember it for this project. */
export function chooseEnvironment(environmentId: string): void {
  useResearch.setState({ selectedEnvironment: environmentId });
  void setupCommands.select(environmentId).catch(() => undefined);
}

/** Research works in the folder opened by the workspace (Files view). */
export async function openProjectFolder(): Promise<void> {
  await registry.runCommand("workspace.openFolder");
  await refreshAll();
}

/** Ask for trust through the shared dialog; refreshes the workspace status. */
export async function trustWorkspace(): Promise<boolean> {
  const root = useResearch.getState().workspace?.root;
  if (!root) return false;
  const trusted = await requestWorkspaceTrust(root);
  try {
    useResearch.setState({ workspace: await commands.researchWorkspace() });
  } catch {
    // keep the previous status
  }
  return trusted;
}

/** The user's live session (agents use their own sessions). */
export function userSession(state: ResearchState = useResearch.getState()): SessionInfo | undefined {
  return state.sessions.find((s) => s.owner.kind === "user" && s.state.kind !== "stopped");
}

export async function startSession(environmentId: string): Promise<SessionInfo | null> {
  const workspace = useResearch.getState().workspace;
  if (!workspace) {
    useResearch.setState({ error: "Open a project folder before starting a research session." });
    return null;
  }
  if (!workspace.trusted && !(await trustWorkspace())) return null;
  useResearch.setState({ starting: true, error: null });
  try {
    // A session detached from its folder never runs again; clear it away first.
    for (const old of useResearch.getState().sessions.filter((s) => s.state.kind === "detached")) {
      await commands.researchSessionShutdown(old.id).catch(() => undefined);
    }
    const session = await commands.researchSessionStart({ environmentId });
    useResearch.setState((state) => ({
      starting: false,
      sessions: [...state.sessions.filter((s) => s.id !== session.id), session],
    }));
    return session;
  } catch (e) {
    // Not trusted: the banner and the trust prompt say so; no stale error text.
    const notTrusted = isCommandError(e) && e.code === "research.notTrusted";
    useResearch.setState({ starting: false, error: notTrusted ? null : errorMessage(e) });
    if (notTrusted) void trustWorkspace();
    return null;
  }
}

async function ensureSession(): Promise<SessionInfo | null> {
  const state = useResearch.getState();
  const existing = userSession(state);
  if (existing && existing.state.kind !== "detached") return existing;
  if (!state.selectedEnvironment) {
    useResearch.setState({ error: "Choose a research environment first." });
    return null;
  }
  return startSession(state.selectedEnvironment);
}

/**
 * Run code in the user's session (starting one if needed). `declaredInputs`
 * are project files the code reads (an experiment's inputs): Rust pins their
 * content in the run's inputs when it is dispatched.
 */
export async function execute(source: RunSource, declaredInputs?: string[]): Promise<RunSummary | null> {
  const session = await ensureSession();
  if (!session) return null;
  try {
    const run = await commands.researchExecute({
      sessionId: session.id,
      source,
      ...(declaredInputs && declaredInputs.length > 0 ? { declaredInputs } : {}),
    });
    useResearch.setState((state) => ({
      error: null,
      selectedRun: run.id,
      runs: state.runs.some((r) => r.id === run.id) ? state.runs : [run, ...state.runs],
    }));
    return run;
  } catch (e) {
    useResearch.setState({ error: errorMessage(e) });
    return null;
  }
}

export const runCode = (code: string) => execute({ kind: "input", code });
export const runFile = (path: string) => execute({ kind: "file", path });

let inputSelection: (() => string | null) | null = null;

/** The Research panel registers a reader for its own input's selection. */
export function setPanelSelectionReader(reader: (() => string | null) | null): void {
  inputSelection = reader;
}

/**
 * The active source editor's primary selection, pinned to the service revision
 * the editor shows (waits briefly for unacknowledged local edits to settle).
 */
async function editorSelection(): Promise<{ document: DocumentId; revision: number; range: TextRange } | null> {
  const handle = activeHandle();
  if (!handle) return null;
  for (let i = 0; i < 40 && handle.sync.pendingCount > 0; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  const range = handle.selection[0];
  if (handle.sync.pendingCount > 0 || !range || range.start === range.end) return null;
  const ordered = range.start <= range.end ? range : { start: range.end, end: range.start };
  return { document: handle.id, revision: handle.sync.revision, range: ordered };
}

/** Run the active editor selection, else the Research input's selection. */
export async function runSelection(): Promise<void> {
  const selection = await editorSelection();
  if (selection) {
    await execute({ kind: "selection", ...selection });
    return;
  }
  const panelSelection = inputSelection?.();
  if (panelSelection && panelSelection.trim()) {
    await runCode(panelSelection);
    return;
  }
  notify({
    tone: "info",
    title: "Nothing selected",
    body: "Select code in a Python, R, or Julia editor (or in the Research input), then run the selection.",
  });
}

async function sessionAction(action: (id: string) => Promise<unknown>, failure: string): Promise<void> {
  const session = userSession();
  if (!session) return;
  try {
    await action(session.id);
  } catch (e) {
    notify({ tone: "danger", title: failure, body: errorMessage(e) });
  }
}

export const interruptSession = () => sessionAction(commands.researchSessionInterrupt, "Could not interrupt the kernel");
export const restartSession = () => sessionAction(commands.researchSessionRestart, "Could not restart the kernel");
export const stopSession = () => sessionAction(commands.researchSessionShutdown, "Could not stop the session");

/** Stop: cancel the run the user's session executes now (interrupt, then restart if it ignores it). */
export async function stopRun(): Promise<void> {
  const state = useResearch.getState();
  const runId = activeUserRunId(userSession(state), state.runs);
  if (runId) await cancelRun(runId);
}

/** Run the selection of an open source (pinned to the revision the editor shows). */
export async function runSourceSelection(path: string): Promise<boolean> {
  const handle = handleForPath(path);
  if (!handle) return false;
  for (let i = 0; i < 40 && handle.sync.pendingCount > 0; i += 1) await new Promise((r) => setTimeout(r, 50));
  const range = handle.selection[0];
  if (!range || range.start === range.end || handle.sync.pendingCount > 0) return false;
  const ordered = range.start <= range.end ? range : { start: range.end, end: range.start };
  await execute({ kind: "selection", document: handle.id, revision: handle.sync.revision, range: ordered });
  return true;
}

/**
 * Rerun: the last run's kind of input against the current source — the same
 * file as it is now (open buffer first), the current selection of the same
 * document, or the Research input as it is now (the last input's code when
 * the input is empty). A direct user action: no extra approval.
 */
export async function rerun(): Promise<void> {
  const state = useResearch.getState();
  const last = lastUserRun(state.runs);
  if (!last) return;
  const origin = last.origin;
  switch (origin.kind) {
    case "file":
      await runFile(origin.path);
      return;
    case "selection": {
      if (origin.path && (await runSourceSelection(origin.path))) return;
      notify({
        tone: "info",
        title: "Nothing selected",
        body: origin.path ? `Select code in ${origin.path} to rerun the selection.` : "Select the code to rerun.",
      });
      return;
    }
    case "input": {
      if (state.code.trim()) {
        await runCode(state.code);
        return;
      }
      try {
        const manifest = await commands.researchRun(last.id);
        if (manifest.code.text?.trim()) {
          await runCode(manifest.code.text);
          return;
        }
      } catch {
        // fall through
      }
      notify({ tone: "info", title: "Nothing to rerun", body: "Type code in the Research input, then run it." });
    }
  }
}

export async function cancelRun(runId: string): Promise<void> {
  try {
    await commands.researchRunCancel(runId);
  } catch (e) {
    notify({ tone: "danger", title: "Could not cancel the run", body: errorMessage(e) });
  }
}

/** Select a run and load its retained output (history runs come from the stored transcript). */
export async function selectRun(runId: string): Promise<void> {
  useResearch.setState({ selectedRun: runId });
  const existing = useResearch.getState().outputs[runId];
  if (existing?.loaded && existing.entries.length > 0) return;
  try {
    let after: number | null = null;
    let output = existing ?? EMPTY_RUN_OUTPUT;
    for (let page = 0; page < 10; page += 1) {
      const result = await commands.researchRunOutput(runId, after, 500);
      output = appendEntries(output, result.entries);
      output = { ...output, loaded: true, truncated: output.truncated || result.truncated };
      if (result.entries.length < 500) break;
      after = result.entries[result.entries.length - 1]?.seq ?? null;
    }
    useResearch.setState((state) => ({ outputs: { ...state.outputs, [runId]: output } }));
  } catch (e) {
    useResearch.setState({ error: errorMessage(e) });
  }
}

const imageCache = new Map<string, string>();
const IMAGE_CACHE_LIMIT = 64;

/** Object URL for an artifact's bytes (fetched once, cached, revoked when evicted). */
export async function artifactUrl(runId: string, artifact: ArtifactRecord): Promise<string> {
  const cached = imageCache.get(artifact.id);
  if (cached) return cached;
  const bytes = await commands.researchArtifact(runId, artifact.id);
  const url = URL.createObjectURL(new Blob([bytes], { type: artifact.mediaType }));
  imageCache.set(artifact.id, url);
  if (imageCache.size > IMAGE_CACHE_LIMIT) {
    const [oldest] = imageCache.keys();
    if (oldest) {
      URL.revokeObjectURL(imageCache.get(oldest) as string);
      imageCache.delete(oldest);
    }
  }
  return url;
}

export async function artifactText(runId: string, artifact: ArtifactRecord): Promise<string> {
  const bytes = await commands.researchArtifact(runId, artifact.id);
  return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}
