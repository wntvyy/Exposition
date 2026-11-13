// New Project state and actions. Rust validates every blueprint and
// destination; this store previews, then asks Rust to create, then opens the
// result like File ▸ Open Folder and shows the right page (Editor for a
// manuscript, Research for a research-only project, Agents for an objective).
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";

import { editors } from "../../app/editors";
import { layout } from "../../app/layout";
import { dismiss, notify } from "../../app/notifications";
import { EDITOR_PAGE, pages } from "../../app/pages";
import type { BlueprintEntry } from "../../bindings/BlueprintEntry";
import type { BlueprintValidation } from "../../bindings/BlueprintValidation";
import type { CapabilityReport } from "../../bindings/CapabilityReport";
import type { CreationAction } from "../../bindings/CreationAction";
import type { CreationStatus } from "../../bindings/CreationStatus";
import type { DestinationCheck } from "../../bindings/DestinationCheck";
import type { ExecutionMode } from "../../bindings/ExecutionMode";
import type { ManuscriptFormat } from "../../bindings/ManuscriptFormat";
import type { PermissionProfile } from "../../bindings/PermissionProfile";
import type { PresetCatalog } from "../../bindings/PresetCatalog";
import type { ProjectBlueprint } from "../../bindings/ProjectBlueprint";
import type { ProjectCreated } from "../../bindings/ProjectCreated";
import type { ResearchRuntime } from "../../bindings/ResearchRuntime";
import { errorMessage, events } from "../../lib/ipc";
import { startProjectGeneration } from "../agents/generation";
import { fileKey } from "../editor/documentClient";
import { FILES_VIEW_ID, loadDir, openWorkspace, ROOT, useWorkspace } from "../workspace/store";
import { projectCommands } from "./ipc";
import {
  EMPTY_PRESET,
  folderNameFor,
  joinPath,
  NEW_PROJECT_KEY,
  parentOf,
  presetRequest,
  setupKey,
} from "./model";

export type NewProjectMode = "preset" | "objective";

export interface ProjectsState {
  catalog: PresetCatalog | null;
  catalogError: string | null;
  /** Tool availability (null until discovery answers). */
  report: CapabilityReport | null;
  reportError: string | null;
  mode: NewProjectMode;
  presetId: string;
  title: string;
  format: ManuscriptFormat;
  runtime: ResearchRuntime;
  /** Null: the newest installed toolchain, else the default. */
  leanToolchain: string | null;
  includeWorkflow: boolean;
  /** Absolute path of the project folder (new, or existing and empty). */
  location: string;
  /** The user typed the location (do not re-derive it from the title). */
  locationEdited: boolean;
  destination: DestinationCheck | null;
  /** The user confirmed creating the project inside a trusted folder. */
  acceptTrusted: boolean;
  blueprint: ProjectBlueprint | null;
  preview: BlueprintValidation | null;
  previewError: string | null;
  objective: string;
  permission: PermissionProfile;
  execution: ExecutionMode;
  creating: boolean;
  error: string | null;
  /** Unfinished creations of the open project. */
  setups: CreationStatus[];
  resolving: boolean;
}

export const initialProjectsState: ProjectsState = {
  catalog: null,
  catalogError: null,
  report: null,
  reportError: null,
  mode: "preset",
  presetId: "paper",
  title: "",
  format: "typst",
  runtime: "python",
  leanToolchain: null,
  includeWorkflow: true,
  location: "",
  locationEdited: false,
  destination: null,
  acceptTrusted: false,
  blueprint: null,
  preview: null,
  previewError: null,
  objective: "",
  permission: "reviewEdits",
  execution: "disabled",
  creating: false,
  error: null,
  setups: [],
  resolving: false,
};

export const useProjects = create<ProjectsState>(() => initialProjectsState);

const set = (patch: Partial<ProjectsState>) => useProjects.setState(patch);

let previewSeq = 0;
let destinationSeq = 0;
let destinationTimer: ReturnType<typeof setTimeout> | null = null;

