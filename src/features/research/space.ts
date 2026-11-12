// Research space state shared by the Research page, the bottom Research panel,
// and the Files explorer's Research subsection: one listing (groups over
// graph identities), one selection, one set of open research sources. Runs
// and sessions stay in `store.ts` (one set of sessions for every surface).
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";

import { editors } from "../../app/editors";
import { notify } from "../../app/notifications";
import { pages } from "../../app/pages";
import type { ArtifactRecord } from "../../bindings/ArtifactRecord";
import type { EntityId } from "../../bindings/EntityId";
import type { ExperimentDetail } from "../../bindings/ExperimentDetail";
import type { ExperimentInput } from "../../bindings/ExperimentInput";
import type { ImportMode } from "../../bindings/ImportMode";
import type { OperationId } from "../../bindings/OperationId";
import type { PromotionsState } from "../../bindings/PromotionsState";
import type { ResearchListing } from "../../bindings/ResearchListing";
import type { RetainedResult } from "../../bindings/RetainedResult";
import type { RetainState } from "../../bindings/RetainState";
import type { RunSummary } from "../../bindings/RunSummary";
import type { SpaceGroup } from "../../bindings/SpaceGroup";
import { commands, errorMessage, events, isCommandError } from "../../lib/ipc";
import { graphEvents } from "../../lib/graphIpc";
import { confirmUnsaved } from "../../ui/components";
import { fileKey, handleForPath, saveDocument } from "../editor/documentClient";
import { researchCommands } from "./ipc";
import { execute } from "./store";

export const RESEARCH_PAGE_ID = "research";

/** Something selected in the Research explorer. */
export type ResearchItem =
  | { kind: "file"; path: string }
  | { kind: "reference"; entity: EntityId }
  | { kind: "experiment"; entity: EntityId | null }
  | { kind: "result"; entity: EntityId }
  | { kind: "workflow"; entity: EntityId }
  | { kind: "run"; runId: string };

export interface SpaceState {
  /** The listing has been requested at least once. */
  loaded: boolean;
  loading: boolean;
  enabling: boolean;
  error: string | null;
  listing: ResearchListing | null;
  selected: ResearchItem | null;
  /** Research sources open in the Research page editor (paths), in tab order. */
  openSources: string[];
  activeSource: string | null;
  promotions: PromotionsState | null;
  /** The experiment shown in the inspector (`null` while loading or for a new one). */
  experiment: ExperimentDetail | null;
}

export const useSpace = create<SpaceState>(() => ({
  loaded: false,
  loading: false,
  enabling: false,
  error: null,
  listing: null,
  selected: null,
  openSources: [],
  activeSource: null,
  promotions: null,
  experiment: null,
}));

export function itemKey(item: ResearchItem): string {
  switch (item.kind) {
    case "file":
      return `file:${item.path}`;
    case "run":
      return `run:${item.runId}`;
    case "experiment":
      return `experiment:${item.entity ?? "new"}`;
    default:
      return `${item.kind}:${item.entity}`;
  }
}

/** Parse an explorer key back into an item (keys are produced by `itemKey`). */
export function parseItemKey(key: string): ResearchItem | null {
  const at = key.indexOf(":");
  if (at < 0) return null;
  const kind = key.slice(0, at);
  const rest = key.slice(at + 1);
  switch (kind) {
    case "file":
      return { kind: "file", path: rest };
    case "run":
      return { kind: "run", runId: rest };
    case "experiment":
      return { kind: "experiment", entity: rest === "new" ? null : rest };
    case "reference":
    case "result":
    case "workflow":
      return { kind, entity: rest };
    default:
      return null;
  }
}

let refreshTimer: ReturnType<typeof setTimeout> | null = null;
let sequence = 0;

export async function refreshListing(): Promise<void> {
  const seq = ++sequence;
  useSpace.setState({ loading: true });
  try {
    const listing = await researchCommands.listing();
    if (seq !== sequence) return;
    useSpace.setState({ listing, loading: false, loaded: true, error: null });
  } catch (e) {
    if (seq !== sequence) return;
    const noWorkspace = isCommandError(e) && e.code === "research.noWorkspace";
    useSpace.setState({ loading: false, loaded: true, listing: null, error: noWorkspace ? null : errorMessage(e) });
  }
}

/** Coalesce refreshes (graph events arrive in bursts). */
export function scheduleRefresh(delay = 150): void {
  if (refreshTimer) clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    void refreshListing();
    void refreshPromotions();
  }, delay);
}

let tracking = false;

