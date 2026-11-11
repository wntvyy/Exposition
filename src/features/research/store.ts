// Research state for the workbench: workspace, environments, sessions, runs,
// and bounded run output. All work happens in Rust; this store mirrors events.
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";

import { notify } from "../../app/notifications";
import type { ArtifactRecord } from "../../bindings/ArtifactRecord";
import type { DocumentId } from "../../bindings/DocumentId";
import type { EnvironmentReport } from "../../bindings/EnvironmentReport";
import type { RunSource } from "../../bindings/RunSource";
import type { RunSummary } from "../../bindings/RunSummary";
import type { SessionInfo } from "../../bindings/SessionInfo";
import type { TextRange } from "../../bindings/TextRange";
import type { WorkspaceStatus } from "../../bindings/WorkspaceStatus";
import { commands, errorMessage, events, isCommandError } from "../../lib/ipc";
import { requestWorkspaceTrust } from "../jobs/trust";
import { EMPTY_RUN_OUTPUT, applyEvent, appendEntries, defaultInsertOffset, documentFormat, type RunOutput } from "./model";

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

/** Subscribe to research events once and load the current state. */
export function startResearchTracking(): void {
  if (started) return;
  started = true;
  void events.research((event) => {
    useResearch.setState((state) => {
      const next = applyEvent(state, event);
      const patch: Partial<ResearchState> = { sessions: next.sessions, runs: next.runs, outputs: next.outputs };
      // Follow the newest run of the user's session.
      if (event.kind === "runQueued" && event.run.owner.kind === "user" && !state.runs.some((r) => r.id === event.run.id)) {
        patch.selectedRun = event.run.id;
      }
      return patch;
    });
    if (event.kind === "runFinished" && event.run.status.kind === "failed" && document.visibilityState !== "visible") {
      notify({ tone: "danger", title: "Research run failed", body: event.run.status.message });
    }
  });
  void events.researchResync(() => void refreshAll());
  void refreshAll();
}

export async function refreshAll(): Promise<void> {
  try {
    const [workspace, sessions] = await Promise.all([commands.researchWorkspace(), commands.researchSessions()]);
    useResearch.setState({ workspace, sessions });
    if (workspace) await refreshRuns();
  } catch (e) {
    useResearch.setState({ error: errorMessage(e) });
  }
}

export async function refreshRuns(): Promise<void> {
  try {
    const runs = await commands.researchRuns();
    useResearch.setState({ runs });
  } catch (e) {
    if (!(isCommandError(e) && e.code === "research.noWorkspace")) useResearch.setState({ error: errorMessage(e) });
  }
}

export async function loadEnvironments(refresh = false): Promise<void> {
  useResearch.setState({ environmentsLoading: true, environmentsError: null });
  try {
    const environments = await commands.researchEnvironments(refresh);
    useResearch.setState((state) => {
      const usable = environments.runtimes.filter((r) => r.availability.kind === "available");
      const keep = environments.runtimes.some((r) => r.id === state.selectedEnvironment);
      return {
        environments,
        environmentsLoading: false,
        selectedEnvironment: keep ? state.selectedEnvironment : (usable[0]?.id ?? environments.runtimes[0]?.id ?? null),
      };
    });
  } catch (e) {
    useResearch.setState({ environmentsLoading: false, environmentsError: errorMessage(e) });
  }
}

/** TEMPORARY: choose the project folder until the workspace service provides it. */
export async function chooseWorkspaceFolder(): Promise<void> {
  const picked = await openDialog({ directory: true, multiple: false, title: "Open research project folder" });
  if (typeof picked !== "string") return;
  await setWorkspace(picked);
}