/** Open the New Project page (an editor tab). */
export function openNewProject(mode?: NewProjectMode): void {
  if (mode) set({ mode });
  editors.open({ key: NEW_PROJECT_KEY, title: "New Project" });
  void loadCatalog();
}

export async function loadCatalog(): Promise<void> {
  if (useProjects.getState().catalog) return;
  try {
    const catalog = await projectCommands.presets();
    const recent = useWorkspace.getState().recent[0]?.path;
    set({ catalog, catalogError: null });
    void loadCapabilities();
    const state = useProjects.getState();
    if (!state.location && recent) {
      const parent = parentOf(recent);
      if (parent) set({ location: joinPath(parent, folderNameFor(state.title || defaultTitle())) });
    }
    await refreshPreview();
    scheduleDestinationCheck();
  } catch (error) {
    set({ catalogError: errorMessage(error) });
  }
}

/** Tool availability, in the background (first-time discovery can take a while). */
export async function loadCapabilities(): Promise<void> {
  if (useProjects.getState().report) return;
  try {
    const report = await projectCommands.capabilities();
    set({ report, reportError: null });
    // The automatic Lean toolchain choice may now be an installed one.
    if (useProjects.getState().leanToolchain === null && report.leanToolchains.length > 0) void refreshPreview();
  } catch (error) {
    set({ reportError: errorMessage(error) });
  }
}

/** The toolchain written to `lean-toolchain`. */
export function effectiveLeanToolchain(state: ProjectsState = useProjects.getState()): string | null {
  return state.leanToolchain ?? state.report?.leanToolchains[0] ?? state.catalog?.defaultLeanToolchain ?? null;
}

function defaultTitle(): string {
  const { catalog, presetId, mode, objective } = useProjects.getState();
  if (mode === "objective") return objective.trim().split(/\s+/).slice(0, 6).join(" ") || "New project";
  return catalog?.presets.find((p) => p.id === presetId)?.title ?? "New project";
}

/** Regenerate the preset's blueprint and validate it (fresh ids each time). */
export async function refreshPreview(): Promise<void> {
  const state = useProjects.getState();
  const preset = state.catalog?.presets.find((p) => p.id === state.presetId);
  if (!preset || state.mode !== "preset") return;
  const seq = ++previewSeq;
  try {
    const blueprint = await projectCommands.presetBlueprint(
      presetRequest(preset, {
        title: state.title,
        format: state.format,
        runtime: state.runtime,
        leanToolchain: effectiveLeanToolchain(state),
        includeWorkflow: state.includeWorkflow,
      }),
    );
    const preview = await projectCommands.validate(JSON.stringify(blueprint));
    if (seq === previewSeq) set({ blueprint, preview, previewError: null });
  } catch (error) {
    if (seq === previewSeq) set({ blueprint: null, preview: null, previewError: errorMessage(error) });
  }
}

/** Change options; the location follows the title until the user edits it. */
export function setOptions(patch: Partial<ProjectsState>): void {
  const before = useProjects.getState();
  set({ ...patch, error: null });
  const after = useProjects.getState();
  const titleChanged =
    after.title !== before.title || after.presetId !== before.presetId || after.objective !== before.objective;
  if (titleChanged && !after.locationEdited && after.location) {
    const parent = parentOf(after.location);
    if (parent) {
      set({ location: joinPath(parent, folderNameFor(after.title || defaultTitle())) });
      scheduleDestinationCheck();
    }
  }
  void refreshPreview();
}

export function setLocation(location: string): void {
  set({ location, locationEdited: true, error: null, acceptTrusted: false });
  scheduleDestinationCheck();
}

export function scheduleDestinationCheck(): void {
  if (destinationTimer) clearTimeout(destinationTimer);
  destinationTimer = setTimeout(() => void checkDestination(), 250);
}

export async function checkDestination(): Promise<DestinationCheck | null> {
  const location = useProjects.getState().location.trim();
  if (!location) {
    set({ destination: null });
    return null;
  }
  const seq = ++destinationSeq;
  try {
    const result = await projectCommands.checkDestination(location);
    if (seq === destinationSeq) set({ destination: result });
    return result;
  } catch (error) {
    const result: DestinationCheck = {
      path: location,
      ok: false,
      message: errorMessage(error),
      createsFolder: false,
      trustedVia: null,
    };
    if (seq === destinationSeq) set({ destination: result });
    return result;
  }
}