/** Follow graph changes and project switches (idempotent). */
export function startSpaceTracking(): void {
  if (tracking) return;
  tracking = true;
  try {
    void graphEvents.graph((event) => {
      if (event.kind === "changed" || event.kind === "status") scheduleRefresh();
    });
    void graphEvents.graphResync(() => scheduleRefresh());
    void events.researchResync(() => {
      useSpace.setState({ selected: null, openSources: [], activeSource: null, experiment: null });
      scheduleRefresh(0);
    });
    void events.workspace((event) => {
      if (event.kind === "closed") {
        useSpace.setState({ listing: null, selected: null, openSources: [], activeSource: null, experiment: null, promotions: null });
      }
    });
  } catch {
    // Not running inside the desktop shell (unit tests).
  }
  void refreshListing();
  void refreshPromotions();
}

export async function refreshPromotions(): Promise<void> {
  try {
    useSpace.setState({ promotions: await researchCommands.promotions() });
  } catch {
    useSpace.setState({ promotions: null });
  }
}

/** Enable Research for the open project (creates the space; runs nothing). */
export async function enableResearch(): Promise<boolean> {
  useSpace.setState({ enabling: true, error: null });
  try {
    const space = await researchCommands.enable();
    useSpace.setState({ enabling: false });
    notify({ tone: "success", title: "Research is enabled", body: `Sources, data, and notes live in ${space.root}.` });
    await refreshListing();
    return true;
  } catch (e) {
    useSpace.setState({ enabling: false, error: errorMessage(e) });
    return false;
  }
}

/** Select an explorer item (files also open in the Research editor). */
export function selectItem(item: ResearchItem | null): void {
  useSpace.setState({ selected: item, experiment: null });
  if (item?.kind === "file") openSource(item.path);
  if (item?.kind === "experiment" && item.entity) void loadExperiment(item.entity);
}

/** Show the Research page at an item (Files explorer, notifications). */
export function revealInResearch(item: ResearchItem | null): void {
  pages.show(RESEARCH_PAGE_ID);
  if (item) selectItem(item);
}

export function openSource(path: string): void {
  useSpace.setState((s) => ({
    openSources: s.openSources.includes(path) ? s.openSources : [...s.openSources, path],
    activeSource: path,
  }));
}

/**
 * Close a research source tab. The document stays open when an Editor tab
 * shows it (one document model); otherwise unsaved changes are asked about
 * and the service document is closed.
 */
export async function closeSource(path: string): Promise<boolean> {
  const inEditor = editors.all().some((e) => e.resource.key === fileKey(path));
  const handle = handleForPath(path);
  if (!inEditor && handle) {
    await handle.sync.idle();
    if (handle.isDirty) {
      const choice = await confirmUnsaved(handle.name);
      if (choice === "cancel") return false;
      if (choice === "save" && !(await saveDocument(handle))) return false;
    }
    try {
      await commands.documentClose(handle.id);
    } catch {
      // Already closed.
    }
  }
  useSpace.setState((s) => {
    const index = s.openSources.indexOf(path);
    const openSources = s.openSources.filter((p) => p !== path);
    const activeSource =
      s.activeSource === path ? (openSources[Math.min(index, openSources.length - 1)] ?? null) : s.activeSource;
    return { openSources, activeSource };
  });
  return true;
}

export async function createSourceFile(group: SpaceGroup, name: string): Promise<string | null> {
  try {
    const file = await researchCommands.createFile({ group, name });
    await refreshListing();
    selectItem({ kind: "file", path: file.path });
    return file.path;
  } catch (e) {
    notify({ tone: "danger", title: `Could not create ${name}`, body: errorMessage(e) });
    return null;
  }
}

/** Import a file the person picks (copy into the space, or reference it read-only). */
export async function importInto(group: SpaceGroup, mode: ImportMode): Promise<void> {
  let source: string | null = null;
  try {
    const picked = await openDialog({ multiple: false, directory: false, title: mode === "copy" ? "Copy a file into the research space" : "Reference a file (read-only)" });
    source = typeof picked === "string" ? picked : null;
  } catch (e) {
    notify({ tone: "danger", title: "Could not choose a file", body: errorMessage(e) });
    return;
  }
  if (!source) return;
  await importPath(group, source, mode);
}