export async function setWorkspace(root: string): Promise<WorkspaceStatus | null> {
  try {
    const workspace = await commands.researchWorkspaceSet(root);
    useResearch.setState({ workspace, error: null, selectedRun: null, outputs: {} });
    await refreshRuns();
    return workspace;
  } catch (e) {
    useResearch.setState({ error: errorMessage(e) });
    return null;
  }
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
    const session = await commands.researchSessionStart({ environmentId, owner: { kind: "user" } });
    useResearch.setState((state) => ({
      starting: false,
      sessions: [...state.sessions.filter((s) => s.id !== session.id), session],
    }));
    return session;
  } catch (e) {
    useResearch.setState({ starting: false, error: errorMessage(e) });
    if (isCommandError(e) && e.code === "research.notTrusted") void trustWorkspace();
    return null;
  }
}

async function ensureSession(): Promise<SessionInfo | null> {
  const state = useResearch.getState();
  const existing = userSession(state);
  if (existing) return existing;
  if (!state.selectedEnvironment) {
    useResearch.setState({ error: "Choose a research environment first." });
    return null;
  }
  return startSession(state.selectedEnvironment);
}

export async function execute(source: RunSource): Promise<RunSummary | null> {
  const session = await ensureSession();
  if (!session) return null;
  try {
    const run = await commands.researchExecute({ sessionId: session.id, source, requester: { kind: "user" } });
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

/**
 * Selection source for "Run Selection in Research Session". The source editor
 * registers a provider for its active selection (document id, exact revision,
 * UTF-16 range); until then the panel's own input selection is used.
 */
export type SelectionProvider = () => { document: DocumentId; revision: number; range: TextRange } | null;
let selectionProvider: SelectionProvider | null = null;
let inputSelection: (() => string | null) | null = null;

export function setEditorSelectionProvider(provider: SelectionProvider | null): void {
  selectionProvider = provider;
}

export function setPanelSelectionReader(reader: (() => string | null) | null): void {
  inputSelection = reader;
}

export async function runSelection(): Promise<void> {
  const editorSelection = selectionProvider?.();
  if (editorSelection && editorSelection.range.start !== editorSelection.range.end) {
    await execute({ kind: "selection", ...editorSelection });
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
    body: "Select code in the Research input (or in a source editor that provides selections), then run again.",
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

export interface AttachTarget {
  document: DocumentId;
  path: string;
}

/** Open documents that accept attachments (Typst, LaTeX, Markdown). */
export async function attachTargets(): Promise<AttachTarget[]> {
  const docs = await commands.documentList();
  return docs
    .filter((d) => d.path && documentFormat(d.path) && !d.readOnly)
    .map((d) => ({ document: d.id, path: d.path as string }));
}

export async function attachArtifact(options: {
  run: RunSummary;
  artifact: ArtifactRecord;
  target: AttachTarget;
  mode: { kind: "insert" } | { kind: "update"; key: string };
  caption: string | null;
}): Promise<boolean> {
  const { run, artifact, target, mode, caption } = options;
  const format = documentFormat(target.path);
  if (!format) return false;
  try {
    const state = await commands.documentState(target.document);
    const placement =
      mode.kind === "insert"
        ? ({ kind: "insert", offset: defaultInsertOffset(state.text, format) } as const)
        : ({ kind: "update", attachmentKey: mode.key } as const);
    const result = await commands.artifactAttach({
      document: target.document,
      baseRevision: state.info.revision,
      artifact,
      run: run.id,
      placement,
      caption: caption && caption.trim() ? caption.trim() : null,
      attachmentKey: null,
      origin: { kind: "research", runId: run.id },
    });
    const what = artifact.kind === "table" ? "Table" : "Figure";
    notify({
      tone: "success",
      title: `${what} ${mode.kind === "insert" ? "inserted into" : "updated in"} ${target.path}`,
      body: `Attachment ${result.outcome.attachmentKey} (revision ${result.outcome.revision}). Undo is available in the document.`,
    });
    for (const warning of result.warnings) notify({ tone: "warning", title: "Check the document", body: warning });
    return true;
  } catch (e) {
    notify({ tone: "danger", title: "Could not attach the artifact", body: errorMessage(e) });
    return false;
  }
}