/** Choose the folder that will contain the project (native dialog). */
export async function browseLocation(): Promise<void> {
  const chosen = await openDialog({ directory: true, multiple: false, title: "Choose where to create the project" });
  if (typeof chosen !== "string") return;
  const state = useProjects.getState();
  set({ location: joinPath(chosen, folderNameFor(state.title || defaultTitle())), locationEdited: false });
  scheduleDestinationCheck();
}

/** Close the open project's file editors first (asking about unsaved changes). */
async function closeFileEditors(): Promise<boolean> {
  for (const editor of editors.all()) {
    if (editor.resource.key.startsWith("file:") && !(await editors.close(editor.resource.key))) return false;
  }
  return true;
}

function notesFile(blueprint: ProjectBlueprint | null): string | null {
  const folder = blueprint?.research ? `.exposition/research/${blueprint.research.id}/` : null;
  return (folder && blueprint?.files.find((f) => f.path.startsWith(folder) && f.path.endsWith("notes.md"))?.path) ?? null;
}

/** Show what a new project starts with: never a graph setup screen. */
export function openEntry(entry: BlueprintEntry, blueprint: ProjectBlueprint | null): void {
  if (entry.kind === "editor") {
    pages.show(EDITOR_PAGE);
    editors.open({ key: fileKey(entry.path), path: entry.path, title: entry.path.split("/").pop() ?? entry.path });
    return;
  }
  layout.showView("sidebar", FILES_VIEW_ID);
  if (entry.kind === "research" && !pages.show("research")) {
    // The Research page is not available in this build: open the notes.
    const notes = notesFile(blueprint);
    if (notes) editors.open({ key: fileKey(notes), path: notes, title: notes.split("/").pop() ?? notes });
  }
}

function describeCreated(created: ProjectCreated): string {
  const files = created.applied.resources.length;
  const entities = created.applied.entities.length;
  const drafts = created.applied.workflows.length;
  const parts = [`${files} file${files === 1 ? "" : "s"}`, `${entities} project entit${entities === 1 ? "y" : "ies"}`];
  if (drafts) parts.push(`${drafts} workflow draft${drafts === 1 ? "" : "s"} (nothing runs until you review and start it)`);
  const trust = created.trustedVia
    ? `It lies inside the trusted folder ${created.trustedVia}; nothing was run.`
    : "Nothing was trusted or run.";
  return `${created.root}: ${parts.join(", ")}. ${trust}`;
}

async function createProject(blueprint: ProjectBlueprint): Promise<ProjectCreated | null> {
  const state = useProjects.getState();
  if (!(await closeFileEditors())) return null;
  set({ creating: true, error: null });
  try {
    const created = await projectCommands.create({
      destination: state.location.trim(),
      blueprint: JSON.stringify(blueprint),
      acceptTrustedLocation: state.acceptTrusted,
    });
    await openWorkspace(created.root);
    return created;
  } catch (error) {
    const message = errorMessage(error);
    set({ error: message });
    notify({ tone: "danger", title: "The project was not created", body: message });
    // A partly created project is reported when its folder is opened.
    void checkSetups();
    return null;
  } finally {
    set({ creating: false });
  }
}

/** Preset path: create from the previewed blueprint. */
export async function createFromPreset(): Promise<ProjectCreated | null> {
  const { blueprint, preview } = useProjects.getState();
  if (!blueprint || !preview?.valid) return null;
  const created = await createProject(blueprint);
  if (!created) return null;
  await editors.close(NEW_PROJECT_KEY);
  openEntry(created.entry, blueprint);
  notify({ tone: "success", title: `Created ${created.title}`, body: describeCreated(created) });
  // Regenerate so another project from this page gets fresh ids.
  void refreshPreview();
  return created;
}