export async function importPath(group: SpaceGroup, source: string, mode: ImportMode): Promise<void> {
  try {
    const outcome = await researchCommands.importFile({ group, source, mode });
    await refreshListing();
    if (outcome.kind === "copied") {
      notify({ tone: "success", title: `Copied ${outcome.file.name}`, body: `Origin recorded: ${outcome.file.origin?.source ?? source}` });
      selectItem({ kind: "file", path: outcome.file.path });
    } else {
      notify({ tone: "success", title: `Referenced ${outcome.reference.label}`, body: "Read-only reference; it is never copied, crawled, or run." });
      selectItem({ kind: "reference", entity: outcome.reference.entity });
    }
  } catch (e) {
    notify({ tone: "danger", title: "Could not import the file", body: errorMessage(e) });
  }
}

// ----- retained outcomes ------------------------------------------------------

/** The retained result for an artifact path, if it is retained. */
export function resultForPath(listing: ResearchListing | null, path: string | null | undefined): RetainedResult | undefined {
  if (!listing || !path) return undefined;
  return listing.results.find((r) => r.path === path);
}

export async function retainArtifact(
  run: RunSummary,
  artifact: ArtifactRecord,
  state: RetainState,
  experiment: EntityId | null = null,
): Promise<boolean> {
  try {
    const outcome = await researchCommands.retain({ runId: run.id, artifactId: artifact.id, state, experiment });
    const what = artifact.kind === "table" ? "Table" : artifact.kind === "figure" ? "Figure" : "Output";
    notify({
      tone: "success",
      title: `${what} ${state === "active" ? "kept" : state === "archived" ? "archived" : "discarded"}`,
      body: `${outcome.result.path} is a retained result${outcome.evidence ? " with the run's computational evidence" : ""}.`,
    });
    await refreshListing();
    return true;
  } catch (e) {
    notify({ tone: "danger", title: "Could not change the result", body: errorMessage(e) });
    return false;
  }
}

export async function setResultState(entity: EntityId, state: RetainState): Promise<boolean> {
  try {
    const outcome = await researchCommands.setResultState({ entity, state });
    const uses = outcome.result.usedBy.length;
    notify({
      tone: "info",
      title: `${outcome.result.label} ${state === "active" ? "kept" : state === "archived" ? "archived" : "discarded"}`,
      body:
        state === "discarded" && uses > 0
          ? `Still used by ${outcome.result.usedBy.map((u) => u.label).join(", ")}; those attachments are unchanged.`
          : "Nothing was deleted.",
    });
    await refreshListing();
    return true;
  } catch (e) {
    notify({ tone: "danger", title: "Could not change the result", body: errorMessage(e) });
    return false;
  }
}

// ----- experiments --------------------------------------------------------------

export async function loadExperiment(entity: EntityId): Promise<void> {
  try {
    const detail = await researchCommands.experiment(entity);
    const selected = useSpace.getState().selected;
    if (selected?.kind === "experiment" && selected.entity === entity) useSpace.setState({ experiment: detail });
  } catch (e) {
    useSpace.setState({ error: errorMessage(e) });
  }
}

export async function saveExperiment(input: ExperimentInput): Promise<ExperimentDetail | null> {
  try {
    const detail = await researchCommands.saveExperiment(input);
    await refreshListing();
    useSpace.setState({ selected: { kind: "experiment", entity: detail.summary.entity }, experiment: detail });
    notify({ tone: "success", title: `Experiment “${detail.summary.label}” saved` });
    return detail;
  } catch (e) {
    notify({ tone: "danger", title: "Could not save the experiment", body: errorMessage(e) });
    return null;
  }
}

/** Run an experiment's procedure in the user's session, pinning its declared inputs. */
export async function runExperiment(detail: ExperimentDetail): Promise<RunSummary | null> {
  const procedure = detail.procedure?.path;
  if (!procedure) {
    notify({ tone: "info", title: "No procedure", body: "Choose the script that carries out the experiment, then save." });
    return null;
  }
  return execute({ kind: "file", path: procedure }, detail.inputs.map((i) => i.path));
}

// ----- promotions ---------------------------------------------------------------

export async function reconcilePromotion(operation: OperationId): Promise<void> {
  try {
    const outcome = await researchCommands.reconcilePromotion(operation);
    notify({
      tone: outcome.attachmentPresent ? "success" : "info",
      title: outcome.attachmentPresent ? "Attachment recorded in the project graph" : "Nothing to record",
      body: outcome.record.state.kind === "resolved" ? outcome.record.state.resolution : undefined,
    });
  } catch (e) {
    notify({ tone: "danger", title: "Could not reconcile the promotion", body: errorMessage(e) });
  }
  await refreshPromotions();
  await refreshListing();
}