/**
 * Objective path: create an empty project, open it (its graph attaches), and
 * start the agent service's project-generation task there — an ordinary
 * whole-project task with the chosen permission and no commands by default.
 */
export async function createFromObjective(): Promise<ProjectCreated | null> {
  const state = useProjects.getState();
  const objective = state.objective.trim();
  if (!objective) {
    set({ error: "Describe what the project is for." });
    return null;
  }
  let blueprint: ProjectBlueprint;
  try {
    blueprint = await projectCommands.presetBlueprint({
      preset: EMPTY_PRESET,
      title: state.title.trim() || defaultTitle(),
      format: null,
      runtime: null,
      leanToolchain: null,
      includeWorkflow: false,
      purpose: objective.slice(0, 4000),
    });
  } catch (error) {
    set({ error: errorMessage(error) });
    return null;
  }
  const created = await createProject(blueprint);
  if (!created) return null;
  await editors.close(NEW_PROJECT_KEY);
  try {
    const info = useWorkspace.getState().info;
    await startProjectGeneration({
      objective,
      workspaceRoot: info?.root ?? created.root,
      permission: state.permission,
      execution: state.execution,
      show: "page",
    });
    notify({
      tone: "success",
      title: `Created ${created.title}`,
      body: `${created.root}. An agent task is organizing it; review its changes in Agents.`,
    });
  } catch (error) {
    layout.showView("sidebar", FILES_VIEW_ID);
    notify({
      tone: "warning",
      title: "The project was created, but the agent task did not start",
      body: errorMessage(error),
    });
  }
  return created;
}

// ----- interrupted creation -------------------------------------------------

export async function checkSetups(): Promise<CreationStatus[]> {
  let setups: CreationStatus[] = [];
  try {
    setups = (await projectCommands.creationStatus()).filter((s) => !s.inProgress);
  } catch {
    setups = [];
  }
  set({ setups });
  if (setups.length === 0) {
    dismiss("project-setup-interrupted");
    return setups;
  }
  const first = setups[0] as CreationStatus;
  notify({
    id: "project-setup-interrupted",
    tone: "warning",
    title: "Project setup did not finish",
    body: `${first.title}: ${first.created} file${first.created === 1 ? "" : "s"} created, ${first.missing} missing${first.graphApplied ? "" : ", project structure not recorded"}. Choose how to continue.`,
    actions: [{ label: "Review", run: () => openSetup(first.operation) }],
    timeoutMs: null,
  });
  return setups;
}

export function openSetup(operation: string): void {
  editors.open({ key: setupKey(operation), title: "Project Setup" });
}

export async function resolveSetup(operation: string, action: CreationAction): Promise<boolean> {
  set({ resolving: true, error: null });
  try {
    const result = await projectCommands.creationResolve(operation, action);
    set({ setups: result.statuses.filter((s) => !s.inProgress) });
    void loadDir(ROOT);
    await editors.close(setupKey(operation));
    if (result.statuses.length === 0) dismiss("project-setup-interrupted");
    if (action === "finish") {
      notify({ tone: "success", title: "Project setup finished", body: "Missing files were created and the project structure was recorded." });
      if (result.entry) openEntry(result.entry, null);
    } else {
      notify({ tone: "info", title: "Project kept as created", body: "The files that exist were kept; nothing else was applied." });
    }
    return true;
  } catch (error) {
    const message = errorMessage(error);
    set({ error: message });
    notify({ tone: "danger", title: "Could not resolve the project setup", body: message });
    return false;
  } finally {
    set({ resolving: false });
  }
}

let started = false;

/** Check for unfinished creations whenever a project opens (once). */
export function startProjectEvents(): void {
  if (started) return;
  started = true;
  void events
    .workspace((event) => {
      if (event.kind === "opened") void checkSetups();
      if (event.kind === "closed") set({ setups: [] });
    })
    .catch(() => {
      started = false;
    });
}

export function resetProjectsForTests(): void {
  previewSeq = 0;
  destinationSeq = 0;
  useProjects.setState(initialProjectsState);
}
